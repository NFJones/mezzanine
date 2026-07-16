//! Runtime control helpers for approval decisions and project command-rule persistence.
//!
//! This module owns approval decision routing, configured pre-action hooks for
//! approval decisions, approved-action resumption, and command-rule persistence
//! for session, global, and trusted project scopes. The parent control module
//! keeps request dispatch routing while this module keeps approval persistence
//! details out of the main control facade.

use super::*;

impl RuntimeSessionService {
    /// Runs the dispatch runtime approval request operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_approval_request(
        &mut self,
        body: &str,
        request: &crate::control::JsonRpcRequest,
        caller_client_id: &mez_core::ids::ClientId,
    ) -> String {
        let cache_key = if request.method == "approval/decide" {
            let params = request.params.as_deref().unwrap_or("{}");
            let Some(idempotency_key) = runtime_json_string_field(params, "idempotency_key") else {
                return runtime_json_rpc_error(
                    &request.id,
                    crate::error::MezErrorKind::InvalidArgs,
                    "mutating control request requires idempotency_key",
                );
            };
            let cache_key = format!("{caller_client_id}:{idempotency_key}");
            match self.control.idempotency_mut().cached_response(
                &cache_key,
                request.method.as_str(),
                &request.params,
            ) {
                Ok(Some(response)) => return response,
                Ok(None) => Some(cache_key),
                Err(error) => {
                    return runtime_json_rpc_error(&request.id, error.kind(), error.message());
                }
            }
        } else {
            None
        };

        if request.method == "approval/decide" {
            let params = request.params.as_deref().unwrap_or("{}");
            let approval_id = runtime_json_string_field(params, "approval_id")
                .unwrap_or_else(|| "unknown".to_string());
            let decision = runtime_json_string_field(params, "decision")
                .unwrap_or_else(|| "unknown".to_string());
            if let Some(block) = match self.run_configured_pre_action_hooks(
                HookEvent::PermissionDecision,
                &runtime_permission_decision_hook_payload(&approval_id, &decision),
            ) {
                Ok(block) => block,
                Err(error) => {
                    return runtime_json_rpc_error(&request.id, error.kind(), error.message());
                }
            } {
                return runtime_json_rpc_error(
                    &request.id,
                    crate::error::MezErrorKind::Forbidden,
                    &format!(
                        "permission decision blocked by hook `{}`: {}",
                        block.hook_id, block.message
                    ),
                );
            }
        }

        let response = if let Some(audit_log) = self.persistence.audit_log_mut() {
            dispatch_control_request_with_approvals_and_audit(
                body,
                &mut self.session,
                caller_client_id,
                self.integration.blocked_approvals_mut(),
                audit_log,
            )
        } else {
            dispatch_control_request_with_approvals(
                body,
                &mut self.session,
                caller_client_id,
                self.integration.blocked_approvals_mut(),
            )
        };
        if response.contains(r#""result""#) && request.method == "approval/decide" {
            let params = request.params.as_deref().unwrap_or("{}");
            let approval_id = runtime_json_string_field(params, "approval_id")
                .unwrap_or_else(|| "unknown".to_string());
            let decision = runtime_json_string_field(params, "decision")
                .unwrap_or_else(|| "unknown".to_string());
            let decision_kind = runtime_approval_decision_name_to_kind(&decision);
            let requested_scope = match approval_decide_scope_persistence(params) {
                Ok(scope) => scope,
                Err(error) => {
                    return runtime_json_rpc_error(&request.id, error.kind(), error.message());
                }
            };
            let decided_approval = self.blocked_approvals().get(&approval_id).cloned();
            if let Some(rule_decision) = (match decision_kind {
                Some(ApprovalDecision::Approve) => Some(RuleDecision::Allow),
                Some(ApprovalDecision::Disapprove) => Some(RuleDecision::Forbid),
                Some(ApprovalDecision::Redirect) | None => None,
            }) && let Some(approval) = decided_approval.as_ref()
                && approval.action_kind == "shell_command"
                && matches!(
                    requested_scope,
                    Some(
                        ApprovalDecisionScopePersistence::Session
                            | ApprovalDecisionScopePersistence::Project
                            | ApprovalDecisionScopePersistence::Global
                    )
                )
            {
                match self.persist_shell_approval_rule(
                    approval,
                    requested_scope
                        .expect("requested_scope is Some for persisted approval decision"),
                    rule_decision,
                ) {
                    Ok(_) => {}
                    Err(error) => {
                        return runtime_json_rpc_error(&request.id, error.kind(), error.message());
                    }
                }
            }
            let mut resumed_actions = 0usize;
            if matches!(decision_kind, Some(ApprovalDecision::Approve))
                && let Some(approval) = decided_approval.as_ref()
            {
                match self.resume_approved_blocked_agent_action(
                    &approval_id,
                    approval,
                    caller_client_id,
                ) {
                    Ok(Some(count)) => resumed_actions = count,
                    Ok(None) => {}
                    Err(error) => {
                        return runtime_json_rpc_error(&request.id, error.kind(), error.message());
                    }
                }
            }
            if matches!(
                decision_kind,
                Some(ApprovalDecision::Disapprove | ApprovalDecision::Redirect)
            ) && let Some(approval) = decided_approval.as_ref()
            {
                match self.settle_decided_blocked_agent_action(&approval_id, approval) {
                    Ok(Some(count)) => resumed_actions = count,
                    Ok(None) => {}
                    Err(error) => {
                        return runtime_json_rpc_error(&request.id, error.kind(), error.message());
                    }
                }
                if let Err(error) = self
                    .session
                    .select_pane_global(caller_client_id, &approval.pane_id)
                {
                    let error = crate::error::MezError::from(error);
                    return runtime_json_rpc_error(&request.id, error.kind(), error.message());
                }
                if let Err(error) = self.enter_agent_mode_for_pane(&approval.pane_id) {
                    return runtime_json_rpc_error(&request.id, error.kind(), error.message());
                }
            }
            if let Err(error) = self.append_primary_lifecycle_event(
                EventKind::ApprovalChanged,
                format!(
                    r#"{{"approval_id":"{}","decision":"{}","state":"decided","agent_actions_resumed":{}}}"#,
                    json_escape(&approval_id),
                    json_escape(&decision),
                    resumed_actions
                ),
            ) {
                return runtime_json_rpc_error(&request.id, error.kind(), error.message());
            }
        }
        if let Some(cache_key) = cache_key {
            self.control.idempotency_mut().remember_response(
                cache_key,
                request.method.clone(),
                request.params.clone(),
                response.clone(),
            );
        }
        response
    }

    /// Runs the persist shell approval rule operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn persist_shell_approval_rule(
        &mut self,
        approval: &BlockedApprovalRequest,
        persistence: ApprovalDecisionScopePersistence,
        decision: RuleDecision,
    ) -> Result<()> {
        let normalized = normalize_exact_command_text(&approval.action_summary, false);
        let scope = match persistence {
            ApprovalDecisionScopePersistence::Once => return Ok(()),
            ApprovalDecisionScopePersistence::Session => CommandRuleScope::Session,
            ApprovalDecisionScopePersistence::Project => CommandRuleScope::Project,
            ApprovalDecisionScopePersistence::Global => CommandRuleScope::User,
        };
        let rule = CommandRule::new_exact_sha256(
            &normalized,
            DEFAULT_COMMAND_SHELL_CLASSIFICATION,
            decision,
        )?
        .with_scope(scope)
        .with_justification(format!(
            "approval {} for pane {}",
            approval.id, approval.pane_id
        ));
        if matches!(persistence, ApprovalDecisionScopePersistence::Project) {
            self.persist_project_shell_approval_rule(approval, &rule)?;
        } else {
            self.permission_policy_mut().add_rule(rule);
        }
        Ok(())
    }

    /// Runs the persist project shell approval rule operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn persist_project_shell_approval_rule(
        &mut self,
        approval: &BlockedApprovalRequest,
        rule: &CommandRule,
    ) -> Result<()> {
        let project_root = self.project_root_for_approval(approval);
        if let Some(record) = self
            .project_trust_store
            .as_ref()
            .and_then(|store| store.get(&project_root))
        {
            match record.state {
                TrustDecision::Trusted => {}
                TrustDecision::Pending => {
                    return Err(MezError::conflict(
                        "project approval persistence is blocked until project trust is decided",
                    ));
                }
                TrustDecision::Rejected | TrustDecision::Revoked => {
                    return Err(MezError::forbidden(
                        "project approval persistence requires a trusted project root",
                    ));
                }
            }
        }
        let config_path = project_root.join(".mezzanine/config.toml");
        let parent = config_path.parent().ok_or_else(|| {
            MezError::invalid_args(format!(
                "project config target {} has no parent directory",
                config_path.display()
            ))
        })?;
        let text = self.project_config_text_for_update(&config_path)?;
        let updated = append_project_command_rule_text(&text, rule)?;
        if self.persistence.config_uses_adapter() {
            self.persistence.queue_config(RuntimeSideEffect::Persist {
                target: crate::runtime::PersistenceTarget::ProjectConfig,
                path: config_path.clone(),
                bytes: updated.clone().into_bytes(),
                mode: crate::runtime::PersistenceWriteMode::Replace,
            });
        } else {
            fs::create_dir_all(parent)?;
            fs::write(&config_path, updated.clone())?;
        }
        self.upsert_project_config_layer(config_path, updated, project_root)
    }

    /// Runs the project config text for update operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn project_config_text_for_update(&self, config_path: &Path) -> Result<String> {
        if let Some(layer) = self.integration.config_layers().iter().rev().find(|layer| {
            layer
                .path
                .as_ref()
                .is_some_and(|layer_path| paths_equivalent(layer_path, config_path))
        }) {
            return Ok(layer.text.clone());
        }
        match fs::read_to_string(config_path) {
            Ok(text) => Ok(text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(crate::config::DEFAULT_PROJECT_CONFIG_TOML.to_string())
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Runs the project root for approval operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn project_root_for_approval(&self, approval: &BlockedApprovalRequest) -> PathBuf {
        self.pane_current_working_directory(&approval.pane_id)
            .map(|path| discover_project_root(&path))
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .map(|path| discover_project_root(&path))
            })
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// Runs the upsert project config layer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn upsert_project_config_layer(
        &mut self,
        path: PathBuf,
        text: String,
        project_root: PathBuf,
    ) -> Result<()> {
        let trusted = self
            .project_trust_store
            .as_ref()
            .and_then(|store| store.get(&project_root))
            .is_none_or(|record| record.state == TrustDecision::Trusted);
        if let Some(layer) = self
            .integration
            .config_layers_mut()
            .iter_mut()
            .find(|layer| {
                layer
                    .path
                    .as_ref()
                    .is_some_and(|layer_path| paths_equivalent(layer_path, &path))
            })
        {
            layer.format = ConfigFormat::Toml;
            layer.scope = ConfigScope::ProjectOverlay;
            layer.trusted = trusted;
            layer.text = text;
        } else {
            self.integration.config_layers_mut().push(ConfigLayer {
                name: "project".to_string(),
                path: Some(path),
                format: ConfigFormat::Toml,
                scope: ConfigScope::ProjectOverlay,
                trusted,
                text,
            });
        }
        self.apply_runtime_config_layers()?;
        Ok(())
    }
}

/// Runs the append project command rule text operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn append_project_command_rule_text(text: &str, rule: &CommandRule) -> Result<String> {
    let (digest_hex, shell_classification) = match &rule.rule_match {
        RuleMatch::ExactSha256 {
            digest_hex,
            shell_classification,
        } => (digest_hex, shell_classification),
        RuleMatch::Prefix | RuleMatch::Exact => {
            return Err(MezError::invalid_args(
                "project approval persistence requires an exact_sha256 command rule",
            ));
        }
    };
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| MezError::config(format!("invalid project TOML config: {error}")))?;
    if document.as_table().get("version").is_some() {
        document.as_table_mut().insert(
            "version",
            toml_edit::value(crate::config::CURRENT_CONFIG_SCHEMA_VERSION as i64),
        );
    } else {
        let text = if text.trim().is_empty() {
            format!(
                "version = {}\n",
                crate::config::CURRENT_CONFIG_SCHEMA_VERSION
            )
        } else if text.ends_with('\n') {
            format!(
                "version = {}\n{text}",
                crate::config::CURRENT_CONFIG_SCHEMA_VERSION
            )
        } else {
            format!(
                "version = {}\n{text}\n",
                crate::config::CURRENT_CONFIG_SCHEMA_VERSION
            )
        };
        document = text
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| MezError::config(format!("invalid project TOML config: {error}")))?;
    }
    let root = document.as_table_mut();
    if root.get("permissions").is_none() {
        root.insert(
            "permissions",
            toml_edit::Item::Table(toml_edit::Table::new()),
        );
    }
    let permissions = root
        .get_mut("permissions")
        .and_then(toml_edit::Item::as_table_mut)
        .ok_or_else(|| MezError::config("project config permissions must be a table"))?;
    if let Some(item) = permissions.get("command_rules") {
        let replace_empty_array = matches!(item, toml_edit::Item::Value(value) if value.as_array().is_some_and(|array| array.is_empty()));
        if replace_empty_array {
            permissions.remove("command_rules");
        }
    }
    if permissions.get("command_rules").is_none() {
        permissions.insert(
            "command_rules",
            toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()),
        );
    }
    let rules = permissions
        .get_mut("command_rules")
        .and_then(toml_edit::Item::as_array_of_tables_mut)
        .ok_or_else(|| {
            MezError::config("project config permissions.command_rules must be an array of tables")
        })?;
    let mut pattern = toml_edit::Array::default();
    pattern.push(digest_hex.as_str());
    let mut table = toml_edit::Table::new();
    table.insert("pattern", toml_edit::value(pattern));
    table.insert(
        "decision",
        toml_edit::value(project_rule_decision_name(rule.decision)),
    );
    table.insert(
        "scope",
        toml_edit::value(project_rule_scope_name(rule.scope)),
    );
    table.insert("match", toml_edit::value("exact_sha256"));
    table.insert("exact_sha256", toml_edit::value(digest_hex.as_str()));
    table.insert(
        "shell_classification",
        toml_edit::value(shell_classification.as_str()),
    );
    if let Some(justification) = &rule.justification {
        table.insert("justification", toml_edit::value(justification.as_str()));
    }
    rules.push(table);
    let updated = document.to_string();
    let validation =
        validate_config_text(ConfigFormat::Toml, &updated, ConfigScope::ProjectOverlay);
    if !validation.valid {
        let summary = validation
            .diagnostics
            .iter()
            .map(|diagnostic| format!("{}: {}", diagnostic.path, diagnostic.message))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(MezError::config(format!(
            "project command rule persistence produced invalid config: {summary}"
        )));
    }
    Ok(updated)
}

/// Runs the project rule decision name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn project_rule_decision_name(decision: RuleDecision) -> &'static str {
    match decision {
        RuleDecision::Forbid => "deny",
        RuleDecision::Prompt => "prompt",
        RuleDecision::Allow => "allow",
    }
}

/// Runs the project rule scope name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn project_rule_scope_name(scope: CommandRuleScope) -> &'static str {
    match scope {
        CommandRuleScope::BuiltIn => "built-in",
        CommandRuleScope::Session => "session",
        CommandRuleScope::Project => "project",
        CommandRuleScope::User => "user",
        CommandRuleScope::Managed => "managed",
    }
}
