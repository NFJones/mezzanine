//! Provider-independent agent harness and agent protocol state machines.
//!
//! This crate will own model-facing request normalization, MAAP contracts,
//! turn orchestration, and provider-independent macro and subagent behavior.
//! Product credentials, persistence, transports, process execution, and UI
//! remain behind ports implemented by the root package. The initial empty
//! facade establishes that dependency boundary before those ports are added.

use std::collections::BTreeSet;

/// Provider-neutral model token accounting contracts.
pub mod accounting;
/// Provider authentication routing contracts.
pub mod auth;
/// Model-facing live configuration mutation contracts.
pub mod config_change;
/// Provider-neutral HTTP request and response contracts.
pub mod http;
/// Dependency-neutral project instruction discovery records and parsing.
pub mod instructions;
/// Dependency-neutral MCP prompt manifest records.
pub mod mcp;
/// Agent-facing permission identity contracts.
pub mod permissions;
/// Provider-neutral prompt profile contracts.
pub mod prompt;
/// Provider-neutral API compatibility contracts.
pub mod provider;
/// Secret-safe provider failure diagnostic shaping.
pub mod provider_diagnostics;
/// Provider failure retry and recovery classification.
pub mod provider_error;
/// Provider-native transcript continuity contracts.
pub mod provider_transcript;
/// Dependency-neutral provider quota accounting contracts.
pub mod quota;
/// Dependency-neutral agent slash-command registry and parsing.
pub mod slash;
/// Provider-independent subagent cooperation and scope contracts.
pub mod subagent;

pub use accounting::{AgentContextUsageSnapshot, ModelTokenUsage, ModelTokenUsageKey};
pub use auth::{ProviderAuthMetadata, ProviderCredentialKind};
pub use config_change::{CONFIG_CHANGE_OPERATION_NAMES, CONFIG_CHANGE_VALUE_DESCRIPTION};
pub use http::{
    DEFAULT_PROVIDER_MAX_RESPONSE_BYTES, DEFAULT_PROVIDER_TIMEOUT_MS, ProviderHttpRequest,
    ProviderHttpResponse,
};
pub use mcp::{McpPromptServer, McpPromptSummary, McpPromptTool, McpPromptUnavailableServer};
pub use permissions::{ApprovalPolicy, PermissionPreset, RuleDecision};
pub use prompt::{AGENT_PROMPT_PROFILE_NAME, AGENT_PROMPT_PROFILE_VERSION, AgentPromptProfile};
pub use provider::{
    ANTHROPIC_MESSAGES_API, CLAUDE_CODE_API, DEEPSEEK_CHAT_COMPLETIONS_API,
    OPENAI_CHAT_COMPLETIONS_API, OPENAI_RESPONSES_API, ProviderApiCompatibility,
    ProviderApiCompatibilityError, ProviderCapabilities, ProviderModelCatalog, ProviderModelInfo,
    resolve_provider_api,
};
pub use provider_diagnostics::{
    provider_error_detail, provider_failure_event_json, provider_failure_json,
    provider_malformed_output_failure_json, provider_malformed_output_hint,
};
pub use provider_error::{
    ProviderErrorKind, ProviderErrorRetryClass, classify_provider_error_retry,
};
pub use provider_transcript::{PROVIDER_TRANSCRIPT_EVENT_MARKER, ProviderTranscriptEvent};
pub use quota::{ProviderQuotaUsage, provider_quota_usage_from_headers};
pub use slash::{
    SlashCommandEffect, SlashCommandInvocation, SlashCommandParseError, SlashCommandSpec,
    baseline_slash_commands, parse_slash_command,
};
pub use subagent::{CooperationMode, SubagentScopeDeclaration};

/// Maximum number of issue records a model-authored query may request.
pub const MAX_ISSUE_QUERY_LIMIT: u64 = 200;

/// Borrowed fields used to validate one model-authored issue update.
#[derive(Debug, Clone, Copy)]
pub struct IssueUpdateValidation<'a> {
    /// Optional replacement issue kind.
    pub kind: Option<&'a str>,
    /// Optional replacement issue state.
    pub state: Option<&'a str>,
    /// Optional replacement title.
    pub title: Option<&'a str>,
    /// Optional replacement body.
    pub body: Option<&'a str>,
    /// Whether the existing body should be removed.
    pub clear_body: bool,
    /// Optional replacement progress notes.
    pub notes: Option<&'a str>,
    /// Whether the existing notes should be removed.
    pub clear_notes: bool,
    /// Optional replacement dependency identifiers.
    pub depends_on: Option<&'a [String]>,
    /// Whether existing dependencies should be removed.
    pub clear_depends_on: bool,
}

/// Borrowed fields used to validate one model-authored issue query.
#[derive(Debug, Clone, Copy)]
pub struct IssueQueryValidation<'a> {
    /// Optional issue kind filter.
    pub kind: Option<&'a str>,
    /// Optional issue state filter.
    pub state: Option<&'a str>,
    /// Optional title/body substring filter.
    pub text: Option<&'a str>,
    /// Optional maximum result count.
    pub limit: Option<u64>,
}

/// Validates the stable model-facing issue kind grammar.
pub fn validate_issue_kind(kind: &str) -> Result<(), String> {
    if matches!(kind, "defect" | "task") {
        Ok(())
    } else {
        Err("issue kind must be defect or task".to_string())
    }
}

/// Validates the stable model-facing issue state grammar.
pub fn validate_issue_state(state: &str) -> Result<(), String> {
    if matches!(state, "open" | "resolved") {
        Ok(())
    } else {
        Err("issue state must be open or resolved".to_string())
    }
}

/// Validates a model-authored issue title.
pub fn validate_issue_title(title: &str) -> Result<(), String> {
    validate_non_empty_single_line("issue title", title)
}

/// Validates optional model-authored issue body text.
pub fn validate_issue_body(body: Option<&str>) -> Result<(), String> {
    validate_optional_text("issue body", body)
}

/// Validates optional model-authored issue progress notes.
pub fn validate_issue_notes(notes: Option<&str>) -> Result<(), String> {
    validate_optional_text("issue notes", notes)
}

/// Validates model-authored dependency identifiers before product lookup.
pub fn validate_issue_dependency_ids(depends_on: &[String]) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for dependency_id in depends_on {
        if dependency_id.trim().is_empty() || dependency_id.bytes().any(|byte| byte == 0) {
            return Err("issue dependency id must not be empty".to_string());
        }
        if !seen.insert(dependency_id.as_str()) {
            return Err("issue dependencies must not contain duplicates".to_string());
        }
    }
    Ok(())
}

/// Validates one model-authored issue update without depending on persistence types.
pub fn validate_issue_update(update: IssueUpdateValidation<'_>) -> Result<(), String> {
    let has_changes = update.kind.is_some()
        || update.state.is_some()
        || update.title.is_some()
        || update.body.is_some()
        || update.clear_body
        || update.notes.is_some()
        || update.clear_notes
        || update.depends_on.is_some()
        || update.clear_depends_on;
    if !has_changes {
        return Err("issue update requires at least one field to change".to_string());
    }
    if update.body.is_some() && update.clear_body {
        return Err("issue update cannot set and clear body".to_string());
    }
    if update.notes.is_some() && update.clear_notes {
        return Err("issue update cannot set and clear notes".to_string());
    }
    if update.depends_on.is_some() && update.clear_depends_on {
        return Err("issue update cannot set and clear dependencies".to_string());
    }
    if let Some(kind) = update.kind {
        validate_issue_kind(kind)?;
    }
    if let Some(state) = update.state {
        validate_issue_state(state)?;
    }
    if let Some(title) = update.title {
        validate_issue_title(title)?;
    }
    validate_issue_body(update.body)?;
    validate_issue_notes(update.notes)?;
    if let Some(depends_on) = update.depends_on {
        validate_issue_dependency_ids(depends_on)?;
    }
    Ok(())
}

/// Validates one model-authored issue query without depending on the issue store.
pub fn validate_issue_query(query: IssueQueryValidation<'_>) -> Result<(), String> {
    if let Some(kind) = query.kind {
        validate_issue_kind(kind)?;
    }
    if let Some(state) = query.state {
        validate_issue_state(state)?;
    }
    validate_optional_text("issue query text", query.text)?;
    if let Some(limit) = query.limit {
        if limit == 0 {
            return Err("issue query limit must be greater than zero".to_string());
        }
        if limit > MAX_ISSUE_QUERY_LIMIT {
            return Err(format!(
                "issue query limit must be less than or equal to {MAX_ISSUE_QUERY_LIMIT}"
            ));
        }
    }
    Ok(())
}

fn validate_optional_text(label: &str, value: Option<&str>) -> Result<(), String> {
    if value.is_some_and(|value| value.bytes().any(|byte| byte == 0)) {
        return Err(format!("{label} must not contain NUL bytes"));
    }
    Ok(())
}

fn validate_non_empty_single_line(label: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{label} must not be empty"));
    }
    if value
        .bytes()
        .any(|byte| byte == 0 || byte == b'\n' || byte == b'\r')
    {
        return Err(format!("{label} must be a single line without NUL bytes"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        IssueQueryValidation, IssueUpdateValidation, validate_issue_query, validate_issue_update,
    };

    #[test]
    /// Verifies valid model-authored issue updates and queries are accepted at
    /// the dependency-neutral agent protocol boundary.
    fn issue_action_validation_accepts_valid_fields() {
        let dependencies = vec!["issue-1".to_string()];
        validate_issue_update(IssueUpdateValidation {
            kind: Some("task"),
            state: Some("open"),
            title: Some("Implement agent contract"),
            body: None,
            clear_body: false,
            notes: Some("in progress"),
            clear_notes: false,
            depends_on: Some(&dependencies),
            clear_depends_on: false,
        })
        .unwrap();
        validate_issue_query(IssueQueryValidation {
            kind: Some("defect"),
            state: Some("resolved"),
            text: Some("agent"),
            limit: Some(200),
        })
        .unwrap();
    }

    #[test]
    /// Verifies model-authored issue validation rejects conflicting updates,
    /// duplicate dependencies, invalid grammar, and out-of-range queries.
    fn issue_action_validation_rejects_invalid_fields() {
        let duplicate_dependencies = vec!["issue-1".to_string(), "issue-1".to_string()];
        let error = validate_issue_update(IssueUpdateValidation {
            kind: None,
            state: None,
            title: None,
            body: Some("replacement"),
            clear_body: true,
            notes: None,
            clear_notes: false,
            depends_on: Some(&duplicate_dependencies),
            clear_depends_on: false,
        })
        .unwrap_err();
        assert!(error.contains("set and clear body"), "{error}");

        for query in [
            IssueQueryValidation {
                kind: Some("bug"),
                state: None,
                text: None,
                limit: None,
            },
            IssueQueryValidation {
                kind: None,
                state: Some("closed"),
                text: None,
                limit: None,
            },
            IssueQueryValidation {
                kind: None,
                state: None,
                text: Some("bad\0query"),
                limit: None,
            },
            IssueQueryValidation {
                kind: None,
                state: None,
                text: None,
                limit: Some(201),
            },
        ] {
            assert!(validate_issue_query(query).is_err());
        }
    }
}
