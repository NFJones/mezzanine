//! Semantic Patch tests for planning behavior.
//!
//! This bounded leaf owns the named behavioral scenarios.

use super::*;
use std::fs::File;
use std::process::Stdio;
use std::time::{Duration, Instant};

#[test]
/// Verifies semantic patch lowering accepts related multi-file patch batches.
///
/// Mezzanine patch blocks can contain more than one file operation. Mezzanine
/// still recommends separate actions for independent edits, but accepting
/// related multi-file blocks avoids correctable validation failures when models
/// emit the broader Codex grammar.
fn semantic_apply_patch_plan_accepts_multi_file_payloads() {
    let temp = test_temp_dir("semantic-codex-patch-multi-file");
    let patch =
        "*** Begin Patch\n*** Add File: one.txt\n+one\n*** Add File: two.txt\n+two\n*** End Patch";

    let output = run_apply_patch_action(&temp, patch);

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(temp.join("one.txt")).unwrap(),
        "one\n"
    );
    assert_eq!(
        std::fs::read_to_string(temp.join("two.txt")).unwrap(),
        "two\n"
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies semantic patch lowering auto-converts raw unified diffs.
///
/// `apply_patch` accepts both Mezzanine `*** Begin Patch` blocks and raw
/// unified diffs (with `---`/`+++`/`@@` markers). Raw unified diffs are
/// auto-converted to Mezzanine format before planning so that models which
/// naturally emit unified diff output can still produce valid patches.
fn semantic_apply_patch_plan_accepts_unified_diff_payloads() {
    let action = AgentAction {
        id: "patch-unified".to_string(),
        rationale: String::new(),
        payload: AgentActionPayload::ApplyPatch {
            patch: "diff --git a/note.txt b/note.txt\n--- a/note.txt\n+++ b/note.txt\n@@ -1,2 +1,2 @@\n-old\n+new\n context\n"
                .to_string(),
            strip: None,
        },
    };

    let plan = local_action_plan(&action).unwrap().unwrap();

    assert_eq!(plan.summary, "I\u{2019}ll apply a patch.");
    assert_eq!(plan.policy_command, "apply_patch");
    assert!(!plan.interactive);
}

#[test]
/// Verifies semantic patch lowering supports Mezzanine patch
/// blocks through a shell-backed applicator.
///
/// This protects the provider-facing `*** Begin Patch` syntax, which should be
/// applied without heredocs and with the dedicated short patch timeout.
fn semantic_apply_patch_plan_applies_codex_style_blocks() {
    let temp = test_temp_dir("semantic-codex-patch");
    std::fs::write(temp.join("note.txt"), "old\ncontext\n").unwrap();
    let patch =
        "*** Begin Patch\n*** Update File: note.txt\n@@\n-old\n+new\n context\n*** End Patch";
    let action = AgentAction {
        id: "patch-1".to_string(),
        rationale: String::new(),
        payload: AgentActionPayload::ApplyPatch {
            patch: patch.to_string(),
            strip: None,
        },
    };

    let read_plan = local_action_plan(&action).unwrap().unwrap();
    assert_eq!(read_plan.timeout_ms, Some(APPLY_PATCH_TIMEOUT_MS));
    assert!(
        !read_plan.command.contains("<<"),
        "generated Mezzanine patch command should not use heredocs:\n{}",
        read_plan.command
    );
    assert!(
        !read_plan.command.contains("python"),
        "apply_patch read phase must not require remote Python:\n{}",
        read_plan.command
    );
    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg(&read_plan.command)
        .current_dir(&temp)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "command failed: {}\nstdout:\n{}\nstderr:\n{}",
        read_plan.command,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let write_plan =
        apply_patch_write_plan_from_read_output(patch, &String::from_utf8_lossy(&output.stdout))
            .unwrap();
    assert!(
        !write_plan.command.contains("python"),
        "apply_patch write phase must not require remote Python:\n{}",
        write_plan.command
    );
    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg(&write_plan.command)
        .current_dir(&temp)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "command failed: {}\nstdout:\n{}\nstderr:\n{}",
        write_plan.command,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("diff -- apply patch"), "{stdout}");
    assert!(stdout.contains("--- a/note.txt"), "{stdout}");
    assert!(stdout.contains("+++ b/note.txt"), "{stdout}");
    assert!(stdout.contains("-old"), "{stdout}");
    assert!(stdout.contains("+new"), "{stdout}");
    assert_eq!(
        std::fs::read_to_string(temp.join("note.txt")).unwrap(),
        "new\ncontext\n"
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies Mezzanine patch application rejects non-regular
/// filesystem targets before attempting blocking reads or writes.
///
/// A FIFO target used to block inside Python `read_text`, which made an
/// `apply_patch` action look like an indefinitely stalled turn until the
/// runtime timeout fired. The semantic patch applicator should fail quickly
/// with a model-repairable diagnostic instead.
fn semantic_apply_patch_plan_rejects_fifo_targets_without_blocking() {
    let temp = test_temp_dir("semantic-codex-patch-fifo");
    let target = temp.join("note.txt");
    let mkfifo = Command::new("mkfifo").arg(&target).status().unwrap();
    assert!(
        mkfifo.success(),
        "mkfifo should be available on the Unix test host"
    );
    let action = AgentAction {
        id: "patch-fifo".to_string(),
        rationale: String::new(),
        payload: AgentActionPayload::ApplyPatch {
            patch: "*** Begin Patch\n*** Update File: note.txt\n@@\n-old\n+new\n*** End Patch"
                .to_string(),
            strip: None,
        },
    };

    let read_plan = local_action_plan(&action).unwrap().unwrap();
    let stdout_path = temp.join("stdout.log");
    let stdout = File::create(&stdout_path).unwrap();
    let stderr = File::create(temp.join("stderr.log")).unwrap();
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(&read_plan.command)
        .current_dir(&temp)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(2);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("apply_patch command blocked on a FIFO target");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(
        status.success(),
        "snapshotting FIFO metadata should not block"
    );
    let read_output = std::fs::read_to_string(stdout_path).unwrap();
    let error = apply_patch_write_plan_from_read_output(
        "*** Begin Patch\n*** Update File: note.txt\n@@\n-old\n+new\n*** End Patch",
        &read_output,
    )
    .unwrap_err();
    assert!(
        error
            .message()
            .contains("apply_patch: refusing to patch non-regular file: note.txt"),
        "{}",
        error.message()
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies unanchored repeated exact hunk context is rejected as ambiguous.
///
/// The patcher should fail model-correctably instead of silently changing the
/// first matching block when the old-context lines identify more than one
/// current-file location.
fn semantic_apply_patch_rejects_ambiguous_unanchored_hunk() {
    let temp = test_temp_dir("semantic-codex-patch-ambiguous");
    std::fs::write(
        temp.join("note.rs"),
        "fn first() {\n    println!(\"old\");\n}\n\nfn second() {\n    println!(\"old\");\n}\n",
    )
    .unwrap();
    let patch = "*** Begin Patch\n*** Update File: note.rs\n@@\n-    println!(\"old\");\n+    println!(\"new\");\n*** End Patch";
    let action = AgentAction {
        id: "patch-ambiguous".to_string(),
        rationale: String::new(),
        payload: AgentActionPayload::ApplyPatch {
            patch: patch.to_string(),
            strip: None,
        },
    };

    let read_plan = local_action_plan(&action).unwrap().unwrap();
    let read_output = Command::new("/bin/sh")
        .arg("-c")
        .arg(&read_plan.command)
        .current_dir(&temp)
        .output()
        .unwrap();
    assert!(
        read_output.status.success(),
        "read phase failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&read_output.stdout),
        String::from_utf8_lossy(&read_output.stderr)
    );
    let error = apply_patch_write_plan_from_read_output(
        patch,
        &String::from_utf8_lossy(&read_output.stdout),
    )
    .unwrap_err();

    assert!(
        error
            .message()
            .contains("exact hunk context is ambiguous in the current file"),
        "{}",
        error.message()
    );
    assert!(
        error.message().contains("candidate match line(s): 2, 6"),
        "{}",
        error.message()
    );
    assert!(
        error
            .message()
            .contains("using a distinctive @@ header anchor"),
        "{}",
        error.message()
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies empty Mezzanine patch blocks are rejected before execution.
///
/// An `apply_patch` action with begin/end markers but no file operation should
/// not proceed into read/write planning because it is indistinguishable from an
/// accidental no-op. Rejecting it at payload validation keeps recovery focused
/// on producing a real `Add`, `Update`, or `Delete` operation.
fn semantic_apply_patch_rejects_empty_patch_blocks() {
    let action = AgentAction {
        id: "patch-empty".to_string(),
        rationale: String::new(),
        payload: AgentActionPayload::ApplyPatch {
            patch: "*** Begin Patch\n*** End Patch\n".to_string(),
            strip: None,
        },
    };

    let error = local_action_plan(&action).unwrap_err();

    assert!(
        error.message().contains("at least one file operation"),
        "{}",
        error.message()
    );
}

#[test]
/// Verifies whole-file replacement hunks reject mixed old and new context.
///
/// The convention should not silently behave like an ordinary hunk with a large
/// stale old side. Requiring add-only bodies keeps the model-facing recovery
/// path deterministic and easy to repair.
fn semantic_apply_patch_replace_whole_file_hunk_rejects_old_context() {
    let temp = test_temp_dir("semantic-codex-patch-replace-whole-file-old");
    std::fs::write(temp.join("note.txt"), "old\n").unwrap();
    let patch = "*** Begin Patch\n*** Update File: note.txt\n@@ replace whole file\n-old\n+new\n*** End Patch";

    let error = apply_patch_write_error(&temp, patch);

    assert!(
        error.contains("whole-file replacement hunk for note.txt must contain only added lines"),
        "{error}"
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies `@@ replace whole file` updates replace complete file contents.
///
/// This gives models a safer explicit convention for generated or heavily
/// shifted files without adding a separate `Replace File` directive. The hunk
/// body is still parsed as Mezzanine patch text and only `+` lines become the
/// final file content.
fn semantic_apply_patch_replace_whole_file_hunk_replaces_complete_file() {
    let temp = test_temp_dir("semantic-codex-patch-replace-whole-file");
    std::fs::write(temp.join("note.txt"), "old\nbody\n").unwrap();
    let patch = "*** Begin Patch\n*** Update File: note.txt\n@@ replace whole file\n+new\n+body\n*** End Patch";

    let output = run_apply_patch_action(&temp, patch);

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(temp.join("note.txt")).unwrap(),
        "new\nbody\n"
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies symlink targets are resolved before `apply_patch` decides whether a
/// path can be patched.
///
/// The pane shell may run on a remote system, so the read phase resolves the
/// path remotely and Rust applies the patch against the resolved regular file
/// bytes. A symlink to a regular file inside the pane working directory should
/// patch the target without replacing the symlink itself.
#[cfg(unix)]
fn semantic_apply_patch_resolves_symlink_targets_before_writing() {
    let temp = test_temp_dir("semantic-codex-patch-symlink");
    std::fs::write(temp.join("real.txt"), "old\n").unwrap();
    std::os::unix::fs::symlink("real.txt", temp.join("link.txt")).unwrap();
    let patch = "*** Begin Patch\n*** Update File: link.txt\n@@\n-old\n+new\n*** End Patch";
    let action = AgentAction {
        id: "patch-symlink".to_string(),
        rationale: String::new(),
        payload: AgentActionPayload::ApplyPatch {
            patch: patch.to_string(),
            strip: None,
        },
    };

    let read_plan = local_action_plan(&action).unwrap().unwrap();
    let read_output = Command::new("/bin/sh")
        .arg("-c")
        .arg(&read_plan.command)
        .current_dir(&temp)
        .output()
        .unwrap();
    assert!(
        read_output.status.success(),
        "read phase failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&read_output.stdout),
        String::from_utf8_lossy(&read_output.stderr)
    );
    let write_plan = apply_patch_write_plan_from_read_output(
        patch,
        &String::from_utf8_lossy(&read_output.stdout),
    )
    .unwrap();
    let write_output = Command::new("/bin/sh")
        .arg("-c")
        .arg(&write_plan.command)
        .current_dir(&temp)
        .output()
        .unwrap();
    assert!(
        write_output.status.success(),
        "write phase failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&write_output.stdout),
        String::from_utf8_lossy(&write_output.stderr)
    );

    assert_eq!(
        std::fs::read_to_string(temp.join("real.txt")).unwrap(),
        "new\n"
    );
    assert!(
        std::fs::symlink_metadata(temp.join("link.txt"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies accumulated read snapshots can build one verified write phase.
///
/// Shell-backed `apply_patch` may split large patches into multiple read
/// transactions to stay under PTY capture limits. The semantic planner must be
/// able to merge those independent read outputs before generating the write
/// transaction so multi-file patches keep their original atomic verification
/// behavior.
fn semantic_apply_patch_write_plan_accepts_accumulated_read_snapshots() {
    let temp = test_temp_dir("semantic-codex-patch-accumulated-read");
    let patch =
        "*** Begin Patch\n*** Add File: one.txt\n+one\n*** Add File: two.txt\n+two\n*** End Patch";

    let mut first_paths = BTreeSet::new();
    first_paths.insert("one.txt".to_string());
    let first_plan = apply_patch_read_plan_for_paths(&first_paths);
    let first_output = Command::new("/bin/sh")
        .arg("-c")
        .arg(&first_plan.command)
        .current_dir(&temp)
        .output()
        .unwrap();
    assert!(first_output.status.success());

    let mut second_paths = BTreeSet::new();
    second_paths.insert("two.txt".to_string());
    let second_plan = apply_patch_read_plan_for_paths(&second_paths);
    let second_output = Command::new("/bin/sh")
        .arg("-c")
        .arg(&second_plan.command)
        .current_dir(&temp)
        .output()
        .unwrap();
    assert!(second_output.status.success());

    let read_outputs = vec![
        String::from_utf8_lossy(&first_output.stdout).to_string(),
        String::from_utf8_lossy(&second_output.stdout).to_string(),
    ];
    let write_plan = apply_patch_write_plan_from_read_outputs(patch, &read_outputs).unwrap();
    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg(&write_plan.command)
        .current_dir(&temp)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(temp.join("one.txt")).unwrap(),
        "one\n"
    );
    assert_eq!(
        std::fs::read_to_string(temp.join("two.txt")).unwrap(),
        "two\n"
    );
    std::fs::remove_dir_all(&temp).unwrap();
}
