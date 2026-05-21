//! Control Config implementation.
//!
//! This module owns the control config boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AuditRecord, ClientId, ConfigDiagnostic, ConfigFormat, ConfigLayer, ConfigMutation,
    ConfigMutationOperation, ConfigMutationPlan, ConfigMutationValue, ConfigScope,
    ConfigValidation, ControlIdempotencyCache, JsonRpcRequest, MezError, PathBuf,
    ProjectTrustStore, RequestedRole, Result, Session, TrustDecision, client_descriptor_from_json,
    client_json, compose_effective_config, control_audit_actor,
    ensure_client_descriptor_role_matches, error_code, field_value, json_escape, json_null_field,
    json_object_field, json_optional_string, json_raw_field, json_rpc_error, json_rpc_success,
    json_string_array_field, json_string_field, mezzanine_error_code, parse_json_rpc_request,
    parse_trust_decision, persist_config_mutation, project_trust_json,
    project_trust_state_filter_from_params, reject_unknown_json_fields, require_idempotency_key,
    validate_config_file, validate_control_method_params_schema,
};
use crate::session::ClientTerminalDescriptor;

// Project-trust and configuration control methods.

/// Runs the client terminal descriptor from control operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn client_terminal_descriptor_from_control(
    terminal: Option<&super::TerminalDescriptor>,
) -> Option<ClientTerminalDescriptor> {
    terminal.map(|terminal| ClientTerminalDescriptor {
        columns: terminal.columns,
        rows: terminal.rows,
        term: terminal.term.clone(),
        features: terminal.features.clone(),
    })
}

/// Runs the dispatch session attach request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn dispatch_session_attach_request(body: &str, session: &mut Session) -> String {
    let request = match parse_json_rpc_request(body) {
        Ok(request) => request,
        Err(error) => {
            return json_rpc_error("null", -32600, error.message(), "invalid_request");
        }
    };
    if request.method != "session/attach" {
        return json_rpc_error(
            &request.id,
            -32601,
            "session attach dispatcher accepts only session/attach",
            "not_implemented",
        );
    }
    let result = (|| {
        let params = request
            .params
            .as_deref()
            .ok_or_else(|| MezError::invalid_args("session/attach requires a params object"))?;
        require_idempotency_key(params)?;
        let role = json_string_field(params, "role").unwrap_or_else(|| "primary".to_string());
        let client_descriptor = json_object_field(params, "client")
            .as_deref()
            .map(client_descriptor_from_json)
            .transpose()?
            .ok_or_else(|| MezError::invalid_args("session/attach requires client"))?;
        match role.as_str() {
            "primary" => {
                ensure_client_descriptor_role_matches(
                    &client_descriptor,
                    RequestedRole::Primary,
                    "session/attach client descriptor",
                )?;
                let client_id = session.attach_primary_with_terminal(
                    &client_descriptor.name,
                    client_descriptor.interactive,
                    client_terminal_descriptor_from_control(client_descriptor.terminal.as_ref()),
                )?;
                let client = session
                    .clients()
                    .iter()
                    .find(|client| client.id == client_id)
                    .ok_or_else(|| {
                        MezError::new(crate::error::MezErrorKind::NotFound, "client not found")
                    })?;
                Ok(format!(
                    r#"{{"client":{},"approval_pending":false}}"#,
                    client_json(session, client)
                ))
            }
            "observer" => {
                ensure_client_descriptor_role_matches(
                    &client_descriptor,
                    RequestedRole::Observer,
                    "session/attach client descriptor",
                )?;
                let (client_id, _observer_id) = session.request_observer_with_terminal(
                    &client_descriptor.name,
                    client_terminal_descriptor_from_control(client_descriptor.terminal.as_ref()),
                );
                let client = session
                    .clients()
                    .iter()
                    .find(|client| client.id == client_id)
                    .ok_or_else(|| {
                        MezError::new(crate::error::MezErrorKind::NotFound, "client not found")
                    })?;
                Ok(format!(
                    r#"{{"client":{},"approval_pending":true}}"#,
                    client_json(session, client)
                ))
            }
            _ => Err(MezError::invalid_args(
                "session/attach role must be primary or observer",
            )),
        }
    })();
    match result {
        Ok(result) => json_rpc_success(&request.id, &result),
        Err(error) => json_rpc_error(
            &request.id,
            error_code(error.kind()),
            error.message(),
            mezzanine_error_code(error.kind()),
        ),
    }
}

/// Runs the dispatch project trust request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn dispatch_project_trust_request(body: &str, trust_store: &mut ProjectTrustStore) -> String {
    let request = match parse_json_rpc_request(body) {
        Ok(request) => request,
        Err(error) => {
            return json_rpc_error("null", -32600, error.message(), "invalid_request");
        }
    };
    let result = validate_control_method_params_schema(&request).and_then(|()| {
        match request.method.as_str() {
            "project/trust/list" => {
                let state = project_trust_state_filter_from_params(
                    request.params.as_deref(),
                    "project/trust/list params",
                )?;
                Ok(format!(
                    r#"{{"projects":[{}]}}"#,
                    trust_store
                        .records()
                        .filter(|record| state.is_none_or(|state| record.state == state))
                        .map(project_trust_json)
                        .collect::<Vec<_>>()
                        .join(",")
                ))
            }
            "project/trust/inspect" => {
                let params = request.params.as_deref().ok_or_else(|| {
                    MezError::invalid_args("project/trust/inspect requires a params object")
                });
                params.and_then(|params| {
                    let root = json_string_field(params, "project_root").ok_or_else(|| {
                        MezError::invalid_args("project/trust/inspect requires project_root")
                    })?;
                    let record = trust_store
                        .get(std::path::Path::new(&root))
                        .ok_or_else(|| {
                            MezError::new(crate::error::MezErrorKind::NotFound, "project not found")
                        })?;
                    Ok(format!(r#"{{"project":{}}}"#, project_trust_json(record)))
                })
            }
            "project/trust/decide" => {
                let params = request.params.as_deref().ok_or_else(|| {
                    MezError::invalid_args("project/trust/decide requires a params object")
                });
                params.and_then(|params| {
                    require_idempotency_key(params)?;
                    let root = json_string_field(params, "project_root").ok_or_else(|| {
                        MezError::invalid_args("project/trust/decide requires project_root")
                    })?;
                    let decision = json_string_field(params, "decision")
                        .as_deref()
                        .map(parse_trust_decision)
                        .transpose()?
                        .ok_or_else(|| {
                            MezError::invalid_args("project/trust/decide requires decision")
                        })?;
                    trust_store.decide(std::path::PathBuf::from(&root), decision, None)?;
                    let record = trust_store
                        .get(std::path::Path::new(&root))
                        .expect("record inserted before lookup");
                    Ok(format!(
                        r#"{{"project":{},"diagnostics":[]}}"#,
                        project_trust_json(record)
                    ))
                })
            }
            "project/trust/revoke" => {
                let params = request.params.as_deref().ok_or_else(|| {
                    MezError::invalid_args("project/trust/revoke requires a params object")
                });
                params.and_then(|params| {
                    require_idempotency_key(params)?;
                    let root = json_string_field(params, "project_root").ok_or_else(|| {
                        MezError::invalid_args("project/trust/revoke requires project_root")
                    })?;
                    trust_store.decide(
                        std::path::PathBuf::from(&root),
                        TrustDecision::Revoked,
                        None,
                    )?;
                    let record = trust_store
                        .get(std::path::Path::new(&root))
                        .expect("record inserted before lookup");
                    Ok(format!(
                        r#"{{"project":{},"diagnostics":[]}}"#,
                        project_trust_json(record)
                    ))
                })
            }
            _ => Err(MezError::not_implemented(format!(
                "unknown project trust method `{}`",
                request.method
            ))),
        }
    });
    match result {
        Ok(result) => json_rpc_success(&request.id, &result),
        Err(error) => json_rpc_error(
            &request.id,
            error_code(error.kind()),
            error.message(),
            mezzanine_error_code(error.kind()),
        ),
    }
}

/// Runs the dispatch config request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn dispatch_config_request(body: &str, layers: &[ConfigLayer]) -> String {
    let request = match parse_json_rpc_request(body) {
        Ok(request) => request,
        Err(error) => {
            return json_rpc_error("null", -32600, error.message(), "invalid_request");
        }
    };
    dispatch_config_parsed_to_response(&request, layers)
}

/// Runs the dispatch config request cached operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn dispatch_config_request_cached(
    body: &str,
    caller_client_id: &ClientId,
    layers: &[ConfigLayer],
    idempotency: &mut ControlIdempotencyCache,
) -> String {
    let request = match parse_json_rpc_request(body) {
        Ok(request) => request,
        Err(error) => {
            return json_rpc_error("null", -32600, error.message(), "invalid_request");
        }
    };
    dispatch_config_parsed_to_response_cached(&request, caller_client_id, layers, idempotency)
}

/// Runs the dispatch config parsed to response operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn dispatch_config_parsed_to_response(
    request: &JsonRpcRequest,
    layers: &[ConfigLayer],
) -> String {
    let result = dispatch_config_parsed_request(request, layers);
    match result {
        Ok(result) => json_rpc_success(&request.id, &result),
        Err(error) => json_rpc_error(
            &request.id,
            error_code(error.kind()),
            error.message(),
            mezzanine_error_code(error.kind()),
        ),
    }
}

/// Runs the dispatch config parsed request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn dispatch_config_parsed_request(
    request: &JsonRpcRequest,
    layers: &[ConfigLayer],
) -> Result<String> {
    validate_config_control_params_schema(request)?;
    match request.method.as_str() {
        "config/validate" => dispatch_config_validate(request.params.as_deref(), layers),
        "config/get" => dispatch_config_get(request.params.as_deref(), layers),
        "config/set" => {
            let params = request
                .params
                .as_deref()
                .ok_or_else(|| MezError::invalid_args("config/set requires a params object"));
            params.and_then(|params| dispatch_config_mutation("config/set", params, true))
        }
        "config/unset" => {
            let params = request
                .params
                .as_deref()
                .ok_or_else(|| MezError::invalid_args("config/unset requires a params object"));
            params.and_then(|params| dispatch_config_mutation("config/unset", params, false))
        }
        "config/reload" => {
            let params = request.params.as_deref().unwrap_or("{}");
            dispatch_config_reload(params, layers)
        }
        _ => Err(MezError::not_implemented(format!(
            "unknown config control method `{}`",
            request.method
        ))),
    }
}

/// Runs the validate config control params schema operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn validate_config_control_params_schema(request: &JsonRpcRequest) -> Result<()> {
    let params = request.params.as_deref().unwrap_or("{}");
    match request.method.as_str() {
        "config/validate" => {
            reject_unknown_json_fields(params, "config/validate params", &["files"])
        }
        "config/get" => {
            reject_unknown_json_fields(params, "config/get params", &["path", "effective"])
        }
        "config/set" => reject_unknown_json_fields(
            params,
            "config/set params",
            &["path", "value", "persist", "idempotency_key"],
        ),
        "config/unset" => reject_unknown_json_fields(
            params,
            "config/unset params",
            &["path", "persist", "idempotency_key"],
        ),
        "config/reload" => {
            reject_unknown_json_fields(params, "config/reload params", &["idempotency_key"])
        }
        _ => Ok(()),
    }
}

/// Runs the dispatch config parsed to response cached operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn dispatch_config_parsed_to_response_cached(
    request: &JsonRpcRequest,
    caller_client_id: &ClientId,
    layers: &[ConfigLayer],
    idempotency: &mut ControlIdempotencyCache,
) -> String {
    if !is_config_control_method(&request.method) {
        return json_rpc_error(
            &request.id,
            -32601,
            "config dispatcher accepts only config methods",
            "method_not_found",
        );
    }

    let cache_key = config_request_cache_key(request, caller_client_id);
    if let Some(cache_key) = &cache_key {
        match idempotency.cached_response(cache_key, &request.method, &request.params) {
            Ok(Some(response)) => return response,
            Ok(None) => {}
            Err(error) => {
                return json_rpc_error(
                    &request.id,
                    error_code(error.kind()),
                    error.message(),
                    mezzanine_error_code(error.kind()),
                );
            }
        }
    }

    let response = dispatch_config_parsed_to_response(request, layers);
    if let Some(cache_key) = cache_key {
        idempotency.remember_response(
            cache_key,
            request.method.clone(),
            request.params.clone(),
            response.clone(),
        );
    }
    response
}

/// Runs the config request cache key operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn config_request_cache_key(
    request: &JsonRpcRequest,
    caller_client_id: &ClientId,
) -> Option<String> {
    if !is_config_mutation_method(&request.method) {
        return None;
    }
    request
        .params
        .as_deref()
        .and_then(|params| json_string_field(params, "idempotency_key"))
        .map(|key| format!("{caller_client_id}:{key}"))
}

/// Runs the config response advances generation operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn config_response_advances_generation(method: &str, response: &str) -> bool {
    if !is_config_mutation_method(method) {
        return false;
    }
    !response.contains(r#""error""#)
        && (response.contains(r#""applied":true"#) || response.contains(r#""persisted":true"#))
}

/// Runs the is config control method operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn is_config_control_method(method: &str) -> bool {
    matches!(
        method,
        "config/validate" | "config/get" | "config/set" | "config/unset" | "config/reload"
    )
}

/// Runs the is config mutation method operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn is_config_mutation_method(method: &str) -> bool {
    matches!(method, "config/set" | "config/unset" | "config/reload")
}

/// Runs the dispatch config validate operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn dispatch_config_validate(
    params: Option<&str>,
    layers: &[ConfigLayer],
) -> Result<String> {
    let params = params.unwrap_or("{}");

    if let Some(files) = json_string_array_field(params, "files")? {
        let mut diagnostics = Vec::new();
        for file in files {
            let validation =
                validate_config_file(PathBuf::from(&file).as_path(), ConfigScope::Primary)?;
            diagnostics.extend(validation.diagnostics);
        }
        let validation = ConfigValidation {
            valid: diagnostics.is_empty(),
            diagnostics,
        };
        return Ok(format!(
            r#"{{"valid":{},"diagnostics":{}}}"#,
            validation.valid,
            config_diagnostics_json(&validation.diagnostics)
        ));
    }

    let mut diagnostics = Vec::new();
    for layer in layers {
        let validation =
            crate::config::validate_config_text(layer.format, &layer.text, layer.scope);
        diagnostics.extend(validation.diagnostics);
        if layer.scope == ConfigScope::ProjectOverlay && !layer.trusted {
            diagnostics.push(ConfigDiagnostic {
                path: layer
                    .path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| layer.name.clone()),
                message: "project overlay is pending trust and was not applied".to_string(),
            });
        }
    }
    let validation = ConfigValidation {
        valid: diagnostics.is_empty(),
        diagnostics,
    };
    Ok(format!(
        r#"{{"valid":{},"diagnostics":{}}}"#,
        validation.valid,
        config_diagnostics_json(&validation.diagnostics)
    ))
}

/// Runs the dispatch config get operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn dispatch_config_get(params: Option<&str>, layers: &[ConfigLayer]) -> Result<String> {
    let params = params.unwrap_or("{}");
    let _effective = config_get_effective_param(params)?;

    let effective = compose_effective_config(layers)?;
    let layer_json = config_layers_json(layers, &effective);
    if let Some(path) = json_string_field(params, "path") {
        let value = effective.get(&path);
        let source = effective.source_for(&path);
        return Ok(format!(
            r#"{{"path":"{}","value":{},"source":{},"layers":{}}}"#,
            json_escape(&path),
            config_optional_value_json(value),
            json_optional_string(source),
            layer_json
        ));
    }

    let values = effective
        .values()
        .iter()
        .map(|(path, value)| {
            format!(
                r#""{}":{}"#,
                json_escape(path),
                config_value_json(&value.value)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    Ok(format!(r#"{{"value":{{{values}}},"layers":{layer_json}}}"#))
}

/// Runs the config get effective param operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn config_get_effective_param(params: &str) -> Result<bool> {
    let value = serde_json::from_str::<serde_json::Value>(params)
        .map_err(|_| MezError::invalid_args("config/get params must be a JSON object"))?;
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args("config/get params must be a JSON object"))?;
    match object.get("effective") {
        None => Ok(true),
        Some(serde_json::Value::Bool(value)) => Ok(*value),
        Some(_) => Err(MezError::invalid_args(
            "config/get effective must be a boolean",
        )),
    }
}

/// Runs the config optional value json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn config_optional_value_json(value: Option<&str>) -> String {
    value
        .map(config_value_json)
        .unwrap_or_else(|| "null".to_string())
}

/// Runs the config value json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn config_value_json(value: &str) -> String {
    let trimmed = value.trim();
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return value.to_string();
    }
    format!(r#""{}""#, json_escape(value))
}

/// Runs the dispatch config mutation operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn dispatch_config_mutation(method: &str, params: &str, set: bool) -> Result<String> {
    require_idempotency_key(params)?;
    let path = json_string_field(params, "path")
        .ok_or_else(|| MezError::invalid_args(format!("{method} requires path")))?;
    let operation = if set {
        ConfigMutationOperation::Set(config_mutation_value_from_json(params)?)
    } else {
        ConfigMutationOperation::Unset
    };
    let mutation = ConfigMutation { path, operation };
    let Some(target) = persist_target_from_json(params)? else {
        let diagnostics = [ConfigDiagnostic {
            path: method.to_string(),
            message: "no persistence target was provided; live config mutation is not wired yet"
                .to_string(),
        }];
        return Ok(config_planning_result_json(
            false,
            false,
            false,
            &diagnostics,
            &config_blocked_mutation_plan_json(method, &mutation, None),
        ));
    };
    let Some(path) = target.path.clone() else {
        let diagnostics = [ConfigDiagnostic {
            path: method.to_string(),
            message: format!(
                "{} persistence requires a supported persist.path",
                target.scope_name
            ),
        }];
        return Ok(config_planning_result_json(
            false,
            false,
            false,
            &diagnostics,
            &config_blocked_mutation_plan_json(method, &mutation, Some(&target)),
        ));
    };
    if !matches!(
        target.scope,
        ConfigScope::Primary | ConfigScope::ProjectOverlay
    ) {
        let diagnostics = [ConfigDiagnostic {
            path: path.display().to_string(),
            message: "live persistence planning is not wired yet".to_string(),
        }];
        return Ok(config_planning_result_json(
            false,
            false,
            false,
            &diagnostics,
            &config_blocked_mutation_plan_json(method, &mutation, Some(&target)),
        ));
    }

    match persist_config_mutation(&path, target.scope, mutation) {
        Ok(plan) => Ok(config_mutation_plan_json(&plan, &target)),
        Err(error) => {
            let diagnostics = [ConfigDiagnostic {
                path: path.display().to_string(),
                message: error.message().to_string(),
            }];
            let failed_target = ControlPersistTarget {
                scope: target.scope,
                scope_name: target.scope_name,
                path: Some(path),
            };
            Ok(config_planning_result_json(
                false,
                false,
                false,
                &diagnostics,
                &config_blocked_plan_json(
                    method,
                    &failed_target,
                    "persistence_failed",
                    &diagnostics[0].message,
                ),
            ))
        }
    }
}

/// Runs the dispatch config reload operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn dispatch_config_reload(params: &str, layers: &[ConfigLayer]) -> Result<String> {
    require_idempotency_key(params)?;
    let diagnostics = config_layer_diagnostics(layers);
    let layers_valid = diagnostics.is_empty();
    Ok(config_planning_result_json(
        layers_valid,
        false,
        false,
        &diagnostics,
        &config_reload_plan_json(layers, layers_valid),
    ))
}

/// Runs the config audit plan operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn config_audit_plan(
    session: &Session,
    caller_client_id: &ClientId,
    request: &JsonRpcRequest,
) -> Option<AuditRecord> {
    let operation = request.method.strip_prefix("config/")?;
    if !matches!(operation, "set" | "unset" | "reload") {
        return None;
    }
    let params = request.params.as_deref().unwrap_or("{}");
    let key = json_string_field(params, "path").unwrap_or_else(|| request.method.clone());
    let scope = json_object_field(params, "persist")
        .and_then(|persist| {
            json_string_field(&persist, "scope")
                .or_else(|| json_string_field(&persist, "target"))
                .or_else(|| json_string_field(&persist, "layer"))
        })
        .unwrap_or_else(|| "live".to_string());
    Some(AuditRecord::config_change(
        session.id.as_str(),
        control_audit_actor(caller_client_id),
        scope,
        key,
        operation,
        "unknown",
    ))
}

/// Runs the config audit outcome operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn config_audit_outcome(response: &str) -> &'static str {
    if response.contains(r#""error""#) {
        "failed"
    } else if response.contains(r#""applied":true"#) {
        "applied"
    } else if response.contains(r#""persisted":true"#) {
        "persisted"
    } else {
        "planned"
    }
}

/// Carries Control Persist Target state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
pub(crate) struct ControlPersistTarget {
    /// Stores the scope value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) scope: ConfigScope,
    /// Stores the scope name value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) scope_name: String,
    /// Stores the path value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) path: Option<PathBuf>,
}

/// Runs the persist target from json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn persist_target_from_json(params: &str) -> Result<Option<ControlPersistTarget>> {
    if field_value(params, "persist").is_none() || json_null_field(params, "persist") {
        return Ok(None);
    }
    let persist = json_object_field(params, "persist").ok_or_else(|| {
        MezError::invalid_args("config mutation persist target must be an object or null")
    })?;
    reject_unknown_json_fields(
        &persist,
        "config mutation persist target",
        &["scope", "path"],
    )?;
    let scope_name = json_string_field(&persist, "scope")
        .ok_or_else(|| MezError::invalid_args("config mutation persist target requires scope"))?;
    let scope = parse_persist_scope(&scope_name)?;
    let path = json_string_field(&persist, "path").map(PathBuf::from);
    Ok(Some(ControlPersistTarget {
        scope,
        scope_name: config_persist_scope_name(scope).to_string(),
        path,
    }))
}

/// Runs the parse persist scope operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_persist_scope(value: &str) -> Result<ConfigScope> {
    match value {
        "user" => Ok(ConfigScope::Primary),
        "project" => Ok(ConfigScope::ProjectOverlay),
        "live" => Ok(ConfigScope::LiveOverride),
        _ => Err(MezError::invalid_args(
            "config mutation persist scope must be live, user, or project",
        )),
    }
}

/// Runs the config mutation value from json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn config_mutation_value_from_json(params: &str) -> Result<ConfigMutationValue> {
    let value = json_raw_field(params, "value")
        .ok_or_else(|| MezError::invalid_args("config/set requires value"))?;
    match serde_json::from_str::<serde_json::Value>(value.trim()) {
        Ok(serde_json::Value::String(value)) => Ok(ConfigMutationValue::String(value)),
        Ok(serde_json::Value::Bool(value)) => Ok(ConfigMutationValue::Boolean(value)),
        Ok(serde_json::Value::Number(value)) => value
            .as_i64()
            .map(ConfigMutationValue::Integer)
            .ok_or_else(|| MezError::invalid_args("config/set integer value is invalid")),
        Ok(serde_json::Value::Array(values)) => {
            let mut strings = Vec::with_capacity(values.len());
            for value in values {
                let serde_json::Value::String(value) = value else {
                    return Err(MezError::invalid_args(
                        "config/set string arrays must contain only strings",
                    ));
                };
                strings.push(value);
            }
            Ok(ConfigMutationValue::StringArray(strings))
        }
        Ok(serde_json::Value::Object(_) | serde_json::Value::Null) => Err(MezError::invalid_args(
            "config/set supports only string, integer, boolean, or string-array values",
        )),
        Err(_) => Err(MezError::invalid_args("config/set value is invalid JSON")),
    }
}

/// Runs the config mutation plan json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn config_mutation_plan_json(
    plan: &ConfigMutationPlan,
    target: &ControlPersistTarget,
) -> String {
    config_mutation_plan_result_json(plan, target, true)
}

/// Runs the config mutation plan result json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn config_mutation_plan_result_json(
    plan: &ConfigMutationPlan,
    target: &ControlPersistTarget,
    persisted: bool,
) -> String {
    config_planning_result_json(
        plan.changed,
        persisted,
        plan.reload_required,
        &plan.validation.diagnostics,
        &format!(
            r#"{{"operation":"{}","path":"{}","target":{},"format":"{}","scope":"{}","changed":{},"validated":{},"reload_required":{}}}"#,
            config_mutation_operation_name(&plan.operation),
            json_escape(&plan.path),
            config_persist_target_json(target),
            config_format_name(plan.format),
            config_persist_scope_name(plan.scope),
            plan.changed,
            plan.validation.valid,
            plan.reload_required
        ),
    )
}

/// Runs the config planning result json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn config_planning_result_json(
    applied: bool,
    persisted: bool,
    reload_required: bool,
    diagnostics: &[ConfigDiagnostic],
    plan_json: &str,
) -> String {
    format!(
        r#"{{"applied":{},"persisted":{},"reload_required":{},"diagnostics":{},"plan":{}}}"#,
        applied,
        persisted,
        reload_required,
        config_diagnostics_json(diagnostics),
        plan_json
    )
}

/// Runs the config mutation operation name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn config_mutation_operation_name(operation: &ConfigMutationOperation) -> &'static str {
    match operation {
        ConfigMutationOperation::Set(_) => "set",
        ConfigMutationOperation::Unset => "unset",
    }
}

/// Runs the config blocked mutation plan json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn config_blocked_mutation_plan_json(
    method: &str,
    mutation: &ConfigMutation,
    target: Option<&ControlPersistTarget>,
) -> String {
    format!(
        r#"{{"operation":"{}","path":"{}","target":{},"changed":false,"validated":false,"reload_required":false,"status":"blocked"}}"#,
        if method == "config/set" {
            "set"
        } else {
            "unset"
        },
        json_escape(&mutation.path),
        target
            .map(config_persist_target_json)
            .unwrap_or_else(|| "null".to_string())
    )
}

/// Runs the config blocked plan json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn config_blocked_plan_json(
    method: &str,
    target: &ControlPersistTarget,
    status: &str,
    reason: &str,
) -> String {
    format!(
        r#"{{"operation":"{}","target":{},"changed":false,"validated":false,"reload_required":false,"status":"{}","reason":"{}"}}"#,
        method.strip_prefix("config/").unwrap_or(method),
        config_persist_target_json(target),
        json_escape(status),
        json_escape(reason)
    )
}

/// Runs the config reload plan json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn config_reload_plan_json(layers: &[ConfigLayer], validated: bool) -> String {
    format!(
        r#"{{"operation":"reload","target":"effective_configuration","layers":{},"changed":false,"validated":{},"reload_required":false,"status":"{}"}}"#,
        config_layer_inputs_json(layers),
        validated,
        if validated { "applied" } else { "blocked" }
    )
}

/// Runs the config persist target json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn config_persist_target_json(target: &ControlPersistTarget) -> String {
    format!(
        r#"{{"scope":"{}","path":{}}}"#,
        config_persist_scope_name(target.scope),
        json_optional_string(
            target
                .path
                .as_ref()
                .map(|path| path.to_string_lossy())
                .as_deref()
        )
    )
}

/// Runs the config layer diagnostics operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn config_layer_diagnostics(layers: &[ConfigLayer]) -> Vec<ConfigDiagnostic> {
    let mut diagnostics = Vec::new();
    for layer in layers {
        diagnostics.extend(config_layer_diagnostics_for_layer(layer));
    }
    diagnostics
}

/// Runs the config layer diagnostics for layer operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn config_layer_diagnostics_for_layer(layer: &ConfigLayer) -> Vec<ConfigDiagnostic> {
    let validation = crate::config::validate_config_text(layer.format, &layer.text, layer.scope);
    let mut diagnostics = validation.diagnostics;
    if layer.scope == ConfigScope::ProjectOverlay && !layer.trusted {
        diagnostics.push(ConfigDiagnostic {
            path: layer
                .path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| layer.name.clone()),
            message: "project overlay is pending trust and was not applied".to_string(),
        });
    }
    diagnostics
}

/// Runs the config layer inputs json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn config_layer_inputs_json(layers: &[ConfigLayer]) -> String {
    let layers = layers
        .iter()
        .map(|layer| {
            format!(
                r#"{{"name":"{}","path":{},"format":"{}","scope":"{}","trusted":{}}}"#,
                json_escape(&layer.name),
                json_optional_string(
                    layer
                        .path
                        .as_ref()
                        .map(|path| path.to_string_lossy())
                        .as_deref()
                ),
                config_format_name(layer.format),
                config_scope_name(layer.scope),
                layer.trusted
            )
        })
        .collect::<Vec<_>>();
    format!("[{}]", layers.join(","))
}

/// Runs the config layers json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn config_layers_json(
    layers: &[ConfigLayer],
    effective: &crate::config::EffectiveConfig,
) -> String {
    let layers = layers
        .iter()
        .enumerate()
        .map(|(index, layer)| {
            let state = if effective.applied_layers().contains(&layer.name) {
                "applied"
            } else if effective.skipped_layers().contains(&layer.name) {
                "skipped"
            } else {
                "pending"
            };
            let applied = state == "applied";
            let diagnostics = config_layer_diagnostics_for_layer(layer);
            format!(
                r#"{{"id":"{}","version":1,"name":"{}","layer_type":"{}","precedence":{},"path":{},"format":"{}","scope":"{}","trusted":{},"applied":{},"state":"{}","schema_version":1,"diagnostics":{}}}"#,
                json_escape(&layer.name),
                json_escape(&layer.name),
                config_layer_type_name(layer.scope),
                index,
                json_optional_string(
                    layer
                        .path
                        .as_ref()
                        .map(|path| path.to_string_lossy())
                        .as_deref()
                ),
                config_format_name(layer.format),
                config_scope_name(layer.scope),
                layer.trusted,
                applied,
                state,
                config_diagnostics_json(&diagnostics)
            )
        })
        .collect::<Vec<_>>();
    format!("[{}]", layers.join(","))
}

/// Runs the config diagnostics json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn config_diagnostics_json(diagnostics: &[ConfigDiagnostic]) -> String {
    let diagnostics = diagnostics
        .iter()
        .map(|diagnostic| {
            format!(
                r#"{{"severity":"error","code":"config_invalid","message":"{}","path":"{}"}}"#,
                json_escape(&diagnostic.message),
                json_escape(&diagnostic.path),
            )
        })
        .collect::<Vec<_>>();
    format!("[{}]", diagnostics.join(","))
}

/// Runs the config format name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn config_format_name(format: ConfigFormat) -> &'static str {
    match format {
        ConfigFormat::Toml => "toml",
        ConfigFormat::Yaml => "yaml",
        ConfigFormat::Json => "json",
    }
}

/// Runs the config scope name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn config_scope_name(scope: ConfigScope) -> &'static str {
    match scope {
        ConfigScope::Primary => "user",
        ConfigScope::ProjectOverlay => "project",
        ConfigScope::LiveOverride => "live",
    }
}

/// Runs the config persist scope name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn config_persist_scope_name(scope: ConfigScope) -> &'static str {
    config_scope_name(scope)
}

/// Runs the config layer type name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn config_layer_type_name(scope: ConfigScope) -> &'static str {
    match scope {
        ConfigScope::Primary => "user",
        ConfigScope::ProjectOverlay => "project_root",
        ConfigScope::LiveOverride => "live",
    }
}
