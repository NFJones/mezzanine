//! Durable agent-session checkpoint contracts and validation.

use std::collections::BTreeMap;

use crate::{AgentContextUsageSnapshot, ModelTokenUsage, ModelTokenUsageKey};

use super::TranscriptContractError;
use super::records::{validate_conversation_id, validate_required};

/// Durable pane binding and preference metadata for an active agent session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSessionMetadata {
    /// Owning Mezzanine session identity.
    pub mezzanine_session_id: String,
    /// Pane that owns the agent shell session.
    pub pane_id: String,
    /// Durable conversation identity bound to the pane.
    pub conversation_id: String,
    /// Stable prompt-cache lineage identity for provider routing continuity.
    pub prompt_cache_lineage_id: String,
    /// Agent shell visibility name.
    pub visibility: String,
    /// Running turn at the time of the checkpoint, if any.
    pub running_turn_id: Option<String>,
    /// Number of transcript entries known to the pane metadata.
    pub transcript_entries: u64,
    /// Pane-local agent log level.
    pub log_level: String,
    /// Pane-scoped model profile override, if one is active.
    pub pane_model_profile: Option<String>,
    /// Whether pane-local planning mode is active.
    pub planning_enabled: bool,
    /// Pane-local response style, if one is active.
    pub response_style: Option<String>,
    /// Pane-local session directive appended to developer instructions.
    pub directive: Option<String>,
    /// Pane-local routing override, if one is active.
    pub routing_enabled: Option<bool>,
    /// Saved approval policy name for the session, if known.
    pub approval_policy: Option<String>,
    /// Best-known pane working directory for the agent session.
    pub working_directory: Option<String>,
    /// Best-known project root for the agent session.
    pub project_root: Option<String>,
    /// Provider-reported token usage accumulated for this conversation.
    pub token_usage: ModelTokenUsage,
    /// Provider-reported token usage accumulated per provider/model.
    pub token_usage_by_model: BTreeMap<ModelTokenUsageKey, ModelTokenUsage>,
    /// Last provider-reported context usage label shown in pane status.
    pub context_usage: Option<String>,
    /// Last provider request-context snapshot shown in pane status.
    pub context_usage_snapshot: Option<AgentContextUsageSnapshot>,
}

impl AgentSessionMetadata {
    /// Validates active agent session metadata before persistence or use.
    pub fn validate(&self) -> Result<(), TranscriptContractError> {
        validate_required("mezzanine session id", &self.mezzanine_session_id)?;
        validate_required("pane id", &self.pane_id)?;
        validate_conversation_id(&self.conversation_id)?;
        validate_required("prompt cache lineage id", &self.prompt_cache_lineage_id)?;
        validate_agent_visibility(&self.visibility)?;
        if let Some(turn_id) = self.running_turn_id.as_deref() {
            validate_required("running turn id", turn_id)?;
        }
        validate_log_level(&self.log_level)?;
        for (label, value) in [
            ("pane model profile", self.pane_model_profile.as_deref()),
            ("response style", self.response_style.as_deref()),
            ("directive", self.directive.as_deref()),
            ("working directory", self.working_directory.as_deref()),
            ("project root", self.project_root.as_deref()),
            ("context usage", self.context_usage.as_deref()),
        ] {
            if let Some(value) = value {
                validate_required(label, value)?;
            }
        }
        if let Some(approval_policy) = self.approval_policy.as_deref() {
            validate_agent_approval_policy(approval_policy)?;
        }
        if let Some(snapshot) = self.context_usage_snapshot {
            if snapshot.input_tokens == 0 {
                return Err(TranscriptContractError::new(
                    "context usage snapshot input_tokens must be greater than zero",
                ));
            }
            if snapshot.context_window_tokens == 0 {
                return Err(TranscriptContractError::new(
                    "context usage snapshot context_window_tokens must be greater than zero",
                ));
            }
        }
        for key in self.token_usage_by_model.keys() {
            validate_required("token usage provider", &key.provider)?;
            validate_required("token usage model", &key.model)?;
        }
        Ok(())
    }
}

fn validate_agent_visibility(value: &str) -> Result<(), TranscriptContractError> {
    match value {
        "hidden" | "visible" | "hide-pending-task-completion" => Ok(()),
        _ => Err(TranscriptContractError::new(
            "unknown persisted agent visibility",
        )),
    }
}

fn validate_log_level(value: &str) -> Result<(), TranscriptContractError> {
    match value {
        "normal" | "verbose" | "debug" | "trace" => Ok(()),
        _ => Err(TranscriptContractError::new("unknown persisted log level")),
    }
}

fn validate_agent_approval_policy(value: &str) -> Result<(), TranscriptContractError> {
    match value {
        "ask" | "auto-allow" | "full-access" => Ok(()),
        _ => Err(TranscriptContractError::new(
            "unknown persisted approval policy",
        )),
    }
}
