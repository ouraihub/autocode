//! `ProviderChange` broadcast: notifies UI and cache layers when provider
//! state transitions to a new revision. The channel is bounded and
//! lossy-on-full by design: consumers that fall behind pick up the next
//! event; nothing in `ProviderChange` is secret, so dropping one is safe.
//!
//! See `docs/03-详细设计.md` §4.1 and §13.4.

use std::sync::Arc;

use tokio::sync::broadcast;

use crate::BackendTarget;

/// Capacity of the per-service broadcast buffer. Small on purpose: this is a
/// change signal, not an event log.
const DEFAULT_CAPACITY: usize = 32;

/// Reason for the broadcast. Variants mirror the design-doc classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderChangeKind {
    /// Provider row inserted, updated, deleted, or its `enabled` flipped.
    Provider,
    /// A key metadata row changed (commit / remove / toggle enabled).
    Key,
    /// A header metadata row changed.
    Header,
    /// `agent_launch_preferences` was saved for some backend.
    Preferences,
    /// The entire provider was removed. `provider_id` still carries the
    /// deleted row's id so stale snapshots can be evicted.
    Deleted,
}

/// Signal emitted after a successful transaction. Callers MUST NOT put secret
/// values here; snapshots on this bus already have `credential_ref` only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderChange {
    pub kind: ProviderChangeKind,
    /// The provider affected, when the change touched one specific provider.
    pub provider_id: Option<String>,
    /// The backend affected, used for `Preferences`.
    pub target: Option<BackendTarget>,
    /// Post-commit revision of the Provider (or 0 for Preferences, where
    /// each backend has its own row-level revision).
    pub revision: u64,
}

/// Broadcast sender. `Arc` so the service and any background task can hold
/// one shared handle without exposing `&App`.
#[derive(Clone)]
pub struct ProviderChangeSender {
    tx: Arc<broadcast::Sender<ProviderChange>>,
}

impl ProviderChangeSender {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        Self { tx: Arc::new(tx) }
    }

    /// Publish an event. Dropped when there are no subscribers; that is
    /// intentional for a signal channel.
    pub fn send(&self, event: ProviderChange) {
        let _ = self.tx.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ProviderChange> {
        self.tx.subscribe()
    }

    /// Number of currently registered receivers; test observability only.
    #[doc(hidden)]
    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

impl Default for ProviderChangeSender {
    fn default() -> Self {
        Self::new()
    }
}
