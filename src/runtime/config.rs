//! Runtime Config implementation.
//!
//! This module owns the runtime config boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    ActionResult, AgentAction, AgentId, AgentTurnRecord, ApprovalDecision, ApprovalPolicy,
    AuditConfig, AuditLog, AuditRetentionPolicy, BTreeMap, BlockedApprovalRequest, CommandRule,
    CommandRuleScope, ConfigDiagnostic, ConfigFormat, ConfigLayer, ConfigScope,
    DEFAULT_AGENT_ACTION_FAILURE_RETRY_LIMIT, DEFAULT_AGENT_COMPACTION_RAW_RETENTION_PERCENT,
    DEFAULT_AGENT_IMPLEMENTATION_PRESSURE_AFTER_SHELL_ACTIONS, DEFAULT_AGENT_LOOP_LIMIT,
    DEFAULT_AGENT_ROUTING, DEFAULT_AUTO_SIZING_FALLBACK_POLICY,
    DEFAULT_COMMAND_SHELL_CLASSIFICATION, DEFAULT_HISTORY_LIMIT, DEFAULT_HISTORY_ROTATE_LINES,
    DEFAULT_MAX_CONCURRENT_AGENTS, DEFAULT_MAX_ROOT_SUBAGENTS, DEFAULT_MAX_SUBAGENT_DEPTH,
    DEFAULT_MAX_SUBAGENT_PANES_PER_WINDOW, DEFAULT_MAX_SUBAGENTS_PER_SUBAGENT, DEFAULT_PANE_TERM,
    DEFAULT_SUBAGENT_WAIT_POLICY, DEFAULT_UI_THEME_NAME, EffectiveConfig, HookDefinition,
    HookEvent, HookInvocation, HookMatcherGroup, HookMatcherOperator, HookMatcherPredicate,
    HostClipboard, HostClipboardCommand, KeyBindings, KeyChord, MarkerToken, McpRegistry, MezError,
    ModelProfile, PaneId, Path, PathBuf, PermissionPolicy, PermissionPreset, ProjectTrustRecord,
    Recipient, Result, RuleDecision, RuleMatch, RuntimeAgentPersonalityProfile,
    RuntimeAutoSizingConfig, RuntimeAutoSizingFallbackPolicy, RuntimeCommandBinding,
    RuntimeConfigApplyReport, RuntimeModelPreset, RuntimeModelProfileOverrideScope,
    RuntimePresetRegistry, RuntimeProviderConfig, RuntimeProviderRegistry, RuntimeSessionService,
    SubagentProfile, SubagentScopeDeclaration, SubagentWaitPolicy, TerminalCursorStyle,
    TrustDecision, UiTheme, UiThemeDefinition, Value, WindowId, builtin_subagent_profiles,
    builtin_ui_theme_definition, ensure_absolute, exact_command_sha256, fs, key_chord_notation,
    optional_path_json, optional_string_json, parse_command_sequence, resolve_ui_theme,
    runtime_cooperation_mode, runtime_cooperation_mode_name, runtime_json_string_field,
    runtime_json_value, unix_seconds_to_rfc3339, valid_color_alias_name, validate_config_text,
};
use crate::terminal::TerminalEmojiWidth;
use crate::transcript::DEFAULT_SAVED_AGENT_SESSION_LIMIT;

mod agents;
mod audit;
mod effective;
mod frames;
mod mcp;
mod providers;
mod terminal_options;
mod theme;
pub(super) use agents::{
    runtime_agent_action_failure_retry_limit_from_config, runtime_agent_auto_sizing_from_config,
    runtime_agent_compaction_raw_retention_percent_from_config,
    runtime_agent_custom_system_prompt_from_config,
    runtime_agent_implementation_pressure_after_shell_actions_from_config,
    runtime_agent_loop_limit_from_config, runtime_agent_personality_profiles_from_config,
    runtime_agent_routing_from_config, runtime_default_agent_personality_from_config,
    runtime_max_concurrent_agents_from_config, runtime_max_root_subagents_from_config,
    runtime_max_subagent_depth_from_config, runtime_max_subagent_panes_per_window_from_config,
    runtime_max_subagents_per_subagent_from_config, runtime_subagent_profiles_from_config,
    runtime_subagent_wait_policy_from_config,
};
pub(super) use audit::{runtime_audit_config_present, runtime_audit_log_from_config};
pub use effective::runtime_effective_config_value;
pub(super) use frames::{
    runtime_command_bindings_from_effective, runtime_key_bindings_from_config,
    runtime_pane_frame_position_from_config, runtime_pane_frame_style_from_config,
    runtime_pane_frame_template_from_config, runtime_pane_frame_visible_fields_from_config,
    runtime_pane_frames_enabled_from_config, runtime_window_frame_position_from_config,
    runtime_window_frame_right_status_template_from_config, runtime_window_frame_style_from_config,
    runtime_window_frame_template_from_config, runtime_window_frame_visible_fields_from_config,
    runtime_window_frames_enabled_from_config,
};
pub(super) use mcp::runtime_mcp_registry_from_config;
pub(crate) use providers::runtime_default_models_for_provider;
pub(super) use providers::{
    runtime_preset_registry_from_config, runtime_provider_registry_from_config,
    runtime_recommended_model_for_provider,
};
pub(super) use terminal_options::{
    runtime_history_limit_from_config, runtime_history_rotate_lines_from_config,
    runtime_host_clipboard_from_config, runtime_saved_agent_session_limit_from_config,
    runtime_terminal_clipboard_from_config, runtime_terminal_cursor_blink_from_config,
    runtime_terminal_cursor_blink_interval_ms_from_config,
    runtime_terminal_cursor_style_from_config, runtime_terminal_emoji_width_from_config,
    runtime_terminal_reduced_motion_from_config,
    runtime_terminal_render_rate_limit_fps_from_config,
    runtime_terminal_resize_debounce_ms_from_config,
    runtime_terminal_shell_output_preview_lines_from_config, runtime_terminal_term_from_config,
};
pub use theme::runtime_ui_theme_from_config;

// Runtime config parsing and project trust helpers.

/// Runs the json escape operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn json_escape(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch if ch.is_control() => escaped.push(' '),
            _ => escaped.push(ch),
        }
    }
    escaped
}

/// Runs the runtime fit status line operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_fit_status_line(value: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let mut output = String::new();
    let mut used = 0usize;
    for grapheme in crate::terminal::terminal_graphemes(value) {
        let grapheme_width = crate::terminal::terminal_grapheme_width(grapheme);
        if used.saturating_add(grapheme_width) > width {
            break;
        }
        output.push_str(grapheme);
        used = used.saturating_add(grapheme_width);
    }
    if used < width {
        output.push_str(&" ".repeat(width.saturating_sub(used)));
    }
    output
}

/// Runs the runtime config method applies to live service operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_config_method_applies_to_live_service(method: &str) -> bool {
    matches!(method, "config/set" | "config/unset" | "config/reload")
}

/// Runs the runtime config apply event payload operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_config_apply_event_payload(
    method: &str,
    report: &RuntimeConfigApplyReport,
) -> String {
    format!(
        r#"{{"method":"{}","applied_layers":{},"skipped_layers":{},"terminal_history_limit":{},"terminal_history_rotate_lines":{},"terminal_term":"{}","ui_theme":"{}","window_frames_enabled":{},"pane_frames_enabled":{},"max_concurrent_agents":{},"permission_policy_applied":{},"mcp_servers_configured":{},"mcp_servers_blacklisted":{},"project_trust_prompts_announced":{}}}"#,
        json_escape(method),
        runtime_string_array_json(&report.applied_layers),
        runtime_string_array_json(&report.skipped_layers),
        report.terminal_history_limit,
        report.terminal_history_rotate_lines,
        json_escape(&report.terminal_term),
        json_escape(&report.ui_theme),
        report.window_frames_enabled,
        report.pane_frames_enabled,
        report.max_concurrent_agents,
        report.permission_policy_applied,
        report.mcp_servers_configured,
        runtime_string_array_json(&report.mcp_servers_blacklisted),
        report.project_trust_prompts_announced
    )
}

/// Runs the runtime project root param operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_project_root_param(params: &str, method: &str) -> Result<PathBuf> {
    let value = runtime_json_value(params)?;
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args(format!("{method} requires a params object")))?;
    let root = object
        .get("project_root")
        .and_then(Value::as_str)
        .ok_or_else(|| MezError::invalid_args(format!("{method} requires project_root")))?;
    let root = PathBuf::from(root);
    ensure_absolute(&root)?;
    Ok(fs::canonicalize(&root).unwrap_or(root))
}

/// Runs the runtime trust decision param operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_trust_decision_param(params: &str) -> Result<TrustDecision> {
    match runtime_json_string_field(params, "decision").as_deref() {
        Some("trust" | "trusted") => Ok(TrustDecision::Trusted),
        Some("reject" | "rejected") => Ok(TrustDecision::Rejected),
        Some("revoke" | "revoked") => Ok(TrustDecision::Revoked),
        _ => Err(MezError::invalid_args(
            "project/trust/decide requires decision trust or reject",
        )),
    }
}

/// Runs the runtime path under project root operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_path_under_project_root(path: &Path, project_root: &Path) -> bool {
    let canonical_root =
        fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf());
    let canonical_path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    canonical_path.starts_with(canonical_root)
}

/// Runs the runtime project trust record json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_project_trust_record_json(
    record: &ProjectTrustRecord,
    layers: &[ConfigLayer],
) -> String {
    let overlay_layers = runtime_project_overlay_layers(record, layers);
    let overlays = runtime_project_overlay_files_json(&overlay_layers);
    let capability_summary = runtime_project_overlay_capability_summary_json(&overlay_layers);
    let trusted_at = if record.trusted_at_unix_seconds == 0 {
        "null".to_string()
    } else {
        format!(
            r#""{}""#,
            unix_seconds_to_rfc3339(record.trusted_at_unix_seconds)
        )
    };
    let rejected_at = if matches!(record.state, TrustDecision::Rejected) {
        trusted_at.clone()
    } else {
        "null".to_string()
    };
    let revoked_at = if matches!(record.state, TrustDecision::Revoked) {
        trusted_at.clone()
    } else {
        "null".to_string()
    };
    format!(
        r#"{{"id":"{}","version":1,"project_root":"{}","state":"{}","git_marker_path":{},"trusted_at":{},"rejected_at":{},"revoked_at":{},"decided_by_client_id":{},"trust_policy_version":{},"configuration_schema_version":{},"overlay_files":{},"capability_expansion_summary":{},"diagnostics":[]}}"#,
        json_escape(&record.project_root.to_string_lossy()),
        json_escape(&record.project_root.to_string_lossy()),
        runtime_trust_decision_name(record.state),
        optional_path_json(record.git_marker_path.as_deref()),
        if matches!(record.state, TrustDecision::Trusted) {
            trusted_at.as_str()
        } else {
            "null"
        },
        rejected_at,
        revoked_at,
        optional_string_json(record.decided_by_client_id.as_deref()),
        record.trust_policy_version,
        record.configuration_schema_version,
        overlays,
        capability_summary
    )
}

/// Runs the runtime project overlay layers operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_project_overlay_layers<'a>(
    record: &ProjectTrustRecord,
    layers: &'a [ConfigLayer],
) -> Vec<&'a ConfigLayer> {
    layers
        .iter()
        .filter(|layer| layer.scope == ConfigScope::ProjectOverlay)
        .filter(|layer| {
            layer
                .path
                .as_ref()
                .is_some_and(|path| runtime_path_under_project_root(path, &record.project_root))
        })
        .collect()
}

/// Runs the runtime project overlay files json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_project_overlay_files_json(layers: &[&ConfigLayer]) -> String {
    let files = layers
        .iter()
        .map(|layer| {
            let diagnostics =
                validate_config_text(layer.format, &layer.text, layer.scope).diagnostics;
            let applied = layer.trusted && diagnostics.is_empty();
            format!(
                r#"{{"path":"{}","format":"{}","applied":{},"diagnostics":{}}}"#,
                json_escape(
                    &layer
                        .path
                        .as_ref()
                        .map(|path| path.to_string_lossy().to_string())
                        .unwrap_or_else(|| layer.name.clone())
                ),
                runtime_config_format_name(layer.format),
                applied,
                runtime_config_diagnostics_json(&diagnostics)
            )
        })
        .collect::<Vec<_>>();
    format!("[{}]", files.join(","))
}

/// Runs the runtime project overlay capability summary json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_project_overlay_capability_summary_json(layers: &[&ConfigLayer]) -> String {
    let mut capabilities = Vec::new();
    for layer in layers {
        let lower = layer.text.to_ascii_lowercase();
        push_capability_if(
            &mut capabilities,
            lower.contains("[hooks") || lower.contains("hooks:") || lower.contains("\"hooks\""),
            "hooks",
        );
        push_capability_if(
            &mut capabilities,
            lower.contains("[mcp_servers")
                || lower.contains("mcp_servers:")
                || lower.contains("\"mcp_servers\""),
            "mcp_servers",
        );
        push_capability_if(
            &mut capabilities,
            lower.contains("command_rules")
                || lower.contains("global_command_rules")
                || lower.contains("\"command_rules\""),
            "command_rules",
        );
        push_capability_if(
            &mut capabilities,
            lower.contains("[providers")
                || lower.contains("providers:")
                || lower.contains("\"providers\""),
            "providers",
        );
        push_capability_if(
            &mut capabilities,
            lower.contains("[permissions")
                || lower.contains("permissions:")
                || lower.contains("\"permissions\""),
            "permissions",
        );
    }
    capabilities.sort();
    capabilities.dedup();
    let capabilities = capabilities
        .iter()
        .map(|capability| format!(r#""{}""#, json_escape(capability)))
        .collect::<Vec<_>>();
    format!("[{}]", capabilities.join(","))
}

/// Runs the push capability if operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn push_capability_if(capabilities: &mut Vec<String>, enabled: bool, capability: &str) {
    if enabled {
        capabilities.push(capability.to_string());
    }
}

/// Runs the runtime config diagnostics json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_config_diagnostics_json(diagnostics: &[ConfigDiagnostic]) -> String {
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

/// Runs the runtime config format name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_config_format_name(format: ConfigFormat) -> &'static str {
    match format {
        ConfigFormat::Toml => "toml",
        ConfigFormat::Yaml => "yaml",
        ConfigFormat::Json => "json",
    }
}

/// Runs the runtime trust decision name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_trust_decision_name(decision: TrustDecision) -> &'static str {
    match decision {
        TrustDecision::Pending => "pending",
        TrustDecision::Trusted => "trusted",
        TrustDecision::Rejected => "rejected",
        TrustDecision::Revoked => "revoked",
    }
}

/// Runs the runtime approval decision name to kind operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_approval_decision_name_to_kind(value: &str) -> Option<ApprovalDecision> {
    match value {
        "approve" | "allow" => Some(ApprovalDecision::Approve),
        "disapprove" | "deny" | "reject" => Some(ApprovalDecision::Disapprove),
        "redirect" => Some(ApprovalDecision::Redirect),
        _ => None,
    }
}

/// Runs the runtime marker for action operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_marker_for_action(
    turn: &AgentTurnRecord,
    action_id: &str,
) -> Result<MarkerToken> {
    let material = format!(
        "{}\0{}\0{}\0{}",
        turn.turn_id, turn.agent_id, turn.pane_id, action_id
    );
    runtime_random_marker_token(&material)
}

/// Runs the runtime random marker token operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_random_marker_token(material: &str) -> Result<MarkerToken> {
    let mut random = [0u8; 32];
    {
        use std::io::Read as _;
        let mut source = fs::File::open("/dev/urandom").map_err(|error| {
            MezError::invalid_state(format!("failed to open marker entropy source: {error}"))
        })?;
        source.read_exact(&mut random).map_err(|error| {
            MezError::invalid_state(format!("failed to read marker entropy: {error}"))
        })?;
    }
    let mut token = String::with_capacity(96);
    for byte in random {
        let _ = std::fmt::Write::write_fmt(&mut token, format_args!("{byte:02x}"));
    }
    token.push_str(&exact_command_sha256(DEFAULT_COMMAND_SHELL_CLASSIFICATION, material)[..32]);
    MarkerToken::new(token)
}

/// Runs the runtime pre shell hook payload operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_pre_shell_hook_payload(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    command: &str,
) -> String {
    format!(
        r#"{{"turn_id":"{}","agent_id":"{}","pane_id":"{}","action_id":"{}","action_type":"shell_command","command":"{}","command_sha256":"{}"}}"#,
        json_escape(&turn.turn_id),
        json_escape(&turn.agent_id),
        json_escape(&turn.pane_id),
        json_escape(&action.id),
        json_escape(command),
        exact_command_sha256(DEFAULT_COMMAND_SHELL_CLASSIFICATION, command)
    )
}

/// Runs the runtime post shell hook payload operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_post_shell_hook_payload(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    result: &ActionResult,
    exit_code: i32,
) -> String {
    format!(
        r#"{{"turn_id":"{}","agent_id":"{}","pane_id":"{}","action_id":"{}","action_type":"shell_command","status":"{:?}","is_error":{},"exit_code":{}}}"#,
        json_escape(&turn.turn_id),
        json_escape(&turn.agent_id),
        json_escape(&turn.pane_id),
        json_escape(&action.id),
        result.status,
        result.is_error,
        exit_code
    )
}

/// Runs the runtime hook target pane id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_hook_target_pane_id(event_payload_json: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(event_payload_json).ok()?;
    value
        .get("pane_id")
        .and_then(Value::as_str)
        .or_else(|| value.pointer("/turn/pane_id").and_then(Value::as_str))
        .or_else(|| value.pointer("/pane/id").and_then(Value::as_str))
        .filter(|pane_id| !pane_id.is_empty())
        .map(ToOwned::to_owned)
}

/// Runs the runtime user prompt hook payload operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_user_prompt_hook_payload(pane_id: &str, prompt: &str) -> String {
    format!(
        r#"{{"pane_id":"{}","prompt_bytes":{},"prompt_sha256":"{}"}}"#,
        json_escape(pane_id),
        prompt.len(),
        exact_command_sha256(DEFAULT_COMMAND_SHELL_CLASSIFICATION, prompt)
    )
}

/// Carries Runtime Model Command Args state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct RuntimeModelCommandArgs {
    /// Stores the profile value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) profile: Option<String>,
    /// Stores the reasoning profile value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) reasoning_profile: Option<String>,
    /// Stores the scope value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) scope: Option<String>,
    /// Stores the target value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) target: Option<String>,
    /// Stores the clear value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) clear: bool,
    /// Stores the list value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) list: bool,
    /// Stores the show value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) show: bool,
    /// Stores whether the command targets the routing auto-sizing router.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) routing: bool,
}

/// Runs the runtime model command args operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_model_command_args(args: &str) -> Result<RuntimeModelCommandArgs> {
    let mut parsed = RuntimeModelCommandArgs::default();
    let mut words = args.split_whitespace().peekable();
    while let Some(word) = words.next() {
        match word {
            "--scope" => {
                let scope = words
                    .next()
                    .ok_or_else(|| MezError::invalid_args("/model --scope requires a value"))?;
                parsed.scope = Some(scope.to_string());
            }
            "--target" => {
                let target = words
                    .next()
                    .ok_or_else(|| MezError::invalid_args("/model --target requires a value"))?;
                parsed.target = Some(target.to_string());
            }
            "--routing" | "--router" => parsed.routing = true,
            "--reasoning" | "--reasoning-level" | "--reasoning-profile" => {
                let reasoning = words
                    .next()
                    .ok_or_else(|| MezError::invalid_args("/model --reasoning requires a value"))?;
                parsed.reasoning_profile = Some(reasoning.to_string());
            }
            "--clear" | "clear" => parsed.clear = true,
            "list" if parsed.profile.is_none() && parsed.reasoning_profile.is_none() => {
                parsed.list = true;
            }
            "--show" | "show" => parsed.show = true,
            value if value.starts_with("--") => {
                return Err(MezError::invalid_args(format!(
                    "unknown /model option `{value}`"
                )));
            }
            value => {
                if parsed.list {
                    return Err(MezError::invalid_args(
                        "/model list does not accept model or reasoning arguments",
                    ));
                }
                if parsed.profile.is_none() {
                    parsed.profile = Some(value.to_string());
                } else if parsed.reasoning_profile.is_none() {
                    parsed.reasoning_profile = Some(value.to_string());
                } else {
                    return Err(MezError::invalid_args(
                        "/model accepts at most a model name and optional reasoning level",
                    ));
                }
            }
        }
    }
    if parsed.clear
        && (parsed.profile.is_some() || parsed.reasoning_profile.is_some() || parsed.list)
    {
        return Err(MezError::invalid_args(
            "/model clear cannot be combined with list, model, or reasoning arguments",
        ));
    }
    if parsed.list && parsed.reasoning_profile.is_some() {
        return Err(MezError::invalid_args(
            "/model list cannot be combined with a reasoning level",
        ));
    }
    if parsed.routing && (parsed.scope.is_some() || parsed.target.is_some()) {
        return Err(MezError::invalid_args(
            "/model --routing cannot be combined with --scope or --target",
        ));
    }
    Ok(parsed)
}

/// Runs the runtime model override scope for args operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_model_override_scope_for_args(
    service: &RuntimeSessionService,
    pane_id: &str,
    agent_id: &str,
    args: &RuntimeModelCommandArgs,
) -> Result<RuntimeModelProfileOverrideScope> {
    let scope = args.scope.as_deref().unwrap_or("pane");
    match scope {
        "session" => Ok(RuntimeModelProfileOverrideScope::Session),
        "window" => {
            let window_id = if let Some(target) = args.target.as_deref() {
                target.to_string()
            } else {
                service
                    .find_pane_descriptor(pane_id)
                    .ok_or_else(|| {
                        MezError::new(crate::error::MezErrorKind::NotFound, "pane not found")
                    })?
                    .window_id
                    .to_string()
            };
            Ok(RuntimeModelProfileOverrideScope::Window(window_id))
        }
        "pane" => Ok(RuntimeModelProfileOverrideScope::Pane(
            args.target.as_deref().unwrap_or(pane_id).to_string(),
        )),
        "agent" => Ok(RuntimeModelProfileOverrideScope::Agent(
            args.target.as_deref().unwrap_or(agent_id).to_string(),
        )),
        "subagent" => {
            let target = args.target.as_deref().ok_or_else(|| {
                MezError::invalid_args("/model --scope subagent requires --target")
            })?;
            Ok(RuntimeModelProfileOverrideScope::Subagent(
                target.to_string(),
            ))
        }
        _ => Err(MezError::invalid_args(
            "/model --scope must be session, window, pane, agent, or subagent",
        )),
    }
}

/// Runs the runtime model override scope name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_model_override_scope_name(
    scope: &RuntimeModelProfileOverrideScope,
) -> String {
    match scope {
        RuntimeModelProfileOverrideScope::Session => "session".to_string(),
        RuntimeModelProfileOverrideScope::Window(id) => format!("window:{id}"),
        RuntimeModelProfileOverrideScope::Pane(id) => format!("pane:{id}"),
        RuntimeModelProfileOverrideScope::Agent(id) => format!("agent:{id}"),
        RuntimeModelProfileOverrideScope::Subagent(id) => format!("subagent:{id}"),
    }
}
/// Supported pane-local model latency preferences in display order.
pub(super) const RUNTIME_LATENCY_PREFERENCES: &[&str] = &["slow", "default", "fast"];

/// Validates a user-facing latency preference value.
pub(super) fn runtime_validate_latency_preference(value: &str) -> Result<&str> {
    let value = value.trim();
    if RUNTIME_LATENCY_PREFERENCES
        .iter()
        .any(|allowed| allowed == &value)
    {
        Ok(value)
    } else {
        Err(MezError::invalid_args(format!(
            "latency preference must be slow, default, or fast, got {value:?}"
        )))
    }
}

/// Runs the runtime model profile display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_model_profile_display(
    active_name: &str,
    active_profile: &ModelProfile,
    profiles: &BTreeMap<String, ModelProfile>,
) -> String {
    let available = profiles
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "active_profile={} provider={} model={} latency_preference={} profiles={}",
        active_name,
        active_profile.provider,
        active_profile.model,
        active_profile
            .latency_preference
            .as_deref()
            .unwrap_or("default"),
        available
    )
}

/// Runs the runtime agent turn start hook payload operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_agent_turn_start_hook_payload(
    turn: &AgentTurnRecord,
    model_profile: &ModelProfile,
) -> String {
    format!(
        r#"{{"turn_id":"{}","agent_id":"{}","pane_id":"{}","model_provider":"{}","model":"{}"}}"#,
        json_escape(&turn.turn_id),
        json_escape(&turn.agent_id),
        json_escape(&turn.pane_id),
        json_escape(&model_profile.provider),
        json_escape(&model_profile.model)
    )
}

/// Runs the runtime pre mcp hook payload operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_pre_mcp_hook_payload(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    server: &str,
    tool: &str,
    arguments_json: &str,
) -> String {
    format!(
        r#"{{"turn_id":"{}","agent_id":"{}","pane_id":"{}","action_id":"{}","action_type":"mcp_call","server":"{}","tool":"{}","arguments_json":"{}","arguments_bytes":{}}}"#,
        json_escape(&turn.turn_id),
        json_escape(&turn.agent_id),
        json_escape(&turn.pane_id),
        json_escape(&action.id),
        json_escape(server),
        json_escape(tool),
        json_escape(arguments_json),
        arguments_json.len()
    )
}

/// Runs the runtime post mcp hook payload operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_post_mcp_hook_payload(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    result: &ActionResult,
) -> String {
    format!(
        r#"{{"turn_id":"{}","agent_id":"{}","pane_id":"{}","action_id":"{}","action_type":"mcp_call","status":"{:?}","is_error":{}}}"#,
        json_escape(&turn.turn_id),
        json_escape(&turn.agent_id),
        json_escape(&turn.pane_id),
        json_escape(&action.id),
        result.status,
        result.is_error
    )
}

/// Runs the runtime permission request hook payload operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_permission_request_hook_payload(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    result: &ActionResult,
) -> String {
    format!(
        r#"{{"turn_id":"{}","agent_id":"{}","pane_id":"{}","action_id":"{}","action_type":"{}","approval":{}}}"#,
        json_escape(&turn.turn_id),
        json_escape(&turn.agent_id),
        json_escape(&turn.pane_id),
        json_escape(&action.id),
        action.action_type(),
        result
            .structured_content_json
            .as_deref()
            .unwrap_or(r#"{"approval":{"state":"pending"}}"#)
    )
}

/// Runs the runtime permission decision hook payload operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_permission_decision_hook_payload(
    approval_id: &str,
    decision: &str,
) -> String {
    format!(
        r#"{{"approval_id":"{}","decision":"{}"}}"#,
        json_escape(approval_id),
        json_escape(decision)
    )
}

/// Runs the runtime mcp error code operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_mcp_error_code(error: &MezError) -> &'static str {
    match error.kind() {
        crate::error::MezErrorKind::InvalidState
            if error.message().contains("MCP protocol error")
                || error.message().contains("JSON-RPC")
                || error.message().contains("response") =>
        {
            "mcp_protocol_error"
        }
        crate::error::MezErrorKind::InvalidState => "transport_error",
        crate::error::MezErrorKind::Forbidden => "permission_denied",
        _ => "transport_error",
    }
}

/// Runs the runtime message recipient operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_message_recipient(value: &str) -> Result<Recipient> {
    if value == "session" || value == "group:session" {
        return Ok(Recipient::Session);
    }
    if let Some(agent) = value.strip_prefix("agent:") {
        return AgentId::opaque(agent.to_string())
            .map(Recipient::Agent)
            .ok_or_else(|| MezError::invalid_args("send_message recipient agent id is invalid"));
    }
    if value.starts_with("agent-") {
        return AgentId::opaque(value.to_string())
            .map(Recipient::Agent)
            .ok_or_else(|| MezError::invalid_args("send_message recipient agent id is invalid"));
    }
    if let Some(agent) = AgentId::parse('a', value.to_string()) {
        return Ok(Recipient::Agent(agent));
    }
    if let Some(pane) = value.strip_prefix("pane:") {
        return PaneId::parse('%', pane.to_string())
            .map(Recipient::Pane)
            .ok_or_else(|| MezError::invalid_args("send_message recipient pane id is invalid"));
    }
    if let Some(pane) = PaneId::parse('%', value.to_string()) {
        return Ok(Recipient::Pane(pane));
    }
    if let Some(window) = value.strip_prefix("window:") {
        return WindowId::parse('@', window.to_string())
            .map(Recipient::Window)
            .ok_or_else(|| MezError::invalid_args("send_message recipient window id is invalid"));
    }
    if let Some(window) = WindowId::parse('@', value.to_string()) {
        return Ok(Recipient::Window(window));
    }
    if let Some(role) = value.strip_prefix("role:") {
        if role.is_empty() {
            return Err(MezError::invalid_args(
                "send_message recipient role is invalid",
            ));
        }
        return Ok(Recipient::Role(role.to_string()));
    }
    if let Some(capability) = value.strip_prefix("capability:") {
        if capability.is_empty() {
            return Err(MezError::invalid_args(
                "send_message recipient capability is invalid",
            ));
        }
        return Ok(Recipient::Capability(capability.to_string()));
    }
    if let Some(group) = value.strip_prefix("group:") {
        if group.is_empty() {
            return Err(MezError::invalid_args(
                "send_message recipient group is invalid",
            ));
        }
        return Ok(Recipient::Group(group.to_string()));
    }
    Err(MezError::invalid_args(
        "send_message recipient must be session, agent:<id>, pane:<id>, window:<id>, role:<name>, capability:<name>, or group:<name>",
    ))
}

/// Runs the runtime blocked approval request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_blocked_approval_request(
    turn: &AgentTurnRecord,
    result: &ActionResult,
    scope: Option<&SubagentScopeDeclaration>,
) -> BlockedApprovalRequest {
    let approval = result
        .structured_content_json
        .as_deref()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .and_then(|value| value.get("approval").cloned());
    let action_kind = approval
        .as_ref()
        .and_then(|value| value.get("kind"))
        .and_then(Value::as_str)
        .unwrap_or(result.action_type)
        .to_string();
    let action_summary = runtime_blocked_approval_summary(result, approval.as_ref());
    BlockedApprovalRequest {
        id: String::new(),
        requesting_agent_id: turn.agent_id.clone(),
        pane_id: turn.pane_id.clone(),
        parent_agent_chain: vec![turn.agent_id.clone()],
        action_kind,
        action_summary,
        declared_effects: result.content_texts(),
        matched_rules: vec!["runtime.agent_action_blocked".to_string()],
        read_scopes: scope
            .map(|scope| scope.read_scopes.clone())
            .unwrap_or_default(),
        write_scopes: scope
            .map(|scope| scope.write_scopes.clone())
            .unwrap_or_default(),
        cooperation_mode: scope
            .map(|scope| runtime_cooperation_mode_name(scope.cooperation_mode).to_string())
            .or_else(|| turn.cooperation_mode.clone()),
        created_at_unix_seconds: None,
        decided_at_unix_seconds: None,
        decided_by_client_id: None,
        state: crate::permissions::BlockedApprovalState::Pending,
        decision: None,
        redirect_instruction: None,
    }
}

/// Runs the runtime blocked approval summary operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_blocked_approval_summary(
    result: &ActionResult,
    approval: Option<&Value>,
) -> String {
    if let Some(approval) = approval {
        if let Some(command) = approval.get("command").and_then(Value::as_str) {
            return command.to_string();
        }
        if let Some(command) = approval.get("policy_command").and_then(Value::as_str) {
            return command.to_string();
        }
        if let (Some(server), Some(tool)) = (
            approval.get("server").and_then(Value::as_str),
            approval.get("tool").and_then(Value::as_str),
        ) {
            return format!("{server}/{tool}");
        }
        if let Some(path) = approval.get("path").and_then(Value::as_str) {
            let operation = approval
                .get("operation")
                .and_then(Value::as_str)
                .unwrap_or("change");
            return format!("{operation} {path}");
        }
        if let Some(prompt) = approval.get("prompt").and_then(Value::as_str) {
            return prompt.to_string();
        }
    }
    if result.content.is_empty() {
        result.action_type.to_string()
    } else {
        result.content_text().replace('\n', " ")
    }
}

/// Runs the runtime string array json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_string_array_json(values: &[String]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| format!(r#""{}""#, json_escape(value)))
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// Runs the validate runtime terminal term operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_runtime_terminal_term(term: &str) -> Result<()> {
    if term.trim().is_empty() || term.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(MezError::config(
            "terminal.term must be a non-empty printable string",
        ));
    }
    if matches!(term, "xterm" | "xterm-256color") {
        return Err(MezError::config(
            "terminal.term must not use the host terminal identity in the default profile",
        ));
    }
    Ok(())
}

/// Runs the runtime permission policy from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_permission_policy_from_config(root: &Value) -> Result<PermissionPolicy> {
    let mut policy = PermissionPolicy::default();
    let Some(permissions) = runtime_json_object(root, "permissions") else {
        return Ok(policy);
    };
    if let Some(preset) = runtime_json_string(permissions.get("preset")) {
        policy.preset = runtime_config_permission_preset(preset)?;
    }
    if let Some(approval_policy) = runtime_json_string(permissions.get("approval_policy")) {
        policy.approval_policy = runtime_config_approval_policy(approval_policy)?;
    }
    if let Some(bypass) = runtime_json_bool(permissions.get("bypass_mode")) {
        if bypass {
            return Err(MezError::config(
                "permissions.bypass_mode cannot be enabled from configuration; use explicit approval bypass activation",
            ));
        }
        policy.set_approval_bypass(false);
    }

    for (table, default_scope) in [
        ("command_rules", CommandRuleScope::Managed),
        ("session_command_rules", CommandRuleScope::Session),
        ("global_command_rules", CommandRuleScope::User),
    ] {
        let Some(rules) = permissions.get(table).and_then(Value::as_array) else {
            continue;
        };
        for rule_value in rules {
            policy.add_rule(runtime_command_rule_from_config(rule_value, default_scope)?);
        }
    }
    if let Some(dirs) = permissions
        .get("trusted_directories")
        .and_then(Value::as_array)
    {
        policy.trusted_directories = dirs
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
    }
    if let Some(projects) = permissions
        .get("trusted_projects")
        .and_then(Value::as_array)
    {
        for project in projects.iter().filter_map(Value::as_str) {
            policy.trusted_directories.push(project.to_string());
        }
    }
    Ok(policy)
}

/// Runs the runtime provider registry from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
/// Runs the runtime hook definitions from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_hook_definitions_from_config(root: &Value) -> Result<Vec<HookDefinition>> {
    let mut definitions = Vec::new();
    let Some(hooks) = runtime_json_object(root, "hooks") else {
        return Ok(definitions);
    };

    for (hook_id, value) in hooks {
        let Some(object) = value.as_object() else {
            return Err(MezError::config(format!(
                "hooks.{hook_id} must be an object"
            )));
        };
        let events = runtime_hook_events_from_config(hook_id, object)?;
        if events.is_empty() {
            continue;
        }
        let invocation = runtime_hook_invocation_from_config(hook_id, object)?;
        let enabled = runtime_json_bool(object.get("enabled")).unwrap_or(true);
        let required = runtime_json_bool(object.get("required")).unwrap_or(false);
        let agent_hook = runtime_json_bool(object.get("agent_hook")).unwrap_or(false);
        let matcher_groups = runtime_hook_matcher_groups_from_config(hook_id, object)?;
        let timeout_ms = runtime_json_u64(object.get("timeout_ms")).or_else(|| {
            runtime_json_u64(object.get("timeout_sec")).map(|seconds| seconds.saturating_mul(1000))
        });
        let on_failure = runtime_json_string(object.get("on_failure"))
            .map(runtime_hook_on_failure_from_config)
            .transpose()?;

        for event in events {
            let definition = HookDefinition {
                id: hook_id.clone(),
                event,
                invocation: invocation.clone(),
                enabled,
                required,
                agent_hook,
                matcher_groups: matcher_groups.clone(),
                timeout_ms,
                on_failure,
            };
            definition.validate()?;
            definitions.push(definition);
        }
    }

    Ok(definitions)
}

/// Runs the runtime hook matcher groups from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_hook_matcher_groups_from_config(
    hook_id: &str,
    object: &serde_json::Map<String, Value>,
) -> Result<Vec<HookMatcherGroup>> {
    let mut groups = Vec::new();
    if let Some(group) = object.get("match") {
        groups.push(runtime_hook_matcher_group_from_config(
            &format!("hooks.{hook_id}.match"),
            group,
        )?);
    }
    if let Some(matches) = object.get("matches") {
        let array = matches.as_array().ok_or_else(|| {
            MezError::config(format!(
                "hooks.{hook_id}.matches must be an array of matcher groups"
            ))
        })?;
        for (index, group) in array.iter().enumerate() {
            groups.push(runtime_hook_matcher_group_from_config(
                &format!("hooks.{hook_id}.matches[{index}]"),
                group,
            )?);
        }
    }
    Ok(groups)
}

/// Runs the runtime hook matcher group from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_hook_matcher_group_from_config(path: &str, value: &Value) -> Result<HookMatcherGroup> {
    let object = value
        .as_object()
        .ok_or_else(|| MezError::config(format!("{path} must be a matcher object")))?;
    let mut predicates = Vec::new();
    if runtime_hook_matcher_is_single_predicate(object) {
        let predicate_path = runtime_json_string(object.get("path")).ok_or_else(|| {
            MezError::config(format!("{path}.path is required for matcher predicates"))
        })?;
        predicates.push(runtime_hook_matcher_predicate_from_config(
            path,
            predicate_path,
            value,
        )?);
    } else {
        for (predicate_path, predicate_value) in object {
            predicates.push(runtime_hook_matcher_predicate_from_config(
                &format!("{path}.{predicate_path}"),
                predicate_path,
                predicate_value,
            )?);
        }
    }
    if predicates.is_empty() {
        return Err(MezError::config(format!(
            "{path} must contain at least one matcher predicate"
        )));
    }
    Ok(HookMatcherGroup { predicates })
}

/// Runs the runtime hook matcher is single predicate operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_hook_matcher_is_single_predicate(object: &serde_json::Map<String, Value>) -> bool {
    object.contains_key("path")
        && object.keys().any(|key| {
            matches!(
                key.as_str(),
                "equals" | "prefix" | "suffix" | "contains" | "exists"
            )
        })
}

/// Runs the runtime hook matcher predicate from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_hook_matcher_predicate_from_config(
    diagnostic_path: &str,
    predicate_path: &str,
    value: &Value,
) -> Result<HookMatcherPredicate> {
    if predicate_path.trim().is_empty() {
        return Err(MezError::config(format!(
            "{diagnostic_path} matcher path must not be empty"
        )));
    }
    Ok(HookMatcherPredicate {
        path: predicate_path.to_string(),
        operator: runtime_hook_matcher_operator_from_config(diagnostic_path, value)?,
    })
}

/// Runs the runtime hook matcher operator from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_hook_matcher_operator_from_config(
    diagnostic_path: &str,
    value: &Value,
) -> Result<HookMatcherOperator> {
    let Some(object) = value.as_object() else {
        return Ok(HookMatcherOperator::Equals(runtime_json_scalar_string(
            diagnostic_path,
            value,
        )?));
    };
    if let Some(value) = object.get("equals") {
        return Ok(HookMatcherOperator::Equals(runtime_json_scalar_string(
            &format!("{diagnostic_path}.equals"),
            value,
        )?));
    }
    if let Some(value) = object.get("prefix") {
        return Ok(HookMatcherOperator::Prefix(runtime_json_scalar_string(
            &format!("{diagnostic_path}.prefix"),
            value,
        )?));
    }
    if let Some(value) = object.get("suffix") {
        return Ok(HookMatcherOperator::Suffix(runtime_json_scalar_string(
            &format!("{diagnostic_path}.suffix"),
            value,
        )?));
    }
    if let Some(value) = object.get("contains") {
        return Ok(HookMatcherOperator::Contains(runtime_json_scalar_string(
            &format!("{diagnostic_path}.contains"),
            value,
        )?));
    }
    if let Some(value) = object.get("exists") {
        let exists = value.as_bool().ok_or_else(|| {
            MezError::config(format!("{diagnostic_path}.exists must be a boolean"))
        })?;
        return Ok(HookMatcherOperator::Exists(exists));
    }
    Err(MezError::config(format!(
        "{diagnostic_path} must use equals, prefix, suffix, contains, or exists"
    )))
}

/// Runs the runtime hook events from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_hook_events_from_config(
    hook_id: &str,
    object: &serde_json::Map<String, Value>,
) -> Result<Vec<HookEvent>> {
    if let Some(event) = runtime_json_string(object.get("event")) {
        return Ok(vec![runtime_hook_event_from_config(event)?]);
    }
    if let Some(events) = runtime_json_string_array(object.get("events"))? {
        let mut parsed = Vec::with_capacity(events.len());
        for event in events {
            parsed.push(runtime_hook_event_from_config(&event)?);
        }
        return Ok(parsed);
    }
    if object.is_empty() {
        return Ok(Vec::new());
    }
    Err(MezError::config(format!(
        "hooks.{hook_id}.event or hooks.{hook_id}.events is required"
    )))
}

/// Runs the runtime hook invocation from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_hook_invocation_from_config(
    hook_id: &str,
    object: &serde_json::Map<String, Value>,
) -> Result<HookInvocation> {
    let args = runtime_json_string_array(object.get("args"))?.unwrap_or_default();
    if let Some(program) = runtime_json_string(object.get("program")) {
        return Ok(HookInvocation::Program {
            command: program.to_string(),
            args,
        });
    }
    let Some(command) = runtime_json_string(object.get("command")) else {
        return Err(MezError::config(format!(
            "hooks.{hook_id}.program or hooks.{hook_id}.command is required"
        )));
    };
    match runtime_json_string(object.get("kind")) {
        Some("program") => Ok(HookInvocation::Program {
            command: command.to_string(),
            args,
        }),
        Some("shell" | "focused_shell" | "focused-shell") | None => {
            Ok(HookInvocation::FocusedShell {
                command: command.to_string(),
            })
        }
        Some(kind) => Err(MezError::config(format!(
            "hooks.{hook_id}.kind must be program, shell, or focused_shell; got {kind}"
        ))),
    }
}

/// Runs the runtime hook event from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_hook_event_from_config(value: &str) -> Result<HookEvent> {
    match value {
        "session_start" | "SessionStart" => Ok(HookEvent::SessionStart),
        "session_stop" | "SessionStop" => Ok(HookEvent::SessionStop),
        "client_attach" | "ClientAttach" => Ok(HookEvent::ClientAttach),
        "client_detach" | "ClientDetach" => Ok(HookEvent::ClientDetach),
        "window_create" | "WindowCreate" => Ok(HookEvent::WindowCreate),
        "window_close" | "WindowClose" => Ok(HookEvent::WindowClose),
        "session_detach" | "SessionDetach" => Ok(HookEvent::SessionDetach),
        "pane_create" | "pane_created" | "PaneCreate" | "PaneCreated" => Ok(HookEvent::PaneCreate),
        "pane_close" | "pane_closed" | "PaneClose" | "PaneClosed" => Ok(HookEvent::PaneClose),
        "user_prompt_submit" | "UserPromptSubmit" => Ok(HookEvent::UserPromptSubmit),
        "agent_turn_start" | "AgentTurnStart" => Ok(HookEvent::AgentTurnStart),
        "agent_turn_stop" | "agent_turn_end" | "AgentTurnStop" | "AgentTurnEnd" => {
            Ok(HookEvent::AgentTurnStop)
        }
        "pre_shell_command" | "PreShellCommand" => Ok(HookEvent::PreShellCommand),
        "post_shell_command" | "PostShellCommand" => Ok(HookEvent::PostShellCommand),
        "permission_request" | "PermissionRequest" => Ok(HookEvent::PermissionRequest),
        "permission_decision" | "PermissionDecision" => Ok(HookEvent::PermissionDecision),
        "pre_mcp_tool_use" | "PreMcpToolUse" => Ok(HookEvent::PreMcpToolUse),
        "post_mcp_tool_use" | "PostMcpToolUse" => Ok(HookEvent::PostMcpToolUse),
        "layout_save" | "LayoutSave" => Ok(HookEvent::LayoutSave),
        "layout_load" | "LayoutLoad" => Ok(HookEvent::LayoutLoad),
        _ => Err(MezError::config(format!(
            "unsupported hook event `{value}`"
        ))),
    }
}

/// Runs the runtime hook on failure from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_hook_on_failure_from_config(
    value: &str,
) -> Result<crate::hooks::HookOnFailure> {
    match value {
        "block" => Ok(crate::hooks::HookOnFailure::Block),
        "warn" => Ok(crate::hooks::HookOnFailure::Warn),
        "ignore" => Ok(crate::hooks::HookOnFailure::Ignore),
        _ => Err(MezError::config(format!(
            "hook on_failure must be block, warn, or ignore; got {value}"
        ))),
    }
}

/// Runs the runtime command rule from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_command_rule_from_config(
    value: &Value,
    default_scope: CommandRuleScope,
) -> Result<CommandRule> {
    let object = value
        .as_object()
        .ok_or_else(|| MezError::config("permission command rule must be an object"))?;
    let decision = runtime_config_rule_decision(
        runtime_json_string(object.get("decision"))
            .ok_or_else(|| MezError::config("permission command rule requires decision"))?,
    )?;
    let scope = match runtime_json_string(object.get("scope")) {
        Some(scope) => runtime_config_command_rule_scope(scope)?,
        None => default_scope,
    };
    if scope == CommandRuleScope::BuiltIn {
        return Err(MezError::config(
            "configuration command rules cannot use built-in scope",
        ));
    }
    let match_kind = runtime_json_string(object.get("match")).unwrap_or("prefix");
    let mut rule = if match_kind == "exact_sha256" {
        let digest = runtime_json_string(object.get("exact_sha256")).ok_or_else(|| {
            MezError::config("exact_sha256 command rule requires exact_sha256 digest")
        })?;
        CommandRule::from_exact_sha256_digest(
            digest,
            runtime_json_string(object.get("shell_classification"))
                .unwrap_or(DEFAULT_COMMAND_SHELL_CLASSIFICATION),
            decision,
        )?
    } else {
        let pattern = runtime_json_rule_pattern(object.get("pattern"))?;
        let rule_match = match match_kind {
            "prefix" => RuleMatch::Prefix,
            "exact" => RuleMatch::Exact,
            _ => {
                return Err(MezError::config(
                    "permission command rule match must be prefix, exact, or exact_sha256",
                ));
            }
        };
        CommandRule::new(pattern, decision, rule_match)?
    }
    .with_scope(scope);
    if let Some(justification) = runtime_json_string(object.get("justification")) {
        rule = rule.with_justification(justification);
    }
    Ok(rule)
}

/// Runs the runtime json object operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_json_object<'a>(
    value: &'a Value,
    field: &str,
) -> Option<&'a serde_json::Map<String, Value>> {
    value.get(field).and_then(Value::as_object)
}

/// Runs the runtime json string operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_json_string(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

/// Runs the runtime json scalar string operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_json_scalar_string(path: &str, value: &Value) -> Result<String> {
    match value {
        Value::String(value) if !value.is_empty() => Ok(value.clone()),
        Value::Bool(value) => Ok(value.to_string()),
        Value::Number(value) => Ok(value.to_string()),
        _ => Err(MezError::config(format!(
            "{path} must be a non-empty scalar value"
        ))),
    }
}

/// Runs the runtime json bool operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_json_bool(value: Option<&Value>) -> Option<bool> {
    value.and_then(Value::as_bool)
}

/// Runs the runtime json u64 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_json_u64(value: Option<&Value>) -> Option<u64> {
    value.and_then(Value::as_u64)
}

/// Returns the configured provider auth refresh leeway in seconds.
pub(super) fn runtime_provider_auth_refresh_leeway_seconds_from_config(root: &Value) -> u64 {
    runtime_json_object(root, "auth")
        .and_then(|auth| runtime_json_u64(auth.get("provider_refresh_leeway_seconds")))
        .unwrap_or(crate::auth::DEFAULT_PROVIDER_AUTH_REFRESH_LEEWAY_SECONDS)
}

/// Runs the runtime json string array operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_json_string_array(value: Option<&Value>) -> Result<Option<Vec<String>>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let array = value
        .as_array()
        .ok_or_else(|| MezError::config("configuration value must be an array of strings"))?;
    let mut output = Vec::with_capacity(array.len());
    for item in array {
        let Some(item) = item.as_str() else {
            return Err(MezError::config(
                "configuration array values must all be strings",
            ));
        };
        output.push(item.to_string());
    }
    Ok(Some(output))
}

/// Runs the runtime json string map operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_json_string_map(
    value: Option<&Value>,
) -> Result<Option<BTreeMap<String, String>>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let object = value
        .as_object()
        .ok_or_else(|| MezError::config("configuration value must be a string map"))?;
    let mut output = BTreeMap::new();
    for (key, value) in object {
        let Some(value) = value.as_str() else {
            return Err(MezError::config(
                "configuration map values must all be strings",
            ));
        };
        output.insert(key.clone(), value.to_string());
    }
    Ok(Some(output))
}

/// Runs the runtime json rule pattern operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_json_rule_pattern(value: Option<&Value>) -> Result<Vec<String>> {
    let Some(value) = value else {
        return Err(MezError::config("permission command rule requires pattern"));
    };
    if let Some(pattern) = value.as_str() {
        return Ok(vec![pattern.to_string()]);
    }
    runtime_json_string_array(Some(value))?
        .filter(|pattern| !pattern.is_empty())
        .ok_or_else(|| MezError::config("permission command rule pattern must not be empty"))
}

/// Runs the runtime config permission preset operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_config_permission_preset(value: &str) -> Result<PermissionPreset> {
    match value {
        "read-only" | "readonly" => Ok(PermissionPreset::ReadOnly),
        "auto" => Ok(PermissionPreset::Auto),
        _ => Err(MezError::config("unsupported permission preset")),
    }
}

/// Runs the runtime config approval policy operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_config_approval_policy(value: &str) -> Result<ApprovalPolicy> {
    match value {
        "ask" => Ok(ApprovalPolicy::Ask),
        "auto-allow" | "auto_allow" => Ok(ApprovalPolicy::AutoAllow),
        "full-access" | "full_access" => Ok(ApprovalPolicy::FullAccess),
        _ => Err(MezError::config(
            "unsupported approval policy; use ask, auto-allow, or full-access",
        )),
    }
}

/// Runs the runtime config rule decision operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_config_rule_decision(value: &str) -> Result<RuleDecision> {
    match value {
        "allow" => Ok(RuleDecision::Allow),
        "prompt" => Ok(RuleDecision::Prompt),
        "forbid" | "deny" => Ok(RuleDecision::Forbid),
        _ => Err(MezError::config("unsupported command rule decision")),
    }
}

/// Runs the runtime config command rule scope operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_config_command_rule_scope(value: &str) -> Result<CommandRuleScope> {
    match value {
        "session" => Ok(CommandRuleScope::Session),
        "project" => Ok(CommandRuleScope::Project),
        "user" | "global" => Ok(CommandRuleScope::User),
        "managed" => Ok(CommandRuleScope::Managed),
        _ => Err(MezError::config("unsupported command rule scope")),
    }
}

/// Runs the runtime mcp approval setting operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn optional_i32_json(value: Option<i32>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string())
}

#[cfg(test)]
mod tests {
    use crate::terminal::{TerminalEmojiWidth, terminal_text_width};

    use super::{runtime_fit_status_line, runtime_terminal_emoji_width_from_config};

    /// Verifies that fitting ASCII text truncates to the requested display width.
    #[test]
    fn fits_ascii_text_to_width() {
        assert_eq!(runtime_fit_status_line("hello world", 5), "hello");
        assert_eq!(runtime_fit_status_line("ab", 5), "ab   ");
    }

    /// Verifies that fitting fullwidth text counts display columns, not Unicode
    /// scalar values, so wide characters do not overflow the target width.
    #[test]
    fn fits_fullwidth_text_by_display_width() {
        let result = runtime_fit_status_line("ＡＢＣＤＥＦ", 6);
        assert_eq!(terminal_text_width(&result), 6);
        assert_eq!(result.chars().count(), 3);
    }

    /// Verifies that fitting text with mixed fullwidth and narrow characters
    /// truncates at the display-width boundary.
    #[test]
    fn fits_mixed_width_text_by_display_width() {
        let result = runtime_fit_status_line("ＡbcＤ", 4);
        // Ａ = 2, b = 1, c = 1 → fits in 4 cols
        // Ｄ would be another 2 → 6 > 4, dropped
        assert_eq!(terminal_text_width(&result), 4);
        assert!(result.starts_with('Ａ'));
    }

    /// Verifies that fitting with zero width returns an empty string.
    #[test]
    fn fits_zero_width_returns_empty() {
        assert_eq!(runtime_fit_status_line("hello", 0), "");
        assert_eq!(runtime_fit_status_line("ＡＢ", 0), "");
    }

    /// Verifies that runtime configuration parses the terminal emoji-width
    /// compatibility policy and rejects unsupported values. This protects the
    /// pane renderer from silently falling back to the wrong width model when a
    /// user opts into one-cell text fallback status glyphs.
    #[test]
    fn parses_terminal_emoji_width_policy_from_config() {
        assert_eq!(
            runtime_terminal_emoji_width_from_config(&serde_json::json!({
                "terminal": {
                    "emoji_width": "narrow"
                }
            }))
            .unwrap(),
            TerminalEmojiWidth::Narrow
        );
        assert_eq!(
            runtime_terminal_emoji_width_from_config(&serde_json::json!({})).unwrap(),
            TerminalEmojiWidth::Wide
        );
        assert!(
            runtime_terminal_emoji_width_from_config(&serde_json::json!({
                "terminal": {
                    "emoji_width": "auto"
                }
            }))
            .is_err()
        );
    }

    /// Verifies that narrow characters pad to the exact display width while
    /// fullwidth characters are not truncated mid-grapheme.
    #[test]
    fn fits_narrow_pads_and_wide_truncates_cleanly() {
        let narrow = runtime_fit_status_line("x", 4);
        assert_eq!(terminal_text_width(&narrow), 4);

        let wide = runtime_fit_status_line("Ａ", 1);
        // 'Ａ' is 2 cols wide, 2 > 1, so it is dropped entirely
        assert_eq!(terminal_text_width(&wide), 1);
        assert_eq!(wide, " ");
    }
}
