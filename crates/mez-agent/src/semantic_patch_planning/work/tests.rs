//! Cooperative CPU work at real shared matching and native diff boundaries.
//!
//! Fixtures use actual byte scans/copies and semantic planners. Revocation is an
//! injected original fence, never a permission grant or a process timeout.

use super::*;
use crate::native_action::NativePatchOperation;
use crate::semantic_patch_planning::native::{native_unified_diff, plan_native_patch};
use crate::semantic_patch_planning::{
    ApplyPatchFileChange, ApplyPatchOriginalState, ApplyPatchSnapshot, ApplyPatchSnapshotState,
};
use std::collections::BTreeMap;

/// Chunked UTF-8 decoding must preserve characters crossing a quantum boundary,
/// split CRLF exactly like str::lines, and reject malformed boundary sequences.
#[test]
fn bounded_text_preserves_utf8_and_line_semantics() {
    let checkpoint = || Ok(());
    let work = PatchWorkControl::native(&checkpoint);
    let text = format!(
        "{}€\r\nsecond\r\n\nlast\r",
        "x".repeat(PATCH_WORK_QUANTUM - 1)
    );
    assert_eq!(work.decode_text(text.as_bytes()).unwrap(), text);
    assert_eq!(
        work.text_lines(&text).unwrap(),
        text.lines().map(str::to_string).collect::<Vec<_>>()
    );
    for bytes in [&b"\xff"[..], &b"valid\xe2\x82"[..]] {
        assert!(work.decode_text(bytes).is_err());
    }
    assert!(work.text_lines("").unwrap().is_empty());
}

/// Literal anchor matching is linear even for repeated near-matches. It must
/// agree with Rust's literal substring relation, including overlapping prefixes,
/// Unicode and empty needles; cancellation remains terminal on the same owner.
#[test]
fn controlled_anchor_search_preserves_literal_matches_and_stops() {
    let checkpoint = || Ok(());
    let work = PatchWorkControl::native(&checkpoint);
    for text in ["", "abababac", "αβγαβδ", "aaaaabaaaaab"] {
        for needle in ["", "abac", "βδ", "aaaaab", "missing"] {
            assert_eq!(work.contains(text, needle), text.contains(needle));
        }
    }
    let calls = Cell::new(0);
    let cancel = || {
        calls.set(calls.get() + 1);
        if calls.get() >= 3 {
            Err(SemanticPatchPlanningError::invalid_args(
                "original CPU fence revoked",
            ))
        } else {
            Ok(())
        }
    };
    let work = PatchWorkControl::native(&cancel);
    assert!(!work.contains(&"a".repeat(PATCH_WORK_QUANTUM * 8), "aaaaab"));
    assert_eq!(calls.get(), 3);
    assert!(
        work.check()
            .unwrap_err()
            .message()
            .contains("fence revoked")
    );
    assert!(!work.equal("", ""));
}

/// Native text bookkeeping and cumulative work are finite independently of
/// wall-clock cancellation. Failure is sticky; neither a new mode nor successful
/// later callback can renew the exhausted allowance or return a partial copy.
#[test]
fn native_resource_exhaustion_is_sticky() {
    let checkpoint = || Ok(());
    let work = PatchWorkControl::native(&checkpoint);
    work.remaining.set(PATCH_WORK_QUANTUM);
    assert!(
        work.copy_bytes(&vec![b'x'; PATCH_WORK_QUANTUM + 1])
            .is_err()
    );
    assert!(!work.contains("abc", "a"));
    assert!(
        work.check()
            .unwrap_err()
            .message()
            .contains("computational work")
    );
    let work = PatchWorkControl::native(&checkpoint);
    assert!(
        work.text_lines(&"\n".repeat(NATIVE_PATCH_TEXT_LINES + 1))
            .unwrap_err()
            .message()
            .contains("line budget")
    );
}

/// A native planner must return the original terminal error rather than a plan
/// whose repairable per-operation errors hide cancellation and permit earlier
/// planned endpoints to be committed. Multiple semantic operations are real.
#[test]
fn native_plan_cancellation_never_returns_partial_plans() {
    let operation = NativePatchOperation::parse("*** Begin Patch\n*** Add File: added\n+new\n*** Update File: large\n@@\n-old\n+new\n*** End Patch", None).unwrap();
    let snapshots = BTreeMap::from([
        (
            "added".to_string(),
            ApplyPatchSnapshot {
                path: "added".into(),
                resolved_path: "/fixture/added".into(),
                state: ApplyPatchSnapshotState::Missing,
            },
        ),
        (
            "large".to_string(),
            ApplyPatchSnapshot {
                path: "large".into(),
                resolved_path: "/fixture/large".into(),
                state: ApplyPatchSnapshotState::Regular("old\n".repeat(32_768).into_bytes()),
            },
        ),
    ]);
    let calls = Cell::new(0);
    let cancel = || {
        calls.set(calls.get() + 1);
        if calls.get() >= 40 {
            Err(SemanticPatchPlanningError::invalid_args(
                "terminal original planning fence",
            ))
        } else {
            Ok(())
        }
    };
    let work = PatchWorkControl::native(&cancel);
    let error = plan_native_patch(&operation, &snapshots, &work).unwrap_err();
    assert!(error.message().contains("terminal original planning fence"));
    assert_eq!(calls.get(), 40);
}

/// Full raw line counts survive a zero display budget and long Unicode lines.
/// Cancellation during a diff's actual full-input scan returns no display result
/// that could be mistaken for a positively committed or complete endpoint.
#[test]
fn native_diff_counts_are_prepared_cooperatively_before_commit() {
    let change = ApplyPatchFileChange {
        path: "a".into(),
        resolved_path: "/fixture/a".into(),
        original: ApplyPatchOriginalState::Regular(b"old\nsecond".to_vec()),
        final_bytes: Some(format!("{}\nlast", "€".repeat(PATCH_WORK_QUANTUM)).into_bytes()),
    };
    let checkpoint = || Ok(());
    let work = PatchWorkControl::native(&checkpoint);
    let diff = native_unified_diff(&change, 0, &work).unwrap();
    assert_eq!((diff.old_lines, diff.new_lines), (2, 2));
    assert!(diff.truncated);
    let diff = native_unified_diff(&change, usize::MAX, &work).unwrap();
    assert!(!diff.truncated);
    assert!(
        diff.source
            .ends_with("+last\n\\ No newline at end of file\n")
    );
    assert_eq!(diff.source.matches("+last").count(), 1);
    let calls = Cell::new(0);
    let cancel = || {
        calls.set(calls.get() + 1);
        if calls.get() >= 3 {
            Err(SemanticPatchPlanningError::invalid_args(
                "diff fence revoked",
            ))
        } else {
            Ok(())
        }
    };
    let work = PatchWorkControl::native(&cancel);
    assert!(
        native_unified_diff(&change, usize::MAX, &work)
            .unwrap_err()
            .message()
            .contains("diff fence revoked")
    );
}

/// Oversized inert labels must fail before escaping/allocation just like file
/// inputs. Display generation does not accept an unbounded secondary payload.
#[test]
fn native_diff_rejects_oversized_label() {
    let change = ApplyPatchFileChange {
        path: "x".repeat(4097),
        resolved_path: "/fixture/a".into(),
        original: ApplyPatchOriginalState::Missing,
        final_bytes: Some(b"new".to_vec()),
    };
    let checkpoint = || Ok(());
    assert!(native_unified_diff(&change, 1024, &PatchWorkControl::native(&checkpoint)).is_err());
}

/// A pane adapter must not inherit the native text-line limit merely because
/// both adapters share parsing and matching helpers. This uses actual line
/// construction beyond that boundary, without filesystem or process effects.
#[test]
fn pane_control_preserves_existing_text_line_policy() {
    let lines = PatchWorkControl::unlimited()
        .text_lines(&"\n".repeat(NATIVE_PATCH_TEXT_LINES + 1))
        .unwrap();
    assert_eq!(lines.len(), NATIVE_PATCH_TEXT_LINES + 1);
}

/// Semantic descriptors must preserve every parsed field through controlled
/// copying, and a large parsed line must observe revocation during its copy
/// rather than finishing an uncharged deep Clone of the operation.
#[test]
fn descriptor_copies_preserve_fields_and_observe_original_control() {
    let operation = NativePatchOperation::parse(
        &format!(
            "*** Begin Patch\n*** Add File: a\n+{}\n*** End Patch",
            "x".repeat(PATCH_WORK_QUANTUM * 4)
        ),
        None,
    )
    .unwrap();
    let checkpoint = || Ok(());
    let work = PatchWorkControl::native(&checkpoint);
    assert_eq!(work.copy_native_operation(&operation).unwrap(), operation);
    let calls = Cell::new(0);
    let cancel = || {
        calls.set(calls.get() + 1);
        if calls.get() >= 8 {
            Err(SemanticPatchPlanningError::invalid_args(
                "parsed operation copy cancelled",
            ))
        } else {
            Ok(())
        }
    };
    let work = PatchWorkControl::native(&cancel);
    assert!(
        work.copy_parsed_operation(&operation.patch.operations[0])
            .unwrap_err()
            .message()
            .contains("parsed operation copy cancelled")
    );
    assert_eq!(calls.get(), 8);
}

/// Native working-tail transfers must consult the original fence while moving
/// actual retained String owners. An abandoned working buffer is discarded;
/// resource cleanup cannot turn cancellation into a usable partial result.
#[test]
fn retained_owner_transfer_stops_inside_the_original_fence() {
    let calls = Cell::new(0);
    let cancel = || {
        calls.set(calls.get() + 1);
        if calls.get() >= 3 {
            Err(SemanticPatchPlanningError::invalid_args(
                "retained tail transfer cancelled",
            ))
        } else {
            Ok(())
        }
    };
    let work = PatchWorkControl::native(&cancel);
    let mut lines = vec!["retained".to_string(); 120_000];
    let error = work
        .replace_lines(&mut lines, 0..0, vec!["new".into()])
        .unwrap_err();
    assert!(error.message().contains("retained tail transfer cancelled"));
    assert_eq!(calls.get(), 3);
    assert!(lines.is_empty());
}
