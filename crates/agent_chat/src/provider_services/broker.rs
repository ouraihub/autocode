//! `GpuiCredentialBroker` routes `CredentialStore` requests to the GPUI
//! platform credential API through a bounded channel consumed by a foreground
//! task. See `docs/03-详细设计.md` §4.2 for the contract.
//!
//! Lifecycle:
//!
//! 1. Call `spawn_credential_broker(cx)` once during `agent_chat::init`.
//! 2. The returned `GpuiCredentialBroker` is cheap to clone and `Send + Sync`;
//!    hand it to every Tokio task that needs credentials.
//! 3. Keep the returned `gpui::Task` alive (store it on a global entity).
//!    Dropping the task stops the pump; in-flight requests return
//!    `CredentialError::Unavailable`.

use anyhow::anyhow;
use async_trait::async_trait;
use futures::channel::{mpsc, oneshot};
use futures::{SinkExt as _, StreamExt as _};
use gpui::{App, Task};
use model_provider::{CredentialError, CredentialResult, CredentialStore, Secret};

/// Bound suggested by §4.2 of the detailed design.
const CHANNEL_CAPACITY: usize = 64;

/// Fixed username used when writing credentials. Each credential reference is
/// addressed by `(url=reference, username=SECRET_USERNAME)` so the platform
/// treats every Key/Header value as a separate entry per §5.3.
const SECRET_USERNAME: &str = "secret";

enum Request {
    Put {
        url: String,
        value: Secret,
        reply: oneshot::Sender<CredentialResult<()>>,
    },
    Get {
        url: String,
        reply: oneshot::Sender<CredentialResult<Option<Secret>>>,
    },
    Delete {
        url: String,
        reply: oneshot::Sender<CredentialResult<()>>,
    },
}

/// Client handle. Clone it as needed; every clone shares one pump.
#[derive(Clone)]
pub struct GpuiCredentialBroker {
    sender: mpsc::Sender<Request>,
}

/// Spawns the foreground pump. Keep the returned `Task` alive; dropping it
/// closes the channel and makes every subsequent call return `Unavailable`.
pub fn spawn_credential_broker(cx: &mut App) -> (GpuiCredentialBroker, Task<()>) {
    let (sender, mut receiver) = mpsc::channel::<Request>(CHANNEL_CAPACITY);
    let task = cx.spawn(async move |cx| {
        while let Some(request) = receiver.next().await {
            match request {
                Request::Put { url, value, reply } => {
                    let outcome = run_put(cx, &url, value).await;
                    let _ = reply.send(outcome);
                }
                Request::Get { url, reply } => {
                    let outcome = run_get(cx, &url).await;
                    let _ = reply.send(outcome);
                }
                Request::Delete { url, reply } => {
                    let outcome = run_delete(cx, &url).await;
                    let _ = reply.send(outcome);
                }
            }
        }
    });
    (GpuiCredentialBroker { sender }, task)
}

async fn run_put(cx: &mut gpui::AsyncApp, url: &str, value: Secret) -> CredentialResult<()> {
    let bytes = value.expose().as_bytes().to_vec();
    let task = cx.update(|cx| cx.write_credentials(url, SECRET_USERNAME, &bytes));
    task.await.map_err(classify_platform_error)
}

async fn run_get(cx: &mut gpui::AsyncApp, url: &str) -> CredentialResult<Option<Secret>> {
    let task = cx.update(|cx| cx.read_credentials(url));
    match task.await {
        Ok(None) => Ok(None),
        Ok(Some((_username, bytes))) => {
            let s = String::from_utf8(bytes)
                .map_err(|e| CredentialError::Backend(anyhow!("non-utf8 credential: {e}")))?;
            Ok(Some(Secret::new(s)))
        }
        Err(e) => Err(classify_platform_error(e)),
    }
}

async fn run_delete(cx: &mut gpui::AsyncApp, url: &str) -> CredentialResult<()> {
    let task = cx.update(|cx| cx.delete_credentials(url));
    task.await.map_err(classify_platform_error)
}

/// Platform-specific messages may contain user-sensitive hints. Keep the
/// mapping conservative: anything we cannot positively identify as a user
/// refusal goes into `Backend`, which asks the caller to retry at most once.
fn classify_platform_error(err: anyhow::Error) -> CredentialError {
    let message = err.to_string();
    let lowered = message.to_ascii_lowercase();
    if lowered.contains("denied")
        || lowered.contains("cancelled")
        || lowered.contains("canceled")
        || lowered.contains("rejected")
        || lowered.contains("too large")
    {
        CredentialError::Rejected(message)
    } else {
        CredentialError::Backend(err)
    }
}

#[async_trait]
impl CredentialStore for GpuiCredentialBroker {
    async fn put(&self, reference: &str, value: Secret) -> CredentialResult<()> {
        let (reply, response) = oneshot::channel();
        self.sender
            .clone()
            .send(Request::Put {
                url: reference.to_owned(),
                value,
                reply,
            })
            .await
            .map_err(|_| CredentialError::Unavailable)?;
        response.await.unwrap_or(Err(CredentialError::Unavailable))
    }

    async fn get(&self, reference: &str) -> CredentialResult<Option<Secret>> {
        let (reply, response) = oneshot::channel();
        self.sender
            .clone()
            .send(Request::Get {
                url: reference.to_owned(),
                reply,
            })
            .await
            .map_err(|_| CredentialError::Unavailable)?;
        response.await.unwrap_or(Err(CredentialError::Unavailable))
    }

    async fn delete(&self, reference: &str) -> CredentialResult<()> {
        let (reply, response) = oneshot::channel();
        self.sender
            .clone()
            .send(Request::Delete {
                url: reference.to_owned(),
                reply,
            })
            .await
            .map_err(|_| CredentialError::Unavailable)?;
        response.await.unwrap_or(Err(CredentialError::Unavailable))
    }
}
