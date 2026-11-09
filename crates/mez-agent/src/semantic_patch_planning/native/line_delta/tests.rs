//! Exact-count oracle and display-independent raw-line regressions.

/// Sparse updates must not spend the preview on remote unchanged lines.
#[test]
fn native_focused_diff_sparse_ten_thousand_lines() {
    let old = (0..10_000)
        .map(|i| format!("line-{i}\n"))
        .collect::<String>();
    for index in [1, 5000, 9998] {
        let new = old.replace(&format!("line-{index}\n"), "replacement\n");
        let change = ApplyPatchFileChange {
            path: "file".into(),
            resolved_path: "/fixture/file".into(),
            original: ApplyPatchOriginalState::Regular(old.as_bytes().to_vec()),
            final_bytes: Some(new.into_bytes()),
        };
        let diff =
            native_unified_diff(&change, 1024, &PatchWorkControl::native(&|| Ok(()))).unwrap();
        assert!(!diff.truncated, "sparse diff unexpectedly truncated");
        assert_eq!((diff.added_lines, diff.removed_lines), (1, 1));
        assert!(
            diff.source
                .contains(&format!("-line-{index}\n+replacement\n"))
        );
        assert!(
            diff.source
                .lines()
                .filter(|line| line.starts_with(' '))
                .count()
                <= 6
        );
        assert!(!diff.source.contains("line-7000\n"));
        assert_eq!(
            diff.source
                .lines()
                .filter(|line| line.starts_with("@@"))
                .count(),
            1
        );
    }
}

use super::*;
use crate::semantic_patch::SemanticPatchPlanningError;
use crate::semantic_patch_planning::{
    ApplyPatchFileChange, ApplyPatchOriginalState, native::native_unified_diff,
};
use std::cell::Cell;

/// Disjoint lines reach the frontier search well before the twentieth bounded
/// checkpoint. Revoking its original owner must return no approximate counts.
#[test]
fn native_line_delta_search_cancellation_is_terminal() {
    let calls = Cell::new(0);
    let cancel = || {
        calls.set(calls.get() + 1);
        if calls.get() == 20 {
            Err(SemanticPatchPlanningError::invalid_args(
                "count search cancelled",
            ))
        } else {
            Ok(())
        }
    };
    let work = PatchWorkControl::native(&cancel);
    let error = count(&"old\n".repeat(1000), &"new\n".repeat(1000), &work).unwrap_err();
    assert_eq!(error.message(), "count search cancelled");
    assert_eq!(calls.get(), 20);
    assert_eq!(count("a\n", "b\n", &work).unwrap_err(), error);
    assert_eq!(calls.get(), 20);
}

/// Exhaustive small repeated-line inputs compare the linear-space frontier to
/// an independent quadratic LCS oracle (only used for these tiny fixtures).
#[test]
fn native_line_delta_matches_exhaustive_lcs_oracle() {
    let mut texts = vec![String::new()];
    for length in 1..=4 {
        for mut ordinal in 0..3usize.pow(length) {
            let mut text = String::new();
            for _ in 0..length {
                text.push_str(["a\n", "b\n", "c\n"][ordinal % 3]);
                ordinal /= 3;
            }
            texts.push(text);
        }
    }
    for old in &texts {
        for new in &texts {
            let a = old.lines().collect::<Vec<_>>();
            let b = new.lines().collect::<Vec<_>>();
            let mut lcs = vec![vec![0; b.len() + 1]; a.len() + 1];
            for (i, left) in a.iter().enumerate() {
                for (j, right) in b.iter().enumerate() {
                    lcs[i + 1][j + 1] = if left == right {
                        lcs[i][j] + 1
                    } else {
                        lcs[i + 1][j].max(lcs[i][j + 1])
                    };
                }
            }
            let common = lcs[a.len()][b.len()];
            assert_eq!(
                count(old, new, &PatchWorkControl::native(&|| Ok(()))).unwrap(),
                (b.len() - common, a.len() - common),
                "old={old:?}, new={new:?}"
            );
            let script = Script::new(
                old.as_bytes(),
                new.as_bytes(),
                &PatchWorkControl::native(&|| Ok(())),
            )
            .unwrap();
            let (mut x, mut y) = (0, 0);
            let mut replay = Vec::new();
            for run in &script.runs {
                for _ in 0..run.len {
                    match run.kind {
                        Kind::Equal => {
                            assert_eq!(script.old.get(x), script.new.get(y));
                            replay.extend_from_slice(script.old.get(x));
                            x += 1;
                            y += 1;
                        }
                        Kind::Delete => x += 1,
                        Kind::Insert => {
                            replay.extend_from_slice(script.new.get(y));
                            y += 1;
                        }
                    }
                }
            }
            assert_eq!((x, y), (a.len(), b.len()));
            assert_eq!(replay, new.as_bytes());
            let diff = super::focused_tests::generate(old, new, usize::MAX);
            assert_eq!(super::focused_tests::replay(old, &diff.source), *new);
        }
    }
}

/// Prefix/suffix-only accounting would overcount the untouched middle here.
/// Display budgets, including zero, must not affect the authoritative counts.
#[test]
fn native_line_delta_sparse_large_file_is_independent_of_preview_budget() {
    let old = (0..10_000)
        .map(|i| format!("line-{i}\n"))
        .collect::<String>();
    for (new, expected) in [
        (old.replace("line-5000\n", "replacement\n"), (1, 1)),
        (old.replace("line-5000\n", "extra\nline-5000\n"), (1, 0)),
        (old.replace("line-5000\n", ""), (0, 1)),
        (
            old.replace("line-2\n", "two\n")
                .replace("line-9998\n", "end\n"),
            (2, 2),
        ),
        (old.clone(), (0, 0)),
    ] {
        for budget in [0, 1, 64, usize::MAX] {
            let change = ApplyPatchFileChange {
                path: "file".into(),
                resolved_path: "/fixture/file".into(),
                original: ApplyPatchOriginalState::Regular(old.as_bytes().to_vec()),
                final_bytes: Some(new.as_bytes().to_vec()),
            };
            let diff = native_unified_diff(&change, budget, &PatchWorkControl::native(&|| Ok(())))
                .unwrap();
            assert_eq!((diff.added_lines, diff.removed_lines), expected);
            if expected == (0, 0) {
                assert!(diff.source.is_empty());
                assert!(!diff.truncated);
            } else if budget == 0 {
                assert!(diff.source.is_empty());
                assert!(diff.truncated);
            }
        }
    }
}

/// Raw terminators are part of each line; normalization or str::lines equality
/// would hide CRLF changes and changes to the final newline.
#[test]
fn native_line_delta_preserves_raw_newline_and_empty_file_semantics() {
    for (old, new, expected) in [
        ("", "", (0, 0)),
        ("", "one\ntwo", (2, 0)),
        ("one\ntwo", "", (0, 2)),
        ("a\r\nb\r\n", "a\nb\n", (2, 2)),
        ("a\nb", "a\nb\n", (1, 1)),
        ("a\nb\n", "a\nb", (1, 1)),
        ("雪\n\nlast", "雪\nlast", (0, 1)),
    ] {
        assert_eq!(
            count(old, new, &PatchWorkControl::native(&|| Ok(()))).unwrap(),
            expected
        );
    }
}
