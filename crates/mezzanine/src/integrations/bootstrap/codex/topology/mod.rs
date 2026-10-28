//! Negative classification of documented shared Codex command modes.
//!
//! Callers supply bounded native observations, never callback-provided argv.
//! This pure predicate can refuse a known shared topology but cannot authorize
//! an executable, source, client or request. Private input must not be logged,
//! transported or persisted. Aliases and exact vendor versions are irrelevant.

/// Classifies only the documented server mode, independently of executable path,
/// version and aliases. Any non-stdio listener is potentially multi-client.
pub(crate) fn shared_server(arguments: &[Vec<u8>]) -> bool {
    if !arguments.iter().any(|argument| argument == b"app-server") {
        return false;
    }
    arguments.iter().enumerate().any(|(index, argument)| {
        if argument == b"--managed-daemon" {
            return true;
        }
        let listener = if argument == b"--listen" {
            Some(arguments.get(index + 1).map_or(&b""[..], Vec::as_slice))
        } else {
            argument.strip_prefix(b"--listen=")
        };
        listener.is_some_and(|value| value != b"stdio://" && value != b"stdio")
    })
}
