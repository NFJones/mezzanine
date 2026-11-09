//! Independent unified-hunk replay and deterministic resource/fidelity controls.

use super::*;
use crate::semantic_patch_planning::{ApplyPatchFileChange, ApplyPatchOriginalState};
use std::cell::Cell;

pub(super) fn generate(old: &str, new: &str, budget: usize) -> super::super::NativeUnifiedDiff {
    super::super::native_unified_diff(
        &change(old, new),
        budget,
        &PatchWorkControl::native(&|| Ok(())),
    )
    .unwrap()
}

fn change(old: &str, new: &str) -> ApplyPatchFileChange {
    ApplyPatchFileChange {
        path: "file".into(),
        resolved_path: "/fixture/file".into(),
        original: ApplyPatchOriginalState::Regular(old.as_bytes().to_vec()),
        final_bytes: Some(new.as_bytes().to_vec()),
    }
}

/// Independent standard unified parser checks numbered ranges and every old
/// byte, including CRLF and no-newline markers, while reconstructing new bytes.
pub(super) fn replay(old: &str, source: &str) -> String {
    let original = old.split_inclusive('\n').collect::<Vec<_>>();
    let lines = source.split_inclusive('\n').collect::<Vec<_>>();
    let (mut cursor, mut index, mut new_lines) = (0, 0, 0);
    let mut out = String::new();
    while index < lines.len() {
        if !lines[index].starts_with("@@ ") {
            index += 1;
            continue;
        }
        let header = lines[index].split_whitespace().collect::<Vec<_>>();
        let range = |value: &str| {
            let (start, count) = value[1..].split_once(',').unwrap();
            let count: usize = count.parse().unwrap();
            (
                start.parse::<usize>().unwrap() - usize::from(count > 0),
                count,
            )
        };
        let (a, count_a) = range(header[1]);
        let (b, count_b) = range(header[2]);
        assert!(a >= cursor);
        for text in &original[cursor..a] {
            out.push_str(text);
            new_lines += 1;
        }
        cursor = a;
        assert_eq!(b, new_lines, "{source}");
        index += 1;
        let (mut consumed_a, mut consumed_b) = (0, 0);
        while index < lines.len() && !lines[index].starts_with("@@ ") {
            let line = lines[index];
            let marker = line.as_bytes()[0];
            assert!(matches!(marker, b' ' | b'-' | b'+'));
            let mut text = &line[1..];
            if lines
                .get(index + 1)
                .is_some_and(|line| line.starts_with("\\ No newline"))
            {
                text = text.strip_suffix('\n').unwrap();
                index += 1;
            }
            if marker != b'+' {
                assert_eq!(original[cursor], text);
                cursor += 1;
                consumed_a += 1;
            }
            if marker != b'-' {
                out.push_str(text);
                new_lines += 1;
                consumed_b += 1;
            }
            index += 1;
        }
        assert_eq!((consumed_a, consumed_b), (count_a, count_b), "{source}");
    }
    for text in &original[cursor..] {
        out.push_str(text);
    }
    out
}

#[test]
fn native_focused_diff_context_merge_boundary_and_shifted_ranges() {
    let old = (0..40).map(|i| format!("line-{i}\n")).collect::<String>();
    for gap in [5, 6, 7, 15] {
        let second = 5 + gap + 1;
        let new = old
            .replace("line-5\n", "first\nextra\n")
            .replace(&format!("line-{second}\n"), "second\n");
        let diff = generate(&old, &new, usize::MAX);
        assert_eq!(
            diff.source.lines().filter(|l| l.starts_with("@@ ")).count(),
            if gap <= 6 { 1 } else { 2 },
            "{}",
            diff.source
        );
        assert_eq!(replay(&old, &diff.source), new);
        assert_eq!((diff.added_lines, diff.removed_lines), (3, 2));
        assert_eq!(diff, generate(&old, &new, usize::MAX));
    }
    for (old, new, range) in [
        ("", "x\n", "@@ -0,0 +1,1 @@"),
        ("x\n", "", "@@ -1,1 +0,0 @@"),
        ("a\n", "a\nx\n", "@@ -1,1 +1,2 @@"),
    ] {
        let diff = generate(old, new, usize::MAX);
        assert!(diff.source.contains(range));
        assert_eq!(replay(old, &diff.source), new);
    }
}

#[test]
fn native_focused_diff_raw_fidelity_and_empty_endpoint_identity() {
    for (old, new) in [
        ("", ""),
        ("a\nb", "a\nb\n"),
        ("a\nb\n", "a\nb"),
        ("a", "b"),
        ("a\r\nb\r\n", "a\nb\n"),
        ("a\rb\n\n雪\n", "a\rc\n\n雪\n"),
        ("a\n\n", "a\n"),
    ] {
        let diff = generate(old, new, usize::MAX);
        assert_eq!(replay(old, &diff.source), new);
    }
    for create in [true, false] {
        let mut endpoint = change("", "");
        if create {
            endpoint.original = ApplyPatchOriginalState::Missing;
        } else {
            endpoint.final_bytes = None;
        }
        let diff = super::super::native_unified_diff(
            &endpoint,
            usize::MAX,
            &PatchWorkControl::native(&|| Ok(())),
        )
        .unwrap();
        assert!(diff.source.contains("/dev/null"));
        assert!(diff.source.ends_with("@@ -0,0 +0,0 @@\n"));
        assert_eq!((diff.added_lines, diff.removed_lines), (0, 0));
    }
    let old = format!("{}雪\nlast", "€".repeat(PATCH_WORK_QUANTUM));
    let new = old.replace("last", "changed");
    let diff = generate(&old, &new, usize::MAX);
    assert!(!diff.truncated);
    assert_eq!(replay(&old, &diff.source), new);
}

#[test]
fn native_focused_diff_every_utf8_budget_is_a_truthful_prefix() {
    let complete = generate("a\n雪\nend", "a\n€\nfinish", usize::MAX);
    for budget in 0..=complete.source.len() + 1 {
        let bounded = generate("a\n雪\nend", "a\n€\nfinish", budget);
        assert!(bounded.source.len() <= budget);
        assert!(complete.source.starts_with(&bounded.source));
        assert_eq!(bounded.truncated, budget < complete.source.len());
        assert_eq!((bounded.added_lines, bounded.removed_lines), (2, 2));
    }
}

#[test]
fn native_focused_diff_scratch_stack_line_and_utf8_refusals_are_sticky() {
    for (scratch, stack, expected) in [
        (0, STACK_TASKS, "scratch"),
        (SCRATCH_BYTES, 0, "task stack"),
        (SCRATCH_BYTES, 1, "task stack"),
    ] {
        let work = PatchWorkControl::native(&|| Ok(()));
        let error = Script::with_limits(b"a\nb\nc\n", b"x\nb\ny\n", &work, scratch, stack)
            .err()
            .unwrap();
        assert!(error.message().contains(expected), "{error:?}");
        assert_eq!(work.check().unwrap_err(), error);
    }
    for bytes in [b"\xff".to_vec(), vec![b'\n'; NATIVE_PATCH_TEXT_LINES + 1]] {
        let work = PatchWorkControl::native(&|| Ok(()));
        let error = Script::new(&bytes, b"", &work).err().unwrap();
        assert_eq!(work.check().unwrap_err(), error);
    }
    // Worst admitted line bookkeeping remains within scratch without retaining
    // a frontier history; an all-equal case does not pay adversarial search CPU.
    let bytes = vec![b'\n'; NATIVE_PATCH_TEXT_LINES];
    let script = Script::new(&bytes, &bytes, &PatchWorkControl::native(&|| Ok(()))).unwrap();
    assert_eq!(
        script.runs,
        [Run {
            kind: Kind::Equal,
            len: NATIVE_PATCH_TEXT_LINES as u32
        }]
    );
}

/// Revoking every observed checkpoint exercises indexing, bisection, run
/// assembly and rendering under the same original fence, even at zero budget.
#[test]
fn native_focused_diff_cancellation_at_each_checkpoint_and_after_clipping() {
    let old = (0..120).map(|i| format!("line-{i}\n")).collect::<String>();
    let new = old
        .replace("line-1\n", "start\n")
        .replace("line-118\n", "end\n");
    for budget in [0, 96, usize::MAX] {
        let calls = Cell::new(0);
        let checkpoint = || {
            calls.set(calls.get() + 1);
            Ok(())
        };
        super::super::native_unified_diff(
            &change(&old, &new),
            budget,
            &PatchWorkControl::native(&checkpoint),
        )
        .unwrap();
        for fail_at in 1..=calls.get() {
            let calls = Cell::new(0);
            let checkpoint = || {
                calls.set(calls.get() + 1);
                if calls.get() == fail_at {
                    Err(resource("test original fence revoked"))
                } else {
                    Ok(())
                }
            };
            let work = PatchWorkControl::native(&checkpoint);
            let error =
                super::super::native_unified_diff(&change(&old, &new), budget, &work).unwrap_err();
            assert!(error.message().contains("original fence revoked"));
            assert_eq!(work.check().unwrap_err(), error);
        }
    }
}
