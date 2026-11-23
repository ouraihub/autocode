mod interaction;
mod model;
mod runtime;
mod store;
mod transcript;
pub use transcript::{changes_nothing, Entry, SavedSubagent, SavedTranscript, SerializedEntry, Subagent, ToolEntry, ToolStatus, Transcript};
pub use transcript::QueuedMessage;
pub(crate) use runtime::{ActiveRun, RunInput};
pub use runtime::SessionEvent;

pub use interaction::{BackgroundTask, PendingQuestion};
pub(crate) use interaction::agent_stopped_waiting;
pub use model::{ContextSnapshot, RunSettings};

use agent_harness::{
    AgentEvent, CancellationToken, CatalogContext, ClaudeHarness, CodexHarness, DoneStatus,
    DshHarness, ExternalSession, Harness, HarnessId, LaunchContext, Model, OmpHarness,
    OpencodeHarness, PermissionMode, PiHarness, ReasoningLevel, RunControls, RunRequest, SkillRef,
    SlashCommand, SteerMessage, ToolCall, UserInputAnswer, UserInputQuestion, usage::PlanUsage,
};
use anyhow::{Context as _, Result};
use collections::{HashMap, HashSet};
use futures::{FutureExt as _, StreamExt as _, future::Shared};
use gpui::{App, AppContext as _, Context, Entity, EventEmitter, Global, SharedString, Task};
use i18n::{n, t};
use language::LanguageRegistry;
use markdown::Markdown;
use serde::{Deserialize, Serialize};
use settings::Settings as _;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use util::ResultExt as _;

use crate::{AgentChatSettings, AgentKind};

const FIRST_MESSAGE_TITLE_CHARS: usize = 20;
const AGENT_TITLE_CHARS: usize = 60;
const TOOL_OUTPUT_MAX_CHARS: usize = 200_000;
const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);
const USAGE_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);
const USAGE_FORCED_MIN_INTERVAL: Duration = Duration::from_secs(30);
const CATALOG_REFRESH_INTERVAL: Duration = Duration::from_secs(60);
const NEW_CHAT_TITLE: &str = n!("New chat");
const FORKED_NOTICE: &str = n!("Forked from \"{title}\"");
const SIDE_CHAT_NOTICE: &str = n!("Side chat of \"{title}\"");
const CONTINUED_NOTICE: &str = n!("Continued from a saved {source} chat");
const KEEPS_LIMITS_NOTICE: &str =
    n!("{agent} keeps its current limits until this turn ends, then uses the new permission mode.");

pub(crate) fn display_title(title: &str) -> SharedString {
    if title.is_empty() {
        i18n::translate(NEW_CHAT_TITLE)
    } else if title == agent_harness::opencode::UNTITLED_CHAT {
        i18n::translate(agent_harness::opencode::UNTITLED_CHAT)
    } else {
        SharedString::from(title.to_owned())
    }
}

pub(crate) fn translated_notice(text: &SharedString) -> SharedString {
    if let Some(translation) = i18n::lookup(text) {
        return translation;
    }
    for template in [FORKED_NOTICE, SIDE_CHAT_NOTICE] {
        for default_title in [NEW_CHAT_TITLE, agent_harness::opencode::UNTITLED_CHAT] {
            let filled = i18n::format(
                SharedString::new_static(template),
                &[("title", &default_title)],
            );
            if filled == *text {
                return i18n::format(
                    i18n::translate(template),
                    &[("title", &i18n::translate(default_title))],
                );
            }
        }
    }
    i18n::translate_filled(
        text,
        &[
            FORKED_NOTICE,
            SIDE_CHAT_NOTICE,
            CONTINUED_NOTICE,
            KEEPS_LIMITS_NOTICE,
        ],
    )
    .unwrap_or_else(|| text.clone())
}

fn metadata_path(directory: &Path, id: &str) -> PathBuf {
    directory.join(format!("{id}.meta.json"))
}

fn entries_path(directory: &Path, id: &str) -> PathBuf {
    directory.join(format!("{id}.entries.json"))
}

fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

static FILE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn write_atomically(path: &Path, contents: &[u8]) -> Result<()> {
    let _guard = FILE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let directory = path
        .parent()
        .context("session file has no parent directory")?;
    std::fs::create_dir_all(directory)?;
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, contents)?;
    std::fs::rename(&temporary, path)?;
    Ok(())
}

pub(crate) fn diff_for_call(call: &ToolCall) -> Option<agent_harness::ToolDiff> {
    match call {
        ToolCall::EditFile {
            path,
            old_string,
            new_string: Some(new_string),
        } => Some(agent_harness::ToolDiff {
            path: path.clone(),
            old_text: Some(old_string.clone().unwrap_or_default()),
            new_text: new_string.clone(),
        }),
        ToolCall::WriteFile {
            path,
            content: Some(content),
        } => Some(agent_harness::ToolDiff {
            path: path.clone(),
            old_text: None,
            new_text: content.clone(),
        }),
        _ => None,
    }
}

fn attachment_paths(attachments: &[PathBuf]) -> Vec<String> {
    attachments
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect()
}

fn title_from(text: &str) -> String {
    shortened_title(text, FIRST_MESSAGE_TITLE_CHARS)
}

fn agent_title(text: &str) -> String {
    shortened_title(text, AGENT_TITLE_CHARS)
}

fn shortened_title(text: &str, max_chars: usize) -> String {
    let line = agent_harness::view::single_line(text);
    if line.chars().count() <= max_chars {
        return line;
    }
    let mut title: String = line.chars().take(max_chars - 1).collect();
    title.push('…');
    title
}

fn truncate_output(text: String) -> String {
    if text.chars().count() <= TOOL_OUTPUT_MAX_CHARS {
        return text;
    }
    let mut truncated: String = text.chars().take(TOOL_OUTPUT_MAX_CHARS).collect();
    truncated.push('…');
    truncated
}

impl AgentKind {
    pub(crate) fn harness_id(self) -> HarnessId {
        match self {
            AgentKind::Claude => HarnessId::ClaudeCode,
            AgentKind::Codex => HarnessId::Codex,
            AgentKind::Opencode => HarnessId::Opencode,
            AgentKind::Pi => HarnessId::Pi,
            AgentKind::Omp => HarnessId::Omp,
            AgentKind::Dsh => HarnessId::Dsh,
        }
    }

    fn new_harness(self) -> Arc<dyn Harness> {
        match self {
            AgentKind::Claude => Arc::new(ClaudeHarness::new()),
            AgentKind::Codex => Arc::new(CodexHarness::new()),
            AgentKind::Opencode => Arc::new(OpencodeHarness::new()),
            AgentKind::Pi => Arc::new(PiHarness::new()),
            AgentKind::Omp => Arc::new(OmpHarness::new()),
            AgentKind::Dsh => Arc::new(DshHarness::new()),
        }
    }
}


#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub id: String,
    pub kind: AgentKind,
    pub title: String,
    pub cwd: PathBuf,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub native_session_id: Option<String>,
    #[serde(default)]
    pub settings: RunSettings,
    #[serde(default)]
    pub context: Option<ContextSnapshot>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub side_chat: bool,
    #[serde(default)]
    pub fork_context: Option<String>,
    /// The parent's agent session, copied by the agent itself on the fork's first run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_source: Option<String>,
    #[serde(default)]
    pub outcome: Option<ChatOutcome>,
    #[serde(default)]
    pub unseen: bool,
    #[serde(default)]
    pub project_root: Option<PathBuf>,
    #[serde(default)]
    pub branch: Option<String>,
    /// The agent's id for a message taken back to edit, forgotten by the agent on the next run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped_prompt: Option<String>,
}

impl SessionMetadata {
    fn new(kind: AgentKind, cwd: PathBuf, settings: RunSettings) -> Self {
        let created_at = now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            kind,
            title: String::new(),
            cwd,
            created_at,
            updated_at: created_at,
            native_session_id: None,
            settings,
            context: None,
            pinned: false,
            parent_id: None,
            side_chat: false,
            fork_context: None,
            fork_source: None,
            outcome: None,
            unseen: false,
            project_root: None,
            branch: None,
            dropped_prompt: None,
        }
    }

    pub fn project_root(&self) -> &Path {
        self.project_root.as_deref().unwrap_or(&self.cwd)
    }

    fn is_in(&self, roots: &[PathBuf]) -> bool {
        roots.iter().any(|root| root == self.project_root())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatOutcome {
    Completed,
    Errored,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Prompt {
    pub text: String,
    pub attachments: Vec<PathBuf>,
    pub skills: Vec<SkillRef>,
}

impl Prompt {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Default::default()
        }
    }

    fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.attachments.is_empty()
    }

    pub fn merge(&mut self, other: Prompt) {
        if !other.text.trim().is_empty() {
            if !self.text.trim().is_empty() {
                self.text.push_str("\n\n");
            }
            self.text.push_str(&other.text);
        }
        self.attachments.extend(other.attachments);
        self.skills.extend(other.skills);
    }
}

impl From<String> for Prompt {
    fn from(text: String) -> Self {
        Self::text(text)
    }
}

impl From<&str> for Prompt {
    fn from(text: &str) -> Self {
        Self::text(text)
    }
}


pub struct AgentSession {
    directory: Arc<Path>,
    generation: u64,
    working_since: Option<Instant>,
    run_failed: bool,
    reported_model: Option<(String, Option<String>)>,
    harness: Arc<dyn Harness>,
    restart_on_next_send: bool,
    native_fork_prompt: Option<Prompt>,
    native_fork_failed: bool,
    native_fork_errors: Vec<String>,
    metadata: SessionMetadata,
    transcript: Transcript,
    subagents: HashMap<String, Subagent>,
    working: bool,
    compacting: bool,
    background_tasks: Vec<BackgroundTask>,
    known_tasks: HashMap<String, BackgroundTask>,
    run: Option<ActiveRun>,
    prompt_after_stop: Option<Prompt>,
    user_stopped: bool,
    turn_prompts: usize,
    turn_prompt_id: Option<String>,
    editable_prompt: Option<String>,
    queue: Vec<QueuedMessage>,
    next_queue_id: u64,
    editing_queued: Option<u64>,
    deleted: bool,
    pending_questions: Vec<PendingQuestion>,
    noticed_auto_unavailable: bool,
    languages: Arc<LanguageRegistry>,
    save_metadata_task: Task<()>,
    save_entries_task: Task<()>,
}

impl EventEmitter<SessionEvent> for AgentSession {}

impl AgentSession {
    fn new(
        directory: Arc<Path>,
        harness: Arc<dyn Harness>,
        metadata: SessionMetadata,
        saved: SavedTranscript,
        languages: Arc<LanguageRegistry>,
        cx: &mut Context<Self>,
    ) -> Self {
        let (entries, saved_subagents, saved_queue) = match saved {
            SavedTranscript::Current {
                entries,
                subagents,
                queue,
            } => (entries, subagents, queue),
            SavedTranscript::EntriesOnly(entries) => (entries, Vec::new(), Vec::new()),
        };
        let transcript = Transcript::load(entries, &languages, cx);
        let subagents = saved_subagents
            .into_iter()
            .map(|saved| {
                let subagent = Subagent {
                    transcript: Transcript::load(saved.entries, &languages, cx),
                    status: Some(saved.status.unwrap_or(DoneStatus::Interrupted)),
                    started_at: Instant::now(),
                };
                (saved.id, subagent)
            })
            .collect();
        Self {
            directory,
            generation: 0,
            working_since: None,
            run_failed: false,
            reported_model: None,
            harness,
            restart_on_next_send: false,
            native_fork_prompt: None,
            native_fork_failed: false,
            native_fork_errors: Vec::new(),
            metadata,
            transcript,
            subagents,
            working: false,
            compacting: false,
            background_tasks: Vec::new(),
            known_tasks: HashMap::default(),
            run: None,
            prompt_after_stop: None,
            user_stopped: false,
            turn_prompts: 0,
            turn_prompt_id: None,
            editable_prompt: None,
            queue: saved_queue
                .into_iter()
                .enumerate()
                .map(|(index, prompt)| QueuedMessage {
                    id: index as u64,
                    prompt: prompt.into(),
                })
                .collect(),
            next_queue_id: u64::MAX / 2,
            editing_queued: None,
            deleted: false,
            pending_questions: Vec::new(),
            noticed_auto_unavailable: false,
            languages,
            save_metadata_task: Task::ready(()),
            save_entries_task: Task::ready(()),
        }
    }

    pub fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }

    pub fn kind(&self) -> AgentKind {
        self.metadata.kind
    }

    pub fn entries(&self) -> &[Entry] {
        &self.transcript.entries
    }

    pub fn subagent(&self, id: &str) -> Option<&Subagent> {
        self.subagents.get(id)
    }

    pub fn subagent_running(&self, id: &str) -> bool {
        let Some(subagent) = self.subagents.get(id) else {
            return false;
        };
        let spawn_running = std::iter::once(&self.transcript)
            .chain(self.subagents.values().map(|subagent| &subagent.transcript))
            .find_map(|transcript| transcript.tool(id))
            .is_some_and(|tool| tool.status == ToolStatus::Running);
        let backgrounded = self
            .background_tasks
            .iter()
            .chain(self.known_tasks.values())
            .any(|task| task.tool_use_id.as_deref() == Some(id));
        subagent.status.is_none()
            && self.run.is_some()
            && (spawn_running || backgrounded || self.working)
    }

    pub fn mark_seen(&mut self, cx: &mut Context<Self>) {
        if self.metadata.unseen {
            self.metadata.unseen = false;
            self.save_metadata(cx);
            cx.dismiss_system_notification(&self.metadata.id);
        }
    }

    pub fn update_metadata(&mut self, change: impl FnOnce(&mut SessionMetadata), cx: &mut Context<Self>) {
        change(&mut self.metadata);
        self.save_metadata(cx);
        cx.notify();
    }

    pub fn is_deleted(&self) -> bool {
        self.deleted
    }

    pub fn is_working(&self) -> bool {
        self.working
    }

    pub(crate) fn reported_model(&self) -> Option<(&str, Option<&str>)> {
        self.reported_model.as_ref().map(|(model, provider)| (model.as_str(), provider.as_deref()))
    }

    fn agent_activity(&self) -> Option<workspace::AgentActivity> {
        if self.deleted {
            None
        } else if !self.pending_questions.is_empty() {
            Some(workspace::AgentActivity::NeedsInput)
        } else if self.working {
            Some(workspace::AgentActivity::Working)
        } else {
            None
        }
    }

    fn auto_compact_tokens(&self, cx: &App) -> Option<u64> {
        let limit = AgentChatSettings::get_global(cx).auto_compact_tokens?;
        let window = self.metadata.context.and_then(|context| context.window);
        Some(match (self.metadata.kind, window) {
            // Codex may not cap the limit at its context window, and would then never compact.
            (AgentKind::Codex, Some(window)) if window > 0 => limit.min(window * 9 / 10),
            _ => limit,
        })
    }

    pub fn background_tasks(&self) -> &[BackgroundTask] {
        &self.background_tasks
    }

    pub fn tool_entry(&self, id: &str) -> Option<&ToolEntry> {
        std::iter::once(&self.transcript)
            .chain(self.subagents.values().map(|subagent| &subagent.transcript))
            .find_map(|transcript| transcript.tool(id))
    }

    pub fn is_compacting(&self) -> bool {
        self.working && self.compacting
    }

    pub fn can_steer(&self, prompt: &Prompt) -> bool {
        // Claude reads a slash command sent into a running turn as plain text.
        self.working
            && !(self.metadata.kind == AgentKind::Claude
                && agent_harness::leading_command(&prompt.text).is_some())
    }

    pub fn working_since(&self) -> Option<Instant> {
        self.working.then_some(self.working_since).flatten()
    }

    pub fn run_failed(&self) -> bool {
        self.run_failed
    }

    pub fn pending_questions(&self) -> &[PendingQuestion] {
        &self.pending_questions
    }

    pub fn settings(&self) -> &RunSettings {
        &self.metadata.settings
    }

    pub fn context(&self) -> Option<ContextSnapshot> {
        self.metadata.context
    }

    /// Model and effort are launch flags: a change takes effect by resuming in a new process.
    pub fn update_settings(
        &mut self,
        change: impl FnOnce(&mut RunSettings),
        cx: &mut Context<Self>,
    ) {
        if self.change_settings(change, cx) {
            cx.emit(SessionEvent::SettingsChanged);
        }
    }

    /// Like `update_settings`, but not remembered as the default for new chats.
    pub fn pin_settings(&mut self, change: impl FnOnce(&mut RunSettings), cx: &mut Context<Self>) {
        let restart_on_next_send = self.restart_on_next_send;
        self.change_settings(change, cx);
        self.restart_on_next_send = restart_on_next_send;
    }

    fn change_settings(
        &mut self,
        change: impl FnOnce(&mut RunSettings),
        cx: &mut Context<Self>,
    ) -> bool {
        let before = self.metadata.settings.clone();
        change(&mut self.metadata.settings);
        if self.metadata.settings == before {
            return false;
        }
        let permission = self.metadata.settings.permission;
        if permission != before.permission {
            self.apply_permission_now(permission, cx);
        }
        self.restart_on_next_send = self.run.is_some();
        self.save_metadata(cx);
        cx.notify();
        true
    }

    pub fn queue(&self) -> &[QueuedMessage] {
        &self.queue
    }

    pub fn send_message(&mut self, prompt: impl Into<Prompt>, cx: &mut Context<Self>) {
        let mut prompt = prompt.into();
        prompt.text = prompt.text.trim().to_string();
        if prompt.is_empty() || self.deleted {
            return;
        }
        let stopping = self
            .run
            .as_ref()
            .is_some_and(|run| run.interrupt.is_cancelled());
        if self.working && self.run.is_some() && !stopping {
            if AgentChatSettings::get_global(cx).steer_while_working {
                self.deliver(prompt, cx);
            } else {
                self.enqueue(prompt, cx);
            }
            return;
        }
        if !stopping && self.has_sendable_queued() {
            self.enqueue(prompt, cx);
            if let Some(next) = self.take_next_queued() {
                self.deliver(next.prompt, cx);
            }
            return;
        }
        self.deliver(prompt, cx);
    }

    fn has_sendable_queued(&self) -> bool {
        self.queue
            .iter()
            .any(|queued| Some(queued.id) != self.editing_queued)
    }

    fn take_next_queued(&mut self) -> Option<QueuedMessage> {
        let index = self
            .queue
            .iter()
            .position(|queued| Some(queued.id) != self.editing_queued)?;
        Some(self.queue.remove(index))
    }

    fn enqueue(&mut self, prompt: Prompt, cx: &mut Context<Self>) {
        self.next_queue_id += 1;
        self.queue.push(QueuedMessage {
            id: self.next_queue_id,
            prompt,
        });
        self.save_entries(cx);
        cx.notify();
    }

    fn take_queued(&mut self, id: u64) -> Option<(usize, Prompt)> {
        let index = self.queue.iter().position(|queued| queued.id == id)?;
        if self.editing_queued == Some(id) {
            self.editing_queued = None;
        }
        Some((index, self.queue.remove(index).prompt))
    }

    pub fn steer_queued(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some((_, prompt)) = self.take_queued(id) {
            self.deliver(prompt, cx);
        }
    }

    pub fn send_queued_now(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some((_, prompt)) = self.take_queued(id) else {
            return;
        };
        if self.working
            && let Some(run) = &self.run
        {
            run.interrupt.cancel();
        }
        self.deliver(prompt, cx);
    }

    pub fn remove_queued(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.take_queued(id).is_some() {
            self.save_entries(cx);
            cx.notify();
        }
    }

    pub fn move_queued(&mut self, id: u64, to_index: usize, cx: &mut Context<Self>) {
        let Some(from) = self.queue.iter().position(|queued| queued.id == id) else {
            return;
        };
        let queued = self.queue.remove(from);
        let to_index = to_index.min(self.queue.len());
        self.queue.insert(to_index, queued);
        self.save_entries(cx);
        cx.notify();
    }

    pub fn editing_queued(&self) -> Option<u64> {
        self.editing_queued
    }

    pub fn begin_queued_edit(&mut self, id: u64, cx: &mut Context<Self>) -> Option<Prompt> {
        let queued = self.queue.iter().find(|queued| queued.id == id)?;
        let prompt = queued.prompt.clone();
        self.editing_queued = Some(id);
        cx.notify();
        Some(prompt)
    }

    pub fn finish_queued_edit(
        &mut self,
        id: u64,
        edited: Option<Prompt>,
        cx: &mut Context<Self>,
    ) {
        if self.editing_queued == Some(id) {
            self.editing_queued = None;
        }
        if let Some(prompt) = edited.filter(|prompt| !prompt.is_empty()) {
            match self.queue.iter_mut().find(|queued| queued.id == id) {
                Some(queued) => queued.prompt = prompt,
                None => self.enqueue(prompt, cx),
            }
            self.save_entries(cx);
        }
        cx.notify();
    }

    /// The last message, once the user stops its turn before the agent changed anything.
    pub fn editable_message(&self) -> Option<usize> {
        if self.editable_prompt.is_none() || self.working {
            return None;
        }
        let index = self.last_user_index()?;
        self.transcript.entries[index + 1..]
            .iter()
            .all(|entry| match entry {
                Entry::Tool(tool) => changes_nothing(&tool.call),
                _ => true,
            })
            .then_some(index)
    }

    pub fn edit_last_message(&mut self, cx: &mut Context<Self>) -> Option<Prompt> {
        let index = self.editable_message()?;
        let Some(Entry::User {
            text,
            attachments,
            skills,
            ..
        }) = self.transcript.entries.get(index)
        else {
            return None;
        };
        let prompt = Prompt {
            text: text.to_string(),
            attachments: attachments.clone(),
            skills: skills.clone(),
        };
        self.transcript.entries.truncate(index);
        self.transcript.text_block_open = false;
        self.metadata.dropped_prompt = self.editable_prompt.take();
        if self.last_user_index().is_none() && self.metadata.title == title_from(&prompt.text) {
            self.metadata.title.clear();
        }
        self.save_metadata(cx);
        self.save_entries(cx);
        cx.notify();
        Some(prompt)
    }

    fn last_user_index(&self) -> Option<usize> {
        self.transcript
            .entries
            .iter()
            .rposition(|entry| matches!(entry, Entry::User { .. }))
    }

    pub fn retry_undelivered(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(Entry::User {
            text,
            attachments,
            skills,
            undelivered: true,
            ..
        }) = self.transcript.entries.get(index)
        else {
            return;
        };
        let prompt = Prompt {
            text: text.to_string(),
            attachments: attachments.clone(),
            skills: skills.clone(),
        };
        let notices = self.transcript.entries[index + 1..]
            .iter()
            .take_while(|entry| matches!(entry, Entry::Notice { is_error: true, .. }))
            .count();
        self.transcript.entries.drain(index..=index + notices);
        self.send_message(prompt, cx);
    }
    pub fn retry_latest_undelivered(&mut self, cx: &mut Context<Self>) {
        if let Some(index) = self.transcript.entries.iter().rposition(|entry| matches!(entry, Entry::User { undelivered: true, .. })) {
            self.retry_undelivered(index, cx);
        }
    }

    fn deliver(&mut self, prompt: Prompt, cx: &mut Context<Self>) {
        let stopping = self
            .run
            .as_ref()
            .is_some_and(|run| run.interrupt.is_cancelled());
        let is_command = |prompt: &Prompt| agent_harness::leading_command(&prompt.text).is_some();
        let steering_command = self.run.is_some() && !stopping && !self.can_steer(&prompt);
        // Merging would turn the other message into the command's arguments.
        let merging_command = stopping
            && self
                .prompt_after_stop
                .as_ref()
                .is_some_and(|pending| is_command(pending) || is_command(&prompt));
        if self.working && (steering_command || merging_command) {
            self.next_queue_id += 1;
            self.queue.insert(
                0,
                QueuedMessage {
                    id: self.next_queue_id,
                    prompt,
                },
            );
            self.save_entries(cx);
            cx.notify();
            return;
        }
        self.user_stopped = false;
        self.editable_prompt = None;
        let new_turn = !self.working;
        if (self.restart_on_next_send || self.metadata.dropped_prompt.is_some()) && !self.working {
            self.run = None;
            self.restart_on_next_send = false;
        }
        if self.metadata.title.is_empty() {
            self.metadata.title = title_from(&prompt.text);
        }
        self.transcript.push_user(
            prompt.text.clone(),
            prompt.attachments.clone(),
            prompt.skills.clone(),
        );
        if !self.working {
            self.working_since = Some(Instant::now());
        }
        self.working = true;
        self.run_failed = false;
        self.metadata.outcome = None;
        self.touch(cx);

        if let Some(run) = &self.run {
            if run.interrupt.is_cancelled() {
                match &mut self.prompt_after_stop {
                    Some(pending) => pending.merge(prompt),
                    None => self.prompt_after_stop = Some(prompt),
                }
                cx.notify();
                return;
            }
            let steer = SteerMessage {
                prompt: prompt.text.clone(),
                message_id: None,
                attachments: attachment_paths(&prompt.attachments),
                skills: prompt.skills.clone(),
            };
            if run.steering.try_send(steer).is_ok() {
                if new_turn {
                    self.turn_prompts = 1;
                    self.turn_prompt_id = None;
                } else {
                    self.turn_prompts += 1;
                }
                cx.notify();
                return;
            }
            self.run = None;
        }
        self.start_run(prompt, cx);
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        self.prompt_after_stop = None;
        self.user_stopped = true;
        match &self.run {
            Some(run) => run.interrupt.cancel(),
            None => {
                self.working = false;
                cx.notify();
            }
        }
    }

    pub fn answer(&mut self, id: &str, answers: Vec<UserInputAnswer>, cx: &mut Context<Self>) {
        let Some(index) = self
            .pending_questions
            .iter()
            .position(|pending| pending.id() == id)
        else {
            return;
        };
        let pending = self.pending_questions.get(index).expect("index came from the same list");
        if pending.generation != self.generation
            || pending.deadline.is_some_and(|deadline| Instant::now() >= deadline)
        {
            return;
        }
        let mut answers = answers;
        for answer in &mut answers {
            if let Some(question) = pending.questions.iter().find(|question| question.id == answer.question_id) {
                for label in &mut answer.labels {
                    if let Some(option) = question.option_label(label) {
                        *label = option.to_owned();
                    }
                }
            }
        }
        let mut pending = self.pending_questions.remove(index);
        cx.dismiss_system_notification(&self.metadata.id);
        if let Some(responder) = pending.responder.take()
            && responder.send(answers).is_err()
        {
            log::debug!("agent stopped waiting for an answer");
        }
        cx.notify();
    }

    fn drop_withdrawn_questions(&mut self, cx: &mut Context<Self>) {
        let before = self.pending_questions.len();
        self.pending_questions.retain(|pending| {
            !pending.expired(Instant::now())
                && !pending
                    .responder
                    .as_ref()
                    .is_some_and(tokio::sync::oneshot::Sender::is_closed)
        });
        if self.pending_questions.len() == before {
            return;
        }
        if self.pending_questions.is_empty() {
            cx.dismiss_system_notification(&self.metadata.id);
        }
        cx.notify();
    }

    fn apply_permission_now(&mut self, permission: PermissionMode, cx: &mut Context<Self>) {
        if let Some(run) = &self.run {
            run.permission.send_replace(permission);
        }
        if self.skips_prompts() {
            let waiting: Vec<String> = self
                .pending_questions
                .iter()
                .filter(|pending| pending.is_tool_permission())
                .map(|pending| pending.id().to_string())
                .collect();
            for id in waiting {
                self.answer_permission(&id, true, cx);
            }
        } else if self.working && self.kind() != AgentKind::Claude {
            let notice = i18n::format(
                SharedString::new_static(KEEPS_LIMITS_NOTICE),
                &[("agent", &self.kind().label())],
            );
            self.push_notice(notice.to_string(), false, cx);
        }
    }

    fn skips_prompts(&self) -> bool {
        self.metadata
            .settings
            .permission
            .for_harness(self.kind().harness_id())
            .skips_prompts()
    }

    pub fn answer_permission(&mut self, id: &str, allow: bool, cx: &mut Context<Self>) {
        let Some(pending) = self
            .pending_questions
            .iter()
            .find(|pending| pending.id() == id)
        else {
            return;
        };
        let label = if allow {
            agent_harness::claude::PERMISSION_ALLOW
        } else {
            agent_harness::claude::PERMISSION_DENY
        };
        let answers = pending
            .questions
            .iter()
            .map(|question| UserInputAnswer {
                question_id: question.id.clone(),
                labels: vec![label.to_string()],
            })
            .collect();
        self.answer(id, answers, cx);
    }

    fn start_run(&mut self, prompt: Prompt, cx: &mut Context<Self>) {
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.reported_model = None;
        self.restart_on_next_send = false;
        self.turn_prompts = 1;
        self.turn_prompt_id = None;
        let drop_prompt = self.metadata.dropped_prompt.take();
        if drop_prompt.is_some() {
            self.save_metadata(cx);
        }
        let harness = self.harness.clone();
        if self.metadata.native_session_id.is_none() {
            self.metadata.context = None;
        }
        let fork = self
            .metadata
            .native_session_id
            .is_none()
            .then(|| self.metadata.fork_source.clone())
            .flatten();
        self.native_fork_prompt = fork.is_some().then(|| prompt.clone());
        self.native_fork_errors.clear();
        let text = match self.metadata.fork_context.take() {
            context if fork.is_some() => {
                self.metadata.fork_context = context;
                prompt.text.clone()
            }
            Some(context) if agent_harness::leading_command(&prompt.text).is_none() => {
                self.save_metadata(cx);
                format!(
                    "Continue this conversation using the following prior conversation as context.\n<conversation>\n{context}\n</conversation>\n\n{}",
                    prompt.text
                )
            }
            context => {
                self.metadata.fork_context = context;
                prompt.text.clone()
            }
        };
        let settings = self.metadata.settings.clone();
        let support_path = paths::data_dir()
            .join("agent_support")
            .join(&self.metadata.id)
            .join(format!("{}-{}", generation, uuid::Uuid::new_v4()));
        let support = match agent_harness::RunSupportLease::create(support_path) {
            Ok(support) => Some(support),
            Err(error) => {
                self.working = false;
                self.push_notice(
                    format!("Could not create the agent support directory: {error}"),
                    true,
                    cx,
                );
                return;
            }
        };
        let request = RunRequest {
            prompt: text,
            attachments: attachment_paths(&prompt.attachments),
            skills: prompt.skills.clone(),
            model: settings.native_model_id(),
            reasoning: settings.reasoning,
            model_options: settings.options,
            cwd: self.metadata.cwd.to_string_lossy().into_owned(),
            permission: settings.permission,
            resume: self.metadata.native_session_id.clone(),
            fork,
            auto_compact_tokens: self.auto_compact_tokens(cx),
            chrome: AgentChatSettings::get_global(cx).claude_in_chrome,
            drop_prompt,
            ephemeral: false,
        };
        let (steering_tx, steering_rx) = tokio::sync::mpsc::channel(32);
        let interrupt = CancellationToken::new();
        let (permission_tx, permission_rx) = tokio::sync::watch::channel(settings.permission);
        let (question_tx, question_rx) = futures::channel::mpsc::unbounded::<(
            Vec<UserInputQuestion>,
            tokio::sync::oneshot::Sender<Vec<UserInputAnswer>>,
        )>();
        let controls = RunControls {
            request_input: Box::new(move |questions| {
                let (answer_tx, answer_rx) = tokio::sync::oneshot::channel();
                if question_tx.unbounded_send((questions, answer_tx)).is_err() {
                    log::debug!("agent asked a question after its chat closed");
                }
                answer_rx
            }),
            steering: steering_rx,
            interrupt: interrupt.clone(),
            permission: permission_rx,
        };

        let target = agent_harness::routing::backend_target_for_harness(harness.id());
        let node = target.and_then(|_| crate::dsh_node_runtime(cx));
        let managed = target.and_then(|target| crate::provider_services::ProviderServices::try_global(cx)
            .map(|services| services.managed_request(target, settings.model.clone())));
        let launch = LaunchContext {
            chat_id: self.metadata.id.clone(), generation, launch: None, support, gateway: None,
        };
        let events = cx.spawn(async move |this, cx| {
            let resolution = async {
                let Some(managed) = managed else { return Ok((None, None)); };
                let service = managed.service.clone();
                let target = managed.target;
                let resolved = managed.resolve().await?;
                let executable = if resolved.is_none() {
                    match service.load_preferences(target).await?.launch_executable {
                        Some(path) => Some(crate::engine_launch::find_program(&path)
                            .ok_or_else(|| anyhow::anyhow!("Coding partner executable was not found: {}", path.display()))?),
                        None => None,
                    }
                } else { None };
                Ok::<_, anyhow::Error>((resolved, executable))
            }.await;
            let (resolved, native_executable) = match resolution {
                Ok(resolution) => resolution,
                Err(error) => {
                    this.update(cx, |this, cx| { if this.is_current_generation(generation) { this.fail_run(error.to_string(), cx); } }).log_err();
                    return;
                }
            };
            if let Some(resolved) = &resolved {
                let selected = model_provider::ModelSelection::Managed { provider_id: resolved.provider.id.clone(), model_id: resolved.model_id.clone() };
                this.update(cx, |this, cx| {
                    if this.is_current_generation(generation) && this.metadata.settings.model.is_none() {
                        this.metadata.settings.model = Some(selected);
                        this.metadata.settings.config_source = model_provider::ConfigSource::Managed;
                        this.save_metadata(cx);
                    }
                }).log_err();
            }
            let Some(run) = this.update(cx, |this, cx| {
                if !this.is_current_generation(generation) || !this.working { return None; }
                Some(spawn_agent_work(cx, async move {
                    let mut launch = launch;
                    let mut request = request;
                    if let Some(executable) = native_executable {
                        launch.launch = Some(agent_harness::LaunchSpec {
                            executable, prefix_args: Vec::new(), config_dir: None,
                            env_refs: Vec::new(), env: Vec::new(),
                        });
                    }
                    if let (Some(resolved), Some(target)) = (resolved, target) {
                        request.model = Some(resolved.apply(target, &mut launch).await?);
                    }
                    if let Some(target) = target {
                        crate::engine_launch::apply(target, node, &mut launch).await
                            .map_err(|error| agent_harness::HarnessError::Protocol(error.to_string()))?;
                    }
                    harness.run(request, controls, launch).await
                }))
            }).ok().flatten() else { return; };
            let stream = match run.await {
                Ok(Ok(stream)) => stream,
                Ok(Err(error)) => {
                    this.update(cx, |this, cx| {
                        if this.is_current_generation(generation) {
                            this.fail_run(error.to_string(), cx);
                        }
                    })
                    .log_err();
                    return;
                }
                Err(error) => {
                    this.update(cx, |this, cx| {
                        if this.is_current_generation(generation) {
                            this.fail_run(error.to_string(), cx);
                        }
                    })
                    .log_err();
                    return;
                }
            };
            let events = stream
                .map(RunInput::Event)
                .chain(futures::stream::iter([RunInput::Ended]));
            let questions = question_rx.map(|(questions, responder)| RunInput::Question {
                questions,
                responder,
            });
            let mut inputs = futures::stream::select(events, questions);
            while let Some(input) = inputs.next().await {
                let update = match input {
                    RunInput::Event(Ok(event)) => this.update(cx, |this, cx| {
                        if this.is_current_generation(generation) {
                            this.apply_event(event, cx);
                        }
                    }),
                    RunInput::Event(Err(error)) => this.update(cx, |this, cx| {
                        if this.is_current_generation(generation) {
                            this.push_notice(error.to_string(), true, cx);
                        }
                    }),
                    RunInput::Question {
                        questions,
                        responder,
                    } => {
                        let (relay_tx, relay_rx) = tokio::sync::oneshot::channel();
                        cx.spawn({
                            let this = this.clone();
                            async move |cx| {
                                if agent_stopped_waiting(responder, relay_rx).await {
                                    this.update(cx, |this, cx| {
                                        if this.is_current_generation(generation) {
                                            this.drop_withdrawn_questions(cx);
                                        }
                                    })
                                    .log_err();
                                }
                            }
                        })
                        .detach();
                        this.update(cx, |this, cx| {
                            if this.is_current_generation(generation) {
                                this.push_question(questions, relay_tx, cx);
                            }
                        })
                    }
                    RunInput::Ended => {
                        if this
                            .update(cx, |this, cx| {
                                if this.is_current_generation(generation)
                                    && this.run.as_ref().is_some_and(|run| !run.round_done)
                                {
                                    this.fail_run(
                                        "The agent process exited before completing the turn.".into(),
                                        cx,
                                    );
                                }
                            })
                            .is_err()
                        {
                            return;
                        }
                        break;
                    }
                };
                if update.is_err() {
                    return;
                }
            }
            this.update(cx, |this, cx| {
                if this.is_current_generation(generation) {
                    this.run_ended(cx);
                }
            }).log_err();
        });
        self.run = Some(ActiveRun {
            generation,
            round_done: false,
            steering: steering_tx,
            interrupt,
            permission: permission_tx,
            _events: events,
        });
        cx.notify();
    }

    fn is_current_generation(&self, generation: u64) -> bool {
        self.run
            .as_ref()
            .is_some_and(|run| run.generation == generation)
    }

    fn fail_run(&mut self, message: String, cx: &mut Context<Self>) {
        if self.native_fork_pending() {
            log::warn!(
                "native fork failed, sending the conversation as context instead: {message}"
            );
            self.native_fork_failed = true;
            self.run_ended(cx);
            return;
        }
        self.run_failed = true;
        if let Some(Entry::User { undelivered, .. }) = self.transcript.entries.last_mut() {
            *undelivered = true;
        }
        self.push_notice(message, true, cx);
        self.run_ended(cx);
    }

    fn native_fork_pending(&self) -> bool {
        self.native_fork_prompt.is_some() && self.metadata.native_session_id.is_none()
    }

    fn run_ended(&mut self, cx: &mut Context<Self>) {
        self.run = None;
        self.background_tasks.clear();
        self.known_tasks.clear();
        self.pending_questions.clear();
        self.cancel_running_tools();
        for subagent in self.subagents.values_mut() {
            subagent.transcript.cancel_running_tools();
            subagent.status.get_or_insert(DoneStatus::Interrupted);
        }
        self.save_entries(cx);
        if std::mem::take(&mut self.native_fork_failed)
            && let Some(prompt) = self.native_fork_prompt.take()
            && !self.deleted
        {
            self.native_fork_errors.clear();
            self.metadata.fork_source = None;
            self.save_metadata(cx);
            self.working = true;
            self.start_run(prompt, cx);
            cx.notify();
            return;
        }
        match self.prompt_after_stop.take() {
            Some(prompt) if !self.deleted => {
                self.working = true;
                self.working_since = Some(Instant::now());
                self.start_run(prompt, cx);
            }
            _ => {
                self.working = false;
                if !self.user_stopped {
                    self.send_next_queued(cx);
                }
            }
        }
        cx.notify();
    }

    fn send_next_queued(&mut self, cx: &mut Context<Self>) {
        if self.working || self.deleted {
            return;
        }
        if let Some(next) = self.take_next_queued() {
            self.deliver(next.prompt, cx);
        }
    }

    fn close_for_deletion(&mut self, cx: &mut Context<Self>) -> Vec<Task<()>> {
        self.deleted = true;
        self.run = None;
        self.prompt_after_stop = None;
        self.pending_questions.clear();
        self.working = false;
        cx.notify();
        vec![
            std::mem::replace(&mut self.save_metadata_task, Task::ready(())),
            std::mem::replace(&mut self.save_entries_task, Task::ready(())),
        ]
    }

    fn push_question(
        &mut self,
        questions: Vec<UserInputQuestion>,
        responder: tokio::sync::oneshot::Sender<Vec<UserInputAnswer>>,
        cx: &mut Context<Self>,
    ) {
        if questions.is_empty() {
            if responder.send(Vec::new()).is_err() {
                log::debug!("agent stopped waiting for an answer");
            }
            return;
        }
        let pending = PendingQuestion {
            questions,
            responder: Some(responder),
            generation: self.generation,
            deadline: None,
        };
        // The harness may have asked just before the switch to a mode that skips prompts.
        let approve = pending.is_tool_permission() && self.skips_prompts();
        let id = pending.id().to_string();
        self.pending_questions.push(pending);
        if approve {
            self.answer_permission(&id, true, cx);
            return;
        }
        cx.emit(SessionEvent::NeedsInput);
        cx.notify();
    }

    fn apply_event(&mut self, event: AgentEvent, cx: &mut Context<Self>) {
        match event {
            AgentEvent::SessionStarted { session_id, .. } => {
                if !session_id.is_empty() {
                    self.set_native_session_id(session_id, cx);
                }
            }
            AgentEvent::ModelReported { model_id, provider_id, .. } => {
                self.reported_model = Some((model_id, provider_id));
            }
            AgentEvent::PermissionModeReported { mode } => {
                let asked_for_auto = self.metadata.kind == AgentKind::Claude
                    && self.metadata.settings.permission == PermissionMode::Auto;
                if asked_for_auto && mode != "auto" && !self.noticed_auto_unavailable {
                    self.noticed_auto_unavailable = true;
                    self.push_notice(
                        n!(
                            "Auto mode isn't available here, so Claude will ask before acting. It's off with fast mode, and on some plans and models."
                        )
                        .into(),
                        false,
                        cx,
                    );
                }
            }
            AgentEvent::TextDelta { text } => {
                self.resume_working();
                self.append_text(&text, false, cx)
            }
            AgentEvent::ReasoningDelta { text } => {
                self.resume_working();
                self.append_text(&text, true, cx)
            }
            AgentEvent::AssistantMessageCompleted { .. } => self.transcript.text_block_open = false,
            AgentEvent::Steered { .. } => {
                self.transcript.text_block_open = false;
                self.resume_working();
            }
            AgentEvent::ToolCall { id, call } => {
                self.resume_working();
                if call.is_subagent_spawn() {
                    self.subagents.entry(id.clone()).or_insert_with(Subagent::new);
                }
                self.transcript.start_tool(id, call);
                self.save_entries(cx);
            }
            AgentEvent::ToolResult {
                id,
                is_error,
                output,
                diff,
            } => {
                self.transcript.finish_tool(&id, is_error, output, diff);
                self.save_entries(cx);
            }
            AgentEvent::Error { message } if self.native_fork_pending() => {
                self.native_fork_errors.push(message);
            }
            AgentEvent::Error { message } => self.push_notice(message, true, cx),
            AgentEvent::Done {
                status: DoneStatus::Errored,
                error,
                ..
            } if self.native_fork_pending() => {
                log::warn!(
                    "native fork failed, sending the conversation as context instead: {error:?} {:?}",
                    self.native_fork_errors
                );
                self.native_fork_failed = true;
            }
            AgentEvent::Done { .. } if self.run.as_ref().is_some_and(|run| run.round_done) => {}
            AgentEvent::Done {
                status,
                error,
                session_id,
                ..
            } => {
                if let Some(run) = self.run.as_mut() {
                    run.round_done = true;
                }
                if let Some(session_id) = session_id.filter(|id| !id.is_empty()) {
                    self.set_native_session_id(session_id, cx);
                }
                self.working = self.prompt_after_stop.is_some();
                self.compacting = false;
                self.transcript.text_block_open = false;
                self.cancel_running_tools();
                match status {
                    DoneStatus::Completed => {
                        self.metadata.outcome = Some(ChatOutcome::Completed);
                        self.metadata.unseen = true;
                        cx.emit(SessionEvent::Finished { errored: false });
                    }
                    DoneStatus::Interrupted => {
                        if self.user_stopped && self.turn_prompts == 1 {
                            self.editable_prompt = self.turn_prompt_id.clone();
                        }
                    }
                    DoneStatus::Errored => {
                        self.run_failed = true;
                        self.metadata.outcome = Some(ChatOutcome::Errored);
                        self.metadata.unseen = true;
                        cx.emit(SessionEvent::Finished { errored: true });
                        self.push_notice(
                            error.unwrap_or_else(|| n!("The agent stopped with an error.").into()),
                            true,
                            cx,
                        )
                    }
                }
                self.touch(cx);
                if status != DoneStatus::Interrupted {
                    self.send_next_queued(cx);
                }
            }
            AgentEvent::PromptRecorded { id } => {
                self.turn_prompt_id.get_or_insert(id);
            }
            AgentEvent::TitleChanged { title } => self.adopt_agent_title(&title, cx),
            AgentEvent::GeneratedImage { path, .. } => {
                self.transcript.text_block_open = false;
                self.transcript.entries.push(Entry::Image { path: path.into() });
                self.save_entries(cx);
            }
            AgentEvent::Compacting { active } => {
                if active {
                    self.resume_working();
                }
                self.compacting = active;
            }
            AgentEvent::TaskStarted {
                task_id,
                tool_use_id,
                kind,
                description,
            } => {
                let started_at = self
                    .known_tasks
                    .get(&task_id)
                    .or_else(|| {
                        self.background_tasks
                            .iter()
                            .find(|task| task.task_id == task_id)
                    })
                    .map_or_else(Instant::now, |task| task.started_at);
                if let Some(live) = self
                    .background_tasks
                    .iter_mut()
                    .find(|task| task.task_id == task_id)
                {
                    if live.tool_use_id.is_none() {
                        live.tool_use_id = tool_use_id.clone();
                    }
                    if live.description.trim().is_empty() {
                        live.description = description.clone();
                    }
                }
                self.known_tasks.insert(
                    task_id.clone(),
                    BackgroundTask {
                        task_id,
                        tool_use_id,
                        kind,
                        description,
                        started_at,
                    },
                );
            }
            AgentEvent::TaskFinished { task_id, .. } => {
                self.known_tasks.remove(&task_id);
                self.background_tasks.retain(|task| task.task_id != task_id);
            }
            AgentEvent::BackgroundTasksChanged { tasks } => {
                let previous = std::mem::take(&mut self.background_tasks);
                self.background_tasks = tasks
                    .into_iter()
                    .map(|info| {
                        let known = self
                            .known_tasks
                            .get(&info.task_id)
                            .or_else(|| previous.iter().find(|task| task.task_id == info.task_id));
                        BackgroundTask {
                            tool_use_id: known.and_then(|task| task.tool_use_id.clone()),
                            started_at: known.map_or_else(Instant::now, |task| task.started_at),
                            description: if info.description.trim().is_empty() {
                                known.map(|task| task.description.clone()).unwrap_or_default()
                            } else {
                                info.description
                            },
                            kind: info.kind,
                            task_id: info.task_id,
                        }
                    })
                    .collect();
            }
            AgentEvent::Compacted { tokens, manual } => {
                self.compacting = false;
                // After /compact the Claude process keeps the old history in memory; a resume loads the new one.
                if manual && self.metadata.kind == AgentKind::Claude {
                    self.restart_on_next_send = true;
                }
                let mut context = self.metadata.context.unwrap_or_default();
                if context.tokens != tokens {
                    context.tokens = tokens;
                    self.metadata.context = Some(context);
                    self.save_metadata(cx);
                }
                self.push_notice(n!("Context compacted.").into(), false, cx);
            }
            AgentEvent::ContextUsage { tokens, window } => {
                let mut context = self.metadata.context.unwrap_or_default();
                if tokens.is_some() {
                    context.tokens = tokens;
                }
                if let Some(window) = window.filter(|window| *window > 0) {
                    context.window = Some(window);
                }
                if self.metadata.context != Some(context) {
                    self.metadata.context = Some(context);
                    self.save_metadata(cx);
                }
            }
            AgentEvent::Usage { .. }
            | AgentEvent::AvailableCommands { .. }
            | AgentEvent::InputRequested { .. }
            | AgentEvent::InputResolved { .. }
            | AgentEvent::UserMessage { .. }
            | AgentEvent::CapabilitiesReported { .. }
            | AgentEvent::SessionPersistenceReported { .. } => {}
            AgentEvent::Subagent {
                parent_tool_use_id,
                event,
            } => self.apply_subagent_event(parent_tool_use_id, *event, cx),
        }
        cx.notify();
    }

    /// The agent can start a turn on its own after a background command or subagent finishes.
    fn resume_working(&mut self) {
        if !self.working {
            self.editable_prompt = None;
            self.working = true;
            self.working_since = Some(Instant::now());
        }
    }

    fn apply_subagent_event(&mut self, parent: String, event: AgentEvent, cx: &mut Context<Self>) {
        let languages = self.languages.clone();
        let subagent = self.subagents.entry(parent).or_insert_with(Subagent::new);
        let transcript = &mut subagent.transcript;
        match event {
            AgentEvent::UserMessage { text } => transcript.push_user(text, Vec::new(), Vec::new()),
            AgentEvent::TextDelta { text } => transcript.append_text(&text, false, &languages, cx),
            AgentEvent::ReasoningDelta { text } => {
                transcript.append_text(&text, true, &languages, cx)
            }
            AgentEvent::AssistantMessageCompleted { .. } | AgentEvent::Steered { .. } => {
                transcript.close_text_block()
            }
            AgentEvent::ToolCall { id, call } => transcript.start_tool(id, call),
            AgentEvent::ToolResult {
                id,
                is_error,
                output,
                diff,
            } => transcript.finish_tool(&id, is_error, output, diff),
            AgentEvent::Error { message } => transcript.push_notice(message, true),
            AgentEvent::Done { status, error, .. } => {
                transcript.cancel_running_tools();
                transcript.text_block_open = false;
                if let Some(error) = error.filter(|_| status == DoneStatus::Errored) {
                    transcript.push_notice(error, true);
                }
                subagent.status = Some(status);
            }
            AgentEvent::Subagent {
                parent_tool_use_id,
                event,
            } => return self.apply_subagent_event(parent_tool_use_id, *event, cx),
            _ => return,
        }
        self.save_entries(cx);
    }

    fn load_external_history(
        &mut self,
        events: Vec<AgentEvent>,
        source: String,
        cx: &mut Context<Self>,
    ) {
        let languages = self.languages.clone();
        let mut seen_user: Vec<String> = Vec::new();
        let mut seen_tools: Vec<String> = Vec::new();
        let mut seen_tool_results: Vec<String> = Vec::new();
        let mut seen_assistant: Vec<String> = Vec::new();
        for event in events {
            let duplicate = match &event {
                AgentEvent::UserMessage { text } => {
                    let duplicate = seen_user.contains(text);
                    if !duplicate { seen_user.push(text.clone()); }
                    duplicate
                }
                AgentEvent::ToolCall { id, .. } => {
                    let duplicate = seen_tools.contains(id);
                    if !duplicate { seen_tools.push(id.clone()); }
                    duplicate
                }
                AgentEvent::ToolResult { id, .. } => {
                    let duplicate = seen_tool_results.contains(id);
                    if !duplicate { seen_tool_results.push(id.clone()); }
                    duplicate
                }
                AgentEvent::AssistantMessageCompleted { assistant_message_id } => {
                    let duplicate = seen_assistant.contains(assistant_message_id);
                    if !duplicate { seen_assistant.push(assistant_message_id.clone()); }
                    duplicate
                }
                _ => false,
            };
            if duplicate { continue; }
            let transcript = &mut self.transcript;
            match event {
                AgentEvent::UserMessage { text } => {
                    transcript.push_user(text, Vec::new(), Vec::new())
                }
                AgentEvent::TextDelta { text } => {
                    transcript.append_text(&text, false, &languages, cx)
                }
                AgentEvent::ReasoningDelta { text } => {
                    transcript.append_text(&text, true, &languages, cx)
                }
                AgentEvent::AssistantMessageCompleted { .. } => transcript.text_block_open = false,
                AgentEvent::ToolCall { id, call } => transcript.start_tool(id, call),
                AgentEvent::ToolResult {
                    id,
                    is_error,
                    output,
                    diff,
                } => transcript.finish_tool(&id, is_error, output, diff),
                AgentEvent::Error { message } => transcript.push_notice(message, true),
                AgentEvent::Subagent {
                    parent_tool_use_id,
                    event,
                } => self.apply_subagent_event(parent_tool_use_id, *event, cx),
                _ => {}
            }
        }
        self.cancel_running_tools();
        for subagent in self.subagents.values_mut() {
            subagent.transcript.cancel_running_tools();
            subagent.status.get_or_insert(DoneStatus::Completed);
        }
        self.metadata.fork_context = fork_context(&self.transcript.serialize(cx));
        let notice = i18n::format(
            SharedString::new_static(CONTINUED_NOTICE),
            &[("source", &source)],
        );
        self.transcript.push_notice(notice.to_string(), false);
        self.save_metadata(cx);
        self.save_entries(cx);
        cx.notify();
    }

    fn append_text(&mut self, text: &str, thinking: bool, cx: &mut Context<Self>) {
        let languages = self.languages.clone();
        self.transcript.append_text(text, thinking, &languages, cx);
        self.save_entries(cx);
    }

    pub(crate) fn push_notice(&mut self, text: String, is_error: bool, cx: &mut Context<Self>) {
        self.transcript.push_notice(text, is_error);
        self.save_entries(cx);
        cx.notify();
    }

    fn cancel_running_tools(&mut self) {
        self.transcript.cancel_running_tools();
    }

    /// Replaces the title taken from the first message, but never one the user typed.
    fn adopt_agent_title(&mut self, title: &str, cx: &mut Context<Self>) {
        let first_prompt = self
            .transcript
            .entries
            .iter()
            .find_map(|entry| match entry {
                Entry::User { text, .. } => Some(text),
                _ => None,
            });
        if !first_prompt.is_some_and(|text| self.metadata.title == title_from(text)) {
            return;
        }
        let title = agent_title(title);
        if title.is_empty() || title == self.metadata.title {
            return;
        }
        self.metadata.title = title;
        self.save_metadata(cx);
    }

    fn set_native_session_id(&mut self, session_id: String, cx: &mut Context<Self>) {
        if self.metadata.native_session_id.as_deref() != Some(session_id.as_str()) {
            self.metadata.native_session_id = Some(session_id);
            if self.native_fork_prompt.take().is_some() {
                self.metadata.fork_source = None;
                self.metadata.fork_context = None;
                for message in std::mem::take(&mut self.native_fork_errors) {
                    self.push_notice(message, true, cx);
                }
            }
            self.save_metadata(cx);
        }
    }

    fn touch(&mut self, cx: &mut Context<Self>) {
        self.metadata.updated_at = now();
        self.save_metadata(cx);
        self.save_entries(cx);
    }

    fn saved_transcript(&self, cx: &App) -> SavedTranscript {
        let mut subagents: Vec<SavedSubagent> = self
            .subagents
            .iter()
            .map(|(id, subagent)| SavedSubagent {
                id: id.clone(),
                entries: subagent.transcript.serialize(cx),
                status: subagent.status,
            })
            .collect();
        subagents.sort_by(|left, right| left.id.cmp(&right.id));
        SavedTranscript::Current {
            entries: self.transcript.serialize(cx),
            subagents,
            queue: self.queue.iter().map(|queued| (&queued.prompt).into()).collect(),
        }
    }

    fn save_metadata(&mut self, cx: &mut Context<Self>) {
        if self.deleted {
            return;
        }
        cx.emit(SessionEvent::MetadataChanged);
        let metadata = self.metadata.clone();
        let directory = self.directory.clone();
        let previous_save = std::mem::replace(&mut self.save_metadata_task, Task::ready(()));
        self.save_metadata_task = cx.background_spawn(async move {
            previous_save.await;
            let path = metadata_path(&directory, &metadata.id);
            serde_json::to_vec_pretty(&metadata)
                .map_err(anyhow::Error::from)
                .and_then(|json| write_atomically(&path, &json))
                .log_err();
        });
    }

    fn save_entries(&mut self, cx: &mut Context<Self>) {
        if self.deleted {
            return;
        }
        let directory = self.directory.clone();
        self.save_entries_task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            let Some((id, entries)) = this
                .read_with(cx, |this, cx| {
                    (!this.deleted).then(|| (this.metadata.id.clone(), this.saved_transcript(cx)))
                })
                .log_err()
                .flatten()
            else {
                return;
            };
            cx.background_spawn(async move {
                serde_json::to_vec(&entries)
                    .map_err(anyhow::Error::from)
                    .and_then(|json| write_atomically(&entries_path(&directory, &id), &json))
                    .log_err();
            })
            .await;
        });
    }
}

#[derive(Clone)]
pub struct SessionSummary {
    pub id: String,
    pub kind: AgentKind,
    pub title: SharedString,
    pub created_at: i64,
    pub updated_at: i64,
    pub working: bool,
    pub needs_input: bool,
    pub pinned: bool,
    pub parent_id: Option<String>,
    pub side_chat: bool,
    pub outcome: Option<ChatOutcome>,
    pub unseen: bool,
    pub project_root: PathBuf,
    pub branch: Option<String>,
    pub native_session_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatSort {
    #[default]
    Updated,
    Created,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatListPrefs {
    pub all_projects: bool,
    pub group_by_project: bool,
    pub sort: ChatSort,
    pub compact_rows: bool,
    pub collapsed_sections: Vec<String>,
    pub favorite_models: Vec<String>,
    pub dictation_device: Option<String>,
    /// 1.1.11 kept saved messages here; they now live in their own file.
    #[serde(rename = "saved_messages", skip_serializing)]
    pub(crate) legacy_saved_messages: Vec<SavedMessage>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedMessage {
    pub chat_id: String,
    /// Where the message was in its chat; checked against `text` because chats can drop messages.
    pub entry: usize,
    pub text: String,
    pub from_agent: bool,
    pub saved_at: i64,
}

impl ChatListPrefs {
    pub fn favorite_key(kind: AgentKind, model_id: &str) -> String {
        format!("{}:{model_id}", kind.label())
    }

    pub fn is_favorite(&self, kind: AgentKind, model_id: &str) -> bool {
        self.favorite_models
            .contains(&Self::favorite_key(kind, model_id))
    }
}

pub enum StoreEvent {
    SessionFinished { id: String, errored: bool },
    SessionNeedsInput { id: String },
}

fn chat_list_path(directory: &Path) -> PathBuf {
    directory.join("chat_list.json")
}

fn saved_messages_path(directory: &Path) -> PathBuf {
    directory.join("saved_messages.json")
}

fn fork_title(parent_title: &str, sessions: &[SessionMetadata]) -> String {
    if parent_title.is_empty() {
        return String::new();
    }
    let base = parent_title
        .strip_suffix(')')
        .and_then(|rest| rest.rsplit_once(" ("))
        .filter(|(_, number)| number.parse::<u32>().is_ok_and(|number| number >= 2))
        .map_or(parent_title, |(base, _)| base);
    (2..)
        .map(|number| format!("{base} ({number})"))
        .find(|title| !sessions.iter().any(|session| session.title == *title))
        .unwrap_or_default()
}

fn fork_context(entries: &[SerializedEntry]) -> Option<String> {
    let history: Vec<serde_json::Value> = entries
        .iter()
        .filter_map(|entry| match entry {
            SerializedEntry::User { text, .. } => Some(("user", text.clone())),
            SerializedEntry::Assistant { text, .. } => Some(("assistant", text.clone())),
            SerializedEntry::Tool { call, output, .. } => Some((
                "assistant",
                format!(
                    "Tool: {}\n{}",
                    serde_json::to_string(call).unwrap_or_default(),
                    output.clone().unwrap_or_default()
                ),
            )),
            SerializedEntry::Thinking { .. }
            | SerializedEntry::Notice { .. }
            | SerializedEntry::Image { .. } => None,
        })
        .filter(|(_, text)| !text.trim().is_empty())
        .map(|(role, text)| serde_json::json!({ "role": role, "text": text }))
        .collect();
    (!history.is_empty()).then(|| serde_json::Value::Array(history).to_string())
}

pub(crate) struct ShellEnvLoaded(
    pub futures::future::Shared<futures::channel::oneshot::Receiver<()>>,
);

impl Global for ShellEnvLoaded {}

pub(crate) fn shell_env_loaded(cx: &App) -> impl std::future::Future<Output = ()> + Send + 'static {
    let loaded = cx.try_global::<ShellEnvLoaded>().map(|loaded| loaded.0.clone());
    async move {
        if let Some(loaded) = loaded {
            loaded.await.ok();
        }
    }
}

/// Agent CLIs and their config folders come from the login shell's environment, which loads after startup.
fn spawn_agent_work<T: 'static, F, R>(
    cx: &Context<T>,
    work: F,
) -> Task<std::result::Result<R, gpui_tokio::JoinError>>
where
    F: std::future::Future<Output = R> + Send + 'static,
    R: Send + 'static,
{
    let env_loaded = shell_env_loaded(cx);
    gpui_tokio::Tokio::spawn(cx, async move {
        env_loaded.await;
        work.await
    })
}

fn shell_env_is_loaded(cx: &App) -> bool {
    cx.try_global::<ShellEnvLoaded>()
        .is_none_or(|loaded| loaded.0.peek().is_some())
}

struct GlobalAgentStore(Entity<AgentStore>);

impl Global for GlobalAgentStore {}

impl EventEmitter<StoreEvent> for AgentStore {}

/// Owns every live chat, so an agent keeps running after its tab is closed.
pub use self::store::AgentStore;
pub(crate) use self::store::{load_saved_chats, project_agent_activity, quit_warning, reasoning_for_model, stop_all_agents, working_agent_count, AgentUpdateStatus};

#[cfg(test)]
pub(crate) use store::tests;

