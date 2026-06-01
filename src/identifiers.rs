//! Shared identifier validation helpers.
//!
//! This module owns low-level identifier predicates that are reused by command,
//! configuration, and permission-facing code. Callers keep their own diagnostic
//! text so user-facing errors remain specific to the subsystem that rejected
//! the input.

/// Returns whether `value` is a non-empty ASCII identifier segment.
///
/// The accepted grammar is `[A-Za-z0-9_-]+`. Dotted paths should split their
/// path first and validate each segment independently so empty segments remain
/// distinguishable from invalid characters.
pub(crate) fn is_ascii_identifier_segment(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::is_ascii_identifier_segment;

    /// Verifies the shared identifier segment grammar used by command and
    /// configuration paths.
    ///
    /// The helper deliberately validates one dotted-path segment at a time:
    /// empty strings, dots, spaces, and non-ASCII bytes are rejected while the
    /// command/config-safe ASCII punctuation remains accepted.
    #[test]
    fn ascii_identifier_segment_accepts_only_non_empty_safe_ascii() {
        assert!(is_ascii_identifier_segment("provider_1"));
        assert!(is_ascii_identifier_segment("model-profile"));

        assert!(!is_ascii_identifier_segment(""));
        assert!(!is_ascii_identifier_segment("model.profile"));
        assert!(!is_ascii_identifier_segment("model profile"));
        assert!(!is_ascii_identifier_segment("modèle"));
    }
}
