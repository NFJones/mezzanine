//! Provider failure retry and recovery classification.
//!
//! This module owns provider-domain interpretation of sanitized failure
//! diagnostics. Product error envelopes and async transport channels adapt
//! their error kinds into these dependency-neutral contracts.

/// Stable provider error categories needed by retry classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderErrorKind {
    /// Invalid model- or caller-authored arguments.
    InvalidArgs,
    /// Invalid provider or turn state.
    InvalidState,
    /// Invalid product configuration.
    Config,
    /// A transport or operating-system I/O failure.
    Io,
    /// A conflicting operation.
    Conflict,
    /// A missing provider-side resource.
    NotFound,
    /// A forbidden provider operation.
    Forbidden,
    /// A provider rate-limit rejection.
    RateLimited,
    /// An unsupported provider operation.
    NotImplemented,
}

impl ProviderErrorKind {
    /// Returns the stable snake-case identifier used across provider worker
    /// event channels.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidArgs => "invalid_args",
            Self::InvalidState => "invalid_state",
            Self::Config => "config",
            Self::Io => "io",
            Self::Conflict => "conflict",
            Self::NotFound => "not_found",
            Self::Forbidden => "forbidden",
            Self::RateLimited => "rate_limited",
            Self::NotImplemented => "not_implemented",
        }
    }

    /// Parses a stable provider worker event identifier.
    ///
    /// Both canonical snake-case identifiers and legacy Rust variant names
    /// are accepted. Unknown identifiers return `None` so the product adapter
    /// can apply its fail-closed error policy.
    pub fn from_event_name(name: &str) -> Option<Self> {
        match name {
            "invalid_args" | "InvalidArgs" => Some(Self::InvalidArgs),
            "invalid_state" | "InvalidState" => Some(Self::InvalidState),
            "config" | "Config" => Some(Self::Config),
            "io" | "Io" => Some(Self::Io),
            "conflict" | "Conflict" => Some(Self::Conflict),
            "not_found" | "NotFound" => Some(Self::NotFound),
            "forbidden" | "Forbidden" => Some(Self::Forbidden),
            "rate_limited" | "RateLimited" => Some(Self::RateLimited),
            "not_implemented" | "NotImplemented" => Some(Self::NotImplemented),
            _ => None,
        }
    }
}

/// Shared retry/recovery class for provider failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderErrorRetryClass {
    /// The request exceeded the provider input context window.
    ContextLimit,
    /// The response exhausted the provider output-token budget.
    OutputLimit,
    /// The same request may be retried without a terminal failure summary.
    RetryableTransport,
    /// The provider failure should terminate the current recovery attempt.
    NonRetryable,
}

/// Provider retry budget and bounded exponential-backoff policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderRetryPolicy {
    /// Maximum retries accepted after the initial provider failure.
    pub max_attempts: u32,
    /// Whether retryable failures bypass the finite attempt budget.
    pub unlimited: bool,
    /// Initial delay used for the first accepted retry.
    pub initial_delay_ms: u64,
    /// Maximum delay applied after exponential growth.
    pub max_delay_ms: u64,
}

impl ProviderRetryPolicy {
    /// Returns whether a failure class remains eligible under the recorded
    /// retry-attempt count.
    pub const fn should_retry(
        self,
        recorded_attempts: u64,
        retry_class: ProviderErrorRetryClass,
    ) -> bool {
        let retryable = matches!(
            retry_class,
            ProviderErrorRetryClass::ContextLimit
                | ProviderErrorRetryClass::OutputLimit
                | ProviderErrorRetryClass::RetryableTransport
        );
        let budget_available = recorded_attempts < self.max_attempts as u64
            || (self.unlimited
                && matches!(retry_class, ProviderErrorRetryClass::RetryableTransport));
        retryable && budget_available
    }

    /// Returns one bounded, jittered delay for a one-based retry attempt.
    ///
    /// `jitter_sample` is injected so callers can use runtime randomness while
    /// tests remain deterministic. Provider advice is treated as a minimum
    /// after both local backoff and advice are capped by `max_delay_ms`.
    pub fn delay_ms(
        self,
        attempt: u64,
        advised_delay_ms: Option<u64>,
        jitter_sample: Option<u64>,
    ) -> u64 {
        let exponent = attempt.saturating_sub(1).min(10) as u32;
        let exponential_delay = self
            .initial_delay_ms
            .saturating_mul(2u64.saturating_pow(exponent))
            .min(self.max_delay_ms);
        let local_delay = if exponential_delay == self.max_delay_ms {
            exponential_delay
        } else {
            jitter_sample.map_or(exponential_delay, |jitter_sample| {
                let jitter_floor = exponential_delay / 2;
                let jitter_span = exponential_delay.saturating_sub(jitter_floor);
                jitter_floor.saturating_add(jitter_sample % jitter_span.saturating_add(1))
            })
        };
        local_delay.max(advised_delay_ms.unwrap_or(0).min(self.max_delay_ms))
    }
}

/// Canonical runtime provider retry budget and backoff settings.
pub const DEFAULT_PROVIDER_RETRY_POLICY: ProviderRetryPolicy = ProviderRetryPolicy {
    max_attempts: 5,
    unlimited: false,
    initial_delay_ms: 1_000,
    max_delay_ms: 15 * 60 * 1_000,
};

/// Parses provider `Retry-After` advice from a sanitized failure payload.
///
/// Delta-seconds and HTTP-date forms are supported. `now_unix_ms` is injected
/// to make date handling deterministic. Malformed values return `None`; past
/// dates normalize to an immediate zero-millisecond advisory delay.
pub fn provider_retry_after_delay_ms(
    provider_failure_json: Option<&str>,
    now_unix_ms: u64,
) -> Option<u64> {
    let value: serde_json::Value = serde_json::from_str(provider_failure_json?).ok()?;
    let retry_after = value.get("retry_after")?;
    if let Some(seconds) = retry_after.as_u64() {
        return Some(seconds.saturating_mul(1_000));
    }
    let text = retry_after.as_str()?.trim();
    if let Ok(seconds) = text.parse::<u64>() {
        return Some(seconds.saturating_mul(1_000));
    }
    let advised = httpdate::parse_http_date(text)
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis();
    Some(
        u64::try_from(advised)
            .unwrap_or(u64::MAX)
            .saturating_sub(now_unix_ms),
    )
}

/// Stable failure code recorded for one typed provider HTTP phase timeout.
///
/// The code lives in the structured failure payload instead of a caller matching
/// human-readable error text, so every consumer classifies the same typed
/// transport failure the same way.
pub const PROVIDER_HTTP_TIMEOUT_FAILURE_CODE: &str = "provider_http_timeout";

/// Builds the structured failure payload for one typed provider HTTP timeout.
pub fn provider_http_timeout_failure_json(timeout_phase: &str) -> String {
    serde_json::json!({
        "error": {
            "code": PROVIDER_HTTP_TIMEOUT_FAILURE_CODE,
            "timeout_phase": timeout_phase,
        }
    })
    .to_string()
}

/// Reports whether one sanitized provider failure payload marks a typed timeout.
///
/// Only the structured failure payload is inspected, so an unrelated message that
/// merely mentions a timeout never changes the classification.
pub fn provider_failure_json_is_timeout(provider_failure_json: Option<&str>) -> bool {
    let Some(value) = provider_failure_json
        .and_then(|payload| serde_json::from_str::<serde_json::Value>(payload).ok())
    else {
        return false;
    };
    ["/error/code", "/code", "/body/error/code", "/body/code"]
        .iter()
        .any(|pointer| {
            value.pointer(pointer).and_then(serde_json::Value::as_str)
                == Some(PROVIDER_HTTP_TIMEOUT_FAILURE_CODE)
        })
}

/// Classifies sanitized provider failure fields for recovery and retry policy.
///
/// Context and output limits take precedence over generic transport retries.
/// Unsupported status-400 failures remain terminal, while rate limits,
/// server errors, transient provider types, and explicit retry invitations are
/// retryable.
///
/// The classifier inputs are the typed error kind, the sanitized primary
/// display message produced by the shared diagnostics boundary, and the
/// sanitized structured failure payload. The unsupported-parameter check reads
/// sanitized structured fields only. The context-limit, output-limit,
/// transient, and retry-invitation checks read those sanitized structured
/// fields and that sanitized display message, whose provider-derived portions
/// were bounded and redacted at the same boundary, and the transport-text
/// fallback below matches crate-owned message text this crate writes. No check
/// reads raw provider text, so withholding credential-shaped display text
/// cannot move a failure between retryable and terminal handling.
pub fn classify_provider_error_retry(
    kind: ProviderErrorKind,
    message: &str,
    provider_failure_json: Option<&str>,
) -> ProviderErrorRetryClass {
    if provider_error_is_malformed_maap_output(message) {
        return ProviderErrorRetryClass::NonRetryable;
    }
    if provider_error_is_context_limit_exceeded(message, provider_failure_json) {
        return ProviderErrorRetryClass::ContextLimit;
    }
    if provider_error_is_output_limit_exceeded(message, provider_failure_json) {
        return ProviderErrorRetryClass::OutputLimit;
    }
    if provider_error_is_transient_overload_or_unavailable(message, provider_failure_json) {
        return ProviderErrorRetryClass::RetryableTransport;
    }
    if let Some(status_code) = provider_failure_status_code(provider_failure_json) {
        if status_code == 400 && provider_error_is_unsupported_parameter(provider_failure_json) {
            return ProviderErrorRetryClass::NonRetryable;
        }
        if status_code == 429 || (500..=599).contains(&status_code) {
            return ProviderErrorRetryClass::RetryableTransport;
        }
        if provider_error_invites_retry(message, provider_failure_json) {
            return ProviderErrorRetryClass::RetryableTransport;
        }
        return ProviderErrorRetryClass::NonRetryable;
    }
    if kind == ProviderErrorKind::Io {
        return ProviderErrorRetryClass::RetryableTransport;
    }
    if kind != ProviderErrorKind::InvalidState {
        return ProviderErrorRetryClass::NonRetryable;
    }
    if message.contains("provider HTTP request failed")
        || message.contains("provider HTTP response read failed")
        || message.contains("provider HTTP response read stalled")
        || message.contains("provider HTTP timeout phase=")
        || message.contains("provider stream response did not contain SSE data events")
        || provider_error_invites_retry(message, provider_failure_json)
    {
        ProviderErrorRetryClass::RetryableTransport
    } else {
        ProviderErrorRetryClass::NonRetryable
    }
}

/// Reports whether a provider failure message describes malformed MAAP model
/// output owned by the model-repair path.
///
/// Malformed model output is never a transport failure: retry classification
/// must not promote it to a transport retry regardless of error kind or
/// incidental substrings in the sanitized failure payload.
pub fn provider_error_is_malformed_maap_output(message: &str) -> bool {
    message.starts_with("provider MAAP output is malformed:")
}

fn provider_failure_status_code(provider_failure_json: Option<&str>) -> Option<u16> {
    let value: serde_json::Value = serde_json::from_str(provider_failure_json?).ok()?;
    let status_code = value.get("status_code")?.as_u64()?;
    u16::try_from(status_code).ok()
}

/// Reports whether a structured provider failure names an unsupported parameter.
///
/// The check reads only sanitized structured failure fields: the provider's own
/// error code, type, and message, its plain-string error value, and the
/// sanitized whole-body text that the failure shaping records when the provider
/// body was not JSON. Those fields follow the documented detail precedence used
/// to build the primary display message (`/error/message`, `/error_description`,
/// `/message`, then the bounded body text), so a status-400 unsupported
/// parameter keeps its pre-sanitization terminal classification even when the
/// bounded display text was withheld for containing credential-shaped
/// material. The rendered display message is never an input, and no raw
/// provider value is persisted or re-read for the decision.
fn provider_error_is_unsupported_parameter(provider_failure_json: Option<&str>) -> bool {
    provider_error_structured_fields(
        provider_failure_json,
        &[
            "/error/code",
            "/error/type",
            "/error/message",
            "/error_description",
            "/error",
            "/message",
            "/body_text",
            "/body/error/code",
            "/body/error/type",
            "/body/error/message",
            "/body/message",
            "/body",
            "/response/error/code",
            "/response/error/type",
            "/response/error/message",
        ],
    )
    .any(|text| text.to_ascii_lowercase().contains("unsupported"))
}

/// Iterates sanitized structured failure strings selected by JSON pointers.
fn provider_error_structured_fields(
    provider_failure_json: Option<&str>,
    pointers: &'static [&'static str],
) -> impl Iterator<Item = String> {
    let parsed = provider_failure_json
        .and_then(|failure| serde_json::from_str::<serde_json::Value>(failure).ok());
    pointers.iter().filter_map(move |pointer| {
        parsed
            .as_ref()?
            .pointer(pointer)?
            .as_str()
            .map(str::to_string)
    })
}

fn provider_error_invites_retry(message: &str, provider_failure_json: Option<&str>) -> bool {
    provider_error_fields(
        message,
        provider_failure_json,
        &[
            "/error/message",
            "/message",
            "/body/error/message",
            "/body/message",
            "/response/error/message",
        ],
    )
    .any(|text| provider_error_text_invites_retry(&text))
}

fn provider_error_is_transient_overload_or_unavailable(
    message: &str,
    provider_failure_json: Option<&str>,
) -> bool {
    provider_error_fields(
        message,
        provider_failure_json,
        &[
            "/error/type",
            "/error/message",
            "/message",
            "/body/error/type",
            "/body/error/message",
            "/body/message",
            "/response/error/type",
            "/response/error/message",
        ],
    )
    .any(|text| provider_error_text_is_transient_overload_or_unavailable(&text))
}

fn provider_error_is_context_limit_exceeded(
    message: &str,
    provider_failure_json: Option<&str>,
) -> bool {
    provider_error_fields(
        message,
        provider_failure_json,
        &[
            "/error/code",
            "/error/type",
            "/error/message",
            "/message",
            "/body/error/code",
            "/body/error/type",
            "/body/error/message",
            "/body/message",
            "/response/error/code",
            "/response/error/type",
            "/response/error/message",
            "/response/incomplete_details/reason",
        ],
    )
    .any(|text| provider_error_text_is_context_limit_exceeded(&text))
}

fn provider_error_is_output_limit_exceeded(
    message: &str,
    provider_failure_json: Option<&str>,
) -> bool {
    provider_error_fields(
        message,
        provider_failure_json,
        &[
            "/incomplete_details/reason",
            "/response/incomplete_details/reason",
            "/body/incomplete_details/reason",
            "/body/response/incomplete_details/reason",
            "/error/code",
            "/error/message",
            "/message",
            "/body/error/code",
            "/body/error/message",
            "/body/message",
            "/response/error/code",
            "/response/error/message",
        ],
    )
    .any(|text| provider_error_text_is_output_limit_exceeded(&text))
}

/// Iterates the classifier inputs for one provider failure in documented order.
///
/// The first element is the sanitized primary display message the product
/// adapter built at the shared diagnostics boundary; the remaining elements are
/// sanitized structured failure fields selected by JSON pointers. Both inputs
/// are already provider-sanitized: the display message carries provider detail
/// that boundary bounded and redacted plus crate-owned label text such as
/// `provider HTTP timeout phase=`, and the structured payload is the sanitized
/// failure object. No caller-supplied raw provider text is read here.
fn provider_error_fields(
    message: &str,
    provider_failure_json: Option<&str>,
    pointers: &'static [&'static str],
) -> impl Iterator<Item = String> {
    let parsed = provider_failure_json
        .and_then(|failure| serde_json::from_str::<serde_json::Value>(failure).ok());
    std::iter::once(message.to_string()).chain(pointers.iter().filter_map(move |pointer| {
        parsed
            .as_ref()?
            .pointer(pointer)?
            .as_str()
            .map(str::to_string)
    }))
}

fn provider_error_text_invites_retry(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("you can retry your request")
        || lower.contains("you can retry the request")
        || (lower.contains("an error occurred while processing your request")
            && lower.contains("retry"))
}

fn provider_error_text_is_transient_overload_or_unavailable(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("api_error")
        || lower.contains("timeout_error")
        || lower.contains("rate_limit_error")
        || lower.contains("overloaded_error")
        || lower.contains("api overloaded")
        || lower.contains("server overloaded")
        || lower.contains("server is overloaded")
        || lower.contains("temporarily unavailable")
        || lower.contains("service unavailable")
        || (lower.contains("overloaded") && lower.contains("try again"))
}

fn provider_error_text_is_context_limit_exceeded(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("context_length_exceeded")
        || lower.contains("context length exceeded")
        || lower.contains("context_window_exceeded")
        || lower.contains("model_context_window_exceeded")
        || lower.contains("request_too_large")
        || lower.contains("exceeds the context window")
        || lower.contains("maximum context length")
        || lower.contains("max context length")
        || lower.contains("context window")
        || lower.contains("prompt is too long")
        || lower.contains("input is too large")
        || lower.contains("input too large")
        || lower.contains("too many input tokens")
        || lower.contains("too many tokens")
        || lower.contains("reduce the length of the messages")
        || lower.contains("reduce the length of your input")
        || lower.contains("request too large for the model")
}

fn provider_error_text_is_output_limit_exceeded(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("max_output_tokens")
        || lower.contains("max_tokens")
        || lower.contains("maximum output tokens")
        || lower.contains("output token limit")
        || lower.contains("output tokens limit")
        || lower.contains("response output limit")
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_PROVIDER_RETRY_POLICY, PROVIDER_HTTP_TIMEOUT_FAILURE_CODE, ProviderErrorKind,
        ProviderErrorRetryClass, ProviderRetryPolicy, classify_provider_error_retry,
        provider_failure_json_is_timeout, provider_http_timeout_failure_json,
        provider_retry_after_delay_ms,
    };
    use crate::ProviderHttpTimeoutPhase;

    /// Verifies only the structured typed-timeout marker classifies a timeout.
    #[test]
    fn provider_timeout_marker_classifies_only_typed_timeouts() {
        let payload = provider_http_timeout_failure_json(ProviderHttpTimeoutPhase::Total.as_str());
        assert!(
            provider_failure_json_is_timeout(Some(&payload)),
            "{payload}"
        );
        let nested = serde_json::json!({
            "body": {"error": {"code": PROVIDER_HTTP_TIMEOUT_FAILURE_CODE}}
        })
        .to_string();
        assert!(provider_failure_json_is_timeout(Some(&nested)), "{nested}");
        assert!(!provider_failure_json_is_timeout(Some(
            &serde_json::json!({"error": {"code": "rate_limited"}}).to_string()
        )));
        assert!(!provider_failure_json_is_timeout(Some("not json")));
        assert!(!provider_failure_json_is_timeout(None));
    }

    /// Verifies retry eligibility accepts recoverable classes only while the
    /// canonical budget remains available.
    #[test]
    fn provider_retry_policy_bounds_eligible_failures() {
        assert!(
            DEFAULT_PROVIDER_RETRY_POLICY
                .should_retry(0, ProviderErrorRetryClass::RetryableTransport)
        );
        assert!(
            DEFAULT_PROVIDER_RETRY_POLICY.should_retry(4, ProviderErrorRetryClass::ContextLimit)
        );
        assert!(
            !DEFAULT_PROVIDER_RETRY_POLICY.should_retry(5, ProviderErrorRetryClass::OutputLimit)
        );
        assert!(
            !DEFAULT_PROVIDER_RETRY_POLICY.should_retry(0, ProviderErrorRetryClass::NonRetryable)
        );
    }

    /// Verifies unlimited retry mode bypasses only the finite attempt budget
    /// while non-retryable provider failures remain terminal.
    #[test]
    fn provider_retry_policy_supports_explicit_unlimited_mode() {
        let policy = ProviderRetryPolicy {
            max_attempts: 2,
            unlimited: true,
            initial_delay_ms: 1_000,
            max_delay_ms: 900_000,
        };

        assert!(policy.should_retry(u64::MAX, ProviderErrorRetryClass::RetryableTransport));
        assert!(!policy.should_retry(u64::MAX, ProviderErrorRetryClass::ContextLimit));
        assert!(!policy.should_retry(u64::MAX, ProviderErrorRetryClass::OutputLimit));
        assert!(!policy.should_retry(u64::MAX, ProviderErrorRetryClass::NonRetryable));
    }

    /// Verifies exponential delays are one-based, jittered deterministically,
    /// honor bounded provider advice, and saturate at the canonical cap.
    #[test]
    fn provider_retry_policy_bounds_exponential_delay() {
        assert_eq!(
            DEFAULT_PROVIDER_RETRY_POLICY.delay_ms(0, None, Some(0)),
            500
        );
        assert_eq!(DEFAULT_PROVIDER_RETRY_POLICY.delay_ms(1, None, None), 1_000);
        assert_eq!(
            DEFAULT_PROVIDER_RETRY_POLICY.delay_ms(2, None, Some(0)),
            1_000
        );
        assert_eq!(
            DEFAULT_PROVIDER_RETRY_POLICY.delay_ms(2, Some(1_750), Some(0)),
            1_750
        );
        assert_eq!(
            DEFAULT_PROVIDER_RETRY_POLICY.delay_ms(1, Some(u64::MAX), Some(0)),
            900_000
        );
        assert_eq!(
            DEFAULT_PROVIDER_RETRY_POLICY.delay_ms(u64::MAX, None, Some(0)),
            900_000
        );
    }

    /// Verifies retry advice accepts delta-seconds and HTTP dates while
    /// malformed and past values remain safe and deterministic.
    #[test]
    fn provider_retry_after_parses_supported_bounded_inputs() {
        assert_eq!(
            provider_retry_after_delay_ms(Some(r#"{"retry_after":"12"}"#), 0),
            Some(12_000)
        );
        assert_eq!(
            provider_retry_after_delay_ms(
                Some(r#"{"retry_after":"Thu, 01 Jan 1970 00:00:20 GMT"}"#),
                5_000,
            ),
            Some(15_000)
        );
        assert_eq!(
            provider_retry_after_delay_ms(
                Some(r#"{"retry_after":"Thu, 01 Jan 1970 00:00:01 GMT"}"#),
                5_000,
            ),
            Some(0)
        );
        assert_eq!(
            provider_retry_after_delay_ms(Some(r#"{"retry_after":"later"}"#), 0),
            None
        );
    }

    /// Verifies provider worker event identifiers remain stable while legacy
    /// variant names continue to decode during rolling runtime transitions.
    #[test]
    fn provider_error_kinds_have_stable_event_names() {
        let cases = [
            (
                ProviderErrorKind::InvalidArgs,
                "invalid_args",
                "InvalidArgs",
            ),
            (
                ProviderErrorKind::InvalidState,
                "invalid_state",
                "InvalidState",
            ),
            (ProviderErrorKind::Config, "config", "Config"),
            (ProviderErrorKind::Io, "io", "Io"),
            (ProviderErrorKind::Conflict, "conflict", "Conflict"),
            (ProviderErrorKind::NotFound, "not_found", "NotFound"),
            (ProviderErrorKind::Forbidden, "forbidden", "Forbidden"),
            (
                ProviderErrorKind::RateLimited,
                "rate_limited",
                "RateLimited",
            ),
            (
                ProviderErrorKind::NotImplemented,
                "not_implemented",
                "NotImplemented",
            ),
        ];
        for (kind, canonical, legacy) in cases {
            assert_eq!(kind.as_str(), canonical);
            assert_eq!(ProviderErrorKind::from_event_name(canonical), Some(kind));
            assert_eq!(ProviderErrorKind::from_event_name(legacy), Some(kind));
        }
        assert_eq!(ProviderErrorKind::from_event_name("unknown"), None);
        assert_eq!(ProviderErrorKind::from_event_name("rate-limited"), None);
        assert_eq!(ProviderErrorKind::from_event_name("invalid"), None);
        assert_eq!(ProviderErrorKind::from_event_name(""), None);
    }

    /// Verifies transport stalls remain retryable after classification moves
    /// below the product error-envelope adapter.
    #[test]
    fn response_read_stalls_are_retryable_transport_failures() {
        assert_eq!(
            classify_provider_error_retry(
                ProviderErrorKind::InvalidState,
                "provider HTTP response read stalled for 50ms while waiting for body chunk",
                None,
            ),
            ProviderErrorRetryClass::RetryableTransport
        );
        assert_eq!(
            classify_provider_error_retry(
                ProviderErrorKind::InvalidState,
                "provider HTTP timeout phase=total limit_ms=100 while waiting for provider response body progress",
                None,
            ),
            ProviderErrorRetryClass::RetryableTransport
        );
    }

    /// Verifies an otherwise successful streaming response with no SSE data
    /// events is retried through the bounded provider transport recovery path.
    #[test]
    fn empty_sse_streams_are_retryable_transport_failures() {
        assert_eq!(
            classify_provider_error_retry(
                ProviderErrorKind::InvalidState,
                "provider stream response did not contain SSE data events",
                None,
            ),
            ProviderErrorRetryClass::RetryableTransport
        );
    }

    /// Verifies explicit retry invitations override otherwise terminal status
    /// codes without weakening unsupported status-400 handling.
    #[test]
    fn retry_invitations_with_status_codes_are_honored() {
        assert_eq!(
            classify_provider_error_retry(
                ProviderErrorKind::InvalidState,
                "Chat Completions API returned status 409: you can retry your request",
                Some(r#"{"status_code":409,"error":{"message":"you can retry your request"}}"#),
            ),
            ProviderErrorRetryClass::RetryableTransport
        );
        assert_eq!(
            classify_provider_error_retry(
                ProviderErrorKind::InvalidState,
                "Chat Completions API returned status 400: Unsupported parameter",
                Some(r#"{"status_code":400,"error":{"message":"Unsupported parameter"}}"#),
            ),
            ProviderErrorRetryClass::NonRetryable
        );
    }

    /// Verifies structured provider status and error types map to the shared
    /// context, transport, and terminal recovery classes.
    #[test]
    fn structured_provider_failures_map_to_recovery_classes() {
        let cases = [
            (
                401,
                "authentication_error",
                ProviderErrorRetryClass::NonRetryable,
            ),
            (
                408,
                "timeout_error",
                ProviderErrorRetryClass::RetryableTransport,
            ),
            (
                413,
                "request_too_large",
                ProviderErrorRetryClass::ContextLimit,
            ),
            (
                429,
                "rate_limit_error",
                ProviderErrorRetryClass::RetryableTransport,
            ),
            (
                529,
                "overloaded_error",
                ProviderErrorRetryClass::RetryableTransport,
            ),
        ];
        for (status, error_type, expected) in cases {
            let failure = format!(
                r#"{{"status_code":{status},"error":{{"type":"{error_type}","message":"{error_type}"}}}}"#
            );
            assert_eq!(
                classify_provider_error_retry(
                    ProviderErrorKind::InvalidState,
                    error_type,
                    Some(&failure),
                ),
                expected,
                "{status} {error_type}"
            );
        }
    }

    /// Malformed MAAP model output is never a transport failure: the stable
    /// malformed-output prefix short-circuits retry classification for every
    /// error kind, even when transport-flavored text or an explicit retry
    /// invitation appears in the sanitized failure payload.
    #[test]
    fn malformed_maap_output_never_classifies_as_transport_retry() {
        let message = "provider MAAP output is malformed: mezzanine-action-json block is invalid JSON: expected `,` or `}` at line 1 column 282";
        for kind in [
            ProviderErrorKind::InvalidArgs,
            ProviderErrorKind::InvalidState,
            ProviderErrorKind::Io,
        ] {
            for payload in [
                None,
                Some(r#"{"error":{"message":"service temporarily unavailable"}}"#),
                Some(r#"{"status_code":429,"retry_after":1}"#),
                Some(r#"{"error":{"message":"you can retry your request"}}"#),
            ] {
                assert_eq!(
                    classify_provider_error_retry(kind, message, payload),
                    ProviderErrorRetryClass::NonRetryable,
                    "{kind:?} payload={payload:?}"
                );
            }
        }
    }

    /// Verifies retry classification is unchanged when the bounded primary
    /// display message is withheld for containing credential-shaped text.
    ///
    /// The classifier must read stable typed and structured categories -- error
    /// kind, HTTP status, and the sanitized structured error type, code, and
    /// message -- so redacting the display text cannot move a rate-limited,
    /// transient, auth/permanent, or unsupported-parameter failure between
    /// retryable and terminal handling.
    #[test]
    fn provider_retry_classification_parity_survives_display_text_redaction() {
        let cases = [
            (
                "rate limited over HTTP",
                ProviderErrorKind::InvalidState,
                "Chat Completions API returned status 429: rate limit reached",
                r#"{"status_code":429,"error":{"type":"rate_limit_error","code":"rate_limited","message":"[REDACTED]"}}"#,
                ProviderErrorRetryClass::RetryableTransport,
            ),
            (
                "transient overload without status",
                ProviderErrorKind::InvalidState,
                "OpenAI stream failed: overloaded_error",
                r#"{"error":{"type":"overloaded_error","message":"[REDACTED]"}}"#,
                ProviderErrorRetryClass::RetryableTransport,
            ),
            (
                "auth/permanent",
                ProviderErrorKind::InvalidState,
                "Anthropic Messages API returned status 401: authentication_error",
                r#"{"status_code":401,"error":{"type":"authentication_error","code":"invalid_api_key","message":"[REDACTED]"}}"#,
                ProviderErrorRetryClass::NonRetryable,
            ),
            (
                "unsupported parameter on 400",
                ProviderErrorKind::InvalidState,
                "Chat Completions API returned status 400: Unsupported parameter",
                r#"{"status_code":400,"error":{"type":"invalid_request_error","message":"Unsupported parameter"}}"#,
                ProviderErrorRetryClass::NonRetryable,
            ),
        ];

        for (label, kind, display, failure, expected) in cases {
            let before = classify_provider_error_retry(kind, display, Some(failure));
            let after = classify_provider_error_retry(kind, "[REDACTED]", Some(failure));
            assert_eq!(before, expected, "before {label}");
            assert_eq!(after, expected, "after {label}");
        }
    }

    /// Verifies malformed output, budget exhaustion, and missing safe metadata
    /// keep their existing conservative classification after sanitization.
    ///
    /// Malformed generated output stays with the bounded repair path, a typed
    /// retryable class still stops at the configured attempt budget, and a
    /// provider failure with no safe structured metadata never becomes an
    /// unbounded retry.
    #[test]
    fn provider_retry_classification_fails_closed_after_sanitization() {
        let malformed = crate::sanitize_provider_primary_error_text(
            "provider MAAP output is malformed: mezzanine-action-json block is invalid JSON",
        );
        for kind in [
            ProviderErrorKind::InvalidArgs,
            ProviderErrorKind::InvalidState,
        ] {
            for payload in [
                None,
                Some(r#"{"status_code":429,"error":{"message":"[REDACTED]"}}"#),
            ] {
                assert_eq!(
                    classify_provider_error_retry(kind, &malformed, payload),
                    ProviderErrorRetryClass::NonRetryable,
                    "{kind:?} payload={payload:?}"
                );
            }
        }

        let retryable = classify_provider_error_retry(
            ProviderErrorKind::InvalidState,
            "[REDACTED]",
            Some(r#"{"status_code":503}"#),
        );
        assert_eq!(retryable, ProviderErrorRetryClass::RetryableTransport);
        assert!(DEFAULT_PROVIDER_RETRY_POLICY.should_retry(0, retryable));
        assert!(
            !DEFAULT_PROVIDER_RETRY_POLICY.should_retry(5, retryable),
            "budget exhaustion must bound eligible retries"
        );
        assert_eq!(
            classify_provider_error_retry(ProviderErrorKind::InvalidState, "[REDACTED]", None),
            ProviderErrorRetryClass::NonRetryable,
            "missing safe metadata must fall back conservatively"
        );
    }

    /// Verifies cancellation is a typed turn state rather than a provider
    /// message match, so sanitizing provider-authored display text cannot
    /// change whether a cancelled turn keeps retrying.
    #[test]
    fn cancellation_is_typed_and_independent_of_provider_text() {
        assert_eq!(
            crate::AgentTurnState::Interrupted,
            crate::AgentTurnState::Interrupted
        );
        assert_ne!(
            crate::AgentTurnState::Interrupted,
            crate::AgentTurnState::Failed
        );
        assert_eq!(
            classify_provider_error_retry(ProviderErrorKind::InvalidState, "[REDACTED]", None),
            ProviderErrorRetryClass::NonRetryable,
            "a cancelled turn's redacted provider text never becomes an unbounded retry"
        );
    }

    /// Verifies the unsupported-parameter status-400 decision survives
    /// sanitization for every provider body shape.
    ///
    /// The pre-sanitization classifier read the rendered display message, so a
    /// status-400 failure that carried the unsupported signal only in a non-JSON
    /// body, in a plain-string error value, or in an ordinary JSON error object
    /// still classified as terminal. The structured check must preserve that
    /// outcome without reading the rendered display message, including for a body
    /// whose text also invites a retry.
    #[test]
    fn provider_unsupported_parameter_classification_parity_across_body_shapes() {
        let cases = [
            (
                "non-JSON body",
                "Unsupported parameter: `temperature`. You can retry your request without it.",
            ),
            (
                "plain-string JSON error value",
                r#"{"error":"Unsupported parameter: `temperature`"}#,
            ),
            (
                "JSON error object",
                r#"{"error":{"type":"invalid_request_error","message":"Unsupported parameter: `temperature`"}}"#,
            ),
        ];

        for (label, body) in cases {
            // Mirrors the product HTTP failure text and structured payload
            // built from the same provider body in production.
            let display = format!(
                "DeepSeek Chat Completions API returned status 400: {}",
                crate::provider_error_detail(body)
            );
            let failure = crate::provider_failure_json(Some(400), body);

            assert_eq!(
                classify_provider_error_retry(
                    ProviderErrorKind::InvalidState,
                    &display,
                    Some(&failure)
                ),
                ProviderErrorRetryClass::NonRetryable,
                "display text {label}"
            );
            assert_eq!(
                classify_provider_error_retry(
                    ProviderErrorKind::InvalidState,
                    "[REDACTED]",
                    Some(&failure)
                ),
                ProviderErrorRetryClass::NonRetryable,
                "withheld display text {label}: {failure}"
            );
        }
    }
}
