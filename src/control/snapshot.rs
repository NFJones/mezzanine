//! Control Snapshot implementation.
//!
//! This module owns the control snapshot boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    JsonRpcRequest, MezError, Result, Session, SnapshotRepository, json_bool_field,
    json_object_field, json_string_field, nullable_state_request_session_target_matches,
    require_idempotency_key, resume_plan_json, snapshot_state_json, snapshots_json,
    string_array_json, validate_control_method_params_schema,
};

// Snapshot control methods.

/// Runs the dispatch snapshot request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn dispatch_snapshot_request(
    request: &JsonRpcRequest,
    session: &Session,
    snapshots: &SnapshotRepository,
) -> Result<String> {
    dispatch_snapshot_request_with_captures(request, session, snapshots, &[])
}

/// Runs the dispatch snapshot request with captures operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn dispatch_snapshot_request_with_captures(
    request: &JsonRpcRequest,
    session: &Session,
    snapshots: &SnapshotRepository,
    pane_captures: &[crate::storage::snapshot::SnapshotPaneCapture],
) -> Result<String> {
    dispatch_snapshot_request_with_captures_and_config_layers(
        request,
        session,
        snapshots,
        pane_captures,
        &[],
    )
}

/// Runs the dispatch snapshot request with captures and config layers operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn dispatch_snapshot_request_with_captures_and_config_layers(
    request: &JsonRpcRequest,
    session: &Session,
    snapshots: &SnapshotRepository,
    pane_captures: &[crate::storage::snapshot::SnapshotPaneCapture],
    active_config_layers: &[crate::storage::snapshot::SnapshotConfigLayerMetadata],
) -> Result<String> {
    dispatch_snapshot_request_with_captures_and_config_layers_and_frame_state(
        request,
        session,
        snapshots,
        pane_captures,
        active_config_layers,
        &crate::storage::snapshot::SnapshotFrameState::default(),
    )
}

/// Runs the dispatch snapshot request with captures and config layers and frame state operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn dispatch_snapshot_request_with_captures_and_config_layers_and_frame_state(
    request: &JsonRpcRequest,
    session: &Session,
    snapshots: &SnapshotRepository,
    pane_captures: &[crate::storage::snapshot::SnapshotPaneCapture],
    active_config_layers: &[crate::storage::snapshot::SnapshotConfigLayerMetadata],
    frame_state: &crate::storage::snapshot::SnapshotFrameState,
) -> Result<String> {
    dispatch_snapshot_request_with_context(
        request,
        session,
        snapshots,
        crate::storage::snapshot::SnapshotCreationContext::new(
            pane_captures,
            active_config_layers,
            frame_state,
            &[],
        ),
    )
}

/// Runs the dispatch snapshot request with context operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn dispatch_snapshot_request_with_context(
    request: &JsonRpcRequest,
    session: &Session,
    snapshots: &SnapshotRepository,
    context: crate::storage::snapshot::SnapshotCreationContext<'_>,
) -> Result<String> {
    validate_control_method_params_schema(request)?;
    match request.method.as_str() {
        "snapshot/list" => {
            nullable_state_request_session_target_matches(
                session,
                request.params.as_deref(),
                "snapshot/list params",
            )?;
            let states = snapshots
                .list()?
                .into_iter()
                .filter(|snapshot| snapshot.session_id == session.id.to_string())
                .collect::<Vec<_>>();
            Ok(format!(r#"{{"snapshots":{}}}"#, snapshots_json(&states)))
        }
        "snapshot/create" => {
            let params = request.params.as_deref().ok_or_else(|| {
                MezError::invalid_args("snapshot/create requires a params object")
            })?;
            require_idempotency_key(params)?;
            let target = json_object_field(params, "target")
                .ok_or_else(|| MezError::invalid_args("snapshot/create requires target"))?;
            require_session_target_matches(&target, session)?;
            let idempotency_key =
                json_string_field(params, "idempotency_key").ok_or_else(|| {
                    MezError::invalid_args("snapshot/create requires idempotency_key")
                })?;
            let snapshot_id = snapshot_id_for_idempotency_key(session, &idempotency_key);
            let name = json_string_field(params, "name");
            let snapshot =
                snapshots.create_from_session_with_context(&snapshot_id, name, session, context)?;
            Ok(format!(
                r#"{{"snapshot":{}}}"#,
                snapshot_state_json(&snapshot)
            ))
        }
        "snapshot/resume" => {
            let params = request.params.as_deref().ok_or_else(|| {
                MezError::invalid_args("snapshot/resume requires a params object")
            })?;
            require_idempotency_key(params)?;
            let snapshot_id = json_string_field(params, "snapshot_id")
                .ok_or_else(|| MezError::invalid_args("snapshot/resume requires snapshot_id"))?;
            let plan = snapshots.resume_plan(&snapshot_id)?;
            Ok(format!(
                r#"{{"session":null,"resumed":false,"resume_plan":{},"limitations":{}}}"#,
                resume_plan_json(&plan),
                string_array_json(&plan.limitations)
            ))
        }
        "snapshot/delete" => {
            let params = request.params.as_deref().ok_or_else(|| {
                MezError::invalid_args("snapshot/delete requires a params object")
            })?;
            require_idempotency_key(params)?;
            let snapshot_id = json_string_field(params, "snapshot_id")
                .ok_or_else(|| MezError::invalid_args("snapshot/delete requires snapshot_id"))?;
            Ok(format!(
                r#"{{"deleted":{}}}"#,
                snapshots.delete(&snapshot_id)?
            ))
        }
        _ => Err(MezError::not_implemented(format!(
            "unknown snapshot control method `{}`",
            request.method
        ))),
    }
}

/// Runs the dispatch snapshot request with context async operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn dispatch_snapshot_request_with_context_async(
    request: &JsonRpcRequest,
    session: &Session,
    snapshots: &SnapshotRepository,
    context: crate::storage::snapshot::SnapshotCreationContext<'_>,
) -> Result<String> {
    validate_control_method_params_schema(request)?;
    match request.method.as_str() {
        "snapshot/list" => {
            nullable_state_request_session_target_matches(
                session,
                request.params.as_deref(),
                "snapshot/list params",
            )?;
            let states = snapshots
                .list_async()
                .await?
                .into_iter()
                .filter(|snapshot| snapshot.session_id == session.id.to_string())
                .collect::<Vec<_>>();
            Ok(format!(r#"{{"snapshots":{}}}"#, snapshots_json(&states)))
        }
        "snapshot/create" => {
            let params = request.params.as_deref().ok_or_else(|| {
                MezError::invalid_args("snapshot/create requires a params object")
            })?;
            require_idempotency_key(params)?;
            let target = json_object_field(params, "target")
                .ok_or_else(|| MezError::invalid_args("snapshot/create requires target"))?;
            require_session_target_matches(&target, session)?;
            let idempotency_key =
                json_string_field(params, "idempotency_key").ok_or_else(|| {
                    MezError::invalid_args("snapshot/create requires idempotency_key")
                })?;
            let snapshot_id = snapshot_id_for_idempotency_key(session, &idempotency_key);
            let name = json_string_field(params, "name");
            let snapshot = snapshots
                .create_from_session_with_context_async(&snapshot_id, name, session, context)
                .await?;
            Ok(format!(
                r#"{{"snapshot":{}}}"#,
                snapshot_state_json(&snapshot)
            ))
        }
        "snapshot/resume" => {
            let params = request.params.as_deref().ok_or_else(|| {
                MezError::invalid_args("snapshot/resume requires a params object")
            })?;
            require_idempotency_key(params)?;
            let snapshot_id = json_string_field(params, "snapshot_id")
                .ok_or_else(|| MezError::invalid_args("snapshot/resume requires snapshot_id"))?;
            let payload = snapshots.inspect_payload_async(&snapshot_id).await?;
            let plan = payload.resume_plan();
            Ok(format!(
                r#"{{"session":null,"resumed":false,"resume_plan":{},"limitations":{}}}"#,
                resume_plan_json(&plan),
                string_array_json(&plan.limitations)
            ))
        }
        "snapshot/delete" => {
            let params = request.params.as_deref().ok_or_else(|| {
                MezError::invalid_args("snapshot/delete requires a params object")
            })?;
            require_idempotency_key(params)?;
            let snapshot_id = json_string_field(params, "snapshot_id")
                .ok_or_else(|| MezError::invalid_args("snapshot/delete requires snapshot_id"))?;
            Ok(format!(
                r#"{{"deleted":{}}}"#,
                snapshots.delete_async(&snapshot_id).await?
            ))
        }
        _ => Err(MezError::not_implemented(format!(
            "unknown snapshot control method `{}`",
            request.method
        ))),
    }
}

/// Runs the require session target matches operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn require_session_target_matches(target: &str, session: &Session) -> Result<()> {
    let session_id = json_string_field(target, "session_id");
    let name = json_string_field(target, "name");
    let default = json_bool_field(target, "default").unwrap_or(false);
    let selector_count =
        usize::from(session_id.is_some()) + usize::from(name.is_some()) + usize::from(default);

    if selector_count != 1 {
        return Err(MezError::invalid_args(
            "SessionTarget must use exactly one of session_id, name, or default=true",
        ));
    }
    if let Some(session_id) = session_id {
        if session_id == session.id.to_string() {
            return Ok(());
        }
        return Err(MezError::new(
            crate::error::MezErrorKind::NotFound,
            "session target not found",
        ));
    }
    if let Some(name) = name {
        if name == session.name {
            return Ok(());
        }
        return Err(MezError::new(
            crate::error::MezErrorKind::NotFound,
            "session target not found",
        ));
    }
    Ok(())
}

/// Runs the snapshot id for idempotency key operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn snapshot_id_for_idempotency_key(session: &Session, idempotency_key: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in session
        .id
        .to_string()
        .bytes()
        .chain([0])
        .chain(idempotency_key.bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("snap-{hash:016x}")
}
