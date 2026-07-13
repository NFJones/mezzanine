//! Provider-independent pane readiness state and override policy.
//!
//! This module owns deterministic readiness decisions and one-epoch manual
//! overrides. Runtime probing, concrete shell environment signatures, pane
//! lifecycle orchestration, and product error aggregation remain outside this
//! crate.

use std::collections::BTreeMap;
use std::fmt;

/// Result type returned by pane-readiness operations.
pub type ReadinessResult<T> = Result<T, ReadinessError>;

/// Stable category for a pane-readiness failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadinessErrorKind {
    /// A caller supplied an empty or otherwise malformed value.
    InvalidArgs,
    /// A safety-sensitive override was attempted without acknowledgement.
    Forbidden,
    /// An internal readiness-state invariant was not preserved.
    InvalidState,
}

/// A deterministic pane-readiness failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadinessError {
    kind: ReadinessErrorKind,
    message: String,
}

impl ReadinessError {
    /// Creates a readiness failure with a stable category and diagnostic.
    pub fn new(kind: ReadinessErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Creates an invalid-argument readiness failure.
    pub fn invalid_args(message: impl Into<String>) -> Self {
        Self::new(ReadinessErrorKind::InvalidArgs, message)
    }

    /// Creates a forbidden readiness failure.
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(ReadinessErrorKind::Forbidden, message)
    }

    /// Creates an invalid-state readiness failure.
    pub fn invalid_state(message: impl Into<String>) -> Self {
        Self::new(ReadinessErrorKind::InvalidState, message)
    }

    /// Returns the stable failure category.
    pub fn kind(&self) -> ReadinessErrorKind {
        self.kind
    }

    /// Returns the diagnostic message without formatting the error.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ReadinessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ReadinessError {}

fn validate_required(field: &str, value: &str) -> ReadinessResult<()> {
    if value.trim().is_empty() {
        return Err(ReadinessError::invalid_args(format!(
            "{field} must not be empty"
        )));
    }
    Ok(())
}

// Pane readiness state and override handling.

/// Carries Pane Readiness State state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneReadinessState {
    /// Represents the Unknown case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Unknown,
    /// Represents the Prompt Candidate case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    PromptCandidate,
    /// Represents the Probing case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Probing,
    /// Represents the Ready case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Ready,
    /// Represents the Busy case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Busy,
    /// Represents the Degraded case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Degraded,
    /// Represents the Interactive Blocked case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    InteractiveBlocked,
    /// Represents the Full Screen case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    FullScreen,
    /// Represents the Password Prompt case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    PasswordPrompt,
}

/// Carries Readiness Override Revocation state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadinessOverrideRevocation {
    /// Represents the Command Start Metadata case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    CommandStartMetadata,
    /// Represents the Harness Owned Command case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    HarnessOwnedCommand,
    /// Represents the Alternate Screen Entry case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    AlternateScreenEntry,
    /// Represents the Foreground Interactive Prompt case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ForegroundInteractivePrompt,
    /// Represents the Primary Pid Changed case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    PrimaryPidChanged,
    /// Represents the Environment Signature Changed case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    EnvironmentSignatureChanged,
    /// Represents the Readiness Probe Failed case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ReadinessProbeFailed,
    /// Represents the Pane Closed case for this enumeration.
    ///
    /// Runtime cleanup uses this revocation when a pane leaves the session
    /// topology and all readiness overrides tied to that pane become invalid.
    PaneClosed,
    /// Represents the Consumed Epoch case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ConsumedEpoch,
}

/// Carries Pane Readiness Override state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneReadinessOverride {
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_id: String,
    /// Stores the epoch value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub epoch: u64,
    /// Stores the reason value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub reason: String,
    /// Stores the warning acknowledged value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub warning_acknowledged: bool,
}

/// Carries Pane Readiness Override Store state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaneReadinessOverrideStore {
    /// Stores the overrides value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) overrides: BTreeMap<String, PaneReadinessOverride>,
    /// Stores the active pending probe marker by pane id.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pending_probes: BTreeMap<String, String>,
}

impl PaneReadinessOverrideStore {
    /// Runs the record pending probe operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn record_pending_probe(
        &mut self,
        pane_id: impl Into<String>,
        marker: impl Into<String>,
    ) -> ReadinessResult<()> {
        let pane_id = pane_id.into();
        let marker = marker.into();
        validate_required("pane id", &pane_id)?;
        validate_required("readiness probe marker", &marker)?;
        self.pending_probes.insert(pane_id, marker);
        Ok(())
    }

    /// Runs the has pending probe operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn has_pending_probe(&self, pane_id: &str) -> bool {
        self.pending_probes.contains_key(pane_id)
    }

    /// Runs the pending probe marker operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn pending_probe_marker(&self, pane_id: &str) -> Option<&str> {
        self.pending_probes.get(pane_id).map(String::as_str)
    }

    /// Runs the clear pending probe operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn clear_pending_probe(&mut self, pane_id: &str) -> bool {
        self.pending_probes.remove(pane_id).is_some()
    }

    /// Runs the clear pending probe if marker matches operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn clear_pending_probe_if_matches(&mut self, pane_id: &str, marker: &str) -> bool {
        if self.pending_probe_marker(pane_id) != Some(marker) {
            return false;
        }
        self.clear_pending_probe(pane_id)
    }

    /// Runs the mark ready for epoch operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn mark_ready_for_epoch(
        &mut self,
        pane_id: impl Into<String>,
        epoch: u64,
        reason: impl Into<String>,
        warning_acknowledged: bool,
    ) -> ReadinessResult<&PaneReadinessOverride> {
        let pane_id = pane_id.into();
        let reason = reason.into();
        validate_required("pane id", &pane_id)?;
        validate_required("readiness override reason", &reason)?;
        if !warning_acknowledged {
            return Err(ReadinessError::forbidden(
                "mark-pane-ready requires acknowledgement of the readiness warning",
            ));
        }
        self.overrides.insert(
            pane_id.clone(),
            PaneReadinessOverride {
                pane_id: pane_id.clone(),
                epoch,
                reason,
                warning_acknowledged,
            },
        );
        self.clear_pending_probe(&pane_id);
        self.overrides.get(&pane_id).ok_or_else(|| {
            ReadinessError::invalid_state("readiness override was not available after insertion")
        })
    }

    /// Runs the allows epoch operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn allows_epoch(&self, pane_id: &str, epoch: u64) -> bool {
        self.overrides
            .get(pane_id)
            .is_some_and(|override_state| override_state.epoch == epoch)
    }

    /// Runs the revoke operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn revoke(
        &mut self,
        pane_id: &str,
        _reason: ReadinessOverrideRevocation,
    ) -> Option<PaneReadinessOverride> {
        self.overrides.remove(pane_id)
    }

    /// Runs the consume epoch operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn consume_epoch(&mut self, pane_id: &str, epoch: u64) -> Option<PaneReadinessOverride> {
        if self.allows_epoch(pane_id, epoch) {
            self.revoke(pane_id, ReadinessOverrideRevocation::ConsumedEpoch)
        } else {
            None
        }
    }
}

/// Carries Readiness Decision state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadinessDecision {
    /// Stores the may probe value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub may_probe: bool,
    /// Stores the may send agent command value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub may_send_agent_command: bool,
    /// Stores the stale signature allowed value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub stale_signature_allowed: bool,
    /// Stores the diagnostic value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub diagnostic: Option<String>,
}

/// Runs the readiness decision operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn readiness_decision(state: PaneReadinessState) -> ReadinessDecision {
    match state {
        PaneReadinessState::Ready => ReadinessDecision {
            may_probe: true,
            may_send_agent_command: true,
            stale_signature_allowed: false,
            diagnostic: None,
        },
        PaneReadinessState::Unknown
        | PaneReadinessState::PromptCandidate
        | PaneReadinessState::Degraded => ReadinessDecision {
            may_probe: true,
            may_send_agent_command: false,
            stale_signature_allowed: true,
            diagnostic: Some("pane needs a bounded readiness probe".to_string()),
        },
        PaneReadinessState::Busy
        | PaneReadinessState::Probing
        | PaneReadinessState::InteractiveBlocked
        | PaneReadinessState::FullScreen
        | PaneReadinessState::PasswordPrompt => ReadinessDecision {
            may_probe: false,
            may_send_agent_command: false,
            stale_signature_allowed: true,
            diagnostic: Some("pane is not at a verified shell boundary".to_string()),
        },
    }
}

/// Carries Bootstrap Decision state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapDecision {
    /// Stores the should bootstrap value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub should_bootstrap: bool,
    /// Stores the block turn value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub block_turn: bool,
    /// Stores the reason value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub reason: String,
}

/// Runs the decide bootstrap before user prompt operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn decide_bootstrap_before_user_prompt<T: PartialEq + ?Sized>(
    readiness: PaneReadinessState,
    previous_signature: Option<&T>,
    current_signature: Option<&T>,
) -> BootstrapDecision {
    let readiness = readiness_decision(readiness);
    if !readiness.may_probe {
        return BootstrapDecision {
            should_bootstrap: false,
            block_turn: current_signature.is_none(),
            reason: readiness
                .diagnostic
                .unwrap_or_else(|| "pane is not ready".to_string()),
        };
    }

    match (previous_signature, current_signature) {
        (None, Some(_)) => BootstrapDecision {
            should_bootstrap: true,
            block_turn: false,
            reason: "first observed environment signature".to_string(),
        },
        (Some(previous), Some(current)) if previous != current => BootstrapDecision {
            should_bootstrap: true,
            block_turn: false,
            reason: "environment signature changed".to_string(),
        },
        (Some(_), Some(_)) => BootstrapDecision {
            should_bootstrap: false,
            block_turn: false,
            reason: "environment signature unchanged".to_string(),
        },
        (_, None) => BootstrapDecision {
            should_bootstrap: false,
            block_turn: true,
            reason: "environment signature could not be observed".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PaneReadinessOverrideStore, PaneReadinessState, ReadinessErrorKind,
        ReadinessOverrideRevocation, decide_bootstrap_before_user_prompt, readiness_decision,
    };

    /// Bootstrap policy compares opaque product signatures without importing
    /// the concrete shell environment representation.
    #[test]
    fn bootstrap_policy_uses_comparable_signature_inputs() {
        let first = "host:user:/repo";
        let second = "host:user:/repo/sub";

        let unchanged = decide_bootstrap_before_user_prompt(
            PaneReadinessState::Ready,
            Some(first),
            Some(first),
        );
        let changed = decide_bootstrap_before_user_prompt(
            PaneReadinessState::Ready,
            Some(first),
            Some(second),
        );
        let blocked = decide_bootstrap_before_user_prompt::<str>(
            PaneReadinessState::PasswordPrompt,
            Some(first),
            None,
        );

        assert!(!unchanged.should_bootstrap);
        assert!(changed.should_bootstrap);
        assert!(blocked.block_turn);
    }

    /// Readiness decisions preserve probe and command safety across ready,
    /// uncertain, and blocked pane states.
    #[test]
    fn readiness_decisions_preserve_shell_boundary_policy() {
        let busy = readiness_decision(PaneReadinessState::Busy);
        let unknown = readiness_decision(PaneReadinessState::Unknown);
        let ready = readiness_decision(PaneReadinessState::Ready);

        assert!(!busy.may_probe);
        assert!(busy.stale_signature_allowed);
        assert!(unknown.may_probe);
        assert!(!unknown.may_send_agent_command);
        assert!(ready.may_probe);
        assert!(ready.may_send_agent_command);
    }

    /// Manual readiness overrides require explicit warning acknowledgement and
    /// remain valid for exactly one matching pane epoch.
    #[test]
    fn readiness_override_is_acknowledged_and_epoch_scoped() {
        let mut store = PaneReadinessOverrideStore::default();
        store.record_pending_probe("%1", "probe-1").unwrap();

        let error = store
            .mark_ready_for_epoch("%1", 7, "manual override", false)
            .unwrap_err();
        assert_eq!(error.kind(), ReadinessErrorKind::Forbidden);
        assert!(store.has_pending_probe("%1"));

        store
            .mark_ready_for_epoch("%1", 7, "manual override", true)
            .unwrap();
        assert!(store.allows_epoch("%1", 7));
        assert!(!store.allows_epoch("%1", 8));
        assert!(store.consume_epoch("%1", 7).is_some());
        assert!(!store.allows_epoch("%1", 7));
    }

    /// Revocation and marker matching prevent stale probes or environment
    /// changes from retaining an override for the wrong shell boundary.
    #[test]
    fn readiness_override_revocation_rejects_stale_boundaries() {
        let mut store = PaneReadinessOverrideStore::default();
        store
            .mark_ready_for_epoch("%1", 1, "manual override", true)
            .unwrap();
        assert!(
            store
                .revoke(
                    "%1",
                    ReadinessOverrideRevocation::EnvironmentSignatureChanged,
                )
                .is_some()
        );

        store.record_pending_probe("%1", "probe-a").unwrap();
        store.record_pending_probe("%1", "probe-b").unwrap();
        assert!(!store.clear_pending_probe_if_matches("%1", "probe-a"));
        assert!(store.clear_pending_probe_if_matches("%1", "probe-b"));
    }
}
