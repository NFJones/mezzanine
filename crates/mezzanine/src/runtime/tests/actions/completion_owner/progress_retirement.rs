//! Exact transient metadata retirement without repainting unowned screen rows.
//!
//! These fixtures admit and claim a genuine external fetch, but never execute
//! the dispatch. Progress and terminal outcomes are synthetic. Screen changes
//! use the supported conversation-bound screen API, not private owner writes.

use super::*;
use mez_agent::{
    ActionPresentationComponentIdentity, ActionPresentationExecutionIdentity,
    ActionPresentationProgress,
};

/// Builds progress for the exact actually claimed attempt, not a fake marker.
fn claimed_read_progress(
    fixture: &CompletionFixture,
    source: String,
) -> ActionPresentationProgress {
    ActionPresentationProgress::new(
        &fixture.turn.turn_id,
        &fixture.action.id,
        ActionPresentationExecutionIdentity::Attempt(fixture.attempt.clone()),
        1,
        ActionPresentationComponentIdentity::ProvisionalReadBody,
        source,
    )
}

/// Exercises both existing exact retirement owners, without cancelling work.
fn retire_exact_progress(
    fixture: &mut CompletionFixture,
    whole_turn: bool,
) -> crate::runtime::render::RuntimeActionProgressRetirement {
    if whole_turn {
        fixture
            .service
            .retire_action_presentation_progress_for_turn(&fixture.turn.turn_id)
    } else {
        fixture
            .service
            .retire_action_presentation_progress_for_action(
                &fixture.turn.turn_id,
                &fixture.action.id,
            )
    }
}

/// Retiring metadata must not install a new lineage or alter replacement pixels,
/// cursor or render generation when the old suffix is no longer safely owned.
fn assert_screen_unchanged(
    fixture: &CompletionFixture,
    before: &TerminalScreen,
    lineage: Option<u64>,
) {
    let screen = fixture
        .service
        .agent_pane_screen(&fixture.turn.pane_id)
        .unwrap();
    assert_eq!(screen.normal_content_lines(), before.normal_content_lines());
    assert_eq!(screen.visible_lines(), before.visible_lines());
    assert_eq!(screen.cursor_state(), before.cursor_state());
    assert_eq!(screen.render_generation(), before.render_generation());
    assert_eq!(
        fixture
            .service
            .agent_pane_screen_lineage(&fixture.turn.pane_id, &fixture.turn.conversation_id),
        lineage
    );
}

/// A genuine recoverable pre-install failure must retain the original progress
/// source until an actual successful rollback. This control prevents fixing
/// retirement by reverting every snapshot to destructive map removal. Terminal
/// result acceptance and duplicate ingress do not replay the worker or context.
#[tokio::test]
async fn runtime_completion_progress_preinstall_failure_preserves_source_until_install() {
    let mut fixture = CompletionFixture::fetch().await;
    let progress = claimed_read_progress(
        &fixture,
        "provisional read differs from terminal result".into(),
    );
    assert!(
        fixture
            .service
            .apply_action_presentation_progress(progress.clone())
            .unwrap()
    );
    assert_eq!(
        fixture
            .service
            .action_presentation_progress_counts_for_tests(&fixture.turn.pane_id),
        (1, 0)
    );
    let source = fixture
        .service
        .action_presentation_progress_entries_for_tests(&fixture.turn.pane_id);
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(true);
    let outcome = fixture.outcome();
    assert!(
        fixture
            .service
            .complete_approved_external_action(outcome)
            .unwrap()
    );
    assert_eq!(
        fixture
            .service
            .action_presentation_progress_counts_for_tests(&fixture.turn.pane_id),
        (1, 0)
    );
    assert_eq!(
        fixture
            .service
            .action_presentation_progress_entries_for_tests(&fixture.turn.pane_id),
        source
    );
    let canonical = fixture.retained();
    assert_eq!(canonical.action_results[0], fixture.expected);
    let before = fixture
        .service
        .agent_pane_screen(&fixture.turn.pane_id)
        .unwrap()
        .clone();
    let lineage = fixture
        .service
        .agent_pane_screen_lineage(&fixture.turn.pane_id, &fixture.turn.conversation_id);
    let error = fixture
        .service
        .reconcile_action_presentation_progress(&progress, &fixture.expected)
        .unwrap_err();
    assert_eq!(
        error.message(),
        "injected held pre-install presentation failure"
    );
    assert_screen_unchanged(&fixture, &before, lineage);
    assert_eq!(
        fixture
            .service
            .action_presentation_progress_entries_for_tests(&fixture.turn.pane_id),
        source
    );
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(false);
    assert!(
        !fixture
            .service
            .reconcile_action_presentation_progress(&progress, &fixture.expected)
            .unwrap()
    );
    assert_eq!(
        fixture
            .service
            .action_presentation_progress_counts_for_tests(&fixture.turn.pane_id),
        (0, 0)
    );
    assert_eq!(fixture.retained(), canonical);
    let duplicate = fixture.outcome();
    assert!(
        !fixture
            .service
            .complete_approved_external_action(duplicate)
            .unwrap()
    );
    assert_eq!(fixture.retained(), canonical);
}

/// Updating a live component must not consume its prior source before the
/// replacement screen is installed. A rejected update preserves the original
/// provenance, pixels and canonical execution; retry uses the same component
/// identity rather than admitting another worker or a replacement observation.
#[tokio::test]
async fn runtime_completion_progress_update_failure_preserves_original_owner() {
    let mut fixture = CompletionFixture::fetch().await;
    fixture
        .service
        .agent_shell_store_mut()
        .set_log_level(&fixture.turn.pane_id, AgentLogLevel::Debug)
        .unwrap();
    let mut progress = claimed_read_progress(&fixture, "F668_INITIAL_PROGRESS".into());
    assert!(
        fixture
            .service
            .apply_action_presentation_progress(progress.clone())
            .unwrap()
    );
    let original = fixture
        .service
        .action_presentation_progress_entries_for_tests(&fixture.turn.pane_id);
    let canonical = fixture.retained();
    let screen = fixture
        .service
        .agent_pane_screen(&fixture.turn.pane_id)
        .unwrap()
        .clone();
    assert!(
        screen
            .normal_content_lines()
            .join("\n")
            .contains("F668_INITIAL_PROGRESS")
    );
    let lineage = fixture
        .service
        .agent_pane_screen_lineage(&fixture.turn.pane_id, &fixture.turn.conversation_id);
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(true);
    progress.revision += 1;
    progress.source = "F668_REPLACEMENT_PROGRESS".into();
    assert_eq!(
        fixture
            .service
            .apply_action_presentation_progress(progress.clone())
            .unwrap_err()
            .message(),
        "injected held pre-install presentation failure"
    );
    assert_screen_unchanged(&fixture, &screen, lineage);
    assert_eq!(
        fixture
            .service
            .action_presentation_progress_sources_for_tests(&fixture.turn.pane_id),
        vec![(1, "F668_INITIAL_PROGRESS".into())]
    );
    assert_eq!(
        fixture
            .service
            .action_presentation_progress_counts_for_tests(&fixture.turn.pane_id),
        (1, 0)
    );
    assert_eq!(
        fixture
            .service
            .action_presentation_progress_entries_for_tests(&fixture.turn.pane_id),
        original
    );
    assert_eq!(fixture.retained(), canonical);
    assert!(!fixture.cancellation.flag().load(Ordering::SeqCst));
    assert!(fixture.service.approved_external_action_attempt_is_current(
        &fixture.turn.turn_id,
        &fixture.action.id,
        &fixture.attempt
    ));
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(false);
    assert!(
        fixture
            .service
            .apply_action_presentation_progress(progress)
            .unwrap()
    );
    assert_eq!(
        fixture
            .service
            .action_presentation_progress_entries_for_tests(&fixture.turn.pane_id),
        original
    );
    assert_eq!(fixture.retained(), canonical);
    assert_eq!(
        fixture
            .service
            .action_presentation_progress_sources_for_tests(&fixture.turn.pane_id),
        vec![(2, "F668_REPLACEMENT_PROGRESS".into())]
    );
    let updated = fixture
        .service
        .agent_pane_screen(&fixture.turn.pane_id)
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(updated.contains("F668_REPLACEMENT_PROGRESS"), "{updated}");
    assert!(!updated.contains("F668_INITIAL_PROGRESS"), "{updated}");
}

/// A failed optional cleanup installs no pixels and retains exact source plus
/// intent. That intent cannot promote the same body or suppress a final result;
/// a later successful cleanup discharges it once without touching the worker.
#[tokio::test]
async fn runtime_completion_progress_retained_cleanup_cannot_promote_or_suppress() {
    let mut fixture = CompletionFixture::fetch().await;
    fixture
        .service
        .agent_shell_store_mut()
        .set_log_level(&fixture.turn.pane_id, AgentLogLevel::Debug)
        .unwrap();
    let progress = claimed_read_progress(&fixture, fixture.expected.content_text());
    assert!(
        fixture
            .service
            .apply_action_presentation_progress(progress.clone())
            .unwrap()
    );
    let source = fixture
        .service
        .action_presentation_progress_sources_for_tests(&fixture.turn.pane_id);
    let canonical = fixture.retained();
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(true);
    let held = retire_exact_progress(&mut fixture, false);
    assert_eq!((held.retired, held.retained), (0, 1));
    assert_eq!(held.errors.len(), 1);
    assert_eq!(
        held.errors[0].1.message(),
        "injected held pre-install presentation failure"
    );
    assert!(
        !fixture
            .service
            .reconcile_action_presentation_progress(&progress, &fixture.expected)
            .unwrap()
    );
    assert_eq!(
        fixture
            .service
            .action_presentation_progress_sources_for_tests(&fixture.turn.pane_id),
        source
    );
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(false);
    let retired = retire_exact_progress(&mut fixture, false);
    assert_eq!((retired.retired, retired.retained), (1, 0));
    assert!(retired.errors.is_empty());
    assert_eq!(retire_exact_progress(&mut fixture, false).retired, 0);
    assert_eq!(fixture.retained(), canonical);
    assert!(!fixture.cancellation.flag().load(Ordering::SeqCst));
}

/// A supported unrelated screen replacement invalidates the old progress
/// lineage without proving the old rows were removed. Exact action/turn cleanup
/// must report Retained, not a speculative successful removal, preserve source
/// and intent, and enqueue canonical replay without modifying replacement pixels.
#[tokio::test]
async fn runtime_completion_progress_stale_lineage_retires_exact_metadata_once() {
    for whole_turn in [false, true] {
        let mut fixture = CompletionFixture::fetch().await;
        let progress = claimed_read_progress(&fixture, "F668_OLD_TRANSIENT_READ".into());
        assert!(
            fixture
                .service
                .apply_action_presentation_progress(progress)
                .unwrap()
        );
        assert_eq!(
            fixture
                .service
                .action_presentation_progress_counts_for_tests(&fixture.turn.pane_id),
            (1, 0)
        );
        let canonical = fixture.retained();
        let mut replacement = fixture
            .service
            .agent_pane_screen(&fixture.turn.pane_id)
            .unwrap()
            .clone();
        replacement.feed(b"\r\nF668_REPLACEMENT_SCREEN_MUST_SURVIVE\r\n");
        fixture.service.set_agent_pane_screen(
            fixture.turn.pane_id.clone(),
            fixture.turn.conversation_id.clone(),
            replacement,
        );
        let before = fixture
            .service
            .agent_pane_screen(&fixture.turn.pane_id)
            .unwrap()
            .clone();
        let lineage = fixture
            .service
            .agent_pane_screen_lineage(&fixture.turn.pane_id, &fixture.turn.conversation_id);
        let original_entries = fixture
            .service
            .action_presentation_progress_entries_for_tests(&fixture.turn.pane_id);
        assert_eq!(
            fixture
                .service
                .retire_action_presentation_progress_for_action(
                    &fixture.turn.turn_id,
                    "not-the-owned-progress-action"
                )
                .retired,
            0
        );
        assert_eq!(
            fixture
                .service
                .action_presentation_progress_entries_for_tests(&fixture.turn.pane_id),
            original_entries
        );
        assert_screen_unchanged(&fixture, &before, lineage);
        let first = retire_exact_progress(&mut fixture, whole_turn);
        assert_eq!((first.retired, first.retained), (0, 1));
        assert!(first.errors.is_empty());
        assert_eq!(
            fixture
                .service
                .action_presentation_progress_counts_for_tests(&fixture.turn.pane_id),
            (1, 0)
        );
        assert_eq!(
            fixture
                .service
                .action_presentation_progress_entries_for_tests(&fixture.turn.pane_id),
            original_entries
        );
        assert_screen_unchanged(&fixture, &before, lineage);
        let repeated = retire_exact_progress(&mut fixture, whole_turn);
        assert_eq!((repeated.retired, repeated.retained), (0, 1));
        assert!(repeated.errors.is_empty());
        assert!(
            fixture
                .service
                .presentation
                .agent_presentation_resize_is_deferred(&fixture.turn.pane_id)
        );
        assert_screen_unchanged(&fixture, &before, lineage);
        assert_eq!(fixture.retained(), canonical);
        assert!(!fixture.cancellation.flag().load(Ordering::SeqCst));
        assert!(fixture.service.approved_external_action_attempt_is_current(
            &fixture.turn.turn_id,
            &fixture.action.id,
            &fixture.attempt
        ));
    }
}
