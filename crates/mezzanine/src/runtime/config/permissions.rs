//! Runtime permission, approval, and recipient config helpers.
//!
//! This module owns live permission-policy materialization, command-rule
//! parsing, approval decision naming, blocked-approval request summaries, and
//! message recipient parsing for runtime config consumers. Keeping these helpers
//! outside the config root separates permission-specific contracts from shared
//! JSON and scalar parsing utilities.

use std::collections::BTreeMap;

use serde_json::Value;

use mez_agent::messaging::Recipient;
use mez_agent::permissions::{
    ApprovalDecision, ApprovalPolicy, BlockedApprovalRequest, CommandRule, CommandRuleScope,
    DEFAULT_COMMAND_SHELL_CLASSIFICATION, DeclaredCommandEffects, EffectCompleteness,
    PermissionPolicy, PermissionPreset, RuleDecision, RuleMatch,
};
use mez_agent::{ActionResult, AgentTurnRecord, SubagentScopeDeclaration};
use mez_core::ids::{AgentId, PaneId, WindowId};

use crate::error::{MezError, Result};
use crate::security::sandbox::parse_sandbox_toolchain_kind;

use super::{
    runtime_cooperation_mode_name, runtime_json_bool, runtime_json_object, runtime_json_string,
    runtime_json_string_array,
};

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
            sandbox: SandboxConfig::PolicyOnly,
        }
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
}

impl SandboxConfig {
    /// Returns the stable backend name without exposing backend arguments.
    pub(crate) const fn as_str(&self) -> &'static str {
        match self {
            Self::PolicyOnly => "policy-only",
            Self::Bubblewrap(_) => "bubblewrap",
        }
    }
}

/// Reports whether configured Bubblewrap confinement applies to one effective
/// permission policy. Host access leaves the configured backend unchanged but
/// deliberately executes local shell actions outside it.
pub(crate) fn bubblewrap_applies_to_policy(
    sandbox: &SandboxConfig,
    policy: &PermissionPolicy,
) -> bool {
    matches!(sandbox, SandboxConfig::Bubblewrap(_)) && !policy.approval_policy.bypasses_sandbox()
}

/// Typed fail-closed Bubblewrap configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BubblewrapConfig {
    /// Absolute executable path resolved and probed in the pane environment.
    pub(crate) executable: String,
    /// Missing or nonfunctional Bubblewrap behavior.
    pub(crate) unavailable: SandboxUnavailablePolicy,
    /// Network namespace policy.
    pub(crate) network: BubblewrapNetworkMode,
    /// Environment reconstruction policy.
    pub(crate) environment: SandboxEnvironmentPolicy,
    /// Optional non-secret Git author name projected without host Git config.
    pub(crate) git_user_name: Option<String>,
    /// Optional non-secret Git author email projected without host Git config.
    pub(crate) git_user_email: Option<String>,
    /// Direct-user-selected built-in projections consumed by current resolvers.
    pub(crate) toolchains: Vec<SandboxToolchainKind>,
    /// Ordered built-in and constrained custom persisted selections.
    pub(crate) toolchain_selections: Vec<ToolchainSelection>,
    /// Primary-user custom definitions keyed by validated stable name.
    pub(crate) custom_toolchains: BTreeMap<String, CustomToolchainDefinition>,
}

/// One selected built-in or primary-user-defined sandbox toolchain.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ToolchainSelection {
    /// One closed code-owned descriptor identity.
    BuiltIn(SandboxToolchainKind),
    /// One constrained custom definition identity.
    Custom(CustomToolchainName),
}

impl ToolchainSelection {
    /// Parses one persisted built-in or `custom:<name>` selector.
    ///
    /// # Errors
    /// Returns a configuration error when the selector is neither a supported
    /// built-in kind nor a valid custom identity.
    pub(crate) fn parse(selector: &str) -> Result<Self> {
        if let Some(name) = selector.strip_prefix("custom:") {
            return CustomToolchainName::parse(name).map(Self::Custom);
        }
        parse_sandbox_toolchain_kind(selector)
            .map(Self::BuiltIn)
            .ok_or_else(|| MezError::config("unsupported sandbox toolchain selector"))
    }

    /// Returns the stable persisted selector spelling.
    pub(crate) fn as_str(&self) -> &str {
        match self {
            Self::BuiltIn(kind) => kind.as_str(),
            Self::Custom(name) => name.selector(),
        }
    }

    /// Constructs one validated custom selection for focused sandbox tests.
    #[cfg(test)]
    pub(crate) fn custom_for_test(name: &str) -> Result<Self> {
        Self::parse(&format!("custom:{name}"))
    }
}

/// Validated custom toolchain name and persisted selector.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct CustomToolchainName {
    name: String,
    selector: String,
}

impl CustomToolchainName {
    /// Parses one custom selector name using the schema-v32 identity contract.
    pub(crate) fn parse(name: &str) -> Result<Self> {
        if !valid_custom_toolchain_name(name) {
            return Err(MezError::config(
                "custom toolchain name must match [a-z][a-z0-9_-]{0,31} and not be reserved",
            ));
        }
        Ok(Self {
            name: name.to_string(),
            selector: format!("custom:{name}"),
        })
    }

    /// Returns the definition-map key without the selector prefix.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Returns the stable `custom:<name>` selector.
    pub(crate) fn selector(&self) -> &str {
        &self.selector
    }
}

/// One validated reference into a custom toolchain root.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct CustomToolchainReference {
    /// Zero-based index into the definition's roots.
    pub(crate) root_index: usize,
    /// Normalized lexical path relative to the selected root; `.` means root.
    pub(crate) relative_path: String,
}

impl CustomToolchainReference {
    /// Parses one lexical root-relative reference against a declared root set.
    ///
    /// # Errors
    /// Returns a configuration error for malformed syntax, traversal, absolute
    /// paths, control characters, or an out-of-range root index.
    pub(crate) fn parse(reference: &str, root_count: usize) -> Result<Self> {
        parse_custom_toolchain_reference(reference, root_count)
    }

    /// Returns the stable persisted `<root-index>:<relative-path>` spelling.
    pub(crate) fn as_str(&self) -> String {
        format!("{}:{}", self.root_index, self.relative_path)
    }
}

/// Structurally validated custom toolchain configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CustomToolchainDefinition {
    /// Optional bounded user-facing description.
    pub(crate) description: Option<String>,
    /// Absolute host roots; runtime resolution canonicalizes and authorizes them.
    pub(crate) roots: Vec<String>,
    /// Ordered PATH entries relative to declared roots.
    pub(crate) path_entries: Vec<CustomToolchainReference>,
    /// Required executables relative to declared roots.
    pub(crate) required_executables: Vec<CustomToolchainReference>,
    /// Narrow synthesized environment references in deterministic key order.
    pub(crate) environment: BTreeMap<String, CustomToolchainReference>,
}

impl CustomToolchainDefinition {
    /// Constructs one structurally validated custom toolchain definition.
    ///
    /// Filesystem existence, canonicalization, executable shape, protected
    /// roots, and pane authority remain runtime projection responsibilities.
    ///
    /// # Errors
    /// Returns a configuration error when any schema-v32 structural bound or
    /// lexical reference invariant is violated.
    pub(crate) fn new(
        description: Option<String>,
        roots: Vec<String>,
        path_entries: Vec<String>,
        required_executables: Vec<String>,
        environment: BTreeMap<String, String>,
    ) -> Result<Self> {
        if !(1..=8).contains(&roots.len())
            || roots.iter().any(|root| {
                let path = std::path::Path::new(root);
                !path.is_absolute()
                    || root.chars().any(char::is_control)
                    || path
                        .components()
                        .any(|component| matches!(component, std::path::Component::ParentDir))
            })
        {
            return Err(MezError::config(
                "custom toolchain must declare 1 to 8 absolute printable roots without lexical traversal",
            ));
        }
        if !(1..=16).contains(&path_entries.len()) || required_executables.len() > 16 {
            return Err(MezError::config(
                "custom toolchain path entries must contain 1 to 16 references and required executables at most 16 references",
            ));
        }
        if description.as_deref().is_some_and(|description| {
            description.trim().is_empty()
                || description.len() > 256
                || description.chars().any(char::is_control)
        }) {
            return Err(MezError::config(
                "custom toolchain description must be non-empty printable text of at most 256 bytes",
            ));
        }
        if environment.len() > 16
            || environment
                .keys()
                .any(|variable| !valid_custom_toolchain_environment_name(variable))
        {
            return Err(MezError::config(
                "custom toolchain environment contains too many entries or a reserved variable name",
            ));
        }
        let root_count = roots.len();
        let path_entries = path_entries
            .iter()
            .map(|reference| CustomToolchainReference::parse(reference, root_count))
            .collect::<Result<Vec<_>>>()?;
        let required_executables = required_executables
            .iter()
            .map(|reference| CustomToolchainReference::parse(reference, root_count))
            .collect::<Result<Vec<_>>>()?;
        let environment = environment
            .into_iter()
            .map(|(variable, reference)| {
                CustomToolchainReference::parse(&reference, root_count)
                    .map(|reference| (variable, reference))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        Ok(Self {
            description,
            roots,
            path_entries,
            required_executables,
            environment,
        })
    }
}

/// Allowlisted developer toolchains that may be projected read-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SandboxToolchainKind {
    /// Rustup and Cargo roots discovered from the active pane environment.
    Rust,
    /// One self-contained Zig distribution selected by the active pane.
    Zig,
    /// One self-contained Go SDK selected by the active pane.
    Go,
    /// One self-contained Deno runtime selected by the active pane.
    Deno,
    /// One self-contained Bun distribution selected by the active pane.
    Bun,
    /// One self-contained Node.js distribution selected by the active pane.
    Node,
    /// One selected Python base runtime with an optional trusted-project environment.
    Python,
    /// One selected Java Development Kit distribution.
    Jdk,
    /// One selected .NET SDK installation.
    Dotnet,
    /// One selected standalone Dart SDK installation.
    Dart,
    /// One selected standalone Kotlin/JVM compiler distribution.
    Kotlin,
    /// One selected standalone Ruby runtime distribution.
    Ruby,
}

impl SandboxToolchainKind {
    /// Returns the stable configuration spelling for this toolchain kind.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Zig => "zig",
            Self::Go => "go",
            Self::Deno => "deno",
            Self::Bun => "bun",
            Self::Node => "node",
            Self::Python => "python",
            Self::Jdk => "jdk",
            Self::Dotnet => "dotnet",
            Self::Dart => "dart",
            Self::Kotlin => "kotlin",
            Self::Ruby => "ruby",
        }
    }
}

/// Materializes structurally validated custom definitions without consulting
/// the filesystem or ambient process environment.
fn runtime_custom_toolchains_from_config(
    bubblewrap: Option<&serde_json::Map<String, Value>>,
) -> Result<BTreeMap<String, CustomToolchainDefinition>> {
    let Some(definitions) = bubblewrap
        .and_then(|config| config.get("custom_toolchains"))
        .and_then(Value::as_object)
    else {
        return Ok(BTreeMap::new());
    };
    if definitions.len() > 32 {
        return Err(MezError::config(
            "permissions.bubblewrap.custom_toolchains must contain at most 32 definitions",
        ));
    }
    let mut parsed = BTreeMap::new();
    for (name, definition) in definitions {
        CustomToolchainName::parse(name)?;
        let definition = definition
            .as_object()
            .ok_or_else(|| MezError::config("custom toolchain definition must be a mapping"))?;
        let roots = runtime_json_string_array(definition.get("roots"))?.unwrap_or_default();
        if !(1..=8).contains(&roots.len())
            || roots.iter().any(|root| {
                let path = std::path::Path::new(root);
                !path.is_absolute()
                    || root.chars().any(char::is_control)
                    || path
                        .components()
                        .any(|component| matches!(component, std::path::Component::ParentDir))
            })
        {
            return Err(MezError::config(
                "custom toolchain must declare 1 to 8 absolute printable roots without lexical traversal",
            ));
        }
        let path_entries = runtime_custom_toolchain_references(
            definition.get("path_entries"),
            roots.len(),
            1,
            16,
        )?;
        let required_executables = runtime_custom_toolchain_references(
            definition.get("required_executables"),
            roots.len(),
            0,
            16,
        )?;
        let description = definition
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_string);
        if description.as_deref().is_some_and(|description| {
            description.trim().is_empty()
                || description.len() > 256
                || description.chars().any(char::is_control)
        }) {
            return Err(MezError::config(
                "custom toolchain description must be non-empty printable text of at most 256 bytes",
            ));
        }
        let mut environment = BTreeMap::new();
        if let Some(values) = definition.get("environment") {
            let values = values.as_object().ok_or_else(|| {
                MezError::config("custom toolchain environment must be a mapping")
            })?;
            if values.len() > 16 {
                return Err(MezError::config(
                    "custom toolchain environment must contain at most 16 entries",
                ));
            }
            for (variable, reference) in values {
                if !valid_custom_toolchain_environment_name(variable) {
                    return Err(MezError::config(
                        "custom toolchain reserved environment name is not allowed",
                    ));
                }
                let reference = reference.as_str().ok_or_else(|| {
                    MezError::config(
                        "custom toolchain environment values must be root-relative references",
                    )
                })?;
                environment.insert(
                    variable.clone(),
                    parse_custom_toolchain_reference(reference, roots.len())?,
                );
            }
        }
        parsed.insert(
            name.clone(),
            CustomToolchainDefinition {
                description,
                roots,
                path_entries,
                required_executables,
                environment,
            },
        );
    }
    Ok(parsed)
}

/// Parses one bounded array of root-relative custom definition references.
fn runtime_custom_toolchain_references(
    value: Option<&Value>,
    root_count: usize,
    minimum: usize,
    maximum: usize,
) -> Result<Vec<CustomToolchainReference>> {
    let references = runtime_json_string_array(value)?.unwrap_or_default();
    if !(minimum..=maximum).contains(&references.len()) {
        return Err(MezError::config(format!(
            "custom toolchain references must contain {minimum} to {maximum} entries"
        )));
    }
    references
        .iter()
        .map(|reference| parse_custom_toolchain_reference(reference, root_count))
        .collect()
}

/// Parses one lexical root-relative reference and validates its root index.
fn parse_custom_toolchain_reference(
    reference: &str,
    root_count: usize,
) -> Result<CustomToolchainReference> {
    let (index, relative_path) = reference.split_once(':').ok_or_else(|| {
        MezError::config(
            "custom toolchain root-relative reference must use <root-index>:<relative-path>",
        )
    })?;
    let root_index = index.parse::<usize>().map_err(|_| {
        MezError::config("custom toolchain reference root index must be an integer")
    })?;
    let path = std::path::Path::new(relative_path);
    if root_index >= root_count
        || relative_path.is_empty()
        || relative_path.chars().any(char::is_control)
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(MezError::config(
            "custom toolchain root-relative reference is invalid or outside its declared root",
        ));
    }
    Ok(CustomToolchainReference {
        root_index,
        relative_path: relative_path.to_string(),
    })
}

/// Reports whether one custom identity has the stable schema-v32 shape.
fn valid_custom_toolchain_name(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name.bytes().enumerate().all(|(index, byte)| match byte {
            b'a'..=b'z' => true,
            b'0'..=b'9' | b'_' | b'-' => index > 0,
            _ => false,
        })
        && !matches!(
            name,
            "rust"
                | "zig"
                | "go"
                | "deno"
                | "bun"
                | "node"
                | "python"
                | "system"
                | "host"
                | "default"
        )
}

/// Reports whether one synthesized variable name is narrow and non-reserved.
fn valid_custom_toolchain_environment_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.bytes().enumerate().all(|(index, byte)| match byte {
            b'A'..=b'Z' | b'_' => true,
            b'0'..=b'9' => index > 0,
            _ => false,
        })
        && !matches!(name, "PATH" | "HOME" | "SHELL" | "BASH_ENV" | "ENV")
        && !["LD_", "DYLD_", "GIT_", "SSH_", "MEZ_", "MEZZANINE_"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

/// Behavior when the configured sandbox backend is unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SandboxUnavailablePolicy {
    /// Fail before launching the workload; never retry unsandboxed.
    Fail,
}

/// Bubblewrap network namespace mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BubblewrapNetworkMode {
    /// Use an isolated private network namespace.
    Isolated,
    /// Share the pane host's network namespace for an authorized workload.
    Connected,
}

impl BubblewrapNetworkMode {
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
        return Ok(ConfiguredPermissions {
            authorization: policy,
            resources: ResourceAuthorityConfig {
                read_scopes: Vec::new(),
                write_scopes: Vec::new(),
                network_policy: NetworkPolicy::Prompt,
            },
            sandbox: SandboxConfig::PolicyOnly,
        });
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

    let read_scopes =
        runtime_json_string_array(permissions.get("read_scopes"))?.unwrap_or_default();
    let write_scopes =
        runtime_json_string_array(permissions.get("write_scopes"))?.unwrap_or_default();
    validate_configured_scopes(&read_scopes, "permissions.read_scopes")?;
    validate_configured_scopes(&write_scopes, "permissions.write_scopes")?;
    let network_policy =
        match runtime_json_string(permissions.get("network_policy")).unwrap_or("prompt") {
            "deny" => NetworkPolicy::Deny,
            "prompt" => NetworkPolicy::Prompt,
            "allow" => NetworkPolicy::Allow,
            _ => return Err(MezError::config("unsupported permissions.network_policy")),
        };
    let sandbox = match runtime_json_string(permissions.get("sandbox")).unwrap_or("policy-only") {
        "policy-only" => SandboxConfig::PolicyOnly,
        "bubblewrap" => {
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
                "isolated" => BubblewrapNetworkMode::Isolated,
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
            let configured_toolchains =
                runtime_json_string_array(bubblewrap.and_then(|config| config.get("toolchains")))?
                    .unwrap_or_default();
            let mut toolchains = Vec::new();
            let mut toolchain_selections = Vec::new();
            let custom_toolchains = runtime_custom_toolchains_from_config(bubblewrap)?;
            for selector in configured_toolchains {
                let selection = if let Some(name) = selector.strip_prefix("custom:") {
                    let name = CustomToolchainName::parse(name)?;
                    if !custom_toolchains.contains_key(name.name()) {
                        return Err(MezError::config(format!(
                            "missing custom toolchain definition for `{}`",
                            name.name()
                        )));
                    }
                    ToolchainSelection::Custom(name)
                } else {
                    let kind = parse_sandbox_toolchain_kind(&selector).ok_or_else(|| {
                        MezError::config(
                            "permissions.bubblewrap.toolchains contains an unsupported kind",
                        )
                    })?;
                    toolchains.push(kind);
                    ToolchainSelection::BuiltIn(kind)
                };
                if toolchain_selections.contains(&selection) {
                    return Err(MezError::config(
                        "permissions.bubblewrap.toolchains must not contain duplicate kinds",
                    ));
                }
                toolchain_selections.push(selection);
            }
            SandboxConfig::Bubblewrap(BubblewrapConfig {
                executable,
                unavailable,
                network,
                environment,
                git_user_name,
                git_user_email,
                toolchains,
                toolchain_selections,
                custom_toolchains,
            })
        }
        _ => return Err(MezError::config("unsupported permissions.sandbox backend")),
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

fn validate_configured_scopes(scopes: &[String], field: &str) -> Result<()> {
    if scopes
        .iter()
        .any(|scope| scope.is_empty() || scope.contains('\0') || scope.starts_with('~'))
    {
        return Err(MezError::config(format!(
            "{field} must contain non-empty, unexpanded paths without NUL bytes"
        )));
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
