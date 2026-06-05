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
    DEFAULT_AGENT_IMPLEMENTATION_PRESSURE_AFTER_SHELL_ACTIONS, DEFAULT_AGENT_ROUTING,
    DEFAULT_AUTO_SIZING_FALLBACK_POLICY, DEFAULT_COMMAND_SHELL_CLASSIFICATION,
    DEFAULT_HISTORY_LIMIT, DEFAULT_HISTORY_ROTATE_LINES, DEFAULT_MAX_CONCURRENT_AGENTS,
    DEFAULT_MAX_ROOT_SUBAGENTS, DEFAULT_MAX_SUBAGENT_DEPTH, DEFAULT_MAX_SUBAGENT_PANES_PER_WINDOW,
    DEFAULT_MAX_SUBAGENTS_PER_SUBAGENT, DEFAULT_PANE_TERM, DEFAULT_SUBAGENT_WAIT_POLICY,
    DEFAULT_UI_THEME_NAME, EffectiveConfig, HookDefinition, HookEvent, HookInvocation,
    HookMatcherGroup, HookMatcherOperator, HookMatcherPredicate, HostClipboard,
    HostClipboardCommand, KeyBindings, KeyChord, MarkerToken, McpApprovalSetting,
    McpExternalCapability, McpRegistry, McpServerConfig, McpServerKind, MezError, ModelProfile,
    PaneId, Path, PathBuf, PermissionPolicy, PermissionPreset, ProjectTrustRecord, Recipient,
    Result, RuleDecision, RuleMatch, RuntimeAgentPersonalityProfile, RuntimeAutoSizingConfig,
    RuntimeAutoSizingFallbackPolicy, RuntimeCommandBinding, RuntimeConfigApplyReport,
    RuntimeModelPreset, RuntimeModelProfileOverrideScope, RuntimePresetRegistry,
    RuntimeProviderConfig, RuntimeProviderRegistry, RuntimeSessionService, SubagentProfile,
    SubagentScopeDeclaration, SubagentWaitPolicy, TerminalCursorStyle, TrustDecision, UiTheme,
    UiThemeDefinition, Value, WindowId, builtin_subagent_profiles, builtin_ui_theme_definition,
    ensure_absolute, exact_command_sha256, fs, key_chord_notation, optional_path_json,
    optional_string_json, parse_command_sequence, resolve_ui_theme, runtime_cooperation_mode,
    runtime_cooperation_mode_name, runtime_json_string_field, runtime_json_value,
    unix_seconds_to_rfc3339, valid_color_alias_name, validate_config_text,
};
use crate::agent::effective_provider_api;
use crate::terminal::TerminalEmojiWidth;

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

/// Runs the runtime ui theme from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn runtime_ui_theme_from_config(root: &Value) -> Result<UiTheme> {
    let theme = root.get("theme").and_then(Value::as_object);
    let active = theme
        .and_then(|object| object.get("active"))
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_UI_THEME_NAME);
    if active.trim().is_empty() || !valid_color_alias_name(active) {
        return Err(MezError::config(
            "theme.active must be a non-empty theme identifier",
        ));
    }

    let mut definition = if let Some(definition) = builtin_ui_theme_definition(active) {
        definition
    } else {
        let Some(custom_theme) = root
            .get("themes")
            .and_then(Value::as_object)
            .and_then(|themes| themes.get(active))
        else {
            return Err(MezError::config(format!(
                "theme.active `{active}` does not name a built-in or configured theme"
            )));
        };
        let mut definition = builtin_ui_theme_definition("deepforest")
            .ok_or_else(|| MezError::config("built-in deepforest theme is unavailable"))?;
        definition.merge(runtime_theme_definition_from_value(
            custom_theme,
            &format!("themes.{active}"),
        )?);
        definition
    };

    if let Some(theme) = root.get("theme") {
        definition.merge(runtime_theme_definition_from_value(theme, "theme")?);
    }

    resolve_ui_theme(active, definition)
}

/// Runs the runtime theme definition from value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_theme_definition_from_value(value: &Value, path: &str) -> Result<UiThemeDefinition> {
    let object = value
        .as_object()
        .ok_or_else(|| MezError::config(format!("{path} must be a table")))?;
    Ok(UiThemeDefinition {
        aliases: runtime_string_map_from_value(object.get("aliases"), &format!("{path}.aliases"))?,
        colors: runtime_string_map_from_value(object.get("colors"), &format!("{path}.colors"))?,
    })
}

/// Runs the runtime string map from value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_string_map_from_value(
    value: Option<&Value>,
    path: &str,
) -> Result<BTreeMap<String, String>> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let object = value
        .as_object()
        .ok_or_else(|| MezError::config(format!("{path} must be a table")))?;
    object
        .iter()
        .map(|(key, value)| {
            let value = value
                .as_str()
                .ok_or_else(|| MezError::config(format!("{path}.{key} must be a string value")))?;
            Ok((key.clone(), value.to_string()))
        })
        .collect()
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

/// Runs the runtime effective config value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn runtime_effective_config_value(layers: &[ConfigLayer]) -> Result<Value> {
    let mut root = Value::Object(serde_json::Map::new());
    for layer in layers {
        let validation = validate_config_text(layer.format, &layer.text, layer.scope);
        if !validation.valid {
            return Err(MezError::config(format!(
                "configuration layer `{}` is invalid",
                layer.name
            )));
        }
        if layer.scope == ConfigScope::ProjectOverlay && !layer.trusted {
            continue;
        }
        let value = runtime_config_layer_value(layer)?;
        runtime_merge_json_values(&mut root, value);
    }
    Ok(root)
}

/// Runs the runtime config layer value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_config_layer_value(layer: &ConfigLayer) -> Result<Value> {
    match layer.format {
        ConfigFormat::Toml => {
            let value = toml::from_str::<toml::Table>(&layer.text)
                .map_err(|error| MezError::config(error.to_string()))?;
            serde_json::to_value(value).map_err(|error| MezError::config(error.to_string()))
        }
        ConfigFormat::Yaml => {
            let value = serde_norway::from_str::<serde_norway::Value>(&layer.text)
                .map_err(|error| MezError::config(error.to_string()))?;
            serde_json::to_value(value).map_err(|error| MezError::config(error.to_string()))
        }
        ConfigFormat::Json => {
            serde_json::from_str(&layer.text).map_err(|error| MezError::config(error.to_string()))
        }
    }
}

/// Runs the runtime merge json values operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_merge_json_values(target: &mut Value, source: Value) {
    match (target, source) {
        (Value::Object(target), Value::Object(source)) => {
            for (key, value) in source {
                if let Some(existing) = target.get_mut(&key) {
                    runtime_merge_json_values(existing, value);
                } else {
                    target.insert(key, value);
                }
            }
        }
        (target, source) => *target = source,
    }
}

/// Runs the runtime history limit from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_history_limit_from_config(root: &Value) -> Result<usize> {
    let Some(history) = runtime_json_object(root, "history") else {
        return Ok(DEFAULT_HISTORY_LIMIT);
    };
    let Some(value) = history.get("lines") else {
        return Ok(DEFAULT_HISTORY_LIMIT);
    };
    let Some(limit) = value.as_u64() else {
        return Err(MezError::config("history.lines must be a positive integer"));
    };
    let limit = usize::try_from(limit)
        .map_err(|_| MezError::config("history.lines is too large for this platform"))?;
    if limit == 0 {
        return Err(MezError::config("history.lines must be greater than zero"));
    }
    Ok(limit)
}

/// Reads the configured terminal history overflow rotation batch.
pub(super) fn runtime_history_rotate_lines_from_config(root: &Value) -> Result<usize> {
    let Some(history) = runtime_json_object(root, "history") else {
        return Ok(DEFAULT_HISTORY_ROTATE_LINES);
    };
    let Some(value) = history.get("rotate_lines") else {
        return Ok(DEFAULT_HISTORY_ROTATE_LINES);
    };
    let Some(rotate_lines) = value.as_u64() else {
        return Err(MezError::config(
            "history.rotate_lines must be a positive integer",
        ));
    };
    let rotate_lines = usize::try_from(rotate_lines)
        .map_err(|_| MezError::config("history.rotate_lines is too large for this platform"))?;
    if rotate_lines == 0 {
        return Err(MezError::config(
            "history.rotate_lines must be greater than zero",
        ));
    }
    Ok(rotate_lines)
}

/// Runs the runtime terminal term from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_terminal_term_from_config(root: &Value) -> Result<String> {
    let Some(terminal) = runtime_json_object(root, "terminal") else {
        return Ok(DEFAULT_PANE_TERM.to_string());
    };
    let Some(value) = terminal.get("term") else {
        return Ok(DEFAULT_PANE_TERM.to_string());
    };
    let Some(term) = runtime_json_string(Some(value)) else {
        return Err(MezError::config("terminal.term must be a string"));
    };
    validate_runtime_terminal_term(term)?;
    Ok(term.to_string())
}

/// Runs the runtime terminal cursor style from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_terminal_cursor_style_from_config(
    root: &Value,
) -> Result<TerminalCursorStyle> {
    let Some(terminal) = runtime_json_object(root, "terminal") else {
        return Ok(TerminalCursorStyle::Block);
    };
    let Some(value) = terminal.get("cursor_style") else {
        return Ok(TerminalCursorStyle::Block);
    };
    let Some(style) = runtime_json_string(Some(value)) else {
        return Err(MezError::config("terminal.cursor_style must be a string"));
    };
    match style {
        "block" => Ok(TerminalCursorStyle::Block),
        "underline" => Ok(TerminalCursorStyle::Underline),
        "bar" => Ok(TerminalCursorStyle::Bar),
        _ => Err(MezError::config(
            "terminal.cursor_style must be block, underline, or bar",
        )),
    }
}

/// Runs the runtime terminal cursor blink from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_terminal_cursor_blink_from_config(root: &Value) -> Result<bool> {
    let Some(terminal) = runtime_json_object(root, "terminal") else {
        return Ok(false);
    };
    let Some(value) = terminal.get("cursor_blink") else {
        return Ok(false);
    };
    value
        .as_bool()
        .ok_or_else(|| MezError::config("terminal.cursor_blink must be a boolean"))
}

/// Runs the runtime terminal cursor blink interval ms from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_terminal_cursor_blink_interval_ms_from_config(root: &Value) -> Result<u64> {
    let Some(terminal) = runtime_json_object(root, "terminal") else {
        return Ok(500);
    };
    let Some(value) = terminal.get("cursor_blink_interval_ms") else {
        return Ok(500);
    };
    let Some(interval) = value.as_u64() else {
        return Err(MezError::config(
            "terminal.cursor_blink_interval_ms must be a positive integer",
        ));
    };
    if interval == 0 {
        return Err(MezError::config(
            "terminal.cursor_blink_interval_ms must be greater than zero",
        ));
    }
    Ok(interval)
}

/// Returns the configured emoji status-glyph width policy for terminal
/// measurement.
pub(super) fn runtime_terminal_emoji_width_from_config(root: &Value) -> Result<TerminalEmojiWidth> {
    let Some(terminal) = runtime_json_object(root, "terminal") else {
        return Ok(TerminalEmojiWidth::Wide);
    };
    let Some(value) = terminal.get("emoji_width") else {
        return Ok(TerminalEmojiWidth::Wide);
    };
    let Some(emoji_width) = runtime_json_string(Some(value)) else {
        return Err(MezError::config("terminal.emoji_width must be a string"));
    };
    match emoji_width {
        "wide" => Ok(TerminalEmojiWidth::Wide),
        "narrow" => Ok(TerminalEmojiWidth::Narrow),
        _ => Err(MezError::config(
            "terminal.emoji_width must be wide or narrow",
        )),
    }
}

/// Runs the runtime terminal resize debounce ms from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_terminal_resize_debounce_ms_from_config(root: &Value) -> Result<u64> {
    let Some(terminal) = runtime_json_object(root, "terminal") else {
        return Ok(200);
    };
    let Some(value) = terminal.get("resize_debounce_ms") else {
        return Ok(200);
    };
    let Some(interval) = value.as_u64() else {
        return Err(MezError::config(
            "terminal.resize_debounce_ms must be a positive integer",
        ));
    };
    if interval == 0 {
        return Err(MezError::config(
            "terminal.resize_debounce_ms must be greater than zero",
        ));
    }
    Ok(interval)
}

/// Returns the configured attached-terminal render rate limit in frames per second.
pub(super) fn runtime_terminal_render_rate_limit_fps_from_config(root: &Value) -> Result<u64> {
    let Some(terminal) = runtime_json_object(root, "terminal") else {
        return Ok(5);
    };
    let Some(value) = terminal.get("render_rate_limit_fps") else {
        return Ok(5);
    };
    value.as_u64().ok_or_else(|| {
        MezError::config("terminal.render_rate_limit_fps must be a non-negative integer")
    })
}

/// Returns whether optional terminal animations should render as static UI.
pub(super) fn runtime_terminal_reduced_motion_from_config(root: &Value) -> Result<bool> {
    let Some(terminal) = runtime_json_object(root, "terminal") else {
        return Ok(false);
    };
    let Some(value) = terminal.get("reduced_motion") else {
        return Ok(false);
    };
    value
        .as_bool()
        .ok_or_else(|| MezError::config("terminal.reduced_motion must be true or false"))
}

/// Runs the runtime terminal clipboard from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_terminal_clipboard_from_config(root: &Value) -> Result<String> {
    let Some(terminal) = runtime_json_object(root, "terminal") else {
        return Ok("external".to_string());
    };
    let Some(value) = terminal.get("clipboard") else {
        return Ok("external".to_string());
    };
    let Some(clipboard) = runtime_json_string(Some(value)) else {
        return Err(MezError::config("terminal.clipboard must be a string"));
    };
    match clipboard {
        "external" | "host" | "internal" | "disabled" | "off" | "none" => Ok(clipboard.to_string()),
        _ => Err(MezError::config(
            "terminal.clipboard must be external, internal, or disabled",
        )),
    }
}

/// Runs the runtime host clipboard from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_host_clipboard_from_config(root: &Value) -> Result<HostClipboard> {
    let Some(terminal) = runtime_json_object(root, "terminal") else {
        return Ok(HostClipboard::system());
    };
    let copy = runtime_clipboard_command_from_config(
        terminal.get("clipboard_copy_command"),
        "terminal.clipboard_copy_command",
    )?;
    let paste = runtime_clipboard_command_from_config(
        terminal.get("clipboard_paste_command"),
        "terminal.clipboard_paste_command",
    )?;
    if copy.is_none() && paste.is_none() {
        return Ok(HostClipboard::system());
    }
    Ok(HostClipboard::configured(copy, paste))
}

/// Parses one optional host clipboard command value.
///
/// # Parameters
/// - `value`: The configuration value to parse.
/// - `name`: The dotted configuration key used in diagnostics.
fn runtime_clipboard_command_from_config(
    value: Option<&Value>,
    name: &str,
) -> Result<Option<HostClipboardCommand>> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        Value::String(command) => runtime_clipboard_command_from_string(command, name).map(Some),
        Value::Array(arguments) => runtime_clipboard_command_from_array(arguments, name).map(Some),
        _ => Err(MezError::config(format!(
            "{name} must be a command string or array of strings"
        ))),
    }
}

/// Parses one shell-like clipboard command string.
///
/// # Parameters
/// - `command`: The command text to split.
/// - `name`: The dotted configuration key used in diagnostics.
fn runtime_clipboard_command_from_string(
    command: &str,
    name: &str,
) -> Result<HostClipboardCommand> {
    let arguments = shlex::split(command).ok_or_else(|| {
        MezError::config(format!("{name} must contain a valid shell-like command"))
    })?;
    runtime_clipboard_command_from_arguments(arguments, name)
}

/// Parses one clipboard command array.
///
/// # Parameters
/// - `arguments`: The command tokens supplied in configuration.
/// - `name`: The dotted configuration key used in diagnostics.
fn runtime_clipboard_command_from_array(
    arguments: &[Value],
    name: &str,
) -> Result<HostClipboardCommand> {
    let mut parsed = Vec::new();
    for argument in arguments {
        let Some(argument) = argument.as_str() else {
            return Err(MezError::config(format!(
                "{name} must contain only string arguments"
            )));
        };
        parsed.push(argument.to_string());
    }
    runtime_clipboard_command_from_arguments(parsed, name)
}

/// Builds a clipboard command from parsed command tokens.
///
/// # Parameters
/// - `arguments`: The command tokens with the program in the first slot.
/// - `name`: The dotted configuration key used in diagnostics.
fn runtime_clipboard_command_from_arguments(
    mut arguments: Vec<String>,
    name: &str,
) -> Result<HostClipboardCommand> {
    if arguments.is_empty() || arguments[0].trim().is_empty() {
        return Err(MezError::config(format!("{name} must not be empty")));
    }
    let program = arguments.remove(0);
    Ok(HostClipboardCommand::new(program, arguments))
}

/// Runs the runtime pane frames enabled from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_pane_frames_enabled_from_config(root: &Value) -> Result<bool> {
    let Some(frames) = runtime_json_object(root, "frames") else {
        return Ok(true);
    };
    let Some(pane) = frames.get("pane").and_then(Value::as_object) else {
        return Ok(true);
    };
    let Some(value) = pane.get("enabled") else {
        return Ok(true);
    };
    value
        .as_bool()
        .ok_or_else(|| MezError::config("frames.pane.enabled must be a boolean"))
}

/// Runs the runtime window frames enabled from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_window_frames_enabled_from_config(root: &Value) -> Result<bool> {
    let Some(frames) = runtime_json_object(root, "frames") else {
        return Ok(true);
    };
    let Some(window) = frames.get("window").and_then(Value::as_object) else {
        return Ok(true);
    };
    let Some(value) = window.get("enabled") else {
        return Ok(true);
    };
    value
        .as_bool()
        .ok_or_else(|| MezError::config("frames.window.enabled must be a boolean"))
}

/// Runs the runtime window frame template from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_window_frame_template_from_config(root: &Value) -> Result<String> {
    runtime_frame_template_from_config(
        root,
        "window",
        crate::terminal::DEFAULT_WINDOW_FRAME_TEMPLATE,
        crate::terminal::DEFAULT_WINDOW_FRAME_VISIBLE_FIELDS,
    )
}

/// Runs the runtime window frame right status template from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_window_frame_right_status_template_from_config(
    root: &Value,
) -> Result<String> {
    let Some(frames) = runtime_json_object(root, "frames") else {
        return Ok(crate::terminal::DEFAULT_WINDOW_FRAME_RIGHT_STATUS_TEMPLATE.to_string());
    };
    let Some(window) = frames.get("window").and_then(Value::as_object) else {
        return Ok(crate::terminal::DEFAULT_WINDOW_FRAME_RIGHT_STATUS_TEMPLATE.to_string());
    };
    let Some(value) = window.get("right_status") else {
        return Ok(crate::terminal::DEFAULT_WINDOW_FRAME_RIGHT_STATUS_TEMPLATE.to_string());
    };
    value
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| MezError::config("frames.window.right_status must be a string"))
}

/// Runs the runtime pane frame template from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_pane_frame_template_from_config(root: &Value) -> Result<String> {
    runtime_frame_template_from_config(
        root,
        "pane",
        crate::terminal::DEFAULT_PANE_FRAME_TEMPLATE,
        crate::terminal::DEFAULT_PANE_FRAME_VISIBLE_FIELDS,
    )
}

/// Runs the runtime window frame position from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_window_frame_position_from_config(
    root: &Value,
) -> Result<crate::terminal::TerminalFramePosition> {
    runtime_frame_position_from_config(
        root,
        "window",
        crate::terminal::TerminalFramePosition::Bottom,
    )
}

/// Runs the runtime pane frame position from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_pane_frame_position_from_config(
    root: &Value,
) -> Result<crate::terminal::TerminalFramePosition> {
    runtime_frame_position_from_config(root, "pane", crate::terminal::TerminalFramePosition::Top)
}

/// Runs the runtime window frame style from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_window_frame_style_from_config(
    root: &Value,
) -> Result<crate::terminal::TerminalFrameStyle> {
    runtime_frame_style_from_config(root, "window")
}

/// Runs the runtime pane frame style from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_pane_frame_style_from_config(
    root: &Value,
) -> Result<crate::terminal::TerminalFrameStyle> {
    runtime_frame_style_from_config(root, "pane")
}

/// Runs the runtime window frame visible fields from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_window_frame_visible_fields_from_config(root: &Value) -> Result<Vec<String>> {
    runtime_frame_visible_fields_from_config(
        root,
        "window",
        crate::terminal::DEFAULT_WINDOW_FRAME_VISIBLE_FIELDS,
    )
}

/// Runs the runtime pane frame visible fields from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_pane_frame_visible_fields_from_config(root: &Value) -> Result<Vec<String>> {
    runtime_frame_visible_fields_from_config(
        root,
        "pane",
        crate::terminal::DEFAULT_PANE_FRAME_VISIBLE_FIELDS,
    )
}

/// Runs the runtime frame template from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_frame_template_from_config(
    root: &Value,
    target: &str,
    default_template: &str,
    default_visible_fields: &[&str],
) -> Result<String> {
    let Some(frames) = runtime_json_object(root, "frames") else {
        return Ok(default_template.to_string());
    };
    let Some(frame) = frames.get(target).and_then(Value::as_object) else {
        return Ok(default_template.to_string());
    };
    if let Some(value) = frame.get("template") {
        let Some(template) = value.as_str() else {
            return Err(MezError::config(format!(
                "frames.{target}.template must be a string"
            )));
        };
        if !template.is_empty() {
            return Ok(template.to_string());
        }
    }
    let visible_fields =
        runtime_frame_visible_fields_from_config(root, target, default_visible_fields)?;
    Ok(frame_template_from_visible_fields(&visible_fields))
}

/// Runs the runtime frame position from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_frame_position_from_config(
    root: &Value,
    target: &str,
    default: crate::terminal::TerminalFramePosition,
) -> Result<crate::terminal::TerminalFramePosition> {
    let Some(frames) = runtime_json_object(root, "frames") else {
        return Ok(default);
    };
    let Some(frame) = frames.get(target).and_then(Value::as_object) else {
        return Ok(default);
    };
    let Some(value) = frame.get("position") else {
        return Ok(default);
    };
    let Some(position) = runtime_json_string(Some(value)) else {
        return Err(MezError::config(format!(
            "frames.{target}.position must be a string"
        )));
    };
    match position {
        "top" | "border" => Ok(crate::terminal::TerminalFramePosition::Top),
        "bottom" => Ok(crate::terminal::TerminalFramePosition::Bottom),
        _ => Err(MezError::config(format!(
            "frames.{target}.position must be top, bottom, or border"
        ))),
    }
}

/// Runs the runtime frame style from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_frame_style_from_config(
    root: &Value,
    target: &str,
) -> Result<crate::terminal::TerminalFrameStyle> {
    let Some(frames) = runtime_json_object(root, "frames") else {
        return Ok(crate::terminal::TerminalFrameStyle::Default);
    };
    let Some(frame) = frames.get(target).and_then(Value::as_object) else {
        return Ok(crate::terminal::TerminalFrameStyle::Default);
    };
    let Some(value) = frame.get("style") else {
        return Ok(crate::terminal::TerminalFrameStyle::Default);
    };
    let Some(style) = runtime_json_string(Some(value)) else {
        return Err(MezError::config(format!(
            "frames.{target}.style must be a string"
        )));
    };
    match style {
        "default" => Ok(crate::terminal::TerminalFrameStyle::Default),
        "bold" => Ok(crate::terminal::TerminalFrameStyle::Bold),
        "underline" => Ok(crate::terminal::TerminalFrameStyle::Underline),
        "inverse" | "reverse" => Ok(crate::terminal::TerminalFrameStyle::Inverse),
        _ => Err(MezError::config(format!(
            "frames.{target}.style must be default, bold, underline, or inverse"
        ))),
    }
}

/// Runs the runtime frame visible fields from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_frame_visible_fields_from_config(
    root: &Value,
    target: &str,
    default_visible_fields: &[&str],
) -> Result<Vec<String>> {
    let Some(frames) = runtime_json_object(root, "frames") else {
        return Ok(default_visible_fields
            .iter()
            .map(|field| (*field).to_string())
            .collect());
    };
    let Some(frame) = frames.get(target).and_then(Value::as_object) else {
        return Ok(default_visible_fields
            .iter()
            .map(|field| (*field).to_string())
            .collect());
    };
    let fields = runtime_json_string_array(frame.get("visible_fields"))?.unwrap_or_else(|| {
        default_visible_fields
            .iter()
            .map(|field| (*field).to_string())
            .collect()
    });
    Ok(fields)
}

/// Runs the frame template from visible fields operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn frame_template_from_visible_fields(fields: &[String]) -> String {
    fields
        .iter()
        .map(|field| format!("#{{{field}}}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Runs the runtime key bindings from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_key_bindings_from_config(root: &Value) -> Result<KeyBindings> {
    let Some(keys) = runtime_json_object(root, "keys") else {
        return Ok(KeyBindings::default());
    };
    let defaults = KeyBindings::default();
    Ok(KeyBindings {
        escape: runtime_key_binding_value(keys, "escape", defaults.escape)?,
        split_vertical: runtime_optional_key_binding_value(
            keys,
            "split_vertical",
            defaults.split_vertical,
        )?,
        split_horizontal: runtime_optional_key_binding_value(
            keys,
            "split_horizontal",
            defaults.split_horizontal,
        )?,
        new_window: runtime_optional_key_binding_value(keys, "new_window", defaults.new_window)?,
        new_group: runtime_optional_key_binding_value(keys, "new_group", defaults.new_group)?,
        agent_shell: runtime_optional_key_binding_value(keys, "agent_shell", defaults.agent_shell)?,
        focus_up: runtime_optional_key_binding_value(keys, "focus_up", defaults.focus_up)?,
        focus_down: runtime_optional_key_binding_value(keys, "focus_down", defaults.focus_down)?,
        focus_left: runtime_optional_key_binding_value(keys, "focus_left", defaults.focus_left)?,
        focus_right: runtime_optional_key_binding_value(keys, "focus_right", defaults.focus_right)?,
        focus_previous_window: runtime_optional_key_binding_value(
            keys,
            "focus_previous_window",
            defaults.focus_previous_window,
        )?,
        focus_next_window: runtime_optional_key_binding_value(
            keys,
            "focus_next_window",
            defaults.focus_next_window,
        )?,
        focus_previous_group: runtime_optional_key_binding_value(
            keys,
            "focus_previous_group",
            defaults.focus_previous_group,
        )?,
        focus_next_group: runtime_optional_key_binding_value(
            keys,
            "focus_next_group",
            defaults.focus_next_group,
        )?,
    })
}

/// Runs the runtime key binding value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_key_binding_value(
    keys: &serde_json::Map<String, Value>,
    field: &str,
    default: KeyChord,
) -> Result<KeyChord> {
    let Some(value) = keys.get(field) else {
        return Ok(default);
    };
    let Some(notation) = value.as_str() else {
        return Err(MezError::config(format!("keys.{field} must be a string")));
    };
    KeyChord::parse(notation)
        .map_err(|error| MezError::config(format!("keys.{field} is invalid: {error}")))
}

/// Reads an optional direct key binding from effective configuration.
///
/// Missing fields keep the generated default. A string configures the direct
/// binding, while `null` disables it explicitly.
///
/// # Parameters
/// - `keys`: The effective `[keys]` object.
/// - `field`: The direct binding field name.
/// - `default`: The generated default binding state.
pub(super) fn runtime_optional_key_binding_value(
    keys: &serde_json::Map<String, Value>,
    field: &str,
    default: Option<KeyChord>,
) -> Result<Option<KeyChord>> {
    let Some(value) = keys.get(field) else {
        return Ok(default);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(notation) = value.as_str() else {
        return Err(MezError::config(format!(
            "keys.{field} must be a string or null"
        )));
    };
    KeyChord::parse(notation)
        .map(Some)
        .map_err(|error| MezError::config(format!("keys.{field} is invalid: {error}")))
}

/// Runs the runtime command bindings from effective operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_command_bindings_from_effective(
    effective: &EffectiveConfig,
) -> Result<BTreeMap<KeyChord, RuntimeCommandBinding>> {
    let mut bindings = BTreeMap::new();
    for (path, value) in effective.values() {
        let Some(config_key) = path.strip_prefix("keys.command_bindings.") else {
            continue;
        };
        let (chord, notation) = runtime_chord_from_binding_config_key(config_key)?;
        parse_command_sequence(&value.value).map_err(|error| {
            MezError::config(format!(
                "keys.command_bindings.{config_key} command is invalid: {error}"
            ))
        })?;
        bindings.insert(
            chord,
            RuntimeCommandBinding {
                notation,
                command: value.value.clone(),
                source_layer: value.source_layer.clone(),
            },
        );
    }
    Ok(bindings)
}

/// Runs the runtime chord from binding config key operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_chord_from_binding_config_key(
    config_key: &str,
) -> Result<(KeyChord, String)> {
    let notation = if let Some(encoded) = config_key.strip_prefix("key_") {
        runtime_decode_binding_config_key(encoded)?
    } else {
        config_key.to_string()
    };
    let chord = KeyChord::parse(&notation).map_err(|error| {
        MezError::config(format!(
            "keys.command_bindings.{config_key} is not a valid key binding: {error}"
        ))
    })?;
    Ok((chord, key_chord_notation(chord)))
}

/// Runs the runtime decode binding config key operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_decode_binding_config_key(encoded: &str) -> Result<String> {
    if encoded.is_empty() {
        return Err(MezError::config("encoded key binding must not be empty"));
    }
    let mut bytes = Vec::new();
    for segment in encoded.split('_') {
        if segment.len() != 2 {
            return Err(MezError::config("encoded key binding segment is invalid"));
        }
        let byte = u8::from_str_radix(segment, 16)
            .map_err(|_| MezError::config("encoded key binding segment is not hexadecimal"))?;
        bytes.push(byte);
    }
    String::from_utf8(bytes).map_err(|_| MezError::config("encoded key binding is not valid UTF-8"))
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

/// Runs the runtime max concurrent agents from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_max_concurrent_agents_from_config(root: &Value) -> Result<usize> {
    runtime_positive_agents_usize_from_config(
        root,
        "max_concurrent_agents",
        DEFAULT_MAX_CONCURRENT_AGENTS,
    )
}

/// Parses the retained raw-tail percentage used during context compaction.
pub(super) fn runtime_agent_compaction_raw_retention_percent_from_config(
    root: &Value,
) -> Result<usize> {
    let Some(agents) = runtime_json_object(root, "agents") else {
        return Ok(DEFAULT_AGENT_COMPACTION_RAW_RETENTION_PERCENT);
    };
    let Some(value) = agents.get("compaction_raw_retention_percent") else {
        return Ok(DEFAULT_AGENT_COMPACTION_RAW_RETENTION_PERCENT);
    };
    let percent = value.as_u64().ok_or_else(|| {
        MezError::config("agents.compaction_raw_retention_percent must be an integer from 1 to 100")
    })?;
    if !(1..=100).contains(&percent) {
        return Err(MezError::config(
            "agents.compaction_raw_retention_percent must be an integer from 1 to 100",
        ));
    }
    Ok(percent as usize)
}

/// Parses whether routing model and reasoning sizing is enabled.
pub(super) fn runtime_agent_routing_from_config(root: &Value) -> Result<bool> {
    let Some(agents) = runtime_json_object(root, "agents") else {
        return Ok(DEFAULT_AGENT_ROUTING);
    };
    let Some(value) = agents.get("routing") else {
        return Ok(DEFAULT_AGENT_ROUTING);
    };
    runtime_json_bool(Some(value))
        .ok_or_else(|| MezError::config("agents.routing must be a boolean"))
}

/// Parses user-configured system prompt text appended to the base prompt.
pub(super) fn runtime_agent_custom_system_prompt_from_config(
    root: &Value,
) -> Result<Option<String>> {
    let Some(agents) = runtime_json_object(root, "agents") else {
        return Ok(None);
    };
    let Some(value) = agents.get("custom_system_prompt") else {
        return Ok(None);
    };
    let prompt = value
        .as_str()
        .ok_or_else(|| MezError::config("agents.custom_system_prompt must be a string"))?;
    Ok((!prompt.trim().is_empty()).then(|| prompt.to_string()))
}

/// Parses the configured default personality profile id.
pub(super) fn runtime_default_agent_personality_from_config(
    root: &Value,
) -> Result<Option<String>> {
    let Some(agents) = runtime_json_object(root, "agents") else {
        return Ok(None);
    };
    let Some(value) = agents.get("default_personality") else {
        return Ok(None);
    };
    let profile = value
        .as_str()
        .ok_or_else(|| MezError::config("agents.default_personality must be a string"))?;
    if profile.trim().is_empty() {
        return Ok(None);
    }
    validate_agent_personality_profile_id(profile)?;
    Ok(Some(profile.to_string()))
}

/// Parses the model-correctable action failure retry budget.
pub(super) fn runtime_agent_action_failure_retry_limit_from_config(root: &Value) -> Result<usize> {
    runtime_positive_agents_usize_from_config(
        root,
        "action_failure_retry_limit",
        DEFAULT_AGENT_ACTION_FAILURE_RETRY_LIMIT,
    )
}

/// Parses the shell-command streak that triggers implementation-pressure hints.
pub(super) fn runtime_agent_implementation_pressure_after_shell_actions_from_config(
    root: &Value,
) -> Result<usize> {
    runtime_positive_agents_usize_from_config(
        root,
        "implementation_pressure_after_shell_actions",
        DEFAULT_AGENT_IMPLEMENTATION_PRESSURE_AFTER_SHELL_ACTIONS,
    )
}

/// Parses automatic turn model-sizing configuration from `[agents.auto_sizing]`.
pub(super) fn runtime_agent_auto_sizing_from_config(
    root: &Value,
) -> Result<RuntimeAutoSizingConfig> {
    let Some(agents) = runtime_json_object(root, "agents") else {
        return Ok(RuntimeAutoSizingConfig::default());
    };
    let Some(auto_sizing) = agents.get("auto_sizing").and_then(Value::as_object) else {
        return Ok(RuntimeAutoSizingConfig::default());
    };
    let mut config = RuntimeAutoSizingConfig::default();
    if let Some(profile) = runtime_json_string(auto_sizing.get("router_model_profile")) {
        config.router_model_profile = profile.to_string();
    }
    if let Some(profile) = runtime_json_string(auto_sizing.get("small_model_profile")) {
        config.small_model_profile = profile.to_string();
    }
    if let Some(profile) = runtime_json_string(auto_sizing.get("medium_model_profile")) {
        config.medium_model_profile = profile.to_string();
    }
    if let Some(profile) = runtime_json_string(auto_sizing.get("large_model_profile")) {
        config.large_model_profile = profile.to_string();
    }
    if let Some(value) = auto_sizing.get("allowed_reasoning_efforts") {
        config.allowed_reasoning_efforts =
            runtime_json_string_array(Some(value))?.ok_or_else(|| {
                MezError::config("agents.auto_sizing.allowed_reasoning_efforts must be an array")
            })?;
        if config.allowed_reasoning_efforts.is_empty() {
            return Err(MezError::config(
                "agents.auto_sizing.allowed_reasoning_efforts must not be empty",
            ));
        }
    }
    if let Some(policy) = runtime_json_string(auto_sizing.get("fallback_policy")) {
        config.fallback_policy = match policy {
            DEFAULT_AUTO_SIZING_FALLBACK_POLICY => {
                RuntimeAutoSizingFallbackPolicy::UseDefaultProfile
            }
            other => {
                return Err(MezError::config(format!(
                    "agents.auto_sizing.fallback_policy `{other}` is not supported"
                )));
            }
        };
    }
    for (path, value) in [
        (
            "agents.auto_sizing.router_model_profile",
            config.router_model_profile.as_str(),
        ),
        (
            "agents.auto_sizing.small_model_profile",
            config.small_model_profile.as_str(),
        ),
        (
            "agents.auto_sizing.medium_model_profile",
            config.medium_model_profile.as_str(),
        ),
        (
            "agents.auto_sizing.large_model_profile",
            config.large_model_profile.as_str(),
        ),
    ] {
        if value.trim().is_empty() {
            return Err(MezError::config(format!("{path} must not be empty")));
        }
    }
    for effort in &config.allowed_reasoning_efforts {
        if !matches!(effort.as_str(), "low" | "medium" | "high" | "xhigh") {
            return Err(MezError::config(format!(
                "agents.auto_sizing.allowed_reasoning_efforts contains unsupported effort `{effort}`"
            )));
        }
    }
    Ok(config)
}

/// Parses one positive integer setting from the `[agents]` table.
fn runtime_positive_agents_usize_from_config(
    root: &Value,
    key: &str,
    default: usize,
) -> Result<usize> {
    let Some(agents) = runtime_json_object(root, "agents") else {
        return Ok(default);
    };
    let Some(value) = agents.get(key) else {
        return Ok(default);
    };
    let Some(limit) = value.as_u64() else {
        return Err(MezError::config(format!(
            "agents.{key} must be a positive integer"
        )));
    };
    let limit = usize::try_from(limit)
        .map_err(|_| MezError::config(format!("agents.{key} is too large")))?;
    if limit == 0 {
        return Err(MezError::config(format!(
            "agents.{key} must be greater than zero"
        )));
    }
    Ok(limit)
}

/// Parses the maximum number of subagent panes that may share one window.
pub(super) fn runtime_max_subagent_panes_per_window_from_config(root: &Value) -> Result<usize> {
    runtime_positive_agents_usize_from_config(
        root,
        "max_subagent_panes_per_window",
        DEFAULT_MAX_SUBAGENT_PANES_PER_WINDOW,
    )
}

/// Parses the maximum direct subagents available to a root pane agent.
pub(super) fn runtime_max_root_subagents_from_config(root: &Value) -> Result<usize> {
    runtime_positive_agents_usize_from_config(
        root,
        "max_root_subagents",
        DEFAULT_MAX_ROOT_SUBAGENTS,
    )
}

/// Parses the maximum direct subagents available to a spawned subagent.
pub(super) fn runtime_max_subagents_per_subagent_from_config(root: &Value) -> Result<usize> {
    runtime_positive_agents_usize_from_config(
        root,
        "max_subagents_per_subagent",
        DEFAULT_MAX_SUBAGENTS_PER_SUBAGENT,
    )
}

/// Parses the maximum nested subagent delegation depth.
pub(super) fn runtime_max_subagent_depth_from_config(root: &Value) -> Result<usize> {
    runtime_positive_agents_usize_from_config(root, "max_depth", DEFAULT_MAX_SUBAGENT_DEPTH)
}

/// Parses how parent agent turns wait for MAAP-spawned child subagents.
pub(super) fn runtime_subagent_wait_policy_from_config(root: &Value) -> Result<SubagentWaitPolicy> {
    let Some(agents) = runtime_json_object(root, "agents") else {
        return Ok(DEFAULT_SUBAGENT_WAIT_POLICY);
    };
    let Some(value) = agents.get("subagent_wait_policy") else {
        return Ok(DEFAULT_SUBAGENT_WAIT_POLICY);
    };
    let Some(policy) = runtime_json_string(Some(value)) else {
        return Err(MezError::config(
            "agents.subagent_wait_policy must be a string",
        ));
    };
    match policy {
        "join" | "join-and-wait" | "wait" => Ok(SubagentWaitPolicy::Join),
        "detach" | "fire-and-forget" => Ok(SubagentWaitPolicy::Detach),
        _ => Err(MezError::config(
            "agents.subagent_wait_policy must be join or detach",
        )),
    }
}

/// Runs the runtime subagent profiles from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_subagent_profiles_from_config(
    root: &Value,
) -> Result<BTreeMap<String, SubagentProfile>> {
    let mut profiles = builtin_subagent_profiles();
    let Some(configured) = runtime_json_object(root, "subagents") else {
        return Ok(profiles);
    };
    for (profile_id, value) in configured {
        validate_subagent_profile_id(profile_id)?;
        let object = value
            .as_object()
            .ok_or_else(|| MezError::config("subagent profile must be an object"))?;
        let name = runtime_json_string(object.get("name"))
            .unwrap_or(profile_id)
            .to_string();
        let description = runtime_json_string(object.get("description"))
            .unwrap_or("")
            .to_string();
        let developer_instructions = runtime_json_string(object.get("developer_instructions"))
            .or_else(|| runtime_json_string(object.get("developer_prompt")))
            .map(ToOwned::to_owned);
        let model_profile = runtime_json_string(object.get("model_profile"))
            .or_else(|| runtime_json_string(object.get("model_profile_override")))
            .map(ToOwned::to_owned);
        let permission_preset = runtime_json_string(object.get("permission_preset"))
            .or_else(|| runtime_json_string(object.get("permission_override")))
            .map(runtime_config_permission_preset)
            .transpose()?;
        let mcp_servers = runtime_json_string_array(object.get("mcp_servers"))?.unwrap_or_default();
        let shell_env = runtime_json_string_map(object.get("shell_env"))?.unwrap_or_default();
        let default_cooperation_mode = runtime_json_string(object.get("default_cooperation_mode"))
            .or_else(|| runtime_json_string(object.get("default_mode")))
            .map(runtime_cooperation_mode)
            .transpose()?;
        let default_read_scopes =
            runtime_json_string_array(object.get("default_read_scopes"))?.unwrap_or_default();
        let default_write_scopes =
            runtime_json_string_array(object.get("default_write_scopes"))?.unwrap_or_default();
        profiles.insert(
            profile_id.clone(),
            SubagentProfile {
                id: profile_id.clone(),
                name,
                description,
                developer_instructions,
                model_profile,
                permission_preset,
                mcp_servers,
                shell_env,
                default_cooperation_mode,
                default_read_scopes,
                default_write_scopes,
            },
        );
    }
    Ok(profiles)
}

/// Parses user-defined agent personality profiles.
pub(super) fn runtime_agent_personality_profiles_from_config(
    root: &Value,
) -> Result<BTreeMap<String, RuntimeAgentPersonalityProfile>> {
    let mut profiles = BTreeMap::new();
    let Some(configured) = runtime_json_object(root, "personalities") else {
        return Ok(profiles);
    };
    for (profile_id, value) in configured {
        validate_agent_personality_profile_id(profile_id)?;
        let object = value
            .as_object()
            .ok_or_else(|| MezError::config("personality profile must be an object"))?;
        let profile = RuntimeAgentPersonalityProfile {
            id: profile_id.clone(),
            name: runtime_json_string(object.get("name")).map(ToOwned::to_owned),
            system_prompt: runtime_json_string(object.get("system_prompt"))
                .or_else(|| runtime_json_string(object.get("instructions")))
                .map(ToOwned::to_owned),
            response_style: runtime_json_string(object.get("response_style"))
                .or_else(|| runtime_json_string(object.get("style")))
                .map(ToOwned::to_owned),
            model_profile: runtime_json_string(object.get("model_profile")).map(ToOwned::to_owned),
            planning_enabled: runtime_json_bool(object.get("planning_enabled"))
                .or_else(|| runtime_json_bool(object.get("planning"))),
            routing_enabled: runtime_json_bool(object.get("routing_enabled"))
                .or_else(|| runtime_json_bool(object.get("routing"))),
        };
        profiles.insert(profile_id.clone(), profile);
    }
    Ok(profiles)
}

/// Validates one configured personality profile id.
///
/// # Parameters
/// - `profile_id`: The candidate profile id from config or a slash command.
fn validate_agent_personality_profile_id(profile_id: &str) -> Result<()> {
    if profile_id.is_empty()
        || !profile_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(MezError::config(
            "personality profile id must contain only ASCII letters, digits, hyphen, or underscore",
        ));
    }
    Ok(())
}

/// Runs the validate subagent profile id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validate_subagent_profile_id(profile_id: &str) -> Result<()> {
    if profile_id.trim().is_empty()
        || profile_id.chars().any(char::is_control)
        || profile_id.contains('/')
    {
        return Err(MezError::config("subagent profile name is invalid"));
    }
    Ok(())
}

/// Runs the runtime audit log from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_audit_log_from_config(
    root: &Value,
    config_root: Option<&Path>,
) -> Result<Option<AuditLog>> {
    let Some(audit) = runtime_json_object(root, "audit") else {
        return Ok(None);
    };
    if let Some(format) = runtime_json_string(audit.get("format"))
        && format != "jsonl"
    {
        return Err(MezError::config("audit.format must be jsonl"));
    }
    let enabled = runtime_json_bool(audit.get("enabled")).unwrap_or(false);
    let required = runtime_json_bool(audit.get("required")).unwrap_or(false);
    if !enabled && !required {
        return Ok(None);
    }
    let path_text = runtime_json_string(audit.get("path")).unwrap_or("audit.jsonl");
    if path_text.trim().is_empty() {
        return Err(MezError::config("audit.path must not be empty"));
    }
    let path = PathBuf::from(path_text);
    let path = if path.is_absolute() {
        path
    } else if let Some(config_root) = config_root {
        config_root.join(path)
    } else {
        path
    };
    let retention = runtime_audit_retention_policy(audit)?;
    Ok(Some(
        AuditLog::new(AuditConfig {
            enabled,
            path,
            hash_chain: runtime_json_bool(audit.get("hash_chain")).unwrap_or(false),
            required,
        })
        .with_retention(retention),
    ))
}

/// Runs the runtime audit config present operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_audit_config_present(root: &Value) -> bool {
    runtime_json_object(root, "audit").is_some()
}

/// Runs the runtime audit retention policy operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_audit_retention_policy(
    audit: &serde_json::Map<String, Value>,
) -> Result<AuditRetentionPolicy> {
    let Some(value) = audit.get("retention_days") else {
        return Ok(AuditRetentionPolicy::disabled());
    };
    let Some(days) = value.as_u64() else {
        return Err(MezError::config(
            "audit.retention_days must be a positive integer",
        ));
    };
    if days == 0 {
        return Err(MezError::config(
            "audit.retention_days must be greater than zero",
        ));
    }
    Ok(AuditRetentionPolicy::retain_days(days))
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

/// Runs the runtime mcp registry from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_mcp_registry_from_config(root: &Value) -> Result<McpRegistry> {
    let mut registry = McpRegistry::default();
    let Some(servers) = runtime_json_object(root, "mcp_servers") else {
        return Ok(registry);
    };
    for (server_id, value) in servers {
        let Some(server) = value.as_object() else {
            return Err(MezError::config(format!(
                "mcp_servers.{server_id} must be an object"
            )));
        };
        let name = runtime_json_string(server.get("name")).unwrap_or(server_id);
        let args = runtime_json_string_array(server.get("args"))?.unwrap_or_default();
        let has_stdio_args = !args.is_empty();
        let mut config = if let Some(url) = runtime_json_string(server.get("url")) {
            McpServerConfig::streamable_http(server_id, name, url)
        } else {
            let command = runtime_json_string(server.get("command")).ok_or_else(|| {
                MezError::config(format!(
                    "mcp_servers.{server_id}.command is required for stdio transport"
                ))
            })?;
            McpServerConfig::stdio(server_id, name, command, args)
        };
        if let Some(enabled) = runtime_json_bool(server.get("enabled")) {
            config.enabled = enabled;
        }
        if config.kind == McpServerKind::Http && has_stdio_args {
            return Err(MezError::config(format!(
                "mcp_servers.{server_id}.args is only valid for stdio transport"
            )));
        }
        config.env = runtime_json_string_map(server.get("env"))?.unwrap_or_default();
        config.env_vars = runtime_json_string_array(server.get("env_vars"))?.unwrap_or_default();
        config.cwd = runtime_json_string(server.get("cwd")).map(ToOwned::to_owned);
        config.http_headers =
            runtime_json_string_map(server.get("http_headers"))?.unwrap_or_default();
        config.bearer_token_env =
            runtime_json_string(server.get("bearer_token_env")).map(ToOwned::to_owned);
        config.enabled_tools =
            runtime_json_string_array(server.get("enabled_tools"))?.unwrap_or_default();
        config.disabled_tools =
            runtime_json_string_array(server.get("disabled_tools"))?.unwrap_or_default();
        if let Some(timeout) = runtime_json_u64(server.get("startup_timeout_ms")) {
            config.startup_timeout_ms = timeout;
        } else if let Some(timeout) = runtime_json_u64(server.get("startup_timeout_sec")) {
            config.startup_timeout_ms = timeout.saturating_mul(1000);
        }
        if let Some(timeout) = runtime_json_u64(server.get("tool_timeout_ms")) {
            config.tool_timeout_ms = timeout;
        } else if let Some(timeout) = runtime_json_u64(server.get("tool_timeout_sec")) {
            config.tool_timeout_ms = timeout.saturating_mul(1000);
        }
        if let Some(approval) = runtime_json_string(server.get("approval")) {
            config.approval = runtime_mcp_approval_setting(approval)?;
        }
        if let Some(approvals) = server.get("tool_approvals") {
            config.tool_approvals = runtime_mcp_tool_approvals(approvals)?;
        }
        if let Some(external) = server.get("external_capability") {
            config.external_capability = runtime_mcp_external_capability(external)?;
        }
        registry.add_server(config)?;
    }
    Ok(registry)
}

/// Runs the runtime provider registry from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_provider_registry_from_config(
    root: &Value,
) -> Result<RuntimeProviderRegistry> {
    let agents = runtime_json_object(root, "agents");
    let default_provider = agents
        .and_then(|agents| runtime_json_string(agents.get("default_provider")))
        .unwrap_or("openai");
    let default_profile = agents
        .and_then(|agents| runtime_json_string(agents.get("default_model_profile")))
        .unwrap_or("default")
        .to_string();
    let mut registry = RuntimeProviderRegistry {
        default_profile: Some(default_profile.clone()),
        ..RuntimeProviderRegistry::default()
    };

    if let Some(providers) = runtime_json_object(root, "providers") {
        for (provider_id, value) in providers {
            let config = runtime_provider_config_from_config(provider_id, value)?;
            registry.providers.insert(provider_id.clone(), config);
        }
    }

    if registry.providers.is_empty() {
        registry.providers.insert(
            "openai".to_string(),
            RuntimeProviderConfig {
                provider_id: "openai".to_string(),
                kind: "openai".to_string(),
                api: None,
                auth_profile: "default".to_string(),
                base_url: None,
                models: runtime_default_models_for_provider("openai")?
                    .iter()
                    .map(|model| (*model).to_string())
                    .collect(),
                default_model: Some(runtime_recommended_model_for_provider("openai")?.to_string()),
                options: BTreeMap::new(),
            },
        );
    }

    let default_config = registry.providers.get(default_provider).ok_or_else(|| {
        MezError::config(format!(
            "agents.default_provider `{default_provider}` is not configured in providers"
        ))
    })?;
    let default_model = default_config
        .default_model
        .clone()
        .unwrap_or_else(|| default_config.models.first().cloned().unwrap_or_default());
    let default_model = if default_model.is_empty() {
        runtime_recommended_model_for_provider(&default_config.kind)?.to_string()
    } else {
        default_model
    };
    registry.profiles.insert(
        default_profile.clone(),
        ModelProfile {
            provider: default_provider.to_string(),
            model: default_model,
            reasoning_profile: default_config.options.get("reasoning_effort").cloned(),
            latency_preference: Some("default".to_string()),
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
    );

    for (provider_id, config) in &registry.providers {
        for model in &config.models {
            if model.is_empty() {
                continue;
            }
            registry
                .profiles
                .entry(model.clone())
                .or_insert(ModelProfile {
                    provider: provider_id.clone(),
                    model: model.clone(),
                    reasoning_profile: config.options.get("reasoning_effort").cloned(),
                    latency_preference: Some("default".to_string()),
                    multimodal_required: false,
                    provider_options: std::collections::BTreeMap::new(),
                    safety_tier: None,
                });
        }
    }

    if let Some(configured_profiles) = runtime_json_object(root, "model_profiles") {
        for (profile_name, value) in configured_profiles {
            let (profile, fallbacks) =
                runtime_model_profile_from_config(profile_name, value, &registry.providers)?;
            registry.profiles.insert(profile_name.clone(), profile);
            if !fallbacks.is_empty() {
                registry
                    .fallback_profiles
                    .insert(profile_name.clone(), fallbacks);
            }
        }
    }
    if !registry.profiles.contains_key(&default_profile) {
        return Err(MezError::config(format!(
            "agents.default_model_profile `{default_profile}` is not configured in model_profiles"
        )));
    }
    for (profile_name, fallbacks) in &registry.fallback_profiles {
        for fallback in fallbacks {
            if !registry.profiles.contains_key(fallback) {
                return Err(MezError::config(format!(
                    "model_profiles.{profile_name}.fallback_profiles references unknown model profile `{fallback}`"
                )));
            }
        }
    }

    Ok(registry)
}

/// Parses model presets from the config root.
pub(super) fn runtime_preset_registry_from_config(
    root: &Value,
    profiles: &BTreeMap<String, ModelProfile>,
) -> Result<RuntimePresetRegistry> {
    let mut registry = RuntimePresetRegistry::default();
    let Some(presets) = runtime_json_object(root, "model_presets") else {
        return Ok(registry);
    };
    for (preset_name, value) in presets {
        let object = value.as_object().ok_or_else(|| {
            MezError::config(format!("model_presets.{preset_name} must be a table"))
        })?;
        let default_model_profile = runtime_json_string(object.get("default_model_profile"))
            .ok_or_else(|| {
                MezError::config(format!(
                    "model_presets.{preset_name}.default_model_profile is required"
                ))
            })?;
        if !profiles.contains_key(default_model_profile) {
            return Err(MezError::config(format!(
                "model_presets.{preset_name}.default_model_profile `{default_model_profile}` is not configured in model_profiles"
            )));
        }
        let auto_sizing_router_model_profile = runtime_preset_model_profile_reference(
            preset_name,
            "auto_sizing_router_model_profile",
            object,
            profiles,
            default_model_profile,
        )?;
        let auto_sizing_small_model_profile = runtime_preset_model_profile_reference(
            preset_name,
            "auto_sizing_small_model_profile",
            object,
            profiles,
            default_model_profile,
        )?;
        let auto_sizing_medium_model_profile = runtime_preset_model_profile_reference(
            preset_name,
            "auto_sizing_medium_model_profile",
            object,
            profiles,
            default_model_profile,
        )?;
        let auto_sizing_large_model_profile = runtime_preset_model_profile_reference(
            preset_name,
            "auto_sizing_large_model_profile",
            object,
            profiles,
            default_model_profile,
        )?;
        let allowed_reasoning_efforts =
            runtime_json_string_array(object.get("allowed_reasoning_efforts"))?.unwrap_or_default();
        for effort in &allowed_reasoning_efforts {
            if !matches!(effort.as_str(), "low" | "medium" | "high" | "xhigh") {
                return Err(MezError::config(format!(
                    "model_presets.{preset_name}.allowed_reasoning_efforts contains unsupported effort `{effort}`"
                )));
            }
        }
        let preset = RuntimeModelPreset {
            default_model_profile: default_model_profile.to_string(),
            auto_sizing_router_model_profile,
            auto_sizing_small_model_profile,
            auto_sizing_medium_model_profile,
            auto_sizing_large_model_profile,
            allowed_reasoning_efforts,
        };
        registry.presets.insert(preset_name.clone(), preset);
    }
    Ok(registry)
}

/// Parses and validates one model-profile reference from a model preset.
fn runtime_preset_model_profile_reference(
    preset_name: &str,
    key: &str,
    object: &serde_json::Map<String, Value>,
    profiles: &BTreeMap<String, ModelProfile>,
    fallback: &str,
) -> Result<String> {
    let profile = runtime_json_string(object.get(key)).unwrap_or(fallback);
    if profile.trim().is_empty() {
        return Err(MezError::config(format!(
            "model_presets.{preset_name}.{key} must not be empty"
        )));
    }
    if !profiles.contains_key(profile) {
        return Err(MezError::config(format!(
            "model_presets.{preset_name}.{key} `{profile}` is not configured in model_profiles"
        )));
    }
    Ok(profile.to_string())
}

/// Runs the runtime model profile from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_model_profile_from_config(
    profile_name: &str,
    value: &Value,
    providers: &BTreeMap<String, RuntimeProviderConfig>,
) -> Result<(ModelProfile, Vec<String>)> {
    let Some(object) = value.as_object() else {
        return Err(MezError::config(format!(
            "model_profiles.{profile_name} must be an object"
        )));
    };
    let provider = runtime_json_string(object.get("provider")).ok_or_else(|| {
        MezError::config(format!(
            "model_profiles.{profile_name}.provider is required"
        ))
    })?;
    if !providers.contains_key(provider) {
        return Err(MezError::config(format!(
            "model_profiles.{profile_name}.provider `{provider}` is not configured"
        )));
    }
    let model = runtime_json_string(object.get("model")).ok_or_else(|| {
        MezError::config(format!("model_profiles.{profile_name}.model is required"))
    })?;
    let mut provider_options =
        runtime_json_string_map(object.get("provider_options"))?.unwrap_or_default();
    if let Some(reasoning_effort) = runtime_json_string(object.get("reasoning_effort")) {
        provider_options
            .entry("reasoning_effort".to_string())
            .or_insert_with(|| reasoning_effort.to_string());
    }
    if let Some(privacy) = runtime_json_string(object.get("privacy")) {
        provider_options
            .entry("privacy".to_string())
            .or_insert_with(|| privacy.to_string());
    }
    if let Some(privacy_tier) = runtime_json_string(object.get("privacy_tier")) {
        provider_options
            .entry("privacy_tier".to_string())
            .or_insert_with(|| privacy_tier.to_string());
    }
    if let Some(residency) = runtime_json_string(object.get("residency")) {
        provider_options
            .entry("residency".to_string())
            .or_insert_with(|| residency.to_string());
    }
    if let Some(approval) = runtime_json_string(object.get("approval")) {
        provider_options
            .entry("approval".to_string())
            .or_insert_with(|| approval.to_string());
    }
    if let Some(approval_policy) = runtime_json_string(object.get("approval_policy")) {
        provider_options
            .entry("approval_policy".to_string())
            .or_insert_with(|| approval_policy.to_string());
    }
    if let Some(context_window_tokens) =
        runtime_model_profile_context_window_tokens(profile_name, object)?
    {
        provider_options
            .entry("context_window_tokens".to_string())
            .or_insert_with(|| context_window_tokens.to_string());
    }
    if let Some(max_output_tokens) =
        runtime_model_profile_positive_token_count(profile_name, object, "max_output_tokens")?
    {
        provider_options
            .entry("max_output_tokens".to_string())
            .or_insert_with(|| max_output_tokens.to_string());
    }
    let safety_tier = runtime_json_string(object.get("safety_tier")).map(str::to_string);
    if let Some(safety_tier) = safety_tier.as_deref()
        && !matches!(safety_tier, "basic" | "medium" | "high")
    {
        return Err(MezError::config(format!(
            "model_profiles.{profile_name}.safety_tier must be basic, medium, or high"
        )));
    }
    let fallbacks = runtime_json_string_array(object.get("fallback_profiles"))?.unwrap_or_default();
    Ok((
        ModelProfile {
            provider: provider.to_string(),
            model: model.to_string(),
            reasoning_profile: runtime_json_string(object.get("reasoning_profile"))
                .or_else(|| runtime_json_string(object.get("reasoning_effort")))
                .map(str::to_string),
            latency_preference: Some(
                runtime_validate_latency_preference(
                    runtime_json_string(object.get("latency_preference")).unwrap_or("default"),
                )?
                .to_string(),
            ),
            multimodal_required: runtime_json_bool(object.get("multimodal_required"))
                .or_else(|| runtime_json_bool(object.get("multimodal")))
                .unwrap_or(false),
            provider_options,
            safety_tier,
        },
        fallbacks,
    ))
}

/// Parses model-profile context window configuration as a positive token count.
fn runtime_model_profile_context_window_tokens(
    profile_name: &str,
    object: &serde_json::Map<String, Value>,
) -> Result<Option<usize>> {
    runtime_model_profile_positive_token_count_with_aliases(
        profile_name,
        object,
        &["context_window_tokens", "context_limit_tokens"],
    )
}

/// Parses a positive model-profile token count from one key.
fn runtime_model_profile_positive_token_count(
    profile_name: &str,
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<usize>> {
    runtime_model_profile_positive_token_count_with_aliases(profile_name, object, &[key])
}

/// Parses a positive model-profile token count from one or more equivalent
/// keys.
fn runtime_model_profile_positive_token_count_with_aliases(
    profile_name: &str,
    object: &serde_json::Map<String, Value>,
    keys: &[&str],
) -> Result<Option<usize>> {
    let Some((key, value)) = keys
        .iter()
        .find_map(|key| object.get(*key).map(|value| (*key, value)))
    else {
        return Ok(None);
    };
    let tokens = if let Some(tokens) = value.as_u64() {
        tokens
    } else if let Some(tokens) = runtime_json_string(Some(value)) {
        tokens.parse::<u64>().map_err(|_| {
            MezError::config(format!(
                "model_profiles.{profile_name}.{key} must be a positive integer"
            ))
        })?
    } else {
        return Err(MezError::config(format!(
            "model_profiles.{profile_name}.{key} must be a positive integer"
        )));
    };
    let tokens = usize::try_from(tokens).map_err(|_| {
        MezError::config(format!("model_profiles.{profile_name}.{key} is too large"))
    })?;
    if tokens == 0 {
        return Err(MezError::config(format!(
            "model_profiles.{profile_name}.{key} must be greater than zero"
        )));
    }
    Ok(Some(tokens))
}

/// Runs the runtime provider config from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_provider_config_from_config(
    provider_id: &str,
    value: &Value,
) -> Result<RuntimeProviderConfig> {
    let Some(object) = value.as_object() else {
        return Err(MezError::config(format!(
            "providers.{provider_id} must be an object"
        )));
    };
    let kind = runtime_json_string(object.get("kind")).unwrap_or(provider_id);
    let api = runtime_json_string(object.get("api")).map(ToOwned::to_owned);
    effective_provider_api(kind, api.as_deref())?;
    let models = runtime_json_string_array(object.get("models"))?.unwrap_or_default();
    let default_model = runtime_json_string(object.get("default_model"))
        .filter(|model| !model.is_empty())
        .map(ToOwned::to_owned);
    let mut options = BTreeMap::new();
    if let Some(option_map) = object.get("options").and_then(Value::as_object) {
        for (key, value) in option_map {
            let Some(value) = runtime_json_string(Some(value)) else {
                return Err(MezError::config(format!(
                    "providers.{provider_id}.options.{key} must be a string"
                )));
            };
            options.insert(key.clone(), value.to_string());
        }
    }
    Ok(RuntimeProviderConfig {
        provider_id: provider_id.to_string(),
        kind: kind.to_string(),
        api,
        auth_profile: runtime_json_string(object.get("auth_profile"))
            .unwrap_or("default")
            .to_string(),
        base_url: runtime_json_string(object.get("base_url")).map(ToOwned::to_owned),
        models,
        default_model,
        options,
    })
}

/// Returns the built-in model catalog for a provider kind.
///
/// The returned slice is used when a provider's configured `models` list is
/// empty, keeping local model selection useful without requiring a live
/// provider catalog request.
pub(crate) fn runtime_default_models_for_provider(kind: &str) -> Result<&'static [&'static str]> {
    match kind {
        "openai" => Ok(&[
            "gpt-5.5",
            "gpt-5.4",
            "gpt-5.4-mini",
            "gpt-5.3-codex",
            "gpt-5.3-codex-spark",
            "gpt-5.2",
        ]),
        "deepseek" => Ok(&["deepseek-v4-pro", "deepseek-v4-flash"]),
        _ => Err(MezError::config(format!(
            "providers.{kind}.models is required for provider kind `{kind}`"
        ))),
    }
}

/// Runs the runtime recommended model for provider operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_recommended_model_for_provider(kind: &str) -> Result<&'static str> {
    runtime_default_models_for_provider(kind)?
        .first()
        .copied()
        .ok_or_else(|| MezError::config(format!("providers.{kind}.default_model is required")))
}

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
        "snapshot_create" | "SnapshotCreate" => Ok(HookEvent::SnapshotCreate),
        "snapshot_resume" | "SnapshotResume" => Ok(HookEvent::SnapshotResume),
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
pub(super) fn runtime_mcp_approval_setting(value: &str) -> Result<McpApprovalSetting> {
    match value {
        "inherit" => Ok(McpApprovalSetting::Inherit),
        "prompt" => Ok(McpApprovalSetting::Prompt),
        "allow" => Ok(McpApprovalSetting::Allow),
        "deny" | "forbid" => Ok(McpApprovalSetting::Deny),
        _ => Err(MezError::config("unsupported MCP approval setting")),
    }
}

/// Runs the runtime mcp tool approvals operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_mcp_tool_approvals(
    value: &Value,
) -> Result<BTreeMap<String, McpApprovalSetting>> {
    let object = value
        .as_object()
        .ok_or_else(|| MezError::config("mcp tool_approvals must be a map"))?;
    let mut approvals = BTreeMap::new();
    for (tool, value) in object {
        let Some(value) = value.as_str() else {
            return Err(MezError::config("mcp tool approvals must be strings"));
        };
        approvals.insert(tool.clone(), runtime_mcp_approval_setting(value)?);
    }
    Ok(approvals)
}

/// Runs the runtime mcp external capability operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_mcp_external_capability(value: &Value) -> Result<McpExternalCapability> {
    let object = value
        .as_object()
        .ok_or_else(|| MezError::config("mcp external_capability must be a map"))?;
    Ok(McpExternalCapability {
        mutates_filesystem_outside_shell: runtime_json_bool(
            object.get("mutates_filesystem_outside_shell"),
        )
        .unwrap_or(false),
        executes_processes_outside_shell: runtime_json_bool(
            object.get("executes_processes_outside_shell"),
        )
        .unwrap_or(false),
        accesses_credentials_outside_shell: runtime_json_bool(
            object.get("accesses_credentials_outside_shell"),
        )
        .unwrap_or(false),
        purpose: runtime_json_string(object.get("purpose"))
            .unwrap_or_default()
            .to_string(),
    })
}

/// Runs the optional i32 json operation for this subsystem.
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
