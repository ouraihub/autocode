use super::*;

pub struct AgentStore {
    directory: Arc<Path>,
    sessions: Vec<SessionMetadata>,
    live: HashMap<String, Entity<AgentSession>>,
    languages: Arc<LanguageRegistry>,
    harnesses: HashMap<AgentKind, Arc<dyn Harness>>,
    catalogs: HashMap<AgentKind, ModelCatalog>,
    unlisted_models: HashMap<AgentKind, Vec<String>>,
    defaults: HashMap<AgentKind, RunSettings>,
    plan_usage: HashMap<AgentKind, PlanUsageState>,
    commands: HashMap<(AgentKind, PathBuf), CommandCatalog>,
    skills: HashMap<(AgentKind, PathBuf), SkillCatalog>,
    accounts: HashMap<AgentKind, AccountsState>,
    list_prefs: ChatListPrefs,
    save_list_prefs_task: Task<()>,
    saved_messages: Vec<SavedMessage>,
    save_saved_messages_task: Task<()>,
    metadata_writes: HashMap<String, Task<()>>,
    save_defaults_task: Task<()>,
    load_task: Option<Shared<Task<()>>>,
    agent_updates: HashMap<AgentKind, AgentUpdate>,
    idle_sleep_guard: Option<Task<()>>,
}

#[derive(Clone)]
pub enum AgentUpdateStatus {
    Updating,
    Finished(SharedString),
    Failed {
        message: SharedString,
        reason: SharedString,
    },
}

struct AgentUpdate {
    status: Option<AgentUpdateStatus>,
    checked_at: Instant,
    _task: Task<()>,
}

#[derive(Default)]
struct ModelCatalog {
    models: Vec<Model>,
    fallback: Vec<Model>,
    loaded: bool,
    revision: u64,
    _task: Option<Task<()>>,
}

#[derive(Default)]
pub struct AccountsState {
    pub accounts: Vec<agent_harness::accounts::Account>,
    pub usage: HashMap<String, std::result::Result<PlanUsage, String>>,
    pub error: Option<SharedString>,
    pub loading: bool,
    pub switching: Option<String>,
    _listing: Option<Task<()>>,
    _activation: Option<Task<()>>,
}

fn accounts_dir() -> PathBuf {
    paths::data_dir().clone()
}

#[derive(Default)]
pub struct SkillCatalog {
    pub skills: Vec<agent_harness::Skill>,
    pub loaded: bool,
    fetched_at: Option<Instant>,
    _task: Option<Task<()>>,
}

#[derive(Default)]
pub struct CommandCatalog {
    pub commands: Vec<SlashCommand>,
    pub loaded: bool,
    pub error: Option<SharedString>,
    fetched_at: Option<Instant>,
    _task: Option<Task<()>>,
}

impl CommandCatalog {
    pub fn is_loading(&self) -> bool {
        self._task.is_some()
    }
}

#[derive(Default)]
pub struct PlanUsageState {
    pub usage: Option<PlanUsage>,
    pub error: Option<SharedString>,
    pub loading: bool,
    fetched_at: Option<Instant>,
    _task: Option<Task<()>>,
}

fn defaults_path(directory: &Path) -> PathBuf {
    directory.join("defaults.json")
}

const AGENT_UPDATE_INTERVAL: Duration = Duration::from_secs(12 * 60 * 60);
const AGENT_UPDATE_STATUS_SHOWN_FOR: Duration = Duration::from_secs(10);

fn agent_updates_path(directory: &Path) -> PathBuf {
    directory.join("agent_updates.json")
}

/// Recorded before updating, so an update that never finishes isn't retried on every new chat.
fn claim_update_attempt(path: &Path, kind: AgentKind) -> bool {
    static ATTEMPTS_FILE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _locked = ATTEMPTS_FILE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let mut attempts: HashMap<AgentKind, u64> = std::fs::read(path)
        .ok()
        .and_then(|json| serde_json::from_slice(&json).log_err())
        .unwrap_or_default();
    if attempts
        .get(&kind)
        .is_some_and(|last| now.saturating_sub(*last) < AGENT_UPDATE_INTERVAL.as_secs())
    {
        return false;
    }
    attempts.insert(kind, now);
    let saved = serde_json::to_vec(&attempts)
        .map_err(anyhow::Error::from)
        .and_then(|json| {
            if let Some(directory) = path.parent() {
                std::fs::create_dir_all(directory)?;
            }
            Ok(std::fs::write(path, json)?)
        });
    if let Err(error) = saved {
        log::warn!("couldn't record the agent update attempt: {error}");
    }
    true
}

/// The effort to switch to when the model doesn't offer the chat's, so what's sent matches the picker.
pub(crate) fn reasoning_for_model(
    settings: &RunSettings,
    models: &[Model],
) -> Option<Option<ReasoningLevel>> {
    let level = settings.reasoning?;
    let model = agent_harness::view::selected_catalog_model(models, settings.native_model())?;
    if model.reasoning_levels.contains(&level) {
        return None;
    }
    Some(agent_harness::view::clamp_reasoning(
        Some(level),
        &model.reasoning_levels,
    ))
}

pub(crate) fn working_agent_count(cx: &App) -> usize {
    cx.try_global::<GlobalAgentStore>()
        .map_or(0, |store| store.0.read(cx).working_count(cx))
}

pub(crate) fn load_saved_chats(cx: &mut App) {
    if let Some(store) = cx
        .try_global::<GlobalAgentStore>()
        .map(|store| store.0.clone())
    {
        store.update(cx, |store, cx| store.load_saved_chats(cx));
    }
}

pub(crate) fn stop_all_agents(cx: &mut App) {
    let Some(store) = cx
        .try_global::<GlobalAgentStore>()
        .map(|store| store.0.clone())
    else {
        return;
    };
    let sessions: Vec<Entity<AgentSession>> = store.read(cx).live.values().cloned().collect();
    for session in sessions {
        session.update(cx, |session, cx| {
            if session.is_working() {
                session.stop(cx);
            }
        });
    }
}

fn announce_activity_changes(store: &Entity<AgentStore>, cx: &mut App) {
    let Some(changes) = workspace::agent_activity_changes(cx) else {
        return;
    };
    let mut last_seen = Vec::new();
    cx.observe(store, move |store, cx| {
        let activities = store.read(cx).agent_activities(cx);
        if activities != last_seen {
            last_seen = activities;
            changes.update(cx, |_, cx| cx.notify());
        }
    })
    .detach();
}

pub(crate) fn project_agent_activity(
    roots: &[PathBuf],
    cx: &App,
) -> Option<workspace::AgentActivity> {
    cx.try_global::<GlobalAgentStore>()?
        .0
        .read(cx)
        .agent_activity_in(roots, cx)
}

pub(crate) fn quit_warning(cx: &App) -> Option<String> {
    let settings = AgentChatSettings::get_global(cx);
    if !settings.enabled || !settings.confirm_quit_while_working {
        return None;
    }
    match working_agent_count(cx) {
        0 => None,
        1 => Some(t!("An agent is still working.").to_string()),
        count => Some(t!("{count} agents are still working.", count = count).to_string()),
    }
}

impl AgentStore {
    fn agent_activity_in(&self, roots: &[PathBuf], cx: &App) -> Option<workspace::AgentActivity> {
        self.live
            .values()
            .map(|session| session.read(cx))
            .filter(|session| session.metadata.is_in(roots))
            .filter_map(|session| session.agent_activity())
            .max_by_key(|activity| *activity == workspace::AgentActivity::NeedsInput)
    }

    fn agent_activities(&self, cx: &App) -> Vec<(String, workspace::AgentActivity)> {
        let mut activities: Vec<_> = self
            .live
            .iter()
            .filter_map(|(id, session)| Some((id.clone(), session.read(cx).agent_activity()?)))
            .collect();
        activities.sort_by(|a, b| a.0.cmp(&b.0));
        activities
    }

    fn working_count(&self, cx: &App) -> usize {
        self.live
            .values()
            .filter(|session| {
                let session = session.read(cx);
                session.is_working() && !session.is_deleted()
            })
            .count()
    }

    pub fn global(languages: Arc<LanguageRegistry>, cx: &mut App) -> Entity<Self> {
        if let Some(store) = cx.try_global::<GlobalAgentStore>() {
            return store.0.clone();
        }
        let directory = paths::data_dir().join("agent_sessions");
        let store = cx.new(|cx| Self::new(directory.into(), languages, cx));
        cx.set_global(GlobalAgentStore(store.clone()));
        crate::notifications::watch(&store, cx);
        announce_activity_changes(&store, cx);
        store
    }

    fn new(directory: Arc<Path>, languages: Arc<LanguageRegistry>, cx: &mut Context<Self>) -> Self {
        let mut store = Self {
            directory,
            sessions: Vec::new(),
            live: HashMap::default(),
            languages,
            harnesses: HashMap::default(),
            catalogs: HashMap::default(),
            unlisted_models: HashMap::default(),
            defaults: HashMap::default(),
            plan_usage: HashMap::default(),
            commands: HashMap::default(),
            skills: HashMap::default(),
            accounts: HashMap::default(),
            list_prefs: ChatListPrefs::default(),
            save_list_prefs_task: Task::ready(()),
            saved_messages: Vec::new(),
            save_saved_messages_task: Task::ready(()),
            metadata_writes: HashMap::default(),
            save_defaults_task: Task::ready(()),
            load_task: None,
            agent_updates: HashMap::default(),
            idle_sleep_guard: None,
        };
        cx.observe_self(|store, cx| store.update_idle_sleep_guard(cx)).detach();
        cx.observe_global::<settings::SettingsStore>(|store, cx| {
            store.update_idle_sleep_guard(cx)
        })
        .detach();
        if AgentChatSettings::get_global(cx).enabled {
            store.load_saved_chats(cx);
        }
        store
    }

    fn has_agent_working_unattended(&self, cx: &App) -> bool {
        self.live.values().any(|session| {
            session.read(cx).agent_activity() == Some(workspace::AgentActivity::Working)
        })
    }

    fn update_idle_sleep_guard(&mut self, cx: &mut Context<Self>) {
        let keep_awake = AgentChatSettings::get_global(cx).prevent_idle_sleep
            && self.has_agent_working_unattended(cx);
        if keep_awake == self.idle_sleep_guard.is_some() {
            return;
        }
        self.idle_sleep_guard = keep_awake.then(|| {
            let guard = cx.prevent_idle_sleep(&t!("An agent is working"));
            cx.spawn(async move |_, _| match guard.await {
                Ok(_guard) => futures::future::pending::<()>().await,
                Err(error) => log::warn!("Failed to keep the computer awake: {error:#}"),
            })
        });
    }

    fn load_saved_chats(&mut self, cx: &mut Context<Self>) {
        if self.load_task.is_some() {
            return;
        }
        let load = cx.spawn({
            let directory = self.directory.clone();
            async move |this, cx| {
                let defaults_file = defaults_path(&directory);
                let list_file = chat_list_path(&directory);
                let saved_file = saved_messages_path(&directory);
                let (sessions, defaults, list_prefs, saved_messages) = cx
                    .background_spawn(async move {
                        let defaults = std::fs::read(&defaults_file).ok().and_then(|json| {
                            serde_json::from_slice::<HashMap<AgentKind, RunSettings>>(&json)
                                .log_err()
                        });
                        let list_prefs = std::fs::read(&list_file).ok().and_then(|json| {
                            serde_json::from_slice::<ChatListPrefs>(&json).log_err()
                        });
                        let saved_messages = std::fs::read(&saved_file).ok().and_then(|json| {
                            serde_json::from_slice::<Vec<SavedMessage>>(&json).log_err()
                        });
                        (
                            load_all_metadata(&directory),
                            defaults,
                            list_prefs,
                            saved_messages,
                        )
                    })
                    .await;
                let sessions = sessions.log_err().unwrap_or_default();
                this.update(cx, |this, cx| {
                    if let Some(defaults) = defaults {
                        this.defaults = defaults;
                    }
                    if let Some(mut list_prefs) = list_prefs {
                        let legacy = std::mem::take(&mut list_prefs.legacy_saved_messages);
                        this.list_prefs = list_prefs;
                        if saved_messages.is_none() && !legacy.is_empty() {
                            this.update_saved_messages(|saved| *saved = legacy, cx);
                            this.update_list_prefs(|_| {}, cx);
                        }
                    }
                    if let Some(saved_messages) = saved_messages {
                        this.saved_messages = saved_messages;
                    }
                    for metadata in sessions {
                        if !this.sessions.iter().any(|known| known.id == metadata.id) {
                            this.sessions.push(metadata);
                        }
                    }
                    this.sort();
                    cx.notify();
                })
                .log_err();
            }
        });
        self.load_task = Some(load.shared());
    }

    /// Waits for the saved chat list, which may still be loading at startup.
    pub fn open_saved_session(
        &mut self,
        id: SharedString,
        cx: &mut Context<Self>,
    ) -> Task<Result<Entity<AgentSession>>> {
        let loaded = self.load_task.clone();
        cx.spawn(async move |this, cx| {
            if let Some(loaded) = loaded {
                loaded.await;
            }
            this.update(cx, |this, cx| this.open_session(&id, cx))?
                .await
        })
    }

    pub(crate) fn harness(&mut self, kind: AgentKind) -> Arc<dyn Harness> {
        self.harnesses
            .entry(kind)
            .or_insert_with(|| kind.new_harness())
            .clone()
    }

    pub fn languages(&self) -> &Arc<LanguageRegistry> {
        &self.languages
    }

    pub fn models(&self, kind: AgentKind) -> &[Model] {
        match self.catalogs.get(&kind) {
            Some(catalog) if !catalog.models.is_empty() => &catalog.models,
            Some(catalog) => &catalog.fallback,
            None => &[],
        }
    }

    /// The catalog plus models chats used that it doesn't list, so a switch away can be undone.
    pub fn picker_models(&self, kind: AgentKind, selected: Option<&str>) -> Vec<Model> {
        let mut models = self.models(kind).to_vec();
        let remembered = self.unlisted_models.get(&kind).into_iter().flatten();
        for id in selected.into_iter().chain(remembered.map(String::as_str)) {
            if agent_harness::view::selected_catalog_model(&models, Some(id)).is_none() {
                models.push(Model {
                    id: id.to_owned(),
                    label: id.to_owned(),
                    description: None,
                    reasoning_levels: Vec::new(),
                    options: Vec::new(),
                });
            }
        }
        models
    }

    pub fn remember_unlisted_model(&mut self, kind: AgentKind, id: &str) {
        if agent_harness::view::selected_catalog_model(self.models(kind), Some(id)).is_some() {
            return;
        }
        let remembered = self.unlisted_models.entry(kind).or_default();
        if !remembered.iter().any(|remembered| remembered == id) {
            remembered.push(id.to_owned());
        }
    }

    pub(crate) fn discovered_models(&self, kind: AgentKind) -> Option<&[Model]> {
        self.catalogs
            .get(&kind)
            .filter(|catalog| catalog.loaded)
            .map(|catalog| catalog.models.as_slice())
    }

    pub(crate) fn models_loading(&self, kind: AgentKind) -> bool {
        self.catalogs.get(&kind).is_some_and(|catalog| catalog._task.is_some())
    }

    pub fn refresh_models(&mut self, kind: AgentKind, cx: &mut Context<Self>) {
        if let Some(catalog) = self.catalogs.get_mut(&kind) {
            catalog.loaded = false;
            catalog.revision = catalog.revision.wrapping_add(1);
            catalog._task = None;
        }
        self.ensure_models(kind, cx);
    }

    pub fn ensure_models(&mut self, kind: AgentKind, cx: &mut Context<Self>) {
        let harness = self.harness(kind);
        let env_loaded = shell_env_is_loaded(cx);
        let catalog = self.catalogs.entry(kind).or_default();
        if catalog.fallback.is_empty() && env_loaded {
            catalog.fallback = harness.fallback_models();
        }
        if catalog.loaded || catalog._task.is_some() {
            return;
        }
        let target = agent_harness::routing::backend_target_for_harness(kind.harness_id());
        let node = target.and_then(|_| crate::dsh_node_runtime(cx));
        let discovery = spawn_agent_work(cx, async move {
            let mut launch = LaunchContext::default();
            if let Some(target) = target {
                crate::engine_launch::apply(target, node, &mut launch).await
                    .map_err(|error| agent_harness::HarnessError::Protocol(error.to_string()))?;
            }
            harness.models(CatalogContext { launch: launch.launch, ..Default::default() }).await
        });
        let task = cx.spawn(async move |this, cx| {
            let models = match discovery.await {
                Ok(Ok(models)) => {
                    log::info!(
                        "{} model discovery found {} models, starting with {:?}",
                        kind.label(),
                        models.len(),
                        models.first().map(|model| &model.id)
                    );
                    models
                }
                Ok(Err(error)) => {
                    log::info!("{} model discovery failed: {error}", kind.label());
                    Vec::new()
                }
                Err(error) => {
                    log::error!("{} model discovery stopped: {error}", kind.label());
                    Vec::new()
                }
            };
            this.update(cx, |this, cx| {
                let fallback = this.harness(kind).fallback_models();
                let catalog = this.catalogs.entry(kind).or_default();
                catalog.fallback = fallback;
                catalog.loaded = true;
                catalog.models = models;
                catalog._task = None;
                cx.notify();
            })
            .log_err();
        });
        if let Some(catalog) = self.catalogs.get_mut(&kind) {
            catalog._task = Some(task);
        }
    }

    pub fn skills(&self, kind: AgentKind, cwd: &Path) -> Option<&SkillCatalog> {
        self.skills.get(&(kind, cwd.to_path_buf()))
    }

    pub fn refresh_skills(&mut self, kind: AgentKind, cwd: PathBuf, cx: &mut Context<Self>) {
        let key = (kind, cwd.clone());
        if self.skills.get(&key).is_some_and(|catalog| {
            catalog._task.is_some()
                || catalog
                    .fetched_at
                    .is_some_and(|fetched_at| fetched_at.elapsed() < CATALOG_REFRESH_INTERVAL)
        }) {
            return;
        }
        let harness = self.harness(kind);
        let discovery = spawn_agent_work(cx, async move { harness.skills(&cwd).await });
        let task = cx.spawn({
            let key = key.clone();
            async move |this, cx| {
                let skills = match discovery.await {
                    Ok(Ok(skills)) => skills,
                    Ok(Err(error)) => {
                        log::info!("{} skill discovery failed: {error}", kind.label());
                        Vec::new()
                    }
                    Err(error) => {
                        log::error!("{} skill discovery stopped: {error}", kind.label());
                        Vec::new()
                    }
                };
                this.update(cx, |this, cx| {
                    let catalog = this.skills.entry(key).or_default();
                    catalog._task = None;
                    catalog.loaded = true;
                    catalog.fetched_at = Some(Instant::now());
                    catalog.skills = skills;
                    cx.notify();
                })
                .log_err();
            }
        });
        self.skills.entry(key).or_default()._task = Some(task);
    }

    pub fn commands(&self, kind: AgentKind, cwd: &Path) -> Option<&CommandCatalog> {
        self.commands.get(&(kind, cwd.to_path_buf()))
    }

    pub fn refresh_commands(&mut self, kind: AgentKind, cwd: PathBuf, cx: &mut Context<Self>) {
        let key = (kind, cwd.clone());
        if self.commands.get(&key).is_some_and(|catalog| {
            catalog.is_loading()
                || catalog
                    .fetched_at
                    .is_some_and(|fetched_at| fetched_at.elapsed() < CATALOG_REFRESH_INTERVAL)
        }) {
            return;
        }
        let harness = self.harness(kind);
        let discovery = spawn_agent_work(cx, async move { harness.commands(&cwd).await });
        let task = cx.spawn({
            let key = key.clone();
            async move |this, cx| {
                let result = match discovery.await {
                    Ok(result) => result.map_err(|error| error.to_string()),
                    Err(error) => Err(error.to_string()),
                };
                this.update(cx, |this, cx| {
                    let catalog = this.commands.entry(key).or_default();
                    catalog._task = None;
                    catalog.loaded = true;
                    catalog.fetched_at = Some(Instant::now());
                    match result {
                        Ok(commands) => {
                            catalog.commands = commands;
                            catalog.error = None;
                        }
                        Err(error) => {
                            log::info!("{} command discovery failed: {error}", kind.label());
                            catalog.error = Some(error.into());
                        }
                    }
                    cx.notify();
                })
                .log_err();
            }
        });
        self.commands.entry(key).or_default()._task = Some(task);
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn skip_plan_usage(&mut self) {
        for kind in AgentKind::ALL {
            self.plan_usage.entry(kind).or_default().loading = true;
        }
    }

    pub fn accounts(&self, kind: AgentKind) -> Option<&AccountsState> {
        self.accounts.get(&kind)
    }

    pub fn refresh_accounts(&mut self, kind: AgentKind, cx: &mut Context<Self>) {
        if !kind.has_plan_usage() {
            return;
        }
        let state = self.accounts.entry(kind).or_default();
        if state.loading {
            return;
        }
        state.loading = true;
        let harness_id = kind.harness_id();
        let listing = spawn_agent_work(cx, async move {
            agent_harness::accounts::list(harness_id, &accounts_dir()).await
        });
        let http = cx.http_client();
        let task = cx.spawn(async move |this, cx| {
            let accounts = match listing.await {
                Ok(result) => result,
                Err(error) => Err(error.to_string()),
            };
            let others: Vec<String> = accounts
                .as_ref()
                .map(|accounts| {
                    accounts
                        .iter()
                        .filter(|account| !account.active)
                        .map(|account| account.id.clone())
                        .collect()
                })
                .unwrap_or_default();
            this.update(cx, |this, cx| {
                let state = this.accounts.entry(kind).or_default();
                state.loading = false;
                match accounts {
                    Ok(accounts) => {
                        state.accounts = accounts;
                        state.error = None;
                    }
                    Err(error) => state.error = Some(error.into()),
                }
                cx.notify();
            })
            .log_err();
            for account_id in others {
                let request = {
                    let account_id = account_id.clone();
                    let Ok(request) = this.update(cx, |_, cx| {
                        spawn_agent_work(cx, async move {
                            agent_harness::accounts::usage_request(
                                harness_id,
                                &account_id,
                                &accounts_dir(),
                            )
                            .await
                        })
                    }) else {
                        return;
                    };
                    request
                };
                let usage = fetch_plan_usage(harness_id, request, http.clone()).await;
                this.update(cx, |this, cx| {
                    this.accounts
                        .entry(kind)
                        .or_default()
                        .usage
                        .insert(account_id, usage);
                    cx.notify();
                })
                .log_err();
            }
        });
        if let Some(state) = self.accounts.get_mut(&kind) {
            state._listing = Some(task);
        }
        cx.notify();
    }

    pub fn activate_account(&mut self, kind: AgentKind, account_id: String, cx: &mut Context<Self>) {
        let state = self.accounts.entry(kind).or_default();
        if state.switching.is_some() {
            return;
        }
        state.switching = Some(account_id.clone());
        let harness_id = kind.harness_id();
        let activation = spawn_agent_work(cx, async move {
            agent_harness::accounts::activate(harness_id, &account_id, &accounts_dir()).await
        });
        let task = cx.spawn(async move |this, cx| {
            let result = match activation.await {
                Ok(result) => result,
                Err(error) => Err(error.to_string()),
            };
            this.update(cx, |this, cx| {
                let state = this.accounts.entry(kind).or_default();
                state.switching = None;
                if let Err(error) = result {
                    state.error = Some(error.into());
                }
                if let Some(usage) = this.plan_usage.get_mut(&kind) {
                    usage.fetched_at = None;
                }
                if let Some(catalog) = this.catalogs.get_mut(&kind) {
                    catalog.loaded = false;
                }
                this.refresh_plan_usage(kind, true, cx);
                this.ensure_models(kind, cx);
                this.refresh_accounts(kind, cx);
            })
            .log_err();
        });
        if let Some(state) = self.accounts.get_mut(&kind) {
            state._activation = Some(task);
        }
        cx.notify();
    }

    pub fn agent_update(&self, kind: AgentKind) -> Option<&AgentUpdateStatus> {
        self.agent_updates.get(&kind)?.status.as_ref()
    }

    pub fn update_agent_if_due(&mut self, kind: AgentKind, cx: &mut Context<Self>) {
        if self
            .agent_updates
            .get(&kind)
            .is_some_and(|update| update.checked_at.elapsed() < AGENT_UPDATE_INTERVAL)
        {
            return;
        }
        let harness = self.harness(kind);
        let attempts = agent_updates_path(&self.directory);
        let claim = spawn_agent_work(cx, {
            let harness = harness.clone();
            async move {
                let args = harness.update_args();
                let executable = harness.executable_path().filter(|_| !args.is_empty())?;
                claim_update_attempt(&attempts, kind).then_some((executable, args))
            }
        });
        let task = cx.spawn(async move |this, cx| {
            let Ok(Some((executable, args))) = claim.await else {
                return;
            };
            let Some(run) = this
                .update(cx, |this, cx| {
                    this.set_agent_update(kind, Some(AgentUpdateStatus::Updating), cx);
                    spawn_agent_work(
                        cx,
                        agent_harness::update::update_cli(executable, args, move || {
                            harness.executable_path()
                        }),
                    )
                })
                .log_err()
            else {
                return;
            };
            let label = kind.label();
            let failed = |reason: String| AgentUpdateStatus::Failed {
                message: t!("Couldn't update {agent}", agent = label),
                reason: reason.into(),
            };
            let status = match run.await {
                Ok(Ok(agent_harness::update::UpdateOutcome::Updated(Some(version)))) => {
                    AgentUpdateStatus::Finished(t!(
                        "{agent} updated to {version}",
                        agent = label,
                        version = version
                    ))
                }
                Ok(Ok(agent_harness::update::UpdateOutcome::Updated(None))) => {
                    AgentUpdateStatus::Finished(t!("{agent} updated", agent = label))
                }
                Ok(Ok(agent_harness::update::UpdateOutcome::UpToDate)) => {
                    AgentUpdateStatus::Finished(t!("{agent} is up to date", agent = label))
                }
                Ok(Err(error)) => failed(error.0),
                Err(error) => failed(error.to_string()),
            };
            this.update(cx, |this, cx| this.set_agent_update(kind, Some(status), cx))
                .log_err();
            cx.background_executor()
                .timer(AGENT_UPDATE_STATUS_SHOWN_FOR)
                .await;
            this.update(cx, |this, cx| this.set_agent_update(kind, None, cx))
                .log_err();
        });
        self.agent_updates.insert(
            kind,
            AgentUpdate {
                status: None,
                checked_at: Instant::now(),
                _task: task,
            },
        );
    }

    fn set_agent_update(
        &mut self,
        kind: AgentKind,
        status: Option<AgentUpdateStatus>,
        cx: &mut Context<Self>,
    ) {
        if let Some(update) = self.agent_updates.get_mut(&kind) {
            update.status = status;
            cx.notify();
        }
    }

    pub fn plan_usage(&self, kind: AgentKind) -> Option<&PlanUsageState> {
        self.plan_usage.get(&kind)
    }

    pub fn refresh_plan_usage(&mut self, kind: AgentKind, force: bool, cx: &mut Context<Self>) {
        if !kind.has_plan_usage() {
            return;
        }
        let state = self.plan_usage.entry(kind).or_default();
        let min_age = if force {
            USAGE_FORCED_MIN_INTERVAL
        } else {
            USAGE_REFRESH_INTERVAL
        };
        if state.loading
            || state
                .fetched_at
                .is_some_and(|fetched_at| fetched_at.elapsed() < min_age)
        {
            return;
        }
        state.loading = true;
        let harness_id = kind.harness_id();
        let request = spawn_agent_work(cx, async move {
            agent_harness::usage::usage_request(harness_id).await
        });
        let http = cx.http_client();
        let task = cx.spawn(async move |this, cx| {
            let result = fetch_plan_usage(harness_id, request, http).await;
            this.update(cx, |this, cx| {
                let state = this.plan_usage.entry(kind).or_default();
                state.loading = false;
                state.fetched_at = Some(Instant::now());
                state._task = None;
                match result {
                    Ok(usage) => {
                        state.usage = Some(usage);
                        state.error = None;
                    }
                    Err(error) => state.error = Some(error.into()),
                }
                cx.notify();
            })
            .log_err();
        });
        if let Some(state) = self.plan_usage.get_mut(&kind) {
            state._task = Some(task);
        }
    }

    fn new_chat_settings(&self, kind: AgentKind, cx: &App) -> RunSettings {
        let mut settings = self.defaults.get(&kind).cloned().unwrap_or_default();
        if let Some(target) = agent_harness::routing::backend_target_for_harness(kind.harness_id()) {
            if let Some(preferences) = crate::provider_services::ProviderServices::try_global(cx).and_then(|services| services.default_for(target)) {
                settings.model = preferences.default_model;
                settings.config_source = preferences.source;
            }
        }
        let chat_settings = AgentChatSettings::get_global(cx);
        if let Some(effort) = chat_settings.default_effort {
            settings.reasoning = Some(effort);
            if let Some(reasoning) = self
                .models_discovered(kind)
                .and_then(|models| reasoning_for_model(&settings, models))
            {
                settings.reasoning = reasoning;
            }
        }
        settings.permission = chat_settings.default_permission(kind);
        settings
    }

    fn remember_settings(
        &mut self,
        kind: AgentKind,
        settings: RunSettings,
        cx: &mut Context<Self>,
    ) {
        if self.defaults.get(&kind) == Some(&settings) {
            return;
        }
        self.defaults.insert(kind, settings);
        let defaults = self.defaults.clone();
        let path = defaults_path(&self.directory);
        let previous_save = std::mem::replace(&mut self.save_defaults_task, Task::ready(()));
        self.save_defaults_task = cx.background_spawn(async move {
            previous_save.await;
            serde_json::to_vec_pretty(&defaults)
                .map_err(anyhow::Error::from)
                .and_then(|json| write_atomically(&path, &json))
                .log_err();
        });
    }

    pub fn sessions_in(&self, roots: &[PathBuf], cx: &App) -> Vec<SessionSummary> {
        let all_projects = self.list_prefs.all_projects;
        let mut summaries: Vec<SessionSummary> = self
            .sessions
            .iter()
            .filter(|metadata| all_projects || metadata.is_in(roots))
            .map(|metadata| {
                let live = self.live.get(&metadata.id).map(|session| session.read(cx));
                SessionSummary {
                    id: metadata.id.clone(),
                    kind: metadata.kind,
                    title: display_title(&metadata.title),
                    created_at: metadata.created_at,
                    updated_at: metadata.updated_at,
                    working: live.is_some_and(|session| session.is_working()),
                    needs_input: live.is_some_and(|session| !session.pending_questions.is_empty()),
                    pinned: metadata.pinned,
                    parent_id: metadata.parent_id.clone(),
                    side_chat: metadata.side_chat,
                    outcome: metadata.outcome,
                    unseen: metadata.unseen,
                    project_root: metadata.project_root().to_path_buf(),
                    branch: metadata.branch.clone(),
                    native_session_id: metadata.native_session_id.clone(),
                }
            })
            .collect();
        if self.list_prefs.sort == ChatSort::Created {
            summaries.sort_by_key(|summary| std::cmp::Reverse(summary.created_at));
        }
        summaries
    }

    pub fn sessions_metadata(&self) -> impl Iterator<Item = &SessionMetadata> {
        self.sessions.iter()
    }

    pub fn list_prefs(&self) -> &ChatListPrefs {
        &self.list_prefs
    }

    pub fn update_list_prefs(&mut self, change: impl FnOnce(&mut ChatListPrefs), cx: &mut Context<Self>) {
        change(&mut self.list_prefs);
        let prefs = self.list_prefs.clone();
        let path = chat_list_path(&self.directory);
        let previous_save = std::mem::replace(&mut self.save_list_prefs_task, Task::ready(()));
        self.save_list_prefs_task = cx.background_spawn(async move {
            previous_save.await;
            serde_json::to_vec_pretty(&prefs)
                .map_err(anyhow::Error::from)
                .and_then(|json| write_atomically(&path, &json))
                .log_err();
        });
        cx.notify();
    }

    pub fn saved_messages(&self) -> &[SavedMessage] {
        &self.saved_messages
    }

    pub fn saved_text(&self, chat_id: &str, entry: usize) -> Option<&str> {
        self.saved_messages
            .iter()
            .find(|saved| saved.chat_id == chat_id && saved.entry == entry)
            .map(|saved| saved.text.as_str())
    }

    fn update_saved_messages(
        &mut self,
        change: impl FnOnce(&mut Vec<SavedMessage>),
        cx: &mut Context<Self>,
    ) {
        change(&mut self.saved_messages);
        let saved = self.saved_messages.clone();
        let path = saved_messages_path(&self.directory);
        let previous_save = std::mem::replace(&mut self.save_saved_messages_task, Task::ready(()));
        self.save_saved_messages_task = cx.background_spawn(async move {
            previous_save.await;
            serde_json::to_vec_pretty(&saved)
                .map_err(anyhow::Error::from)
                .and_then(|json| write_atomically(&path, &json))
                .log_err();
        });
        cx.notify();
    }

    pub fn toggle_saved_message(
        &mut self,
        chat_id: &str,
        entry: usize,
        text: String,
        from_agent: bool,
        cx: &mut Context<Self>,
    ) {
        let saved = self.saved_text(chat_id, entry) == Some(text.as_str());
        self.update_saved_messages(
            |saved_messages| {
                saved_messages
                    .retain(|message| !(message.chat_id == chat_id && message.entry == entry));
                if !saved {
                    saved_messages.push(SavedMessage {
                        chat_id: chat_id.to_string(),
                        entry,
                        text,
                        from_agent,
                        saved_at: now(),
                    });
                }
            },
            cx,
        );
    }

    pub fn update_session_metadata(
        &mut self,
        id: &str,
        change: impl FnOnce(&mut SessionMetadata),
        cx: &mut Context<Self>,
    ) {
        if let Some(session) = self.live.get(id).cloned() {
            session.update(cx, |session, cx| session.update_metadata(change, cx));
            return;
        }
        let Some(metadata) = self.sessions.iter_mut().find(|metadata| metadata.id == id) else {
            return;
        };
        change(metadata);
        let metadata = metadata.clone();
        let path = metadata_path(&self.directory, &metadata.id);
        let previous_write = self
            .metadata_writes
            .remove(&metadata.id)
            .unwrap_or_else(|| Task::ready(()));
        let write = cx.background_spawn(async move {
            previous_write.await;
            serde_json::to_vec_pretty(&metadata)
                .map_err(anyhow::Error::from)
                .and_then(|json| write_atomically(&path, &json))
                .log_err();
        });
        self.metadata_writes.insert(id.to_string(), write);
        self.sort();
        cx.notify();
    }

    pub fn rename_session(&mut self, id: &str, title: String, cx: &mut Context<Self>) {
        let title = title.trim().to_string();
        if title.is_empty() {
            return;
        }
        self.update_session_metadata(id, |metadata| metadata.title = title, cx);
    }

    pub fn set_pinned(&mut self, id: &str, pinned: bool, cx: &mut Context<Self>) {
        self.update_session_metadata(id, |metadata| metadata.pinned = pinned, cx);
    }

    pub fn fork_session(
        &mut self,
        id: &str,
        side_chat: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<Entity<AgentSession>>> {
        let opening = self.open_session(id, cx);
        cx.spawn(async move |this, cx| {
            let parent = opening.await?;
            this.update(cx, |this, cx| {
                let (parent_metadata, mut entries, parent_working) =
                    parent.read_with(cx, |parent, cx| {
                        let mut entries = parent.transcript.serialize(cx);
                        if parent.is_working()
                            && let Some(last_user) = entries
                                .iter()
                                .rposition(|entry| matches!(entry, SerializedEntry::User { .. }))
                        {
                            entries.truncate(last_user);
                        }
                        (parent.metadata.clone(), entries, parent.is_working())
                    });
                let mut metadata = SessionMetadata::new(
                    parent_metadata.kind,
                    parent_metadata.cwd.clone(),
                    parent_metadata.settings.clone(),
                );
                metadata.parent_id = Some(parent_metadata.id.clone());
                metadata.settings.permission = parent_metadata.settings.permission;
                metadata.side_chat = side_chat;
                metadata.project_root = parent_metadata.project_root.clone();
                metadata.branch = parent_metadata.branch.clone();
                metadata.fork_context = fork_context(&entries);
                if !parent_working {
                    metadata.fork_source = parent_metadata.native_session_id.clone();
                }
                let parent_title = if parent_metadata.title.is_empty() {
                    NEW_CHAT_TITLE.to_string()
                } else {
                    parent_metadata.title.clone()
                };
                let template = if side_chat {
                    entries.clear();
                    SIDE_CHAT_NOTICE
                } else {
                    metadata.title = fork_title(&parent_metadata.title, &this.sessions);
                    FORKED_NOTICE
                };
                let notice = i18n::format(
                    SharedString::new_static(template),
                    &[("title", &parent_title)],
                );
                entries.push(SerializedEntry::Notice {
                    text: notice.to_string(),
                    is_error: false,
                });
                Ok(this.insert_session(metadata, SavedTranscript::EntriesOnly(entries), cx))
            })?
        })
    }

    fn insert_session(
        &mut self,
        metadata: SessionMetadata,
        saved: SavedTranscript,
        cx: &mut Context<Self>,
    ) -> Entity<AgentSession> {
        let languages = self.languages.clone();
        let directory = self.directory.clone();
        let harness = self.harness(metadata.kind);
        let session = cx.new(|cx| {
            let mut session =
                AgentSession::new(directory, harness, metadata.clone(), saved, languages, cx);
            session.touch(cx);
            session
        });
        self.sessions.push(metadata);
        self.sort();
        self.track(session.clone(), cx);
        session
    }

    /// Chats the agent's own app saved for `cwd`, minus the ones autocode runs itself.
    pub fn external_sessions(
        &mut self,
        kind: AgentKind,
        cwd: PathBuf,
        cx: &mut Context<Self>,
    ) -> Task<Result<Vec<ExternalSession>>> {
        let harness = self.harness(kind);
        let listing = spawn_agent_work(cx, async move { harness.external_sessions(&cwd).await });
        cx.spawn(async move |this, cx| {
            let sessions = listing.await??;
            this.read_with(cx, |this, _| {
                let own: HashSet<&str> = this
                    .sessions
                    .iter()
                    .filter_map(|metadata| metadata.native_session_id.as_deref())
                    .collect();
                sessions
                    .into_iter()
                    .filter(|session| !own.contains(session.id.as_str()))
                    .collect()
            })
        })
    }

    /// The agent copies the saved chat on the first message, so the original stays as it was.
    pub fn continue_external(
        &mut self,
        kind: AgentKind,
        cwd: PathBuf,
        external: ExternalSession,
        cx: &mut Context<Self>,
    ) -> Task<Result<Entity<AgentSession>>> {
        let harness = self.harness(kind);
        let history = spawn_agent_work(cx, {
            let cwd = cwd.clone();
            let id = external.id.clone();
            async move { harness.external_history(&cwd, &id).await }
        });
        cx.spawn(async move |this, cx| {
            let events = history.await??;
            this.update(cx, |this, cx| {
                let mut metadata =
                    SessionMetadata::new(kind, cwd, this.new_chat_settings(kind, cx));
                metadata.title = agent_title(&external.title);
                metadata.fork_source = Some(external.id);
                let session =
                    this.insert_session(metadata, SavedTranscript::EntriesOnly(Vec::new()), cx);
                session.update(cx, |session, cx| {
                    session.load_external_history(events, kind.label().to_owned(), cx)
                });
                session
            })
        })
    }

    pub fn create_session(
        &mut self,
        kind: AgentKind,
        cwd: PathBuf,
        cx: &mut Context<Self>,
    ) -> Entity<AgentSession> {
        self.create_session_in(kind, cwd, None, None, cx)
    }

    pub fn create_session_in(
        &mut self,
        kind: AgentKind,
        cwd: PathBuf,
        project_root: Option<PathBuf>,
        branch: Option<String>,
        cx: &mut Context<Self>,
    ) -> Entity<AgentSession> {
        let mut metadata = SessionMetadata::new(kind, cwd, self.new_chat_settings(kind, cx));
        metadata.project_root = project_root.filter(|root| *root != metadata.cwd);
        metadata.branch = branch;
        let languages = self.languages.clone();
        let directory = self.directory.clone();
        let harness = self.harness(kind);
        let session = cx.new(|cx| {
            AgentSession::new(
                directory,
                harness,
                metadata.clone(),
                SavedTranscript::EntriesOnly(Vec::new()),
                languages,
                cx,
            )
        });
        self.sessions.push(metadata);
        self.sort();
        self.track(session.clone(), cx);
        session
    }

    pub fn open_session(
        &mut self,
        id: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<Entity<AgentSession>>> {
        if let Some(session) = self.live.get(id) {
            return Task::ready(Ok(session.clone()));
        }
        let Some(metadata) = self
            .sessions
            .iter()
            .find(|metadata| metadata.id == id)
            .cloned()
        else {
            return Task::ready(Err(anyhow::anyhow!("chat {id} not found")));
        };
        let directory = self.directory.clone();
        cx.spawn(async move |this, cx| {
            let path = entries_path(&directory, &metadata.id);
            let entries = cx
                .background_spawn(async move {
                    match std::fs::read(&path) {
                        Ok(json) => serde_json::from_slice::<SavedTranscript>(&json)
                            .context("failed to parse the chat transcript"),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            Ok(SavedTranscript::EntriesOnly(Vec::new()))
                        }
                        Err(error) => Err(error).context("failed to read the chat transcript"),
                    }
                })
                .await?;
            this.update(cx, |this, cx| {
                if let Some(session) = this.live.get(&metadata.id) {
                    return Ok(session.clone());
                }
                if !this.sessions.iter().any(|known| known.id == metadata.id) {
                    anyhow::bail!("chat {} was deleted", metadata.id);
                }
                let languages = this.languages.clone();
                let harness = this.harness(metadata.kind);
                let pending_write = this.metadata_writes.remove(&metadata.id);
                let session = cx.new(|cx| {
                    let mut session =
                        AgentSession::new(directory, harness, metadata, entries, languages, cx);
                    if let Some(pending_write) = pending_write {
                        session.save_metadata_task = pending_write;
                    }
                    session
                });
                this.track(session.clone(), cx);
                Ok(session)
            })?
        })
    }

    pub fn delete_session(&mut self, id: &str, cx: &mut Context<Self>) {
        let mut pending_writes = self
            .live
            .remove(id)
            .map(|session| session.update(cx, |session, cx| session.close_for_deletion(cx)))
            .unwrap_or_default();
        pending_writes.extend(self.metadata_writes.remove(id));
        self.sessions.retain(|metadata| metadata.id != id);
        if self
            .saved_messages
            .iter()
            .any(|message| message.chat_id == id)
        {
            self.update_saved_messages(|saved| saved.retain(|message| message.chat_id != id), cx);
        }
        let id = id.to_string();
        let directory = self.directory.clone();
        cx.background_spawn(async move {
            futures::future::join_all(pending_writes).await;
            let _guard = FILE_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for path in [
                metadata_path(&directory, &id),
                entries_path(&directory, &id),
            ] {
                match std::fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => log::error!("failed to delete {}: {error}", path.display()),
                }
            }
        })
        .detach();
        cx.notify();
    }

    pub fn models_discovered(&self, kind: AgentKind) -> Option<&[Model]> {
        self.catalogs
            .get(&kind)
            .filter(|catalog| catalog.loaded && !catalog.models.is_empty())
            .map(|catalog| catalog.models.as_slice())
    }

    fn track(&mut self, session: Entity<AgentSession>, cx: &mut Context<Self>) {
        let id = session.read(cx).metadata.id.clone();
        cx.observe(&session, |_, _, cx| cx.notify()).detach();
        cx.subscribe(&session, |this, session, event, cx| match event {
            SessionEvent::MetadataChanged => {
                let metadata = session.read(cx).metadata.clone();
                // A native fork copies the parent as it is at the fork's first run, so it must not have moved on.
                let stale_forks: Vec<String> = this
                    .sessions
                    .iter()
                    .filter(|fork| {
                        fork.parent_id.as_deref() == Some(metadata.id.as_str())
                            && fork.fork_source.is_some()
                            && fork.native_session_id.is_none()
                            && metadata.updated_at >= fork.created_at
                    })
                    .map(|fork| fork.id.clone())
                    .collect();
                if let Some(known) = this
                    .sessions
                    .iter_mut()
                    .find(|known| known.id == metadata.id)
                {
                    *known = metadata;
                }
                for fork in stale_forks {
                    this.update_session_metadata(&fork, |fork| fork.fork_source = None, cx);
                }
                this.sort();
                cx.notify();
            }
            SessionEvent::SettingsChanged => {
                let session = session.read(cx);
                let (kind, settings) = (session.kind(), session.settings().clone());
                this.remember_settings(kind, settings, cx);
            }
            SessionEvent::Finished { errored } => {
                let id = session.read(cx).metadata.id.clone();
                cx.emit(StoreEvent::SessionFinished {
                    id,
                    errored: *errored,
                });
            }
            SessionEvent::NeedsInput => {
                let id = session.read(cx).metadata.id.clone();
                cx.emit(StoreEvent::SessionNeedsInput { id });
            }
        })
        .detach();
        self.live.insert(id, session);
        cx.notify();
    }

    fn sort(&mut self) {
        self.sessions
            .sort_by_key(|metadata| std::cmp::Reverse(metadata.updated_at));
    }
}

async fn fetch_plan_usage(
    harness_id: HarnessId,
    request: Task<
        std::result::Result<
            std::result::Result<agent_harness::usage::UsageRequest, String>,
            gpui_tokio::JoinError,
        >,
    >,
    http: Arc<dyn http_client::HttpClient>,
) -> std::result::Result<PlanUsage, String> {
    use futures::AsyncReadExt as _;
    let request = request.await.map_err(|error| error.to_string())??;
    let mut builder = http_client::Request::builder()
        .method(http_client::Method::GET)
        .uri(request.url);
    for (name, value) in &request.headers {
        builder = builder.header(*name, value);
    }
    let unavailable =
        |error: anyhow::Error| t!("Usage unavailable: {error}", error = error).to_string();
    let http_request = builder
        .body(http_client::AsyncBody::empty())
        .map_err(|error| unavailable(error.into()))?;
    let mut response = http.send(http_request).await.map_err(unavailable)?;
    if !response.status().is_success() {
        return Err(agent_harness::usage::status_message(
            response.status().as_u16(),
        ));
    }
    let mut body = Vec::new();
    response
        .body_mut()
        .read_to_end(&mut body)
        .await
        .map_err(|error| unavailable(error.into()))?;
    let json: serde_json::Value =
        serde_json::from_slice(&body).map_err(|error| unavailable(error.into()))?;
    request
        .parse(harness_id, &json)
        .ok_or_else(|| t!("Usage unavailable: unexpected response").to_string())
}

fn load_all_metadata(directory: &Path) -> Result<Vec<SessionMetadata>> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).context("failed to list saved chats"),
    };
    let mut sessions = Vec::new();
    for entry in entries {
        let path = entry?.path();
        let is_metadata = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".meta.json"));
        if !is_metadata {
            continue;
        }
        let parsed = std::fs::read(&path)
            .map_err(anyhow::Error::from)
            .and_then(|json| Ok(serde_json::from_slice::<SessionMetadata>(&json)?))
            .with_context(|| format!("failed to load {}", path.display()));
        if let Some(metadata) = parsed.log_err() {
            sessions.push(metadata);
        }
    }
    Ok(sessions)
}

#[cfg(test)]
#[path = "tests.rs"]
pub(crate) mod tests;
