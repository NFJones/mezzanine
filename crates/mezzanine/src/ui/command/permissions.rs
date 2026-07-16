//! Command Permissions implementation.
//!
//! This module owns the command permissions boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AuditActor, AuditRecord, ClientId, CommandInvocation, CredentialStoreKind, MezError,
    PaneReadinessState, Result, Session,
};
use crate::protocol::identifiers::is_ascii_identifier_segment;

// Permission and approval-bypass command helpers.

/// Runs the command target pane id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn command_target_pane_id(
    session: &Session,
    invocation: &CommandInvocation,
) -> Result<String> {
    let target = invocation
        .target_arg()
        .or_else(|| invocation.positional_args().first().copied());
    match target {
        None => session
            .active_window()
            .map(|window| window.active_pane().id.to_string())
            .ok_or_else(|| MezError::invalid_state("session has no active window")),
        Some(target) => {
            if let Some(window) = session.active_window()
                && let Some(pane) = window
                    .panes()
                    .iter()
                    .find(|pane| pane.id.as_str() == target || pane.index.to_string() == target)
            {
                return Ok(pane.id.to_string());
            }

            session
                .windows()
                .iter()
                .flat_map(|window| window.panes())
                .find(|pane| pane.id.as_str() == target)
                .map(|pane| pane.id.to_string())
                .ok_or_else(|| {
                    MezError::new(crate::error::MezErrorKind::NotFound, "pane not found")
                })
        }
    }
}

/// Runs the mark pane ready warning display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mark_pane_ready_warning_display(
    pane_id: &str,
    current_state: PaneReadinessState,
) -> String {
    format!(
        "pane={pane_id}:readiness_state={}:override=not-applied:acknowledgement_required=true:warning=automatic-readiness-not-verified:risk=next-agent-command-may-reach-foreground-program",
        pane_readiness_state_name(current_state)
    )
}

/// Runs the mark pane ready audit record operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mark_pane_ready_audit_record(
    session: &Session,
    primary_client_id: &ClientId,
    pane_id: &str,
    current_state: PaneReadinessState,
    epoch: u64,
    reason: &str,
) -> AuditRecord {
    let mut record = AuditRecord::new(
        session.id.to_string(),
        AuditActor {
            kind: "primary_client".to_string(),
            id: primary_client_id.to_string(),
        },
        "agent_readiness",
        "mark_pane_ready",
    )
    .with_pane_id(pane_id.to_string())
    .with_metadata("readiness_state", pane_readiness_state_name(current_state))
    .with_metadata("epoch", epoch.to_string())
    .with_metadata("reason", reason.to_string());
    record.approval_state = "accepted".to_string();
    record.outcome = "applied".to_string();
    record.sanitized()
}

/// Runs the pane readiness state name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn pane_readiness_state_name(state: PaneReadinessState) -> &'static str {
    match state {
        PaneReadinessState::Unknown => "unknown",
        PaneReadinessState::PromptCandidate => "prompt-candidate",
        PaneReadinessState::Probing => "probing",
        PaneReadinessState::Ready => "ready",
        PaneReadinessState::Busy => "busy",
        PaneReadinessState::Degraded => "degraded",
        PaneReadinessState::InteractiveBlocked
        | PaneReadinessState::FullScreen
        | PaneReadinessState::PasswordPrompt => "interactive-blocked",
    }
}

/// Runs the credential store kind name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn credential_store_kind_name(kind: CredentialStoreKind) -> &'static str {
    match kind {
        CredentialStoreKind::OperatingSystem => "os",
        CredentialStoreKind::PrivateFileFallback => "file",
    }
}

/// Runs the validate command identifier operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_command_identifier(value: &str, label: &str) -> Result<()> {
    if !is_ascii_identifier_segment(value) {
        return Err(MezError::invalid_args(format!(
            "{label} must contain only ASCII letters, digits, '_' or '-'"
        )));
    }
    Ok(())
}
