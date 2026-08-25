//! Snapshot, resume-plan, collection, and shared state-name serialization.

use super::{
    ClientRole, ClientState, LayoutLoadPlan, SessionState, SnapshotKind, SnapshotState, json_escape,
};

/// Runs the snapshots json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn snapshots_json(snapshots: &[SnapshotState]) -> String {
    format!(
        "[{}]",
        snapshots
            .iter()
            .map(snapshot_state_json)
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// Runs the snapshot state json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn snapshot_state_json(snapshot: &SnapshotState) -> String {
    format!(
        r#"{{"id":"{}","version":{},"session_id":"{}","name":{},"created_at":"{}","kind":"{}","restorable":{},"window_count":{},"pane_count":{},"limitations":{},"storage_ref":"{}"}}"#,
        json_escape(&snapshot.id),
        snapshot.version,
        json_escape(&snapshot.session_id),
        json_optional_string(snapshot.name.as_deref()),
        json_escape(&snapshot.created_at),
        snapshot_kind_name(snapshot.kind),
        snapshot.restorable,
        snapshot.window_count,
        snapshot.pane_count,
        string_array_json(&snapshot.limitations),
        json_escape(&snapshot.storage_ref)
    )
}

/// Runs the resume plan json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn resume_plan_json(plan: &LayoutLoadPlan) -> String {
    format!(
        r#"{{"session_id":"{}","window_count":{},"pane_count":{},"restart_required_panes":{},"limitations":{}}}"#,
        json_escape(&plan.session_id),
        plan.window_count,
        plan.pane_count,
        string_array_json(&plan.restart_required_panes),
        string_array_json(&plan.limitations)
    )
}

/// Runs the snapshot kind name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn snapshot_kind_name(kind: SnapshotKind) -> &'static str {
    match kind {
        SnapshotKind::Live => "live",
        SnapshotKind::Manual => "manual",
        SnapshotKind::Automatic => "automatic",
        SnapshotKind::CrashRecovery => "crash_recovery",
    }
}

/// Runs the json optional string operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn json_optional_string(value: Option<&str>) -> String {
    value
        .map(|value| format!(r#""{}""#, json_escape(value)))
        .unwrap_or_else(|| "null".to_string())
}

/// Runs the string array json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn string_array_json(values: &[String]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| format!(r#""{}""#, json_escape(value)))
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// Runs the session state name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn session_state_name(state: SessionState) -> &'static str {
    match state {
        SessionState::Running => "running",
        SessionState::Detached => "detached",
        SessionState::Empty => "empty",
        SessionState::Stopping => "stopping",
        SessionState::Failed => "failed",
    }
}

/// Runs the client role name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn client_role_name(role: ClientRole) -> &'static str {
    match role {
        ClientRole::Primary => "primary",
        ClientRole::Observer => "observer",
        ClientRole::Agent => "agent",
        ClientRole::Automation => "automation",
    }
}

/// Runs the client state name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn client_state_name(state: ClientState) -> &'static str {
    match state {
        ClientState::Attached => "attached",
        ClientState::Pending => "pending",
        ClientState::Detached => "detached",
        ClientState::Revoked => "revoked",
        ClientState::Failed => "failed",
    }
}
