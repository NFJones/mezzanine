//! Direct structured shell-read observation parsing tests.

use super::*;

#[test]
/// Verifies structured shell-read extraction scopes targets to each shell
/// segment instead of stealing the last file-looking token from a later
/// unrelated command.
fn shell_read_observations_scope_targets_per_shell_segment() {
    let observations = shell_read_observations_for_command(
        "sed -n '300,420p' src/runtime/render/overlay.rs && cat README.md",
    );

    assert_eq!(observations.len(), 2, "{observations:?}");
    assert_eq!(observations[0].kind, ShellReadObservationKind::Read);
    assert_eq!(observations[0].target, "src/runtime/render/overlay.rs");
    assert_eq!(observations[0].ranges.len(), 1);
    assert_eq!(observations[0].ranges[0].start_line, 300);
    assert_eq!(observations[0].ranges[0].end_line, 420);
    assert_eq!(observations[1].kind, ShellReadObservationKind::Read);
    assert_eq!(observations[1].target, "README.md");
    assert!(observations[1].ranges.is_empty());
}
