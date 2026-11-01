//! Exact checked-read and append-admission evidence for retained candidates.
//!
//! The existing candidate queue and its private claim retain payload custody.
//! This receipt is shared only with that exact candidate witness; matching
//! generations, snapshots or successful transport booleans cannot mint it.
//! Acceptance records checked logical source plus actual append ownership. It
//! never proves physical append, durability, worker join or producer transfer.
//! Entered failures retain first error and cannot authorize replay.

use super::*;
use std::sync::{Arc, Mutex};

/// One real checked-source boundary, independent of candidate retention.
enum CheckedSourceState {
    /// No checked worker read entered for this retained candidate.
    Pending,
    /// Original read entered; acceptance has not been observed.
    Entered,
    /// Checked rows are in existing history or an actual admitted append owner.
    Accepted,
    /// First boundary failure remains authoritative despite later callbacks.
    Failed(Box<MezError>),
}

/// Opaque actual checked-source allocation, never reconstructed from labels.
#[derive(Clone)]
pub(crate) struct RuntimeCompletionCheckedSource(Arc<Mutex<CheckedSourceState>>);

impl PartialEq for RuntimeCompletionCheckedSource {
    /// Equal metadata cannot substitute for the actual retained allocation.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for RuntimeCompletionCheckedSource {}

impl std::fmt::Debug for RuntimeCompletionCheckedSource {
    /// No row, path, private diagnostic or pointer value is exposed.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RuntimeCompletionCheckedSource")
    }
}

impl RuntimeCompletionCheckedSource {
    /// Creates pending evidence only. Construction grants no accepted gate.
    pub(crate) fn pending() -> Self {
        Self(Arc::new(Mutex::new(CheckedSourceState::Pending)))
    }

    /// Reports actual checked admission; this is not physical persistence.
    pub(crate) fn accepted(&self) -> bool {
        self.0
            .lock()
            .is_ok_and(|state| matches!(*state, CheckedSourceState::Accepted))
    }

    /// Preserves the first typed boundary failure for the original consumer.
    pub(crate) fn failure(&self) -> Option<MezError> {
        match self.0.lock() {
            Ok(state) => match &*state {
                CheckedSourceState::Failed(error) => Some((**error).clone()),
                _ => None,
            },
            Err(_) => Some(MezError::invalid_state(
                "completion checked-source owner poisoned",
            )),
        }
    }

    /// Enters once immediately before the exact work's actual checked read.
    pub(super) fn enter(&self) -> Result<()> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| MezError::invalid_state("completion checked-source owner poisoned"))?;
        match &*state {
            CheckedSourceState::Pending => {
                *state = CheckedSourceState::Entered;
                Ok(())
            }
            CheckedSourceState::Failed(error) => Err((**error).clone()),
            _ => Err(MezError::conflict(
                "completion checked-source read already entered",
            )),
        }
    }

    /// Called only after exact claim/read validation and every required append
    /// admission suffix succeeds. No boolean supplied by a worker can invoke it.
    pub(super) fn accept(&self) -> Result<()> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| MezError::invalid_state("completion checked-source owner poisoned"))?;
        match &*state {
            CheckedSourceState::Entered => {
                *state = CheckedSourceState::Accepted;
                Ok(())
            }
            CheckedSourceState::Failed(error) => Err((**error).clone()),
            _ => Err(MezError::invalid_state(
                "completion checked-source acceptance has no entered read",
            )),
        }
    }

    /// Retains first failure without deleting append owners already admitted.
    pub(super) fn fail(&self, error: &MezError) {
        if let Ok(mut state) = self.0.lock()
            && !matches!(*state, CheckedSourceState::Failed(_))
        {
            *state = CheckedSourceState::Failed(Box::new(error.clone()));
        }
    }
}
