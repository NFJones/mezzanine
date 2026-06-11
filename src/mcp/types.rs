//! MCP configuration, state, transport plans, and response data types.
//!
//! These types model configured servers, discovered tools, startup plans, call
//! plans, prompt summaries, and transport/discovery results without owning I/O.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::{MezError, Result};

use super::protocol::build_mcp_tools_call_request;

/// Defines the DEFAULT MCP STARTUP TIMEOUT MS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const DEFAULT_MCP_STARTUP_TIMEOUT_MS: u64 = 10_000;
/// Defines the DEFAULT MCP TOOL TIMEOUT MS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const DEFAULT_MCP_TOOL_TIMEOUT_MS: u64 = 60_000;
/// Defines the DEFAULT MCP PROTOCOL VERSION const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const DEFAULT_MCP_PROTOCOL_VERSION: &str = "2025-11-25";
/// Defines the DEFAULT MCP MAX MESSAGE BYTES const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const DEFAULT_MCP_MAX_MESSAGE_BYTES: usize = 1024 * 1024;
/// Maximum number of `tools/list` pages accepted during MCP discovery.
///
/// Discovery runs on runtime-owned startup paths, so pagination must be bounded
/// even when an external server repeatedly returns a continuation cursor.
pub const DEFAULT_MCP_MAX_TOOL_LIST_PAGES: usize = 128;

/// Tracks MCP `tools/list` pagination progress and rejects non-terminating
/// cursor streams.
#[derive(Debug, Clone, Default)]
pub struct McpToolListPagination {
    /// Number of pages accepted for the current discovery request.
    pages: usize,
    /// Continuation cursors already returned by the server.
    seen_cursors: BTreeSet<String>,
}

impl McpToolListPagination {
    /// Records one listed page and returns the next cursor to request.
    ///
    /// # Parameters
    /// - `server_id`: MCP server identifier used in diagnostics.
    /// - `next_cursor`: Cursor returned by the server for the next page.
    ///
    /// # Errors
    /// Returns an error when the server exceeds the page cap, returns an empty
    /// cursor, or repeats a cursor that would cause discovery to loop.
    pub fn advance(
        &mut self,
        server_id: &str,
        next_cursor: Option<String>,
    ) -> Result<Option<String>> {
        self.pages = self.pages.saturating_add(1);
        if self.pages > DEFAULT_MCP_MAX_TOOL_LIST_PAGES {
            return Err(MezError::invalid_state(format!(
                "MCP server `{server_id}` exceeded the tools/list page limit of {DEFAULT_MCP_MAX_TOOL_LIST_PAGES}"
            )));
        }
        let Some(cursor) = next_cursor else {
            return Ok(None);
        };
        if cursor.trim().is_empty() {
            return Err(MezError::invalid_state(format!(
                "MCP server `{server_id}` returned an empty tools/list cursor"
            )));
        }
        if !self.seen_cursors.insert(cursor.clone()) {
            return Err(MezError::invalid_state(format!(
                "MCP server `{server_id}` repeated tools/list cursor `{cursor}`"
            )));
        }
        Ok(Some(cursor))
    }
}

/// Carries Mcp Server Kind state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpServerKind {
    /// Represents the Stdio case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Stdio,
    /// Represents the Http case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Http,
}

/// Carries Mcp Server Status state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpServerStatus {
    /// Represents the Configured case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Configured,
    /// Represents the Starting case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Starting,
    /// Represents the Available case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Available,
    /// Represents the Unavailable case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Unavailable,
    /// Represents the Blacklisted case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Blacklisted,
    /// Represents the Failed case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Failed,
}

/// Carries Mcp Approval Setting state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpApprovalSetting {
    /// Represents the Inherit case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Inherit,
    /// Represents the Prompt case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Prompt,
    /// Represents the Allow case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Allow,
    /// Represents the Deny case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Deny,
}

/// Carries Mcp Tool Effects state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpToolEffects {
    /// Stores the reads filesystem value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub reads_filesystem: bool,
    /// Stores the mutates filesystem value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub mutates_filesystem: bool,
    /// Stores the executes processes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub executes_processes: bool,
    /// Stores the accesses credentials value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub accesses_credentials: bool,
    /// Stores the uses network value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub uses_network: bool,
    /// Stores the has side effects value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub has_side_effects: bool,
}

impl McpToolEffects {
    /// Defines the fn const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    pub const fn none() -> Self {
        Self {
            reads_filesystem: false,
            mutates_filesystem: false,
            executes_processes: false,
            accesses_credentials: false,
            uses_network: false,
            has_side_effects: false,
        }
    }

    /// Defines the fn const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    pub const fn risky(self) -> bool {
        self.reads_filesystem
            || self.mutates_filesystem
            || self.executes_processes
            || self.accesses_credentials
            || self.uses_network
            || self.has_side_effects
    }
}

/// Carries Mcp External Capability state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpExternalCapability {
    /// Stores the mutates filesystem outside shell value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub mutates_filesystem_outside_shell: bool,
    /// Stores the executes processes outside shell value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub executes_processes_outside_shell: bool,
    /// Stores the accesses credentials outside shell value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub accesses_credentials_outside_shell: bool,
    /// Stores the purpose value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub purpose: String,
    /// Stores user-authored usage instructions for this server.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub usage_instructions: String,
}

impl McpExternalCapability {
    /// Runs the none operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn none() -> Self {
        Self {
            mutates_filesystem_outside_shell: false,
            executes_processes_outside_shell: false,
            accesses_credentials_outside_shell: false,
            purpose: String::new(),
            usage_instructions: String::new(),
        }
    }

    /// Runs the requires explicit purpose operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn requires_explicit_purpose(&self) -> bool {
        self.mutates_filesystem_outside_shell
            || self.executes_processes_outside_shell
            || self.accesses_credentials_outside_shell
    }
}

/// Carries Mcp Server Config state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerConfig {
    /// Stores the id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub id: String,
    /// Stores the name value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub name: String,
    /// Stores the kind value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub kind: McpServerKind,
    /// Stores the command value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub command: Option<String>,
    /// Stores the args value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub args: Vec<String>,
    /// Stores the env value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub env: BTreeMap<String, String>,
    /// Stores the env vars value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub env_vars: Vec<String>,
    /// Stores the cwd value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub cwd: Option<String>,
    /// Stores the url value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub url: Option<String>,
    /// Stores the http headers value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub http_headers: BTreeMap<String, String>,
    /// Stores the bearer token env value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub bearer_token_env: Option<String>,
    /// Stores the enabled value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub enabled: bool,
    /// Stores the enabled tools value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub enabled_tools: Vec<String>,
    /// Stores the disabled tools value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub disabled_tools: Vec<String>,
    /// Stores the startup timeout ms value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub startup_timeout_ms: u64,
    /// Stores the tool timeout ms value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub tool_timeout_ms: u64,
    /// Stores the approval value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub approval: McpApprovalSetting,
    /// Stores the tool approvals value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub tool_approvals: BTreeMap<String, McpApprovalSetting>,
    /// Stores the external capability value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub external_capability: McpExternalCapability,
}

impl McpServerConfig {
    /// Runs the stdio operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn stdio(
        id: impl Into<String>,
        name: impl Into<String>,
        command: impl Into<String>,
        args: Vec<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            kind: McpServerKind::Stdio,
            command: Some(command.into()),
            args,
            env: BTreeMap::new(),
            env_vars: Vec::new(),
            cwd: None,
            url: None,
            http_headers: BTreeMap::new(),
            bearer_token_env: None,
            enabled: true,
            enabled_tools: Vec::new(),
            disabled_tools: Vec::new(),
            startup_timeout_ms: DEFAULT_MCP_STARTUP_TIMEOUT_MS,
            tool_timeout_ms: DEFAULT_MCP_TOOL_TIMEOUT_MS,
            approval: McpApprovalSetting::Inherit,
            tool_approvals: BTreeMap::new(),
            external_capability: McpExternalCapability::none(),
        }
    }

    /// Runs the streamable http operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn streamable_http(
        id: impl Into<String>,
        name: impl Into<String>,
        url: impl Into<String>,
    ) -> Self {
        let mut config = Self::stdio(id, name, "", Vec::new());
        config.kind = McpServerKind::Http;
        config.command = None;
        config.url = Some(url.into());
        config
    }

    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn validate(&self) -> Result<()> {
        if self.id.is_empty() || self.name.is_empty() {
            return Err(MezError::invalid_args(
                "MCP server id and name must not be empty",
            ));
        }
        if self.startup_timeout_ms == 0 || self.tool_timeout_ms == 0 {
            return Err(MezError::invalid_args(
                "MCP startup and tool timeouts must be non-zero",
            ));
        }
        match self.kind {
            McpServerKind::Stdio if self.command.as_deref().unwrap_or("").is_empty() => {
                Err(MezError::invalid_args("stdio MCP server requires command"))
            }
            McpServerKind::Http if self.url.as_deref().unwrap_or("").is_empty() => {
                Err(MezError::invalid_args("HTTP MCP server requires url"))
            }
            McpServerKind::Http if self.command.is_some() => Err(MezError::invalid_args(
                "HTTP MCP server must not define a stdio command",
            )),
            _ => {
                if self
                    .http_headers
                    .keys()
                    .any(|key| secret_bearing_header_name(key))
                {
                    return Err(MezError::invalid_args(
                        "MCP HTTP secret-bearing headers must use environment references",
                    ));
                }
                if self
                    .env
                    .keys()
                    .any(|key| secret_bearing_environment_name(key))
                {
                    return Err(MezError::invalid_args(
                        "MCP secret-bearing environment values must use env_vars references",
                    ));
                }
                if self.external_capability.requires_explicit_purpose()
                    && self.external_capability.purpose.trim().is_empty()
                {
                    return Err(MezError::invalid_args(
                        "MCP external capabilities require an explicit purpose",
                    ));
                }
                Ok(())
            }
        }
    }

    /// Runs the tool allowed by config operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn tool_allowed_by_config(&self, tool_name: &str) -> bool {
        if self
            .disabled_tools
            .iter()
            .any(|disabled| disabled == tool_name)
        {
            return false;
        }
        self.enabled_tools.is_empty()
            || self
                .enabled_tools
                .iter()
                .any(|enabled| enabled == tool_name)
    }

    /// Runs the approval for tool operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn approval_for_tool(&self, tool_name: &str) -> McpApprovalSetting {
        self.tool_approvals
            .get(tool_name)
            .copied()
            .unwrap_or(self.approval)
    }
}

/// Carries Mcp Tool State state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpToolState {
    /// Stores the server id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub server_id: String,
    /// Stores the name value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub name: String,
    /// Stores the available value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub available: bool,
    /// Stores the blacklisted value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub blacklisted: bool,
    /// Stores the permission required value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub permission_required: bool,
    /// Stores the effects value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub effects: McpToolEffects,
    /// Stores the approval value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub approval: McpApprovalSetting,
    /// Stores the description value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub description: String,
    /// Stores the input schema json value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub input_schema_json: String,
}

/// Carries Mcp Server State state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerState {
    /// Stores the configured value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub configured: McpServerConfig,
    /// Stores the status value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub status: McpServerStatus,
    /// Stores the last checked at unix seconds value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub last_checked_at_unix_seconds: Option<u64>,
    /// Stores the blacklist reason value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub blacklist_reason: Option<String>,
    /// Stores the tools value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub tools: Vec<McpToolState>,
}

/// Carries Mcp Environment Plan state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpEnvironmentPlan {
    /// Stores the set value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub set: BTreeMap<String, String>,
    /// Stores the pass value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pass: Vec<String>,
}

/// Carries Mcp Startup Transport Plan state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpStartupTransportPlan {
    /// Represents the Stdio case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Stdio {
        /// Stores the command value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        command: String,
        /// Stores the args value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        args: Vec<String>,
        /// Stores the environment value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        environment: McpEnvironmentPlan,
        /// Stores the cwd value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        cwd: Option<String>,
    },
    /// Represents the Streamable Http case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    StreamableHttp {
        /// Stores the url value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        url: String,
        /// Stores the headers value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        headers: BTreeMap<String, String>,
        /// Stores the bearer token env value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        bearer_token_env: Option<String>,
    },
}

/// Carries Mcp Startup Plan state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpStartupPlan {
    /// Stores the server id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub server_id: String,
    /// Stores the server name value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub server_name: String,
    /// Stores the timeout ms value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub timeout_ms: u64,
    /// Stores the transport value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub transport: McpStartupTransportPlan,
}

/// Carries Mcp Tool Call Request state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpToolCallRequest {
    /// Stores the server id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub server_id: String,
    /// Stores the tool name value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub tool_name: String,
    /// Stores the arguments json value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub arguments_json: String,
    /// Stores the timeout ms value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub timeout_ms: Option<u64>,
    /// Stores the approval bypass value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub approval_bypass: bool,
}

/// Carries Mcp Tool Call Plan state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpToolCallPlan {
    /// Stores the server id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub server_id: String,
    /// Stores the tool name value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub tool_name: String,
    /// Stores the arguments json value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub arguments_json: String,
    /// Stores the timeout ms value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub timeout_ms: u64,
    /// Stores the approval required value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub approval_required: bool,
    /// Stores the audit event class value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub audit_event_class: &'static str,
    /// Stores the effects value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub effects: McpToolEffects,
}

impl McpToolCallPlan {
    /// Runs the json rpc request operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn json_rpc_request(&self, id: u64) -> Result<String> {
        build_mcp_tools_call_request(id, &self.tool_name, &self.arguments_json)
    }
}

/// Carries Mcp Initialize Response state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpInitializeResponse {
    /// Stores the protocol version value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub protocol_version: String,
    /// Stores the server name value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub server_name: String,
    /// Stores the server version value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub server_version: String,
    /// Stores the instructions value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub instructions: Option<String>,
    /// Stores the supports tools value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub supports_tools: bool,
}

/// Carries Mcp Discovered Tool state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpDiscoveredTool {
    /// Stores the name value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub name: String,
    /// Stores the title value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub title: Option<String>,
    /// Stores the description value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub description: String,
    /// Stores the input schema json value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub input_schema_json: String,
}

/// Carries Mcp Tools List Response state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpToolsListResponse {
    /// Stores the tools value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub tools: Vec<McpDiscoveredTool>,
    /// Stores the next cursor value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub next_cursor: Option<String>,
}

/// Carries Mcp Tool Call Response state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpToolCallResponse {
    /// Stores the content json value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub content_json: String,
    /// Stores the structured content json value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub structured_content_json: Option<String>,
    /// Stores the is error value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub is_error: bool,
}

/// Carries Mcp Streamable Http Response state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpStreamableHttpResponse {
    /// Stores the status code value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub status_code: u16,
    /// Stores the headers value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub headers: BTreeMap<String, String>,
    /// Stores the protocol body value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub protocol_body: String,
    /// Stores the session id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub session_id: Option<String>,
}

/// Carries Mcp Stdio Discovery state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpStdioDiscovery {
    /// Stores the initialize value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub initialize: McpInitializeResponse,
    /// Stores the tools value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub tools: Vec<McpDiscoveredTool>,
}

/// Carries Mcp Streamable Http Discovery state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpStreamableHttpDiscovery {
    /// Stores the initialize value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub initialize: McpInitializeResponse,
    /// Stores the tools value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub tools: Vec<McpDiscoveredTool>,
    /// Stores the session id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub session_id: Option<String>,
}

/// Carries Mcp Prompt Tool state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpPromptTool {
    /// Stores the server id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub server_id: String,
    /// Stores the tool name value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub tool_name: String,
    /// Stores the description value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub description: String,
    /// Stores the approval required value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub approval_required: bool,
    /// Stores the input schema json value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub input_schema_json: String,
}

/// Carries Mcp Prompt Server state for this subsystem.
///
/// The type keeps related data explicit so prompt builders can present a
/// bounded, secret-safe server manifest without parsing the full runtime
/// registry or exposing transport configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpPromptServer {
    /// Stores the server id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub server_id: String,
    /// Stores the display name value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub display_name: String,
    /// Stores the purpose value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub purpose: String,
    /// Stores user-authored usage instructions for this server.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub usage_instructions: String,
    /// Stores the tool count value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub tool_count: usize,
    /// Stores the approval required tool count value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub approval_required_tool_count: usize,
}

/// Carries Mcp Prompt Unavailable Server state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpPromptUnavailableServer {
    /// Stores the server id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub server_id: String,
    /// Stores the purpose value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub purpose: String,
    /// Stores user-authored usage instructions for this server.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub usage_instructions: String,
    /// Stores the reason value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub reason: String,
    /// Stores the retryable value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub retryable: bool,
}

/// Carries Mcp Prompt Summary state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpPromptSummary {
    /// Stores the available servers value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub available_servers: Vec<McpPromptServer>,
    /// Stores the available tools value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub available_tools: Vec<McpPromptTool>,
    /// Stores the unavailable servers value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub unavailable_servers: Vec<McpPromptUnavailableServer>,
}

/// Runs the secret bearing header name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn secret_bearing_header_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "authorization" | "proxy-authorization" | "cookie" | "set-cookie"
    )
}

/// Runs the secret bearing environment name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn secret_bearing_environment_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("token")
        || lower.contains("secret")
        || lower.contains("password")
        || lower.contains("credential")
        || lower.contains("key")
}
