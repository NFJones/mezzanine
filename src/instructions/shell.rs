//! Shell escaping helpers for instruction discovery.
//!
//! Discovery commands execute inside the pane shell, so command planning keeps
//! its escaping rules small and POSIX-compatible.

/// Runs the shell quote operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn shell_quote(value: &str) -> String {
    let escaped = value.replace('\'', "'\"'\"'");
    format!("'{escaped}'")
}
