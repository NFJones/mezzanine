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
/// Unknown identifiers return `None` so callers can apply their fail-closed
/// error policy instead of silently relabeling the kind as `InvalidState`.
pub(crate) fn provider_event_error_kind(kind: &str) -> Option<crate::error::MezErrorKind> {
    ProviderErrorKind::from_event_name(kind).map(Into::into)
}

/// Returns a bounded char-boundary-safe copy of a provider event kind name.
///
/// Unknown kind names are worker-supplied strings; diagnostics keep a short
/// bounded prefix so operator logs never retain long raw payload fragments.
pub(crate) fn bounded_provider_event_kind(kind: &str) -> String {
    const PROVIDER_EVENT_KIND_LIMIT_BYTES: usize = 64;
    if kind.len() <= PROVIDER_EVENT_KIND_LIMIT_BYTES {
        return kind.to_string();
    }
    let mut end = PROVIDER_EVENT_KIND_LIMIT_BYTES;
    while !kind.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}...", &kind[..end])
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
    let (error_kind, message_text) = match provider_event_error_kind(kind) {
        Some(parsed_kind) => (parsed_kind, message.to_string()),
        None => (
            crate::error::MezErrorKind::InvalidState,
            format!(
                "provider event kind unknown: `{}`: {message}",
                bounded_provider_event_kind(kind)
            ),
        ),
    };
    let mut error = MezError::new(error_kind, &message_text);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Malformed-MAAP errors classify NonRetryable for the in-process kind and
    /// for the async event-kind round-trip, regardless of incidental payload
    /// text, so the model-repair path owns recovery instead of transport retry.
    #[test]
    fn malformed_maap_output_classifies_non_retryable_across_kind_round_trip() {
        let message = "provider MAAP output is malformed: mezzanine-action-json block is invalid JSON: expected `,` or `}` at line 1 column 282";
        let failure_json = Some(
            r#"{"type":"malformed_model_output","error":{"kind":"invalid_state","message":"mezzanine-action-json block is invalid JSON"}}"#,
        );
        for kind in ["invalid_args", "invalid_state", "rate_limited"] {
            let event_kind = provider_event_error_kind(kind).expect("known event kind");
            let class = provider_error_retry_class_from_parts(event_kind, message, failure_json);
            assert_eq!(class, ProviderErrorRetryClass::NonRetryable, "{kind}");
        }
    }

    /// Unknown provider event kinds are marked unknown and fail closed
    /// instead of being silently relabeled as InvalidState.
    #[test]
    fn unknown_provider_event_kinds_are_marked_and_never_relabeled() {
        assert_eq!(provider_event_error_kind("bogus"), None);
        assert_eq!(provider_event_error_kind("rate-limited"), None);
        let envelope =
            provider_event_error_from_parts("bogus", "provider failure detail", None, Some("raw"));
        assert_eq!(envelope.kind(), crate::error::MezErrorKind::InvalidState);
        assert_eq!(
            envelope.message(),
            "provider event kind unknown: `bogus`: provider failure detail"
        );
        assert_eq!(envelope.provider_raw_text(), Some("raw"));
        let long_kind = "x".repeat(200);
        let bounded = provider_event_error_from_parts(&long_kind, "detail", None, None);
        assert!(
            bounded.message().contains("provider event kind unknown"),
            "{}",
            bounded.message()
        );
        assert!(
            !bounded.message().contains(&long_kind),
            "kind must stay bounded"
        );
    }
}
