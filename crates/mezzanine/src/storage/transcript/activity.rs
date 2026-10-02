//! Versioned semantic presentation source for actor-owned activity components.
//!
//! These records describe accepted presentation, never authorize execution.
//! Exact response and action ordinals separate siblings and repeated action ids.
//! An optional transaction identifies executor attempts only when supplied by
//! that owner; absence is not reconstructed from labels or timestamps. The
//! envelope retains the original renderer and source, so disclosure and replay
//! cannot replace canonical evidence with a truncated display reconstruction.

use crate::error::{MezError, Result};
use serde::{Deserialize, Serialize};

/// Renderer identity for the version-one activity envelope, not config schema.
pub(crate) const ACTIVITY_CONTENT_TYPE: &str =
    "application/vnd.mezzanine.agent-presentation.activity-v1+json; charset=utf-8";
/// Maximum encoded activity record; display bounds are independently smaller.
const MAX_ACTIVITY_BYTES: usize = 2 * 1024 * 1024;

/// Explicit producer-owned component category; never derived from label text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ActivityComponentKind {
    /// Accepted batch rationale.
    Rationale,
    /// Accepted model summary preceding command intent.
    Summary,
    /// Exact accepted command intent, not proof of execution.
    Command,
    /// Executor-admitted action header.
    Header,
    /// Settled canonical result display.
    Result,
    /// Visible runtime outcome, including denial or pending approval.
    Outcome,
    /// Positively confirmed filesystem effect.
    ConfirmedMutation,
}

/// Immutable identity and source for one accepted activity component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActivitySource {
    /// Envelope version; independent of TSV and configuration versions.
    pub version: u8,
    /// Conversation that owns this component.
    pub conversation_id: String,
    /// Exact turn, not adjacent timestamp correlation.
    pub turn_id: String,
    /// Existing provider execution group binding the request and response.
    pub response_id: String,
    /// Execution-scoped action id; absent for response-wide rationale.
    pub action_id: Option<String>,
    /// Authored action ordinal; absent for response-wide rationale.
    pub action_ordinal: Option<usize>,
    /// Executor transaction identity when positively available.
    pub transaction: Option<String>,
    /// Confirmed executor section and path; absent for non-mutation components.
    #[serde(default)]
    pub mutation: Option<ActivityMutation>,
    /// Explicit producer category.
    pub kind: ActivityComponentKind,
    /// Evidence-backed lifecycle state, not inferred from component receipt.
    pub status: String,
    /// Original semantic renderer selected by the presentation producer.
    pub content_type: String,
    /// Exact source retained within existing presentation retention limits.
    pub source: String,
    /// Bounded live preview source replayed through the same semantic renderer.
    #[serde(default)]
    pub preview_source: Option<String>,
    /// Accepted intent supplied by the response owner, not inferred from logs.
    #[serde(default)]
    pub intent: ActivityIntent,
}

/// Exact executor-confirmed mutation endpoint, independent of diff parsing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActivityMutation {
    /// Zero-based executor-confirmed section ordinal.
    pub section_index: usize,
    /// Exact executor-supplied user-visible path, not inferred from diff text.
    pub path: String,
}

/// Visible accepted intent retained independently of settled result details.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActivityIntent {
    /// Visible batch rationale; absent when the logging policy hides it.
    pub rationale: Option<String>,
    /// Visible action summary; not execution evidence.
    pub summary: Option<String>,
    /// Accepted command source when this action owns shell intent.
    pub command: Option<String>,
    /// Accepted semantic action header.
    pub header: Option<String>,
}

impl ActivitySource {
    /// Rejects invalid identities, nested envelopes and over-budget sources.
    pub fn validate(&self) -> Result<()> {
        mez_agent::transcript::validate_conversation_id(&self.conversation_id)
            .map_err(|error| MezError::invalid_args(error.to_string()))?;
        let token = |value: &str| {
            !value.is_empty() && value.len() <= 1024 && !value.chars().any(char::is_control)
        };
        if self.version != 1
            || !token(&self.turn_id)
            || !token(&self.response_id)
            || self.action_id.is_some() != self.action_ordinal.is_some()
            || self.action_id.as_deref().is_some_and(|value| !token(value))
            || self
                .mutation
                .as_ref()
                .is_some_and(|mutation| !token(&mutation.path))
            || (self.kind == ActivityComponentKind::ConfirmedMutation
                && (self.mutation.is_none()
                    || self.transaction.is_none()
                    || self.action_id.is_none()
                    || self.status != "confirmed"))
            || (self.kind != ActivityComponentKind::ConfirmedMutation && self.mutation.is_some())
            || (self.status == "confirmed" && self.kind != ActivityComponentKind::ConfirmedMutation)
            || self
                .transaction
                .as_deref()
                .is_some_and(|value| !token(value))
            || !token(&self.content_type)
            || self.content_type == ACTIVITY_CONTENT_TYPE
            || self.source.len() > MAX_ACTIVITY_BYTES
            || [
                &self.intent.rationale,
                &self.intent.summary,
                &self.intent.command,
                &self.intent.header,
            ]
            .into_iter()
            .flatten()
            .any(|source| source.len() > MAX_ACTIVITY_BYTES)
            || self
                .preview_source
                .as_ref()
                .is_some_and(|source| source.len() > MAX_ACTIVITY_BYTES)
            || !matches!(
                self.status.as_str(),
                "accepted"
                    | "confirmed"
                    | "running"
                    | "blocked"
                    | "rejected"
                    | "denied"
                    | "succeeded"
                    | "failed"
                    | "cancelled"
                    | "timedout"
                    | "interrupted"
            )
        {
            return Err(MezError::invalid_args("invalid semantic activity source"));
        }
        Ok(())
    }

    /// Encodes accepted source without changing its original renderer payload.
    pub fn encode(&self) -> Result<String> {
        self.validate()?;
        let encoded = serde_json::to_string(self).map_err(|error| {
            MezError::invalid_args(format!("activity encoding failed: {error}"))
        })?;
        if encoded.len() > MAX_ACTIVITY_BYTES {
            return Err(MezError::invalid_args(
                "encoded activity source exceeds byte budget",
            ));
        }
        Ok(encoded)
    }

    /// Decodes bounded source; malformed records cannot become assistant text.
    pub fn decode(encoded: &str) -> Result<Self> {
        if encoded.len() > MAX_ACTIVITY_BYTES {
            return Err(MezError::invalid_args(
                "activity source exceeds byte budget",
            ));
        }
        let source: Self = serde_json::from_str(encoded).map_err(|error| {
            MezError::invalid_args(format!("activity decoding failed: {error}"))
        })?;
        source.validate()?;
        Ok(source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reused action ids stay distinct across exact response owners; encoding
    /// retains source bytes and rejects unsupported versions/nested renderers.
    #[test]
    fn activity_source_preserves_exact_identity_and_rejects_ambiguous_records() {
        let source = ActivitySource {
            version: 1,
            conversation_id: "conversation".into(),
            turn_id: "turn".into(),
            response_id: "response-1".into(),
            action_id: Some("action".into()),
            action_ordinal: Some(0),
            transaction: None,
            mutation: None,
            kind: ActivityComponentKind::Result,
            status: "succeeded".into(),
            content_type: "text/plain".into(),
            source: "雪\r\n".into(),
            preview_source: None,
            intent: ActivityIntent::default(),
        };
        assert_eq!(
            ActivitySource::decode(&source.encode().unwrap()).unwrap(),
            source
        );
        let mut later = source.clone();
        later.response_id = "response-2".into();
        assert_ne!(later, source);
        later.version = 2;
        assert!(later.encode().is_err());
        later = source.clone();
        later.action_ordinal = None;
        assert!(later.encode().is_err());
        later = source;
        later.content_type = ACTIVITY_CONTENT_TYPE.into();
        assert!(later.encode().is_err());
    }

    /// Confirmed mutation evidence requires an explicit action, executor and
    /// endpoint. Missing fields and confirmation attached to other component
    /// kinds are rejected rather than interpreted as whole-action success.
    #[test]
    fn activity_confirmation_requires_exact_endpoint_evidence() {
        let source = ActivitySource {
            version: 1,
            conversation_id: "conversation".into(),
            turn_id: "turn".into(),
            response_id: "response".into(),
            action_id: Some("action".into()),
            action_ordinal: Some(0),
            transaction: Some("attempt:exact".into()),
            mutation: Some(ActivityMutation {
                section_index: 1,
                path: "note.txt".into(),
            }),
            kind: ActivityComponentKind::ConfirmedMutation,
            status: "confirmed".into(),
            content_type: "text/x-diff; charset=utf-8".into(),
            source: "diff".into(),
            preview_source: None,
            intent: ActivityIntent::default(),
        };
        assert_eq!(
            ActivitySource::decode(&source.encode().unwrap()).unwrap(),
            source
        );
        let mut invalid = source.clone();
        invalid.mutation = None;
        assert!(invalid.encode().is_err());
        invalid = source.clone();
        invalid.transaction = None;
        assert!(invalid.encode().is_err());
        invalid = source.clone();
        invalid.action_id = None;
        invalid.action_ordinal = None;
        assert!(invalid.encode().is_err());
        invalid = source.clone();
        invalid.status = "succeeded".into();
        assert!(invalid.encode().is_err());
        invalid = source.clone();
        invalid.kind = ActivityComponentKind::Result;
        assert!(invalid.encode().is_err());
        invalid = source;
        invalid.mutation.as_mut().unwrap().path = "bad\npath".into();
        assert!(invalid.encode().is_err());
    }
}
