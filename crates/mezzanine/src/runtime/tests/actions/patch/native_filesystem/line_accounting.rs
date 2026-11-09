//! Actual native worker receipts -> canonical completion -> deferred listing.
//!
//! The original confirmation/phase guards, not a reconstructed diff or test
//! counter, own attribution. Every case also retries projection to prove that
//! positive deltas cannot be accumulated twice.

use super::canonical::execute_and_retain;
use super::*;

fn large_file() -> String {
    (0..10_000).map(|i| format!("line-{i}\n")).collect()
}

/// Exercises command admission and its shared deferred formatter, not just the
/// retained map or a helper-generated synthetic listing.
fn listing(fixture: &mut Fixture) -> String {
    let primary = fixture
        .service
        .attach_primary("accounting", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    fixture
        .service
        .execute_agent_shell_command(&primary, "/list-modified-files")
        .unwrap();
    let response = fixture
        .service
        .run_pending_deferred_agent_command_for_tests()
        .unwrap()
        .unwrap();
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    response["body"].as_str().unwrap().to_owned()
}

fn assert_row(body: &str, path: &std::path::Path, added: usize, removed: usize) {
    let row = format!(
        "- edited `{}` (<span class=\"mez-diff-addition\">+{added}</span> <span class=\"mez-diff-deletion\">-{removed}</span>)",
        path.display()
    );
    assert!(body.contains(&row), "missing {row:?} in {body}");
}

fn project_once(
    fixture: &mut Fixture,
    attempt: &crate::runtime::agent::native_filesystem::NativeFilesystemAttempt,
) {
    assert!(
        fixture
            .service
            .project_native_filesystem_completion(attempt)
            .unwrap()
    );
    assert!(
        !fixture
            .service
            .project_native_filesystem_completion(attempt)
            .unwrap()
    );
    assert_eq!(super::canonical::result_events(fixture), 1);
}

#[test]
fn native_line_accounting_one_line_in_ten_thousand_reaches_receipt_and_listing() {
    let mut fixture = Fixture::with_patch(false, |root| {
        format!(
            "*** Begin Patch\n*** Update File: {}\n@@\n-line-5000\n+replacement\n*** End Patch",
            root.join("file").display()
        )
    });
    fs::write(fixture.root.join("file"), large_file()).unwrap();
    let (attempt, raw) = execute_and_retain(&mut fixture, false);
    assert_eq!(raw.result.certainty, NativeEffectCertainty::Confirmed);
    assert_eq!(raw.receipts.len(), 1);
    assert_eq!(
        (raw.receipts[0].added_lines, raw.receipts[0].removed_lines),
        (1, 1)
    );
    project_once(&mut fixture, &attempt);
    let result = &fixture.service.agent_turn_executions()[&fixture.turn.turn_id].action_results[0];
    let metadata: serde_json::Value =
        serde_json::from_str(result.structured_content_json.as_ref().unwrap()).unwrap();
    assert_eq!(metadata["confirmed_endpoints"][0]["added_lines"], 1);
    assert_eq!(metadata["confirmed_endpoints"][0]["removed_lines"], 1);
    let body = listing(&mut fixture);
    assert_row(&body, &fixture.root.join("file"), 1, 1);
}

/// A replacement and its inverse still count as two actual operations, not
/// zero net working-tree change. Insert/delete/no-op/create/delete and directory
/// effects share that same real receipt and completion path.
#[test]
fn native_line_accounting_repeated_edits_and_file_effects_accumulate_once() {
    let mut fixture = Fixture::with_patch(false, |root| {
        let file = root.join("file");
        format!(
            "*** Begin Patch\n*** Update File: {file}\n@@\n-line-5000\n+replacement\n*** Update File: {file}\n@@\n-replacement\n+line-5000\n*** Update File: {file}\n@@\n line-1234\n+insert-one\n+insert-two\n*** Update File: {file}\n@@\n-line-2345\n-line-2346\n*** Update File: {file}\n@@\n line-10\n*** Add File: {}\n+one\n+two\n*** Delete File: {}\n*** End Patch",
            root.join("nested/created").display(),
            root.join("deleted").display(),
            file = file.display()
        )
    });
    fs::write(fixture.root.join("file"), large_file()).unwrap();
    fs::write(fixture.root.join("deleted"), "one\ntwo\nthree").unwrap();
    let (attempt, raw) = execute_and_retain(&mut fixture, false);
    assert_eq!(raw.result.certainty, NativeEffectCertainty::Confirmed);
    assert_eq!(raw.completed_operations, [0, 1, 2, 3, 4, 5, 6]);
    assert_eq!(
        raw.receipts
            .iter()
            .filter(|r| r.is_file())
            .map(|r| (r.operation_ordinal, r.added_lines, r.removed_lines))
            .collect::<Vec<_>>(),
        [
            (0, 1, 1),
            (1, 1, 1),
            (2, 2, 0),
            (3, 0, 2),
            (5, 2, 0),
            (6, 0, 3)
        ]
    );
    let directory = raw.receipts.iter().find(|r| !r.is_file()).unwrap();
    assert_eq!((directory.added_lines, directory.removed_lines), (0, 0));
    project_once(&mut fixture, &attempt);
    let body = listing(&mut fixture);
    assert_row(&body, &fixture.root.join("file"), 4, 4);
    assert_row(&body, &fixture.root.join("nested/created"), 2, 0);
    assert_row(&body, &fixture.root.join("deleted"), 0, 3);
    assert!(!body.contains(&format!(
        "edited `{}`",
        fixture.root.join("nested").display()
    )));
    let expected = large_file()
        .replace("line-1234\n", "line-1234\ninsert-one\ninsert-two\n")
        .replace("line-2345\nline-2346\n", "");
    assert_eq!(
        fs::read_to_string(fixture.root.join("file")).unwrap(),
        expected
    );
    assert!(!fixture.root.join("deleted").exists());
}

/// The first preview exhausts the aggregate allowance; the sparse edit's source
/// is empty but its authoritative +1/-1 and listing entry still survive.
#[test]
fn native_line_accounting_exhausted_preview_keeps_later_sparse_delta() {
    let mut fixture = Fixture::with_patch(false, |root| {
        format!(
            "*** Begin Patch\n*** Add File: {}\n{}*** Update File: {}\n@@\n-line-5000\n+replacement\n*** End Patch",
            root.join("big").display(),
            "+padding\n".repeat(50_000),
            root.join("file").display()
        )
    });
    fs::write(fixture.root.join("file"), large_file()).unwrap();
    let (attempt, raw) = execute_and_retain(&mut fixture, false);
    assert_eq!(raw.result.certainty, NativeEffectCertainty::Confirmed);
    assert_eq!(raw.receipts.len(), 2);
    assert!(raw.receipts[0].diff_truncated);
    assert!(raw.receipts[1].diff_truncated);
    assert!(raw.receipts[1].diff.is_empty());
    assert_eq!(
        (raw.receipts[1].added_lines, raw.receipts[1].removed_lines),
        (1, 1)
    );
    project_once(&mut fixture, &attempt);
    let body = listing(&mut fixture);
    assert_row(&body, &fixture.root.join("file"), 1, 1);
    assert_row(&body, &fixture.root.join("big"), 50_000, 0);
}

/// Cancellation after one positive endpoint retains its actual sparse delta,
/// without attributing the later uncommitted create or permitting replay.
#[test]
fn native_line_accounting_partial_completion_keeps_only_confirmed_delta() {
    let mut fixture = Fixture::with_patch(false, |root| {
        format!(
            "*** Begin Patch\n*** Update File: {}\n@@\n-line-5000\n+replacement\n*** Add File: {}\n+not committed\n*** End Patch",
            root.join("file").display(),
            root.join("later").display()
        )
    });
    fs::write(fixture.root.join("file"), large_file()).unwrap();
    let (attempt, raw) = execute_and_retain(&mut fixture, true);
    assert_eq!(raw.result.certainty, NativeEffectCertainty::Partial);
    assert_eq!(raw.receipts.len(), 1);
    assert_eq!(
        (raw.receipts[0].added_lines, raw.receipts[0].removed_lines),
        (1, 1)
    );
    project_once(&mut fixture, &attempt);
    let body = listing(&mut fixture);
    assert_row(&body, &fixture.root.join("file"), 1, 1);
    assert!(!body.contains(&format!(
        "edited `{}`",
        fixture.root.join("later").display()
    )));
    assert!(!fixture.root.join("later").exists());
}
