//! Multiplexer-owned clipboard routing and deterministic effect planning.
//!
//! This module normalizes clipboard policy, decides whether terminal-originated
//! writes may target internal and host clipboards, and selects paste sources.
//! It deliberately performs no host I/O, product authorization, configuration
//! loading, auditing, or terminal protocol parsing. Callers provide explicit
//! authorization results and execute the returned effect intents.

use std::fmt;

/// Configured routing policy for terminal-originated clipboard operations.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardPolicy {
    /// Store writes internally and attempt the product host clipboard effect.
    #[default]
    External,
    /// Store writes only in the mux-owned internal paste buffer.
    Internal,
    /// Reject terminal-originated clipboard operations.
    Disabled,
}

impl ClipboardPolicy {
    /// Normalizes one supported configuration value into typed policy.
    ///
    /// Compatibility spellings are accepted here so product configuration
    /// readers do not duplicate deterministic mode interpretation.
    pub fn from_config_value(value: &str) -> Option<Self> {
        match value {
            "external" | "host" => Some(Self::External),
            "internal" => Some(Self::Internal),
            "disabled" | "off" | "none" => Some(Self::Disabled),
            _ => None,
        }
    }

    /// Returns the canonical configuration spelling for this policy.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::External => "external",
            Self::Internal => "internal",
            Self::Disabled => "disabled",
        }
    }
}

/// Product authorization result supplied to deterministic clipboard policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardAuthorization {
    /// Product security policy permits the requested clipboard operation.
    Allowed,
    /// Product security policy denies the requested clipboard operation.
    Denied,
}

/// Operation kind extracted from a terminal clipboard protocol request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalClipboardOperation {
    /// A pane requested a clipboard write.
    Write,
    /// A pane requested clipboard contents to be returned.
    Query,
}

/// Product effect that should be applied to clipboard content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardEffectIntent {
    /// Store content in the mux-owned internal paste buffer.
    StoreInternal,
    /// Ask the product host adapter to copy content on a best-effort basis.
    CopyToHost,
}

/// Ordered clipboard effects approved by deterministic mux policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardWritePlan {
    intents: Vec<ClipboardEffectIntent>,
}

impl ClipboardWritePlan {
    /// Returns the ordinary text-selection copy plan required by mux behavior.
    ///
    /// Selection copies always update the internal buffer before attempting a
    /// best-effort host copy.
    pub fn text_selection() -> Self {
        Self::new(true)
    }

    /// Returns ordered effect intents for execution by the product adapter.
    pub fn intents(&self) -> &[ClipboardEffectIntent] {
        self.intents.as_slice()
    }

    /// Builds one terminal write plan with optional host routing.
    fn new(copy_to_host: bool) -> Self {
        let mut intents = vec![ClipboardEffectIntent::StoreInternal];
        if copy_to_host {
            intents.push(ClipboardEffectIntent::CopyToHost);
        }
        Self { intents }
    }
}

/// Stable reason that a terminal clipboard request produced no effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardRejection {
    /// Product security policy denied the operation.
    AuthorizationDenied,
    /// Configured terminal clipboard policy disabled the operation.
    PolicyDisabled,
    /// Clipboard queries are intentionally unsupported to prevent disclosure.
    QueryUnsupported,
}

/// Deterministic result of mux clipboard policy evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardDecision {
    /// The product may execute these ordered write effects.
    Write(ClipboardWritePlan),
    /// The request must produce no clipboard effects.
    Reject(ClipboardRejection),
}

/// Plans one terminal-originated clipboard request without performing effects.
///
/// Authorization denial takes precedence over configured policy, and disabled
/// policy takes precedence over operation support. Queries never produce a
/// read effect because Mezzanine does not disclose clipboard data to panes.
pub fn plan_terminal_clipboard_request(
    policy: ClipboardPolicy,
    authorization: ClipboardAuthorization,
    operation: TerminalClipboardOperation,
) -> ClipboardDecision {
    if authorization == ClipboardAuthorization::Denied {
        return ClipboardDecision::Reject(ClipboardRejection::AuthorizationDenied);
    }
    if policy == ClipboardPolicy::Disabled {
        return ClipboardDecision::Reject(ClipboardRejection::PolicyDisabled);
    }
    match operation {
        TerminalClipboardOperation::Query => {
            ClipboardDecision::Reject(ClipboardRejection::QueryUnsupported)
        }
        TerminalClipboardOperation::Write => {
            ClipboardDecision::Write(ClipboardWritePlan::new(policy == ClipboardPolicy::External))
        }
    }
}

/// Identifies the selected source for a paste request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardPasteSourceKind {
    /// Non-empty text read by the product host clipboard adapter.
    Host,
    /// Non-empty text from the most recently updated internal paste buffer.
    PasteBuffer {
        /// Stable internal paste-buffer name.
        name: String,
    },
}

/// Clipboard paste content selected by mux fallback policy.
///
/// `Debug` intentionally reports only source metadata and payload size so
/// logs and failed assertions do not expose clipboard contents.
#[derive(Clone, PartialEq, Eq)]
pub struct ClipboardPasteSource {
    kind: ClipboardPasteSourceKind,
    content: String,
}

impl ClipboardPasteSource {
    /// Returns source metadata for lifecycle and routing decisions.
    pub fn kind(&self) -> &ClipboardPasteSourceKind {
        &self.kind
    }

    /// Returns selected text to the product input-effect adapter.
    pub fn content(&self) -> &str {
        self.content.as_str()
    }
}

impl fmt::Debug for ClipboardPasteSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClipboardPasteSource")
            .field("kind", &self.kind)
            .field("bytes", &self.content.len())
            .finish()
    }
}

/// Selects non-empty paste content using host-first mux fallback policy.
///
/// Host access and internal-buffer lookup happen before this call. The product
/// supplies their observed values, and this pure function selects host text
/// first or the most recent non-empty internal buffer second.
pub fn select_clipboard_paste_source(
    host_content: Option<String>,
    most_recent_buffer: Option<(String, String)>,
) -> Option<ClipboardPasteSource> {
    if let Some(content) = host_content.filter(|content| !content.is_empty()) {
        return Some(ClipboardPasteSource {
            kind: ClipboardPasteSourceKind::Host,
            content,
        });
    }
    let (name, content) = most_recent_buffer.filter(|(_, content)| !content.is_empty())?;
    Some(ClipboardPasteSource {
        kind: ClipboardPasteSourceKind::PasteBuffer { name },
        content,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        ClipboardAuthorization, ClipboardDecision, ClipboardEffectIntent, ClipboardPasteSourceKind,
        ClipboardPolicy, ClipboardRejection, TerminalClipboardOperation,
        plan_terminal_clipboard_request, select_clipboard_paste_source,
    };

    /// Verifies every supported clipboard configuration spelling normalizes to
    /// one canonical typed policy while unknown values remain invalid.
    ///
    /// Keeping aliases in the mux boundary prevents product configuration and
    /// runtime call sites from interpreting the same raw string differently.
    #[test]
    fn clipboard_policy_normalizes_supported_configuration_values() {
        assert_eq!(
            ClipboardPolicy::from_config_value("external"),
            Some(ClipboardPolicy::External)
        );
        assert_eq!(
            ClipboardPolicy::from_config_value("host"),
            Some(ClipboardPolicy::External)
        );
        assert_eq!(
            ClipboardPolicy::from_config_value("internal"),
            Some(ClipboardPolicy::Internal)
        );
        for value in ["disabled", "off", "none"] {
            assert_eq!(
                ClipboardPolicy::from_config_value(value),
                Some(ClipboardPolicy::Disabled)
            );
        }
        assert_eq!(ClipboardPolicy::from_config_value("unknown"), None);
        assert_eq!(ClipboardPolicy::External.as_str(), "external");
        assert_eq!(ClipboardPolicy::Internal.as_str(), "internal");
        assert_eq!(ClipboardPolicy::Disabled.as_str(), "disabled");
    }

    /// Verifies external and internal policies return distinct typed write
    /// plans with internal storage ordered before any best-effort host effect.
    ///
    /// This order preserves an internal copy even when the product host adapter
    /// is unavailable or rejects the subsequent effect.
    #[test]
    fn terminal_clipboard_write_policy_plans_internal_and_host_effects() {
        let external = plan_terminal_clipboard_request(
            ClipboardPolicy::External,
            ClipboardAuthorization::Allowed,
            TerminalClipboardOperation::Write,
        );
        let internal = plan_terminal_clipboard_request(
            ClipboardPolicy::Internal,
            ClipboardAuthorization::Allowed,
            TerminalClipboardOperation::Write,
        );

        assert_eq!(
            external,
            ClipboardDecision::Write(super::ClipboardWritePlan::text_selection())
        );
        let ClipboardDecision::Write(internal) = internal else {
            panic!("internal policy should approve a write");
        };
        assert_eq!(internal.intents(), &[ClipboardEffectIntent::StoreInternal]);
    }

    /// Verifies explicit product denial, disabled policy, and clipboard queries
    /// return stable no-effect decisions.
    ///
    /// A query must never fall through to a host read because returning host
    /// clipboard data to an untrusted pane would disclose user content.
    #[test]
    fn terminal_clipboard_policy_rejects_denied_disabled_and_query_requests() {
        assert_eq!(
            plan_terminal_clipboard_request(
                ClipboardPolicy::External,
                ClipboardAuthorization::Denied,
                TerminalClipboardOperation::Write,
            ),
            ClipboardDecision::Reject(ClipboardRejection::AuthorizationDenied)
        );
        assert_eq!(
            plan_terminal_clipboard_request(
                ClipboardPolicy::Disabled,
                ClipboardAuthorization::Allowed,
                TerminalClipboardOperation::Write,
            ),
            ClipboardDecision::Reject(ClipboardRejection::PolicyDisabled)
        );
        assert_eq!(
            plan_terminal_clipboard_request(
                ClipboardPolicy::Internal,
                ClipboardAuthorization::Allowed,
                TerminalClipboardOperation::Query,
            ),
            ClipboardDecision::Reject(ClipboardRejection::QueryUnsupported)
        );
    }

    /// Verifies paste selection prefers non-empty host text, falls back to the
    /// most recent non-empty internal buffer, and rejects empty observations.
    ///
    /// Host I/O remains outside the mux while its deterministic precedence is
    /// represented once in this lower-crate policy function.
    #[test]
    fn clipboard_paste_source_uses_host_first_non_empty_fallback_policy() {
        let host = select_clipboard_paste_source(
            Some("host text".to_string()),
            Some(("recent".to_string(), "buffer text".to_string())),
        )
        .unwrap();
        assert_eq!(host.kind(), &ClipboardPasteSourceKind::Host);
        assert_eq!(host.content(), "host text");

        let fallback = select_clipboard_paste_source(
            Some(String::new()),
            Some(("recent".to_string(), "buffer text".to_string())),
        )
        .unwrap();
        assert_eq!(
            fallback.kind(),
            &ClipboardPasteSourceKind::PasteBuffer {
                name: "recent".to_string()
            }
        );
        assert_eq!(fallback.content(), "buffer text");
        assert_eq!(select_clipboard_paste_source(None, None), None);
        assert_eq!(
            select_clipboard_paste_source(None, Some(("recent".to_string(), String::new()))),
            None
        );
    }

    /// Verifies paste-source diagnostics retain routing metadata and payload
    /// size without exposing the selected clipboard text.
    ///
    /// This guard protects lifecycle and assertion output from accidentally
    /// becoming a secondary store for sensitive clipboard content.
    #[test]
    fn clipboard_paste_source_debug_output_redacts_content() {
        let source = select_clipboard_paste_source(Some("secret-token".to_string()), None).unwrap();

        let diagnostic = format!("{source:?}");
        assert!(diagnostic.contains("bytes: 12"), "{diagnostic}");
        assert!(!diagnostic.contains("secret-token"), "{diagnostic}");
    }
}
