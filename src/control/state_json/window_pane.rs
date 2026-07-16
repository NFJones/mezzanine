//! Window, pane, layout, and frame-read state projection.

use super::approvals::optional_rfc3339_timestamp_json;
use super::*;

/// Runs the window state json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn window_state_json(session: &Session, window: &Window) -> String {
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
pub(in crate::control) fn window_panes_json(session: &Session, window: &Window) -> String {
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
pub(in crate::control) fn pane_state_json(
    session: &Session,
    window: &Window,
    pane: &mez_mux::layout::Pane,
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
pub(in crate::control) fn pane_state_json_with_capture(
    session_id: &str,
    window: &Window,
    pane: &mez_mux::layout::Pane,
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
            readiness_state: source.readiness_state.as_deref().unwrap_or("unknown"),
            alternate_screen_active: source.alternate_screen_active,
        },
    )
}

/// Runtime-backed process and terminal fields included in pane state JSON.
pub(super) struct PaneRuntimeStateJsonFields<'a> {
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
pub(super) fn pane_state_json_with_runtime(
    session_id: &str,
    window: &Window,
    pane: &mez_mux::layout::Pane,
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
pub(super) fn layout_root_json(window: &Window) -> String {
    let geometries = window.pane_geometries();
    layout_node_json(window.layout_root(), window, &geometries)
}

/// Runs the layout node json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn layout_node_json(
    node: &LayoutNode,
    window: &Window,
    geometries: &[mez_mux::layout::PaneGeometry],
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
                .unwrap_or(mez_mux::layout::PaneGeometry {
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
pub(super) fn layout_pane_geometry_json(geometry: &mez_mux::layout::PaneGeometry) -> String {
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
pub(in crate::control) fn frame_read_json(
    session: &Session,
    params: Option<&str>,
) -> Result<String> {
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
pub(super) fn frame_read_fields(
    session: &Session,
    window: &Window,
    pane: &mez_mux::layout::Pane,
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
pub(super) fn frame_read_fields_json(fields: &[(&str, String)]) -> String {
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
pub(in crate::control) fn frame_read_target<'a>(
    session: &'a Session,
    params: Option<&str>,
) -> Result<(&'a Window, &'a mez_mux::layout::Pane)> {
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
