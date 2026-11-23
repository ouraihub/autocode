use std::time::Instant;

use agent_harness::{UserInputAnswer, UserInputQuestion};

pub struct PendingQuestion {
    pub questions: Vec<UserInputQuestion>,
    pub(crate) responder: Option<tokio::sync::oneshot::Sender<Vec<UserInputAnswer>>>,
    pub(crate) generation: u64,
    pub(crate) deadline: Option<Instant>,
}

impl PendingQuestion {
    pub fn id(&self) -> &str {
        self.questions.first().map(|question| question.id.as_str()).unwrap_or_default()
    }

    pub fn is_permission(&self) -> bool {
        !self.questions.is_empty()
            && self.questions.iter().all(|question| {
                question.header == agent_harness::claude::PERMISSION_HEADER
            })
    }

    pub(crate) fn is_tool_permission(&self) -> bool {
        self.is_permission()
            && self.questions.iter().all(|question| {
                question.options == [
                    agent_harness::claude::PERMISSION_ALLOW,
                    agent_harness::claude::PERMISSION_DENY,
                ]
            })
    }
    pub(crate) fn expired(&self, now: Instant) -> bool {
        self.deadline.is_some_and(|deadline| now >= deadline)
    }
}

pub(crate) async fn agent_stopped_waiting(
    mut agent: tokio::sync::oneshot::Sender<Vec<UserInputAnswer>>,
    chat: tokio::sync::oneshot::Receiver<Vec<UserInputAnswer>>,
) -> bool {
    let answered = {
        let withdrawn = std::pin::pin!(agent.closed());
        match futures::future::select(chat, withdrawn).await {
            futures::future::Either::Left((answer, _)) => Some(answer),
            futures::future::Either::Right(_) => None,
        }
    };
    match answered {
        Some(Ok(answers)) => {
            if agent.send(answers).is_err() {
                log::debug!("agent stopped waiting for an answer");
            }
            false
        }
        Some(Err(_)) => false,
        None => true,
    }
}

#[derive(Clone)]
pub struct BackgroundTask {
    pub task_id: String,
    pub tool_use_id: Option<String>,
    pub kind: agent_harness::BackgroundTaskKind,
    pub description: String,
    pub started_at: Instant,
}
