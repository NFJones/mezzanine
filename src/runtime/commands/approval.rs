//! Agent approval and project-trust command helpers.
//!
//! This child module owns the parsing and display helpers for `/approve` and
//! `/trust` command flows. It deliberately keeps command execution in the
//! parent runtime command module while isolating the approval-selection and
//! project-trust formatting rules shared by those execution paths.

use super::super::{
    BlockedApprovalRequest, MezError, Path, PathBuf, Result, Value, discover_project_root,
};

/// Carries Agent Approve Scope state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AgentApproveScope {
    /// Represents the Once case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Once,
    /// Represents the Session case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Session,
    /// Represents the Project case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Project,
    /// Represents the Global case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Global,
}

impl AgentApproveScope {
    /// Runs the parse operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn parse(value: &str) -> Option<Self> {
        match value {
            "once" => Some(Self::Once),
            "session" => Some(Self::Session),
            "project" => Some(Self::Project),
            "global" => Some(Self::Global),
            _ => None,
        }
    }

    /// Runs the as str operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::Session => "session",
            Self::Project => "project",
            Self::Global => "global",
        }
    }
}

/// Carries Agent Approve Selection state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AgentApproveSelection {
    /// Stores the approval id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) approval_id: String,
    /// Stores the scope value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) scope: AgentApproveScope,
}

/// Carries Agent Project Trust Request state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AgentProjectTrustRequest {
    /// Stores the project root value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) project_root: PathBuf,
    /// Stores the overlay files value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) overlay_files: Vec<PathBuf>,
}

/// Runs the parse agent approve selection operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_agent_approve_selection(
    args: &str,
    pane_id: &str,
    pending_for_pane: &[&BlockedApprovalRequest],
) -> Result<AgentApproveSelection> {
    let tokens = args.split_whitespace().collect::<Vec<_>>();
    match tokens.as_slice() {
        [] => Ok(AgentApproveSelection {
            approval_id: select_default_agent_approval_id(pane_id, pending_for_pane)?,
            scope: AgentApproveScope::Once,
        }),
        [scope] if AgentApproveScope::parse(scope).is_some() => Ok(AgentApproveSelection {
            approval_id: select_default_agent_approval_id(pane_id, pending_for_pane)?,
            scope: AgentApproveScope::parse(scope)
                .ok_or_else(|| MezError::invalid_args("invalid approval scope"))?,
        }),
        [approval_id] => Ok(AgentApproveSelection {
            approval_id: select_named_agent_approval_id(approval_id, pane_id, pending_for_pane)?,
            scope: AgentApproveScope::Once,
        }),
        [approval_id, scope] => Ok(AgentApproveSelection {
            approval_id: select_named_agent_approval_id(approval_id, pane_id, pending_for_pane)?,
            scope: AgentApproveScope::parse(scope).ok_or_else(|| {
                MezError::invalid_args("/approve scope must be once, session, project, or global")
            })?,
        }),
        _ => Err(MezError::invalid_args(
            "/approve expects [approval-id|latest] [once|session|project|global]",
        )),
    }
}

/// Runs the select default agent approval id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn select_default_agent_approval_id(
    pane_id: &str,
    pending_for_pane: &[&BlockedApprovalRequest],
) -> Result<String> {
    match pending_for_pane {
        [] => Err(MezError::invalid_args(format!(
            "no pending approvals for pane {pane_id}"
        ))),
        [approval] => Ok(approval.id.clone()),
        _ => Err(MezError::invalid_args(format!(
            "multiple pending approvals for pane {pane_id}; use /approve <approval-id>\n{}",
            pending_agent_approval_lines(pending_for_pane).join("\n")
        ))),
    }
}

/// Runs the select named agent approval id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn select_named_agent_approval_id(
    requested: &str,
    pane_id: &str,
    pending_for_pane: &[&BlockedApprovalRequest],
) -> Result<String> {
    if requested == "latest" {
        return pending_for_pane
            .last()
            .map(|approval| approval.id.clone())
            .ok_or_else(|| {
                MezError::invalid_args(format!("no pending approvals for pane {pane_id}"))
            });
    }
    Ok(requested.to_string())
}

/// Runs the pending agent approval lines operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn pending_agent_approval_lines(pending: &[&BlockedApprovalRequest]) -> Vec<String> {
    pending
        .iter()
        .map(|approval| {
            format!(
                "approval {} pending: {} {}",
                approval.id,
                approval.action_kind,
                agent_approval_summary_preview(&approval.action_summary)
            )
        })
        .collect()
}

/// Runs the agent approval summary preview operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn agent_approval_summary_preview(summary: &str) -> String {
    /// Defines the MAX APPROVAL SUMMARY CHARS const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    const MAX_APPROVAL_SUMMARY_CHARS: usize = 160;
    let mut preview = String::new();
    let mut chars = summary.trim().chars();
    for _ in 0..MAX_APPROVAL_SUMMARY_CHARS {
        let Some(ch) = chars.next() else {
            return preview;
        };
        preview.push(match ch {
            '\r' | '\n' => ' ',
            ch if ch.is_control() => ' ',
            ch => ch,
        });
    }
    if chars.next().is_some() {
        preview.push_str("...");
    }
    preview
}

/// Runs the agent approve control error message operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn agent_approve_control_error_message(response: &str) -> Option<String> {
    serde_json::from_str::<Value>(response)
        .ok()
        .and_then(|value| value.get("error").cloned())
        .and_then(|error| {
            error
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

/// Runs the agent approve pending display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn agent_approve_pending_display(
    pane_id: &str,
    pending: &[&BlockedApprovalRequest],
) -> String {
    if pending.is_empty() {
        format!("no pending approvals for pane {pane_id}")
    } else {
        format!(
            "pending approvals for pane {pane_id}:\n{}\nUse /approve <approval-id> [once|session|project|global].",
            pending_agent_approval_lines(pending).join("\n")
        )
    }
}

/// Runs the agent project trust log line operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn agent_project_trust_log_line(request: &AgentProjectTrustRequest) -> String {
    format!(
        "project trust pending: {} overlays={} (trust with /trust {})",
        agent_path_preview(&request.project_root),
        request.overlay_files.len(),
        agent_path_preview(&request.project_root)
    )
}

/// Runs the agent project trust pending display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn agent_project_trust_pending_display(pending: &[AgentProjectTrustRequest]) -> String {
    if pending.is_empty() {
        "no pending project trust requests".to_string()
    } else {
        format!(
            "pending project trust requests:\n{}\nUse /trust <project-root>.",
            pending
                .iter()
                .map(agent_project_trust_log_line)
                .collect::<Vec<_>>()
                .join("\n")
        )
    }
}

/// Runs the agent select project trust request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn agent_select_project_trust_request(
    args: &str,
    pending: &[AgentProjectTrustRequest],
) -> Result<AgentProjectTrustRequest> {
    let args = args.trim();
    match args {
        "" => match pending {
            [] => Err(MezError::invalid_args("no pending project trust requests")),
            [request] => Ok(request.clone()),
            _ => Err(MezError::invalid_args(format!(
                "multiple pending project trust requests; use /trust <project-root>\n{}",
                agent_project_trust_pending_display(pending)
            ))),
        },
        "latest" => pending
            .last()
            .cloned()
            .ok_or_else(|| MezError::invalid_args("no pending project trust requests")),
        "list" | "pending" => Err(MezError::invalid_state(
            "project trust list requests must be handled before selection",
        )),
        path => {
            let requested_root = discover_project_root(&PathBuf::from(path));
            pending
                .iter()
                .find(|request| project_trust_root_matches(&request.project_root, &requested_root))
                .cloned()
                .ok_or_else(|| {
                    MezError::invalid_args(format!(
                        "pending project trust request {} was not found",
                        requested_root.display()
                    ))
                })
        }
    }
}

/// Runs the project trust root matches operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn project_trust_root_matches(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    let left = std::fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = std::fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    left == right
}

/// Runs the agent path preview operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn agent_path_preview(path: &Path) -> String {
    agent_approval_summary_preview(&path.to_string_lossy())
}
