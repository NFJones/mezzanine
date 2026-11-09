//! All-endpoint preparation fails before any endpoint, while actual commit
//! failures retain only the positive focused source and unchanged authority.

use super::*;

use std::cell::RefCell;

/// Synchronous thread-local instrumentation never replaces a work control,
/// deadline or authority. It only revokes the exact request's real fence.
#[derive(Default)]
struct DiffProbe {
    current: Option<(usize, usize)>,
    visited: Vec<(usize, usize)>,
    remaining: usize,
    triggered: bool,
}

thread_local! {
    static DIFF_PROBE: RefCell<Option<DiffProbe>> = const { RefCell::new(None) };
}

pub(in super::super) struct DiffScope;

impl Drop for DiffScope {
    fn drop(&mut self) {
        DIFF_PROBE.with_borrow_mut(|probe| {
            if let Some(probe) = probe {
                probe.current = None;
            }
        });
    }
}

pub(in super::super) fn enter_diff(operation: usize, endpoint: usize) -> DiffScope {
    DIFF_PROBE.with_borrow_mut(|probe| {
        if let Some(probe) = probe {
            probe.current = Some((operation, endpoint));
            probe.visited.push((operation, endpoint));
        }
    });
    DiffScope
}

pub(in super::super) fn diff_checkpoint(fence: &NativePatchCommitFence) {
    DIFF_PROBE.with_borrow_mut(|probe| {
        if let Some(probe) = probe
            && probe.current == Some((2, 0))
            && !probe.triggered
        {
            probe.remaining -= 1;
            if probe.remaining == 0 {
                fence.cancel();
                probe.triggered = true;
            }
        }
    });
}

struct ProbeOwner;
impl Drop for ProbeOwner {
    fn drop(&mut self) {
        DIFF_PROBE.with_borrow_mut(|probe| *probe = None);
    }
}

/// All three operations plan successfully. The first update and missing-parent
/// create finish diff preparation; only the third diff revokes its original
/// fence during line indexing. No earlier mutation/lease/stage is permitted.
#[test]
fn native_focused_diff_later_calculation_cancellation_precedes_every_mutation() {
    let fixture = Fixture::new();
    fs::write(fixture.directory.join("a"), "old\n").unwrap();
    fs::write(fixture.directory.join("z"), "old\n").unwrap();
    let patch = "*** Begin Patch\n*** Update File: $ROOT/a\n@@\n-old\n+new\n*** Add File: $ROOT/missing/file\n+created\n*** Update File: $ROOT/z\n@@\n-old\n+new\n*** End Patch";
    DIFF_PROBE.with_borrow_mut(|probe| {
        *probe = Some(DiffProbe {
            remaining: 3,
            ..Default::default()
        })
    });
    let owner = ProbeOwner;
    let request = fixture.request(patch);
    let fence = request.fence.clone();
    let mut commits = 0;
    let mut progress = 0;
    let outcome = execute_native_patch(
        request,
        |_, phase, _| {
            if phase == NativePatchPhase::Commit {
                commits += 1;
            }
            Ok(fixture.scopes.clone())
        },
        |_| progress += 1,
    );
    DIFF_PROBE.with_borrow(|probe| {
        let probe = probe.as_ref().unwrap();
        assert_eq!(probe.visited, [(0, 0), (1, 0), (2, 0)]);
        assert!(probe.triggered);
        assert_eq!(probe.remaining, 0);
        assert!(probe.current.is_none(), "scope retires on the error path");
    });
    drop(owner);
    assert!(fence.check().is_err());
    assert_eq!(outcome.result.phase, NativePatchPhase::Plan);
    assert_eq!(outcome.result.certainty, NativeEffectCertainty::Unexecuted);
    assert!(
        outcome
            .result
            .diagnostic
            .as_ref()
            .unwrap()
            .contains("cancel")
    );
    assert!(outcome.receipts.is_empty());
    assert!(outcome.completed_operations.is_empty());
    assert_eq!((commits, progress), (0, 0));
    assert_eq!(fs::read(fixture.directory.join("a")).unwrap(), b"old\n");
    assert_eq!(fs::read(fixture.directory.join("z")).unwrap(), b"old\n");
    assert!(!fixture.directory.join("missing").exists());
    fixture.assert_no_stages();
    // A separate newly admitted operation can succeed; the old owner is never
    // reset or retried, and the test hook cannot affect another request/thread.
    let fresh = fixture.run(patch);
    assert_eq!(fresh.result.certainty, NativeEffectCertainty::Confirmed);
    assert_eq!(fs::read(fixture.directory.join("a")).unwrap(), b"new\n");
    assert_eq!(fs::read(fixture.directory.join("z")).unwrap(), b"new\n");
    assert_eq!(
        fs::read(fixture.directory.join("missing/file")).unwrap(),
        b"created\n"
    );
    fixture.assert_no_stages();
}

#[test]
fn native_focused_diff_preparation_failure_keeps_all_targets_and_parents_unchanged() {
    let fixture = Fixture::new();
    let old = vec![b'\n'; mez_agent::semantic_patch_planning::work::NATIVE_PATCH_TEXT_LINES + 1];
    fs::write(fixture.directory.join("delete"), &old).unwrap();
    let patch = "*** Begin Patch\n*** Add File: $ROOT/missing/file\n+new\n*** Delete File: $ROOT/delete\n*** End Patch";
    let outcome = fixture.run(patch);
    assert_eq!(outcome.result.certainty, NativeEffectCertainty::Unexecuted);
    assert!(
        outcome
            .result
            .diagnostic
            .as_ref()
            .unwrap()
            .contains("native text line budget exceeded"),
        "{:?}",
        outcome.result.diagnostic
    );
    assert!(outcome.receipts.is_empty());
    assert!(!fixture.directory.join("missing").exists());
    assert_eq!(fs::read(fixture.directory.join("delete")).unwrap(), old);
    fixture.assert_no_stages();
    // A new bounded request with fresh original authority remains useful.
    fs::write(fixture.directory.join("delete"), "old\n").unwrap();
    let outcome = fixture.run(patch);
    assert_eq!(outcome.result.certainty, NativeEffectCertainty::Confirmed);
    assert_eq!(
        fs::read(fixture.directory.join("missing/file")).unwrap(),
        b"new\n"
    );
    assert!(!fixture.directory.join("delete").exists());
    fixture.assert_no_stages();
}

#[test]
fn native_focused_diff_commit_failure_retains_actual_positive_prefix() {
    let fixture = Fixture::new();
    let old = (0..10_000)
        .map(|i| format!("line-{i}\n"))
        .collect::<String>();
    fs::write(fixture.directory.join("a"), &old).unwrap();
    let request = fixture.request("*** Begin Patch\n*** Update File: $ROOT/a\n@@\n-line-5000\n+replacement\n*** Add File: $ROOT/b\n+uncommitted\n*** End Patch");
    let outcome = execute_native_patch(
        request,
        |_, phase, path| {
            if phase == NativePatchPhase::Commit && std::path::Path::new(path).ends_with("b") {
                return Err(MezError::forbidden("second commit revoked"));
            }
            Ok(fixture.scopes.clone())
        },
        |_| {},
    );
    assert_eq!(outcome.result.certainty, NativeEffectCertainty::Partial);
    assert_eq!(outcome.receipts.len(), 1);
    let receipt = &outcome.receipts[0];
    assert_eq!((receipt.added_lines, receipt.removed_lines), (1, 1));
    assert!(receipt.diff.contains("@@ -4998,7 +4998,7 @@"));
    assert!(!receipt.diff.contains("line-7000\n"));
    assert!(
        outcome
            .result
            .diagnostic
            .unwrap()
            .contains("second commit revoked")
    );
    assert_eq!(
        fs::read_to_string(fixture.directory.join("a")).unwrap(),
        old.replace("line-5000\n", "replacement\n")
    );
    assert!(!fixture.directory.join("b").exists());
    fixture.assert_no_stages();
}
