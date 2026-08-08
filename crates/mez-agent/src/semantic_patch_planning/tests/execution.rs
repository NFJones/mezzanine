//! Semantic-patch shell transaction execution tests.
//!
//! This leaf verifies generated commands against a real POSIX shell while the
//! product pane executor remains outside the lower crate.

use super::*;

#[test]
/// Verifies generated semantic file-mutation commands emit an actual diff on
/// success.
///
/// The runtime uses this cleaned stdout for normal-mode pane logging, so the
/// lowering itself must produce copyable diff content rather than relying on the
/// model to describe the file change after the action completes.
fn semantic_apply_patch_command_emits_success_diff() {
    let temp = test_temp_dir("semantic-patch-diff");
    let patch = add_file_patch("note.txt", "one\ntwo\n");
    let output = run_apply_patch_action(&temp, &patch);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("diff -- apply patch"), "{stdout}");
    assert!(stdout.contains("+one"), "{stdout}");
    assert!(stdout.contains("+two"), "{stdout}");
    std::fs::remove_dir_all(temp).unwrap();
}

#[test]
/// Verifies generated file-content commands do not inject raw multiline model
/// content into the shell source.
///
/// Large patch actions can contain quotes, command substitutions, and
/// hundreds of lines of source text. Embedding that payload directly in the
/// pane shell input risks leaving the shell waiting for more quoted input and
/// prevents Mezzanine from observing the transaction marker. The lowering
/// should encode payload bytes and decode them inside the transaction instead.
fn semantic_apply_patch_command_encodes_shell_sensitive_content() {
    let temp = test_temp_dir("semantic-patch-encoded");
    let target = temp.join("quoted.txt");
    let content = format!(
        "first line\nrepository's quoted text\n$(not-a-command)\n{}\nlast line\n",
        "middle\n".repeat(64)
    );
    let patch = add_file_patch("quoted.txt", &content);
    let action = AgentAction {
        id: "patch-quoted".to_string(),
        rationale: String::new(),
        payload: AgentActionPayload::ApplyPatch {
            patch: patch.clone(),
            strip: None,
        },
    };
    let plan = local_action_plan(&action).unwrap().unwrap();

    assert!(plan.command.contains("base64"), "{}", plan.command);
    assert!(!plan.command.contains("repository's quoted text"));
    assert!(!plan.command.contains("$(not-a-command)"));
    let output = run_apply_patch_action(&temp, &patch);
    assert!(output.status.success(), "command failed: {}", plan.command);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), content);
    std::fs::remove_dir_all(temp).unwrap();
}

#[test]
/// Verifies generated file-content shell source keeps each physical line below
/// PTY canonical-line limits.
///
/// File mutations are delivered as pane shell input. A single oversized base64
/// line can fill the PTY input line discipline before the newline arrives,
/// preventing the transaction wrapper from reaching its end marker.
fn semantic_apply_patch_command_keeps_encoded_lines_short() {
    let temp = test_temp_dir("semantic-patch-short-lines");
    let patch = add_file_patch("large.txt", &"0123456789abcdef\n".repeat(2048));
    let action = AgentAction {
        id: "patch-large".to_string(),
        rationale: String::new(),
        payload: AgentActionPayload::ApplyPatch { patch, strip: None },
    };
    let plan = local_action_plan(&action).unwrap().unwrap();
    let longest_line = plan.command.lines().map(str::len).max().unwrap_or(0);

    assert!(
        longest_line < 1024,
        "generated shell line should stay PTY-safe; longest={longest_line}"
    );
    assert!(plan.command.contains("base64"), "{}", plan.command);
    std::fs::remove_dir_all(temp).unwrap();
}

#[test]
/// Verifies explicit empty `apply_patch` file content creates a
/// zero-byte regular file.
///
/// Empty file content is distinct from an omitted action payload. The semantic
/// planner must still lower it into a complete shell transaction that writes
/// the empty payload and emits bounded success output.
fn semantic_apply_patch_command_writes_zero_byte_content() {
    let temp = test_temp_dir("semantic-patch-empty");
    let target = temp.join("empty-created.txt");
    let patch = add_file_patch("empty-created.txt", "");
    let output = run_apply_patch_action(&temp, &patch);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success(), "stdout={stdout}\nstderr={stderr}");
    assert_eq!(std::fs::metadata(target).unwrap().len(), 0);
    assert!(stdout.contains("diff -- apply patch"), "{stdout}");

    std::fs::remove_dir_all(temp).unwrap();
}

#[test]
/// Verifies a late precondition failure preserves earlier mutations and emits
/// one machine-readable outcome for every attempted target.
///
/// Multi-file writes are intentionally serial rather than transactional. If a
/// later target changes after the read phase, the command must retain the
/// earlier confirmed diff, attribute the failed target, and exit nonzero only
/// after recording both outcomes.
fn semantic_apply_patch_command_reports_partial_late_failure_per_file() {
    let temp = test_temp_dir("semantic-patch-partial-late-failure");
    std::fs::write(temp.join("one.txt"), "old one\n").unwrap();
    std::fs::write(temp.join("two.txt"), "old two\n").unwrap();
    let patch = "*** Begin Patch\n*** Update File: one.txt\n@@\n-old one\n+new one\n*** Update File: two.txt\n@@\n-old two\n+new two\n*** End Patch";
    let read_plan = apply_patch_plan(patch, None).unwrap();
    let read_output = Command::new("/bin/sh")
        .arg("-c")
        .arg(&read_plan.command)
        .current_dir(&temp)
        .output()
        .unwrap();
    assert!(read_output.status.success());
    let write_plan = apply_patch_write_plan_from_read_output(
        patch,
        &String::from_utf8_lossy(&read_output.stdout),
    )
    .unwrap();
    std::fs::write(temp.join("two.txt"), "changed concurrently\n").unwrap();

    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg(&write_plan.command)
        .current_dir(&temp)
        .output()
        .unwrap();
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let outcomes = parse_apply_patch_file_outcomes(&combined).unwrap();

    assert!(!output.status.success(), "{combined}");
    assert_eq!(
        std::fs::read_to_string(temp.join("one.txt")).unwrap(),
        "new one\n"
    );
    assert_eq!(
        std::fs::read_to_string(temp.join("two.txt")).unwrap(),
        "changed concurrently\n"
    );
    assert!(combined.contains("diff -- apply patch"), "{combined}");
    assert_eq!(
        outcomes,
        vec![
            ApplyPatchFileOutcome::Applied {
                path: "one.txt".to_string(),
            },
            ApplyPatchFileOutcome::Failed {
                path: "two.txt".to_string(),
                diagnostic: "apply_patch: file changed before apply: two.txt\n".to_string(),
            },
        ]
    );

    std::fs::remove_dir_all(temp).unwrap();
}

#[test]
/// Verifies the native pane resolver matches physical missing-path semantics.
///
/// A missing component followed by `..` can expose a later symlink that still
/// needs physical resolution. Resolving only the nearest existing prefix would
/// miss that link and could authorize the wrong boundary. The test suppresses
/// GNU `realpath -m` so every Unix host exercises the macOS-compatible native
/// component walker.
#[cfg(unix)]
fn semantic_apply_patch_native_resolver_handles_symlink_after_missing_parent() {
    let root = test_temp_dir("semantic-patch-native-path-resolution");
    let cwd = root.join("cwd");
    let outside = root.join("outside");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink("../outside", cwd.join("link")).unwrap();
    let path = path_with_failing_realpath(&root);
    let mut lines = vec!["MEZ_APPLY_CWD=$(pwd -P) || exit 1".to_string()];
    lines.extend(apply_patch_path_resolution_lines());
    lines.push(format!(
        "mez_apply_patch_resolve {}",
        crate::shell_quote("missing/../link/new.txt")
    ));

    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg(lines.join("\n"))
        .current_dir(&cwd)
        .env("PATH", path)
        .output()
        .unwrap();
    let actual = String::from_utf8(output.stdout).unwrap();
    let expected = std::fs::canonicalize(&outside).unwrap().join("new.txt");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(actual.trim_end(), expected.to_string_lossy());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
/// Verifies the native pane resolver fails closed on symbolic-link cycles.
///
/// The macOS-compatible fallback reads links one component at a time and must
/// impose its own traversal bound. A cyclic target should therefore return a
/// failure instead of hanging the pane transaction or emitting an apparently
/// canonical path.
#[cfg(unix)]
fn semantic_apply_patch_native_resolver_rejects_symlink_loops() {
    let root = test_temp_dir("semantic-patch-native-path-loop");
    std::os::unix::fs::symlink("second", root.join("first")).unwrap();
    std::os::unix::fs::symlink("first", root.join("second")).unwrap();
    let path = path_with_failing_realpath(&root);
    let mut lines = vec!["MEZ_APPLY_CWD=$(pwd -P) || exit 1".to_string()];
    lines.extend(apply_patch_path_resolution_lines());
    lines.push("mez_apply_patch_resolve first/file.txt".to_string());

    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg(lines.join("\n"))
        .current_dir(&root)
        .env("PATH", path)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
/// Verifies a target retargeted through a symlink after snapshotting cannot
/// reuse failed path-resolution state to mutate a file outside the boundary.
#[cfg(unix)]
fn semantic_apply_patch_rejects_symlink_retarget_before_write() {
    let root = test_temp_dir("semantic-patch-symlink-retarget");
    let cwd = root.join("cwd");
    let outside = root.join("outside.txt");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(cwd.join("note.txt"), "old\n").unwrap();
    std::fs::write(&outside, "old\n").unwrap();
    let patch = "*** Begin Patch\n*** Update File: note.txt\n@@\n-old\n+new\n*** End Patch";
    let read_plan = apply_patch_plan(patch, None).unwrap();
    let read_output = Command::new("/bin/sh")
        .arg("-c")
        .arg(&read_plan.command)
        .current_dir(&cwd)
        .output()
        .unwrap();
    assert!(read_output.status.success());
    let write_plan = apply_patch_write_plan_from_read_output(
        patch,
        &String::from_utf8_lossy(&read_output.stdout),
    )
    .unwrap();
    std::fs::remove_file(cwd.join("note.txt")).unwrap();
    std::os::unix::fs::symlink("../outside.txt", cwd.join("note.txt")).unwrap();

    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg(&write_plan.command)
        .current_dir(&cwd)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("resolved path"), "{stderr}");
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "old\n");
    assert!(
        std::fs::symlink_metadata(cwd.join("note.txt"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
/// Verifies write transactions resolve the target again immediately before
/// mutation and reject an identity change after their initial preconditions.
#[cfg(unix)]
fn semantic_apply_patch_revalidates_target_immediately_before_write() {
    use std::os::unix::fs::PermissionsExt;

    let root = test_temp_dir("semantic-patch-final-revalidation");
    let cwd = root.join("cwd");
    let outside = root.join("outside.txt");
    let fake_bin = root.join("bin");
    let retargeted_path = root.join("retargeted");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&fake_bin).unwrap();
    std::fs::write(cwd.join("note.txt"), "old\n").unwrap();
    std::fs::write(&outside, "old\n").unwrap();
    let patch = "*** Begin Patch\n*** Update File: note.txt\n@@\n-old\n+new\n*** End Patch";
    let read_plan = apply_patch_plan(patch, None).unwrap();
    let read_output = Command::new("/bin/sh")
        .arg("-c")
        .arg(&read_plan.command)
        .current_dir(&cwd)
        .output()
        .unwrap();
    assert!(read_output.status.success());
    let write_plan = apply_patch_write_plan_from_read_output(
        patch,
        &String::from_utf8_lossy(&read_output.stdout),
    )
    .unwrap();
    let cmp_output = Command::new("/bin/sh")
        .arg("-c")
        .arg("command -v cmp")
        .output()
        .unwrap();
    assert!(cmp_output.status.success());
    let cmp = String::from_utf8(cmp_output.stdout)
        .unwrap()
        .trim()
        .to_string();
    let fake_cmp = fake_bin.join("cmp");
    std::fs::write(
        &fake_cmp,
        format!(
            "#!/bin/sh\n{cmp} \"$@\"\nstatus=$?\nif [ \"$status\" = 0 ]; then\n  rm -f -- note.txt\n  ln -s ../outside.txt note.txt\n  printf retargeted > {retargeted}\nfi\nexit \"$status\"\n",
            retargeted = retargeted_path.display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake_cmp, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg(&write_plan.command)
        .current_dir(&cwd)
        .env("PATH", path)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("resolved path is outside current working directory"),
        "{stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(&retargeted_path).unwrap(),
        "retargeted"
    );
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "old\n");
    assert!(
        std::fs::symlink_metadata(cwd.join("note.txt"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    std::fs::remove_dir_all(root).unwrap();
}
