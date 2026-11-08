//! Exact prepared publication on the existing ordered persistence worker.
//! The actor only tries the result lock; it never waits on filesystem work.
//! Attempt guards survive logical cancellation and reset busy only when their
//! last physical owner retires, including a blocking task outliving its wrapper.

use super::*;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicU8, Ordering},
};

/// Three finite storage attempts; retries never regenerate a prepared head.
pub(super) const MAX_ATTEMPTS: u8 = 3;

#[derive(Clone)]
pub(crate) struct RuntimeReplayEpochPublication(Arc<Publication>);

struct Publication {
    pane: String,
    generation: u64,
    source: Arc<RuntimeCompactionReplaySource>,
    /// Same live incarnation at response acceptance, qualified against the
    /// original frozen plan; newer source is retained, never summarized anew.
    publication_source: ReplayEpochContext,
    summary: String,
    busy: AtomicBool,
    cancelled: AtomicBool,
    attempts: AtomicU8,
    handled: AtomicU8,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    prepared: Option<Prepared>,
    outcome: Option<(u8, Arc<Result<Published>>)>,
}

#[derive(Clone)]
enum Prepared {
    Durable(Arc<ReplayEpochPublication>),
    Ephemeral(Arc<mez_agent::ReplayPreparedCompaction>),
}

enum Published {
    Durable {
        proposal: Arc<ReplayEpochPublication>,
        ack: ReplayEpochCommit,
        history: Arc<crate::storage::transcript::replay_epoch::ReplayEpochSnapshot>,
    },
    Ephemeral(Arc<mez_agent::ReplayPreparedCompaction>),
}

/// One actor admission into the existing lane, not an ordinary append claim.
#[derive(Clone)]
pub(crate) struct RuntimeReplayEpochAttempt(Arc<Attempt>);

struct Attempt {
    work: RuntimeReplayEpochPublication,
    number: u8,
}

impl Drop for Attempt {
    fn drop(&mut self) {
        self.work.0.busy.store(false, Ordering::Release);
    }
}

/// Only worker completion can supply a successful nonserializable receipt.
#[derive(Clone)]
pub(crate) struct RuntimeReplayEpochCompletion {
    pub(super) work: RuntimeReplayEpochPublication,
    pub(super) attempt: u8,
    result: Arc<Result<Published>>,
}

impl RuntimeReplayEpochPublication {
    pub(super) fn new(
        pane: String,
        generation: u64,
        source: Arc<RuntimeCompactionReplaySource>,
        publication_source: ReplayEpochContext,
        summary: String,
    ) -> Self {
        Self(Arc::new(Publication {
            pane,
            generation,
            source,
            publication_source,
            summary,
            busy: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            attempts: AtomicU8::new(0),
            handled: AtomicU8::new(0),
            state: Mutex::new(State::default()),
        }))
    }

    pub(crate) fn pane(&self) -> &str {
        &self.0.pane
    }
    pub(crate) fn generation(&self) -> u64 {
        self.0.generation
    }
    pub(super) fn source(&self) -> &RuntimeCompactionReplaySource {
        &self.0.source
    }
    pub(super) fn attempts(&self) -> u8 {
        self.0.attempts.load(Ordering::Acquire)
    }
    pub(super) fn busy(&self) -> bool {
        self.0.busy.load(Ordering::Acquire)
    }
    pub(crate) fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
    }

    /// Admission retains capacity through all physical attempt guards, not task
    /// cancellation. A busy/retired/exhausted owner cannot enqueue another write.
    pub(super) fn claim_attempt(&self) -> Option<RuntimeReplayEpochAttempt> {
        if self.0.cancelled.load(Ordering::Acquire)
            || self.attempts() >= MAX_ATTEMPTS
            || self
                .0
                .busy
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return None;
        }
        let number = self.0.attempts.fetch_add(1, Ordering::AcqRel) + 1;
        Some(RuntimeReplayEpochAttempt(Arc::new(Attempt {
            work: self.clone(),
            number,
        })))
    }

    /// Recovers an actual completed receipt if its callback was lost. This never
    /// blocks the actor or invents success from a loaded manifest identity.
    pub(super) fn completed(&self) -> Option<RuntimeReplayEpochCompletion> {
        let state = match self.0.state.try_lock() {
            Ok(state) => state,
            Err(std::sync::TryLockError::WouldBlock) => return None,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        };
        let (attempt, result) = state.outcome.as_ref()?;
        Some(RuntimeReplayEpochCompletion {
            work: self.clone(),
            attempt: *attempt,
            result: result.clone(),
        })
    }

    pub(super) fn mark_handled(&self, attempt: u8) -> bool {
        self.0.handled.fetch_max(attempt, Ordering::AcqRel) < attempt
    }

    /// Records an actual joined worker failure; no success receipt can be made.
    pub(crate) fn failed(&self, attempt: u8, error: String) -> RuntimeReplayEpochCompletion {
        RuntimeReplayEpochCompletion {
            work: self.clone(),
            attempt,
            result: Arc::new(Err(MezError::new(crate::error::MezErrorKind::Io, error))),
        }
    }
}

impl RuntimeReplayEpochAttempt {
    pub(crate) fn work(&self) -> &RuntimeReplayEpochPublication {
        &self.0.work
    }
    pub(crate) fn number(&self) -> u8 {
        self.0.number
    }

    /// Runs exclusively on a blocking worker. The proposal is retained before
    /// the first uncertain write and reused exactly after every storage failure.
    pub(crate) fn execute(self) -> RuntimeReplayEpochCompletion {
        let work = &self.0.work;
        let result = (|| {
            // Only complete immutable proposals/outcomes are assigned under
            // this lock, and the proposal is installed before the first write.
            // A joined panic may poison the mutex but cannot tear those Option
            // assignments. Recovery revalidates/retries that exact proposal;
            // none means preparation never reached an uncertain publication.
            let mut state = match work.0.state.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            if let Some((attempt, result)) = &state.outcome
                && (*attempt == self.0.number || result.is_ok())
            {
                return Ok(result.clone());
            }
            // Cache preparation failures as well as write outcomes while this
            // same lock is held. Duplicate physical copies of one attempt must
            // neither regenerate a head nor repeat an uncertain operation.
            let result = (|| {
                if work.0.cancelled.load(Ordering::Acquire) {
                    return Err(MezError::conflict(
                        "canonical publication owner retired before write",
                    ));
                }
                let source = &work.0.source;
                if state.prepared.is_none() {
                    let output = ReplayHeadPublication::Summary(work.0.summary.clone());
                    state.prepared = Some(
                        if !source.ephemeral
                            && let Some(store) = source.original.store.as_ref()
                        {
                            let history = source
                                .original
                                .history
                                .as_ref()
                                .and_then(|history| history.replay.as_deref());
                            let raw_rows = source
                                .original
                                .history
                                .as_ref()
                                .filter(|original| {
                                    original.replay.is_none() && original.raw.is_none()
                                })
                                .map(|original| {
                                    original.raw_replay_rows(&work.0.publication_source)
                                })
                                .transpose()?
                                .unwrap_or_default();
                            let capture = if history.is_some() {
                                source.origin.clone().capture(history)
                            } else if let Some(raw) = source
                                .original
                                .history
                                .as_ref()
                                .and_then(|history| history.raw.as_deref())
                            {
                                source.origin.clone().capture_legacy_raw(raw)
                            } else {
                                source.origin.clone().capture_raw(&raw_rows)
                            };
                            Prepared::Durable(Arc::new(store.prepare_replay_epoch(
                                capture,
                                source.owner.conversation_id().as_str(),
                                history.map(|history| history.identity().clone()),
                                &work.0.publication_source,
                                source.plan.clone(),
                                output,
                            )?))
                        } else {
                            Prepared::Ephemeral(Arc::new(
                                work.0
                                    .publication_source
                                    .prepare_compaction_publication(source.plan.clone(), output)?,
                            ))
                        },
                    );
                }
                if work.0.cancelled.load(Ordering::Acquire) {
                    return Err(MezError::conflict(
                        "canonical publication retired during preparation",
                    ));
                }
                match state.prepared.as_ref().ok_or_else(|| {
                    MezError::invalid_state("canonical publication proposal missing")
                })? {
                    Prepared::Durable(proposal) => {
                        let store = source.original.store.as_ref().ok_or_else(|| {
                            MezError::invalid_state("canonical publication original store missing")
                        })?;
                        let ack = store
                            .publish_replay_epoch_unless_cancelled(proposal, &work.0.cancelled)?;
                        let history = Arc::new(ack.snapshot(store, proposal)?);
                        Ok(Published::Durable {
                            proposal: proposal.clone(),
                            ack,
                            history,
                        })
                    }
                    Prepared::Ephemeral(prepared) => Ok(Published::Ephemeral(prepared.clone())),
                }
            })();
            let result = Arc::new(result);
            state.outcome = Some((self.0.number, result.clone()));
            Ok(result)
        })();
        match result {
            Ok(result) => RuntimeReplayEpochCompletion {
                work: work.clone(),
                attempt: self.0.number,
                result,
            },
            Err(error) => RuntimeReplayEpochCompletion {
                work: work.clone(),
                attempt: self.0.number,
                result: Arc::new(Err(error)),
            },
        }
    }
}

impl RuntimeReplayEpochCompletion {
    pub(crate) fn pane(&self) -> &str {
        self.work.pane()
    }
    pub(crate) fn generation(&self) -> u64 {
        self.work.generation()
    }
    pub(crate) fn error(&self) -> Option<&MezError> {
        self.result.as_ref().as_ref().err()
    }
    pub(super) fn history(
        &self,
    ) -> Option<&Arc<crate::storage::transcript::replay_epoch::ReplayEpochSnapshot>> {
        match self.result.as_ref() {
            Ok(Published::Durable { history, .. }) => Some(history),
            _ => None,
        }
    }
    /// Physical audit position from the original leased writer, not a model
    /// member count or an acknowledgment of ordinary queued transcript rows.
    pub(super) fn position(&self) -> Option<mez_agent::AgentTranscriptPosition> {
        match self.result.as_ref() {
            Ok(Published::Durable { ack, .. }) => Some(ack.position()),
            _ => None,
        }
    }
    pub(super) fn adopt(&self, body: &mut AgentContext) -> Result<()> {
        match self.result.as_ref() {
            Ok(Published::Durable { proposal, ack, .. }) => ack.adopt(
                self.work.source().original.store.as_ref().ok_or_else(|| {
                    MezError::invalid_state("canonical original store disappeared")
                })?,
                proposal,
                body,
            ),
            Ok(Published::Ephemeral(prepared)) => body
                .update_replay_owner(|owner| {
                    owner.adopt_compaction_publication(prepared)?;
                    Ok(())
                })
                .map_err(Into::into),
            Err(_) => Err(MezError::invalid_state(
                "failed canonical publication has no adoption receipt",
            )),
        }
    }
}

macro_rules! opaque_identity {
    ($ty:ty, $field:tt) => {
        impl PartialEq for $ty {
            fn eq(&self, other: &Self) -> bool {
                Arc::ptr_eq(&self.$field, &other.$field)
            }
        }
        impl Eq for $ty {}
        impl std::fmt::Debug for $ty {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(stringify!($ty))
            }
        }
    };
}
opaque_identity!(RuntimeReplayEpochPublication, 0);
opaque_identity!(RuntimeReplayEpochAttempt, 0);
opaque_identity!(RuntimeReplayEpochCompletion, result);
