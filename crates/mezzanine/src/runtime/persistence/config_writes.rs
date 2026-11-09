//! Exact physical acknowledgement for primary configuration revisions.
//!
//! The ordered persistence lane owns execution. A shared one-shot receipt
//! survives lost/duplicate event delivery; a stale receipt cannot acknowledge a
//! newer revision. Only the newest proposal and its finite recovery text are
//! retained by the runtime, independently of pane/conversation lifetimes.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::{RuntimePersistenceComponent, RuntimeSideEffect};

#[derive(Debug)]
enum State {
    Pending,
    Writing,
    Durable,
    Failed(String),
}

/// Opaque allocation identity, not pathname/byte-count based acknowledgement.
#[derive(Debug, Clone)]
pub struct ConfigWriteReceipt(Arc<Inner>);

impl PartialEq for ConfigWriteReceipt {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for ConfigWriteReceipt {}

struct Inner {
    path: PathBuf,
    previous: String,
    proposed: String,
    config_generation: u64,
    state: Mutex<State>,
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Config may contain credentials. Never expose source/recovery bytes.
        f.debug_struct("ConfigWrite")
            .field("status", &self.state)
            .finish_non_exhaustive()
    }
}

impl ConfigWriteReceipt {
    fn new(path: PathBuf, previous: String, proposed: String, config_generation: u64) -> Self {
        Self(Arc::new(Inner {
            path,
            previous,
            proposed,
            config_generation,
            state: Mutex::new(State::Pending),
        }))
    }

    /// Only the first worker may perform this revision. Abandoned physical
    /// execution remains unproven rather than authorizing a blind replay.
    pub(crate) fn claim(&self) -> bool {
        let Ok(mut state) = self.0.state.lock() else {
            return false;
        };
        if !matches!(*state, State::Pending) {
            return false;
        }
        *state = State::Writing;
        true
    }

    pub(crate) fn path(&self) -> &std::path::Path {
        &self.0.path
    }
    pub(crate) fn bytes(&self) -> &[u8] {
        self.0.proposed.as_bytes()
    }

    /// Called only after the actual ordered writer returns; event delivery is
    /// not the durability witness. Does not change live config or retry writes.
    pub(crate) fn settle(&self, result: &crate::error::Result<()>) {
        if let Ok(mut state) = self.0.state.lock()
            && matches!(*state, State::Writing)
        {
            *state = match result {
                Ok(()) => State::Durable,
                Err(error) => State::Failed(error.message().to_string()),
            };
        }
    }

    fn status(&self) -> String {
        match self.0.state.lock() {
            Ok(state) => match &*state {
                State::Pending | State::Writing => {
                    "pending (live applied; disk not acknowledged)".into()
                }
                State::Durable => "durable".into(),
                State::Failed(error) => format!(
                    "failed (live applied; disk unconfirmed; prior {} bytes retained): {}",
                    self.0.previous.len(),
                    error.replace(['\n', '\r', '|'], " ")
                ),
            },
            Err(_) => "failed (write evidence unavailable; disk unconfirmed)".into(),
        }
    }
}

impl RuntimePersistenceComponent {
    /// Replaces only the status owner; earlier queued writes retain their own
    /// receipt and execute in the existing serial lane, never out of order.
    pub(crate) fn queue_tracked_config(
        &mut self,
        path: PathBuf,
        previous: String,
        proposed: String,
        config_generation: u64,
    ) {
        let receipt = ConfigWriteReceipt::new(path, previous, proposed, config_generation);
        self.latest_primary_config_write = Some(receipt.clone());
        self.queue_config(RuntimeSideEffect::PersistConfig { receipt });
    }

    /// Synchronous writer has already returned successfully, unlike queued acceptance.
    pub(crate) fn record_durable_config(
        &mut self,
        path: PathBuf,
        previous: String,
        proposed: String,
        config_generation: u64,
    ) {
        let receipt = ConfigWriteReceipt::new(path, previous, proposed, config_generation);
        if receipt.claim() {
            receipt.settle(&Ok(()));
        }
        self.latest_primary_config_write = Some(receipt);
    }

    pub(crate) fn primary_config_persistence_status(
        &self,
        layer: Option<&crate::config::ConfigLayer>,
        config_generation: u64,
    ) -> String {
        self.latest_primary_config_write
            .as_ref()
            .filter(|receipt| {
                receipt.0.config_generation == config_generation
                    && layer.is_some_and(|layer| {
                        layer.path.as_deref() == Some(receipt.path())
                            && layer.text == receipt.0.proposed
                    })
            })
            .map_or_else(
                || "not-applicable (no matching command write in this runtime)".into(),
                ConfigWriteReceipt::status,
            )
    }
}
