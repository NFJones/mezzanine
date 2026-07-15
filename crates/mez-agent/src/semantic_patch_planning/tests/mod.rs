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
    Command::new("/bin/sh")
        .arg("-c")
        .arg(&write_plan.command)
        .current_dir(cwd)
        .output()
        .unwrap()
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
