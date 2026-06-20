//! Agent Readiness implementation.
//!
//! This module owns the agent readiness boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{BTreeMap, EnvironmentSignature, MezError, Result, validate_non_empty};

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
    ) -> Result<()> {
        let pane_id = pane_id.into();
        let marker = marker.into();
        validate_non_empty("pane id", &pane_id)?;
        validate_non_empty("readiness probe marker", &marker)?;
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
    ) -> Result<&PaneReadinessOverride> {
        let pane_id = pane_id.into();
        let reason = reason.into();
        validate_non_empty("pane id", &pane_id)?;
        validate_non_empty("readiness override reason", &reason)?;
        if !warning_acknowledged {
            return Err(MezError::forbidden(
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
            MezError::invalid_state("readiness override was not available after insertion")
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
pub fn decide_bootstrap_before_user_prompt(
    readiness: PaneReadinessState,
    previous_signature: Option<&EnvironmentSignature>,
    current_signature: Option<&EnvironmentSignature>,
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
