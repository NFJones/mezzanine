//! Provider error classification and diagnostic shaping.
//!
//! This module owns provider-facing error text extraction, sanitized failure
//! payload construction, retry/context/output-limit classification, and
//! malformed model-output diagnostics.

use crate::error::MezError;
use mez_agent::{
    ProviderErrorKind, ProviderErrorRetryClass, classify_provider_error_retry,
    provider_malformed_output_error,
};

/// Classifies one provider failure for runtime recovery and retry handling.
///
/// The classifier preserves existing precedence: context-limit and
/// output-limit recovery win over generic transport retry, unsupported 400s do
/// not retry, and provider-authored retry invitations remain visible.
pub(crate) fn provider_error_retry_class(error: &MezError) -> ProviderErrorRetryClass {
    provider_error_retry_class_from_parts(
        error.kind(),
        error.message(),
        error.provider_failure_json(),
    )
}

/// Classifies provider failure fields after an error crosses an async boundary.
///
/// Provider worker events carry the stable error kind, message, and sanitized
/// provider failure payload separately. This helper keeps their retry policy in
/// sync with in-process `MezError` classification.
pub(crate) fn provider_error_retry_class_from_parts(
    kind: crate::error::MezErrorKind,
    message: &str,
    provider_failure_json: Option<&str>,
) -> ProviderErrorRetryClass {
    classify_provider_error_retry(kind.into(), message, provider_failure_json)
}

/// Converts a serialized provider-event error kind into a Mezzanine error kind.
///
/// Async provider workers carry error kinds as strings across actor channels.
/// Keeping this parser beside provider retry classification and error-envelope
/// construction prevents runtime and async-runtime copies from drifting.
pub(crate) fn provider_event_error_kind(kind: &str) -> crate::error::MezErrorKind {
    ProviderErrorKind::from_event_name(kind)
        .map(Into::into)
        .unwrap_or(crate::error::MezErrorKind::InvalidState)
}

/// Builds a provider/runtime error envelope from serialized provider-event fields.
///
/// The resulting `MezError` preserves the structured provider failure payload
/// and raw provider text while sharing the event-kind parser used by retry
/// classification.
pub(crate) fn provider_event_error_from_parts(
    kind: &str,
    message: &str,
    provider_failure_json: Option<&str>,
    provider_raw_text: Option<&str>,
) -> MezError {
    let mut error = MezError::new(provider_event_error_kind(kind), message);
    if let Some(raw_text) = provider_raw_text {
        error = error.with_provider_raw_text(raw_text.to_string());
    }
    if let Some(failure_json) = provider_failure_json {
        error = error.with_provider_failure_json(failure_json.to_string());
    }
    error
}

/// Runs the insert provider failure value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
/// Runs the provider maap parse error operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn provider_maap_parse_error(error: impl Into<MezError>, raw_text: &str) -> MezError {
    let error = error.into();
    provider_malformed_output_error(error.kind().into(), error.message(), raw_text).into()
}
