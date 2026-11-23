use agent_harness::{CancellationToken, PermissionMode, SteerMessage, UserInputAnswer, UserInputQuestion};
use gpui::Task;

pub(crate) enum RunInput {
    Event(Result<agent_harness::AgentEvent, agent_harness::HarnessError>),
    Question {
        questions: Vec<UserInputQuestion>,
        responder: tokio::sync::oneshot::Sender<Vec<UserInputAnswer>>,
    },
    Ended,
}

pub(crate) struct ActiveRun {
    pub generation: u64,
    pub round_done: bool,
    pub steering: tokio::sync::mpsc::Sender<SteerMessage>,
    pub interrupt: CancellationToken,
    pub permission: tokio::sync::watch::Sender<PermissionMode>,
    pub _events: Task<()>,
}

pub enum SessionEvent {
    MetadataChanged,
    SettingsChanged,
    Finished { errored: bool },
    NeedsInput,
}
