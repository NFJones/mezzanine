//! Shared matching regressions for ambiguity, structural scopes and work fences.
//!
//! These tests use the real search helpers. Native controls must stop CPU work;
//! the unlimited pane controls preserve the established matching behavior.

use super::PatchWorkControl;
use super::{
    ApplyPatchBlankGapPolicy, find_unanchored_hunk_position_layered, rust_like_brace_counts,
    structural_anchor_ranges,
};

/// Verifies tolerant unanchored search uses non-overlapping cursor-before
/// and cursor-after ranges. A match at or after the cursor used to be
/// discovered once in the cursor-forward range and again in the full-file
/// fallback range, which made a unique blank-gap match look ambiguous.
#[test]
fn unanchored_tolerant_search_does_not_duplicate_cursor_forward_match() {
    let lines = ["before", "target", "", "after", "tail"]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let old = ["target", "after"]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let blank_gap_policies = [
        ApplyPatchBlankGapPolicy::Disallow,
        ApplyPatchBlankGapPolicy::Preserve,
    ];

    let result = find_unanchored_hunk_position_layered(
        &PatchWorkControl::unlimited(),
        &lines,
        &old,
        &blank_gap_policies,
        1,
        None,
    );

    assert!(result.is_ok());
}

/// Verifies raw-string detection does not disable structural brace counts
/// merely because ordinary string or comment text contains the byte
/// sequence `r\"`. Structural anchor scoping depends on this helper
/// returning counts for ordinary Rust-like code.
#[test]
fn rust_like_brace_counts_ignores_raw_string_markers_inside_strings_and_comments() {
    let mut in_block_comment = false;

    let counts = rust_like_brace_counts(
        &PatchWorkControl::unlimited(),
        r#"fn target() { let text = "mentions r\" without raw syntax"; } // r\" comment"#,
        &mut in_block_comment,
    );

    assert_eq!(counts, Some((1, 1)));
}

/// Verifies real Rust raw string literals still disable structural brace
/// counting. The brace scanner intentionally bails out for raw strings so
/// braces inside raw-string bodies cannot corrupt anchor scope detection.
#[test]
fn rust_like_brace_counts_still_rejects_actual_raw_string_literals() {
    let mut in_block_comment = false;

    let counts = rust_like_brace_counts(
        &PatchWorkControl::unlimited(),
        r##"fn target() { let text = r#"{"#; }"##,
        &mut in_block_comment,
    );

    assert_eq!(counts, None);
}

/// Verifies structural anchor scope ranges are merged before hunk matching
/// searches them. Nested anchors can otherwise produce overlapping ranges,
/// causing a unique old-context position inside the overlap to be collected
/// twice and reported as an ambiguous hunk match.
#[test]
fn structural_anchor_ranges_merge_nested_scope_overlaps() {
    let lines = [
        "fn outer() {",
        "    impl Thing {",
        "        fn update(&self) {",
        "            println!(\"old\");",
        "        }",
        "    }",
        "}",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();

    let ranges =
        structural_anchor_ranges(&PatchWorkControl::unlimited(), &lines, &[vec![0], vec![1]]);

    assert_eq!(ranges, vec![(0, 7)]);
}
/// Cancellation inside the real candidate scan must abandon search before
/// diagnostics or another tolerance mode can consume the remaining file.
/// Exact equality is charged even when the first byte already mismatches.
#[test]
fn candidate_scan_stops_on_original_work_fence() {
    use super::{ApplyPatchMatchMode, find_line_sequence_matches};
    use crate::semantic_patch::SemanticPatchPlanningError;
    use std::cell::Cell;
    let lines = vec!["x".repeat(16 * 1024); 256];
    let needle = vec!["y".repeat(16 * 1024)];
    let calls = Cell::new(0);
    let cancel = || {
        calls.set(calls.get() + 1);
        if calls.get() >= 3 {
            Err(SemanticPatchPlanningError::invalid_args(
                "candidate work cancelled",
            ))
        } else {
            Ok(())
        }
    };
    let work = PatchWorkControl::native(&cancel);
    let found = find_line_sequence_matches(
        &work,
        &lines,
        &needle,
        0,
        lines.len(),
        ApplyPatchMatchMode::Exact,
    );
    assert!(found.lines.is_empty());
    assert_eq!(calls.get(), 3);
    assert!(
        work.check()
            .unwrap_err()
            .message()
            .contains("candidate work cancelled")
    );
}

/// The default finite allowance must bound adversarial repeated near-matches,
/// independently of cancellation or elapsed time. The actual candidate search
/// would otherwise compare many gigabytes within a 16MiB input file.
#[test]
fn repeated_near_matches_exhaust_default_computational_budget() {
    use super::{ApplyPatchMatchMode, find_line_sequence_matches};
    let lines = vec!["a".repeat(1024); 16 * 1024];
    let mut needle = vec!["a".repeat(1024); 512];
    needle[511] = "b".repeat(1024);
    let checkpoint = || Ok(());
    let work = PatchWorkControl::native(&checkpoint);
    let found = find_line_sequence_matches(
        &work,
        &lines,
        &needle,
        0,
        lines.len(),
        ApplyPatchMatchMode::Exact,
    );
    assert!(found.lines.is_empty());
    assert!(
        work.check()
            .unwrap_err()
            .message()
            .contains("computational work budget")
    );
}

/// Unicode normalization and long whitespace scans must observe revocation,
/// rather than finishing whole long lines outside the original work fence.
#[test]
fn normalized_long_line_matching_observes_revocation() {
    use super::{ApplyPatchMatchMode, patch_line_matches};
    use crate::semantic_patch::SemanticPatchPlanningError;
    use std::cell::Cell;
    let actual = format!("“{}”", "x".repeat(256 * 1024));
    let expected = format!("\"{}\"", "x".repeat(256 * 1024));
    let calls = Cell::new(0);
    let checkpoint = || {
        calls.set(calls.get() + 1);
        if calls.get() >= 3 {
            Err(SemanticPatchPlanningError::invalid_args(
                "normalization cancelled",
            ))
        } else {
            Ok(())
        }
    };
    let work = PatchWorkControl::native(&checkpoint);
    assert!(!patch_line_matches(
        &work,
        &actual,
        &expected,
        ApplyPatchMatchMode::Normalized
    ));
    assert_eq!(calls.get(), 3);
    assert!(
        work.check()
            .unwrap_err()
            .message()
            .contains("normalization cancelled")
    );
}

/// Native work accounting must preserve each established matching mode and
/// structural prefix recognition below resource limits. In particular trailing
/// whitespace in "fn " belongs to the prefix: only leading whitespace is trimmed.
#[test]
fn native_and_pane_controls_preserve_matching_modes_and_prefixes() {
    use super::{ApplyPatchMatchMode, looks_like_rust_structural_anchor, patch_line_matches};
    let checkpoint = || Ok(());
    let native = PatchWorkControl::native(&checkpoint);
    let pane = PatchWorkControl::unlimited();
    for (mode, actual, expected) in [
        (ApplyPatchMatchMode::Exact, "雪", "雪"),
        (ApplyPatchMatchMode::TrimEnd, "value\u{2003}", "value"),
        (
            ApplyPatchMatchMode::Trim,
            "\u{3000} value \u{00a0}",
            "value",
        ),
        (ApplyPatchMatchMode::Normalized, " “x–y” ", "\"x-y\""),
    ] {
        assert!(patch_line_matches(&native, actual, expected, mode));
        assert_eq!(
            patch_line_matches(&native, actual, expected, mode),
            patch_line_matches(&pane, actual, expected, mode)
        );
        assert!(!patch_line_matches(&native, actual, "unrelated", mode));
    }
    for line in ["   fn ", "  pub fn ", "fn target() {", "fn", "unrelated"] {
        assert_eq!(
            looks_like_rust_structural_anchor(&native, line),
            looks_like_rust_structural_anchor(&pane, line)
        );
    }
    assert!(looks_like_rust_structural_anchor(&native, "   fn "));
    native.check().unwrap();
}

/// Repeated small range-hinted insertions must charge the large retained tail
/// they move, rather than bypassing the native work allowance via Vec::splice.
/// The authored source is tiny; the actual moved String ownership exceeds 1GiB.
#[test]
fn repeated_small_insertions_charge_large_retained_tail() {
    use super::{ApplyPatchTextFile, apply_patch_hunks_to_file};
    use crate::semantic_patch::{MezPatchOperation, parse_mez_patch};
    let source = format!(
        "*** Begin Patch\n*** Update File: a\n{}*** End Patch",
        "@@ -1,0 +1,1 @@\n+insert\n".repeat(512)
    );
    let patch = parse_mez_patch(&source).unwrap();
    let MezPatchOperation::Update { hunks, .. } = &patch.operations[0] else {
        panic!("fixture must be update");
    };
    let file = ApplyPatchTextFile::from_lines(vec!["tail".to_string(); 120_000], true);
    let checkpoint = || Ok(());
    let work = PatchWorkControl::native(&checkpoint);
    let result = apply_patch_hunks_to_file(&work, "a", file, hunks);
    assert!(
        result.is_err(),
        "bulk retained-tail movement escaped native work accounting"
    );
    assert!(
        result
            .err()
            .unwrap()
            .message()
            .contains("computational work budget")
    );
}
