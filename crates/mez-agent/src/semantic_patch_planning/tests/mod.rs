//! Intrinsic semantic-patch planning tests.
//!
//! These tests exercise parser integration, snapshot matching, diagnostics,
//! and generated read/write shell transactions without product runtime ports.

use super::*;
use crate::semantic_patch::try_convert_unified_diff_to_mez_patch;
use crate::{AgentAction, AgentActionPayload, LocalActionPlan};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

mod execution;
mod matching;
mod parsing;
mod planning;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Creates a unique temporary directory for one semantic-patch test.
fn test_temp_dir(label: &str) -> PathBuf {
    let sequence = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "mez-agent-{label}-{}-{sequence}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

/// Creates a leading `PATH` entry whose `realpath` rejects GNU `-m` probes.
///
/// Native-resolver tests use this to exercise the pane fallback even on Linux
/// development hosts where GNU coreutils would otherwise take the fast path.
#[cfg(unix)]
fn path_with_failing_realpath(root: &Path) -> String {
    use std::os::unix::fs::PermissionsExt;

    let fake_bin = root.join("no-gnu-realpath");
    std::fs::create_dir_all(&fake_bin).unwrap();
    let fake_realpath = fake_bin.join("realpath");
    std::fs::write(&fake_realpath, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&fake_realpath, std::fs::Permissions::from_mode(0o755)).unwrap();
    format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

/// Builds a Mezzanine add-file patch for one relative path and exact content.
fn add_file_patch(path: &str, content: &str) -> String {
    let mut patch = format!("*** Begin Patch\n*** Add File: {path}\n");
    for line in content.split_inclusive('\n') {
        patch.push('+');
        patch.push_str(line);
    }
    if !content.ends_with('\n') && !content.is_empty() {
        patch.push('\n');
    }
    patch.push_str("*** End Patch");
    patch
}

/// Plans the semantic action kinds exercised by this lower-crate test module.
fn local_action_plan(action: &AgentAction) -> Result<Option<LocalActionPlan>> {
    match &action.payload {
        AgentActionPayload::ApplyPatch { patch, strip } => {
            apply_patch_plan(patch, *strip).map(Some)
        }
        _ => Ok(None),
    }
}

/// Executes one local action plan through the materialized-script shape used
/// by production shell transactions.
///
/// Semantic write sidecars are appended as inert comments so generated write
/// commands can read their single-encoded payload through `$0` without tests
/// accidentally inspecting the `/bin/sh` executable used by `sh -c`.
fn run_local_action_plan(cwd: &Path, plan: &LocalActionPlan) -> Output {
    run_local_action_plan_with_path(cwd, plan, None)
}

/// Executes one materialized local action plan with an optional PATH override.
fn run_local_action_plan_with_path(
    cwd: &Path,
    plan: &LocalActionPlan,
    path: Option<&str>,
) -> Output {
    run_local_action_plan_with_shell_path(cwd, plan, Path::new("/bin/sh"), path)
}

/// Executes one materialized local action plan through an explicit shell.
fn run_local_action_plan_with_shell(cwd: &Path, plan: &LocalActionPlan, shell: &Path) -> Output {
    run_local_action_plan_with_shell_path(cwd, plan, shell, None)
}

/// Executes one materialized plan through an explicit shell and PATH.
fn run_local_action_plan_with_shell_path(
    cwd: &Path,
    plan: &LocalActionPlan,
    shell: &Path,
    path: Option<&str>,
) -> Output {
    let sequence = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let script = cwd.join(format!(
        ".mez-agent-plan-{}-{sequence}.sh",
        std::process::id()
    ));
    let mut source = plan.command.clone();
    if !source.ends_with('\n') {
        source.push('\n');
    }
    if let Some(sidecar) = &plan.input_sidecar {
        for record in sidecar.lines() {
            source.push_str("# __MEZ_INPUT_SIDECAR_V1__ ");
            source.push_str(record);
            source.push('\n');
        }
    }
    std::fs::write(&script, source).unwrap();
    let mut command = Command::new(shell);
    command.arg(&script).current_dir(cwd);
    if let Some(path) = path {
        command.env("PATH", path);
    }
    let output = command.output().unwrap();
    std::fs::remove_file(script).unwrap();
    output
}

/// Executes an `apply_patch` action through its read and write phases.
fn run_apply_patch_action(cwd: &Path, patch: &str) -> Output {
    let action = AgentAction {
        id: "patch".to_string(),
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
        .current_dir(cwd)
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
    run_local_action_plan(cwd, &write_plan)
}

/// Returns the write-phase error for one semantic-patch action.
fn apply_patch_write_error(cwd: &Path, patch: &str) -> String {
    let action = AgentAction {
        id: "patch-error".to_string(),
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
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        read_output.status.success(),
        "read phase failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&read_output.stdout),
        String::from_utf8_lossy(&read_output.stderr)
    );
    apply_patch_write_plan_from_read_output(patch, &String::from_utf8_lossy(&read_output.stdout))
        .unwrap_err()
        .message()
        .to_string()
}
