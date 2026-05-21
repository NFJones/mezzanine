//! Control State Json implementation.
//!
//! This module owns the control state json boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AgentShellSession, AgentShellStore, AgentShellVisibility, AgentTurnLedger, AgentTurnState,
    ApprovalDecision, AuditActor, AuditRecord, BlockedApprovalQueue, BlockedApprovalRequest,
    BlockedApprovalState, ClientId, ClientRole, ClientState, EventAudience, EventKind, EventLog,
    FrameContext, FrameOverflow, GrantedRole, JsonRpcRequest, MAX_EVENT_REPLAY_RETENTION,
    McpRegistry, McpServerKind, McpServerStatus, MezError, ObserverDecisionState,
    PaneCaptureSource, ProjectTrustRecord, Result, Session, SessionState, SnapshotKind,
    SnapshotResumePlan, SnapshotState, TrustDecision, VisibleEvent, Window, json_escape,
    json_raw_field, json_string_field, pane_by_id, pane_target_checked_resolved,
    parse_json_object_value, reject_unknown_json_fields, render_frame_template,
    require_idempotency_key, require_session_target_matches_value, resolve_pane_target_value,
    resolve_window_target_value, target_or_active_pane, target_value_has_pane_shape,
    unix_seconds_to_rfc3339, window_by_id,
};
use crate::agent::{AgentShellCommandOutcome, execute_agent_shell_command};
use crate::layout::LayoutNode;
use crate::permissions::builtin_rules;
use crate::process::PaneExitStatus;
use crate::session::ClientTerminalDescriptor;
use crate::terminal::{DEFAULT_HISTORY_LIMIT, DEFAULT_PANE_TERM, TerminalFrameContext};
use std::collections::BTreeMap;

// Control state serialization helpers.

/// Runs the granted role name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn granted_role_name(role: GrantedRole) -> &'static str {
    match role {
        GrantedRole::Primary => "primary",
        GrantedRole::PendingObserver => "pending_observer",
        GrantedRole::Observer => "observer",
        GrantedRole::Agent => "agent",
        GrantedRole::Automation => "automation",
    }
}

/// Runs the session summary json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn session_summary_json(session: &Session) -> String {
    let active_window_id = session.active_window().map(|window| window.id.to_string());
    let primary_client_id = session
        .primary_client_id()
        .map(|client_id| client_id.to_string());
    let attached_client_count = session
        .clients()
        .iter()
        .filter(|client| client.state == ClientState::Attached)
        .count();
    format!(
        r#"{{"id":"{}","version":1,"name":"{}","state":"{}","created_at":{},"last_attached_at":{},"window_count":{},"attached_client_count":{},"has_primary":{},"primary_client_id":{},"active_window_id":{}}}"#,
        json_escape(&session.id.to_string()),
        json_escape(&session.name),
        session_state_name(session.state),
        optional_rfc3339_timestamp_json(Some(session.created_at_unix_seconds)),
        optional_rfc3339_timestamp_json(session.last_attached_at_unix_seconds),
        session.windows().len(),
        attached_client_count,
        session.primary_client_id().is_some(),
        json_optional_string(primary_client_id.as_deref()),
        json_optional_string(active_window_id.as_deref())
    )
}

/// Runs the session state json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn session_state_json(session: &Session) -> String {
    let primary_client_id = session
        .primary_client_id()
        .map(|client_id| client_id.to_string());
    let active_window_id = session.active_window().map(|window| window.id.to_string());
    format!(
        r#"{{"id":"{}","version":1,"session_id":"{}","name":"{}","state":"{}","created_at":{},"updated_at":{},"primary_client_id":{},"authoritative_size":{{"columns":{},"rows":{}}},"active_window_id":{},"windows":{},"window_count":{},"clients":{},"observers":{},"config_generation":{},"permission_summary":{}}}"#,
        json_escape(&session.id.to_string()),
        json_escape(&session.id.to_string()),
        json_escape(&session.name),
        session_state_name(session.state),
        optional_rfc3339_timestamp_json(Some(session.created_at_unix_seconds)),
        optional_rfc3339_timestamp_json(Some(session.updated_at_unix_seconds)),
        json_optional_string(primary_client_id.as_deref()),
        session.authoritative_size.columns,
        session.authoritative_size.rows,
        json_optional_string(active_window_id.as_deref()),
        windows_json(session),
        session.windows().len(),
        clients_json(session),
        observers_json(session),
        session.config_generation,
        permission_summary_json()
    )
}

/// Runs the session state json for params operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn session_state_json_for_params(
    session: &Session,
    params: Option<&str>,
) -> Result<String> {
    state_request_session_target_matches(session, params, "session/get params")?;
    Ok(session_state_json(session))
}

/// Runs the permission summary json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn permission_summary_json() -> String {
    format!(
        r#"{{"preset":"read-only","approval_policy":"ask","bypass_active":false,"trusted_project":false,"trusted_directories":[],"read_scopes":[],"write_scopes":[],"command_rule_generation":{}}}"#,
        builtin_rules().len()
    )
}

/// Runs the windows json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn windows_json(session: &Session) -> String {
    let windows = session
        .windows()
        .iter()
        .map(|window| window_state_json(session, window))
        .collect::<Vec<_>>();
    format!("[{}]", windows.join(","))
}

/// Runs the windows json for params operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn windows_json_for_params(session: &Session, params: Option<&str>) -> Result<String> {
    state_request_session_target_matches(session, params, "window/list params")?;
    Ok(windows_json(session))
}

/// Runs the panes json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn panes_json(session: &Session) -> String {
    let panes = session
        .active_window()
        .map(|window| {
            window
                .panes()
                .iter()
                .map(|pane| pane_state_json(session, window, pane))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    format!("[{}]", panes.join(","))
}

/// Runs the panes json for params operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn panes_json_for_params(session: &Session, params: Option<&str>) -> Result<String> {
    let Some(window_ids) = state_request_pane_list_window_ids(session, params, "pane/list params")?
    else {
        return Ok(panes_json(session));
    };
    let panes = window_ids
        .iter()
        .map(|window_id| window_by_id(session, window_id))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flat_map(|window| {
            window
                .panes()
                .iter()
                .map(|pane| pane_state_json(session, window, pane))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    Ok(format!("[{}]", panes.join(",")))
}

/// Runs the clients json for params operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn clients_json_for_params(session: &Session, params: Option<&str>) -> Result<String> {
    state_request_session_target_matches(session, params, "client/list params")?;
    Ok(clients_json(session))
}

/// Runs the observers json for params operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn observers_json_for_params(session: &Session, params: Option<&str>) -> Result<String> {
    state_request_session_target_matches(session, params, "observer/list params")?;
    let state = observer_state_filter_from_params(params, "observer/list params")?;
    Ok(observers_json_for_state(session, state))
}

/// Validate a read-only state request `target` as a SessionTarget when present.
pub(crate) fn state_request_session_target_matches(
    session: &Session,
    params: Option<&str>,
    label: &str,
) -> Result<()> {
    let Some(target) = state_request_target_value(params, label)? else {
        return Ok(());
    };
    require_session_target_matches_value(session, &target)
}

/// Validate a nullable read-only `target` as a SessionTarget when present.
pub(crate) fn nullable_state_request_session_target_matches(
    session: &Session,
    params: Option<&str>,
    label: &str,
) -> Result<()> {
    let Some(params) = params else {
        return Ok(());
    };
    let value = parse_json_object_value(params, label)?;
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args(format!("{label} must be an object")))?;
    let Some(target) = object.get("target") else {
        return Ok(());
    };
    if target.is_null() {
        return Ok(());
    }
    require_session_target_matches_value(session, target)
}

/// Resolve the optional `pane/list` target into the windows whose panes should
/// be serialized. A missing target preserves legacy active-window behavior,
/// while a SessionTarget expands to every window in the current session.
pub(crate) fn state_request_pane_list_window_ids(
    session: &Session,
    params: Option<&str>,
    label: &str,
) -> Result<Option<Vec<String>>> {
    let Some(target) = state_request_target_value(params, label)? else {
        return Ok(None);
    };
    if state_target_has_window_shape(session, &target) {
        let window_id = resolve_window_target_value(session, &target)?
            .ok_or_else(|| MezError::invalid_args("pane/list WindowTarget requires a selector"))?;
        return Ok(Some(vec![window_id]));
    }
    require_session_target_matches_value(session, &target)?;
    Ok(Some(
        session
            .windows()
            .iter()
            .map(|window| window.id.to_string())
            .collect(),
    ))
}

/// Runs the state request target value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn state_request_target_value(
    params: Option<&str>,
    label: &str,
) -> Result<Option<serde_json::Value>> {
    let Some(params) = params else {
        return Ok(None);
    };
    let value = parse_json_object_value(params, label)?;
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args(format!("{label} must be an object")))?;
    Ok(object.get("target").cloned())
}

/// Runs the state target has window shape operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn state_target_has_window_shape(session: &Session, value: &serde_json::Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    object.contains_key("window_id")
        || object.contains_key("window_index")
        || object.contains_key("window_name")
        || object.contains_key("index")
        || object.contains_key("active")
        || object.contains_key("session")
        || object.contains_key("default_session")
        || object
            .get("name")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|name| name != session.name)
}

/// Runs the window state json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn window_state_json(session: &Session, window: &Window) -> String {
    format!(
        r#"{{"id":"{}","version":1,"session_id":"{}","window_id":"{}","index":{},"name":"{}","active":{},"created_at":{},"size":{{"columns":{},"rows":{}}},"active_pane_id":{},"panes":{},"pane_count":{},"layout":{}}}"#,
        json_escape(&window.id.to_string()),
        json_escape(&session.id.to_string()),
        json_escape(&window.id.to_string()),
        window.index,
        json_escape(&window.name),
        session
            .active_window()
            .is_some_and(|active| active.id == window.id),
        optional_rfc3339_timestamp_json(window.created_at_unix_seconds),
        window.size.columns,
        window.size.rows,
        json_optional_string(Some(window.active_pane().id.as_str())),
        window_panes_json(session, window),
        window.panes().len(),
        layout_state_json(window)
    )
}

/// Runs the window panes json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn window_panes_json(session: &Session, window: &Window) -> String {
    let panes = window
        .panes()
        .iter()
        .map(|pane| pane_state_json(session, window, pane))
        .collect::<Vec<_>>();
    format!("[{}]", panes.join(","))
}

/// Runs the pane state json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn pane_state_json(
    session: &Session,
    window: &Window,
    pane: &crate::layout::Pane,
) -> String {
    let metadata = session.pane_state_metadata(pane.id.as_str());
    pane_state_json_with_runtime(
        session.id.as_str(),
        window,
        pane,
        PaneRuntimeStateJsonFields {
            primary_pid: None,
            process_state: None,
            exit_status: None,
            current_working_directory: metadata
                .and_then(|metadata| metadata.current_working_directory.as_deref()),
            readiness_state: metadata
                .map(|metadata| metadata.readiness_state.as_str())
                .unwrap_or("unknown"),
            alternate_screen_active: metadata
                .map(|metadata| metadata.alternate_screen_active)
                .unwrap_or(false),
        },
    )
}

/// Runs the pane state json with capture operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn pane_state_json_with_capture(
    session_id: &str,
    window: &Window,
    pane: &crate::layout::Pane,
    source: &PaneCaptureSource,
) -> String {
    pane_state_json_with_runtime(
        session_id,
        window,
        pane,
        PaneRuntimeStateJsonFields {
            primary_pid: source.primary_pid,
            process_state: source.process_state.as_deref(),
            exit_status: source.exit_status.as_ref(),
            current_working_directory: None,
            readiness_state: "unknown",
            alternate_screen_active: source.alternate_screen_active,
        },
    )
}

/// Runtime-backed process and terminal fields included in pane state JSON.
struct PaneRuntimeStateJsonFields<'a> {
    /// Primary child process id for the pane, when a process is attached.
    primary_pid: Option<u32>,
    /// Process lifecycle state supplied by runtime capture data.
    process_state: Option<&'a str>,
    /// Exit status supplied by runtime capture data, if the pane process ended.
    exit_status: Option<&'a PaneExitStatus>,
    /// Current working directory reported for the pane process.
    current_working_directory: Option<&'a str>,
    /// Readiness state reported by pane metadata.
    readiness_state: &'a str,
    /// Whether the pane terminal is currently using the alternate screen.
    alternate_screen_active: bool,
}

/// Runs the pane state json with runtime operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn pane_state_json_with_runtime(
    session_id: &str,
    window: &Window,
    pane: &crate::layout::Pane,
    runtime: PaneRuntimeStateJsonFields<'_>,
) -> String {
    let process_state = runtime.process_state.unwrap_or_else(|| {
        if runtime.primary_pid.is_some() {
            "running"
        } else if pane.live {
            "starting"
        } else {
            "exited"
        }
    });
    let exit_status = runtime
        .exit_status
        .map(|status| status.to_json())
        .unwrap_or_else(|| "null".to_string());
    format!(
        r#"{{"id":"{}","version":1,"session_id":"{}","window_id":"{}","pane_id":"{}","index":{},"title":"{}","active":{},"size":{{"columns":{},"rows":{}}},"columns":{},"rows":{},"primary_pid":{},"process_state":"{}","exit_status":{},"current_working_directory":{},"terminal_profile":"{}","history_limit":{},"alternate_screen_active":{},"readiness_state":"{}","agent_id":null,"live":{}}}"#,
        json_escape(&pane.id.to_string()),
        json_escape(session_id),
        json_escape(&window.id.to_string()),
        json_escape(&pane.id.to_string()),
        pane.index,
        json_escape(&pane.title),
        pane.active,
        pane.size.columns,
        pane.size.rows,
        pane.size.columns,
        pane.size.rows,
        runtime
            .primary_pid
            .map(|pid| pid.to_string())
            .unwrap_or_else(|| "null".to_string()),
        json_escape(process_state),
        exit_status,
        json_optional_string(runtime.current_working_directory),
        json_escape(DEFAULT_PANE_TERM),
        DEFAULT_HISTORY_LIMIT,
        runtime.alternate_screen_active,
        json_escape(runtime.readiness_state),
        pane.live
    )
}

/// Runs the layout state json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn layout_state_json(window: &Window) -> String {
    let root = layout_root_json(window);
    format!(
        r#"{{"id":"layout-{}","version":1,"window_id":"{}","root":{},"minimum_pane_size":{{"columns":2,"rows":2}}}}"#,
        json_escape(&window.id.to_string()),
        json_escape(&window.id.to_string()),
        root
    )
}

/// Runs the layout root json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn layout_root_json(window: &Window) -> String {
    let geometries = window.pane_geometries();
    layout_node_json(window.layout_root(), window, &geometries)
}

/// Runs the layout node json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn layout_node_json(
    node: &LayoutNode,
    window: &Window,
    geometries: &[crate::layout::PaneGeometry],
) -> String {
    match node {
        LayoutNode::Pane { index } => {
            let Some(pane) = window.panes().get(*index) else {
                return r#"{"type":"pane","pane_id":"","size":{"columns":0,"rows":0},"geometry":{"column":0,"row":0,"columns":0,"rows":0}}"#.to_string();
            };
            let geometry = geometries
                .iter()
                .find(|geometry| geometry.index == *index)
                .copied()
                .unwrap_or(crate::layout::PaneGeometry {
                    index: *index,
                    column: 0,
                    row: 0,
                    columns: pane.size.columns,
                    rows: pane.size.rows,
                });
            format!(
                r#"{{"type":"pane","pane_id":"{}","size":{{"columns":{},"rows":{}}},"geometry":{}}}"#,
                json_escape(&pane.id.to_string()),
                pane.size.columns,
                pane.size.rows,
                layout_pane_geometry_json(&geometry)
            )
        }
        LayoutNode::Split {
            direction,
            children,
        } => {
            let child_json = children
                .iter()
                .map(|child| layout_node_json(child, window, geometries))
                .collect::<Vec<_>>();
            let sizes = children
                .iter()
                .map(|child| {
                    child
                        .allocation_on_axis(window.panes(), *direction)
                        .to_string()
                })
                .collect::<Vec<_>>();
            format!(
                r#"{{"type":"split","direction":"{}","children":[{}],"sizes":[{}]}}"#,
                direction.name(),
                child_json.join(","),
                sizes.join(",")
            )
        }
    }
}

/// Runs the layout pane geometry json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn layout_pane_geometry_json(geometry: &crate::layout::PaneGeometry) -> String {
    format!(
        r#"{{"column":{},"row":{},"columns":{},"rows":{}}}"#,
        geometry.column, geometry.row, geometry.columns, geometry.rows
    )
}

/// Runs the frame read json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn frame_read_json(session: &Session, params: Option<&str>) -> Result<String> {
    frame_read_json_with_context(session, params, &TerminalFrameContext::default())
}

/// Runs the frame read json with context operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn frame_read_json_with_context(
    session: &Session,
    params: Option<&str>,
    frame_context: &TerminalFrameContext,
) -> Result<String> {
    let (window, pane) = frame_read_target(session, params)?;
    let fields = frame_read_fields(session, window, pane, frame_context);
    let context = fields
        .iter()
        .fold(FrameContext::new(), |context, (key, value)| {
            context.with(*key, value.as_str())
        });
    let rendered = render_frame_template(
        "#{window.index}:#{window.title} #{pane.index}:#{pane.title}",
        &context,
        usize::from(window.size.columns),
        FrameOverflow::Elide,
    );
    Ok(format!(
        r#"{{"fields":{{{}}},"rendered":"{}"}}"#,
        frame_read_fields_json(&fields),
        json_escape(&rendered)
    ))
}

/// Runs the frame read fields operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn frame_read_fields(
    session: &Session,
    window: &Window,
    pane: &crate::layout::Pane,
    frame_context: &TerminalFrameContext,
) -> Vec<(&'static str, String)> {
    let pane_context = frame_context.panes.get(pane.id.as_str());
    let pending_observer_count = if frame_context.pending_observer_count == 0 {
        session
            .observers()
            .iter()
            .filter(|observer| observer.state == ObserverDecisionState::Pending)
            .count()
    } else {
        frame_context.pending_observer_count
    };
    vec![
        (
            "session.id",
            frame_context
                .session_id
                .clone()
                .unwrap_or_else(|| session.id.to_string()),
        ),
        ("session.name", session.name.clone()),
        ("window.id", window.id.to_string()),
        ("window.index", window.index.to_string()),
        (
            "window.list",
            if frame_context.windows.is_empty() {
                format!(" {}: {} ", window.index, window.title())
            } else {
                frame_context
                    .windows
                    .iter()
                    .map(|window| format!(" {}: {} ", window.index, window.title))
                    .collect::<Vec<_>>()
                    .join(" ")
            },
        ),
        ("window.title", window.title()),
        ("window.name", window.name.clone()),
        (
            "window.active",
            session
                .active_window()
                .is_some_and(|active| active.id == window.id)
                .to_string(),
        ),
        ("window.pane_count", window.panes().len().to_string()),
        ("layout.name", window.layout_policy().name().to_string()),
        (
            "agent.active_count",
            frame_context
                .window_agent_active_counts
                .get(window.id.as_str())
                .copied()
                .unwrap_or_default()
                .to_string(),
        ),
        (
            "message.unread_count",
            frame_context
                .window_unread_message_counts
                .get(window.id.as_str())
                .copied()
                .unwrap_or_default()
                .to_string(),
        ),
        ("pane.id", pane.id.to_string()),
        ("pane.index", pane.index.to_string()),
        ("pane.title", pane.title.clone()),
        ("pane.active", pane.active.to_string()),
        (
            "pane.size",
            format!("{}x{}", pane.size.columns, pane.size.rows),
        ),
        (
            "pane.primary_pid",
            pane_context
                .and_then(|context| context.primary_pid)
                .map(|pid| pid.to_string())
                .unwrap_or_default(),
        ),
        (
            "pane.process_name",
            pane_context
                .and_then(|context| context.process_name.clone())
                .unwrap_or_default(),
        ),
        (
            "pane.exit_status",
            pane_context
                .and_then(|context| context.exit_status.clone())
                .unwrap_or_default(),
        ),
        (
            "pane.mode",
            pane_context
                .and_then(|context| context.mode.clone())
                .unwrap_or_else(|| "normal".to_string()),
        ),
        (
            "agent.id",
            pane_context
                .and_then(|context| context.agent_id.clone())
                .unwrap_or_default(),
        ),
        (
            "agent.name",
            pane_context
                .and_then(|context| context.agent_name.clone())
                .unwrap_or_default(),
        ),
        (
            "agent.status",
            pane_context
                .and_then(|context| context.agent_status.clone())
                .unwrap_or_else(|| "idle".to_string()),
        ),
        (
            "agent.model",
            pane_context
                .and_then(|context| context.agent_model.clone())
                .unwrap_or_default(),
        ),
        (
            "policy.mode",
            frame_context.policy_mode.clone().unwrap_or_default(),
        ),
        ("observer.pending_count", pending_observer_count.to_string()),
        (
            "history.position",
            pane_context
                .and_then(|context| context.history_position.clone())
                .unwrap_or_default(),
        ),
    ]
}

/// Runs the frame read fields json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn frame_read_fields_json(fields: &[(&str, String)]) -> String {
    fields
        .iter()
        .map(|(key, value)| format!(r#""{}":"{}""#, json_escape(key), json_escape(value)))
        .collect::<Vec<_>>()
        .join(",")
}

/// Runs the frame read target operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn frame_read_target<'a>(
    session: &'a Session,
    params: Option<&str>,
) -> Result<(&'a Window, &'a crate::layout::Pane)> {
    let Some(params) = params else {
        let window = session
            .active_window()
            .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
        return Ok((window, window.active_pane()));
    };
    let value = parse_json_object_value(params, "frame/read params")?;
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args("frame/read params must be an object"))?;
    let Some(target) = object.get("target") else {
        let window = session
            .active_window()
            .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
        return Ok((window, window.active_pane()));
    };

    if target_value_has_pane_shape(target) {
        let pane_id = resolve_pane_target_value(session, target)?
            .ok_or_else(|| MezError::invalid_args("frame/read target did not resolve a pane"))?;
        return pane_by_id(session, &pane_id);
    }
    if let Some(window_id) = resolve_window_target_value(session, target)? {
        let window = window_by_id(session, &window_id)?;
        return Ok((window, window.active_pane()));
    }
    Err(MezError::invalid_args(
        "frame/read target must be a WindowTarget or PaneTarget",
    ))
}

/// Runs the clients json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn clients_json(session: &Session) -> String {
    let clients = session
        .clients()
        .iter()
        .map(|client| client_json(session, client))
        .collect::<Vec<_>>();
    format!("[{}]", clients.join(","))
}

/// Runs the client json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn client_json(session: &Session, client: &crate::session::Client) -> String {
    let terminal_descriptor = generic_client_terminal_descriptor(session, client);
    let terminal_size = terminal_descriptor
        .as_ref()
        .map(|terminal| crate::layout::Size {
            columns: terminal.columns,
            rows: terminal.rows,
        });
    format!(
        r#"{{"id":"{}","version":1,"client_id":"{}","name":"{}","role":"{}","requested_role":"{}","state":"{}","attached_at":{},"last_seen_at":{},"descriptor":{{"name":"{}","interactive":{},"terminal":{}}},"terminal_size":{},"interactive":{}}}"#,
        json_escape(&client.id.to_string()),
        json_escape(&client.id.to_string()),
        json_escape(&client.name),
        client_role_name(client.role),
        client_requested_role_name(client.role),
        client_state_name(client.state),
        optional_rfc3339_timestamp_json(client.attached_at_unix_seconds),
        optional_rfc3339_timestamp_json(client.last_seen_at_unix_seconds),
        json_escape(&client.name),
        client.interactive,
        generic_client_terminal_descriptor_json(terminal_descriptor.as_ref()),
        generic_size_object_json(terminal_size),
        client.interactive
    )
}

/// Runs the client requested role name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn client_requested_role_name(role: ClientRole) -> &'static str {
    match role {
        ClientRole::PendingObserver => "observer",
        _ => client_role_name(role),
    }
}

/// Runs the generic client terminal descriptor operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn generic_client_terminal_descriptor(
    session: &Session,
    client: &crate::session::Client,
) -> Option<ClientTerminalDescriptor> {
    if let Some(terminal) = client.terminal.as_ref() {
        return Some(terminal.clone());
    }
    let is_primary = session
        .primary_client_id()
        .is_some_and(|primary| primary == &client.id);
    (is_primary && client.interactive && client.state == ClientState::Attached).then(|| {
        ClientTerminalDescriptor {
            columns: session.authoritative_size.columns,
            rows: session.authoritative_size.rows,
            term: DEFAULT_PANE_TERM.to_string(),
            features: Vec::new(),
        }
    })
}

/// Runs the generic size object json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn generic_size_object_json(size: Option<crate::layout::Size>) -> String {
    size.map(|size| format!(r#"{{"columns":{},"rows":{}}}"#, size.columns, size.rows))
        .unwrap_or_else(|| "null".to_string())
}

/// Runs the generic client terminal descriptor json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn generic_client_terminal_descriptor_json(terminal: Option<&ClientTerminalDescriptor>) -> String {
    terminal
        .map(generic_client_terminal_descriptor_object_json)
        .unwrap_or_else(|| "null".to_string())
}

/// Runs the generic client terminal descriptor object json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn generic_client_terminal_descriptor_object_json(terminal: &ClientTerminalDescriptor) -> String {
    if terminal.features.is_empty() {
        format!(
            r#"{{"columns":{},"rows":{},"term":"{}"}}"#,
            terminal.columns,
            terminal.rows,
            json_escape(&terminal.term)
        )
    } else {
        format!(
            r#"{{"columns":{},"rows":{},"term":"{}","features":{}}}"#,
            terminal.columns,
            terminal.rows,
            json_escape(&terminal.term),
            string_array_json(&terminal.features)
        )
    }
}

/// Runs the observers json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn observers_json(session: &Session) -> String {
    observers_json_for_state(session, None)
}

/// Runs the observers json for state operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn observers_json_for_state(session: &Session, state: Option<ObserverDecisionState>) -> String {
    let observers = session
        .observers()
        .iter()
        .filter(|observer| state.is_none_or(|state| observer.state == state))
        .map(observer_json_by_ref)
        .collect::<Vec<_>>();
    format!("[{}]", observers.join(","))
}

/// Runs the agents json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn agents_json(session: &Session) -> String {
    let agents = session
        .windows()
        .iter()
        .flat_map(|window| {
            window
                .panes()
                .iter()
                .map(|pane| agent_state_json(session.id.as_str(), window, pane, false))
        })
        .collect::<Vec<_>>();
    format!("[{}]", agents.join(","))
}

/// Runs the agents json for params operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn agents_json_for_params(session: &Session, params: Option<&str>) -> Result<String> {
    state_request_session_target_matches(session, params, "agent/list params")?;
    Ok(agents_json(session))
}

/// Serializes `agent/list` with optional pane-keyed model profile names.
///
/// The optional map lets live runtime dispatch replace the generic offline
/// `default` profile placeholder with the selected profile from runtime state
/// while keeping generic control fixtures unchanged.
pub(super) fn dispatch_agent_list_with_store_and_model_profiles(
    request: &JsonRpcRequest,
    session: &Session,
    agent_store: &AgentShellStore,
    model_profiles_by_pane: Option<&BTreeMap<String, String>>,
) -> Result<String> {
    state_request_session_target_matches(session, request.params.as_deref(), "agent/list params")?;
    let agents = session
        .windows()
        .iter()
        .flat_map(|window| {
            window.panes().iter().map(|pane| {
                let model_profile = model_profiles_by_pane
                    .and_then(|profiles| profiles.get(pane.id.as_str()))
                    .map(String::as_str)
                    .unwrap_or("default");
                agent_store.get(pane.id.as_str()).map_or_else(
                    || {
                        agent_state_json_with_model_profile(
                            session.id.as_str(),
                            window,
                            pane,
                            false,
                            model_profile,
                        )
                    },
                    |agent_session| {
                        agent_state_json_with_shell_session_and_model_profile(
                            session.id.as_str(),
                            pane,
                            agent_session,
                            model_profile,
                        )
                    },
                )
            })
        })
        .collect::<Vec<_>>();
    Ok(format!(r#"{{"agents":[{}]}}"#, agents.join(",")))
}

/// Runs the dispatch agent shell visibility with store operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn dispatch_agent_shell_visibility_with_store(
    request: &JsonRpcRequest,
    session: &Session,
    agent_store: &mut AgentShellStore,
) -> Result<String> {
    let params = request.params.as_deref().ok_or_else(|| {
        MezError::invalid_args(format!("{} requires a params object", request.method))
    })?;
    require_idempotency_key(params)?;
    let target = pane_target_checked_resolved(session, params)?;
    let (_window, pane) = target_or_active_pane(session, target.as_deref())?;
    let agent_session = if request.method == "agent/shell/show" {
        agent_store.enter_or_resume(pane.id.as_str())?
    } else {
        agent_store.request_exit(pane.id.as_str())?
    };
    let visible = !matches!(agent_session.visibility, AgentShellVisibility::Hidden);
    Ok(format!(
        r#"{{"agent":{},"visible":{}}}"#,
        agent_state_json_with_shell_session(session.id.as_str(), pane, agent_session),
        visible
    ))
}

/// Runs the dispatch agent shell command with store operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn dispatch_agent_shell_command_with_store(
    request: &JsonRpcRequest,
    session: &Session,
    agent_store: &mut AgentShellStore,
) -> Result<String> {
    let params = request
        .params
        .as_deref()
        .ok_or_else(|| MezError::invalid_args("agent/shell/command requires a params object"))?;
    require_idempotency_key(params)?;
    let input = json_string_field(params, "input")
        .ok_or_else(|| MezError::invalid_args("agent/shell/command requires input"))?;
    let window = session
        .active_window()
        .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
    let pane = window.active_pane();
    let visible = agent_store
        .get(pane.id.as_str())
        .is_some_and(|agent| agent.visibility == AgentShellVisibility::Visible);
    if !visible {
        return Err(MezError::invalid_state(
            "agent shell command requires a visible agent shell session",
        ));
    }
    let outcome = execute_agent_shell_command(agent_store, pane.id.as_str(), &input)?;
    Ok(agent_shell_command_response_json(
        pane.id.as_str(),
        &input,
        outcome.as_ref(),
    ))
}

/// Runs the agent shell command response json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn agent_shell_command_response_json(
    pane_id: &str,
    input: &str,
    outcome: Option<&AgentShellCommandOutcome>,
) -> String {
    match outcome {
        Some(AgentShellCommandOutcome::Display { command, body }) => format!(
            r#"{{"pane_id":"{}","input":"{}","kind":"display","command":"{}","body":"{}","turn":null}}"#,
            json_escape(pane_id),
            json_escape(input),
            json_escape(command),
            json_escape(body)
        ),
        Some(AgentShellCommandOutcome::Mutated {
            command,
            body,
            visibility,
        }) => format!(
            r#"{{"pane_id":"{}","input":"{}","kind":"mutated","command":"{}","visibility":"{}","body":"{}","turn":null}}"#,
            json_escape(pane_id),
            json_escape(input),
            json_escape(command),
            agent_shell_visibility_json_name(*visibility),
            json_escape(body)
        ),
        Some(AgentShellCommandOutcome::RequiresRuntime { command, reason }) => format!(
            r#"{{"pane_id":"{}","input":"{}","kind":"requires_runtime","command":"{}","body":"{}","turn":null}}"#,
            json_escape(pane_id),
            json_escape(input),
            json_escape(command),
            json_escape(reason)
        ),
        None => format!(
            r#"{{"pane_id":"{}","input":"{}","kind":"requires_runtime","command":"prompt","body":"live model-loop task execution requires the runtime service","turn":null}}"#,
            json_escape(pane_id),
            json_escape(input)
        ),
    }
}

/// Runs the agent shell visibility json name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn agent_shell_visibility_json_name(visibility: AgentShellVisibility) -> &'static str {
    match visibility {
        AgentShellVisibility::Visible => "visible",
        AgentShellVisibility::Hidden => "hidden",
        AgentShellVisibility::HidePendingTaskCompletion => "hide-pending-task-completion",
    }
}

/// Runs the dispatch agent task list with ledger operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn dispatch_agent_task_list_with_ledger(
    request: &JsonRpcRequest,
    session: &Session,
    turn_ledger: &AgentTurnLedger,
) -> Result<String> {
    let filter = AgentTaskListFilter::from_params(session, request.params.as_deref())?;
    let tasks = turn_ledger
        .turns()
        .iter()
        .filter(|turn| {
            filter
                .agent_id
                .as_deref()
                .is_none_or(|agent_id| turn.agent_id == agent_id)
                && filter
                    .pane_id
                    .as_deref()
                    .is_none_or(|pane_id| turn.pane_id == pane_id)
        })
        .map(agent_task_state_json)
        .collect::<Vec<_>>();
    Ok(format!(r#"{{"tasks":[{}]}}"#, tasks.join(",")))
}

/// Runs the validate agent task list params operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_agent_task_list_params(
    session: &Session,
    params: Option<&str>,
) -> Result<()> {
    let _ = AgentTaskListFilter::from_params(session, params)?;
    Ok(())
}

/// Carries Agent Task List Filter state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct AgentTaskListFilter {
    /// Stores the agent id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    agent_id: Option<String>,
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pane_id: Option<String>,
}

impl AgentTaskListFilter {
    /// Runs the from params operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn from_params(session: &Session, params: Option<&str>) -> Result<Self> {
        let Some(params) = params else {
            return Ok(Self::default());
        };
        let value = parse_json_object_value(params, "agent/task/list params")?;
        let object = value
            .as_object()
            .ok_or_else(|| MezError::invalid_args("agent/task/list params must be an object"))?;
        let target_filter = object
            .get("target")
            .map(|target| Self::from_target_value(session, target))
            .transpose()?;
        let inline_filter = Self::from_inline_fields(session, params)?;
        match (target_filter, inline_filter) {
            (Some(target), Some(inline)) if target != inline => Err(MezError::invalid_args(
                "agent/task/list target conflicts with top-level agent filters",
            )),
            (Some(target), _) => Ok(target),
            (None, Some(inline)) => Ok(inline),
            (None, None) => Ok(Self::default()),
        }
    }

    /// Runs the from target value operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn from_target_value(session: &Session, target: &serde_json::Value) -> Result<Self> {
        let object = target
            .as_object()
            .ok_or_else(|| MezError::invalid_args("agent/task/list target must be an object"))?;
        let has_agent_id = object.contains_key("agent_id");
        let has_pane_id = object.contains_key("pane_id");
        let has_session_selector = object.contains_key("session_id")
            || object.contains_key("name")
            || object.contains_key("default");
        if (has_agent_id || has_pane_id) && has_session_selector {
            return Err(MezError::invalid_args(
                "AgentTarget must not be combined with SessionTarget fields",
            ));
        }
        if has_agent_id || has_pane_id {
            return Self::from_agent_target_fields(session, target);
        }
        require_session_target_matches_value(session, target)?;
        Ok(Self::default())
    }

    /// Runs the from inline fields operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn from_inline_fields(session: &Session, params: &str) -> Result<Option<Self>> {
        let value = parse_json_object_value(params, "agent/task/list params")?;
        let object = value
            .as_object()
            .ok_or_else(|| MezError::invalid_args("agent/task/list params must be an object"))?;
        if !object.contains_key("agent_id") && !object.contains_key("pane_id") {
            return Ok(None);
        }
        Ok(Some(Self::from_agent_target_fields(session, &value)?))
    }

    /// Runs the from agent target fields operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn from_agent_target_fields(session: &Session, value: &serde_json::Value) -> Result<Self> {
        let object = value
            .as_object()
            .ok_or_else(|| MezError::invalid_args("AgentTarget must be an object"))?;
        let agent_id =
            match object.get("agent_id") {
                Some(value) => Some(value.as_str().ok_or_else(|| {
                    MezError::invalid_args("AgentTarget agent_id must be a string")
                })?),
                None => None,
            };
        let pane_id =
            match object.get("pane_id") {
                Some(value) => Some(value.as_str().ok_or_else(|| {
                    MezError::invalid_args("AgentTarget pane_id must be a string")
                })?),
                None => None,
            };
        match (agent_id, pane_id) {
            (Some(_), Some(_)) => Err(MezError::invalid_args(
                "AgentTarget must use exactly one of agent_id or pane_id",
            )),
            (Some(agent_id), None) => {
                if let Some(pane_id) = agent_id.strip_prefix("agent-") {
                    pane_by_id(session, pane_id)?;
                }
                Ok(Self {
                    agent_id: Some(agent_id.to_string()),
                    pane_id: None,
                })
            }
            (None, Some(pane_id)) => {
                pane_by_id(session, pane_id)?;
                Ok(Self {
                    agent_id: None,
                    pane_id: Some(pane_id.to_string()),
                })
            }
            (None, None) => Err(MezError::invalid_args(
                "AgentTarget must use exactly one of agent_id or pane_id",
            )),
        }
    }
}

/// Runs the agent state json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn agent_state_json(
    session_id: &str,
    window: &Window,
    pane: &crate::layout::Pane,
    visible: bool,
) -> String {
    agent_state_json_with_model_profile(session_id, window, pane, visible, "default")
}

/// Serializes an idle agent state with an explicit model profile name.
pub(super) fn agent_state_json_with_model_profile(
    session_id: &str,
    _window: &Window,
    pane: &crate::layout::Pane,
    visible: bool,
    model_profile: &str,
) -> String {
    format!(
        r#"{{"id":"agent-{}","version":1,"session_id":"{}","pane_id":"{}","status":"idle","visible":{},"conversation_id":"agent-{}","model_profile":"{}","cooperation_mode":"user-directed","read_scopes":[],"write_scopes":[],"last_turn_id":null}}"#,
        json_escape(pane.id.as_str()),
        json_escape(session_id),
        json_escape(pane.id.as_str()),
        visible,
        json_escape(pane.id.as_str()),
        json_escape(model_profile)
    )
}

/// Runs the agent state json with shell session operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn agent_state_json_with_shell_session(
    session_id: &str,
    pane: &crate::layout::Pane,
    agent_session: &AgentShellSession,
) -> String {
    agent_state_json_with_shell_session_and_model_profile(
        session_id,
        pane,
        agent_session,
        "default",
    )
}

/// Serializes a live agent shell session with an explicit model profile name.
pub(super) fn agent_state_json_with_shell_session_and_model_profile(
    session_id: &str,
    pane: &crate::layout::Pane,
    agent_session: &AgentShellSession,
    model_profile: &str,
) -> String {
    let visible = !matches!(agent_session.visibility, AgentShellVisibility::Hidden);
    let status = if agent_session.running_turn_id.is_some() {
        "running"
    } else {
        "idle"
    };
    format!(
        r#"{{"id":"agent-{}","version":1,"session_id":"{}","pane_id":"{}","status":"{}","visible":{},"conversation_id":"{}","model_profile":"{}","cooperation_mode":"user-directed","read_scopes":[],"write_scopes":[],"last_turn_id":{},"transcript_entries":{}}}"#,
        json_escape(pane.id.as_str()),
        json_escape(session_id),
        json_escape(pane.id.as_str()),
        status,
        visible,
        json_escape(&agent_session.session_id),
        json_escape(model_profile),
        json_optional_string(agent_session.running_turn_id.as_deref()),
        agent_session.transcript_entries
    )
}

/// Runs the agent task state json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn agent_task_state_json(turn: &crate::agent::AgentTurnRecord) -> String {
    let time = unix_seconds_to_rfc3339(turn.started_at_unix_seconds);
    let result_summary = if turn.state == AgentTurnState::Interrupted {
        r#""interrupted by snapshot resume; explicit user confirmation is required before retrying non-idempotent actions""#.to_string()
    } else {
        "null".to_string()
    };
    format!(
        r#"{{"id":"{}","version":1,"agent_id":"{}","state":"{}","created_at":"{}","started_at":"{}","finished_at":{},"prompt_preview":"{}","approval_ids":[],"result_summary":{},"pane_id":"{}","policy_profile":"{}","model_profile":"{}"}}"#,
        json_escape(&turn.turn_id),
        json_escape(&turn.agent_id),
        agent_turn_state_name(turn.state),
        json_escape(&time),
        json_escape(&time),
        if matches!(turn.state, AgentTurnState::Running | AgentTurnState::Queued) {
            "null".to_string()
        } else {
            format!(r#""{}""#, json_escape(&time))
        },
        json_escape(agent_turn_trigger_name(turn.trigger)),
        result_summary,
        json_escape(&turn.pane_id),
        json_escape(&turn.policy_profile),
        json_escape(&turn.model_profile)
    )
}

/// Runs the agent turn state name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn agent_turn_state_name(state: AgentTurnState) -> &'static str {
    match state {
        AgentTurnState::Queued => "queued",
        AgentTurnState::Running => "running",
        AgentTurnState::Blocked => "waiting_approval",
        AgentTurnState::Completed => "completed",
        AgentTurnState::Failed => "failed",
        AgentTurnState::Interrupted => "interrupted",
    }
}

/// Runs the agent turn trigger name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn agent_turn_trigger_name(trigger: crate::agent::AgentTurnTrigger) -> &'static str {
    match trigger {
        crate::agent::AgentTurnTrigger::UserPrompt => "user prompt",
        crate::agent::AgentTurnTrigger::LocalMessage => "local message",
        crate::agent::AgentTurnTrigger::ScheduledTask => "scheduled task",
        crate::agent::AgentTurnTrigger::SubagentEvent => "subagent event",
        crate::agent::AgentTurnTrigger::ApprovedContinuation => "approved continuation",
    }
}

/// Runs the control event audience operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn control_event_audience(
    session: &Session,
    caller_client_id: &ClientId,
) -> Result<EventAudience> {
    let client = session
        .clients()
        .iter()
        .find(|client| client.id == *caller_client_id)
        .ok_or_else(|| MezError::forbidden("unknown control client"))?;
    match client.role {
        ClientRole::Primary => Ok(EventAudience::Primary),
        ClientRole::Observer => {
            let observer = session
                .observers()
                .iter()
                .find(|observer| observer.client_id == *caller_client_id)
                .ok_or_else(|| {
                    MezError::new(
                        crate::error::MezErrorKind::NotFound,
                        "observer request not found",
                    )
                })?;
            Ok(EventAudience::ApprovedObserver {
                visible_from_event_id: observer.visible_from_event_id.unwrap_or(u64::MAX),
            })
        }
        ClientRole::PendingObserver => {
            let observer = session
                .observers()
                .iter()
                .find(|observer| observer.client_id == *caller_client_id)
                .ok_or_else(|| {
                    MezError::new(
                        crate::error::MezErrorKind::NotFound,
                        "observer request not found",
                    )
                })?;
            Ok(EventAudience::PendingObserver {
                observer_request_id: observer.id.to_string(),
            })
        }
        ClientRole::Agent => Ok(EventAudience::Agent {
            agent_id: caller_client_id.to_string(),
        }),
        ClientRole::Automation => Ok(EventAudience::Automation),
    }
}

/// Carries Event List Params state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct EventListParams {
    /// Stores the after event id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) after_event_id: Option<u64>,
    /// Stores the limit value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) limit: Option<usize>,
}

/// Runs the dispatch event list request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn dispatch_event_list_request(
    request: &JsonRpcRequest,
    session: &Session,
    caller_client_id: &ClientId,
    event_log: &EventLog,
) -> Result<String> {
    let audience = control_event_audience(session, caller_client_id)?;
    let params = parse_event_list_params(request.params.as_deref())?;
    let effective_limit = params.limit.unwrap_or(MAX_EVENT_REPLAY_RETENTION);
    let mut events = if params.after_event_id.is_some() || params.limit.is_some() {
        event_log.replay_after_for(
            &audience,
            params.after_event_id.unwrap_or(0),
            effective_limit.saturating_add(1),
        )
    } else {
        event_log.replay_for(&audience)
    };
    let truncated = params.limit.is_some_and(|limit| events.len() > limit);
    if truncated {
        events.truncate(effective_limit);
    }
    Ok(format!(
        r#"{{"events":{},"latest_event_id":{},"retained_from_event_id":{},"replay_retention":{},"truncated":{}}}"#,
        events_json(events),
        event_log.latest_event_id(),
        event_log
            .first_retained_event_id()
            .map(|event_id| event_id.to_string())
            .unwrap_or_else(|| "null".to_string()),
        event_log.retention_limit(),
        truncated
    ))
}

/// Runs the parse event list params operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_event_list_params(params: Option<&str>) -> Result<EventListParams> {
    let Some(params) = params else {
        return Ok(EventListParams {
            after_event_id: None,
            limit: None,
        });
    };
    reject_unknown_json_fields(params, "event/list params", &["after_event_id", "limit"])?;
    let value = serde_json::from_str::<serde_json::Value>(params)
        .map_err(|_| MezError::invalid_args("event/list params must be a JSON object"))?;
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args("event/list params must be a JSON object"))?;
    let after_event_id = match object.get("after_event_id") {
        Some(value) => Some(value.as_u64().ok_or_else(|| {
            MezError::invalid_args("event/list after_event_id must be a non-negative integer")
        })?),
        None => None,
    };
    let limit = match object.get("limit") {
        Some(value) => {
            let limit = value.as_u64().ok_or_else(|| {
                MezError::invalid_args("event/list limit must be a non-negative integer")
            })?;
            let limit = usize::try_from(limit)
                .map_err(|_| MezError::invalid_args("event/list limit is too large"))?;
            if limit > MAX_EVENT_REPLAY_RETENTION {
                return Err(MezError::invalid_args(format!(
                    "event/list limit must be at most {MAX_EVENT_REPLAY_RETENTION}"
                )));
            }
            Some(limit)
        }
        None => None,
    };
    Ok(EventListParams {
        after_event_id,
        limit,
    })
}

/// Runs the events json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn events_json(events: Vec<VisibleEvent>) -> String {
    let encoded = events
        .iter()
        .map(|event| {
            format!(
                r#"{{"event_id":{},"time":"{}","event_type":"{}","kind":"{}","session_id":{},"object":{},"payload":"{}"}}"#,
                event.id,
                json_escape(&event.time),
                event_kind_name(event.kind),
                event_kind_name(event.kind),
                json_optional_string(event.session_id.as_deref()),
                event_payload_object_json(&event.payload),
                json_escape(&event.payload)
            )
        })
        .collect::<Vec<_>>();
    format!("[{}]", encoded.join(","))
}

/// Runs the event payload object json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn event_payload_object_json(payload: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(payload) {
        Ok(value) if value.is_object() => value.to_string(),
        _ => format!(r#"{{"content":"{}"}}"#, json_escape(payload)),
    }
}

/// Runs the approvals json for params operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn approvals_json_for_params(
    session: &Session,
    queue: &BlockedApprovalQueue,
    params: Option<&str>,
) -> Result<String> {
    state_request_session_target_matches(session, params, "approval/list params")?;
    let state = approval_state_filter_from_params(params, "approval/list params")?;
    Ok(approvals_json_for_state(queue, state))
}

/// Runs the approvals json for state operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn approvals_json_for_state(
    queue: &BlockedApprovalQueue,
    state: Option<ApprovalListStateFilter>,
) -> String {
    let approvals = queue
        .requests()
        .filter(|approval| match state {
            Some(ApprovalListStateFilter::Matches(state)) => approval.state == state,
            Some(ApprovalListStateFilter::AlwaysEmpty) => false,
            None => true,
        })
        .map(approval_json)
        .collect::<Vec<_>>();
    format!("[{}]", approvals.join(","))
}

/// Carries Approval List State Filter state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApprovalListStateFilter {
    /// Represents the Matches case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Matches(BlockedApprovalState),
    /// Represents the Always Empty case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    AlwaysEmpty,
}

/// Runs the approval state filter from params operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn approval_state_filter_from_params(
    params: Option<&str>,
    label: &str,
) -> Result<Option<ApprovalListStateFilter>> {
    let Some(params) = params else {
        return Ok(None);
    };
    let value = parse_json_object_value(params, label)?;
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args(format!("{label} must be an object")))?;
    let Some(state) = object.get("state") else {
        return Ok(None);
    };
    if state.is_null() {
        return Ok(None);
    }
    let state = state
        .as_str()
        .ok_or_else(|| MezError::invalid_args("approval/list state must be a string or null"))?;
    match state {
        "pending" => Ok(Some(ApprovalListStateFilter::Matches(
            BlockedApprovalState::Pending,
        ))),
        "approved" => Ok(Some(ApprovalListStateFilter::Matches(
            BlockedApprovalState::Approved,
        ))),
        "disapproved" => Ok(Some(ApprovalListStateFilter::Matches(
            BlockedApprovalState::Disapproved,
        ))),
        "redirected" => Ok(Some(ApprovalListStateFilter::Matches(
            BlockedApprovalState::Redirected,
        ))),
        "cancelled" | "invalidated" => Ok(Some(ApprovalListStateFilter::AlwaysEmpty)),
        _ => Err(MezError::invalid_args(
            "approval/list state must be pending, approved, disapproved, redirected, cancelled, invalidated, or null",
        )),
    }
}

/// Runs the approval audit record operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn approval_audit_record(
    session: &Session,
    primary_client_id: &ClientId,
    approval: &BlockedApprovalRequest,
    outcome: &str,
) -> AuditRecord {
    let decision = approval
        .decision
        .map(approval_decision_name)
        .unwrap_or("pending");
    let scope = if approval.read_scopes.is_empty() && approval.write_scopes.is_empty() {
        "none".to_string()
    } else {
        format!(
            "read=[{}];write=[{}]",
            approval.read_scopes.join(","),
            approval.write_scopes.join(",")
        )
    };
    AuditRecord::approval_decision(
        session.id.as_str(),
        control_audit_actor(primary_client_id),
        &approval.id,
        &approval.requesting_agent_id,
        decision,
        scope,
        outcome,
    )
}

/// Runs the control audit actor operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn control_audit_actor(client_id: &ClientId) -> AuditActor {
    AuditActor {
        kind: "client".to_string(),
        id: client_id.to_string(),
    }
}

/// Runs the approval json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn approval_json(request: &BlockedApprovalRequest) -> String {
    format!(
        r#"{{"id":"{}","version":1,"approval_id":"{}","state":"{}","requester":{{"agent_id":"{}","pane_id":"{}","parent_agent_chain":{}}},"requesting_agent_id":"{}","pane_id":"{}","action_type":"{}","action_kind":"{}","created_at":{},"decided_at":{},"decided_by_client_id":{},"summary":"{}","action_summary":"{}","effects":{},"scope":{},"instruction":{},"decision":{},"redirect_instruction":{},"declared_effects":{},"matched_rules":{},"read_scopes":{},"write_scopes":{},"cooperation_mode":{}}}"#,
        json_escape(&request.id),
        json_escape(&request.id),
        blocked_approval_state_name(request.state),
        json_escape(&request.requesting_agent_id),
        json_escape(&request.pane_id),
        string_array_json(&request.parent_agent_chain),
        json_escape(&request.requesting_agent_id),
        json_escape(&request.pane_id),
        json_escape(&request.action_kind),
        json_escape(&request.action_kind),
        optional_rfc3339_timestamp_json(request.created_at_unix_seconds),
        optional_rfc3339_timestamp_json(request.decided_at_unix_seconds),
        json_optional_string(request.decided_by_client_id.as_deref()),
        json_escape(&request.action_summary),
        json_escape(&request.action_summary),
        approval_effects_json(request),
        approval_scope_json(request),
        json_optional_string(request.redirect_instruction.as_deref()),
        request
            .decision
            .map(approval_decision_name)
            .map(|value| format!(r#""{}""#, value))
            .unwrap_or_else(|| "null".to_string()),
        json_optional_string(request.redirect_instruction.as_deref()),
        string_array_json(&request.declared_effects),
        string_array_json(&request.matched_rules),
        string_array_json(&request.read_scopes),
        string_array_json(&request.write_scopes),
        json_optional_string(request.cooperation_mode.as_deref())
    )
}

/// Runs the optional rfc3339 timestamp json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_rfc3339_timestamp_json(timestamp: Option<u64>) -> String {
    timestamp
        .map(|seconds| format!(r#""{}""#, unix_seconds_to_rfc3339(seconds)))
        .unwrap_or_else(|| "null".to_string())
}

/// Runs the approval effects json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn approval_effects_json(request: &BlockedApprovalRequest) -> String {
    let unknown = request.declared_effects.is_empty()
        || request
            .declared_effects
            .iter()
            .any(|effect| effect == "unknown");
    let network = request
        .declared_effects
        .iter()
        .any(|effect| effect == "network");
    let credentials = request
        .declared_effects
        .iter()
        .any(|effect| effect == "credentials");
    let process_control = request
        .declared_effects
        .iter()
        .any(|effect| effect == "process_control");
    let destructive = request
        .declared_effects
        .iter()
        .any(|effect| effect == "destructive");
    let privilege_change = request
        .declared_effects
        .iter()
        .any(|effect| effect == "privilege_change");
    format!(
        r#"{{"reads":{},"writes":{},"creates":[],"deletes":[],"touches":[],"network":{},"credentials":{},"process_control":{},"destructive":{},"privilege_change":{},"unknown":{}}}"#,
        string_array_json(&request.read_scopes),
        string_array_json(&request.write_scopes),
        network,
        credentials,
        process_control,
        destructive,
        privilege_change,
        unknown
    )
}

/// Runs the approval scope json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn approval_scope_json(request: &BlockedApprovalRequest) -> String {
    format!(
        r#"{{"persistence":"project","read_scopes":{},"write_scopes":{},"matched_rules":{}}}"#,
        string_array_json(&request.read_scopes),
        string_array_json(&request.write_scopes),
        string_array_json(&request.matched_rules)
    )
}

/// Runs the parse approval decision operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_approval_decision(value: &str) -> Result<ApprovalDecision> {
    match value {
        "approve" => Ok(ApprovalDecision::Approve),
        "disapprove" => Ok(ApprovalDecision::Disapprove),
        "redirect" => Ok(ApprovalDecision::Redirect),
        _ => Err(MezError::invalid_args(
            "approval decision must be approve, disapprove, or redirect",
        )),
    }
}

/// Carries Approval Decision Scope Persistence state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApprovalDecisionScopePersistence {
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

/// Parse the optional `approval/decide` scope object.
///
/// The baseline protocol allows callers to choose whether an approval applies
/// once, for the current session, for the current project, or globally. The full
/// scope object carries optional narrowing fields that are not yet semantically
/// consumed by every approval path, but the request shape is validated here so
/// malformed scopes do not pass silently through specialized approval dispatch.
pub(crate) fn approval_decide_scope_persistence(
    params: &str,
) -> Result<Option<ApprovalDecisionScopePersistence>> {
    let Some(raw_scope) = json_raw_field(params, "scope") else {
        return Ok(None);
    };
    if raw_scope.trim() == "null" {
        return Ok(None);
    }
    let scope = serde_json::from_str::<serde_json::Value>(&raw_scope).map_err(|error| {
        MezError::invalid_args(format!("approval/decide scope is invalid JSON: {error}"))
    })?;
    let object = scope
        .as_object()
        .ok_or_else(|| MezError::invalid_args("approval/decide scope must be an object or null"))?;
    for key in object.keys() {
        if !matches!(
            key.as_str(),
            "persistence"
                | "command_prefix"
                | "exact_sha256"
                | "working_directory"
                | "project_root"
                | "external_integration"
        ) {
            return Err(MezError::invalid_args(format!(
                "approval/decide scope contains unknown field `{key}`"
            )));
        }
    }
    let persistence = object
        .get("persistence")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| MezError::invalid_args("approval/decide scope requires persistence"))?;
    validate_approval_scope_fields(object)?;
    match persistence {
        "once" => Ok(Some(ApprovalDecisionScopePersistence::Once)),
        "session" => Ok(Some(ApprovalDecisionScopePersistence::Session)),
        "project" => Ok(Some(ApprovalDecisionScopePersistence::Project)),
        "global" => Ok(Some(ApprovalDecisionScopePersistence::Global)),
        _ => Err(MezError::invalid_args(
            "approval/decide scope persistence must be once, session, project, or global",
        )),
    }
}

/// Runs the validate approval scope fields operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validate_approval_scope_fields(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<()> {
    if let Some(command_prefix) = object.get("command_prefix") {
        let tokens = command_prefix.as_array().ok_or_else(|| {
            MezError::invalid_args("approval/decide scope command_prefix must be an array")
        })?;
        if tokens.is_empty() {
            return Err(MezError::invalid_args(
                "approval/decide scope command_prefix must not be empty",
            ));
        }
        for token in tokens {
            if token.as_str().is_none_or(|token| token.trim().is_empty()) {
                return Err(MezError::invalid_args(
                    "approval/decide scope command_prefix entries must be non-empty strings",
                ));
            }
        }
    }

    if let Some(exact_sha256) = object.get("exact_sha256") {
        let digest = non_empty_scope_string(exact_sha256, "exact_sha256")?;
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(MezError::invalid_args(
                "approval/decide scope exact_sha256 must be a 64-character hexadecimal digest",
            ));
        }
    }

    for field in ["working_directory", "project_root", "external_integration"] {
        if let Some(value) = object.get(field) {
            non_empty_scope_string(value, field)?;
        }
    }

    Ok(())
}

/// Runs the non empty scope string operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn non_empty_scope_string<'a>(value: &'a serde_json::Value, field: &str) -> Result<&'a str> {
    value
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            MezError::invalid_args(format!(
                "approval/decide scope {field} must be a non-empty string"
            ))
        })
}

/// Runs the blocked approval state name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn blocked_approval_state_name(state: BlockedApprovalState) -> &'static str {
    match state {
        BlockedApprovalState::Pending => "pending",
        BlockedApprovalState::Approved => "approved",
        BlockedApprovalState::Disapproved => "disapproved",
        BlockedApprovalState::Redirected => "redirected",
    }
}

/// Runs the approval decision name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn approval_decision_name(decision: ApprovalDecision) -> &'static str {
    match decision {
        ApprovalDecision::Approve => "approve",
        ApprovalDecision::Disapprove => "disapprove",
        ApprovalDecision::Redirect => "redirect",
    }
}

/// Runs the parse trust decision operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_trust_decision(value: &str) -> Result<TrustDecision> {
    match value {
        "trust" | "trusted" => Ok(TrustDecision::Trusted),
        "reject" | "rejected" => Ok(TrustDecision::Rejected),
        _ => Err(MezError::invalid_args(
            "project trust decision must be trust or reject",
        )),
    }
}

/// Runs the project trust state filter from params operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn project_trust_state_filter_from_params(
    params: Option<&str>,
    label: &str,
) -> Result<Option<TrustDecision>> {
    let Some(params) = params else {
        return Ok(None);
    };
    let value = parse_json_object_value(params, label)?;
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args(format!("{label} must be an object")))?;
    let Some(state) = object.get("state") else {
        return Ok(None);
    };
    if state.is_null() {
        return Ok(None);
    }
    let state = state.as_str().ok_or_else(|| {
        MezError::invalid_args("project/trust/list state must be a string or null")
    })?;
    match state {
        "pending" => Ok(Some(TrustDecision::Pending)),
        "trusted" => Ok(Some(TrustDecision::Trusted)),
        "rejected" => Ok(Some(TrustDecision::Rejected)),
        "revoked" => Ok(Some(TrustDecision::Revoked)),
        _ => Err(MezError::invalid_args(
            "project/trust/list state must be pending, trusted, rejected, revoked, or null",
        )),
    }
}

/// Runs the project trust json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn project_trust_json(record: &ProjectTrustRecord) -> String {
    let git_marker_path = record
        .git_marker_path
        .as_ref()
        .map(|path| path.to_string_lossy().to_string());
    let decided_at = if record.trusted_at_unix_seconds == 0 {
        "null".to_string()
    } else {
        format!(
            r#""{}""#,
            unix_seconds_to_rfc3339(record.trusted_at_unix_seconds)
        )
    };
    format!(
        r#"{{"id":"{}","version":1,"project_root":"{}","state":"{}","git_marker_path":{},"trusted_at":{},"rejected_at":{},"revoked_at":{},"decided_by_client_id":{},"trust_policy_version":{},"configuration_schema_version":{},"overlay_files":[],"capability_expansion_summary":[],"diagnostics":[],"trusted_at_unix_seconds":{},"vcs_remote":{}}}"#,
        json_escape(&record.project_root.to_string_lossy()),
        json_escape(&record.project_root.to_string_lossy()),
        trust_decision_name(record.state),
        json_optional_string(git_marker_path.as_deref()),
        if matches!(record.state, TrustDecision::Trusted) {
            decided_at.as_str()
        } else {
            "null"
        },
        if matches!(record.state, TrustDecision::Rejected) {
            decided_at.as_str()
        } else {
            "null"
        },
        if matches!(record.state, TrustDecision::Revoked) {
            decided_at.as_str()
        } else {
            "null"
        },
        json_optional_string(record.decided_by_client_id.as_deref()),
        record.trust_policy_version,
        record.configuration_schema_version,
        record.trusted_at_unix_seconds,
        json_optional_string(record.vcs_remote.as_deref())
    )
}

/// Runs the trust decision name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn trust_decision_name(decision: TrustDecision) -> &'static str {
    match decision {
        TrustDecision::Pending => "pending",
        TrustDecision::Trusted => "trusted",
        TrustDecision::Rejected => "rejected",
        TrustDecision::Revoked => "revoked",
    }
}

/// Runs the event kind name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn event_kind_name(kind: EventKind) -> &'static str {
    match kind {
        EventKind::ClientAttached => "client_attached",
        EventKind::ClientDetached => "client_detached",
        EventKind::ObserverRequested => "observer_requested",
        EventKind::ObserverDecided => "observer_decided",
        EventKind::WindowChanged => "window_changed",
        EventKind::PaneChanged => "pane_changed",
        EventKind::AgentStatus => "agent_status",
        EventKind::Message => "message",
        EventKind::ConfigChanged => "config_changed",
        EventKind::SnapshotChanged => "snapshot_changed",
        EventKind::ApprovalChanged => "approval_changed",
        EventKind::McpServerChanged => "mcp_server_changed",
        EventKind::HookFailed => "hook_failed",
        EventKind::Diagnostic => "diagnostic",
    }
}

/// Runs the observer json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn observer_json(session: &Session, observer_id: &str) -> Result<String> {
    let observer = session
        .observers()
        .iter()
        .find(|observer| observer.id.as_str() == observer_id)
        .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "observer not found"))?;
    Ok(observer_json_by_ref(observer))
}

/// Runs the observer json by ref operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn observer_json_by_ref(observer: &crate::session::ObserverRequest) -> String {
    let visible_from_event_id = observer
        .visible_from_event_id
        .map(|id| id.to_string())
        .unwrap_or_else(|| "null".to_string());
    let visible_from_time = optional_rfc3339_timestamp_json(observer.visible_from_unix_seconds);
    format!(
        r#"{{"id":"{}","version":1,"observer_request_id":"{}","client_id":"{}","state":"{}","requested_at":{},"decided_at":{},"decided_by_client_id":{},"visible_from_event_id":{},"visible_from_time":{},"descriptor":{{"name":"{}","interactive":{},"terminal":{}}},"reason":{}}}"#,
        json_escape(&observer.id.to_string()),
        json_escape(&observer.id.to_string()),
        json_escape(&observer.client_id.to_string()),
        observer_state_name(observer.state),
        optional_rfc3339_timestamp_json(observer.requested_at_unix_seconds),
        optional_rfc3339_timestamp_json(observer.decided_at_unix_seconds),
        json_optional_string(observer.decided_by_client_id.as_deref()),
        visible_from_event_id,
        visible_from_time,
        json_escape(&observer.descriptor_name),
        observer.descriptor_interactive,
        generic_client_terminal_descriptor_json(observer.descriptor_terminal.as_ref()),
        json_optional_string(observer.reason.as_deref())
    )
}

/// Runs the mcp servers json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mcp_servers_json(registry: &McpRegistry) -> String {
    let servers = registry
        .list_servers()
        .iter()
        .map(|server| {
            let blacklisted = matches!(server.status, McpServerStatus::Blacklisted)
                || server.blacklist_reason.is_some();
            format!(
                r#"{{"id":"{}","version":1,"server_id":"{}","name":"{}","state":"{}","status":"{}","configured":true,"blacklisted":{},"transport":{{"kind":"{}"}},"kind":"{}","tools":{},"last_checked_at":{},"diagnostics":{},"enabled":{},"blacklist_reason":{},"retryable":{},"external_purpose":"{}"}}"#,
                json_escape(&server.configured.id),
                json_escape(&server.configured.id),
                json_escape(&server.configured.name),
                mcp_server_state_name(server),
                mcp_status_name(server.status),
                blacklisted,
                mcp_kind_name(server.configured.kind),
                mcp_kind_name(server.configured.kind),
                mcp_server_tool_ids_json(server),
                optional_rfc3339_timestamp_json(server.last_checked_at_unix_seconds),
                mcp_server_diagnostics_json(server),
                server.configured.enabled,
                json_optional_string(server.blacklist_reason.as_deref()),
                server.configured.enabled
                    && matches!(
                        server.status,
                        McpServerStatus::Unavailable
                            | McpServerStatus::Blacklisted
                            | McpServerStatus::Failed
                    ),
                json_escape(&server.configured.external_capability.purpose)
            )
        })
        .collect::<Vec<_>>();
    format!("[{}]", servers.join(","))
}

/// Runs the mcp tools json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mcp_tools_json(registry: &McpRegistry) -> String {
    let tools = registry
        .list_servers()
        .iter()
        .flat_map(|server| {
            server.tools.iter().map(|tool| {
                format!(
                    r#"{{"id":"{}","version":1,"server_id":"{}","name":"{}","available":{},"blacklisted":{},"permission_required":{},"effects":{},"description":"{}","input_schema":{},"approval":"{}"}}"#,
                    json_escape(&mcp_tool_id(tool)),
                    json_escape(&tool.server_id),
                    json_escape(&tool.name),
                    tool.available,
                    tool.blacklisted,
                    tool.permission_required,
                    mcp_tool_effects_json(tool.effects),
                    json_escape(&tool.description),
                    tool.input_schema_json,
                    mcp_approval_name(tool.approval)
                )
            })
        })
        .collect::<Vec<_>>();
    format!("[{}]", tools.join(","))
}

/// Runs the mcp tool effects json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn mcp_tool_effects_json(effects: crate::mcp::McpToolEffects) -> String {
    format!(
        r#"{{"reads_filesystem":{},"mutates_filesystem":{},"executes_processes":{},"accesses_credentials":{},"uses_network":{},"has_side_effects":{}}}"#,
        effects.reads_filesystem,
        effects.mutates_filesystem,
        effects.executes_processes,
        effects.accesses_credentials,
        effects.uses_network,
        effects.has_side_effects
    )
}

/// Runs the mcp server tool ids json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mcp_server_tool_ids_json(server: &crate::mcp::McpServerState) -> String {
    let ids = server
        .tools
        .iter()
        .map(|tool| format!(r#""{}""#, json_escape(&mcp_tool_id(tool))))
        .collect::<Vec<_>>();
    format!("[{}]", ids.join(","))
}

/// Runs the mcp tool id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mcp_tool_id(tool: &crate::mcp::McpToolState) -> String {
    format!("{}:{}", tool.server_id, tool.name)
}

/// Runs the mcp server diagnostics json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mcp_server_diagnostics_json(server: &crate::mcp::McpServerState) -> String {
    match server.blacklist_reason.as_deref() {
        Some(reason) => format!(
            r#"[{{"severity":"warning","code":"mcp_blacklisted","message":"{}"}}]"#,
            json_escape(reason)
        ),
        None => "[]".to_string(),
    }
}

/// Runs the mcp kind name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mcp_kind_name(kind: McpServerKind) -> &'static str {
    match kind {
        McpServerKind::Stdio => "stdio",
        McpServerKind::Http => "streamable_http",
    }
}

/// Runs the mcp server state name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mcp_server_state_name(server: &crate::mcp::McpServerState) -> &'static str {
    if !server.configured.enabled {
        return "disabled";
    }
    match server.status {
        McpServerStatus::Configured => "enabled",
        McpServerStatus::Starting => "starting",
        McpServerStatus::Available => "available",
        McpServerStatus::Unavailable => "unavailable",
        McpServerStatus::Blacklisted => "blacklisted",
        McpServerStatus::Failed => "failed",
    }
}

/// Runs the mcp status name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mcp_status_name(status: McpServerStatus) -> &'static str {
    match status {
        McpServerStatus::Configured => "configured",
        McpServerStatus::Starting => "starting",
        McpServerStatus::Available => "available",
        McpServerStatus::Unavailable => "unavailable",
        McpServerStatus::Blacklisted => "blacklisted",
        McpServerStatus::Failed => "failed",
    }
}

/// Runs the mcp approval name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mcp_approval_name(approval: crate::mcp::McpApprovalSetting) -> &'static str {
    match approval {
        crate::mcp::McpApprovalSetting::Inherit => "inherit",
        crate::mcp::McpApprovalSetting::Prompt => "prompt",
        crate::mcp::McpApprovalSetting::Allow => "allow",
        crate::mcp::McpApprovalSetting::Deny => "deny",
    }
}

/// Runs the snapshots json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn snapshots_json(snapshots: &[SnapshotState]) -> String {
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
pub(super) fn snapshot_state_json(snapshot: &SnapshotState) -> String {
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
pub(super) fn resume_plan_json(plan: &SnapshotResumePlan) -> String {
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
pub(super) fn snapshot_kind_name(kind: SnapshotKind) -> &'static str {
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
pub(super) fn json_optional_string(value: Option<&str>) -> String {
    value
        .map(|value| format!(r#""{}""#, json_escape(value)))
        .unwrap_or_else(|| "null".to_string())
}

/// Runs the string array json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn string_array_json(values: &[String]) -> String {
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
pub(super) fn client_role_name(role: ClientRole) -> &'static str {
    match role {
        ClientRole::Primary => "primary",
        ClientRole::PendingObserver => "pending_observer",
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
pub(super) fn client_state_name(state: ClientState) -> &'static str {
    match state {
        ClientState::Attached => "attached",
        ClientState::Pending => "pending",
        ClientState::Detached => "detached",
        ClientState::Revoked => "revoked",
        ClientState::Failed => "failed",
    }
}

/// Runs the observer state name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn observer_state_name(state: ObserverDecisionState) -> &'static str {
    match state {
        ObserverDecisionState::Pending => "pending",
        ObserverDecisionState::Approved => "approved",
        ObserverDecisionState::Rejected => "rejected",
        ObserverDecisionState::Revoked => "revoked",
    }
}

/// Runs the observer state filter from params operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn observer_state_filter_from_params(
    params: Option<&str>,
    label: &str,
) -> Result<Option<ObserverDecisionState>> {
    let Some(params) = params else {
        return Ok(None);
    };
    let value = parse_json_object_value(params, label)?;
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args(format!("{label} must be an object")))?;
    let Some(state) = object.get("state") else {
        return Ok(None);
    };
    if state.is_null() {
        return Ok(None);
    }
    let state = state
        .as_str()
        .ok_or_else(|| MezError::invalid_args("observer/list state must be a string or null"))?;
    match state {
        "pending" => Ok(Some(ObserverDecisionState::Pending)),
        "approved" => Ok(Some(ObserverDecisionState::Approved)),
        "rejected" => Ok(Some(ObserverDecisionState::Rejected)),
        "revoked" => Ok(Some(ObserverDecisionState::Revoked)),
        _ => Err(MezError::invalid_args(
            "observer/list state must be pending, approved, rejected, revoked, or null",
        )),
    }
}
