//! Original native pre-request custody for synchronous local Codex model hooks.
//!
//! A native-qualified pre invocation owns one immutable marker/origin. No newer
//! pre replaces it: overlapping unknown pairs quarantine that turn. Post facts
//! keep their original normalized report through finite delivery retry and never
//! borrow a current cwd, observer, turn or payload-selected request ID. The shared
//! native worker and ledger still authorize and commit expense. This owner grants
//! no source, lifecycle, lease, approval or provider-replay authority.

use super::*;
use crate::storage::token_usage::{ExternalCounters, ExternalRequestOrigin};
use mez_mux::process::ProcessParentIdentity;
use std::collections::{BTreeMap, VecDeque};

/// Active admissions and retained normalized reports share finite custody.
const CUSTODY: usize = 16;
/// Settled callback receipts are only transport replay hints, never authority.
const RECEIPTS: usize = 128;

/// Closed current-epoch coverage facts, separate from price and expense totals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Gap {
    /// No unique original admission could be retained for this callback.
    Readiness,
    /// A genuine post callback supplied no usable final counters.
    Counters,
    /// An observed normalized report still lacks a durable acknowledgment.
    Durable,
    /// New telemetry could not acquire finite custody without eviction.
    Capacity,
}
impl Gap {
    /// Returns only a fixed public capability label, never callback content.
    fn coverage(self) -> &'static str {
        match self {
            Self::Readiness => "unavailable-codex-usage-request-readiness",
            Self::Counters => "unavailable-codex-usage-finalized-counters",
            Self::Durable => "unavailable-codex-usage-durable-acknowledgment",
            Self::Capacity => "unavailable-codex-usage-capacity",
        }
    }
}

/// One original worker-published request boundary, not a source credential.
#[derive(Debug, Clone)]
pub(super) struct Admission {
    marker: String,
    turn: String,
    writer: ProcessParentIdentity,
    origin: ExternalRequestOrigin,
}

/// Immutable normalized incurred outcome. Unknown response dimensions stay unknown.
#[derive(Debug, Clone)]
pub(super) struct Report {
    owner: String,
    admission: Admission,
    writer: ProcessParentIdentity,
    counters: ExternalCounters,
}
impl Report {
    /// Reconstructs the same original ledger report on every transport attempt.
    fn params(&self) -> serde_json::Value {
        serde_json::json!({"request_id":self.admission.marker,
            "event_id":digest(&["codex-model-outcome/1", &self.admission.marker]),
            "observed_at":self.admission.origin.admitted_at,"provider":"unavailable","model":"unavailable",
            "counters":self.counters})
    }
}

/// Runtime-only state belonging to one exact original native observer epoch.
#[derive(Debug, Default)]
pub(super) struct Requests {
    active: BTreeMap<String, Admission>,
    retained: Vec<Report>,
    settled: VecDeque<Report>,
    /// Missing-counter receipts conserve the original diagnostic on IPC replay.
    missing: VecDeque<(ProcessParentIdentity, String)>,
    poisoned: BTreeSet<String>,
    gap: Option<Gap>,
}
impl Requests {
    /// Distinguishes current missing coverage even after some requests committed.
    pub(super) fn coverage(&self) -> Option<&'static str> {
        self.gap.map(Gap::coverage)
    }
    /// Counts only live custody, never already settled expense or receipt hints.
    fn occupied(&self) -> usize {
        self.active.len() + self.retained.len()
    }
}

/// One frozen native callback decision; no fact is published before worker proof.
#[derive(Debug, Clone)]
pub(super) enum Step {
    /// The actual native pre writer names its own idempotent admission.
    Before {
        turn: String,
        marker: String,
        writer: ProcessParentIdentity,
    },
    /// Only the retained exact active marker may be quarantined on overlap.
    Overlap { turn: String, marker: String },
    /// Missing counters can release only the captured unfinalized admission.
    Missing {
        turn: String,
        writer: ProcessParentIdentity,
        marker: Option<String>,
    },
    /// A real post report or replay owns its original admission exclusively.
    After,
    /// A closed missing-coverage fact, not caller-selected text or power.
    Unavailable(Gap),
}

/// Prepared normalized data plus a worker-only native observation witness.
#[derive(Debug, Clone)]
pub(super) struct RequestWork {
    pub(super) step: Step,
    pub(super) report: Option<Report>,
    native_observed: Arc<OnceLock<()>>,
}
impl RequestWork {
    /// Marks only an actual off-actor ancestry-qualified callback, before I/O.
    pub(super) fn observe(&self) {
        let _ = self.native_observed.set(());
    }
}

/// Canonical opaque IDs exclude counters, content, model labels and clock guesses.
fn digest(parts: &[&str]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

impl RuntimeSessionService {
    /// Plans only against retained original native scope. Worker and actor still
    /// verify the actual caller; public session/turn labels grant no authority.
    pub(super) fn plan_codex_request(
        &self,
        binding: &LaunchBinding,
        kind: &str,
        turn: &str,
        writer: ProcessParentIdentity,
        counters: Option<ExternalCounters>,
        current_turn: bool,
    ) -> RequestWork {
        let requests = binding.enrollment.as_ref().map(|s| &s.codex_requests);
        let mut report = None;
        let step = if let Some(requests) = requests {
            if kind == "PreModelRequest" {
                report = requests.retained.first().cloned();
                let marker = digest(&[
                    "codex-model-request/1",
                    &binding.accounting_owner,
                    turn,
                    &writer.process_id.to_string(),
                    &writer.start_token.to_string(),
                ]);
                if !current_turn || requests.poisoned.contains(turn) {
                    Step::Unavailable(Gap::Readiness)
                } else if let Some(old) = requests.active.get(turn) {
                    if old.marker == marker && old.writer == writer {
                        Step::Before {
                            turn: turn.into(),
                            marker,
                            writer,
                        }
                    } else {
                        Step::Overlap {
                            turn: turn.into(),
                            marker: old.marker.clone(),
                        }
                    }
                } else if requests.occupied() >= CUSTODY {
                    Step::Unavailable(Gap::Capacity)
                } else {
                    Step::Before {
                        turn: turn.into(),
                        marker,
                        writer,
                    }
                }
            } else if let Some(old) = requests
                .retained
                .iter()
                .chain(requests.settled.iter())
                .find(|r| r.writer == writer)
            {
                if old.admission.turn == turn && Some(old.counters) == counters {
                    report = Some(old.clone());
                    Step::After
                } else {
                    Step::Unavailable(Gap::Readiness)
                }
            } else if let Some((_, original_turn)) =
                requests.missing.iter().find(|(old, _)| *old == writer)
            {
                Step::Unavailable(if original_turn == turn && counters.is_none() {
                    Gap::Counters
                } else {
                    Gap::Readiness
                })
            } else if requests.poisoned.contains(turn) {
                Step::Unavailable(Gap::Readiness)
            } else if let Some(admission) = requests.active.get(turn) {
                if let Some(counters) = counters {
                    report = Some(Report {
                        owner: binding.accounting_owner.clone(),
                        admission: admission.clone(),
                        writer,
                        counters,
                    });
                    Step::After
                } else {
                    Step::Missing {
                        turn: turn.into(),
                        writer,
                        marker: Some(admission.marker.clone()),
                    }
                }
            } else {
                Step::Unavailable(Gap::Readiness)
            }
        } else {
            Step::Unavailable(Gap::Readiness)
        };
        RequestWork {
            step,
            report,
            native_observed: Arc::new(OnceLock::new()),
        }
    }

    /// Attaches shared original-origin/ledger work without another source or
    /// storage implementation. Refused storage work retains the exact report.
    pub(super) fn attach_codex_request(
        &mut self,
        work: &mut ExternalEnrollmentWork,
        digest: [u8; 32],
        plan: RequestWork,
    ) -> Result<()> {
        let binding = self
            .control
            .external_agents()
            .bindings
            .get(&digest)
            .ok_or_else(|| MezError::forbidden("Codex original request owner unavailable"))?;
        let begin = if let Step::Before { marker, .. } = &plan.step {
            Some(super::helper_usage::HelperUsageBegin::capture_codex_hook(
                self,
                binding,
                marker.clone(),
            )?)
        } else {
            None
        };
        let end = if let Some(report) = &plan.report {
            self.control.external_usage_begins_mut().cache_origin(
                report.owner.clone(),
                report.admission.marker.clone(),
                report.admission.origin.clone(),
            );
            let candidate = self
                .control
                .external_agents()
                .bindings
                .get(&digest)
                .filter(|b| b.accounting_owner == report.owner)
                .ok_or_else(|| MezError::forbidden("Codex retained request owner changed"))?;
            let end = self.external_request_work_for_binding(
                work.request_id.clone(),
                candidate,
                &report.params(),
            );
            end.and_then(|end| self.reserve_external_usage_work(end))
                .ok()
                .map(Box::new)
        } else {
            None
        };
        let helper = work
            .helper
            .as_mut()
            .ok_or_else(|| MezError::invalid_state("Codex request helper unavailable"))?;
        helper.usage_begin = begin;
        helper.usage_end = end;
        helper.codex_request = Some(plan);
        Ok(())
    }

    /// Publishes only after ordinary native/root/order/epoch actor fencing.
    /// Request facts never mutate presentation or observer proof sequences.
    pub(super) fn commit_codex_request(
        &mut self,
        digest: [u8; 32],
        plan: &RequestWork,
        begin: Option<&super::helper_usage::HelperUsageBegin>,
    ) -> Result<String> {
        let binding = self
            .control
            .external_agents()
            .bindings
            .get(&digest)
            .ok_or_else(|| MezError::forbidden("Codex request owner disappeared"))?;
        let origin = if matches!(plan.step, Step::Before { .. }) {
            Some(
                begin
                    .ok_or_else(|| MezError::invalid_state("Codex original readiness unavailable"))?
                    .origin(binding)?,
            )
        } else {
            None
        };
        let source = self
            .control
            .external_agents_mut()
            .bindings
            .get_mut(&digest)
            .and_then(|b| b.enrollment.as_mut())
            .ok_or_else(|| MezError::forbidden("Codex original request source unavailable"))?;
        let requests = &mut source.codex_requests;
        match &plan.step {
            Step::Before {
                turn,
                marker,
                writer,
            } => {
                if requests.poisoned.contains(turn) {
                    requests.gap = Some(Gap::Readiness);
                } else if requests
                    .active
                    .get(turn)
                    .is_some_and(|old| old.marker != *marker)
                {
                    requests.active.remove(turn);
                    requests.poisoned.insert(turn.clone());
                    requests.gap = Some(Gap::Readiness);
                } else if !requests.active.contains_key(turn) && requests.occupied() >= CUSTODY {
                    requests.gap = Some(Gap::Capacity);
                } else {
                    requests.active.entry(turn.clone()).or_insert(Admission {
                        marker: marker.clone(),
                        turn: turn.clone(),
                        writer: *writer,
                        origin: origin.ok_or_else(|| {
                            MezError::invalid_state("Codex readiness unpublished")
                        })?,
                    });
                }
            }
            Step::Overlap { turn, marker } => {
                if requests
                    .active
                    .get(turn)
                    .is_some_and(|old| old.marker == *marker)
                {
                    requests.active.remove(turn);
                    requests.poisoned.insert(turn.clone());
                }
                requests.gap = Some(Gap::Readiness);
            }
            Step::Missing {
                turn,
                marker,
                writer,
            } => {
                if requests
                    .active
                    .get(turn)
                    .is_some_and(|old| Some(&old.marker) == marker.as_ref())
                {
                    requests.active.remove(turn);
                    if requests.missing.len() >= RECEIPTS {
                        requests.missing.pop_front();
                    }
                    requests.missing.push_back((*writer, turn.clone()));
                }
                requests.gap = Some(Gap::Counters);
            }
            Step::Unavailable(gap) => requests.gap = Some(*gap),
            Step::After => {}
        }
        Ok(serde_json::json!({"observed":true}).to_string())
    }

    /// Retains only worker-observed normalized reports under their original
    /// binding. Durable publication wins over waiter loss; failed delivery keeps
    /// finite custody. Root/epoch replacement cannot transfer these obligations.
    pub(super) fn finish_codex_request(&mut self, work: &ExternalEnrollmentWork) {
        let Some(helper) = &work.helper else {
            return;
        };
        let Some(plan) = &helper.codex_request else {
            return;
        };
        if plan.native_observed.get().is_none() {
            return;
        }
        let Some(report) = &plan.report else {
            return;
        };
        let Some(binding) = self
            .control
            .external_agents_mut()
            .bindings
            .get_mut(&helper.digest)
            .filter(|b| {
                !b.retired
                    && b.generation == helper.generation
                    && b.accounting_owner == report.owner
                    && b.process.same_incarnation(&work.process)
            })
        else {
            return;
        };
        let Some(source) = binding
            .enrollment
            .as_mut()
            .filter(|s| s.epoch == 1 && s.producer.same_owner(&helper.producer))
        else {
            return;
        };
        let requests = &mut source.codex_requests;
        let known = requests
            .active
            .get(&report.admission.turn)
            .is_some_and(|a| a.marker == report.admission.marker)
            || requests
                .retained
                .iter()
                .chain(requests.settled.iter())
                .any(|r| r.admission.marker == report.admission.marker);
        if !known {
            return;
        }
        if requests
            .active
            .get(&report.admission.turn)
            .is_some_and(|a| a.marker == report.admission.marker)
        {
            requests.active.remove(&report.admission.turn);
        }
        if helper
            .usage_end
            .as_ref()
            .is_some_and(|end| end.published_result().is_ok())
        {
            requests
                .retained
                .retain(|r| r.admission.marker != report.admission.marker);
            // Exact retained obligations are recoverable. Clear only their
            // delivery fault after the last report commits; unrelated missing
            // source/counter/readiness facts must remain visible.
            if requests.retained.is_empty() && requests.gap == Some(Gap::Durable) {
                requests.gap = None;
            }
            if !requests.settled.iter().any(|r| r.writer == report.writer) {
                if requests.settled.len() >= RECEIPTS {
                    requests.settled.pop_front();
                }
                requests.settled.push_back(report.clone());
            }
        } else {
            if !requests
                .retained
                .iter()
                .any(|r| r.admission.marker == report.admission.marker)
                && requests.occupied() < CUSTODY
            {
                requests.retained.push(report.clone());
            }
            // Delivery refusal must not overwrite an independent coverage gap:
            // later successful delivery could otherwise erase that older loss.
            if requests.gap.is_none() || requests.gap == Some(Gap::Durable) {
                requests.gap = Some(Gap::Durable);
            }
        }
    }
}

impl EnrollmentBinding {
    /// Exposes a fixed Codex request-coverage gap separately from actual expense.
    pub(in crate::runtime::control) fn codex_request_gap_coverage(&self) -> Option<&'static str> {
        self.codex_requests.coverage()
    }
}
