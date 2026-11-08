//! Decoding completed compactor data never executes the returned actions.

use crate::{AgentActionPayload, ModelResponse, SayStatus};

/// Completed response identified itself as a summary frame but was not exactly
/// one final say. It carries no rejected model text or source content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionSummaryDecodeError;

impl std::fmt::Display for CompactionSummaryDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("completed compaction response contained a malformed summary frame")
    }
}

impl std::error::Error for CompactionSummaryDecodeError {}

/// Decodes an optional final-say frame from completed provider output. Raw text
/// is supported; ordinary JSON/code does not become an executable MAAP envelope
/// merely by mentioning action field names. Recognizable MAAP envelopes and
/// explicit fences must decode to exactly one final say. No action is executed.
/// Empty text is returned for the caller's finite empty-output correction;
/// genuine parser/output resource bounds remain independently enforced.
pub fn decode_completed_summary(
    response: &ModelResponse,
    turn_id: &str,
    agent_id: &str,
) -> Result<String, CompactionSummaryDecodeError> {
    let raw = response.raw_text.trim();
    let decoded = if response.action_batch.is_none() {
        let envelope = serde_json::from_str::<serde_json::Value>(raw)
            .ok()
            .and_then(|value| value.as_object().cloned())
            .is_some_and(|object| {
                object.contains_key("rationale")
                    && object
                        .get("actions")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|actions| {
                            actions.is_empty()
                                || actions.iter().any(|action| {
                                    action
                                        .as_object()
                                        .is_some_and(|action| action.contains_key("type"))
                                })
                        })
            });
        if envelope {
            Some(
                crate::parse_maap_action_batch_json_for_turn(raw, turn_id, agent_id)
                    .map_err(|_| CompactionSummaryDecodeError)?,
            )
        } else {
            crate::parse_fenced_maap_action_batch_for_turn(raw, turn_id, agent_id)
                .map_err(|_| CompactionSummaryDecodeError)?
        }
    } else {
        None
    };
    match response.action_batch.as_ref().or(decoded.as_ref()) {
        Some(batch) => match batch.actions.as_slice() {
            [action] => match &action.payload {
                AgentActionPayload::Say {
                    status: SayStatus::Final,
                    text,
                    ..
                } => Ok(text.trim().to_string()),
                _ => Err(CompactionSummaryDecodeError),
            },
            _ => Err(CompactionSummaryDecodeError),
        },
        None => Ok(raw.to_string()),
    }
}
