//! Agent Maap implementation.
//!
//! This module owns the agent maap boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::semantic::{apply_patch_touched_paths, try_convert_unified_diff_to_mez_patch};
use super::shell::validate_agent_authored_shell_command;
use super::{AgentCapability, AgentTurnRecord, BTreeSet, McpPromptTool, MezError, Result};
use serde_json::Value;

// MAAP action and result data structures.

/// Canonical content type for plain model-authored user-facing text.
pub const AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE: &str = "text/plain; charset=utf-8";
/// Canonical content type for markdown model-authored user-facing text.
pub const AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE: &str = "text/markdown; charset=utf-8";
/// Canonical content type for diff model-authored user-facing text.
pub const AGENT_OUTPUT_TEXT_DIFF_CONTENT_TYPE: &str = "text/x-diff; charset=utf-8";

/// Normalizes the optional media type on a model-authored user-facing action.
pub fn normalize_agent_output_content_type(content_type: Option<&str>) -> String {
    match content_type
        .map(str::trim)
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "" | "text/plain" | "text/plain;charset=utf-8" | "text/plain; charset=utf-8" => {
            AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string()
        }
        "text/markdown" | "text/markdown;charset=utf-8" | "text/markdown; charset=utf-8" => {
            AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE.to_string()
        }
        "text/diff"
        | "text/diff;charset=utf-8"
        | "text/diff; charset=utf-8"
        | "text/x-diff"
        | "text/x-diff;charset=utf-8"
        | "text/x-diff; charset=utf-8" => AGENT_OUTPUT_TEXT_DIFF_CONTENT_TYPE.to_string(),
        _ => content_type.unwrap_or_default().trim().to_string(),
    }
}

/// Returns whether a normalized or raw media type should use markdown display.
pub fn agent_output_content_type_is_markdown(content_type: &str) -> bool {
    normalize_agent_output_content_type(Some(content_type))
        == AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE
}

/// Returns whether a normalized or raw media type should use diff display.
pub fn agent_output_content_type_is_diff(content_type: &str) -> bool {
    normalize_agent_output_content_type(Some(content_type)) == AGENT_OUTPUT_TEXT_DIFF_CONTENT_TYPE
}

/// Declares whether a `say` action is conversational progress, a final answer,
/// or a terminal blocker that requires user/external input before progress can
/// continue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SayStatus {
    /// Informational text while the task remains active.
    Progress,
    /// Final user-facing response for a completed goal.
    Final,
    /// Terminal user-facing blocker that is not an approval wait.
    Blocked,
}

impl SayStatus {
    /// Parses the provider-authored status string for a `say` action.
    ///
    /// # Parameters
    /// - `value`: Raw JSON string value from the provider response.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "progress" => Some(Self::Progress),
            "final" => Some(Self::Final),
            "blocked" => Some(Self::Blocked),
            _ => None,
        }
    }

    /// Returns the compact JSON spelling for this status.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Progress => "progress",
            Self::Final => "final",
            Self::Blocked => "blocked",
        }
    }

    /// Returns whether this status is terminal for action-batch inference.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Final | Self::Blocked)
    }
}

/// Carries Agent Action Payload state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentActionPayload {
    /// Represents the Say case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Say {
        /// Indicates whether this visible text is progress, final, or blocked.
        status: SayStatus,
        /// Stores the text value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        text: String,
        /// HTTP-style media type for `text`.
        ///
        /// The runtime uses this only for presentation decisions; transcript
        /// and copy paths preserve the raw `text` value.
        content_type: String,
    },
    /// Requests a coarse capability without executing an external effect.
    RequestCapability {
        /// Capability name requested by the model.
        capability: AgentCapability,
        /// Model-authored explanation for why the capability is needed.
        reason: String,
    },
    /// Requests the effective skill catalog for the current pane context.
    RequestSkills,
    /// Requests that one named skill be loaded into model context.
    CallSkill {
        /// Skill name to resolve from the effective catalog.
        name: String,
        /// Optional semantic argument appended under an Additional context
        /// heading when the skill is loaded.
        additional_context: Option<String>,
    },
    /// Represents the Shell Command case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ShellCommand {
        /// Stores the summary value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        summary: String,
        /// Stores the command value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        command: String,
        /// Stores the interactive value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        interactive: bool,
        /// Stores the stateful value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        stateful: bool,
        /// Stores the timeout ms value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        timeout_ms: Option<u64>,
    },
    /// Applies a patch through the pane shell.
    ApplyPatch {
        /// Mezzanine patch block beginning with `*** Begin Patch`.
        patch: String,
        /// Optional strip count. Unsupported for Mezzanine `apply_patch`.
        strip: Option<u64>,
    },
    /// Performs a provider-independent web search through runtime HTTP.
    WebSearch {
        /// Search query.
        query: String,
        /// Optional domains to constrain the search query.
        domains: Vec<String>,
        /// Optional recency filter in days when supported by the backend.
        recency_days: Option<u64>,
        /// Optional maximum result count.
        max_results: Option<u64>,
    },
    /// Fetches one HTTP(S) URL through runtime HTTP.
    FetchUrl {
        /// URL to fetch.
        url: String,
        /// Optional response format hint.
        format: Option<String>,
        /// Optional maximum number of bytes to print.
        max_bytes: Option<u64>,
    },
    /// Searches persistent memory through the runtime-owned memory store.
    MemorySearch {
        /// Search query used for memory FTS and deterministic retrieval.
        query: String,
        /// Maximum number of records to return.
        limit: Option<u64>,
    },
    /// Stores one agent-authored persistent memory through the runtime.
    MemoryStore {
        /// Durable memory taxonomy label.
        kind: String,
        /// Retrieval priority from 0 to 100.
        priority: Option<u64>,
        /// Optional target scope hint: global or project.
        scope: Option<String>,
        /// Search/index terms stored with the memory content.
        keywords: Vec<String>,
        /// Durable memory body.
        content: String,
        /// Optional retention period in days.
        expires_in_days: Option<u64>,
    },
    /// Adds one local project issue through the runtime-owned issue store.
    IssueAdd {
        /// Issue kind: defect or task.
        kind: String,
        /// Single-line issue title.
        title: String,
        /// Optional issue detail text.
        body: Option<String>,
        /// Optional mutable progress or handoff notes.
        notes: Option<String>,
        /// Issue ids that this new issue depends on.
        depends_on: Vec<String>,
    },
    /// Updates one local project issue through the runtime-owned issue store.
    IssueUpdate {
        /// Issue id to update.
        id: String,
        /// Optional replacement issue kind: defect or task.
        kind: Option<String>,
        /// Optional replacement single-line issue title.
        title: Option<String>,
        /// Optional replacement issue detail text.
        body: Option<String>,
        /// Clear existing issue detail text.
        clear_body: bool,
        /// Optional replacement mutable progress or handoff notes.
        notes: Option<String>,
        /// Clear existing mutable progress or handoff notes.
        clear_notes: bool,
        /// Optional replacement dependency issue ids.
        depends_on: Option<Vec<String>>,
        /// Clear existing dependency issue ids.
        clear_depends_on: bool,
    },
    /// Queries local project issues through the runtime-owned issue store.
    IssueQuery {
        /// Optional issue kind filter: defect or task.
        kind: Option<String>,
        /// Optional title/body substring filter.
        text: Option<String>,
        /// Optional maximum records to return.
        limit: Option<u64>,
    },
    /// Deletes one local project issue through the runtime-owned issue store.
    IssueDelete {
        /// Issue id to delete.
        id: String,
    },
    /// Represents the Send Message case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    SendMessage {
        /// Stores the recipient value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        recipient: String,
        /// Stores the content type value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        content_type: String,
        /// Stores the payload value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        payload: String,
    },
    /// Represents the Spawn Agent case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    SpawnAgent {
        /// Stores the role value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        role: String,
        /// Stores the placement value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        placement: String,
        /// Stores the cooperation mode value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        cooperation_mode: String,
        /// Stores the read scopes value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        read_scopes: Vec<String>,
        /// Stores the write scopes value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        write_scopes: Vec<String>,
        /// Stores the task prompt value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        task_prompt: String,
    },
    /// Represents the Config Change case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ConfigChange {
        /// Stores the setting path value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        setting_path: String,
        /// Stores the operation value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        operation: String,
        /// Stores the value value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        value: Option<String>,
    },
    /// Represents the Mcp Call case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    McpCall {
        /// Stores the server value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        server: String,
        /// Stores the tool value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        tool: String,
        /// Stores the arguments json value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        arguments_json: String,
    },
    /// Represents the Complete case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Complete,
    /// Represents the Abort case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Abort {
        /// Stores the reason value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reason: String,
    },
}

/// Carries Agent Action state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentAction {
    /// Stores the id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub id: String,
    /// Stores the rationale value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub rationale: String,
    /// Stores the payload value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub payload: AgentActionPayload,
}

/// Carries Maap Batch state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaapBatch {
    /// Stores the protocol value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub protocol: String,
    /// Model-authored rationale for the action batch.
    ///
    /// The field summarizes why the listed actions are being pursued and is
    /// rendered once as user-visible thinking text before action execution.
    pub rationale: String,
    /// Optional durable model-authored work note for the action batch.
    ///
    /// The field carries longer reasoning summaries, learned facts, or
    /// decisions that should persist into future model context without being
    /// rendered in normal-mode pane logs.
    pub thought: Option<String>,
    /// Stores the turn id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub turn_id: String,
    /// Stores the agent id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub agent_id: String,
    /// Stores the actions value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub actions: Vec<AgentAction>,
    /// Stores the final turn value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub final_turn: bool,
}

/// Carries Action Status state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionStatus {
    /// Represents the Rejected case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Rejected,
    /// Represents the Blocked case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Blocked,
    /// Represents the Denied case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Denied,
    /// Represents the Running case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Running,
    /// Represents the Succeeded case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Succeeded,
    /// Represents the Failed case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Failed,
    /// Represents the Cancelled case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Cancelled,
    /// Represents the Timed Out case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    TimedOut,
    /// Represents the Interrupted case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Interrupted,
}

/// Carries Action Error state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionError {
    /// Stores the code value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub code: String,
    /// Stores the message value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub message: String,
    /// Stores the data json value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub data_json: Option<String>,
}

/// Carries Action Content Block state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionContentBlock {
    /// Stores the block type value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub block_type: &'static str,
    /// Stores the text value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub text: String,
}

impl ActionContentBlock {
    /// Runs the text operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            block_type: "text",
            text: text.into(),
        }
    }

    /// Runs the to json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn to_json(&self) -> String {
        let json = serde_json::json!({
            "type": self.block_type,
            "text": self.text,
        });
        json.to_string()
    }
}

/// Carries Action Result state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionResult {
    /// Stores the protocol value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub protocol: String,
    /// Stores the turn id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub turn_id: String,
    /// Stores the agent id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub agent_id: String,
    /// Stores the action id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub action_id: String,
    /// Stores the action type value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub action_type: &'static str,
    /// Stores the status value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub status: ActionStatus,
    /// Stores the content value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub content: Vec<ActionContentBlock>,
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
    /// Stores the error value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub error: Option<ActionError>,
}

impl MaapBatch {
    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn validate(
        &self,
        turn: &AgentTurnRecord,
        available_mcp_servers: &[String],
        available_mcp_tools: &[McpPromptTool],
    ) -> Result<()> {
        if self.protocol != "maap/1" {
            return Err(MezError::invalid_args(
                "agent action batch protocol must be maap/1",
            ));
        }
        if self.rationale.trim().is_empty() {
            return Err(MezError::invalid_args(
                "agent action batch rationale must not be empty",
            ));
        }
        if self.turn_id != turn.turn_id || self.agent_id != turn.agent_id {
            return Err(MezError::invalid_args(
                "agent action batch identity does not match active turn",
            ));
        }
        if self.actions.is_empty() && !self.final_turn {
            return Err(MezError::invalid_args(
                "agent action batch must include actions unless it is final",
            ));
        }
        let non_say_count = self
            .actions
            .iter()
            .filter(|a| !matches!(a.payload, AgentActionPayload::Say { .. }))
            .count();
        let say_count = self
            .actions
            .iter()
            .filter(|a| matches!(&a.payload, AgentActionPayload::Say { text, .. } if !text.trim().is_empty()))
            .count();
        if non_say_count == 0 && say_count == 0 && !self.final_turn {
            return Err(MezError::invalid_args(
                "agent action batch must include at least one non-empty action unless it is final",
            ));
        }

        let mut ids = BTreeSet::new();
        for action in &self.actions {
            validate_non_empty("action id", &action.id)?;
            if !ids.insert(action.id.as_str()) {
                return Err(MezError::invalid_args(
                    "agent action batch contains duplicate action ids",
                ));
            }
        }
        for action in &self.actions {
            action.validate(available_mcp_servers, available_mcp_tools)?;
        }
        Ok(())
    }
}

impl AgentAction {
    /// Runs the action type operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn action_type(&self) -> &'static str {
        match self.payload {
            AgentActionPayload::Say { .. } => "say",
            AgentActionPayload::RequestCapability { .. } => "request_capability",
            AgentActionPayload::RequestSkills => "request_skills",
            AgentActionPayload::CallSkill { .. } => "call_skill",
            AgentActionPayload::ShellCommand { .. } => "shell_command",
            AgentActionPayload::ApplyPatch { .. } => "apply_patch",
            AgentActionPayload::WebSearch { .. } => "web_search",
            AgentActionPayload::FetchUrl { .. } => "fetch_url",
            AgentActionPayload::MemorySearch { .. } => "memory_search",
            AgentActionPayload::MemoryStore { .. } => "memory_store",
            AgentActionPayload::IssueAdd { .. } => "issue_add",
            AgentActionPayload::IssueUpdate { .. } => "issue_update",
            AgentActionPayload::IssueQuery { .. } => "issue_query",
            AgentActionPayload::IssueDelete { .. } => "issue_delete",
            AgentActionPayload::SendMessage { .. } => "send_message",
            AgentActionPayload::SpawnAgent { .. } => "spawn_agent",
            AgentActionPayload::ConfigChange { .. } => "config_change",
            AgentActionPayload::McpCall { .. } => "mcp_call",
            AgentActionPayload::Complete => "complete",
            AgentActionPayload::Abort { .. } => "abort",
        }
    }

    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn validate(
        &self,
        available_mcp_servers: &[String],
        available_mcp_tools: &[McpPromptTool],
    ) -> Result<()> {
        match &self.payload {
            AgentActionPayload::Say {
                text, content_type, ..
            } => {
                validate_non_empty("say text", text)?;
                validate_non_empty("say content type", content_type)
            }
            AgentActionPayload::RequestCapability { reason, .. } => {
                validate_non_empty("capability request reason", reason)
            }
            AgentActionPayload::RequestSkills => Ok(()),
            AgentActionPayload::CallSkill { name, .. } => {
                validate_non_empty("skill name", name)?;
                if !crate::skills::is_valid_skill_name(name) {
                    return Err(MezError::invalid_args(
                        "call_skill name must contain only lowercase letters, digits, and hyphens",
                    ));
                }
                Ok(())
            }
            AgentActionPayload::ShellCommand {
                summary,
                command,
                timeout_ms,
                ..
            } => {
                validate_non_empty("shell command summary", summary)?;
                validate_non_empty("shell command", command)?;
                if matches!(timeout_ms, Some(0)) {
                    return Err(MezError::invalid_args(
                        "shell command timeout_ms must be greater than zero",
                    ));
                }
                validate_agent_authored_shell_command(command)
            }
            AgentActionPayload::ApplyPatch { patch, .. } => validate_apply_patch_payload(patch),
            AgentActionPayload::WebSearch { query, .. } => {
                validate_non_empty("web search query", query)
            }
            AgentActionPayload::FetchUrl { url, .. } => {
                validate_non_empty("fetch url", url)?;
                validate_runtime_http_url("fetch url", url)
            }
            AgentActionPayload::MemorySearch { query, limit } => {
                validate_non_empty("memory search query", query)?;
                if matches!(limit, Some(0)) {
                    return Err(MezError::invalid_args(
                        "memory search limit must be greater than zero",
                    ));
                }
                Ok(())
            }
            AgentActionPayload::MemoryStore {
                kind,
                priority,
                scope,
                content,
                expires_in_days,
                ..
            } => {
                validate_non_empty("memory kind", kind)?;
                validate_non_empty("memory content", content)?;
                if let Some(priority) = priority
                    && *priority > 100
                {
                    return Err(MezError::invalid_args(
                        "memory priority must be between 0 and 100",
                    ));
                }
                if let Some(scope) = scope
                    && !matches!(scope.as_str(), "global" | "project")
                {
                    return Err(MezError::invalid_args(
                        "memory scope must be global or project",
                    ));
                }
                if matches!(expires_in_days, Some(0)) {
                    return Err(MezError::invalid_args(
                        "memory expires_in_days must be greater than zero",
                    ));
                }
                Ok(())
            }
            AgentActionPayload::IssueAdd {
                kind,
                title,
                body,
                notes,
                depends_on,
            } => {
                validate_non_empty("issue kind", kind)?;
                crate::issues::IssueKind::parse(kind)?;
                crate::issues::validate_issue_title(title)?;
                crate::issues::validate_issue_body(body.as_deref())?;
                crate::issues::validate_issue_notes(notes.as_deref())?;
                crate::issues::validate_issue_dependency_ids(None, depends_on)
            }
            AgentActionPayload::IssueUpdate {
                id,
                kind,
                title,
                body,
                clear_body,
                notes,
                clear_notes,
                depends_on,
                clear_depends_on,
            } => {
                validate_non_empty("issue id", id)?;
                crate::issues::IssueUpdate {
                    kind: kind
                        .as_deref()
                        .map(crate::issues::IssueKind::parse)
                        .transpose()?,
                    title: title.clone(),
                    body: body.clone(),
                    clear_body: *clear_body,
                    notes: notes.clone(),
                    clear_notes: *clear_notes,
                    depends_on: depends_on.clone(),
                    clear_depends_on: *clear_depends_on,
                }
                .validate()
            }
            AgentActionPayload::IssueQuery { kind, text, limit } => {
                if let Some(kind) = kind {
                    crate::issues::IssueKind::parse(kind)?;
                }
                if let Some(text) = text
                    && text.bytes().any(|byte| byte == 0)
                {
                    return Err(MezError::invalid_args(
                        "issue query text must not contain NUL bytes",
                    ));
                }
                if let Some(limit) = limit {
                    if *limit == 0 {
                        return Err(MezError::invalid_args(
                            "issue query limit must be greater than zero",
                        ));
                    }
                    if *limit > crate::issues::MAX_ISSUE_QUERY_LIMIT as u64 {
                        return Err(MezError::invalid_args(format!(
                            "issue query limit must be less than or equal to {}",
                            crate::issues::MAX_ISSUE_QUERY_LIMIT
                        )));
                    }
                }
                Ok(())
            }
            AgentActionPayload::IssueDelete { id } => validate_non_empty("issue id", id),
            AgentActionPayload::SendMessage {
                recipient,
                content_type,
                payload,
            } => {
                validate_non_empty("message recipient", recipient)?;
                validate_non_empty("message content type", content_type)?;
                validate_non_empty("message payload", payload)
            }
            AgentActionPayload::SpawnAgent {
                role,
                placement,
                cooperation_mode,
                task_prompt,
                ..
            } => {
                validate_non_empty("subagent role", role)?;
                validate_non_empty("subagent placement", placement)?;
                validate_non_empty("subagent cooperation mode", cooperation_mode)?;
                validate_non_empty("subagent task prompt", task_prompt)
            }
            AgentActionPayload::ConfigChange {
                setting_path,
                operation,
                ..
            } => {
                validate_non_empty("config setting path", setting_path)?;
                validate_non_empty("config operation", operation)
            }
            AgentActionPayload::McpCall {
                server,
                tool,
                arguments_json,
            } => {
                validate_non_empty("mcp server", server)?;
                validate_non_empty("mcp tool", tool)?;
                if !available_mcp_servers
                    .iter()
                    .any(|available| available == server)
                {
                    return Err(MezError::invalid_args(
                        "mcp action references an unavailable server",
                    ));
                }
                let Some(available_tool) = available_mcp_tools.iter().find(|available| {
                    available.server_id == *server && available.tool_name == *tool
                }) else {
                    return Err(MezError::invalid_args(
                        "mcp action references an unavailable or disabled tool",
                    ));
                };
                validate_mcp_call_arguments(arguments_json, &available_tool.input_schema_json)?;
                Ok(())
            }
            AgentActionPayload::Complete => Ok(()),
            AgentActionPayload::Abort { reason } => validate_non_empty("abort reason", reason),
        }
    }
}

/// Validates that a runtime-network URL can be serviced by Mezzanine without
/// relying on pane shell or local filesystem context.
fn validate_runtime_http_url(field: &str, value: &str) -> Result<()> {
    let trimmed = value.trim();
    let lowercase = trimmed.to_ascii_lowercase();
    if lowercase.starts_with("http://") || lowercase.starts_with("https://") {
        return Ok(());
    }
    Err(MezError::invalid_args(format!(
        "{field} must be an http:// or https:// URL; use shell_command for local paths or file:// URLs"
    )))
}

/// Validates MCP tool arguments against the advertised object input schema.
///
/// MCP servers advertise JSON-schema-like input contracts. Runtime MAAP
/// validation rechecks the common object-shape contract before execution so a
/// provider response that bypasses native schema enforcement cannot reach the
/// external integration with missing or clearly mistyped required fields.
fn validate_mcp_call_arguments(arguments_json: &str, input_schema_json: &str) -> Result<()> {
    let arguments: Value = serde_json::from_str(arguments_json).map_err(|error| {
        MezError::invalid_args(format!("mcp action arguments must be valid JSON: {error}"))
    })?;
    let Value::Object(argument_object) = &arguments else {
        return Err(MezError::invalid_args(
            "mcp action arguments must be a JSON object",
        ));
    };
    let schema: Value = serde_json::from_str(input_schema_json).map_err(|error| {
        MezError::invalid_args(format!(
            "mcp tool input schema is not valid JSON for runtime validation: {error}"
        ))
    })?;
    let Value::Object(schema_object) = &schema else {
        return Err(MezError::invalid_args(
            "mcp tool input schema must be a JSON object",
        ));
    };
    if let Some(schema_type) = schema_object.get("type") {
        validate_json_schema_type("mcp tool input schema", schema_type, &arguments)?;
    }
    if let Some(Value::Array(required)) = schema_object.get("required") {
        for required_field in required {
            let Some(field_name) = required_field.as_str() else {
                return Err(MezError::invalid_args(
                    "mcp tool input schema required entries must be strings",
                ));
            };
            if !argument_object.contains_key(field_name) {
                return Err(MezError::invalid_args(format!(
                    "mcp action arguments missing required field `{field_name}`"
                )));
            }
        }
    }
    if let Some(Value::Object(properties)) = schema_object.get("properties") {
        for (field_name, property_schema) in properties {
            if let Some(argument_value) = argument_object.get(field_name)
                && let Some(property_type) = property_schema.get("type")
            {
                validate_json_schema_type(
                    &format!("mcp action argument `{field_name}`"),
                    property_type,
                    argument_value,
                )?;
            }
        }
    }
    Ok(())
}

/// Validates one JSON value against a simple JSON Schema `type` clause.
fn validate_json_schema_type(field: &str, schema_type: &Value, value: &Value) -> Result<()> {
    let expected_types = match schema_type {
        Value::String(expected) => vec![expected.as_str()],
        Value::Array(expected) => expected
            .iter()
            .map(|entry| {
                entry.as_str().ok_or_else(|| {
                    MezError::invalid_args("mcp tool input schema type entries must be strings")
                })
            })
            .collect::<Result<Vec<_>>>()?,
        _ => {
            return Err(MezError::invalid_args(
                "mcp tool input schema type must be a string or string array",
            ));
        }
    };
    if expected_types
        .iter()
        .any(|expected| json_value_matches_schema_type(value, expected))
    {
        return Ok(());
    }
    Err(MezError::invalid_args(format!(
        "{field} does not match MCP tool input schema type {}",
        expected_types.join("|")
    )))
}

/// Reports whether a JSON value matches one JSON Schema primitive type name.
fn json_value_matches_schema_type(value: &Value, expected: &str) -> bool {
    match expected {
        "array" => value.is_array(),
        "boolean" => value.is_boolean(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "null" => value.is_null(),
        "number" => value.is_number(),
        "object" => value.is_object(),
        "string" => value.is_string(),
        _ => false,
    }
}

/// Returns true when a model-authored patch uses Mezzanine's patch block format.
pub(super) fn is_mez_patch_payload(patch: &str) -> bool {
    let trimmed = patch.trim_start();
    trimmed.starts_with("*** Begin Patch")
        || (trimmed
            .lines()
            .next()
            .is_some_and(|line| line.trim_start().starts_with("```"))
            && trimmed.contains("*** Begin Patch"))
        || (trimmed.lines().next().is_some_and(|line| {
            let line = line.trim();
            line.starts_with("<<")
                || line.starts_with("apply_patch <<")
                || line.starts_with("apply_patch<<")
        }) && trimmed.contains("*** Begin Patch"))
}

/// Validates that a model-authored patch matches the Mezzanine semantic patch
/// contract before any pane shell mutation can be dispatched.
pub(super) fn validate_apply_patch_payload(patch: &str) -> Result<()> {
    validate_non_empty("patch", patch)?;
    if !is_mez_patch_payload(patch) {
        return Err(MezError::invalid_args(
            "apply_patch requires Mezzanine patch blocks starting with *** Begin Patch; use shell_command with git apply for raw unified diffs",
        ));
    }
    if !patch.lines().any(|line| line.trim() == "*** End Patch") {
        return Err(MezError::invalid_args(
            "apply_patch Mezzanine patch blocks must end with *** End Patch",
        ));
    }
    let _ = apply_patch_touched_paths(patch)?;
    Ok(())
}

/// Parses the single fenced `mezzanine-action-json` action batch from model text.
pub fn parse_fenced_maap_action_batch(raw_text: &str) -> Result<Option<MaapBatch>> {
    parse_fenced_maap_action_batch_inner(raw_text, None)
}

/// Parses the single fenced `mezzanine-action-json` action batch while filling
/// runtime-owned identity fields that compact provider output may omit.
pub fn parse_fenced_maap_action_batch_for_turn(
    raw_text: &str,
    turn_id: &str,
    agent_id: &str,
) -> Result<Option<MaapBatch>> {
    parse_fenced_maap_action_batch_inner(raw_text, Some((turn_id, agent_id)))
}

/// Runs the parse fenced maap action batch inner operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_fenced_maap_action_batch_inner(
    raw_text: &str,
    identity: Option<(&str, &str)>,
) -> Result<Option<MaapBatch>> {
    let mut blocks = Vec::new();
    let mut active_block: Option<Vec<String>> = None;
    for line in raw_text.lines() {
        if let Some(lines) = active_block.as_mut() {
            if line.trim() == "```" {
                blocks.push(lines.join("\n"));
                active_block = None;
            } else {
                lines.push(line.to_string());
            }
            continue;
        }

        let trimmed = line.trim_start();
        let Some(info) = trimmed.strip_prefix("```") else {
            continue;
        };
        if info.trim() == "mezzanine-action-json" {
            active_block = Some(Vec::new());
        }
    }

    if active_block.is_some() {
        return Err(MezError::invalid_args(
            "mezzanine-action-json block is unterminated",
        ));
    }
    match blocks.as_slice() {
        [] => Ok(None),
        [block] => Ok(Some(parse_maap_action_batch_json_inner(block, identity)?)),
        _ => Err(MezError::invalid_args(
            "model response must contain exactly one mezzanine-action-json block",
        )),
    }
}

/// Parses one `maap/1` action batch JSON object.
pub fn parse_maap_action_batch_json(batch_json: &str) -> Result<MaapBatch> {
    parse_maap_action_batch_json_inner(batch_json, None)
}

/// Parses one compact provider-native MAAP batch JSON object and fills
/// runtime-owned identity fields from the active turn.
pub fn parse_maap_action_batch_json_for_turn(
    batch_json: &str,
    turn_id: &str,
    agent_id: &str,
) -> Result<MaapBatch> {
    parse_maap_action_batch_json_inner(batch_json, Some((turn_id, agent_id)))
}

/// Runs the parse maap action batch json inner operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_maap_action_batch_json_inner(
    batch_json: &str,
    identity: Option<(&str, &str)>,
) -> Result<MaapBatch> {
    let value = serde_json::from_str::<serde_json::Value>(batch_json).map_err(|error| {
        MezError::invalid_args(format!(
            "mezzanine-action-json block is invalid JSON: {error}"
        ))
    })?;
    parse_maap_action_batch_value(&value, identity)
}

/// Runs the parse maap action batch value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_maap_action_batch_value(
    value: &serde_json::Value,
    identity: Option<(&str, &str)>,
) -> Result<MaapBatch> {
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args("maap action batch must be a JSON object"))?;
    let mut actions = required_array(object, "actions")?
        .iter()
        .enumerate()
        .map(|(index, value)| parse_maap_action_value(index, value))
        .collect::<Result<Vec<_>>>()?;
    for (index, action) in actions.iter().enumerate() {
        if let AgentActionPayload::Say { text, .. } = &action.payload
            && text.trim().is_empty()
        {
            return Err(MezError::invalid_args(format!(
                "maap action {} say text must not be empty",
                synthesized_action_id(index)
            )));
        }
    }
    for (index, action) in actions.iter_mut().enumerate() {
        action.id = synthesized_action_id(index);
    }
    if actions.is_empty() {
        return Err(MezError::invalid_args(
            "maap action batch must include at least one action",
        ));
    }
    let protocol = optional_string(object, "protocol")?
        .unwrap_or("maap/1")
        .to_string();
    let rationale = required_string(object, "rationale")?.trim().to_string();
    if rationale.is_empty() {
        return Err(MezError::invalid_args(
            "maap field rationale must not be empty",
        ));
    }
    let thought = optional_string(object, "thought")?
        .map(str::trim)
        .filter(|thought| !thought.is_empty())
        .map(str::to_string);
    let turn_id = optional_string(object, "turn_id")?
        .map(str::to_string)
        .or_else(|| identity.map(|(turn_id, _)| turn_id.to_string()))
        .ok_or_else(|| MezError::invalid_args("maap field turn_id is required"))?;
    let agent_id = optional_string(object, "agent_id")?
        .map(str::to_string)
        .or_else(|| identity.map(|(_, agent_id)| agent_id.to_string()))
        .ok_or_else(|| MezError::invalid_args("maap field agent_id is required"))?;
    let final_turn = optional_bool(object, "final")?.unwrap_or_else(|| infer_final_turn(&actions));
    Ok(MaapBatch {
        protocol,
        rationale,
        thought,
        turn_id,
        agent_id,
        actions,
        final_turn,
    })
}

/// Runs the parse maap action value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_maap_action_value(_index: usize, value: &serde_json::Value) -> Result<AgentAction> {
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args("maap action must be a JSON object"))?;
    let action_type = required_string(object, "type")?;
    let id = String::new();
    let rationale = optional_string(object, "rationale")?
        .unwrap_or("")
        .to_string();
    let payload = match action_type {
        "say" => AgentActionPayload::Say {
            status: parse_say_status(required_string(object, "status")?)?,
            text: required_string(object, "text")?.to_string(),
            content_type: normalize_agent_output_content_type(optional_string(
                object,
                "content_type",
            )?),
        },
        "request_capability" => {
            let capability_name = required_string(object, "capability")?;
            let capability = AgentCapability::parse(capability_name).ok_or_else(|| {
                MezError::invalid_args(format!("unknown agent capability {capability_name}"))
            })?;
            AgentActionPayload::RequestCapability {
                capability,
                reason: required_string(object, "reason")?.to_string(),
            }
        }
        "request_skills" => AgentActionPayload::RequestSkills,
        "call_skill" => AgentActionPayload::CallSkill {
            name: required_string(object, "name")?.to_string(),
            additional_context: optional_string(object, "additional_context")?.map(str::to_string),
        },
        "shell_command" => AgentActionPayload::ShellCommand {
            summary: shell_command_summary(object, &rationale)?,
            command: required_string(object, "command")?.to_string(),
            interactive: optional_bool(object, "interactive")?.unwrap_or(false),
            stateful: optional_bool(object, "stateful")?.unwrap_or(false),
            timeout_ms: optional_nullable_u64(object, "timeout_ms")?,
        },
        "apply_patch" => {
            let patch = required_string(object, "patch")?.to_string();
            AgentActionPayload::ApplyPatch {
                patch: try_convert_unified_diff_to_mez_patch(&patch).unwrap_or(patch),
                strip: optional_nullable_u64(object, "strip")?,
            }
        }
        "web_search" => AgentActionPayload::WebSearch {
            query: required_string(object, "query")?.to_string(),
            domains: optional_string_array(object, "domains")?,
            recency_days: optional_nullable_u64(object, "recency_days")?,
            max_results: optional_nullable_u64(object, "max_results")?,
        },
        "fetch_url" => {
            let url = required_string(object, "url")?;
            if fetch_url_file_path(url)?.is_some() {
                return Err(MezError::invalid_args(
                    "fetch_url does not support local file URLs; use shell_command for local filesystem inspection",
                ));
            }
            AgentActionPayload::FetchUrl {
                url: url.to_string(),
                format: optional_string(object, "format")?.map(str::to_string),
                max_bytes: optional_nullable_u64(object, "max_bytes")?,
            }
        }
        "memory_search" => AgentActionPayload::MemorySearch {
            query: required_string(object, "query")?.to_string(),
            limit: optional_nullable_u64(object, "limit")?,
        },
        "memory_store" => AgentActionPayload::MemoryStore {
            kind: required_string(object, "kind")?.to_string(),
            priority: optional_nullable_u64(object, "priority")?,
            scope: optional_string(object, "scope")?.map(str::to_string),
            keywords: optional_string_array(object, "keywords")?,
            content: required_string(object, "content")?.to_string(),
            expires_in_days: optional_nullable_u64(object, "expires_in_days")?,
        },
        "issue_add" => AgentActionPayload::IssueAdd {
            kind: required_string(object, "kind")?.to_string(),
            title: required_string(object, "title")?.to_string(),
            body: optional_string(object, "body")?.map(str::to_string),
            notes: optional_string(object, "notes")?.map(str::to_string),
            depends_on: optional_string_array(object, "depends_on")?,
        },
        "issue_update" => AgentActionPayload::IssueUpdate {
            id: required_string(object, "id")?.to_string(),
            kind: optional_string(object, "kind")?.map(str::to_string),
            title: optional_string(object, "title")?.map(str::to_string),
            body: optional_string(object, "body")?.map(str::to_string),
            clear_body: optional_bool(object, "clear_body")?.unwrap_or(false),
            notes: optional_string(object, "notes")?.map(str::to_string),
            clear_notes: optional_bool(object, "clear_notes")?.unwrap_or(false),
            depends_on: optional_nullable_string_array(object, "depends_on")?,
            clear_depends_on: optional_bool(object, "clear_depends_on")?.unwrap_or(false),
        },
        "issue_query" => AgentActionPayload::IssueQuery {
            kind: optional_string(object, "kind")?.map(str::to_string),
            text: optional_string(object, "text")?.map(str::to_string),
            limit: optional_nullable_u64(object, "limit")?,
        },
        "issue_delete" => AgentActionPayload::IssueDelete {
            id: required_string(object, "id")?.to_string(),
        },
        "send_message" => AgentActionPayload::SendMessage {
            recipient: required_string(object, "recipient")?.to_string(),
            content_type: required_string(object, "content_type")?.to_string(),
            payload: required_json_or_string(object, "payload")?,
        },
        "spawn_agent" => AgentActionPayload::SpawnAgent {
            role: required_string(object, "role")?.to_string(),
            placement: optional_string(object, "placement")?
                .unwrap_or("new-window")
                .to_string(),
            cooperation_mode: optional_string(object, "cooperation_mode")?
                .map(str::to_string)
                .unwrap_or_else(|| maap_default_cooperation_mode(object)),
            read_scopes: optional_string_array(object, "read_scopes")?,
            write_scopes: optional_string_array(object, "write_scopes")?,
            task_prompt: required_string(object, "task_prompt")?.to_string(),
        },
        "config_change" => AgentActionPayload::ConfigChange {
            setting_path: required_string(object, "setting_path")?.to_string(),
            operation: required_string(object, "operation")?.to_string(),
            value: optional_json_or_string(object, "value")?,
        },
        "mcp_call" => AgentActionPayload::McpCall {
            server: required_string(object, "server")?.to_string(),
            tool: required_string(object, "tool")?.to_string(),
            arguments_json: required_json_object_compact(object, "arguments")?,
        },
        "complete" => AgentActionPayload::Complete,
        "abort" => AgentActionPayload::Abort {
            reason: required_string(object, "reason")?.to_string(),
        },
        _ => {
            return Err(MezError::invalid_args(format!(
                "unknown maap action type {action_type}"
            )));
        }
    };
    Ok(AgentAction {
        id,
        rationale,
        payload,
    })
}

/// Infers whether a compact action batch should complete after its visible
/// actions without requiring the model to emit a redundant final flag.
fn infer_final_turn(actions: &[AgentAction]) -> bool {
    actions.iter().all(|action| {
        matches!(
            action.payload,
            AgentActionPayload::Say {
                status: SayStatus::Final | SayStatus::Blocked,
                ..
            } | AgentActionPayload::Complete
                | AgentActionPayload::Abort { .. }
        )
    })
}

/// Parses a required `say.status` value and returns a targeted diagnostic when
/// the model omits or misspells the terminal intent.
fn parse_say_status(value: &str) -> Result<SayStatus> {
    SayStatus::parse(value).ok_or_else(|| {
        MezError::invalid_args(
            "maap say.status must be one of progress, final, or blocked; use progress only for nonterminal updates, final for completed work, and blocked when user/external input is required",
        )
    })
}

/// Defaults compact MAAP spawn-agent cooperation mode from the fields that
/// remain model-authored.
fn maap_default_cooperation_mode(object: &serde_json::Map<String, serde_json::Value>) -> String {
    let role = object
        .get("role")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let has_write_scopes = object
        .get("write_scopes")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|scopes| !scopes.is_empty());
    if role == "worker" || has_write_scopes {
        "owned-write".to_string()
    } else {
        "explore-only".to_string()
    }
}

/// Runs the synthesized action id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn synthesized_action_id(index: usize) -> String {
    format!("action-{}", index.saturating_add(1))
}

/// Runs the shell command summary operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn shell_command_summary(
    object: &serde_json::Map<String, serde_json::Value>,
    action_rationale: &str,
) -> Result<String> {
    Ok(optional_string(object, "summary")?
        .map(str::to_string)
        .unwrap_or_else(|| action_rationale.to_string()))
}

/// Runs the required value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn required_value<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<&'a serde_json::Value> {
    object
        .get(field)
        .ok_or_else(|| MezError::invalid_args(format!("maap field {field} is required")))
}

/// Runs the required string operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn required_string<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<&'a str> {
    match required_value(object, field)? {
        serde_json::Value::Null => Err(MezError::invalid_args(format!(
            "maap field {field} must be a string, not null"
        ))),
        value => value
            .as_str()
            .ok_or_else(|| MezError::invalid_args(format!("maap field {field} must be a string"))),
    }
}

/// Runs the optional string operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_string<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<&'a str>> {
    match object.get(field) {
        None => Ok(None),
        Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(value)) => Ok(Some(value.as_str())),
        Some(_) => Err(MezError::invalid_args(format!(
            "maap field {field} must be a string"
        ))),
    }
}

/// Runs the optional bool operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_bool(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<bool>> {
    match object.get(field) {
        None => Ok(None),
        Some(serde_json::Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(MezError::invalid_args(format!(
            "maap field {field} must be a boolean"
        ))),
    }
}

/// Requires the field to be present (absent → error) and returns `None`
/// when the value is JSON null, or `Some(u64)` for a valid u64 number.
///
/// For optional fields that may be absent altogether, use
/// [`optional_nullable_u64`] instead — it treats an absent key as `None`
/// without erroring.
fn nullable_u64(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<u64>> {
    match required_value(object, field)? {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::Number(number) => number
            .as_u64()
            .map(Some)
            .ok_or_else(|| MezError::invalid_args(format!("maap field {field} must be a u64"))),
        _ => Err(MezError::invalid_args(format!(
            "maap field {field} must be a u64 or null"
        ))),
    }
}

/// Runs the optional nullable u64 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_nullable_u64(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<u64>> {
    if object.contains_key(field) {
        nullable_u64(object, field)
    } else {
        Ok(None)
    }
}

/// Runs the required object operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn required_object<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<&'a serde_json::Map<String, serde_json::Value>> {
    match required_value(object, field)? {
        serde_json::Value::Null => Err(MezError::invalid_args(format!(
            "maap field {field} must be an object, not null"
        ))),
        value => value
            .as_object()
            .ok_or_else(|| MezError::invalid_args(format!("maap field {field} must be an object"))),
    }
}

/// Runs the required array operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn required_array<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<&'a Vec<serde_json::Value>> {
    match required_value(object, field)? {
        serde_json::Value::Null => Err(MezError::invalid_args(format!(
            "maap field {field} must be an array, not null"
        ))),
        value => value
            .as_array()
            .ok_or_else(|| MezError::invalid_args(format!("maap field {field} must be an array"))),
    }
}

/// Runs the required string array operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn required_string_array(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Vec<String>> {
    required_array(object, field)?
        .iter()
        .map(|value| {
            value.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                MezError::invalid_args(format!("maap field {field} must contain strings"))
            })
        })
        .collect()
}

/// Runs the optional string array operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_string_array(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Vec<String>> {
    if object.contains_key(field) {
        required_string_array(object, field)
    } else {
        Ok(Vec::new())
    }
}

/// Returns an optional string array where missing or null means unchanged.
fn optional_nullable_string_array(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<Vec<String>>> {
    match object.get(field) {
        Some(serde_json::Value::Null) | None => Ok(None),
        Some(_) => required_string_array(object, field).map(Some),
    }
}

/// Runs the required json compact operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn required_json_compact(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<String> {
    serde_json::to_string(required_value(object, field)?)
        .map_err(|error| MezError::invalid_args(format!("maap field {field} is invalid: {error}")))
}

/// Runs the required json object compact operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn required_json_object_compact(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<String> {
    serde_json::to_string(required_object(object, field)?)
        .map_err(|error| MezError::invalid_args(format!("maap field {field} is invalid: {error}")))
}

/// Detects model-authored `fetch_url` file URLs so local filesystem access
/// stays on the pane shell action surface instead of runtime HTTP fetching.
fn fetch_url_file_path(url: &str) -> Result<Option<String>> {
    let trimmed = url.trim();
    if !trimmed.to_ascii_lowercase().starts_with("file://") {
        return Ok(None);
    }
    let mut path = &trimmed["file://".len()..];
    if path.to_ascii_lowercase().starts_with("localhost/") {
        path = &path["localhost".len()..];
    } else if path.eq_ignore_ascii_case("localhost") {
        return Err(MezError::invalid_args(
            "file URL fetch must include a local path; use shell_command for local filesystem inspection",
        ));
    }
    if path.is_empty() {
        return Err(MezError::invalid_args(
            "file URL fetch must include a local path; use shell_command for local filesystem inspection",
        ));
    }
    Ok(Some(percent_decode_file_url_path(path)?))
}

/// Decodes percent escapes in a local file URL path without accepting malformed
/// escape sequences. The decoded bytes are converted lossily so a malformed
/// UTF-8 filename still reaches the shell-backed local action as visible text.
fn percent_decode_file_url_path(path: &str) -> Result<String> {
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let Some(high) = bytes.get(index + 1).and_then(|byte| hex_value(*byte)) else {
                return Err(MezError::invalid_args(
                    "file URL path contains invalid percent encoding",
                ));
            };
            let Some(low) = bytes.get(index + 2).and_then(|byte| hex_value(*byte)) else {
                return Err(MezError::invalid_args(
                    "file URL path contains invalid percent encoding",
                ));
            };
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    Ok(String::from_utf8_lossy(&decoded).into_owned())
}

/// Converts one ASCII hex digit to its numeric value for file URL decoding.
fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Runs the required json or string operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn required_json_or_string(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<String> {
    match required_value(object, field)? {
        serde_json::Value::String(value) => Ok(value.clone()),
        _ => required_json_compact(object, field),
    }
}

/// Runs the optional json or string operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_json_or_string(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<String>> {
    match object.get(field) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => required_json_compact(object, field).map(Some),
    }
}

impl ActionResult {
    /// Runs the running operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn running(
        turn: &AgentTurnRecord,
        action: &AgentAction,
        content: Vec<String>,
        structured_content_json: Option<String>,
    ) -> Self {
        Self {
            protocol: "maap/1".to_string(),
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            action_id: action.id.clone(),
            action_type: action.action_type(),
            status: ActionStatus::Running,
            content: action_text_content_blocks(content),
            structured_content_json,
            is_error: false,
            error: None,
        }
    }

    /// Runs the succeeded operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn succeeded(
        turn: &AgentTurnRecord,
        action: &AgentAction,
        content: Vec<String>,
        structured_content_json: Option<String>,
    ) -> Self {
        Self {
            protocol: "maap/1".to_string(),
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            action_id: action.id.clone(),
            action_type: action.action_type(),
            status: ActionStatus::Succeeded,
            content: action_text_content_blocks(content),
            structured_content_json,
            is_error: false,
            error: None,
        }
    }

    /// Runs the blocked operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn blocked(
        turn: &AgentTurnRecord,
        action: &AgentAction,
        content: Vec<String>,
        structured_content_json: String,
    ) -> Self {
        Self {
            protocol: "maap/1".to_string(),
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            action_id: action.id.clone(),
            action_type: action.action_type(),
            status: ActionStatus::Blocked,
            content: action_text_content_blocks(content),
            structured_content_json: Some(structured_content_json),
            is_error: false,
            error: None,
        }
    }

    /// Runs the failed operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn failed(
        turn: &AgentTurnRecord,
        action: &AgentAction,
        status: ActionStatus,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<Self> {
        if matches!(
            status,
            ActionStatus::Running | ActionStatus::Succeeded | ActionStatus::Blocked
        ) {
            return Err(MezError::invalid_args(
                "failed action result requires an error status",
            ));
        }
        Ok(Self {
            protocol: "maap/1".to_string(),
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            action_id: action.id.clone(),
            action_type: action.action_type(),
            status,
            content: Vec::new(),
            structured_content_json: None,
            is_error: true,
            error: Some(ActionError {
                code: code.into(),
                message: message.into(),
                data_json: None,
            }),
        })
    }

    /// Runs the validate invariants operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn validate_invariants(&self) -> Result<()> {
        match self.status {
            ActionStatus::Succeeded | ActionStatus::Running => {
                if self.is_error || self.error.is_some() {
                    return Err(MezError::invalid_args(
                        "successful or running action results must not carry errors",
                    ));
                }
            }
            ActionStatus::Blocked => {
                if self.is_error || self.error.is_some() || self.structured_content_json.is_none() {
                    return Err(MezError::invalid_args(
                        "blocked action results must include approval structure without error",
                    ));
                }
            }
            ActionStatus::Rejected
            | ActionStatus::Denied
            | ActionStatus::Failed
            | ActionStatus::Cancelled
            | ActionStatus::TimedOut
            | ActionStatus::Interrupted => {
                if !self.is_error || self.error.is_none() {
                    return Err(MezError::invalid_args(
                        "error action results must set is_error and include an error",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Runs the content texts operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn content_texts(&self) -> Vec<String> {
        self.content
            .iter()
            .map(|block| block.text.clone())
            .collect()
    }

    /// Runs the content text operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn content_text(&self) -> String {
        self.content
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Runs the content json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn content_json(&self) -> String {
        format!(
            "[{}]",
            self.content
                .iter()
                .map(ActionContentBlock::to_json)
                .collect::<Vec<_>>()
                .join(",")
        )
    }
}

/// Runs the action text content blocks operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn action_text_content_blocks(content: Vec<String>) -> Vec<ActionContentBlock> {
    content.into_iter().map(ActionContentBlock::text).collect()
}

/// Runs the action content blocks from json or text operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn action_content_blocks_from_json_or_text(
    content_json: &str,
) -> Vec<ActionContentBlock> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(content_json) else {
        return vec![ActionContentBlock::text(content_json.to_string())];
    };
    let Some(items) = value.as_array() else {
        return vec![ActionContentBlock::text(content_json.to_string())];
    };
    let blocks = items
        .iter()
        .filter_map(|item| {
            let item_type = item.get("type").and_then(serde_json::Value::as_str)?;
            if item_type != "text" {
                return None;
            }
            let text = item.get("text").and_then(serde_json::Value::as_str)?;
            Some(ActionContentBlock::text(text.to_string()))
        })
        .collect::<Vec<_>>();
    if blocks.is_empty() {
        vec![ActionContentBlock::text(content_json.to_string())]
    } else {
        blocks
    }
}

/// Runs the validate non empty operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_non_empty(field: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        Err(MezError::invalid_args(format!("{field} must not be empty")))
    } else {
        Ok(())
    }
}

/// Runs the string array json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn string_array_json(values: &[String]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| format!(r#""{}""#, json_escape(value)))
            .collect::<Vec<_>>()
            .join(",")
    )
}

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
            ch if ch.is_control() => {
                escaped.push_str(&format!("\\u{:04x}", ch as u32));
            }
            _ => escaped.push(ch),
        }
    }
    escaped
}
