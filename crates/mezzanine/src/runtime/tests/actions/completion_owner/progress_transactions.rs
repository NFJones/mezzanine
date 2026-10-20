//! Exact progress transaction regressions over genuinely admitted attempts.
//!
//! No dispatch is executed. Durable sources use the fixture-owned transcript;
//! faults affect installation only, never claim, effect or lifecycle authority.

use super::progress_retirement::{claimed_read_progress, retire_exact_progress};
use super::*;

/// Checks the production adapter transition before any deadline or pump call.
fn assert_idle_cleanup_enrolled(service: &RuntimeSessionService) {
    let transition = service.idle_cleanup_timer_transition_with_actor_progress(
        &BTreeSet::new(),
        false,
        77,
        500,
        25,
    );
    assert_eq!(transition.side_effects.len(), 1);
    assert!(matches!(
        &transition.side_effects[0],
        crate::runtime::RuntimeSideEffect::ScheduleTimer { .. }
    ));
}

/// Selects visible read progress locally and creates an actual durable baseline.
async fn visible_progress_fixture() -> CompletionFixture {
    let mut fixture = CompletionFixture::fetch().await;
    fixture
        .service
        .agent_shell_store_mut()
        .set_log_level(&fixture.turn.pane_id, AgentLogLevel::Debug)
        .unwrap();
    fixture
        .service
        .append_agent_status_text_to_terminal_buffer(
            &fixture.turn.pane_id,
            "F668_DURABLE_TRANSACTION_TAIL",
        )
        .unwrap();
    fixture
}

/// A worker captured before cleanup intent must not erase that intent merely
/// because its screen lineage and durable source revision are still current.
/// Building a private candidate cannot invalidate the actor's source fence.
#[tokio::test]
async fn runtime_completion_progress_old_resize_cannot_overwrite_retirement_intent() {
    let mut fixture = visible_progress_fixture().await;
    let progress = claimed_read_progress(&fixture, "F668_CAPTURED_PROGRESS".into());
    assert!(
        fixture
            .service
            .apply_action_presentation_progress(progress.clone())
            .unwrap()
    );
    let size = fixture
        .service
        .agent_pane_screen(&fixture.turn.pane_id)
        .unwrap()
        .size();
    fixture
        .service
        .presentation
        .defer_agent_presentation_resize(&fixture.turn.pane_id, size);
    let work = fixture
        .service
        .take_agent_presentation_resize_work(&fixture.turn.pane_id)
        .unwrap()
        .expect("eligible real source-backed replay");
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(true);
    let held = retire_exact_progress(&mut fixture, false);
    assert_eq!((held.retired, held.retained), (0, 1));
    let before = fixture
        .service
        .agent_pane_screen(&fixture.turn.pane_id)
        .unwrap()
        .clone();
    let source = fixture
        .service
        .action_presentation_progress_sources_for_tests(&fixture.turn.pane_id);
    let result = RuntimeSessionService::build_agent_presentation_resize(work)
        .unwrap()
        .expect("captured source produces a private candidate");
    assert!(
        !fixture
            .service
            .apply_agent_presentation_resize_result(result)
            .unwrap()
    );
    assert_eq!(
        fixture
            .service
            .agent_pane_screen(&fixture.turn.pane_id)
            .unwrap(),
        &before
    );
    assert_eq!(
        fixture
            .service
            .action_presentation_progress_sources_for_tests(&fixture.turn.pane_id),
        source
    );
    assert!(
        !fixture
            .service
            .reconcile_action_presentation_progress(&progress, &fixture.expected)
            .unwrap()
    );
}

/// A committed clean replay discharges the old progress source, never rebinds
/// its old transient-row count to durable rows. Repeated cleanup cannot erase
/// the durable tail, and a later valid revision can use the pane slot again.
#[tokio::test]
async fn runtime_completion_progress_clean_replay_discharge_preserves_durable_tail() {
    let mut fixture = visible_progress_fixture().await;
    let mut progress = claimed_read_progress(&fixture, "F668_TRANSIENT_MUST_DISAPPEAR".into());
    assert!(
        fixture
            .service
            .apply_action_presentation_progress(progress.clone())
            .unwrap()
    );
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(true);
    assert_eq!(retire_exact_progress(&mut fixture, false).retained, 1);
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(false);
    let size = fixture
        .service
        .agent_pane_screen(&fixture.turn.pane_id)
        .unwrap()
        .size();
    assert!(
        fixture
            .service
            .rebuild_agent_presentation_after_resize(&fixture.turn.pane_id, size)
            .unwrap()
    );
    assert_eq!(
        fixture
            .service
            .action_presentation_progress_counts_for_tests(&fixture.turn.pane_id),
        (0, 0)
    );
    let clean = fixture
        .service
        .agent_pane_screen(&fixture.turn.pane_id)
        .unwrap()
        .clone();
    let text = clean.normal_content_lines().join("\n");
    assert_eq!(
        text.matches("F668_DURABLE_TRANSACTION_TAIL").count(),
        1,
        "{text}"
    );
    assert!(!text.contains("F668_TRANSIENT_MUST_DISAPPEAR"), "{text}");
    let repeated = retire_exact_progress(&mut fixture, false);
    assert_eq!((repeated.retired, repeated.retained), (0, 0));
    assert_eq!(
        fixture
            .service
            .agent_pane_screen(&fixture.turn.pane_id)
            .unwrap(),
        &clean
    );
    progress.revision += 1;
    progress.source = "F668_LATER_VALID_PROGRESS".into();
    assert!(
        fixture
            .service
            .apply_action_presentation_progress(progress)
            .unwrap()
    );
    assert!(
        fixture
            .service
            .agent_pane_screen(&fixture.turn.pane_id)
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("F668_LATER_VALID_PROGRESS")
    );
}

/// A clean screen installed provisionally by replay is not cleanup acceptance.
/// Failure restores exact original pixels and payload/intent. A subsequent
/// successful cleanup operates only on its original restored suffix.
#[tokio::test]
async fn runtime_completion_progress_failed_replay_restores_source_and_intent() {
    let mut fixture = visible_progress_fixture().await;
    let progress = claimed_read_progress(&fixture, fixture.expected.content_text());
    assert!(
        fixture
            .service
            .apply_action_presentation_progress(progress.clone())
            .unwrap()
    );
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(true);
    assert_eq!(retire_exact_progress(&mut fixture, false).retained, 1);
    let before = fixture
        .service
        .agent_pane_screen(&fixture.turn.pane_id)
        .unwrap()
        .clone();
    let source = fixture
        .service
        .action_presentation_progress_sources_for_tests(&fixture.turn.pane_id);
    let size = before.size();
    assert!(
        fixture
            .service
            .rebuild_agent_presentation_after_resize(&fixture.turn.pane_id, size)
            .is_err()
    );
    assert_eq!(
        fixture
            .service
            .agent_pane_screen(&fixture.turn.pane_id)
            .unwrap(),
        &before
    );
    assert_eq!(
        fixture
            .service
            .action_presentation_progress_sources_for_tests(&fixture.turn.pane_id),
        source
    );
    assert!(
        !fixture
            .service
            .reconcile_action_presentation_progress(&progress, &fixture.expected)
            .unwrap()
    );
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(false);
    assert_eq!(retire_exact_progress(&mut fixture, false).retired, 1);
    let text = fixture
        .service
        .agent_pane_screen(&fixture.turn.pane_id)
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(text.contains("F668_DURABLE_TRANSACTION_TAIL"), "{text}");
}

/// An old snapshot cannot clear newer cleanup intent or resurrect source after
/// committed discharge, including when the canonical map slot is now empty.
#[tokio::test]
async fn runtime_completion_progress_snapshot_cannot_undo_intent_or_discharge() {
    let mut fixture = visible_progress_fixture().await;
    let progress = claimed_read_progress(&fixture, fixture.expected.content_text());
    assert!(
        fixture
            .service
            .apply_action_presentation_progress(progress.clone())
            .unwrap()
    );
    let before_intent = fixture
        .service
        .snapshot_agent_resume_presentation(&fixture.turn.pane_id);
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(true);
    assert_eq!(retire_exact_progress(&mut fixture, false).retained, 1);
    fixture
        .service
        .restore_agent_resume_presentation(&fixture.turn.pane_id, before_intent);
    assert!(
        !fixture
            .service
            .reconcile_action_presentation_progress(&progress, &fixture.expected)
            .unwrap()
    );
    let before_discharge = fixture
        .service
        .snapshot_agent_resume_presentation(&fixture.turn.pane_id);
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(false);
    assert_eq!(retire_exact_progress(&mut fixture, false).retired, 1);
    fixture
        .service
        .restore_agent_resume_presentation(&fixture.turn.pane_id, before_discharge);
    assert_eq!(
        fixture
            .service
            .action_presentation_progress_counts_for_tests(&fixture.turn.pane_id),
        (0, 0)
    );
}

/// A genuine Stop can remove live execution while optional cleanup is held.
/// The ordinary publication pump must still retire its presentation-only owner
/// after the cadence expires, even under the original runtime-only/no-store
/// policy. This test neither deletes live maps nor manufactures terminal phases.
#[tokio::test]
async fn runtime_completion_progress_no_store_stop_recovers_after_terminal_clear() {
    let mut fixture = CompletionFixture::fetch_with_save_policy(true, false).await;
    fixture
        .service
        .agent_shell_store_mut()
        .set_log_level(&fixture.turn.pane_id, AgentLogLevel::Debug)
        .unwrap();
    let progress = claimed_read_progress(&fixture, "F668_RUNTIME_ONLY_PROGRESS".into());
    assert!(
        fixture
            .service
            .apply_action_presentation_progress(progress)
            .unwrap()
    );
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(true);
    fixture
        .service
        .stop_agent_turn_for_pane(&fixture.turn.pane_id)
        .unwrap();
    assert_eq!(
        fixture
            .service
            .agent_turn_ledger()
            .turn(&fixture.turn.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Interrupted
    );
    assert!(
        !fixture
            .service
            .agent_turn_executions()
            .contains_key(&fixture.turn.turn_id)
    );
    assert_eq!(
        fixture
            .service
            .action_presentation_progress_counts_for_tests(&fixture.turn.pane_id),
        (1, 0)
    );
    assert!(
        fixture
            .cancellation
            .flag()
            .load(std::sync::atomic::Ordering::SeqCst)
    );
    assert!(fixture.service.action_progress_retirement_retry_needed());
    assert_idle_cleanup_enrolled(&fixture.service);
    tokio::time::sleep(std::time::Duration::from_millis(275)).await;
    fixture.service.retry_completion_publications();
    assert_eq!(
        fixture
            .service
            .action_presentation_progress_counts_for_tests(&fixture.turn.pane_id),
        (1, 0)
    );
    // The failed due attempt must re-enroll immediately, not only after the
    // next cadence deadline has already passed without an actor wakeup.
    assert_idle_cleanup_enrolled(&fixture.service);
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(false);
    tokio::time::sleep(std::time::Duration::from_millis(275)).await;
    assert!(fixture.service.completion_publication_retry_needed());
    assert!(fixture.service.retry_completion_publications() > 0);
    assert_eq!(
        fixture
            .service
            .action_presentation_progress_counts_for_tests(&fixture.turn.pane_id),
        (0, 0)
    );
}

/// Preview-only retirement has no canonical completion group to own its retry.
/// The inert display owners below are compositor source labels, not fabricated
/// native execution claims. Failure must preserve both payloads, retain exact
/// cancellation intent through an old snapshot, and enroll idle recovery.
/// Either idle cleanup or a durable write removes only the cancelled preview.
#[test]
fn runtime_completion_preview_failure_retains_intent_and_unrelated_source() {
    for via_idle in [true, false] {
        let mut service = test_runtime_service();
        service
            .attach_primary("primary", true, Size::new(100, 24).unwrap(), 120)
            .unwrap();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        service
            .agent_shell_store_mut()
            .set_log_level("%1", AgentLogLevel::Normal)
            .unwrap();
        service
            .append_agent_status_text_to_terminal_buffer("%1", "F668_PREVIEW_DURABLE")
            .unwrap();
        let owner = |turn: &str| crate::runtime::render::RuntimeAgentShellPreviewOwner {
            turn_id: turn.into(),
            action_id: "display-only".into(),
            marker: format!("display-{turn}"),
        };
        let cancelled = owner("cancelled-preview");
        let survivor = owner("unrelated-preview");
        service
            .update_agent_shell_output_preview(
                "%1",
                cancelled.clone(),
                1,
                &["F668_CANCELLED_PREVIEW".into()],
            )
            .unwrap();
        service
            .update_agent_shell_output_preview(
                "%1",
                survivor.clone(),
                1,
                &["F668_SURVIVING_PREVIEW".into()],
            )
            .unwrap();
        let before = service.agent_pane_screen("%1").unwrap().clone();
        let sources = service.agent_shell_output_previews_for_tests("%1");
        assert_eq!(sources.len(), 2);
        let old_snapshot = service.snapshot_agent_resume_presentation("%1");
        service.hold_agent_presentation_installs_for_tests(true);
        assert_eq!(
            service
                .retire_agent_shell_output_previews_for_turn(&cancelled.turn_id)
                .unwrap_err()
                .message(),
            "injected held pre-install presentation failure"
        );
        assert_eq!(service.agent_pane_screen("%1").unwrap(), &before);
        assert_eq!(service.agent_shell_output_previews_for_tests("%1"), sources);
        service.restore_agent_resume_presentation("%1", old_snapshot);
        assert!(service.shell_preview_retirement_retry_needed());
        assert_idle_cleanup_enrolled(&service);
        service.retry_completion_publications();
        assert_idle_cleanup_enrolled(&service);
        service.hold_agent_presentation_installs_for_tests(false);
        if via_idle {
            assert!(service.retry_completion_publications() > 0);
        } else {
            service
                .append_agent_status_text_to_terminal_buffer("%1", "F668_AFTER_CANCELLED_PREVIEW")
                .unwrap();
        }
        let retained = service.agent_shell_output_previews_for_tests("%1");
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].0, survivor);
        assert!(!service.shell_preview_retirement_retry_needed());
        let text = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        assert!(!text.contains("F668_CANCELLED_PREVIEW"), "{text}");
        assert_eq!(text.matches("F668_SURVIVING_PREVIEW").count(), 1, "{text}");
        assert!(text.contains("F668_PREVIEW_DURABLE"), "{text}");
    }
}
