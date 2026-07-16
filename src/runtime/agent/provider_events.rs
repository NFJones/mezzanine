//! Provider event error and task-state formatting helpers.
//!
//! Runtime provider workers report structured event data back to the owning
//! session service. This module keeps the small translation layer from wire
//! strings into runtime error/state values separate from the agent lifecycle
//! orchestration code.

use super::MezError;
use crate::agent::provider::provider_event_error_from_parts;

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
    provider_event_error_from_parts(kind, message, provider_failure_json, provider_raw_text)
}
