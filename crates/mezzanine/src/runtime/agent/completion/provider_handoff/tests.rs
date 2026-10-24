//! Pure once-only custody-state regressions; no worker or admission execution.

use super::super::retirement::CompletionCapture;
use std::cell::Cell;

/// An observed empty original component is not an invitation to sample a
/// replacement later. The second closure must not run at all, not merely
/// lose a race to an already populated Option.
#[test]
fn captured_absence_never_recaptures_a_later_component() {
    let mut source: CompletionCapture<Option<String>> = CompletionCapture::NotCaptured;
    source.capture_once(|| None);
    let later_reads = Cell::new(0);
    source.capture_once(|| {
        later_reads.set(1);
        Some("replacement".into())
    });
    assert!(matches!(source, CompletionCapture::Captured(None)));
    assert_eq!(later_reads.get(), 0);
}

/// Completing a remaining component must not overwrite an earlier source
/// or refill a captured-empty vector. This is a pure custody-state test,
/// not claim admission, worker execution or a whole negative-settlement proof.
#[test]
fn partial_capture_completes_only_unobserved_components_once() {
    let mut execution = CompletionCapture::Captured(Some("original".to_string()));
    let mut context: CompletionCapture<Option<String>> = CompletionCapture::NotCaptured;
    let mut pending: CompletionCapture<Vec<String>> = CompletionCapture::Captured(Vec::new());
    execution.capture_once(|| Some("later execution".into()));
    context.capture_once(|| Some("original context".into()));
    pending.capture_once(|| vec!["later pending work".into()]);
    assert_eq!(execution.as_ref().map(String::as_str), Some("original"));
    assert_eq!(
        context.as_ref().map(String::as_str),
        Some("original context")
    );
    assert!(matches!(pending, CompletionCapture::Captured(rows) if rows.is_empty()));
}
