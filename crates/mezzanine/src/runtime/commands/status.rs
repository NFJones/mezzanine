//! Agent status and debug display commands.
//!
//! This module owns read-mostly agent presentation commands and their display
//! formatting helpers: `/status`, terminal-view clearing, and `/debug-config`.
//! Keeping these report builders outside the command facade separates UI/status
//! presentation from turn orchestration and policy mutation.

use super::{
    AGENT_PROMPT_PROFILE_NAME, AGENT_PROMPT_PROFILE_VERSION, AgentShellCommandOutcome, BTreeMap,
    ConfigFormat, ConfigScope, MezError, ModelTokenUsage, ModelTokenUsageKey, Result,
    RuntimeSessionService, agent_shell_visibility_json_name, compose_effective_config,
    current_unix_seconds, json_escape, parse_slash_command, runtime_agent_turn_state_name,
    runtime_approval_policy_name, runtime_cooperation_mode_name, runtime_markdown_table,
    runtime_permission_preset_name,
};
use crate::storage::token_usage::TOKEN_USAGE_WINDOWS_DAYS;
use crate::ui::command::auth_status_store_table_row;

const TOKEN_USAGE_TABLE_COLUMNS: [&str; 7] = [
    "Provider",
    "Model",
    "Billed input",
    "Cached input",
    "Output",
    "Reasoning",
    "Cumulative Cache Hit %",
];

impl RuntimeSessionService {
    /// Executes `/auth-status` against the live authentication store.
    pub(super) fn execute_agent_shell_auth_status_command(
        &self,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let slash = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("auth-status command must be a slash command"))?;
        if !slash.args.trim().is_empty() {
            return Err(MezError::invalid_args(
                "auth-status does not accept arguments",
            ));
        }
        let rows = self
            .provider_registry()
            .providers()
            .keys()
            .map(|provider| match self.auth_store() {
                Some(auth_store) => Ok(auth_status_store_table_row(
                    provider,
                    auth_store.status_for_provider(provider)?,
                )),
                None => Ok(vec![
                    provider.clone(),
                    "unknown".to_string(),
                    "none".to_string(),
                    "unavailable".to_string(),
                    "auth-store-unavailable".to_string(),
                ]),
            })
            .collect::<Result<Vec<_>>>()?;
        let body = runtime_markdown_table(
            &[
                "Provider",
                "Authenticated",
                "Profile",
                "Credential store",
                "State",
            ],
            &rows,
        )
        .join("\n");
        Ok(AgentShellCommandOutcome::Display {
            command: "auth-status".to_string(),
            body: format!("## Authentication Status\n\n{body}"),
        })
    }

    /// Executes `/status` against the live runtime status source.
    pub(super) fn execute_agent_shell_status_command(
        &self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let slash = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("status command must be a slash command"))?;
        let extended = match slash.args.trim() {
            "" => false,
            "--extended" => true,
            _ => {
                return Err(MezError::invalid_args(
                    "status accepts only the optional --extended argument",
                ));
            }
        };
        let body = self.runtime_agent_status_display_with_options(pane_id, extended)?;
        if extended {
            Ok(AgentShellCommandOutcome::Display {
                command: "status".to_string(),
                body,
            })
        } else {
            Ok(AgentShellCommandOutcome::LiveDisplay {
                command: "status".to_string(),
                body,
                source: crate::integrations::agent::slash::AgentShellDisplaySource::AgentStatus {
                    pane_id: pane_id.to_string(),
                },
            })
        }
    }

    /// Executes `/reset-status` against pane-lifetime token accounting only.
    pub(super) fn execute_agent_shell_reset_status_command(
        &mut self,
        pane_id: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let visibility = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| session.visibility)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "agent shell session not found for pane",
                )
            })?;
        let changed = self.reset_agent_token_usage_for_pane(pane_id);
        Ok(AgentShellCommandOutcome::Mutated {
            command: "reset-status".to_string(),
            body: format!("pane_token_usage_reset=true changed={changed}"),
            visibility,
        })
    }

    /// Builds the live `/status` display from runtime session state.
    #[cfg(test)]
    pub(crate) fn runtime_agent_status_display(&self, pane_id: &str) -> Result<String> {
        self.runtime_agent_status_display_with_options(pane_id, false)
    }

    /// Builds the live status display, optionally including durable rolling
    /// token-accounting windows.
    pub(crate) fn runtime_agent_status_display_with_options(
        &self,
        pane_id: &str,
        extended: bool,
    ) -> Result<String> {
        let session = self.agent_shell_store().get(pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "agent shell session not found for pane",
            )
        })?;
        let agent_id = format!("agent-{pane_id}");
        let descriptor = self.find_pane_descriptor(pane_id);
        let window_id = descriptor
            .as_ref()
            .map(|descriptor| descriptor.window_id.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let current_working_directory = self
            .pane_current_working_directory(pane_id)
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let (model_profile_name, model_profile) =
            self.active_model_profile_for_pane(pane_id, &agent_id, None)?;
        let active_scopes = self.active_subagent_write_scopes_for(&agent_id);
        let writable_roots = active_scopes
            .iter()
            .map(|scope| scope.scope.clone())
            .collect::<Vec<_>>();
        let latest_turn = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .rev()
            .find(|turn| turn.pane_id == pane_id);
        let latest_turn_id = latest_turn
            .map(|turn| turn.turn_id.as_str())
            .unwrap_or("none");
        let latest_turn_state = latest_turn
            .map(|turn| runtime_agent_turn_state_name(turn.state))
            .unwrap_or("none");
        let context_blocks = latest_turn
            .and_then(|turn| self.agent_turn_contexts().get(&turn.turn_id))
            .map(|context| context.blocks().len())
            .unwrap_or(0);
        let request_messages = latest_turn
            .and_then(|turn| self.agent_turn_executions().get(&turn.turn_id))
            .map(|execution| execution.request.messages.len())
            .unwrap_or(0);
        let token_usage_by_model = self.agent_token_usage_for_pane(pane_id);
        let latest_request_usage = self.agent_latest_request_usage(&session.session_id);
        let context_continuity = self.agent_context_continuity(&session.session_id);
        let instance_token_usage_by_model =
            self.runtime_agent_instance_provider_token_usage_by_model();
        let running_turn = session
            .running_turn_id
            .as_deref()
            .unwrap_or("none")
            .to_string();
        let reasoning_profile = model_profile
            .reasoning_profile
            .as_deref()
            .unwrap_or("none")
            .to_string();
        let thinking = self
            .model_profile_thinking_enabled(&model_profile)
            .map(|enabled| if enabled { "enabled" } else { "disabled" })
            .unwrap_or("unsupported");
        let permission_status = self.permission_policy_status_for_pane(pane_id);
        let permission_policy = &permission_status.policy;
        let preset_owner = permission_status
            .preset_source
            .owner_pane_id
            .as_deref()
            .unwrap_or("none");
        let approval_owner = permission_status
            .approval_policy_source
            .owner_pane_id
            .as_deref()
            .unwrap_or("none");
        let rows = vec![
            vec!["Pane".to_string(), session.pane_id.clone()],
            vec!["Session".to_string(), session.session_id.clone()],
            vec![
                "Visibility".to_string(),
                agent_shell_visibility_json_name(session.visibility).to_string(),
            ],
            vec!["Running turn".to_string(), running_turn],
            vec![
                "Transcript entries".to_string(),
                session.transcript_entries.to_string(),
            ],
            vec![
                "Directive".to_string(),
                session
                    .directive
                    .clone()
                    .unwrap_or_else(|| "none".to_string()),
            ],
            vec![
                "Log level".to_string(),
                session.log_level.as_str().to_string(),
            ],
            vec!["Agent id".to_string(), agent_id],
            vec!["Window id".to_string(), window_id],
            vec!["Current directory".to_string(), current_working_directory],
            vec![
                "Model".to_string(),
                format!(
                    "{} via {} (profile: {}, reasoning: {})",
                    model_profile.model,
                    model_profile.provider,
                    model_profile_name,
                    reasoning_profile
                ),
            ],
            vec!["Thinking".to_string(), thinking.to_string()],
            vec![
                "Prompt profile".to_string(),
                format!("{AGENT_PROMPT_PROFILE_NAME} v{AGENT_PROMPT_PROFILE_VERSION}"),
            ],
            vec![
                "Permissions".to_string(),
                format!(
                    "preset {} ({}; owner {}), approval {} ({}; owner {}), bypass {} (session)",
                    runtime_permission_preset_name(permission_policy.preset),
                    permission_status.preset_source.source,
                    preset_owner,
                    runtime_approval_policy_name(permission_policy.approval_policy),
                    permission_status.approval_policy_source.source,
                    approval_owner,
                    permission_policy.approval_bypass()
                ),
            ],
            vec![
                "Command rules".to_string(),
                permission_policy.rules().len().to_string(),
            ],
            vec![
                "Writable roots".to_string(),
                format!(
                    "{} ({})",
                    if writable_roots.is_empty() {
                        "none".to_string()
                    } else {
                        writable_roots.join(", ")
                    },
                    writable_roots.len()
                ),
            ],
            vec![
                "Active write scopes".to_string(),
                self.active_subagent_write_scope_count().to_string(),
            ],
            vec![
                "Context".to_string(),
                format!(
                    "{context_blocks} blocks, {request_messages} request messages, window={} tokens, compaction={}",
                    model_profile.context_window_tokens(),
                    if model_profile.max_input_tokens().is_some() {
                        "configured-input-limit/provider-rejection/manual"
                    } else {
                        "provider-rejection/manual"
                    }
                ),
            ],
            vec![
                "Pane agent tokens".to_string(),
                Self::runtime_agent_provider_token_usage_summary(&token_usage_by_model),
            ],
            vec![
                "Cumulative cache hit".to_string(),
                Self::runtime_agent_cumulative_cache_hit_display(&token_usage_by_model),
            ],
            vec![
                "Latest request cache hit".to_string(),
                latest_request_usage.map_or_else(
                    || "unknown".to_string(),
                    |sample| {
                        format!(
                            "{} ({}; cached_input={} input={})",
                            sample.usage.cached_input_hit_ratio_display(),
                            sample.model.display_name(),
                            sample.usage.cached_input_tokens_display(),
                            sample.usage.input_tokens,
                        )
                    },
                ),
            ],
            vec![
                "Context continuity".to_string(),
                context_continuity.map_or_else(
                    || "unknown".to_string(),
                    |diagnostics| {
                        format!(
                            "reason={} immutable_tokens~{} volatile_tokens~{} append_only={}",
                            diagnostics.break_reason.as_str(),
                            diagnostics.snapshot.immutable_token_estimate,
                            diagnostics.snapshot.volatile_token_estimate,
                            diagnostics.immutable_append_only,
                        )
                    },
                ),
            ],
            vec![
                "Immutable projection".to_string(),
                context_continuity.map_or_else(
                    || "unknown".to_string(),
                    |diagnostics| {
                        format!(
                            "bytes={} sha256={}",
                            diagnostics.snapshot.stable_projection_bytes,
                            diagnostics.snapshot.stable_projection_sha256,
                        )
                    },
                ),
            ],
            vec![
                "Common immutable prefix".to_string(),
                context_continuity.map_or_else(
                    || "unknown".to_string(),
                    |diagnostics| {
                        format!(
                            "blocks={} tokens~{}",
                            diagnostics.common_immutable_prefix_blocks,
                            diagnostics.common_immutable_prefix_tokens,
                        )
                    },
                ),
            ],
            vec![
                "Latest turn".to_string(),
                format!("{latest_turn_id} ({latest_turn_state})"),
            ],
        ];
        let mut lines = vec!["## Agent Status".to_string(), String::new()];
        lines.extend(runtime_markdown_table(&["Field", "Value"], &rows));
        if !token_usage_by_model.is_empty() {
            lines.push(String::new());
            lines.push("### Pane Agent Token Usage".to_string());
            lines.push(String::new());
            lines.extend(runtime_markdown_table(
                &TOKEN_USAGE_TABLE_COLUMNS,
                &Self::runtime_agent_provider_token_usage_rows(&token_usage_by_model),
            ));
        }
        if !instance_token_usage_by_model.is_empty() {
            lines.push(String::new());
            lines.push("### Mez Session Token Usage".to_string());
            lines.push(String::new());
            lines.extend(runtime_markdown_table(
                &TOKEN_USAGE_TABLE_COLUMNS,
                &Self::runtime_agent_provider_token_usage_rows(&instance_token_usage_by_model),
            ));
        }
        if extended {
            self.append_extended_token_usage(&mut lines);
        }
        if !active_scopes.is_empty() {
            let scope_rows = active_scopes
                .into_iter()
                .map(|scope| {
                    vec![
                        scope.scope,
                        scope.agent_id,
                        runtime_cooperation_mode_name(scope.mode).to_string(),
                        scope.serial_lock.unwrap_or_else(|| "none".to_string()),
                    ]
                })
                .collect::<Vec<_>>();
            lines.push(String::new());
            lines.push("### Writable Roots".to_string());
            lines.extend(runtime_markdown_table(
                &["Root", "Owner", "Mode", "Serial lock"],
                &scope_rows,
            ));
        }
        Ok(lines.join("\n"))
    }

    /// Appends durable rolling token-accounting sections or one bounded
    /// degradation diagnostic when attached storage is not trustworthy.
    fn append_extended_token_usage(&self, lines: &mut Vec<String>) {
        if let Some(message) = self.persistence.token_usage_health_error() {
            lines.push(String::new());
            lines.push("### Rolling Token Usage Unavailable".to_string());
            lines.push(String::new());
            lines.push(message);
            return;
        }
        let Some(store) = self.persistence.token_usage_store() else {
            return;
        };
        let now = current_unix_seconds();
        let oldest_observed_at = match store.oldest_observed_at(now) {
            Ok(oldest_observed_at) => oldest_observed_at,
            Err(_) => {
                let message =
                    "persistent token accounting is degraded after a storage query failure";
                self.persistence.set_token_usage_health_error(message);
                lines.push(String::new());
                lines.push("### Rolling Token Usage Unavailable".to_string());
                lines.push(String::new());
                lines.push(message.to_string());
                return;
            }
        };
        let Some(oldest_observed_at) = oldest_observed_at else {
            return;
        };
        let visible_windows = TOKEN_USAGE_WINDOWS_DAYS
            .iter()
            .copied()
            .position(|days| {
                oldest_observed_at >= now.saturating_sub(u64::from(days).saturating_mul(86_400))
            })
            .map(|index| TOKEN_USAGE_WINDOWS_DAYS[..=index].to_vec())
            .unwrap_or_else(|| TOKEN_USAGE_WINDOWS_DAYS.to_vec());
        let windows = match store.aggregate_windows(now, &visible_windows) {
            Ok(windows) => windows,
            Err(_) => {
                let message =
                    "persistent token accounting is degraded after a storage query failure";
                self.persistence.set_token_usage_health_error(message);
                lines.push(String::new());
                lines.push("### Rolling Token Usage Unavailable".to_string());
                lines.push(String::new());
                lines.push(message.to_string());
                return;
            }
        };
        for days in visible_windows {
            lines.push(String::new());
            lines.push(format!("### {days}-Day Token Usage"));
            lines.push(String::new());
            let rows = windows
                .get(&days)
                .map(Self::runtime_agent_provider_token_usage_rows)
                .unwrap_or_default();
            lines.extend(runtime_markdown_table(&TOKEN_USAGE_TABLE_COLUMNS, &rows));
        }
    }

    /// Returns the compact `/status` summary for per-model provider tokens.
    fn runtime_agent_provider_token_usage_summary(
        usage_by_model: &BTreeMap<ModelTokenUsageKey, ModelTokenUsage>,
    ) -> String {
        match usage_by_model.len() {
            0 => "none".to_string(),
            1 => usage_by_model
                .iter()
                .next()
                .map(|(key, usage)| {
                    format!(
                        "{}: {}",
                        key.display_name(),
                        Self::runtime_agent_provider_token_usage_metrics(*usage)
                    )
                })
                .unwrap_or_else(|| "none".to_string()),
            count => format!("{count} models; see Pane Agent Token Usage"),
        }
    }

    /// Returns the explicitly cumulative cache-hit ratio across pane samples.
    fn runtime_agent_cumulative_cache_hit_display(
        usage_by_model: &BTreeMap<ModelTokenUsageKey, ModelTokenUsage>,
    ) -> String {
        let mut cumulative = ModelTokenUsage::default();
        for usage in usage_by_model.values() {
            cumulative.add_assign(*usage);
        }
        if usage_by_model.is_empty() {
            "unknown".to_string()
        } else {
            cumulative.cached_input_hit_ratio_display()
        }
    }

    /// Aggregates provider/model token accounting across retained conversations.
    fn runtime_agent_instance_provider_token_usage_by_model(
        &self,
    ) -> BTreeMap<ModelTokenUsageKey, ModelTokenUsage> {
        self.total_agent_token_usage_by_model()
    }

    /// Builds markdown table rows for per-model provider token accounting.
    fn runtime_agent_provider_token_usage_rows(
        usage_by_model: &BTreeMap<ModelTokenUsageKey, ModelTokenUsage>,
    ) -> Vec<Vec<String>> {
        usage_by_model
            .iter()
            .map(|(key, usage)| {
                vec![
                    key.provider.clone(),
                    key.model.clone(),
                    usage.billed_input_tokens().to_string(),
                    usage.cached_input_tokens_display(),
                    usage.output_tokens.to_string(),
                    usage.reasoning_tokens.to_string(),
                    usage.cached_input_hit_ratio_display(),
                ]
            })
            .collect()
    }

    /// Formats one provider/model token usage value for compact displays.
    fn runtime_agent_provider_token_usage_metrics(usage: ModelTokenUsage) -> String {
        format!(
            "input={} cached_input={} cumulative_cache_hit={} output={} reasoning={} total={}",
            usage.billed_input_tokens(),
            usage.cached_input_tokens_display(),
            usage.cached_input_hit_ratio_display(),
            usage.output_tokens,
            usage.reasoning_tokens,
            usage.total_tokens()
        )
    }

    /// Moves the current terminal view into history and clears the viewport.
    pub(crate) fn clear_agent_shell_terminal_view(&mut self, pane_id: &str) -> Result<bool> {
        self.clear_copy_state_for_surface(pane_id, crate::runtime::PaneSurfaceKind::Agent);
        let Some(conversation_id) = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| session.session_id.clone())
        else {
            return Ok(false);
        };
        let Some(size) = self
            .agent_pane_screen(pane_id)
            .or_else(|| self.process_pane_screen(pane_id))
            .map(|screen| screen.size())
        else {
            return Ok(false);
        };
        let screen = self.ensure_agent_pane_screen(pane_id, &conversation_id, size)?;
        screen.clear_visible_into_history();
        Ok(true)
    }

    /// Runs the execute agent shell debug config command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn execute_agent_shell_debug_config_command(
        &self,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?.ok_or_else(|| {
            MezError::invalid_args("debug-config command must be a slash command")
        })?;
        let filter = invocation.args.split_whitespace().next();
        Ok(AgentShellCommandOutcome::Display {
            command: "debug-config".to_string(),
            body: self.runtime_debug_config_display(filter)?,
        })
    }

    /// Builds the live `/debug-config` display from effective runtime config state.
    pub(super) fn runtime_debug_config_display(&self, filter: Option<&str>) -> Result<String> {
        let effective = compose_effective_config(self.integration.config_layers())?;
        let mut lines = vec![format!(
            "layers={} applied_layers={} skipped_layers={} values={} diagnostics={} permission_preset={} approval_policy={} bypass={} providers={} model_profiles={} mcp_servers={} hooks={} source=runtime-config",
            self.integration.config_layers().len(),
            effective.applied_layers().len(),
            effective.skipped_layers().len(),
            effective.values().len(),
            effective.diagnostics().len(),
            runtime_permission_preset_name(self.permission_policy().preset),
            runtime_approval_policy_name(self.permission_policy().approval_policy),
            self.permission_policy().approval_bypass(),
            self.provider_registry().providers.len(),
            self.provider_registry().profiles.len(),
            self.mcp_registry().list_servers().len(),
            self.integration.hook_definitions().len()
        )];
        for (index, layer) in self.integration.config_layers().iter().enumerate() {
            lines.push(format!(
                "layer={} index={} scope={} trusted={} applied={} skipped={} format={} path={}",
                json_escape(&layer.name),
                index,
                Self::runtime_config_scope_name(layer.scope),
                layer.trusted,
                effective.applied_layers().contains(&layer.name),
                effective.skipped_layers().contains(&layer.name),
                Self::runtime_config_format_name(layer.format),
                layer
                    .path
                    .as_ref()
                    .map(|path| json_escape(&path.to_string_lossy()))
                    .unwrap_or_else(|| "inline".to_string())
            ));
        }
        for diagnostic in effective.diagnostics() {
            lines.push(format!(
                "diagnostic path={} message={}",
                json_escape(&diagnostic.path),
                json_escape(&diagnostic.message)
            ));
        }
        for (path, value) in effective.values() {
            if filter.is_some_and(|filter| filter != path) {
                continue;
            }
            lines.push(format!(
                "value path={} source={} value={}",
                json_escape(path),
                json_escape(&value.source_layer),
                json_escape(&value.value)
            ));
        }
        Ok(lines.join("\n"))
    }

    /// Runs the runtime config scope name operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_config_scope_name(scope: ConfigScope) -> &'static str {
        match scope {
            ConfigScope::Primary => "primary",
            ConfigScope::ProjectOverlay => "project-overlay",
            ConfigScope::LiveOverride => "live-override",
        }
    }

    /// Runs the runtime config format name operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_config_format_name(format: ConfigFormat) -> &'static str {
        match format {
            ConfigFormat::Toml => "toml",
            ConfigFormat::Yaml => "yaml",
            ConfigFormat::Json => "json",
        }
    }
}
