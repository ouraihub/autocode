use super::*;

pub struct QueuedMessage { pub id: u64, pub prompt: Prompt }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus { Running, Completed, Failed, Canceled }
pub struct ToolEntry { pub id: String, pub call: ToolCall, pub status: ToolStatus, pub output: Option<SharedString>, pub diff: Option<agent_harness::ToolDiff> }
pub enum Entry { User { text: SharedString, at: i64, attachments: Vec<PathBuf>, skills: Vec<SkillRef>, undelivered: bool }, Assistant { markdown: Entity<Markdown>, at: i64 }, Thinking(Entity<Markdown>), Tool(ToolEntry), Notice { text: SharedString, is_error: bool }, Image { path: PathBuf } }

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SerializedEntry { User { text: String, #[serde(default)] at: i64, #[serde(default, skip_serializing_if = "Vec::is_empty")] attachments: Vec<PathBuf>, #[serde(default, skip_serializing_if = "Vec::is_empty")] skills: Vec<SkillRef>, #[serde(default, skip_serializing_if = "std::ops::Not::not")] undelivered: bool }, Assistant { text: String, #[serde(default)] at: i64 }, Thinking { text: String }, Tool { id: String, call: ToolCall, status: ToolStatus, #[serde(default)] output: Option<String>, #[serde(default, skip_serializing_if = "Option::is_none")] diff: Option<agent_harness::ToolDiff> }, Notice { text: String, is_error: bool }, Image { path: PathBuf } }

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub enum SavedTranscript { EntriesOnly(Vec<SerializedEntry>), Current { entries: Vec<SerializedEntry>, #[serde(default)] subagents: Vec<SavedSubagent>, #[serde(default, skip_serializing_if = "Vec::is_empty")] queue: Vec<SavedPrompt> } }
#[derive(Serialize, Deserialize)]
pub struct SavedPrompt { pub text: String, #[serde(default, skip_serializing_if = "Vec::is_empty")] pub attachments: Vec<PathBuf>, #[serde(default, skip_serializing_if = "Vec::is_empty")] pub skills: Vec<SkillRef> }
#[derive(Serialize, Deserialize)]
pub struct SavedSubagent { pub id: String, pub entries: Vec<SerializedEntry>, #[serde(default)] pub status: Option<DoneStatus> }

impl From<SavedPrompt> for Prompt { fn from(saved: SavedPrompt) -> Self { Self { text: saved.text, attachments: saved.attachments, skills: saved.skills } } }
impl From<&Prompt> for SavedPrompt { fn from(prompt: &Prompt) -> Self { Self { text: prompt.text.clone(), attachments: prompt.attachments.clone(), skills: prompt.skills.clone() } } }

#[derive(Default)]
pub struct Transcript { pub entries: Vec<Entry>, pub text_block_open: bool }

impl Transcript {
    pub fn load(entries: Vec<SerializedEntry>, languages: &Arc<LanguageRegistry>, cx: &mut App) -> Self { Self { entries: entries.into_iter().filter(|entry| !matches!(entry, SerializedEntry::Thinking { text } if text.trim().is_empty())).map(|entry| deserialize_entry(entry, languages, cx)).collect(), text_block_open: false } }
    pub fn append_text(&mut self, text: &str, thinking: bool, languages: &Arc<LanguageRegistry>, cx: &mut App) { if text.is_empty() { return; } let extends = self.text_block_open && matches!((self.entries.last(), thinking), (Some(Entry::Assistant { .. }), false) | (Some(Entry::Thinking(_)), true)); if extends { match self.entries.last_mut() { Some(Entry::Assistant { markdown, at }) => { *at = now(); markdown.update(cx, |m, cx| m.append(text, cx)); }, Some(Entry::Thinking(markdown)) => markdown.update(cx, |m, cx| m.append(text, cx)), _ => {} } } else { let markdown = new_markdown(text.to_string(), languages, cx); self.entries.push(if thinking { Entry::Thinking(markdown) } else { Entry::Assistant { markdown, at: now() } }); self.text_block_open = true; } }
    pub fn push_user(&mut self, text: String, attachments: Vec<PathBuf>, skills: Vec<SkillRef>) { self.text_block_open = false; self.entries.push(Entry::User { text: text.into(), at: now(), attachments, skills, undelivered: false }); }
    pub fn push_notice(&mut self, text: String, is_error: bool) { self.text_block_open = false; self.entries.push(Entry::Notice { text: text.into(), is_error }); }
    pub fn tool_mut(&mut self, id: &str) -> Option<&mut ToolEntry> { self.entries.iter_mut().rev().find_map(|entry| match entry { Entry::Tool(tool) if tool.id == id => Some(tool), _ => None }) }
    pub fn tool(&self, id: &str) -> Option<&ToolEntry> { self.entries.iter().rev().find_map(|entry| match entry { Entry::Tool(tool) if tool.id == id => Some(tool), _ => None }) }
    pub fn start_tool(&mut self, id: String, call: ToolCall) { self.text_block_open = false; let diff = diff_for_call(&call); if let Some(tool) = self.tool_mut(&id) { tool.call = call; if diff.is_some() { tool.diff = diff; } } else { self.entries.push(Entry::Tool(ToolEntry { id, call, status: ToolStatus::Running, output: None, diff })); } }
    pub fn finish_tool(&mut self, id: &str, is_error: bool, output: Option<String>, diff: Option<agent_harness::ToolDiff>) { if let Some(tool) = self.tool_mut(id) { tool.status = if is_error { ToolStatus::Failed } else { ToolStatus::Completed }; tool.output = output.map(|output| truncate_output(output).into()); if diff.is_some() { tool.diff = diff; } } }
    pub fn cancel_running_tools(&mut self) { for entry in &mut self.entries { if let Entry::Tool(tool) = entry && tool.status == ToolStatus::Running { tool.status = ToolStatus::Canceled; } } }
    pub fn close_text_block(&mut self) {
        self.text_block_open = false;
    }

    pub fn serialize(&self, cx: &App) -> Vec<SerializedEntry> {
        self.entries.iter().map(|entry| match entry {
            Entry::User { text, at, attachments, skills, undelivered } => SerializedEntry::User {
                text: text.to_string(), at: *at, attachments: attachments.clone(),
                skills: skills.clone(), undelivered: *undelivered,
            },
            Entry::Assistant { markdown, at } => SerializedEntry::Assistant {
                text: markdown.read(cx).source().to_string(), at: *at,
            },
            Entry::Thinking(markdown) => SerializedEntry::Thinking {
                text: markdown.read(cx).source().to_string(),
            },
            Entry::Tool(tool) => SerializedEntry::Tool {
                id: tool.id.clone(), call: tool.call.clone(), status: tool.status,
                output: tool.output.as_ref().map(|output| output.to_string()), diff: tool.diff.clone(),
            },
            Entry::Notice { text, is_error } => SerializedEntry::Notice {
                text: text.to_string(), is_error: *is_error,
            },
            Entry::Image { path } => SerializedEntry::Image { path: path.clone() },
        }).collect()
    }
}
pub fn changes_nothing(call: &ToolCall) -> bool { matches!(call, ToolCall::ReadFile { .. } | ToolCall::Search { .. } | ToolCall::Glob { .. } | ToolCall::WebFetch { .. } | ToolCall::WebSearch { .. } | ToolCall::Todo { .. }) }

fn new_markdown(text: String, languages: &Arc<LanguageRegistry>, cx: &mut App) -> Entity<Markdown> { let languages = languages.clone(); cx.new(|cx| Markdown::new(text.into(), Some(languages), None, cx)) }
fn deserialize_entry(entry: SerializedEntry, languages: &Arc<LanguageRegistry>, cx: &mut App) -> Entry { match entry { SerializedEntry::User { text, at, attachments, skills, undelivered } => Entry::User { text: text.into(), at, attachments, skills, undelivered }, SerializedEntry::Assistant { text, at } => Entry::Assistant { markdown: new_markdown(text, languages, cx), at }, SerializedEntry::Thinking { text } => Entry::Thinking(new_markdown(text, languages, cx)), SerializedEntry::Tool { id, call, status, output, diff } => Entry::Tool(ToolEntry { diff: diff.or_else(|| diff_for_call(&call)), id, call, status: if status == ToolStatus::Running { ToolStatus::Canceled } else { status }, output: output.map(Into::into) }), SerializedEntry::Notice { text, is_error } => Entry::Notice { text: text.into(), is_error }, SerializedEntry::Image { path } => Entry::Image { path } } }

pub struct Subagent { pub transcript: Transcript, pub status: Option<DoneStatus>, pub started_at: Instant }
impl Subagent { pub fn new() -> Self { Self { transcript: Transcript::default(), status: None, started_at: Instant::now() } } pub fn entries(&self) -> &[Entry] { &self.transcript.entries } pub fn status(&self) -> Option<DoneStatus> { self.status } pub fn started_at(&self) -> Instant { self.started_at } }
