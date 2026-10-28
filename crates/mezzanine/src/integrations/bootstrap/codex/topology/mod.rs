//! Negative classification of documented shared Codex command modes.
//!
//! Callers supply bounded native observations, never callback-provided argv.
//! This pure predicate can refuse a known shared topology but cannot authorize
//! an executable, source, client or request. Private input must not be logged,
//! transported or persisted. Aliases and exact vendor versions are irrelevant.

/// Refuses documented app-server invocations independently of path or aliases.
/// Even stdio can resolve persisted remote-control state or enable remote access
/// later. Transport arguments cannot prove a requesting client; this negative
/// predicate grants no authority to other invocations.
pub(crate) fn shared_server(arguments: &[Vec<u8>]) -> bool {
    arguments.iter().any(|argument| argument == b"app-server")
}
