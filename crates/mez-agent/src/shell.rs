//! Provider-independent shell-source helpers used by agent action planning.
//!
//! This module owns deterministic shell text construction that does not read
//! product configuration, inspect the filesystem, or execute a process.

/// Quotes one value as a POSIX shell word.
///
/// The returned text is safe to embed as one literal shell argument. Empty
/// values remain explicit empty arguments, and embedded single quotes use the
/// standard close-double-quote-reopen sequence.
pub fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    let escaped = value.replace('\'', "'\"'\"'");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use super::shell_quote;

    /// Verifies shell quoting preserves empty values and embedded single
    /// quotes as one literal POSIX shell argument.
    #[test]
    fn shell_quote_preserves_literal_arguments() {
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("plain value"), "'plain value'");
        assert_eq!(shell_quote("a'b"), "'a'\"'\"'b'");
    }
}
