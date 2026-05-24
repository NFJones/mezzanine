//! Provider event error and task-state formatting helpers.
//!
//! Runtime provider workers report structured event data back to the owning
//! session service. This module keeps the small translation layer from wire
//! strings into runtime error/state values separate from the agent lifecycle
//! orchestration code.

use super::*;

/// Maps serialized provider event error kinds to Mezzanine error kinds.
///
/// # Parameters
/// - `kind`: Provider event error-kind string from the async provider worker.
fn runtime_provider_event_error_kind(kind: &str) -> crate::error::MezErrorKind {
    match kind {
        "invalid_args" | "InvalidArgs" => crate::error::MezErrorKind::InvalidArgs,
        "config" | "Config" => crate::error::MezErrorKind::Config,
        "io" | "Io" => crate::error::MezErrorKind::Io,
        "conflict" | "Conflict" => crate::error::MezErrorKind::Conflict,
        "not_found" | "NotFound" => crate::error::MezErrorKind::NotFound,
        "forbidden" | "Forbidden" => crate::error::MezErrorKind::Forbidden,
        "not_implemented" | "NotImplemented" => crate::error::MezErrorKind::NotImplemented,
        _ => crate::error::MezErrorKind::InvalidState,
    }
}

/// Builds a typed provider event error from serialized async-provider fields.
///
/// # Parameters
/// - `kind`: Provider event error-kind string.
/// - `message`: Human-readable error message.
/// - `provider_failure_json`: Optional provider failure payload.
/// - `provider_raw_text`: Optional raw provider response text.
pub(super) fn runtime_provider_event_error(
    kind: &str,
    message: &str,
    provider_failure_json: Option<&str>,
    provider_raw_text: Option<&str>,
) -> MezError {
    let mut error = MezError::new(runtime_provider_event_error_kind(kind), message);
    if let Some(raw_text) = provider_raw_text {
        error = error.with_provider_raw_text(raw_text.to_string());
    }
    if let Some(failure_json) = provider_failure_json {
        error = error.with_provider_failure_json(failure_json.to_string());
    }
    error
}

/// Returns the status suffix used in task-result presentation.
///
/// # Parameters
/// - `state`: Current task state to render.
pub(super) fn runtime_task_state_suffix(state: TaskState) -> &'static str {
    match state {
        TaskState::Queued => "queued",
        TaskState::Running => "running",
        TaskState::Blocked => "blocked",
        TaskState::Succeeded => "succeeded",
        TaskState::Failed => "failed",
        TaskState::Cancelled => "cancelled",
    }
}
