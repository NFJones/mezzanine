//! Authoritative, content-free observations of canonical MAAP negotiation.
//!
//! Snapshots describe one canonical invocation, not all effects of a user turn.
//! Counts come from the existing budget and actual provider-call boundary;
//! failure-summary negotiation remains a separate response-only phase. These
//! records never authorize retries, grant actions, or infer provider cutoff.

use crate::{ModelInteractionKind, ModelRequest};

/// Canonical owner that produced the current correction or rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryStage {
    /// Provider output parsing rejected a response.
    ProviderOutput,
    /// A parsed response omitted its MAAP batch.
    MissingBatch,
    /// The captured action surface rejected a batch.
    AllowedActions,
    /// Canonical product batch validation rejected a batch.
    BatchValidation,
    /// Product action planning rejected a validated batch.
    ActionPlanning,
    /// A response or controller boundary violated identity/invariants.
    ControllerInvariant,
}

/// Actual canonical disposition; never guessed from error text or raw markers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryDisposition {
    /// No corrective attempt was needed for the accepted batch.
    NotRequired,
    /// The corrected batch was accepted for planning, not necessarily executed.
    CorrectedBatchAccepted,
    /// The existing phase-local correction budget rejected another attempt.
    BudgetExhausted,
    /// The turn-wide provider-call bound prevented further negotiation.
    InteractionLimit,
    /// A corrective provider call failed rather than supplying corrected output.
    RepairRequestFailed,
    /// Request preparation failed before the corrective provider call started.
    RepairUnavailable,
    /// Existing runtime recovery owns the returned provider failure.
    ReturnedToRuntime,
    /// An invariant or non-correctable boundary rejected the batch.
    Rejected,
    /// A legacy/unannotated boundary cannot establish a stronger disposition.
    Unknown,
}

/// Immutable facts bound to the actual issued request identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryFacts {
    /// Owning logical turn identifier.
    pub turn_id: String,
    /// Owning agent identifier.
    pub agent_id: String,
    /// Captured provider identity, not response-supplied metadata.
    pub provider: String,
    /// Captured requested model identity.
    pub model: String,
    /// Monotonic phase index within this canonical invocation.
    pub phase: usize,
    /// Existing budget reservations in the current phase, not wire deliveries.
    pub attempts_reserved: usize,
    /// Existing correction bound for the current phase.
    pub attempt_limit: usize,
    /// Provider calls started by this canonical invocation.
    pub provider_calls_started: usize,
    /// Existing turn-wide bound for those calls.
    pub provider_call_limit: usize,
    /// Corrective calls actually started; a reservation may remain unsent.
    pub repair_calls_started: usize,
    /// Purpose of the last actual canonical provider call.
    pub last_request_kind: ModelInteractionKind,
    /// Most recent canonical correction/rejection owner, when known.
    pub stage: Option<RecoveryStage>,
    /// Authoritative exit disposition, or explicit unknown.
    pub disposition: RecoveryDisposition,
    /// Whether the final validated batch entered product action planning.
    /// This does not claim effects, approval, or invalid-batch execution.
    pub batch_planned: bool,
}

impl RecoveryFacts {
    /// Formats only enum/counter facts for diagnostics, without identities or content.
    /// This scope excludes response-only failure-summary provider calls.
    pub fn diagnostic_line(&self) -> String {
        format!(
            "MAAP recovery: disposition={:?}; stage={:?}; phase={}; reservations={}/{}; corrective calls started={}; provider calls started={}/{}; final batch planned={}; excludes failure-summary calls",
            self.disposition,
            self.stage,
            self.phase,
            self.attempts_reserved,
            self.attempt_limit,
            self.repair_calls_started,
            self.provider_calls_started,
            self.provider_call_limit,
            self.batch_planned
        )
    }

    /// Rejects a snapshot copied onto a different request identity.
    pub fn matches_request(&self, request: &ModelRequest) -> bool {
        self.turn_id == request.turn_id
            && self.agent_id == request.agent_id
            && self.provider == request.provider
            && self.model == request.model
    }
}

/// Mutable observer owned only by one canonical invocation.
#[derive(Debug, Default)]
pub(crate) struct RecoveryTrace {
    /// No observation exists before an actual request reaches the send boundary.
    pub(crate) facts: Option<RecoveryFacts>,
}

impl RecoveryTrace {
    /// Records request identity and counters directly before the provider call.
    pub(crate) fn start_request(
        &mut self,
        request: &ModelRequest,
        calls: usize,
        limit: usize,
        phase: usize,
        attempts: usize,
        attempt_limit: usize,
    ) {
        let previous = self.facts.take();
        let repairs = previous
            .as_ref()
            .map_or(0, |facts| facts.repair_calls_started);
        self.facts = Some(RecoveryFacts {
            turn_id: request.turn_id.clone(),
            agent_id: request.agent_id.clone(),
            provider: request.provider.clone(),
            model: request.model.clone(),
            phase,
            attempts_reserved: attempts,
            attempt_limit,
            provider_calls_started: calls,
            provider_call_limit: limit,
            repair_calls_started: repairs
                + usize::from(request.interaction_kind == ModelInteractionKind::MaapRepair),
            last_request_kind: request.interaction_kind,
            stage: previous
                .as_ref()
                .filter(|facts| facts.phase == phase)
                .and_then(|facts| facts.stage),
            disposition: RecoveryDisposition::Unknown,
            batch_planned: false,
        });
    }

    /// Updates budget facts after the existing owner admits/rejects an attempt.
    pub(crate) fn observe_budget(&mut self, phase: usize, attempts: usize, limit: usize) {
        if let Some(facts) = &mut self.facts {
            if facts.phase != phase {
                facts.stage = None;
            }
            facts.phase = phase;
            facts.attempts_reserved = attempts;
            facts.attempt_limit = limit;
        }
    }

    /// Records the branch-selected exit without parsing any diagnostic content.
    pub(crate) fn finish(
        &mut self,
        stage: Option<RecoveryStage>,
        disposition: RecoveryDisposition,
        planned: bool,
    ) {
        if let Some(facts) = &mut self.facts {
            facts.stage = stage.or(facts.stage);
            facts.disposition = disposition;
            facts.batch_planned = planned;
        }
    }
}
