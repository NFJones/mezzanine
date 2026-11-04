//! Owned canonical native input work on the original serialized custody lane.
//!
//! The source anchor, original transcript target, application projection and
//! first physical preparation remain outside the worker through cancellation or
//! a lost reply. Only actual joined canonical storage may return a lower commit
//! proof. Lookup results never inject input or schedule provider work; the actor
//! must separately settle an exact original native ACK and its continuation.

use std::task::Context;

use mez_agent::messaging::{MessageNativeInputAnchor, MessageNativeInputObservation};

use super::*;
use crate::storage::mmp_custody::native_input::{NativeInputAttempt, NativeInputProjection};
use crate::storage::transcript::AgentTranscriptStore;

/// Actual immutable input and its first root position retained through loss.
pub(super) struct Admission {
    source: MessageNativeInputAnchor,
    input: Arc<NativeInputAttempt>,
    anchor: AcceptancePlan,
    failures: u32,
}
/// Joined real canonical result and the updated serialized root connection.
pub(super) struct Written {
    opened: Opened,
    observation: MessageNativeInputObservation,
}

impl MmpCustodyRoot {
    /// Starts one original input projection or a lookup-only probe. No lower
    /// offer, registration, input mutation, ACK or provider claim is fabricated.
    pub fn start_native_input(
        &self,
        service: &MessageService,
        source: MessageNativeInputAnchor,
        transcript: AgentTranscriptStore,
        row: Option<Arc<NativeInputProjection>>,
    ) -> Result<()> {
        let mut state = self.0.state.lock().map_err(|_| ownership_error())?;
        let State::Idle(opened) = &*state else {
            return Err(ownership_error());
        };
        // Closing forbids fresh selection, but an actual context-owned input
        // must finish canonical storage and ACK before its actor can retire.
        if (self.0.stopping.load(Ordering::Acquire) && row.is_none())
            || !source.belongs_to(&self.0.storage_scope)
            || !service.native_input_anchor_is_original(&source)
        {
            return Err(ownership_error());
        }
        let admission = Admission {
            source,
            input: Arc::new(NativeInputAttempt::new(transcript, row)),
            anchor: opened.anchor.clone(),
            failures: 0,
        };
        let previous = std::mem::replace(&mut *state, State::Unavailable(ownership_error()));
        if let State::Idle(opened) = previous {
            let worker = self.spawn_native_input(Some(opened.store), &admission);
            *state = State::WritingNativeInput { admission, worker };
        }
        Ok(())
    }

    /// Repeats only the original immutable input after its actual handle joined.
    pub fn retry_native_input(&self, now: Instant) -> Result<bool> {
        let mut state = self.0.state.lock().map_err(|_| ownership_error())?;
        let State::WrittenNativeInput {
            result, retry_at, ..
        } = &*state
        else {
            return Err(ownership_error());
        };
        if result.is_ok() {
            return Err(ownership_error());
        }
        if now < *retry_at {
            return Ok(false);
        }
        let previous = std::mem::replace(&mut *state, State::Unavailable(ownership_error()));
        if let State::WrittenNativeInput { admission, .. } = previous {
            let worker = self.spawn_native_input(None, &admission);
            *state = State::WritingNativeInput { admission, worker };
        }
        Ok(true)
    }

    /// Transfers only the actual joined original proof/qualified absence. A
    /// replacement current pane cannot inherit it; the source remains frozen.
    pub fn adopt_native_input(
        &self,
        service: &MessageService,
    ) -> Result<MessageNativeInputObservation> {
        let mut state = self.0.state.lock().map_err(|_| ownership_error())?;
        let State::WrittenNativeInput {
            admission,
            result: Ok(_),
            ..
        } = &*state
        else {
            return Err(ownership_error());
        };
        if !service.native_input_anchor_is_original(&admission.source) {
            return Err(ownership_error());
        }
        let previous = std::mem::replace(&mut *state, State::Unavailable(ownership_error()));
        let State::WrittenNativeInput {
            result: Ok(written),
            ..
        } = previous
        else {
            return Err(ownership_error());
        };
        let Written {
            opened,
            observation,
        } = *written;
        *state = State::Idle(opened);
        Ok(observation)
    }

    /// Executes actual original I/O while every first-plan cell remains owned
    /// by the root, including if this blocking closure panics after a commit.
    fn spawn_native_input(
        &self,
        store: Option<MmpCustodyStore>,
        admission: &Admission,
    ) -> JoinHandle<Result<Written>> {
        let destination = self.0.destination.clone();
        let ownership = self.0.ownership.clone();
        let scope = self.0.storage_scope.clone();
        let source = admission.source.clone();
        let input = admission.input.clone();
        let first_anchor = admission.anchor.clone();
        #[cfg(test)]
        let probe = self.0.probe.clone();
        self.0.runtime.spawn_blocking(move || {
            if !source.belongs_to(&scope) {
                return Err(ownership_error());
            }
            let fence = ownership
                .lock()
                .map_err(|_| ownership_error())?
                .as_ref()
                .cloned()
                .ok_or_else(ownership_error)?;
            fence.validate_root()?;
            let mut store = match store {
                Some(store) => {
                    if !store.acceptance_plan(0)?.same_position(&first_anchor) {
                        return Err(ownership_error());
                    }
                    store
                }
                None => MmpCustodyStore::open_existing(
                    &destination.root,
                    &destination.owner,
                    destination.limits,
                )?,
            };
            #[cfg(test)]
            if let Some(probe) = &probe {
                probe.before_write();
            }
            let observation = {
                let mut owned = fence.bind_store(&mut store)?;
                let result = source
                    .observe(&mut owned.store_mut().native_input_port(&input))
                    .map_err(lower_error)?;
                owned.revalidate()?;
                result
            };
            #[cfg(test)]
            if let Some(probe) = &probe {
                probe.after_payload_read();
            }
            {
                let _bound = fence.bind_store(&mut store)?;
            }
            let anchor = store.acceptance_plan(0)?;
            Ok(Written {
                opened: Opened { store, anchor },
                observation,
            })
        })
    }
}

/// Observes only the root-owned actual handle; observers cannot consume it.
pub(super) fn poll_join(state: &mut State, cx: &mut Context<'_>) -> Poll<()> {
    let State::WritingNativeInput { worker, .. } = state else {
        return Poll::Ready(());
    };
    let result = match Pin::new(worker).poll(cx) {
        Poll::Pending => return Poll::Pending,
        Poll::Ready(result) => result.map_err(|_| worker_error()).and_then(|r| r),
    };
    let previous = std::mem::replace(state, State::Unavailable(ownership_error()));
    if let State::WritingNativeInput { mut admission, .. } = previous {
        if result.is_err() {
            admission.failures = admission.failures.saturating_add(1);
        }
        let retry_at = Instant::now() + retry_delay(admission.failures);
        *state = State::WrittenNativeInput {
            admission,
            result: result.map(Box::new),
            retry_at,
        };
    }
    Poll::Ready(())
}
