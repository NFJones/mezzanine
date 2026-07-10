//! Semantic Patch tests for matching behavior.
//!
//! This bounded leaf owns the named behavioral scenarios.

use super::*;

#[test]
/// Verifies anchored line-range hints remain conservative.
///
/// Header anchors remain stronger placement constraints than unified old-line
/// ranges. If the anchored structural scope still has only a near range-hint
/// winner, the patch should fail instead of guessing.
fn semantic_apply_patch_anchor_scope_rejects_range_hint_override() {
    let temp = test_temp_dir("semantic-codex-patch-anchor-range-override");
    std::fs::write(
        temp.join("note.rs"),
        "fn target() {\n    println!(\"old\");\n    println!(\"old\");\n}\n",
    )
    .unwrap();
    let patch = "*** Begin Patch\n*** Update File: note.rs\n@@ -2,1 +2,1 @@ fn target() {\n-    println!(\"old\");\n+    println!(\"new\");\n*** End Patch";

    let error = apply_patch_write_error(&temp, patch);

    assert!(
        error.contains("exact hunk context is ambiguous in the current file"),
        "{error}"
    );
    assert!(
        error.contains("matching_scope=structural_anchor_scope"),
        "{error}"
    );
    assert!(
        error.contains("range_hint_disambiguation=rejected reason=near_tie hint_line=2"),
        "{error}"
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies `@@` header anchors can disambiguate repeated exact hunk context.
///
/// Repeated single-line context is common in tests and documentation. Header
/// anchors let the semantic patcher select the intended region without making
/// the model include a brittle oversized hunk.
fn semantic_apply_patch_hunk_header_selects_repeated_context() {
    let temp = test_temp_dir("semantic-codex-patch-anchor");
    std::fs::write(
        temp.join("note.rs"),
        "fn first() {\n    println!(\"old\");\n}\n\nfn second() {\n    println!(\"old\");\n}\n",
    )
    .unwrap();
    let patch = "*** Begin Patch\n*** Update File: note.rs\n@@ fn second\n-    println!(\"old\");\n+    println!(\"new\");\n*** End Patch";

    let output = run_apply_patch_action(&temp, patch);

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(temp.join("note.rs")).unwrap(),
        "fn first() {\n    println!(\"old\");\n}\n\nfn second() {\n    println!(\"new\");\n}\n"
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies Mezzanine patch hunk mismatches report actionable context.
///
/// A hunk mismatch does not prove the file changed after the model read it. It
/// only proves the old-context lines are not an exact match for the current
/// file. The diagnostic should make that distinction and preserve enough of
/// the failed hunk for model correction.
fn semantic_apply_patch_hunk_mismatch_reports_failed_context() {
    let temp = test_temp_dir("semantic-codex-patch-mismatch");
    std::fs::write(temp.join("note.txt"), "old\ncontext\n").unwrap();
    let patch =
        "*** Begin Patch\n*** Update File: note.txt\n@@\n-missing\n+new\n context\n*** End Patch";
    let action = AgentAction {
        id: "patch-mismatch".to_string(),
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
            .contains("apply_patch: hunk did not match: note.txt"),
        "{}",
        error.message()
    );
    assert!(
        error
            .message()
            .contains("hunk context was not found in the current file"),
        "{}",
        error.message()
    );
    assert!(
        error
            .message()
            .contains("failure_code=HUNK_CONTEXT_MISMATCH"),
        "{}",
        error.message()
    );
    assert!(
        error.message().contains("affected_path=note.txt"),
        "{}",
        error.message()
    );
    assert!(
        error
            .message()
            .contains("matching_attempts=exact:0,trim_end:0"),
        "{}",
        error.message()
    );
    assert!(
        error
            .message()
            .contains("suggested_next_step=reread_region"),
        "{}",
        error.message()
    );
    assert!(
        error.message().contains("retry_without_reread=false"),
        "{}",
        error.message()
    );
    assert!(
        error
            .message()
            .contains("suggested_read_range=note.txt:1-2"),
        "{}",
        error.message()
    );
    assert!(
        error
            .message()
            .contains("first old-context line was not found anywhere"),
        "{}",
        error.message()
    );
    assert!(
        error.message().contains("apply_patch:   missing"),
        "{}",
        error.message()
    );
    assert!(
        error
            .message()
            .contains("current file context near line 1 follows"),
        "{}",
        error.message()
    );
    assert!(
        error.message().contains("apply_patch:      1: old"),
        "{}",
        error.message()
    );
    assert!(
        error
            .message()
            .contains("next step: read note.txt around the reported line(s)"),
        "{}",
        error.message()
    );
    assert!(
        error
            .message()
            .contains("retry with a smaller fresh Mezzanine patch"),
        "{}",
        error.message()
    );
    assert!(
        error
            .message()
            .contains("do not retry substantially the same patch"),
        "{}",
        error.message()
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies hunk mismatch diagnostics report nearby non-exact first-context matches.
///
/// Models can self-correct faster when a copied old-context line only differs
/// by trailing or surrounding whitespace. The mismatch diagnostic should report
/// the matching mode and current line instead of only saying that the exact old
/// line is absent.
fn semantic_apply_patch_hunk_mismatch_reports_non_exact_first_context_line() {
    let temp = test_temp_dir("semantic-codex-patch-non-exact-first-context");
    std::fs::write(temp.join("note.txt"), "old   \nother\n").unwrap();
    let patch = "*** Begin Patch\n*** Update File: note.txt\n@@\n-old\n-context\n+new\n+context\n*** End Patch";

    let error = apply_patch_write_error(&temp, patch);

    assert!(
        error.contains("first old-context line was not found anywhere"),
        "{error}"
    );
    assert!(
        error.contains(
            "first old-context line nearest non-exact match mode=trim_end current line(s): 1"
        ),
        "{error}"
    );
    assert!(
        error.contains("suggested_read_range=note.txt:1-2"),
        "{error}"
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies hunk mismatch diagnostics report distinctive added lines.
///
/// When the exact replacement block is no longer present because neighboring
/// context changed, the presence of distinctive added lines is still useful
/// evidence that the target may have been rewritten or partly applied.
fn semantic_apply_patch_hunk_mismatch_reports_present_distinctive_added_lines() {
    let temp = test_temp_dir("semantic-codex-patch-added-lines-present");
    std::fs::write(temp.join("note.txt"), "new_helper();\nother\n").unwrap();
    let patch = "*** Begin Patch\n*** Update File: note.txt\n@@\n-missing_old();\n+new_helper();\n context\n*** End Patch";

    let error = apply_patch_write_error(&temp, patch);

    assert!(
        error.contains("failure_code=HUNK_CONTEXT_MISMATCH"),
        "{error}"
    );
    assert!(
        error.contains("replacement_hint=distinctive_added_lines_present span(s): 1"),
        "{error}"
    );
    assert!(
        error.contains("replacement_hint_next_step=skip_or_reconcile_already_applied_change"),
        "{error}"
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies hunk mismatch diagnostics report already-present replacement blocks.
///
/// A failed hunk can mean the model is replaying a stale patch after the target
/// already reached the intended state. The diagnostic should point recovery
/// toward reconciling current file contents instead of forcing another retry.
fn semantic_apply_patch_hunk_mismatch_reports_present_replacement_block() {
    let temp = test_temp_dir("semantic-codex-patch-replacement-block-present");
    std::fs::write(temp.join("note.txt"), "new\ncontext\n").unwrap();
    let patch =
        "*** Begin Patch\n*** Update File: note.txt\n@@\n-old\n+new\n context\n*** End Patch";

    let error = apply_patch_write_error(&temp, patch);

    assert!(
        error.contains("failure_code=HUNK_CONTEXT_MISMATCH"),
        "{error}"
    );
    assert!(
        error.contains("replacement_hint=full_replacement_block_present span(s): 1-2"),
        "{error}"
    );
    assert!(
        error.contains("replacement_hint_next_step=skip_or_reconcile_already_applied_change"),
        "{error}"
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies insertion hunks tolerate an omitted blank separator line between
/// copied context blocks.
///
/// This reproduces a real Chimera patch failure where the model copied the
/// closing lines of one test, inserted a new test, and then copied the next
/// doc comment, but omitted the blank line separating those tests in the
/// current file. The matcher may recover from that blank-only omission, but it
/// must preserve the current blank separator before the following test.
fn semantic_apply_patch_insertion_tolerates_omitted_blank_separator_context() {
    let temp = test_temp_dir("semantic-codex-patch-omitted-blank-separator");
    let tests_dir = temp.join("tests");
    std::fs::create_dir_all(&tests_dir).unwrap();
    std::fs::write(
        tests_dir.join("standard_config_consumer_test.rs"),
        r#"/// Verifies that the selected-image plan exposes the canonical target file path
/// and the directory containing it as the build context.
#[test]
fn selected_image_uses_config_directory_as_build_context() {
    let selected = build_selected_image_plan(&path, None).unwrap();
    assert_eq!(selected.image_name, "build");
    assert_eq!(selected.effective_object_name, "sample");
    assert_eq!(selected.driver_type, "docker");
    assert_eq!(
        selected.target_config_path,
        fs::canonicalize(&path).unwrap()
    );
    assert_eq!(
        selected.target_build_context,
        fs::canonicalize(path.parent().unwrap()).unwrap()
    );
}

/// Verifies that the consumer rejects configurations that omit the required
/// top-level driver field.
#[test]
fn load_image_context_rejects_missing_driver_field() {}
"#,
    )
    .unwrap();
    let patch = r#"*** Begin Patch
*** Update File: tests/standard_config_consumer_test.rs
@@ fn selected_image_uses_config_directory_as_build_context() {
     assert_eq!(
         selected.target_build_context,
         fs::canonicalize(path.parent().unwrap()).unwrap()
     );
 }
+/// Verifies that the public selected-image plan preserves declared artifact
+/// metadata without altering stage lowering semantics.
+#[test]
+fn selected_image_plan_preserves_declared_artifacts() {
+    let selected = build_selected_image_plan(&path, None).unwrap();
+    assert_eq!(selected.image_name, "build");
+}
 /// Verifies that the consumer rejects configurations that omit the required
 /// top-level driver field.
*** End Patch"#;

    let output = run_apply_patch_action(&temp, patch);

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let updated =
        std::fs::read_to_string(tests_dir.join("standard_config_consumer_test.rs")).unwrap();
    assert!(
        updated.contains(
            "    );\n}\n/// Verifies that the public selected-image plan preserves declared artifact"
        ),
        "{updated}"
    );
    assert!(
        updated.contains(
            "    assert_eq!(selected.image_name, \"build\");\n}\n\n/// Verifies that the consumer rejects"
        ),
        "{updated}"
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies update hunks can tolerate common Unicode punctuation drift.
///
/// This mirrors Codex's final normalized matching pass for typographic
/// punctuation and unusual space characters while preserving deterministic
/// matching: if normalization would identify multiple locations, the patch
/// remains model-correctable instead of applying arbitrarily.
fn semantic_apply_patch_normalized_match_handles_typographic_punctuation() {
    let temp = test_temp_dir("semantic-codex-patch-normalized");
    std::fs::write(temp.join("note.txt"), "old — value\ncontext\n").unwrap();
    let patch = "*** Begin Patch\n*** Update File: note.txt\n@@\n-old - value\n+new - value\n context\n*** End Patch";

    let output = run_apply_patch_action(&temp, patch);

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(temp.join("note.txt")).unwrap(),
        "new - value\ncontext\n"
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies omitted-blank matching retries before cursor progress.
///
/// A later exact hunk can advance the unanchored search cursor before a
/// following hunk needs blank-context tolerance against earlier file content.
/// The tolerant matcher must mirror exact matching by retrying the whole file
/// instead of only searching after the cursor.
fn semantic_apply_patch_omitted_blank_context_retries_before_cursor() {
    let temp = test_temp_dir("semantic-codex-patch-blank-context-before-cursor");
    std::fs::write(
        temp.join("note.rs"),
        "fn earlier() {\n    keep();\n\n    old();\n}\n\nfn later() {\n    tail();\n}\n",
    )
    .unwrap();
    let patch = "*** Begin Patch\n*** Update File: note.rs\n@@\n fn later() {\n-    tail();\n+    done();\n }\n@@\n fn earlier() {\n     keep();\n-    old();\n+    new();\n }\n*** End Patch";

    let output = run_apply_patch_action(&temp, patch);

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(temp.join("note.rs")).unwrap(),
        "fn earlier() {\n    keep();\n    new();\n}\n\nfn later() {\n    done();\n}\n"
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies omitted-line recovery does not skip nonblank current-file content.
///
/// The compatibility path is only for missing blank separators. Nonblank lines
/// between copied context blocks still indicate stale or insufficient context
/// and must force the model to re-read and retry.
fn semantic_apply_patch_omitted_blank_separator_context_rejects_nonblank_gap() {
    let temp = test_temp_dir("semantic-codex-patch-omitted-blank-nonblank");
    std::fs::write(
        temp.join("note.rs"),
        "fn test() {\n    old();\n    keep_this();\n    next();\n}\n",
    )
    .unwrap();
    let patch = "*** Begin Patch\n*** Update File: note.rs\n@@ fn test\n     old();\n+    inserted();\n    next();\n*** End Patch";
    let action = AgentAction {
        id: "patch-nonblank-gap".to_string(),
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
    assert!(read_output.status.success());
    let error = apply_patch_write_plan_from_read_output(
        patch,
        &String::from_utf8_lossy(&read_output.stdout),
    )
    .unwrap_err();

    assert!(
        error.message().contains("hunk did not match: note.rs"),
        "{}",
        error.message()
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies omitted blank-line recovery remains deterministic.
///
/// When the same insertion-boundary context appears more than once, silently
/// choosing one omitted-blank match would risk editing the wrong block. The
/// patch must stay model-correctable instead.
fn semantic_apply_patch_omitted_blank_separator_context_reports_ambiguity() {
    let temp = test_temp_dir("semantic-codex-patch-omitted-blank-ambiguous");
    std::fs::write(
        temp.join("note.rs"),
        "fn first() {\n}\n\n/// next\nfn second() {\n}\n\n/// next\n",
    )
    .unwrap();
    let patch = "*** Begin Patch\n*** Update File: note.rs\n@@ fn\n }\n+// inserted\n /// next\n*** End Patch";
    let action = AgentAction {
        id: "patch-ambiguous-blank".to_string(),
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
    assert!(read_output.status.success());
    let error = apply_patch_write_plan_from_read_output(
        patch,
        &String::from_utf8_lossy(&read_output.stdout),
    )
    .unwrap_err();

    assert!(
        error
            .message()
            .contains("hunk context is ambiguous in the current file"),
        "{}",
        error.message()
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies structural anchor scopes do not hide internal ambiguity.
///
/// If a resolved function block still contains multiple valid old-context
/// candidates, the patch must fail rather than falling back to a broader range
/// or using the first match inside the block.
fn semantic_apply_patch_structural_anchor_scope_rejects_internal_ambiguity() {
    let temp = test_temp_dir("semantic-codex-patch-structural-anchor-ambiguous");
    std::fs::write(
        temp.join("note.rs"),
        "fn target() {\n    println!(\"old\");\n    println!(\"old\");\n}\n\nfn later() {\n    println!(\"old\");\n}\n",
    )
    .unwrap();
    let patch = "*** Begin Patch\n*** Update File: note.rs\n@@ fn target() {\n-    println!(\"old\");\n+    println!(\"new\");\n*** End Patch";

    let error = apply_patch_write_error(&temp, patch);

    assert!(
        error.contains("exact hunk context is ambiguous in the current file"),
        "{error}"
    );
    assert!(
        error.contains("matching_scope=structural_anchor_scope"),
        "{error}"
    );
    assert!(error.contains("candidate match span(s): 2, 3"), "{error}");
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies Rust-like header anchors bound matching to a structural scope.
///
/// A repeated old-context body can appear again after the anchored function.
/// The patcher should use the function block as the first search scope and
/// apply only when that scope contains one deterministic candidate.
fn semantic_apply_patch_structural_anchor_scope_selects_candidate() {
    let temp = test_temp_dir("semantic-codex-patch-structural-anchor");
    std::fs::write(
        temp.join("note.rs"),
        "fn target() {\n    println!(\"old\");\n}\n\nfn later() {\n    println!(\"old\");\n}\n",
    )
    .unwrap();
    let patch = "*** Begin Patch\n*** Update File: note.rs\n@@ fn target() {\n-    println!(\"old\");\n+    println!(\"new\");\n*** End Patch";

    let output = run_apply_patch_action(&temp, patch);

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(temp.join("note.rs")).unwrap(),
        "fn target() {\n    println!(\"new\");\n}\n\nfn later() {\n    println!(\"old\");\n}\n"
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies skipped blank-line recovery applies between copied context and a
/// following removed block.
///
/// When the omitted current-file lines are blank-only and the following old
/// line is being removed, the blank separator is deleted with that removed
/// block. This matches common replacement hunks that omit quiet separator
/// lines around the old block.
fn semantic_apply_patch_tolerates_omitted_blank_between_context_and_remove() {
    let temp = test_temp_dir("semantic-codex-patch-blank-context-remove");
    std::fs::write(
        temp.join("main.rs"),
        r#"fn main() {
    keep();

    old_call();
}
"#,
    )
    .unwrap();
    let patch = r#"*** Begin Patch
*** Update File: main.rs
@@
 fn main() {
     keep();
-    old_call();
+    new_call();
 }
*** End Patch"#;

    let output = run_apply_patch_action(&temp, patch);

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(temp.join("main.rs")).unwrap(),
        r#"fn main() {
    keep();
    new_call();
}
"#
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies skipped blank-line recovery applies between removed text and later
/// copied context.
///
/// Models often omit the visual blank separator after a removed block while
/// keeping the following copied context line. The patcher should preserve that
/// current-file blank before the copied context instead of failing the hunk.
fn semantic_apply_patch_tolerates_omitted_blank_between_remove_and_context() {
    let temp = test_temp_dir("semantic-codex-patch-blank-remove-context");
    std::fs::write(
        temp.join("main.rs"),
        r#"//! Summary.
//!
//! Old implementation note.

use chimera::conf::consumer::build_selected_image_plan;
"#,
    )
    .unwrap();
    let patch = r#"*** Begin Patch
*** Update File: main.rs
@@
 //! Summary.
 //!
-//! Old implementation note.
+//! New implementation note.
+use glob::Pattern;
 use chimera::conf::consumer::build_selected_image_plan;
*** End Patch"#;

    let output = run_apply_patch_action(&temp, patch);

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(temp.join("main.rs")).unwrap(),
        r#"//! Summary.
//!
//! New implementation note.
use glob::Pattern;

use chimera::conf::consumer::build_selected_image_plan;
"#
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies skipped blank-line recovery also applies between copied context
/// lines.
///
/// Models often copy documentation snippets from rendered output or a compact
/// read where blank separator lines are visually easy to miss. The patcher may
/// recover when the omitted current-file content is blank-only and the match is
/// still unique, while preserving those blanks from the current file rather
/// than rewriting the surrounding context from the patch payload.
fn semantic_apply_patch_tolerates_omitted_blank_context_between_copied_lines() {
    let temp = test_temp_dir("semantic-codex-patch-omitted-blank-context");
    std::fs::write(
        temp.join("SPEC.md"),
        r#"#### 13.10.16 `STOPSIGNAL`

`STOPSIGNAL` MUST be serialized as:

`STOPSIGNAL <value>`

#### 13.10.17 `HEALTHCHECK`

The Docker Driver Profile MUST support:
"#,
    )
    .unwrap();
    let patch = r#"*** Begin Patch
*** Update File: SPEC.md
@@
 #### 13.10.16 `STOPSIGNAL`
 `STOPSIGNAL` MUST be serialized as:
 `STOPSIGNAL <value>`
+The `<value>` token MUST be emitted exactly as provided by the Stage Action.
+The Docker Driver Profile MUST NOT rewrite, normalize, or quote the token
+during `STOPSIGNAL` serialization.
 #### 13.10.17 `HEALTHCHECK`
 The Docker Driver Profile MUST support:
*** End Patch"#;

    let output = run_apply_patch_action(&temp, patch);

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(temp.join("SPEC.md")).unwrap(),
        r#"#### 13.10.16 `STOPSIGNAL`

`STOPSIGNAL` MUST be serialized as:

`STOPSIGNAL <value>`
The `<value>` token MUST be emitted exactly as provided by the Stage Action.
The Docker Driver Profile MUST NOT rewrite, normalize, or quote the token
during `STOPSIGNAL` serialization.

#### 13.10.17 `HEALTHCHECK`

The Docker Driver Profile MUST support:
"#
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies skipped blank-line recovery also applies between removed lines.
///
/// Real model-authored replacement hunks often omit visually quiet blank
/// separators inside the old deletion block. When the skipped current-file
/// lines are blank-only and the match is unique, the patcher should include
/// those blanks in the replacement span and delete them with the surrounding
/// removed block instead of reporting a hunk mismatch.
fn semantic_apply_patch_tolerates_omitted_blank_context_between_removed_lines() {
    let temp = test_temp_dir("semantic-codex-patch-omitted-blank-removed-lines");
    std::fs::write(
        temp.join("main.rs"),
        r#"use std::env;

fn parse_cli_args() -> Result<(String, Option<String>), String> {
    let mut arguments = env::args().skip(1);
    let Some(config_path) = arguments.next() else {
        return Err("usage: chi <config-path> [image-name]".to_string());
    };

    let image_name = arguments.next();
    if arguments.next().is_some() {
        return Err("usage: chi <config-path> [image-name]".to_string());
    }

    Ok((config_path, image_name))
}
"#,
    )
    .unwrap();
    let patch = r#"*** Begin Patch
*** Update File: main.rs
@@
 fn parse_cli_args() -> Result<(String, Option<String>), String> {
-    let mut arguments = env::args().skip(1);
-    let Some(config_path) = arguments.next() else {
-        return Err("usage: chi <config-path> [image-name]".to_string());
-    };
-    let image_name = arguments.next();
-    if arguments.next().is_some() {
-        return Err("usage: chi <config-path> [image-name]".to_string());
-    }
-    Ok((config_path, image_name))
+    parse_cli_args_from(env::args().skip(1))
 }
*** End Patch"#;

    let output = run_apply_patch_action(&temp, patch);

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(temp.join("main.rs")).unwrap(),
        r#"use std::env;

fn parse_cli_args() -> Result<(String, Option<String>), String> {
    parse_cli_args_from(env::args().skip(1))
}
"#
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies update hunks tolerate trailing-whitespace drift without rewriting
/// unchanged context lines.
///
/// Models often omit invisible trailing spaces from context. The patcher may
/// use that omission to locate the hunk, but context lines are not proposed
/// changes and must therefore preserve the target file's actual text.
fn semantic_apply_patch_trim_end_match_preserves_current_context_lines() {
    let temp = test_temp_dir("semantic-codex-patch-trim-end");
    std::fs::write(temp.join("note.txt"), "old   \ncontext   \n").unwrap();
    let patch =
        "*** Begin Patch\n*** Update File: note.txt\n@@\n-old\n+new\n context\n*** End Patch";

    let output = run_apply_patch_action(&temp, patch);

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(temp.join("note.txt")).unwrap(),
        "new\ncontext   \n"
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies widened trailing-whitespace matching still fails when it cannot
/// identify one unique target location.
///
/// Tolerant matching is only safe if it remains deterministic. When trimming
/// trailing whitespace produces multiple candidate locations, the action should
/// remain model-correctable instead of choosing the first candidate.
fn semantic_apply_patch_trim_end_match_reports_ambiguity() {
    let temp = test_temp_dir("semantic-codex-patch-trim-end-ambiguous");
    std::fs::write(
        temp.join("note.txt"),
        "first\nold   \ncontext\nsecond\nold\t\ncontext\n",
    )
    .unwrap();
    let patch =
        "*** Begin Patch\n*** Update File: note.txt\n@@\n-old\n+new\n context\n*** End Patch";
    let action = AgentAction {
        id: "patch-trim-end-ambiguous".to_string(),
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
            .contains("trim_end hunk context is ambiguous"),
        "{}",
        error.message()
    );
    assert!(
        error
            .message()
            .contains("matching_attempts=exact:0,trim_end:2"),
        "{}",
        error.message()
    );
    assert!(
        error.message().contains("ambiguous_matching_mode=trim_end"),
        "{}",
        error.message()
    );
    assert!(
        error.message().contains("candidate match line(s): 2, 5"),
        "{}",
        error.message()
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies update hunks can tolerate leading-and-trailing whitespace drift.
///
/// Codex attempts a trim-both match after exact and trailing-whitespace
/// matching. Mezzanine keeps the same recovery path only when it identifies one
/// deterministic location, and it still preserves current-file context lines
/// rather than rewriting them from the patch.
fn semantic_apply_patch_trim_match_preserves_current_context_lines() {
    let temp = test_temp_dir("semantic-codex-patch-trim");
    std::fs::write(temp.join("note.txt"), "    old\n    context\n").unwrap();
    let patch =
        "*** Begin Patch\n*** Update File: note.txt\n@@\n-old\n+new\n context\n*** End Patch";

    let output = run_apply_patch_action(&temp, patch);

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(temp.join("note.txt")).unwrap(),
        "new\n    context\n"
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies unanchored pure-addition hunks append by default.
///
/// Codex applies update hunks with no old lines at the end of the current
/// file. Matching that behavior makes append-like patches predictable while
/// still allowing explicit anchors for insertions elsewhere.
fn semantic_apply_patch_unanchored_pure_addition_appends_like_codex() {
    let temp = test_temp_dir("semantic-codex-patch-pure-addition-append");
    std::fs::write(temp.join("note.txt"), "old\n").unwrap();
    let patch = "*** Begin Patch\n*** Update File: note.txt\n@@\n+new\n*** End Patch";

    let output = run_apply_patch_action(&temp, patch);

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(temp.join("note.txt")).unwrap(),
        "old\nnew\n"
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies unified hunk line ranges can safely disambiguate repeated old
/// context.
///
/// Models frequently include `@@ -old,+new @@` range metadata. The range is
/// not trusted by itself, but when the old-context lines still match at that
/// position it is a useful compatibility hint that avoids unnecessary
/// ambiguity failures in repeated code or test blocks.
fn semantic_apply_patch_unified_range_disambiguates_repeated_unanchored_hunk() {
    let temp = test_temp_dir("semantic-codex-patch-unified-range-disambiguates");
    std::fs::write(
        temp.join("note.rs"),
        "fn first() {\n    old();\n}\n\nfn second() {\n    old();\n}\n",
    )
    .unwrap();
    let patch = "*** Begin Patch\n*** Update File: note.rs\n@@ -6,1 +6,1 @@\n-    old();\n+    new();\n*** End Patch";

    let output = run_apply_patch_action(&temp, patch);

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(temp.join("note.rs")).unwrap(),
        "fn first() {\n    old();\n}\n\nfn second() {\n    new();\n}\n"
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies stale line ranges cannot choose distant repeated candidates.
///
/// A line hint far away from every real text match is treated as unreliable
/// placement data and leaves the repeated hunk body ambiguous.
fn semantic_apply_patch_unified_range_rejects_distant_candidates() {
    let temp = test_temp_dir("semantic-codex-patch-unified-range-distant");
    std::fs::write(temp.join("note.rs"), "old();\nold();\n").unwrap();
    let patch = "*** Begin Patch\n*** Update File: note.rs\n@@ -80,1 +80,1 @@\n-old();\n+new();\n*** End Patch";

    let error = apply_patch_write_error(&temp, patch);

    assert!(
        error.contains("range_hint_disambiguation=rejected reason=distant hint_line=80"),
        "{error}"
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies near range-hint wins are still rejected as ambiguous.
///
/// The range hint should not silently select one of several very close text
/// matches because a stale line number can easily drift by a couple of lines.
fn semantic_apply_patch_unified_range_rejects_near_tie_candidates() {
    let temp = test_temp_dir("semantic-codex-patch-unified-range-near-tie");
    std::fs::write(
        temp.join("note.rs"),
        "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8\nline 9\nold();\nline 11\nold();\n",
    )
    .unwrap();
    let patch = "*** Begin Patch\n*** Update File: note.rs\n@@ -10,1 +10,1 @@\n-old();\n+new();\n*** End Patch";

    let error = apply_patch_write_error(&temp, patch);

    assert!(
        error.contains("range_hint_disambiguation=rejected reason=near_tie hint_line=10"),
        "{error}"
    );
    assert!(error.contains("nearest_distance=0"), "{error}");
    assert!(error.contains("next_distance=2"), "{error}");
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies unified hunk line ranges are only conservative tie-breakers.
///
/// Repeated candidate bodies are common in generated patches. A line hint may
/// select a candidate only when one text match is clearly nearest to the hinted
/// old line; otherwise the patch must fail as ambiguous instead of guessing.
fn semantic_apply_patch_unified_range_rejects_tied_candidates() {
    let temp = test_temp_dir("semantic-codex-patch-unified-range-tie");
    std::fs::write(
        temp.join("note.rs"),
        "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8\nline 9\nold();\nline 11\nold();\n",
    )
    .unwrap();
    let patch = "*** Begin Patch\n*** Update File: note.rs\n@@ -11,1 +11,1 @@\n-old();\n+new();\n*** End Patch";

    let error = apply_patch_write_error(&temp, patch);

    assert!(
        error.contains("exact hunk context is ambiguous in the current file"),
        "{error}"
    );
    assert!(error.contains("matching_scope=full_file"), "{error}");
    assert!(error.contains("candidate match span(s): 10, 12"), "{error}");
    assert!(
        error.contains("suggested_candidate_read_range(s): note.rs:6-12, note.rs:8-12"),
        "{error}"
    );
    assert!(
        error.contains("range_hint_disambiguation=rejected reason=tie hint_line=11"),
        "{error}"
    );
    std::fs::remove_dir_all(&temp).unwrap();
}
