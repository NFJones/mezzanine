//! Runtime control helpers for configuration and project-trust requests.
//!
//! This module owns live and persisted configuration mutation handling, config
//! reload audit records, project trust reads and mutations, and the helpers that
//! connect trusted project state back into runtime config layers. The parent
//! control module keeps request routing while this module keeps configuration
//! persistence rules out of the main control facade.

use super::*;

impl RuntimeSessionService {
    /// Runs the dispatch runtime config request operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_config_request(
        &mut self,
        body: &str,
        request: &crate::control::JsonRpcRequest,
        caller_client_id: &mez_core::ids::ClientId,
    ) -> String {
        if let Some(response) =
            self.dispatch_runtime_live_config_mutation_request(request, caller_client_id)
        {
            return response;
        }
        if let Some(response) =
            self.dispatch_runtime_deferred_config_file_mutation_request(request, caller_client_id)
        {
            return response;
        }
        if let Err(error) = self.validate_runtime_config_disk_persist_target(request) {
            return runtime_json_rpc_error(&request.id, error.kind(), error.message());
        }
        let response = if let Some(audit_log) = self.audit_log.as_mut() {
            dispatch_control_request_for_client_with_config_and_audit(
                body,
                &mut self.session,
                caller_client_id,
                &self.config_layers,
                &mut self.control_idempotency,
                audit_log,
            )
        } else {
            dispatch_control_request_for_client_with_config(
                body,
                &mut self.session,
                caller_client_id,
                &self.config_layers,
                &mut self.control_idempotency,
            )
        };
        if response.contains(r#""result""#)
            && runtime_config_method_applies_to_live_service(&request.method)
        {
            let previous_permission_policy = self.permission_policy.clone();
            match self.reload_config_layers_from_disk() {
                Ok(report) => {
                    let payload = runtime_config_apply_event_payload(&request.method, &report);
                    if let Err(error) =
                        self.append_lifecycle_event(EventKind::ConfigChanged, payload)
                    {
                        return runtime_json_rpc_error(&request.id, error.kind(), error.message());
                    }
                    if let Err(error) = self.append_config_reload_permission_audits(
                        caller_client_id,
                        &previous_permission_policy,
                    ) {
                        return runtime_json_rpc_error(&request.id, error.kind(), error.message());
                    }
                    if let Err(error) = self
                        .reconcile_pending_agent_approvals_after_permission_change(
                            Some(caller_client_id),
                            &previous_permission_policy,
                            &request.method,
                        )
                    {
                        return runtime_json_rpc_error(&request.id, error.kind(), error.message());
                    }
                }
                Err(error) => {
                    return runtime_json_rpc_error(&request.id, error.kind(), error.message());
                }
            }
        }
        response
    }

    /// Runs the validate runtime config disk persist target operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn validate_runtime_config_disk_persist_target(
        &self,
        request: &crate::control::JsonRpcRequest,
    ) -> Result<()> {
        if !matches!(request.method.as_str(), "config/set" | "config/unset") {
            return Ok(());
        }
        let Some(params) = request.params.as_deref() else {
            return Ok(());
        };
        let Some(target) = persist_target_from_json(params)? else {
            return Ok(());
        };
        match target.scope {
            ConfigScope::LiveOverride => Ok(()),
            ConfigScope::Primary => self.validate_runtime_user_config_persist_target(&target),
            ConfigScope::ProjectOverlay => {
                self.validate_runtime_project_config_persist_target(&target)
            }
        }
    }

    /// Runs the validate runtime user config persist target operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn validate_runtime_user_config_persist_target(
        &self,
        target: &ControlPersistTarget,
    ) -> Result<()> {
        let Some(path) = target.path.as_ref() else {
            return Ok(());
        };
        if self.config_layers.iter().any(|layer| {
            layer.scope == ConfigScope::Primary
                && layer
                    .path
                    .as_ref()
                    .is_some_and(|layer_path| paths_equivalent(layer_path, path))
        }) {
            return Ok(());
        }
        if self
            .config_root
            .as_ref()
            .is_some_and(|root| runtime_path_under_project_root(path, root))
        {
            return Ok(());
        }
        Err(MezError::invalid_args(format!(
            "user config persistence target {} must be under the configured user-private config root",
            path.display()
        )))
    }

    /// Runs the validate runtime project config persist target operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn validate_runtime_project_config_persist_target(
        &self,
        target: &ControlPersistTarget,
    ) -> Result<()> {
        let Some(path) = target.path.as_ref() else {
            return Ok(());
        };
        let Some(store) = self.project_trust_store.as_ref() else {
            return Err(MezError::conflict(
                "project config persistence is blocked until project trust is available",
            ));
        };
        let Some(record) = store
            .records()
            .find(|record| runtime_path_under_project_root(path, &record.project_root))
        else {
            let project_root = path
                .parent()
                .map(discover_project_root)
                .unwrap_or_else(|| discover_project_root(path));
            return Err(MezError::conflict(format!(
                "project config persistence for {} is blocked until project trust is decided",
                project_root.display()
            )));
        };
        match record.state {
            TrustDecision::Trusted => Ok(()),
            TrustDecision::Pending => Err(MezError::conflict(
                "project config persistence is blocked until project trust is decided",
            )),
            TrustDecision::Rejected | TrustDecision::Revoked => Err(MezError::forbidden(
                "project config persistence requires a trusted project root",
            )),
        }
    }

    /// Runs the dispatch runtime live config mutation request operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn dispatch_runtime_live_config_mutation_request(
        &mut self,
        request: &crate::control::JsonRpcRequest,
        caller_client_id: &mez_core::ids::ClientId,
    ) -> Option<String> {
        if !matches!(request.method.as_str(), "config/set" | "config/unset") {
            return None;
        }
        if let Err(error) = authorize_control_request(&self.session, caller_client_id, request) {
            return Some(runtime_json_rpc_error(
                &request.id,
                error.kind(),
                error.message(),
            ));
        }
        let params = request.params.as_deref()?;
        let target = match persist_target_from_json(params) {
            Ok(Some(target)) if target.scope == ConfigScope::LiveOverride => target,
            Ok(Some(_)) => return None,
            Ok(None) => ControlPersistTarget {
                scope: ConfigScope::LiveOverride,
                scope_name: "live".to_string(),
                path: None,
            },
            Err(error) => {
                return Some(runtime_json_rpc_error(
                    &request.id,
                    error.kind(),
                    error.message(),
                ));
            }
        };
        Some(self.dispatch_runtime_live_config_mutation_response(
            request,
            caller_client_id,
            params,
            target,
        ))
    }

    /// Runs the dispatch runtime deferred config file mutation request operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn dispatch_runtime_deferred_config_file_mutation_request(
        &mut self,
        request: &crate::control::JsonRpcRequest,
        caller_client_id: &mez_core::ids::ClientId,
    ) -> Option<String> {
        if !self.config_effects_use_adapter {
            return None;
        }
        if !matches!(request.method.as_str(), "config/set" | "config/unset") {
            return None;
        }
        let params = request.params.as_deref()?;
        let target = match persist_target_from_json(params) {
            Ok(Some(target))
                if matches!(
                    target.scope,
                    ConfigScope::Primary | ConfigScope::ProjectOverlay
                ) =>
            {
                target
            }
            Ok(_) => return None,
            Err(error) => {
                return Some(runtime_json_rpc_error(
                    &request.id,
                    error.kind(),
                    error.message(),
                ));
            }
        };
        Some(
            self.dispatch_runtime_deferred_config_file_mutation_response(
                request,
                caller_client_id,
                params,
                target,
            ),
        )
    }

    /// Runs the dispatch runtime live config mutation response operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn dispatch_runtime_live_config_mutation_response(
        &mut self,
        request: &crate::control::JsonRpcRequest,
        caller_client_id: &mez_core::ids::ClientId,
        params: &str,
        target: ControlPersistTarget,
    ) -> String {
        if let Err(error) = validate_control_method_params_schema(request) {
            return runtime_json_rpc_error(&request.id, error.kind(), error.message());
        }
        let idempotency_key = match runtime_json_string_field(params, "idempotency_key") {
            Some(value) => value,
            None => {
                return runtime_json_rpc_error(
                    &request.id,
                    crate::error::MezErrorKind::InvalidArgs,
                    "mutating control method requires idempotency_key",
                );
            }
        };
        let cache_key = format!("{caller_client_id}:{idempotency_key}");
        let audit_plan = config_audit_plan(&self.session, caller_client_id, request);
        if let Some(mut record) = audit_plan.clone() {
            record.outcome = "started".to_string();
            if let Err(error) = self.append_runtime_config_audit_record(record) {
                return runtime_json_rpc_error(&request.id, error.kind(), error.message());
            }
        }
        match self
            .control_idempotency
            .cached_response(&cache_key, &request.method, &request.params)
        {
            Ok(Some(response)) => {
                if let Some(mut record) = audit_plan {
                    record.outcome = config_audit_outcome(&response).to_string();
                    if let Err(error) = self.append_runtime_config_audit_record(record) {
                        return runtime_json_rpc_error(&request.id, error.kind(), error.message());
                    }
                }
                return response;
            }
            Ok(None) => {}
            Err(error) => {
                let response = runtime_json_rpc_error(&request.id, error.kind(), error.message());
                if let Some(mut record) = audit_plan {
                    record.outcome = config_audit_outcome(&response).to_string();
                    if let Err(error) = self.append_runtime_config_audit_record(record) {
                        return runtime_json_rpc_error(&request.id, error.kind(), error.message());
                    }
                }
                return response;
            }
        }

        let response = match self.dispatch_runtime_live_config_mutation_result(
            request.method.as_str(),
            caller_client_id,
            params,
            &target,
        ) {
            Ok(result) => format!(
                r#"{{"jsonrpc":"2.0","id":{},"result":{result}}}"#,
                request.id
            ),
            Err(error) => runtime_json_rpc_error(&request.id, error.kind(), error.message()),
        };
        if let Some(mut record) = audit_plan {
            record.outcome = config_audit_outcome(&response).to_string();
            if let Err(error) = self.append_runtime_config_audit_record(record) {
                return runtime_json_rpc_error(&request.id, error.kind(), error.message());
            }
        }
        if config_response_advances_generation(&request.method, &response) {
            self.session.advance_config_generation();
        }
        self.control_idempotency.remember_response(
            cache_key,
            request.method.clone(),
            request.params.clone(),
            response.clone(),
        );
        response
    }

    /// Runs the dispatch runtime deferred config file mutation response operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn dispatch_runtime_deferred_config_file_mutation_response(
        &mut self,
        request: &crate::control::JsonRpcRequest,
        caller_client_id: &mez_core::ids::ClientId,
        params: &str,
        target: ControlPersistTarget,
    ) -> String {
        if let Err(error) = validate_control_method_params_schema(request) {
            return runtime_json_rpc_error(&request.id, error.kind(), error.message());
        }
        if let Err(error) = authorize_control_request(&self.session, caller_client_id, request) {
            return runtime_json_rpc_error(&request.id, error.kind(), error.message());
        }
        if let Err(error) = self.validate_runtime_config_disk_persist_target(request) {
            return runtime_json_rpc_error(&request.id, error.kind(), error.message());
        }
        let Some(cache_key) = config_request_cache_key(request, caller_client_id) else {
            return runtime_json_rpc_error(
                &request.id,
                crate::error::MezErrorKind::InvalidArgs,
                "mutating control method requires idempotency_key",
            );
        };
        let audit_plan = config_audit_plan(&self.session, caller_client_id, request);
        if let Some(mut record) = audit_plan.clone() {
            record.outcome = "started".to_string();
            if let Err(error) = self.append_runtime_config_audit_record(record) {
                return runtime_json_rpc_error(&request.id, error.kind(), error.message());
            }
        }
        match self
            .control_idempotency
            .cached_response(&cache_key, &request.method, &request.params)
        {
            Ok(Some(response)) => {
                if let Some(mut record) = audit_plan {
                    record.outcome = config_audit_outcome(&response).to_string();
                    if let Err(error) = self.append_runtime_config_audit_record(record) {
                        return runtime_json_rpc_error(&request.id, error.kind(), error.message());
                    }
                }
                return response;
            }
            Ok(None) => {}
            Err(error) => {
                let response = runtime_json_rpc_error(&request.id, error.kind(), error.message());
                if let Some(mut record) = audit_plan {
                    record.outcome = config_audit_outcome(&response).to_string();
                    if let Err(error) = self.append_runtime_config_audit_record(record) {
                        return runtime_json_rpc_error(&request.id, error.kind(), error.message());
                    }
                }
                return response;
            }
        }

        let response = match self.dispatch_runtime_deferred_config_file_mutation_result(
            request.method.as_str(),
            caller_client_id,
            params,
            &target,
        ) {
            Ok(result) => format!(
                r#"{{"jsonrpc":"2.0","id":{},"result":{result}}}"#,
                request.id
            ),
            Err(error) => runtime_json_rpc_error(&request.id, error.kind(), error.message()),
        };
        if let Some(mut record) = audit_plan {
            record.outcome = config_audit_outcome(&response).to_string();
            if let Err(error) = self.append_runtime_config_audit_record(record) {
                return runtime_json_rpc_error(&request.id, error.kind(), error.message());
            }
        }
        if config_response_advances_generation(&request.method, &response) {
            self.session.advance_config_generation();
        }
        self.control_idempotency.remember_response(
            cache_key,
            request.method.clone(),
            request.params.clone(),
            response.clone(),
        );
        response
    }

    /// Runs the dispatch runtime live config mutation result operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn dispatch_runtime_live_config_mutation_result(
        &mut self,
        method: &str,
        caller_client_id: &mez_core::ids::ClientId,
        params: &str,
        target: &ControlPersistTarget,
    ) -> Result<String> {
        let path = runtime_json_string_field(params, "path")
            .ok_or_else(|| MezError::invalid_args(format!("{method} requires path")))?;
        let operation = if method == "config/set" {
            ConfigMutationOperation::Set(config_mutation_value_from_json(params)?)
        } else {
            ConfigMutationOperation::Unset
        };
        let mutation = ConfigMutation { path, operation };
        let current_text = self
            .config_layers
            .iter()
            .find(|layer| {
                layer.name == RUNTIME_CONTROL_LIVE_OVERRIDE_LAYER
                    && layer.scope == ConfigScope::LiveOverride
            })
            .map(|layer| layer.text.as_str())
            .unwrap_or("");
        let plan = plan_config_mutation(
            ConfigFormat::Toml,
            current_text,
            ConfigScope::LiveOverride,
            mutation,
        )?;
        if plan.changed {
            let previous_layers = self.config_layers.clone();
            let previous_permission_policy = self.permission_policy.clone();
            self.store_runtime_control_live_override_plan(&plan.text);
            match self.apply_runtime_config_layers() {
                Ok(report) => {
                    let payload = runtime_config_apply_event_payload(method, &report);
                    self.append_lifecycle_event(EventKind::ConfigChanged, payload)?;
                    self.append_config_reload_permission_audits(
                        caller_client_id,
                        &previous_permission_policy,
                    )?;
                    self.reconcile_pending_agent_approvals_after_permission_change(
                        Some(caller_client_id),
                        &previous_permission_policy,
                        method,
                    )?;
                }
                Err(error) => {
                    self.config_layers = previous_layers;
                    let _ = self.apply_runtime_config_layers();
                    return Err(error);
                }
            }
        }
        Ok(config_mutation_plan_result_json(&plan, target, false))
    }

    /// Runs the dispatch runtime deferred config file mutation result operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn dispatch_runtime_deferred_config_file_mutation_result(
        &mut self,
        method: &str,
        caller_client_id: &mez_core::ids::ClientId,
        params: &str,
        target: &ControlPersistTarget,
    ) -> Result<String> {
        let path = runtime_json_string_field(params, "path")
            .ok_or_else(|| MezError::invalid_args(format!("{method} requires path")))?;
        let operation = if method == "config/set" {
            ConfigMutationOperation::Set(config_mutation_value_from_json(params)?)
        } else {
            ConfigMutationOperation::Unset
        };
        let mutation = ConfigMutation { path, operation };
        let target_path = target.path.as_ref().ok_or_else(|| {
            MezError::invalid_args(format!(
                "{} persistence requires a supported persist.path",
                target.scope_name
            ))
        })?;
        let format = ConfigFormat::from_path(target_path)?;
        let current_text = self.config_file_text_for_update(target_path, target.scope)?;
        let plan = plan_config_mutation(format, &current_text, target.scope, mutation)?;
        if plan.changed {
            let previous_layers = self.config_layers.clone();
            let previous_permission_policy = self.permission_policy.clone();
            self.store_runtime_config_file_plan(
                target_path.clone(),
                plan.format,
                plan.scope,
                &plan.text,
            );
            match self.apply_runtime_config_layers() {
                Ok(report) => {
                    let payload = runtime_config_apply_event_payload(method, &report);
                    self.append_lifecycle_event(EventKind::ConfigChanged, payload)?;
                    self.append_config_reload_permission_audits(
                        caller_client_id,
                        &previous_permission_policy,
                    )?;
                    self.reconcile_pending_agent_approvals_after_permission_change(
                        Some(caller_client_id),
                        &previous_permission_policy,
                        method,
                    )?;
                    let persistence_target = match plan.scope {
                        ConfigScope::Primary | ConfigScope::LiveOverride => {
                            crate::runtime::PersistenceTarget::Config
                        }
                        ConfigScope::ProjectOverlay => {
                            crate::runtime::PersistenceTarget::ProjectConfig
                        }
                    };
                    self.queued_config_effects.push(RuntimeSideEffect::Persist {
                        target: persistence_target,
                        path: target_path.clone(),
                        bytes: plan.text.clone().into_bytes(),
                        mode: crate::runtime::PersistenceWriteMode::Replace,
                    });
                }
                Err(error) => {
                    self.config_layers = previous_layers;
                    let _ = self.apply_runtime_config_layers();
                    return Err(error);
                }
            }
        }
        Ok(config_mutation_plan_result_json(&plan, target, true))
    }

    /// Runs the store runtime control live override plan operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn store_runtime_control_live_override_plan(&mut self, text: &str) {
        if let Some(layer) = self.config_layers.iter_mut().find(|layer| {
            layer.name == RUNTIME_CONTROL_LIVE_OVERRIDE_LAYER
                && layer.scope == ConfigScope::LiveOverride
        }) {
            layer.text = text.to_string();
        } else {
            self.config_layers.push(ConfigLayer {
                name: RUNTIME_CONTROL_LIVE_OVERRIDE_LAYER.to_string(),
                path: None,
                format: ConfigFormat::Toml,
                scope: ConfigScope::LiveOverride,
                trusted: true,
                text: text.to_string(),
            });
        }
    }

    /// Runs the config file text for update operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn config_file_text_for_update(&self, path: &Path, scope: ConfigScope) -> Result<String> {
        if let Some(text) = self.config_layers.iter().find_map(|layer| {
            (layer.scope == scope
                && layer
                    .path
                    .as_ref()
                    .is_some_and(|layer_path| paths_equivalent(layer_path, path)))
            .then_some(layer.text.clone())
        }) {
            return Ok(text);
        }
        Ok(fs::read_to_string(path)?)
    }

    /// Runs the store runtime config file plan operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn store_runtime_config_file_plan(
        &mut self,
        path: PathBuf,
        format: ConfigFormat,
        scope: ConfigScope,
        text: &str,
    ) {
        let trusted = scope != ConfigScope::ProjectOverlay
            || self.project_trust_store.as_ref().is_some_and(|store| {
                store.records().any(|record| {
                    record.state == TrustDecision::Trusted
                        && runtime_path_under_project_root(&path, &record.project_root)
                })
            });
        if let Some(layer) = self.config_layers.iter_mut().find(|layer| {
            layer.scope == scope
                && layer
                    .path
                    .as_ref()
                    .is_some_and(|layer_path| paths_equivalent(layer_path, &path))
        }) {
            layer.format = format;
            layer.trusted = trusted;
            layer.text = text.to_string();
        } else {
            self.config_layers.push(ConfigLayer {
                name: match scope {
                    ConfigScope::Primary => "primary",
                    ConfigScope::ProjectOverlay => "project",
                    ConfigScope::LiveOverride => RUNTIME_CONTROL_LIVE_OVERRIDE_LAYER,
                }
                .to_string(),
                path: Some(path),
                format,
                scope,
                trusted,
                text: text.to_string(),
            });
        }
    }

    /// Runs the append runtime config audit record operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn append_runtime_config_audit_record(&mut self, record: AuditRecord) -> Result<()> {
        let Some(audit_log) = self.audit_log.as_mut() else {
            return Ok(());
        };
        audit_log.append(record.sanitized())?;
        Ok(())
    }

    /// Runs the append config reload permission audits operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn append_config_reload_permission_audits(
        &mut self,
        caller_client_id: &mez_core::ids::ClientId,
        previous: &crate::permissions::PermissionPolicy,
    ) -> Result<()> {
        if self.audit_log.is_none() {
            return Ok(());
        }
        if previous.preset != self.permission_policy.preset {
            self.append_config_reload_permission_audit(
                caller_client_id,
                "permissions.preset",
                runtime_permission_preset_name(self.permission_policy.preset),
            )?;
        }
        if previous.approval_policy != self.permission_policy.approval_policy {
            self.append_config_reload_permission_audit(
                caller_client_id,
                "permissions.approval_policy",
                runtime_approval_policy_name(self.permission_policy.approval_policy),
            )?;
        }
        if previous.approval_bypass() != self.permission_policy.approval_bypass() {
            self.append_config_reload_permission_audit(
                caller_client_id,
                "permissions.bypass_mode",
                if self.permission_policy.approval_bypass() {
                    "enabled"
                } else {
                    "disabled"
                },
            )?;
        }
        if previous.rules() != self.permission_policy.rules() {
            self.append_config_reload_permission_audit(
                caller_client_id,
                "permissions.command_rules",
                "updated",
            )?;
        }
        Ok(())
    }

    /// Runs the append config reload permission audit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn append_config_reload_permission_audit(
        &mut self,
        caller_client_id: &mez_core::ids::ClientId,
        permission_id: &str,
        decision: &str,
    ) -> Result<()> {
        let policy_mode = runtime_permission_preset_name(self.permission_policy.preset).to_string();
        let Some(audit_log) = self.audit_log.as_mut() else {
            return Ok(());
        };
        let record = AuditRecord::permission_decision(
            self.session.id.to_string(),
            AuditActor {
                kind: "client".to_string(),
                id: caller_client_id.as_str().to_string(),
            },
            permission_id.to_string(),
            "config_reload".to_string(),
            decision.to_string(),
            policy_mode,
            "changed".to_string(),
        );
        let _ = audit_log.append(record)?;
        Ok(())
    }

    /// Runs the dispatch runtime project trust request operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_project_trust_request(
        &mut self,
        request: &crate::control::JsonRpcRequest,
        caller_client_id: &mez_core::ids::ClientId,
    ) -> String {
        let result = validate_control_method_params_schema(request).and_then(|()| {
            self.dispatch_runtime_project_trust_result(
                request.method.as_str(),
                caller_client_id,
                request.params.as_deref().unwrap_or("{}"),
            )
        });
        match result {
            Ok(result) => format!(
                r#"{{"jsonrpc":"2.0","id":{},"result":{result}}}"#,
                request.id
            ),
            Err(error) => runtime_json_rpc_error(&request.id, error.kind(), error.message()),
        }
    }

    /// Runs the dispatch runtime project trust result operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_project_trust_result(
        &mut self,
        method: &str,
        caller_client_id: &mez_core::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        if self.session.primary_client_id() != Some(caller_client_id) {
            return Err(MezError::forbidden(
                "project trust methods require the primary client",
            ));
        }
        match method {
            "project/trust/list" => {
                let state = project_trust_state_filter_from_params(
                    Some(params),
                    "project/trust/list params",
                )?;
                let store = self.runtime_project_trust_store()?;
                let projects = store
                    .records()
                    .filter(|record| state.is_none_or(|state| record.state == state))
                    .map(|record| runtime_project_trust_record_json(record, &self.config_layers))
                    .collect::<Vec<_>>()
                    .join(",");
                Ok(format!(r#"{{"projects":[{projects}]}}"#))
            }
            "project/trust/inspect" => {
                let root = runtime_project_root_param(params, "project/trust/inspect")?;
                let store = self.runtime_project_trust_store()?;
                let record = store.get(&root).ok_or_else(|| {
                    MezError::new(
                        crate::error::MezErrorKind::NotFound,
                        "project trust record not found",
                    )
                })?;
                Ok(format!(
                    r#"{{"project":{}}}"#,
                    runtime_project_trust_record_json(record, &self.config_layers)
                ))
            }
            _ => Err(MezError::invalid_state(
                "runtime project trust method was filtered incorrectly",
            )),
        }
    }

    /// Runs the dispatch runtime project trust mutation operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_project_trust_mutation(
        &mut self,
        method: &str,
        caller_client_id: &mez_core::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        if self.session.primary_client_id() != Some(caller_client_id) {
            return Err(MezError::forbidden(
                "project trust mutations require the primary client",
            ));
        }
        let project_root = runtime_project_root_param(params, method)?;
        let decision = if method == "project/trust/revoke" {
            TrustDecision::Revoked
        } else {
            runtime_trust_decision_param(params)?
        };
        let record = {
            let database_path = self.project_trust_database_path.clone().or_else(|| {
                self.config_root
                    .as_ref()
                    .map(|root| default_trust_database_path(root))
            });
            if self.project_trust_database_path.is_none() {
                self.project_trust_database_path = database_path.clone();
            }
            let store = self.runtime_project_trust_store_mut()?;
            store.decide_with_client(
                project_root.clone(),
                decision,
                None,
                Some(caller_client_id.to_string()),
            )?;
            if let Some(path) = database_path.as_ref() {
                store.save_to_file(path)?;
            }
            store
                .get(&project_root)
                .cloned()
                .ok_or_else(|| MezError::invalid_state("project trust record was not retained"))?
        };
        let changed_layers = self.apply_project_trust_decision_to_layers(&project_root, decision);
        self.announced_project_trust_roots.remove(&project_root);
        let report = self.apply_runtime_config_layers()?;
        self.append_lifecycle_event(
            EventKind::ConfigChanged,
            runtime_config_apply_event_payload(method, &report),
        )?;
        if let Some(audit_log) = self.audit_log.as_mut() {
            let operation = method.replace('/', "_");
            let record = AuditRecord::config_change(
                self.session.id.to_string(),
                AuditActor {
                    kind: "client".to_string(),
                    id: caller_client_id.to_string(),
                },
                "project_trust",
                project_root.to_string_lossy().to_string(),
                operation,
                "applied",
            )
            .with_metadata("decision", runtime_trust_decision_name(decision))
            .with_metadata("project_root", project_root.to_string_lossy().to_string());
            let _ = audit_log.append(record)?;
        }
        Ok(format!(
            r#"{{"project":{},"changed_layers":{},"config":{}}}"#,
            runtime_project_trust_record_json(&record, &self.config_layers),
            runtime_string_array_json(&changed_layers),
            runtime_config_apply_event_payload(method, &report)
        ))
    }

    /// Runs the runtime project trust store operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_project_trust_store(&self) -> Result<&ProjectTrustStore> {
        self.project_trust_store
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("runtime project trust store is not configured"))
    }

    /// Runs the runtime project trust store mut operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_project_trust_store_mut(&mut self) -> Result<&mut ProjectTrustStore> {
        self.project_trust_store
            .as_mut()
            .ok_or_else(|| MezError::invalid_state("runtime project trust store is not configured"))
    }

    /// Runs the apply project trust decision to layers operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn apply_project_trust_decision_to_layers(
        &mut self,
        project_root: &Path,
        decision: TrustDecision,
    ) -> Vec<String> {
        let trusted = matches!(decision, TrustDecision::Trusted);
        let mut changed = Vec::new();
        for layer in &mut self.config_layers {
            if layer.scope != ConfigScope::ProjectOverlay {
                continue;
            }
            let Some(path) = layer.path.as_ref() else {
                continue;
            };
            if runtime_path_under_project_root(path, project_root) && layer.trusted != trusted {
                layer.trusted = trusted;
                changed.push(layer.name.clone());
            }
        }
        changed
    }
}
