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

/// Recommended next step after one ambiguous Bubblewrap failure assessment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxFailureAssessmentDecision {
    /// Return the failure evidence to the acting model for a safer correction,
    /// narrower diagnostic, or supported alternative action.
    ModelRecovery,
    /// Offer one warned, approval-gated unsandboxed execution attempt.
    UnsandboxedApproval,
    /// Treat the result as an ordinary command failure without sandbox advice.
    OrdinaryFailure,
}

impl SandboxFailureAssessmentDecision {
    /// Returns the stable wire spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ModelRecovery => "model_recovery",
            Self::UnsandboxedApproval => "unsandboxed_approval",
            Self::OrdinaryFailure => "ordinary_failure",
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
    /// Explicit conservative next-step recommendation.
    pub decision: SandboxFailureAssessmentDecision,
    /// Active Bubblewrap restriction specifically implicated by the evidence.
    pub restriction_id: Option<String>,
    /// Whether reasonable sandboxed corrections, diagnostics, and supported
    /// alternate actions have been exhausted.
    pub sandboxed_recovery_exhausted: bool,
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
        recovery_input: None,
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
                content: "Classify one ambiguous Bubblewrap payload failure. Return only the requested JSON. Never infer causality from exit code alone. Choose sandbox_failure only when bounded evidence identifies a specific active restriction as the likely cause; otherwise choose command_failure or uncertain. Prefer model_recovery whenever a corrected sandboxed command, narrower diagnostic, or supported alternate action is reasonable. Choose unsandboxed_approval only for high-confidence sandbox_failure evidence naming the active restriction and only when sandbox-preserving recovery is exhausted. Use ordinary_failure when no sandbox-specific recovery advice is warranted. The payload may already have produced partial effects, and your response never grants execution authority.".to_string(),
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
    const REQUIRED_FIELDS: [&str; 7] = [
        "version",
        "class",
        "confidence",
        "rationale",
        "decision",
        "restriction_id",
        "sandboxed_recovery_exhausted",
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
    let decision = match object.get("decision").and_then(serde_json::Value::as_str) {
        Some("model_recovery") => SandboxFailureAssessmentDecision::ModelRecovery,
        Some("unsandboxed_approval") => SandboxFailureAssessmentDecision::UnsandboxedApproval,
        Some("ordinary_failure") => SandboxFailureAssessmentDecision::OrdinaryFailure,
        _ => {
            return Err(SandboxFailureAssessmentError::new(
                "sandbox failure assessment decision is invalid",
            ));
        }
    };
    let restriction_id = match object.get("restriction_id") {
        Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(value))
            if !value.trim().is_empty() && value.len() <= 128 =>
        {
            Some(value.clone())
        }
        _ => {
            return Err(SandboxFailureAssessmentError::new(
                "sandbox failure assessment restriction_id is invalid",
            ));
        }
    };
    let sandboxed_recovery_exhausted = object
        .get("sandboxed_recovery_exhausted")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| {
            SandboxFailureAssessmentError::new(
                "sandbox failure assessment sandboxed_recovery_exhausted is missing",
            )
        })?;
    if decision == SandboxFailureAssessmentDecision::UnsandboxedApproval
        && (class != SandboxFailureAssessmentClass::SandboxFailure
            || restriction_id.is_none()
            || !sandboxed_recovery_exhausted)
    {
        return Err(SandboxFailureAssessmentError::new(
            "unsandboxed approval requires a named sandbox failure and exhausted sandboxed recovery",
        ));
    }
    Ok(SandboxFailureAssessment {
        class,
        confidence,
        rationale,
        decision,
        restriction_id,
        sandboxed_recovery_exhausted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Valid assessments retain typed attribution and explicit recovery intent.
    #[test]
    fn parses_typed_sandbox_failure_assessment() {
        let assessment = sandbox_failure_assessment_from_text(
            r#"{"version":1,"class":"sandbox_failure","confidence":0.9,"rationale":"write was denied by the authority projection","decision":"unsandboxed_approval","restriction_id":"authority-mounts-only","sandboxed_recovery_exhausted":true}"#,
        )
        .unwrap();
        assert_eq!(
            assessment.class,
            SandboxFailureAssessmentClass::SandboxFailure
        );
        assert_eq!(
            assessment.decision,
            SandboxFailureAssessmentDecision::UnsandboxedApproval
        );
        assert_eq!(
            assessment.restriction_id.as_deref(),
            Some("authority-mounts-only")
        );
    }

    /// Uncertain or command failures cannot smuggle an approval recommendation.
    #[test]
    fn rejects_retry_for_non_sandbox_classification() {
        assert!(
            sandbox_failure_assessment_from_text(
                r#"{"version":1,"class":"uncertain","confidence":0.4,"rationale":"insufficient evidence","decision":"unsandboxed_approval","restriction_id":"minimal-path","sandboxed_recovery_exhausted":true}"#,
            )
            .is_err()
        );
    }

    /// Model recovery remains valid without claiming a specific restriction
    /// or exhausting safer sandbox-preserving options.
    #[test]
    fn parses_conservative_model_recovery() {
        let assessment = sandbox_failure_assessment_from_text(
            r#"{"version":1,"class":"sandbox_failure","confidence":0.72,"rationale":"a narrower diagnostic can identify the missing executable","decision":"model_recovery","restriction_id":"minimal-path","sandboxed_recovery_exhausted":false}"#,
        )
        .unwrap();

        assert_eq!(
            assessment.decision,
            SandboxFailureAssessmentDecision::ModelRecovery
        );
        assert!(!assessment.sandboxed_recovery_exhausted);
    }
}
