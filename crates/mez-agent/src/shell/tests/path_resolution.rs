//! Regression coverage for pane-shell canonical path resolution.

use super::*;
use crate::permissions::ResolvedPathKind;
use base64::Engine;
use std::os::unix::fs::symlink;
use std::process::Command;

/// Verifies the encoded POSIX resolver observes canonical existing targets and
/// preserves nearest-existing-parent evidence for a non-existent create target
/// reached through a symlink. This proves paths are interpreted by the pane
/// process rather than interpolated into generated shell source.
#[test]
fn pane_path_resolution_observes_symlinks_and_create_targets() {
    let root = test_temp_dir("path-resolution");
    let project = root.join("project");
    let outside = root.join("outside");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), b"secret").unwrap();
    symlink(&outside, project.join("link")).unwrap();

    let request = PanePathResolutionRequest::new(
        vec![".".to_string()],
        vec!["link/new/nested.txt".to_string()],
        vec!["link/secret.txt".to_string()],
    )
    .unwrap();
    let command = pane_path_resolution_command(&request, ShellClassification::PosixSh).unwrap();
    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .current_dir(&project)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let result = parse_pane_path_resolution_output(&stdout, &request).unwrap();
    let scopes = result.into_path_scopes(&request).unwrap();

    assert_eq!(scopes.current_directory, project.to_string_lossy());
    assert_eq!(
        scopes.read_scopes,
        vec![
            outside.join("new/nested.txt").to_string_lossy(),
            project.to_string_lossy(),
        ]
    );
    assert_eq!(
        scopes.write_scopes,
        vec![outside.join("new/nested.txt").to_string_lossy()]
    );
    let create = scopes.path_evidence.get("link/new/nested.txt").unwrap();
    assert_eq!(create.kind, ResolvedPathKind::CreateTarget);
    assert_eq!(create.nearest_existing_parent, outside.to_string_lossy());
    let existing = scopes.path_evidence.get("link/secret.txt").unwrap();
    assert_eq!(existing.kind, ResolvedPathKind::Existing);
    assert_eq!(
        existing.canonical_path,
        outside.join("secret.txt").to_string_lossy()
    );

    std::fs::remove_dir_all(root).unwrap();
}

/// Verifies malformed or incomplete resolver output fails closed instead of
/// creating partially trusted authority from a subset of requested paths.
#[test]
fn pane_path_resolution_rejects_incomplete_protocol_output() {
    let request =
        PanePathResolutionRequest::new(vec![".".to_string()], Vec::new(), vec!["src".to_string()])
            .unwrap();
    let payload = base64::engine::general_purpose::STANDARD
        .encode(br#"{"version":1,"current_directory":"/repo","entries":[]}"#);
    let error = parse_pane_path_resolution_output(
        &format!("MEZ_PATH_RESOLUTION_V1\t{payload}\n"),
        &request,
    )
    .unwrap_err();

    assert_eq!(error.kind(), AgentShellValidationErrorKind::InvalidArgs);
}
