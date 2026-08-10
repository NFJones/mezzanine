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
    let project = std::fs::canonicalize(project).unwrap();
    let outside = std::fs::canonicalize(outside).unwrap();

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

/// Verifies structurally incomplete resolver output still fails closed rather
/// than treating an unreported request as an unavailable mapping.
#[test]
fn pane_path_resolution_rejects_incomplete_protocol_output() {
    let request =
        PanePathResolutionRequest::new(vec![".".to_string()], Vec::new(), vec!["src".to_string()])
            .unwrap();
    let payload = base64::engine::general_purpose::STANDARD
        .encode(br#"{"version":2,"current_directory":"/repo","entries":[]}"#);
    let error = parse_pane_path_resolution_output(
        &format!("MEZ_PATH_RESOLUTION_V2\t{payload}\n"),
        &request,
    )
    .unwrap_err();

    assert_eq!(error.kind(), AgentShellValidationErrorKind::InvalidArgs);
}

/// Verifies one unavailable read mapping is omitted while an independently
/// validated existing read and create-target write mapping remain effective.
#[test]
fn pane_path_resolution_returns_restrictive_partial_authority() {
    let root = test_temp_dir("path-resolution-partial");
    let existing = root.join("existing");
    std::fs::create_dir_all(&existing).unwrap();
    let canonical_existing = std::fs::canonicalize(&existing).unwrap();
    let request = PanePathResolutionRequest::new(
        vec![
            existing.to_string_lossy().into_owned(),
            root.join("missing-read").to_string_lossy().into_owned(),
        ],
        vec![
            root.join("created/write.txt")
                .to_string_lossy()
                .into_owned(),
        ],
        Vec::new(),
    )
    .unwrap();
    let command = pane_path_resolution_command(&request, ShellClassification::PosixSh).unwrap();
    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let parsed =
        parse_pane_path_resolution_output(&String::from_utf8(output.stdout).unwrap(), &request)
            .unwrap();
    let outcome = parsed.into_outcome(&request).unwrap();

    assert_eq!(outcome.scopes.write_scopes.len(), 1);
    assert!(
        outcome
            .scopes
            .read_scopes
            .contains(&canonical_existing.to_string_lossy().into_owned())
    );
    assert!(
        outcome
            .unavailable_paths
            .contains_key(&root.join("missing-read").to_string_lossy().into_owned())
    );
    std::fs::remove_dir_all(root).unwrap();
}

/// Verifies POSIX path resolution selects only reviewed absolute Python
/// interpreters, so pane-local aliases, functions, virtualenvs, and mutable
/// `PATH` entries cannot execute before Bubblewrap confinement.
#[test]
fn posix_path_resolution_uses_only_absolute_python_candidates() {
    let request =
        PanePathResolutionRequest::new(vec![".".to_string()], Vec::new(), Vec::new()).unwrap();
    let command = pane_path_resolution_command(&request, ShellClassification::PosixSh).unwrap();

    assert!(!command.contains("command -v"), "{command}");
    assert!(!command.contains("python3 -c"), "{command}");
    assert!(!command.contains("python -c"), "{command}");
    assert!(command.contains("/usr/bin/python3"), "{command}");
    assert!(command.contains("/bin/python3"), "{command}");
    assert!(command.contains(" -I -S -c "), "{command}");
}

/// Verifies executing the POSIX resolver with a hostile leading `PATH` entry
/// never invokes that entry, while the approved absolute resolver still
/// produces valid pane-local path evidence.
#[test]
fn posix_path_resolution_does_not_execute_mutable_path_python() {
    use std::os::unix::fs::PermissionsExt;

    let root = test_temp_dir("path-resolution-hostile-path");
    let fake_bin = root.join("bin");
    let sentinel = root.join("fake-python-ran");
    std::fs::create_dir_all(&fake_bin).unwrap();
    let fake_python = fake_bin.join("python3");
    std::fs::write(
        &fake_python,
        format!("#!/bin/sh\nprintf ran > '{}'\nexit 0\n", sentinel.display()),
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&fake_python).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&fake_python, permissions).unwrap();

    let request =
        PanePathResolutionRequest::new(vec![".".to_string()], Vec::new(), Vec::new()).unwrap();
    let command = pane_path_resolution_command(&request, ShellClassification::PosixSh).unwrap();
    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .env("PATH", &fake_bin)
        .current_dir(&root)
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    assert!(!sentinel.exists(), "mutable-PATH python executed");
    let stdout = String::from_utf8(output.stdout).unwrap();
    parse_pane_path_resolution_output(&stdout, &request).unwrap();

    std::fs::remove_dir_all(root).unwrap();
}

/// Verifies Fish path resolution uses the same reviewed absolute interpreter
/// candidates without consulting Fish's command lookup or inherited `PATH`.
#[test]
fn fish_path_resolution_uses_only_absolute_python_candidates() {
    let request =
        PanePathResolutionRequest::new(vec![".".to_string()], Vec::new(), Vec::new()).unwrap();
    let command = pane_path_resolution_command(&request, ShellClassification::Fish).unwrap();

    assert!(!command.contains("command -s"), "{command}");
    assert!(!command.contains("python3 -c"), "{command}");
    assert!(!command.contains("python -c"), "{command}");
    assert!(command.contains("/usr/bin/python3"), "{command}");
    assert!(command.contains("/bin/python3"), "{command}");
    assert!(command.contains(" -I -S -c "), "{command}");
}

/// Verifies the generated Fish resolver executes under real Fish and emits a
/// protocol record that can be parsed into pane-local path evidence.
#[test]
fn fish_path_resolution_executes_and_emits_protocol_output() {
    if Command::new("fish").arg("--version").output().is_err() {
        eprintln!("skipping real-Fish path-resolution assertion because fish is unavailable");
        return;
    }
    let request =
        PanePathResolutionRequest::new(vec![".".to_string()], Vec::new(), Vec::new()).unwrap();
    let command = pane_path_resolution_command(&request, ShellClassification::Fish).unwrap();
    let output = Command::new("fish")
        .args(["--no-config", "-c", &command])
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    parse_pane_path_resolution_output(&stdout, &request).unwrap();
}
