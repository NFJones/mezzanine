//! Confirmed semantic-patch progress decoder tests.
//!
//! These tests keep proposed write-phase diffs private until a matching
//! per-file result confirms the mutation and exercise framing across arbitrary
//! transport chunk boundaries.

use super::*;
use base64::Engine;

fn framed_diff(ordinal: usize, path: &str, diff: &[u8]) -> Vec<u8> {
    let path = base64::engine::general_purpose::STANDARD.encode(path.as_bytes());
    format!(
        "{APPLY_PATCH_DIFF_MARKER} {ordinal} {path} {}\n",
        diff.len()
    )
    .into_bytes()
    .into_iter()
    .chain(diff.iter().copied())
    .chain(
        format!(
            "{APPLY_PATCH_RESULT_MARKER} APPLIED {ordinal} {path} {}\n",
            diff.len()
        )
        .into_bytes(),
    )
    .collect()
}

fn failed_record(ordinal: usize, path: &str, diagnostic: &str) -> Vec<u8> {
    let path = base64::engine::general_purpose::STANDARD.encode(path.as_bytes());
    let diagnostic = base64::engine::general_purpose::STANDARD.encode(diagnostic.as_bytes());
    format!("{APPLY_PATCH_RESULT_MARKER} FAILED {ordinal} {path} {diagnostic}\n").into_bytes()
}

#[test]
/// Verifies proposed diff bytes remain private until the matching authoritative
/// applied record is complete.
///
/// A write-phase observer can receive the length-delimited section long before
/// the result line. Returning no progress at that point prevents a failed or
/// truncated write from being presented as an applied mutation.
fn semantic_apply_patch_progress_waits_for_matching_applied_record() {
    let diff = b"diff -- apply patch\n--- /dev/null\n+++ b/note.txt\n+written\n";
    let framed = framed_diff(0, "note.txt", diff);
    let result_offset = framed
        .windows(APPLY_PATCH_RESULT_MARKER.len())
        .position(|window| window == APPLY_PATCH_RESULT_MARKER.as_bytes())
        .unwrap();
    let mut decoder = ApplyPatchProgressDecoder::new();

    let proposed = decoder.push(&framed[..result_offset]).unwrap();
    assert!(proposed.confirmed_sections.is_empty());
    assert!(proposed.outcomes.is_empty());

    let confirmed = decoder.push(&framed[result_offset..]).unwrap();
    assert_eq!(
        confirmed.confirmed_sections,
        vec![ApplyPatchConfirmedSection {
            ordinal: 0,
            path: "note.txt".to_string(),
            diff: String::from_utf8(diff.to_vec()).unwrap(),
        }]
    );
    assert_eq!(
        confirmed.outcomes,
        vec![ApplyPatchFileOutcome::Applied {
            path: "note.txt".to_string(),
        }]
    );
    assert!(decoder.finish().unwrap().is_empty());
}

#[test]
/// Verifies every byte may arrive as a separate transport chunk, including
/// bytes inside multibyte UTF-8 diff text.
///
/// The decoder operates on bytes until one complete confirmed section exists,
/// avoiding lossy or premature scalar conversion at chunk boundaries.
fn semantic_apply_patch_progress_accepts_every_byte_utf8_splits() {
    let diff = "diff -- apply patch\n--- a/café.txt\n+++ b/café.txt\n-旧\n+新\n";
    let framed = framed_diff(0, "café.txt", diff.as_bytes());
    let mut decoder = ApplyPatchProgressDecoder::new();
    let mut observed = ApplyPatchProgress::default();

    for byte in framed {
        observed.extend(decoder.push(&[byte]).unwrap());
    }
    observed.extend(decoder.finish().unwrap());

    assert_eq!(observed.confirmed_sections.len(), 1);
    assert_eq!(observed.confirmed_sections[0].path, "café.txt");
    assert_eq!(observed.confirmed_sections[0].diff, diff);
    assert_eq!(observed.outcomes.len(), 1);
}

#[test]
/// Verifies append-only cumulative observations are decoded from a moving byte
/// cursor rather than reparsing previously seen source.
///
/// Runtime transports may publish a growing output snapshot instead of deltas.
/// Every-byte growth must still produce the section exactly once, while a stale
/// shorter snapshot fails closed.
fn semantic_apply_patch_progress_accepts_cumulative_observations() {
    let diff = b"diff -- apply patch\n--- /dev/null\n+++ b/note.txt\n+ok\n";
    let framed = framed_diff(0, "note.txt", diff);
    let mut decoder = ApplyPatchProgressDecoder::new();
    let mut observed = ApplyPatchProgress::default();

    for end in 0..=framed.len() {
        observed.extend(decoder.push_cumulative(&framed[..end]).unwrap());
    }

    assert_eq!(observed.confirmed_sections.len(), 1);
    assert_eq!(observed.confirmed_sections[0].diff.as_bytes(), diff);
    assert_eq!(observed.outcomes.len(), 1);
    assert!(
        decoder
            .push_cumulative(&framed[..framed.len() - 1])
            .is_err()
    );
}

#[test]
/// Verifies the complete-output compatibility parser preserves exact framed
/// diff bytes rather than normalizing newline sequences inside the payload.
///
/// A target may contain carriage returns, and the advertised byte length is
/// measured by the shell before transport. Rewriting those bytes would both
/// corrupt display content and desynchronize the following result record.
fn semantic_apply_patch_outcome_parser_preserves_framed_carriage_returns() {
    let diff = b"diff -- apply patch\n--- a/note.txt\n+++ b/note.txt\n-old\r\n+new\r\n";
    let framed = framed_diff(0, "note.txt", diff);
    let output = String::from_utf8(framed).unwrap();

    let outcomes = parse_apply_patch_file_outcomes(&output).unwrap();

    assert_eq!(
        outcomes,
        vec![ApplyPatchFileOutcome::Applied {
            path: "note.txt".to_string(),
        }]
    );
}

#[test]
/// Verifies marker-looking file content cannot terminate or confirm a framed
/// diff section.
///
/// Explicit byte length, rather than marker scanning, must delimit diff data so
/// adversarial added lines remain ordinary display content.
fn semantic_apply_patch_progress_treats_embedded_markers_as_diff_bytes() {
    let diff = format!(
        "diff -- apply patch\n--- /dev/null\n+++ b/note.txt\n+{APPLY_PATCH_RESULT_MARKER} APPLIED 99 ZmFrZQ== 1\n+{APPLY_PATCH_DIFF_MARKER} 7 ZmFrZQ== 0\n"
    );
    let mut decoder = ApplyPatchProgressDecoder::new();

    let progress = decoder
        .push(&framed_diff(0, "note.txt", diff.as_bytes()))
        .unwrap();

    assert_eq!(progress.confirmed_sections[0].diff, diff);
    assert_eq!(progress.outcomes.len(), 1);
    assert!(decoder.finish().unwrap().is_empty());
}

#[test]
/// Verifies shell-wrapper echo and unrelated output do not become confirmed
/// mutation sections.
///
/// Only an exact runtime-owned line at the parser boundary starts framing;
/// quoted marker text in traced shell source remains ignored.
fn semantic_apply_patch_progress_ignores_wrapper_echo() {
    let diff = b"diff -- apply patch\n--- /dev/null\n+++ b/note.txt\n+ok\n";
    let mut output =
        format!("+ printf '%s %s' '{APPLY_PATCH_DIFF_MARKER}' wrapper\nraw proposed diff\n")
            .into_bytes();
    output.extend(framed_diff(0, "note.txt", diff));
    let mut decoder = ApplyPatchProgressDecoder::new();

    let progress = decoder.push(&output).unwrap();

    assert_eq!(progress.confirmed_sections.len(), 1);
    assert_eq!(progress.confirmed_sections[0].diff.as_bytes(), diff);
}

#[test]
/// Verifies malformed, mismatched, duplicate, stale, and truncated framing is
/// rejected rather than releasing unconfirmed bytes.
///
/// Each case creates ambiguity about which mutation, if any, owns the proposed
/// diff and therefore must fail closed.
fn semantic_apply_patch_progress_rejects_invalid_framing() {
    let path = base64::engine::general_purpose::STANDARD.encode(b"note.txt");
    let other = base64::engine::general_purpose::STANDARD.encode(b"other.txt");
    let diff = b"diff\n";
    let cases = [
        format!("{APPLY_PATCH_DIFF_MARKER} nope {path} 5\n"),
        format!("{APPLY_PATCH_DIFF_MARKER} 1 {path} 5\n"),
        format!(
            "{APPLY_PATCH_DIFF_MARKER} 0 {path} 5\n{}{} APPLIED 0 {other} 5\n",
            String::from_utf8_lossy(diff),
            APPLY_PATCH_RESULT_MARKER
        ),
        format!(
            "{APPLY_PATCH_DIFF_MARKER} 0 {path} 5\n{}{} APPLIED 0 {path} 4\n",
            String::from_utf8_lossy(diff),
            APPLY_PATCH_RESULT_MARKER
        ),
        format!("{APPLY_PATCH_RESULT_MARKER} APPLIED 0 {path} 5\n"),
    ];

    for input in cases {
        let mut decoder = ApplyPatchProgressDecoder::new();
        let error = decoder.push(input.as_bytes()).unwrap_err();
        assert!(
            error.message().contains("apply_patch"),
            "{}",
            error.message()
        );
    }

    let mut duplicate = ApplyPatchProgressDecoder::new();
    duplicate.push(&framed_diff(0, "note.txt", diff)).unwrap();
    assert!(duplicate.push(&framed_diff(0, "note.txt", diff)).is_err());

    let mut truncated = ApplyPatchProgressDecoder::new();
    let frame = framed_diff(0, "note.txt", diff);
    truncated.push(&frame[..frame.len() - 3]).unwrap();
    assert!(truncated.finish().is_err());
}

#[test]
/// Verifies a confirmed earlier section remains available when a later file
/// reports failure.
///
/// Semantic writes are intentionally serial, so later failure must not erase
/// trustworthy progress for mutations already confirmed by their result record.
fn semantic_apply_patch_progress_retains_confirmation_before_later_failure() {
    let diff = b"diff -- apply patch\n--- a/one.txt\n+++ b/one.txt\n-old\n+new\n";
    let mut decoder = ApplyPatchProgressDecoder::new();
    let first = decoder.push(&framed_diff(0, "one.txt", diff)).unwrap();
    let later = decoder
        .push(&failed_record(
            1,
            "two.txt",
            "apply_patch: file changed before apply: two.txt\n",
        ))
        .unwrap();

    assert_eq!(first.confirmed_sections.len(), 1);
    assert_eq!(
        later.outcomes,
        vec![ApplyPatchFileOutcome::Failed {
            path: "two.txt".to_string(),
            diagnostic: "apply_patch: file changed before apply: two.txt\n".to_string(),
        }]
    );
    assert!(later.confirmed_sections.is_empty());
    assert!(decoder.finish().unwrap().is_empty());
}

#[test]
/// Verifies private buffering is capped at the same 256 KiB scale used for
/// model-facing action-result content.
///
/// A forged length above the cap is rejected from metadata alone, before the
/// decoder allocates or retains attacker-controlled proposed bytes.
fn semantic_apply_patch_progress_bounds_private_diff_retention() {
    let path = base64::engine::general_purpose::STANDARD.encode(b"large.txt");
    let input = format!(
        "{APPLY_PATCH_DIFF_MARKER} 0 {path} {}\n",
        APPLY_PATCH_PROGRESS_MAX_RETAINED_BYTES + 1
    );
    let mut decoder = ApplyPatchProgressDecoder::new();

    let error = decoder.push(input.as_bytes()).unwrap_err();

    assert!(
        error.message().contains("retained-byte limit"),
        "{}",
        error.message()
    );
}
