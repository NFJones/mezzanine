//! Runtime permission, approval, and recipient config helpers.
//!
//! This module owns live permission-policy materialization, command-rule
//! parsing, approval decision naming, blocked-approval request summaries, and
//! message recipient parsing for runtime config consumers. Keeping these helpers
//! outside the config root separates permission-specific contracts from shared
//! JSON and scalar parsing utilities.

use std::fs;
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;

use serde_json::Value;

use mez_agent::messaging::Recipient;
use mez_agent::permissions::{
    ApprovalDecision, ApprovalPolicy, BlockedApprovalRequest, CommandRule, CommandRuleScope,
    DEFAULT_COMMAND_SHELL_CLASSIFICATION, DeclaredCommandEffects, EffectCompleteness,
    PermissionPolicy, PermissionPreset, RuleDecision, RuleMatch,
};
use mez_agent::{ActionResult, AgentTurnRecord, SubagentScopeDeclaration};
use mez_core::ids::{AgentId, PaneId, WindowId};

use super::{
    runtime_cooperation_mode_name, runtime_json_bool, runtime_json_object, runtime_json_string,
    runtime_json_string_array,
};
use crate::error::{MezError, Result};

/// Complete configured permission state before pane-environment path resolution.
#[derive(Debug, Clone)]
pub(crate) struct ConfiguredPermissions {
    /// Existing AST/prefix authorization policy.
    pub(crate) authorization: PermissionPolicy,
    /// Maximum resource authority configured for the primary agent.
    pub(crate) resources: ResourceAuthorityConfig,
    /// Optional additive confinement backend.
    pub(crate) sandbox: SandboxConfig,
}

impl Default for ConfiguredPermissions {
    fn default() -> Self {
        Self {
            authorization: PermissionPolicy::default(),
            resources: ResourceAuthorityConfig::default(),
            sandbox: default_sandbox_config(),
        }
    }
}

/// Selects the supported built-in confinement backend for this platform.
///
/// Bubblewrap is the default on Linux, where the runtime supports its launch
/// contract. Other platforms retain policy-only execution until they provide
/// an equivalent OS-level backend.
fn default_sandbox_config() -> SandboxConfig {
    if cfg!(target_os = "linux") {
        SandboxConfig::default_bubblewrap()
    } else {
        SandboxConfig::PolicyOnly
    }
}

/// Configured maximum filesystem and network authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResourceAuthorityConfig {
    /// Paths that may be projected read-only into a sandbox.
    pub(crate) read_scopes: Vec<String>,
    /// Paths that may be projected read-write into a sandbox.
    pub(crate) write_scopes: Vec<String>,
    /// Authorization policy for commands requiring network access.
    pub(crate) network_policy: NetworkPolicy,
}

impl Default for ResourceAuthorityConfig {
    fn default() -> Self {
        Self {
            read_scopes: Vec::new(),
            write_scopes: Vec::new(),
            network_policy: NetworkPolicy::Prompt,
        }
    }
}

/// Configured network authorization policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NetworkPolicy {
    /// Network-requiring commands are denied.
    Deny,
    /// Network-requiring commands require ordinary approval.
    Prompt,
    /// Network-requiring commands are authorized, subject to backend support.
    Allow,
}

impl NetworkPolicy {
    /// Returns the stable configuration spelling for status and audit output.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Deny => "deny",
            Self::Prompt => "prompt",
            Self::Allow => "allow",
        }
    }
}

/// Selected additive confinement backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SandboxConfig {
    /// Preserve existing policy-only execution behavior.
    PolicyOnly,
    /// Compile authorized commands into Bubblewrap launch plans.
    Bubblewrap(BubblewrapConfig),
    /// Compile authorized commands into macOS Seatbelt launch plans.
    Seatbelt(SeatbeltConfig),
}

impl SandboxConfig {
    /// Returns the selected OS confinement backend, if one is configured.
    pub(crate) const fn backend(&self) -> Option<SandboxBackend> {
        match self {
            Self::PolicyOnly => None,
            Self::Bubblewrap(_) => Some(SandboxBackend::Bubblewrap),
            Self::Seatbelt(_) => Some(SandboxBackend::Seatbelt),
        }
    }

    /// Returns the stable backend name without exposing backend arguments.
    pub(crate) const fn as_str(&self) -> &'static str {
        match self {
            Self::PolicyOnly => "policy-only",
            Self::Bubblewrap(_) => "bubblewrap",
            Self::Seatbelt(_) => "seatbelt",
        }
    }

    /// Builds the code-owned fail-closed Bubblewrap defaults used when a pane
    /// enables confinement without an existing global Bubblewrap profile.
    pub(crate) fn default_bubblewrap() -> Self {
        Self::Bubblewrap(BubblewrapConfig {
            executable: "/usr/bin/bwrap".to_string(),
            unavailable: SandboxUnavailablePolicy::Fail,
            network: SandboxNetworkMode::Isolated,
            environment: SandboxEnvironmentPolicy::Minimal,
            group_whitelist: ConfiguredSandboxGroups::default(),
            env_whitelist: ConfiguredSandboxEnvironment::default(),
            git_user_name: None,
            git_user_email: None,
        })
    }
}

/// Identifies one OS-level confinement implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SandboxBackend {
    /// Linux Bubblewrap namespace confinement.
    Bubblewrap,
    /// macOS Seatbelt operation-level confinement.
    Seatbelt,
}

impl SandboxBackend {
    /// Returns the stable configuration, status, and audit spelling.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Bubblewrap => "bubblewrap",
            Self::Seatbelt => "seatbelt",
        }
    }
}

/// Reports whether configured OS confinement applies to one effective
/// permission policy. Host access leaves the configured backend unchanged but
/// deliberately executes local shell actions outside it.
pub(crate) fn sandbox_applies_to_policy(
    sandbox: &SandboxConfig,
    policy: &PermissionPolicy,
) -> bool {
    sandbox.backend().is_some() && !policy.approval_policy.bypasses_sandbox()
}

/// Typed fail-closed Bubblewrap configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BubblewrapConfig {
    /// Absolute executable path resolved and probed in the pane environment.
    pub(crate) executable: String,
    /// Missing or nonfunctional Bubblewrap behavior.
    pub(crate) unavailable: SandboxUnavailablePolicy,
    /// Network namespace policy.
    pub(crate) network: SandboxNetworkMode,
    /// Environment reconstruction policy.
    pub(crate) environment: SandboxEnvironmentPolicy,
    /// Exact host supplementary groups selected by the primary user.
    pub(crate) group_whitelist: ConfiguredSandboxGroups,
    /// Pane environment variable names selected for best-effort forwarding.
    pub(crate) env_whitelist: ConfiguredSandboxEnvironment,
    /// Optional non-secret Git author name projected without host Git config.
    pub(crate) git_user_name: Option<String>,
    /// Optional non-secret Git author email projected without host Git config.
    pub(crate) git_user_email: Option<String>,
}

/// Typed fail-closed macOS Seatbelt configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SeatbeltConfig {
    /// Absolute `/usr/bin/sandbox-exec` path selected by direct user config.
    pub(crate) executable: String,
    /// Missing or nonfunctional Seatbelt behavior.
    pub(crate) unavailable: SandboxUnavailablePolicy,
    /// Effective socket-access policy compiled by the Seatbelt backend.
    pub(crate) network: SandboxNetworkMode,
    /// Environment reconstruction policy.
    pub(crate) environment: SandboxEnvironmentPolicy,
    /// Pane environment variable names selected for best-effort forwarding.
    pub(crate) env_whitelist: ConfiguredSandboxEnvironment,
    /// Optional non-secret Git author name projected without host Git config.
    pub(crate) git_user_name: Option<String>,
    /// Optional non-secret Git author email projected without host Git config.
    pub(crate) git_user_email: Option<String>,
}

/// Primary-user-selected host group names projected into Bubblewrap.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ConfiguredSandboxGroups {
    /// Ordered configured names retained for diagnostics and host resolution.
    pub(crate) requested_names: Vec<String>,
}

impl ConfiguredSandboxGroups {
    /// Maximum number of supplementary groups accepted by the launcher protocol.
    pub(crate) const MAX_GROUPS: usize = 64;
    /// Maximum total UTF-8 bytes accepted from configured group names.
    pub(crate) const MAX_ENCODED_BYTES: usize = 8 * 1024;

    /// Parses and validates the static configured group-name contract.
    fn parse(names: Vec<String>) -> Result<Self> {
        if names.len() > Self::MAX_GROUPS {
            return Err(MezError::config(
                "permissions.bubblewrap.group_whitelist must contain at most 64 names",
            ));
        }
        let mut encoded_bytes = 0usize;
        let mut seen = std::collections::BTreeSet::new();
        for name in &names {
            encoded_bytes = encoded_bytes.saturating_add(name.len());
            if name.is_empty() || name.chars().any(char::is_control) {
                return Err(MezError::config(
                    "permissions.bubblewrap.group_whitelist must contain non-empty printable names",
                ));
            }
            if name.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(MezError::config(
                    "permissions.bubblewrap.group_whitelist does not accept numeric GIDs",
                ));
            }
            if !seen.insert(name.clone()) {
                return Err(MezError::config(
                    "permissions.bubblewrap.group_whitelist must not contain duplicate names",
                ));
            }
        }
        if encoded_bytes > Self::MAX_ENCODED_BYTES {
            return Err(MezError::config(
                "permissions.bubblewrap.group_whitelist exceeds the 8 KiB input limit",
            ));
        }
        Ok(Self {
            requested_names: names,
        })
    }
}

/// Primary-user-selected pane environment variable names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfiguredSandboxEnvironment {
    /// Ordered configured names retained without reading controller environment state.
    pub(crate) requested_names: Vec<String>,
}

impl Default for ConfiguredSandboxEnvironment {
    /// Selects the active pane command-search path when no explicit list exists.
    fn default() -> Self {
        Self {
            requested_names: vec!["PATH".to_string()],
        }
    }
}

impl ConfiguredSandboxEnvironment {
    /// Maximum number of selected variables.
    pub(crate) const MAX_VARIABLES: usize = 128;
    /// Maximum total bytes across selected names.
    pub(crate) const MAX_ENCODED_BYTES: usize = 16 * 1024;

    /// Parses the portable, bounded configured variable-name contract.
    fn parse(names: Vec<String>) -> Result<Self> {
        if names.len() > Self::MAX_VARIABLES {
            return Err(MezError::config(
                "permissions.bubblewrap.env_whitelist must contain at most 128 names",
            ));
        }
        let mut bytes = 0usize;
        let mut seen = std::collections::BTreeSet::new();
        for name in &names {
            bytes = bytes.saturating_add(name.len());
            let mut characters = name.chars();
            let valid = characters
                .next()
                .is_some_and(|character| character == '_' || character.is_ascii_alphabetic())
                && characters
                    .all(|character| character == '_' || character.is_ascii_alphanumeric());
            if !valid {
                return Err(MezError::config(
                    "permissions.bubblewrap.env_whitelist names must match [A-Za-z_][A-Za-z0-9_]*",
                ));
            }
            if !seen.insert(name.clone()) {
                return Err(MezError::config(
                    "permissions.bubblewrap.env_whitelist must not contain duplicate names",
                ));
            }
        }
        if bytes > Self::MAX_ENCODED_BYTES {
            return Err(MezError::config(
                "permissions.bubblewrap.env_whitelist exceeds the 16 KiB name limit",
            ));
        }
        Ok(Self {
            requested_names: names,
        })
    }
}

/// Behavior when the configured sandbox backend is unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SandboxUnavailablePolicy {
    /// Fail before launching the workload; never retry unsandboxed.
    Fail,
}

/// Effective network boundary requested from a sandbox backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SandboxNetworkMode {
    /// Deny host network access using the selected backend's mechanism.
    Isolated,
    /// Permit host network access for an authorized workload.
    Connected,
}

impl SandboxNetworkMode {
    /// Returns the stable audit spelling for this network mode.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Isolated => "isolated",
            Self::Connected => "connected",
        }
    }
}

/// Sandbox environment projection policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SandboxEnvironmentPolicy {
    /// Clear inherited variables and rebuild a fixed non-secret environment.
    Minimal,
}

/// Runs the runtime approval decision name to kind operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_approval_decision_name_to_kind(value: &str) -> Option<ApprovalDecision> {
    match value {
        "approve" | "allow" => Some(ApprovalDecision::Approve),
        "disapprove" | "deny" | "reject" => Some(ApprovalDecision::Disapprove),
        "redirect" => Some(ApprovalDecision::Redirect),
        _ => None,
    }
}

/// Runs the runtime message recipient operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_message_recipient(value: &str) -> Result<Recipient> {
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
pub(crate) fn runtime_blocked_approval_request(
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
        state: mez_agent::permissions::BlockedApprovalState::Pending,
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

/// Materializes authorization, maximum resource authority, and the optional
/// sandbox backend from one effective configuration tree.
pub(crate) fn runtime_configured_permissions_from_config(
    root: &Value,
) -> Result<ConfiguredPermissions> {
    let mut policy = PermissionPolicy::default();
    let Some(permissions) = runtime_json_object(root, "permissions") else {
        return Ok(ConfiguredPermissions::default());
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
    let read_scopes =
        runtime_json_string_array(permissions.get("read_scopes"))?.unwrap_or_default();
    let write_scopes =
        runtime_json_string_array(permissions.get("write_scopes"))?.unwrap_or_default();
    validate_configured_scopes(&read_scopes, "permissions.read_scopes", true)?;
    validate_configured_scopes(&write_scopes, "permissions.write_scopes", false)?;
    let network_policy =
        match runtime_json_string(permissions.get("network_policy")).unwrap_or("prompt") {
            "deny" => NetworkPolicy::Deny,
            "prompt" => NetworkPolicy::Prompt,
            "allow" => NetworkPolicy::Allow,
            _ => return Err(MezError::config("unsupported permissions.network_policy")),
        };
    let sandbox = match runtime_json_string(permissions.get("sandbox")) {
        None => default_sandbox_config(),
        Some("policy-only") => SandboxConfig::PolicyOnly,
        Some("bubblewrap") => {
            let bubblewrap = permissions.get("bubblewrap").and_then(Value::as_object);
            let executable = bubblewrap
                .and_then(|config| runtime_json_string(config.get("executable")))
                .unwrap_or("/usr/bin/bwrap")
                .to_string();
            if !executable.starts_with('/')
                || executable.bytes().any(|byte| byte.is_ascii_control())
            {
                return Err(MezError::config(
                    "permissions.bubblewrap.executable must be an absolute printable path",
                ));
            }
            let unavailable = match bubblewrap
                .and_then(|config| runtime_json_string(config.get("unavailable")))
                .unwrap_or("fail")
            {
                "fail" => SandboxUnavailablePolicy::Fail,
                _ => {
                    return Err(MezError::config(
                        "permissions.bubblewrap.unavailable must be fail",
                    ));
                }
            };
            let network = match bubblewrap
                .and_then(|config| runtime_json_string(config.get("network")))
                .unwrap_or("isolated")
            {
                "isolated" => SandboxNetworkMode::Isolated,
                _ => {
                    return Err(MezError::config(
                        "permissions.bubblewrap.network must be isolated",
                    ));
                }
            };
            let environment = match bubblewrap
                .and_then(|config| runtime_json_string(config.get("environment")))
                .unwrap_or("minimal")
            {
                "minimal" => SandboxEnvironmentPolicy::Minimal,
                _ => {
                    return Err(MezError::config(
                        "permissions.bubblewrap.environment must be minimal",
                    ));
                }
            };
            let group_whitelist = ConfiguredSandboxGroups::parse(
                runtime_json_string_array(
                    bubblewrap.and_then(|config| config.get("group_whitelist")),
                )?
                .unwrap_or_default(),
            )?;
            let env_whitelist = ConfiguredSandboxEnvironment::parse(
                runtime_json_string_array(
                    bubblewrap.and_then(|config| config.get("env_whitelist")),
                )?
                .unwrap_or_else(|| ConfiguredSandboxEnvironment::default().requested_names),
            )?;
            let git_user_name = bubblewrap
                .and_then(|config| runtime_json_string(config.get("git_user_name")))
                .map(str::to_string);
            let git_user_email = bubblewrap
                .and_then(|config| runtime_json_string(config.get("git_user_email")))
                .map(str::to_string);
            if git_user_name.is_some() != git_user_email.is_some() {
                return Err(MezError::config(
                    "permissions.bubblewrap.git_user_name and git_user_email must be configured together",
                ));
            }
            for (field, value) in [
                ("git_user_name", git_user_name.as_deref()),
                ("git_user_email", git_user_email.as_deref()),
            ] {
                if value.is_some_and(|value| {
                    value.trim().is_empty() || value.chars().any(char::is_control)
                }) {
                    return Err(MezError::config(format!(
                        "permissions.bubblewrap.{field} must be non-empty printable text"
                    )));
                }
            }
            SandboxConfig::Bubblewrap(BubblewrapConfig {
                executable,
                unavailable,
                network,
                environment,
                group_whitelist,
                env_whitelist,
                git_user_name,
                git_user_email,
            })
        }
        Some("seatbelt") => {
            let seatbelt = permissions.get("seatbelt").and_then(Value::as_object);
            let executable = seatbelt
                .and_then(|config| runtime_json_string(config.get("executable")))
                .unwrap_or("/usr/bin/sandbox-exec")
                .to_string();
            if !executable.starts_with('/')
                || executable.bytes().any(|byte| byte.is_ascii_control())
            {
                return Err(MezError::config(
                    "permissions.seatbelt.executable must be an absolute printable path",
                ));
            }
            let unavailable = match seatbelt
                .and_then(|config| runtime_json_string(config.get("unavailable")))
                .unwrap_or("fail")
            {
                "fail" => SandboxUnavailablePolicy::Fail,
                _ => {
                    return Err(MezError::config(
                        "permissions.seatbelt.unavailable must be fail",
                    ));
                }
            };
            let network = match seatbelt
                .and_then(|config| runtime_json_string(config.get("network")))
                .unwrap_or("isolated")
            {
                "isolated" => SandboxNetworkMode::Isolated,
                _ => {
                    return Err(MezError::config(
                        "permissions.seatbelt.network must be isolated",
                    ));
                }
            };
            let environment = match seatbelt
                .and_then(|config| runtime_json_string(config.get("environment")))
                .unwrap_or("minimal")
            {
                "minimal" => SandboxEnvironmentPolicy::Minimal,
                _ => {
                    return Err(MezError::config(
                        "permissions.seatbelt.environment must be minimal",
                    ));
                }
            };
            let env_whitelist = ConfiguredSandboxEnvironment::parse(
                runtime_json_string_array(seatbelt.and_then(|config| config.get("env_whitelist")))?
                    .unwrap_or_else(|| ConfiguredSandboxEnvironment::default().requested_names),
            )?;
            let git_user_name = seatbelt
                .and_then(|config| runtime_json_string(config.get("git_user_name")))
                .map(str::to_string);
            let git_user_email = seatbelt
                .and_then(|config| runtime_json_string(config.get("git_user_email")))
                .map(str::to_string);
            if git_user_name.is_some() != git_user_email.is_some() {
                return Err(MezError::config(
                    "permissions.seatbelt.git_user_name and git_user_email must be configured together",
                ));
            }
            for (field, value) in [
                ("git_user_name", git_user_name.as_deref()),
                ("git_user_email", git_user_email.as_deref()),
            ] {
                if value.is_some_and(|value| {
                    value.trim().is_empty() || value.chars().any(char::is_control)
                }) {
                    return Err(MezError::config(format!(
                        "permissions.seatbelt.{field} must be non-empty printable text"
                    )));
                }
            }
            SandboxConfig::Seatbelt(SeatbeltConfig {
                executable,
                unavailable,
                network,
                environment,
                env_whitelist,
                git_user_name,
                git_user_email,
            })
        }
        Some(_) => return Err(MezError::config("unsupported permissions.sandbox backend")),
    };

    Ok(ConfiguredPermissions {
        authorization: policy,
        resources: ResourceAuthorityConfig {
            read_scopes,
            write_scopes,
            network_policy,
        },
        sandbox,
    })
}

fn validate_configured_scopes(
    scopes: &[String],
    field: &str,
    allow_unix_sockets: bool,
) -> Result<()> {
    if scopes
        .iter()
        .any(|scope| scope.is_empty() || scope.contains('\0') || scope.starts_with('~'))
    {
        return Err(MezError::config(format!(
            "{field} must contain non-empty, unexpanded paths without NUL bytes"
        )));
    }
    for scope in scopes.iter().filter(|scope| Path::new(scope).is_absolute()) {
        match fs::metadata(scope) {
            Ok(metadata) if metadata.is_file() || metadata.is_dir() => {}
            #[cfg(unix)]
            Ok(metadata) if allow_unix_sockets && metadata.file_type().is_socket() => {}
            Ok(_) => {
                return Err(MezError::config(format!(
                    "{field} entry `{scope}` must reference a regular file or directory{}",
                    if allow_unix_sockets {
                        ", or a Unix-domain socket"
                    } else {
                        ""
                    }
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(MezError::config(format!(
                    "failed to inspect {field} entry `{scope}`: {error}"
                )));
            }
        }
    }
    Ok(())
}

/// Runs the runtime provider registry from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
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
    if let Some(id) = runtime_json_string(object.get("id")) {
        rule = rule
            .with_id(id)
            .map_err(|error| MezError::config(error.message()))?;
    }
    if let Some(effects) = object.get("effects") {
        let effects = effects
            .as_object()
            .ok_or_else(|| MezError::config("permission command rule effects must be an object"))?;
        let completeness =
            match runtime_json_string(effects.get("completeness")).unwrap_or("unknown") {
                "unknown" => EffectCompleteness::Unknown,
                "complete" => EffectCompleteness::Complete,
                _ => {
                    return Err(MezError::config(
                        "unsupported command rule effect completeness",
                    ));
                }
            };
        rule = rule
            .with_declared_effects(DeclaredCommandEffects {
                completeness,
                read_scopes: runtime_json_string_array(effects.get("read_scopes"))?
                    .unwrap_or_default(),
                write_scopes: runtime_json_string_array(effects.get("write_scopes"))?
                    .unwrap_or_default(),
                network: runtime_json_bool(effects.get("network")),
                credentials: runtime_json_bool(effects.get("credentials")),
                process_control: runtime_json_bool(effects.get("process_control")),
            })
            .map_err(|error| MezError::config(error.message()))?;
    }
    Ok(rule)
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
pub(crate) fn runtime_config_permission_preset(value: &str) -> Result<PermissionPreset> {
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
        "host-access" | "host_access" => Ok(ApprovalPolicy::HostAccess),
        _ => Err(MezError::config(
            "unsupported approval policy; use ask, auto-allow, full-access, or host-access",
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
