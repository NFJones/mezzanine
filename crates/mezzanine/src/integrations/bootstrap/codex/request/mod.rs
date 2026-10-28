//! Memory-only facts from Codex observation-only model request hooks.
//!
//! Public upstream hooks/events/model_request.rs and its generated command schema
//! at 1cc3cf8060c5f1dbd4e3a4d75e1a77b523e0c1ad expose per-request token_usage,
//! including incurred usage on failed/cancelled requests. That source is evidence,
//! not a version gate. Input has no request index/ID and its model is selected
//! configuration, so neither is invented here. Native source, pre/post pairing,
//! immutable origin, durable identity/ACK and delivery belong to the runtime.
//! Child facts retain an explicit selector but cannot borrow parent authority.
//! No error/tool/content, cwd/transcript, credentials, model label or arbitrary
//! metadata leaves this projection, and it has no I/O or control effects.

use crate::error::{MezError, Result};
use crate::storage::token_usage::ExternalCounters;

/// Final execution status is evidence, not a rule to erase incurred usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    /// The sampling request reached its completed provider boundary.
    Completed,
    /// A request failed; separately reported usage may still be incurred.
    Failed,
    /// Cancellation does not prove that no upstream work was incurred.
    Cancelled,
}

/// Request lifecycle facts cannot be interpreted as parent presentation events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Phase {
    /// No counters or source-owned request identity is inferred from a pre-hook.
    Before,
    /// Optional final counters preserve absence separately from explicit zero.
    After {
        /// The source's fixed status, independent of counter availability.
        status: Status,
        /// Checked inclusive usage; absent/null/invalid data remains unknown.
        counters: Option<ExternalCounters>,
    },
}

/// Content-free metadata, intentionally not serializable as a control capsule.
/// Runtime consumers must independently qualify every original native scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Observation {
    /// Exact observed session label; not a pane or source capability.
    pub(crate) session: String,
    /// Exact observed turn label; not a per-request ID or sequence number.
    pub(crate) turn: String,
    /// A child loop must be correlated explicitly rather than treated as main.
    pub(crate) agent_id: Option<String>,
    /// A pre boundary or independently available finalized counters.
    pub(crate) phase: Phase,
}

/// Parses only known model hooks under finite unique-object bounds. Unknown
/// events return None; malformed selectors/status/JSON return a content-free
/// error. Unknown or invalid counter data is an unavailable After fact, not zero.
/// Private and selected-model fields are neither projected nor used as identity.
pub(crate) fn normalize(bytes: &[u8]) -> Result<Option<Observation>> {
    let unavailable = || MezError::invalid_args("Codex model request observation unavailable");
    if bytes.len() > 64 * 1024 {
        return Err(unavailable());
    }
    let value = crate::protocol::strict_json::decode(bytes).map_err(|_| unavailable())?;
    let object = value
        .as_object()
        .filter(|object| object.len() <= 256)
        .ok_or_else(unavailable)?;
    let event = object
        .get("hook_event_name")
        .and_then(serde_json::Value::as_str);
    if !matches!(event, Some("PreModelRequest" | "PostModelRequest")) {
        return Ok(None);
    }
    let session = super::identifier(&value, "session_id")?;
    let turn = super::identifier(&value, "turn_id")?;
    let agent_id = if object.contains_key("agent_id") {
        Some(super::identifier(&value, "agent_id")?)
    } else if object.contains_key("agent_type") {
        // A child-shaped input without its identity cannot become a main fact.
        return Err(unavailable());
    } else {
        None
    };
    let phase = if event == Some("PreModelRequest") {
        Phase::Before
    } else {
        let status = match value.get("status").and_then(serde_json::Value::as_str) {
            Some("completed") => Status::Completed,
            Some("failed") => Status::Failed,
            Some("cancelled") => Status::Cancelled,
            _ => return Err(unavailable()),
        };
        Phase::After {
            status,
            counters: value.get("token_usage").and_then(counters),
        }
    };
    Ok(Some(Observation {
        session,
        turn,
        agent_id,
        phase,
    }))
}

/// Reads a nonnegative signed source counter without coercing strings/floats,
/// saturating overflow, or turning missing/null fields into invented zero.
fn natural(value: &serde_json::Value) -> Option<u64> {
    value.as_i64().and_then(|number| u64::try_from(number).ok())
}

/// Projects inclusive totals once; cache and reasoning are subsets. Optional
/// category absence stays unknown, while malformed known values invalidate the
/// outcome. Redundant total is checked when present, never added as expense.
fn counters(value: &serde_json::Value) -> Option<ExternalCounters> {
    let object = value.as_object()?;
    let category = |name: &str| -> Option<Option<u64>> {
        match object.get(name) {
            None | Some(serde_json::Value::Null) => Some(None),
            Some(value) => natural(value).map(Some),
        }
    };
    let counters = ExternalCounters {
        input_tokens: natural(object.get("input_tokens")?)?,
        output_tokens: natural(object.get("output_tokens")?)?,
        reasoning_tokens: category("reasoning_output_tokens")?,
        cached_input_tokens: category("cached_input_tokens")?,
        cache_write_input_tokens: category("cache_write_input_tokens")?,
    };
    if let Some(total) = object.get("total_tokens")
        && natural(total)? != counters.input_tokens.checked_add(counters.output_tokens)?
    {
        return None;
    }
    counters.validate().ok()?;
    Some(counters)
}

#[cfg(test)]
mod tests;
