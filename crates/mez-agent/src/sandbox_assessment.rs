//! Provider-independent assessment of ambiguous Bubblewrap command failures.
//!
//! Bubblewrap can prove that a payload was executed, but it cannot determine
//! whether a later non-zero exit was caused by sandbox policy or by the
//! command itself. This module defines the bounded evidence, structured model
//! request, and strict response parser used for that attribution. The model
//! may recommend an approval prompt, but it never grants execution authority.

use crate::{
    AgentCapability, AgentTurnRecord, AllowedActionSet, ContextPlacement, ContextSourceKind,
    ModelInteractionKind, ModelMessage, ModelMessageRole, ModelProfile, ModelRequest,
};

/// Maximum command-output bytes included in one sandbox failure assessment.
pub const SANDBOX_FAILURE_ASSESSMENT_OUTPUT_MAX_BYTES: usize = 8 * 1024;

/// Truncates one evidence string at a valid UTF-8 boundary.
fn truncate_assessment_output(value: &str) -> String {
    if value.len() <= SANDBOX_FAILURE_ASSESSMENT_OUTPUT_MAX_BYTES {
        return value.to_string();
    }
    let mut end = SANDBOX_FAILURE_ASSESSMENT_OUTPUT_MAX_BYTES;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    value[..end].to_string()
}

/// Runtime-owned evidence supplied to one ambiguous failure assessment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxFailureAssessmentEvidence {
    /// Stable action kind without command content.
    pub action_kind: String,
    /// Original permission decision.
    pub permission_decision: String,
    /// Stable matched rule identities.
    pub matched_rule_ids: Vec<String>,
    /// Effective filesystem read paths declared by complete effects.
    pub read_effects: Vec<String>,
    /// Effective filesystem write/create/delete/touch paths.
    pub write_effects: Vec<String>,
    /// Whether effects were complete or unknown.
    pub effect_completeness: String,
    /// Bubblewrap payload exit code, proving payload exec occurred.
    pub exit_code: i32,
    /// Bounded combined command output.
    pub output_preview: String,
    /// Whether output was truncated before assessment.
    pub output_truncated: bool,
    /// Stable descriptions of active Bubblewrap restrictions.
    pub sandbox_restrictions: Vec<String>,
}

/// Model-attributed class for an ambiguous Bubblewrap command failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxFailureAssessmentClass {
    /// The observed failure is likely caused by sandbox restrictions.
    SandboxFailure,
    /// The observed failure is likely intrinsic to the command.
    CommandFailure,
    /// Available evidence cannot safely distinguish the cause.
    Uncertain,
}

impl SandboxFailureAssessmentClass {
    /// Returns the stable wire spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SandboxFailure => "sandbox_failure",
            Self::CommandFailure => "command_failure",
            Self::Uncertain => "uncertain",
        }
    }
}

/// Strictly validated model assessment of an ambiguous sandbox failure.
#[derive(Debug, Clone, PartialEq)]
pub struct SandboxFailureAssessment {
    /// Attributed failure class.
    pub class: SandboxFailureAssessmentClass,
    /// Model confidence in the attribution.
    pub confidence: f64,
    /// Short rationale retained for audit diagnostics.
    pub rationale: String,
    /// Whether the model recommends offering an unsandboxed retry approval.
    pub retry_requested: bool,
}

/// Error returned by sandbox assessment request or response validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxFailureAssessmentError {
    message: String,
}

impl SandboxFailureAssessmentError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Returns the bounded validation diagnostic.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for SandboxFailureAssessmentError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SandboxFailureAssessmentError {}

/// Builds the dedicated structured provider request for one ambiguous failure.
pub fn sandbox_failure_assessment_request(
    turn: &AgentTurnRecord,
    model_profile: &ModelProfile,
    evidence: &SandboxFailureAssessmentEvidence,
) -> Result<ModelRequest, SandboxFailureAssessmentError> {
    if evidence.exit_code == 0 {
        return Err(SandboxFailureAssessmentError::new(
            "sandbox failure assessment requires a non-zero payload exit code",
        ));
    }
    let output_preview = truncate_assessment_output(&evidence.output_preview);
    let task = serde_json::json!({
        "action_kind": evidence.action_kind,
        "permission_decision": evidence.permission_decision,
        "matched_rule_ids": evidence.matched_rule_ids,
        "effects": {
            "completeness": evidence.effect_completeness,
            "reads": evidence.read_effects,
            "writes": evidence.write_effects,
        },
        "bubblewrap": {
            "payload_exec_proven": true,
            "exit_code": evidence.exit_code,
            "restrictions": evidence.sandbox_restrictions,
        },
        "output": {
            "preview": output_preview,
            "truncated": evidence.output_truncated
                || evidence.output_preview.len() > SANDBOX_FAILURE_ASSESSMENT_OUTPUT_MAX_BYTES,
        },
        "partial_effect_warning": true,
    })
    .to_string();
    Ok(ModelRequest {
        provider: model_profile.provider.clone(),
        model: model_profile.model.clone(),
        reasoning_effort: model_profile.reasoning_profile.clone(),
        thinking_enabled: model_profile.thinking_enabled(),
        latency_preference: model_profile.latency_preference.clone(),
        prompt_cache_retention: None,
        max_output_tokens: model_profile.max_output_tokens(),
        temperature: None,
        stop: None,
        prompt_cache_session_id: None,
        prompt_cache_lineage_id: None,
        turn_id: turn.turn_id.clone(),
        agent_id: turn.agent_id.clone(),
        available_mcp_tools: Vec::new(),
        memory_actions_enabled: false,
        issue_actions_enabled: false,
        interaction_kind: ModelInteractionKind::SandboxFailureAssessment,
        allowed_actions: AllowedActionSet::for_capability(AgentCapability::RespondOnly),
        messages: vec![
            ModelMessage {
                role: ModelMessageRole::System,
                source: ContextSourceKind::System,
                placement: ContextPlacement::StablePrefix,
                content: "Classify one ambiguous Bubblewrap payload failure. Return only the requested JSON. Never infer causality from exit code alone. Choose sandbox_failure only when the bounded evidence makes sandbox policy the likely cause; otherwise choose command_failure or uncertain. retry_requested may be true only for sandbox_failure. The payload may already have produced partial effects, and your response never grants execution authority.".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::Context,
                source: ContextSourceKind::CommittedEvidence,
                placement: ContextPlacement::ConversationAppend,
                content: task,
            },
        ],
    })
}

/// Parses one strict sandbox failure assessment response.
pub fn sandbox_failure_assessment_from_text(
    text: &str,
) -> Result<SandboxFailureAssessment, SandboxFailureAssessmentError> {
    let value = serde_json::from_str::<serde_json::Value>(text.trim()).map_err(|error| {
        SandboxFailureAssessmentError::new(format!(
            "sandbox failure assessment must be a JSON object: {error}"
        ))
    })?;
    let object = value.as_object().ok_or_else(|| {
        SandboxFailureAssessmentError::new("sandbox failure assessment must be a JSON object")
    })?;
    const REQUIRED_FIELDS: [&str; 5] = [
        "version",
        "class",
        "confidence",
        "rationale",
        "retry_requested",
    ];
    if object.len() != REQUIRED_FIELDS.len()
        || object
            .keys()
            .any(|field| !REQUIRED_FIELDS.contains(&field.as_str()))
    {
        return Err(SandboxFailureAssessmentError::new(
            "sandbox failure assessment fields are incomplete or unsupported",
        ));
    }
    let version = object.get("version").and_then(serde_json::Value::as_u64);
    if version != Some(1) {
        return Err(SandboxFailureAssessmentError::new(
            "sandbox failure assessment version must be 1",
        ));
    }
    let class = match object.get("class").and_then(serde_json::Value::as_str) {
        Some("sandbox_failure") => SandboxFailureAssessmentClass::SandboxFailure,
        Some("command_failure") => SandboxFailureAssessmentClass::CommandFailure,
        Some("uncertain") => SandboxFailureAssessmentClass::Uncertain,
        _ => {
            return Err(SandboxFailureAssessmentError::new(
                "sandbox failure assessment class is invalid",
            ));
        }
    };
    let confidence = object
        .get("confidence")
        .and_then(serde_json::Value::as_f64)
        .filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
        .ok_or_else(|| {
            SandboxFailureAssessmentError::new(
                "sandbox failure assessment confidence must be between 0 and 1",
            )
        })?;
    let rationale = object
        .get("rationale")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= 1_024)
        .ok_or_else(|| {
            SandboxFailureAssessmentError::new(
                "sandbox failure assessment rationale is missing or too long",
            )
        })?
        .to_string();
    let retry_requested = object
        .get("retry_requested")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| {
            SandboxFailureAssessmentError::new(
                "sandbox failure assessment retry_requested is missing",
            )
        })?;
    if retry_requested && class != SandboxFailureAssessmentClass::SandboxFailure {
        return Err(SandboxFailureAssessmentError::new(
            "only sandbox_failure may request an unsandboxed retry",
        ));
    }
    Ok(SandboxFailureAssessment {
        class,
        confidence,
        rationale,
        retry_requested,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Valid assessments retain typed attribution and retry intent.
    #[test]
    fn parses_typed_sandbox_failure_assessment() {
        let assessment = sandbox_failure_assessment_from_text(
            r#"{"version":1,"class":"sandbox_failure","confidence":0.9,"rationale":"write was denied by the read-only projection","retry_requested":true}"#,
        )
        .unwrap();
        assert_eq!(
            assessment.class,
            SandboxFailureAssessmentClass::SandboxFailure
        );
        assert!(assessment.retry_requested);
    }

    /// Uncertain or command failures cannot smuggle a retry recommendation.
    #[test]
    fn rejects_retry_for_non_sandbox_classification() {
        assert!(
            sandbox_failure_assessment_from_text(
                r#"{"version":1,"class":"uncertain","confidence":0.4,"rationale":"insufficient evidence","retry_requested":true}"#,
            )
            .is_err()
        );
    }
}
