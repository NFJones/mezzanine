//! Exact progress transaction regressions over genuinely admitted attempts.
//!
//! No dispatch is executed. Durable sources use the fixture-owned transcript;
//! faults affect installation only, never claim, effect or lifecycle authority.

use super::progress_retirement::{claimed_read_progress, retire_exact_progress};
use super::*;
use std::collections::BTreeSet;

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

/// Runtime-only progress can lose suffix authority after a real terminal Stop.
/// With no eligible reconstruction backend, retained source must not keep an
/// impossible idle retry alive. A genuine later presentation source/backend
/// transition re-enrolls recovery without changing original no-save policy.
#[tokio::test]
async fn runtime_completion_ineligible_progress_refusal_quiesces_then_reactivates() {
    let mut fixture = CompletionFixture::fetch_with_save_policy(true, false).await;
    fixture
        .service
        .agent_shell_store_mut()
        .set_log_level(&fixture.turn.pane_id, AgentLogLevel::Debug)
        .unwrap();
    let progress = claimed_read_progress(&fixture, "F668_INELIGIBLE_PROGRESS".into());
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
    assert!(
        !fixture
            .service
            .agent_turn_executions()
            .contains_key(&fixture.turn.turn_id)
    );
    assert_eq!(
        fixture
            .service
            .agent_turn_ledger()
            .turn(&fixture.turn.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Interrupted
    );
    let sources = fixture
        .service
        .action_presentation_progress_sources_for_tests(&fixture.turn.pane_id);
    let mut replacement = fixture
        .service
        .agent_pane_screen(&fixture.turn.pane_id)
        .unwrap()
        .clone();
    replacement.feed(b"\r\nF668_INELIGIBLE_REPLACEMENT\r\n");
    fixture.service.set_agent_pane_screen(
        fixture.turn.pane_id.clone(),
        fixture.turn.conversation_id.clone(),
        replacement.clone(),
    );
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(false);
    let refused = retire_exact_progress(&mut fixture, false);
    assert_eq!((refused.retired, refused.retained), (0, 1));
    assert!(refused.errors.is_empty());
    assert_eq!(
        fixture
            .service
            .action_presentation_progress_sources_for_tests(&fixture.turn.pane_id),
        sources
    );
    assert_eq!(
        fixture
            .service
            .agent_pane_screen(&fixture.turn.pane_id)
            .unwrap(),
        &replacement
    );
    assert!(
        !fixture.service.action_progress_retirement_retry_needed(),
        "ineligible refused suffix must not poll forever"
    );
    assert!(
        fixture
            .service
            .idle_cleanup_timer_transition_with_actor_progress(&BTreeSet::new(), false, 79, 500, 25)
            .side_effects
            .is_empty()
    );
    fixture.service.retry_completion_publications();
    assert!(
        fixture
            .service
            .presentation
            .take_agent_presentation_resize_dispatch_effects()
            .is_empty()
    );

    let store = AgentTranscriptStore::new(fixture._root.0.join("transcripts"));
    let mut writer = RuntimeServiceFixture::new()
        .control_socket(fixture._root.0.join("writer.sock"))
        .build();
    writer.set_agent_transcript_store(store.clone());
    writer
        .attach_primary("writer", true, Size::new(120, 32).unwrap(), 120)
        .unwrap();
    writer
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    writer
        .agent_shell_store_mut()
        .bind_conversation("%1", &fixture.turn.conversation_id, 0)
        .unwrap();
    writer
        .append_agent_status_text_to_terminal_buffer("%1", "F668_ELIGIBLE_SOURCE_ARRIVED")
        .unwrap();
    fixture.service.set_agent_transcript_store(store.clone());
    assert!(fixture.service.action_progress_retirement_retry_needed());
    assert_idle_cleanup_enrolled(&fixture.service);
    tokio::time::sleep(std::time::Duration::from_millis(275)).await;
    fixture.service.retry_completion_publications();
    fixture
        .service
        .presentation
        .take_agent_presentation_resize_dispatch_effects();
    let work = fixture
        .service
        .take_agent_presentation_resize_work(&fixture.turn.pane_id)
        .unwrap()
        .expect("genuine eligible-source transition");
    let result = RuntimeSessionService::build_agent_presentation_resize(work)
        .unwrap()
        .unwrap();
    assert!(
        fixture
            .service
            .apply_agent_presentation_resize_result(result)
            .unwrap()
    );
    assert_eq!(
        fixture
            .service
            .action_presentation_progress_counts_for_tests(&fixture.turn.pane_id),
        (0, 0)
    );
    let raw = store.inspect(&fixture.turn.conversation_id);
    assert!(
        raw.as_ref().is_ok_and(|rows| rows.is_empty())
            || raw
                .as_ref()
                .is_err_and(|error| error.kind() == crate::error::MezErrorKind::NotFound),
        "late presentation backend must not archive original runtime-only model rows: {raw:?}"
    );
    let recovered = fixture
        .service
        .agent_pane_screen(&fixture.turn.pane_id)
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        recovered.contains("F668_ELIGIBLE_SOURCE_ARRIVED"),
        "{recovered}"
    );
}

/// An empty journal is a real worker-observed unavailable source, not permission
/// to install a stale candidate or poll the same impossible reconstruction.
/// The returned disposition must stop idle replay while preserving source and
/// pixels, then allow a fresh probe after genuine canonical source is published
/// and the same store path is explicitly reattached through the production API.
#[test]
fn runtime_completion_empty_preview_replay_reports_refusal_and_reactivates() {
    let root = CompletionRoot(
        temp_root("completion-preview-empty-replay"),
        CompletionRootCleanup::RemoveOwned,
    );
    let store = AgentTranscriptStore::new(root.0.join("transcripts"));
    let mut service = RuntimeServiceFixture::new()
        .control_socket(root.0.join("control.sock"))
        .build();
    service.set_agent_transcript_store(store.clone());
    service
        .attach_primary("primary", true, Size::new(100, 24).unwrap(), 120)
        .unwrap();
    let conversation = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Normal)
        .unwrap();
    service.set_agent_pane_screen(
        "%1",
        conversation.clone(),
        TerminalScreen::new(Size::new(100, 24).unwrap(), 120).unwrap(),
    );
    let owner = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: "display-only-empty".into(),
        action_id: "display-only".into(),
        marker: "display-only-empty".into(),
    };
    service
        .update_agent_shell_output_preview(
            "%1",
            owner.clone(),
            1,
            &["F668_EMPTY_JOURNAL_PREVIEW".into()],
        )
        .unwrap();
    service.hold_agent_presentation_installs_for_tests(true);
    assert!(
        service
            .retire_agent_shell_output_previews_for_turn(&owner.turn_id)
            .is_err()
    );
    service.hold_agent_presentation_installs_for_tests(false);
    let sources = service.agent_shell_output_previews_for_tests("%1");
    let mut replacement = service.agent_pane_screen("%1").unwrap().clone();
    replacement.feed(b"\r\nF668_EMPTY_JOURNAL_REPLACEMENT\r\n");
    service.set_agent_pane_screen("%1", conversation.clone(), replacement.clone());
    assert!(
        service
            .append_agent_status_text_to_terminal_buffer("%1", "not accepted yet")
            .is_err()
    );
    assert!(
        store
            .inspect_presentation(&conversation)
            .unwrap()
            .is_empty()
    );
    service
        .presentation
        .take_agent_presentation_resize_dispatch_effects();
    let work = service
        .take_agent_presentation_resize_work("%1")
        .unwrap()
        .expect("metadata-eligible real work");
    let unavailable = RuntimeSessionService::build_agent_presentation_resize(work)
        .unwrap()
        .expect("no-source must return an actor-visible disposition");
    assert!(
        !service
            .apply_agent_presentation_resize_result(unavailable)
            .unwrap()
    );
    assert_eq!(service.agent_pane_screen("%1").unwrap(), &replacement);
    assert_eq!(service.agent_shell_output_previews_for_tests("%1"), sources);
    assert!(!service.shell_preview_retirement_retry_needed());
    assert!(
        service
            .idle_cleanup_timer_transition_with_actor_progress(&BTreeSet::new(), false, 78, 500, 25)
            .side_effects
            .is_empty()
    );
    service.retry_completion_publications();
    assert!(
        service
            .presentation
            .take_agent_presentation_resize_dispatch_effects()
            .is_empty()
    );
    assert!(
        service
            .append_agent_status_text_to_terminal_buffer("%1", "still not accepted")
            .is_err()
    );
    assert!(
        service
            .presentation
            .take_agent_presentation_resize_dispatch_effects()
            .is_empty()
    );
    assert_eq!(service.agent_pane_screen("%1").unwrap(), &replacement);

    // A fixture-owned writer publishes genuine new canonical presentation;
    // no worker claim, durability acknowledgment or action result is fabricated.
    let mut writer = RuntimeServiceFixture::new()
        .control_socket(root.0.join("writer.sock"))
        .build();
    writer.set_agent_transcript_store(store.clone());
    writer
        .attach_primary("writer", true, Size::new(100, 24).unwrap(), 120)
        .unwrap();
    writer
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    writer
        .agent_shell_store_mut()
        .bind_conversation("%1", &conversation, 0)
        .unwrap();
    writer
        .append_agent_status_text_to_terminal_buffer("%1", "F668_SOURCE_NOW_AVAILABLE")
        .unwrap();
    assert!(
        !store
            .inspect_presentation(&conversation)
            .unwrap()
            .is_empty()
    );
    service.set_agent_transcript_store(store.clone());
    assert!(service.shell_preview_retirement_retry_needed());
    service.retry_completion_publications();
    service
        .presentation
        .take_agent_presentation_resize_dispatch_effects();
    let work = service
        .take_agent_presentation_resize_work("%1")
        .unwrap()
        .expect("changed source reactivates recovery");
    let recovered = RuntimeSessionService::build_agent_presentation_resize(work)
        .unwrap()
        .unwrap();
    assert!(
        service
            .apply_agent_presentation_resize_result(recovered)
            .unwrap()
    );
    assert!(
        service
            .agent_shell_output_previews_for_tests("%1")
            .is_empty()
    );
    service
        .append_agent_status_text_to_terminal_buffer("%1", "F668_AFTER_SOURCE_RECOVERY")
        .unwrap();
    let text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(text.contains("F668_SOURCE_NOW_AVAILABLE"), "{text}");
    assert!(text.contains("F668_AFTER_SOURCE_RECOVERY"), "{text}");
    assert!(!text.contains("F668_EMPTY_JOURNAL_PREVIEW"), "{text}");
}

/// A stale cancelled preview cannot prove a suffix, so immediate writes retain
/// source and leave replacement pixels intact. An eligible production resize
/// handoff recovers it; runtime-only refusal retains source without spinning an
/// idle suffix-only retry or inventing authority over those replacement pixels.
#[tokio::test]
async fn runtime_completion_stale_preview_uses_canonical_recovery_or_refuses() {
    for attach_store in [true, false] {
        let mut fixture = CompletionFixture::fetch_with_save_policy(true, attach_store).await;
        fixture
            .service
            .agent_shell_store_mut()
            .set_log_level(&fixture.turn.pane_id, AgentLogLevel::Normal)
            .unwrap();
        fixture
            .service
            .append_agent_status_text_to_terminal_buffer(
                &fixture.turn.pane_id,
                "F668_STALE_PREVIEW_BASELINE",
            )
            .unwrap();
        let owner = crate::runtime::render::RuntimeAgentShellPreviewOwner {
            turn_id: fixture.turn.turn_id.clone(),
            action_id: fixture.action.id.clone(),
            marker: fixture.attempt.clone(),
        };
        fixture
            .service
            .update_agent_shell_output_preview(
                &fixture.turn.pane_id,
                owner,
                1,
                &["F668_STALE_CANCELLED_PREVIEW".into()],
            )
            .unwrap();
        fixture
            .service
            .hold_agent_presentation_installs_for_tests(true);
        assert!(
            fixture
                .service
                .retire_agent_shell_output_previews_for_turn(&fixture.turn.turn_id)
                .is_err()
        );
        let sources = fixture
            .service
            .agent_shell_output_previews_for_tests(&fixture.turn.pane_id);
        let mut replacement = fixture
            .service
            .agent_pane_screen(&fixture.turn.pane_id)
            .unwrap()
            .clone();
        replacement.feed(b"\r\nF668_UNOWNED_REPLACEMENT\r\n");
        fixture.service.set_agent_pane_screen(
            fixture.turn.pane_id.clone(),
            fixture.turn.conversation_id.clone(),
            replacement.clone(),
        );
        fixture
            .service
            .hold_agent_presentation_installs_for_tests(false);
        let error = fixture
            .service
            .append_agent_status_text_to_terminal_buffer(
                &fixture.turn.pane_id,
                "F668_AFTER_STALE_PREVIEW",
            )
            .unwrap_err();
        assert_eq!(error.message(), "shell preview cleanup source is retained");
        assert_eq!(
            fixture
                .service
                .agent_pane_screen(&fixture.turn.pane_id)
                .unwrap(),
            &replacement
        );
        assert_eq!(
            fixture
                .service
                .agent_shell_output_previews_for_tests(&fixture.turn.pane_id),
            sources
        );
        assert_eq!(
            fixture.service.shell_preview_retirement_retry_needed(),
            attach_store
        );
        if attach_store {
            assert!(
                fixture
                    .service
                    .presentation
                    .agent_presentation_resize_is_deferred(&fixture.turn.pane_id)
            );
            let work = fixture
                .service
                .take_agent_presentation_resize_work(&fixture.turn.pane_id)
                .unwrap()
                .expect("eligible canonical recovery owner");
            let result = RuntimeSessionService::build_agent_presentation_resize(work)
                .unwrap()
                .expect("nonempty original durable source");
            assert!(
                fixture
                    .service
                    .apply_agent_presentation_resize_result(result)
                    .unwrap()
            );
            fixture
                .service
                .append_agent_status_text_to_terminal_buffer(
                    &fixture.turn.pane_id,
                    "F668_AFTER_STALE_PREVIEW",
                )
                .unwrap();
            let text = fixture
                .service
                .agent_pane_screen(&fixture.turn.pane_id)
                .unwrap()
                .normal_content_lines()
                .join("\n");
            assert!(!text.contains("F668_STALE_CANCELLED_PREVIEW"), "{text}");
            assert!(text.contains("F668_STALE_PREVIEW_BASELINE"), "{text}");
            assert!(text.contains("F668_AFTER_STALE_PREVIEW"), "{text}");
        } else {
            fixture.service.retry_completion_publications();
            assert_eq!(
                fixture
                    .service
                    .agent_shell_output_previews_for_tests(&fixture.turn.pane_id),
                sources
            );
            assert_eq!(
                fixture
                    .service
                    .agent_pane_screen(&fixture.turn.pane_id)
                    .unwrap(),
                &replacement
            );
        }
    }
}

/// Preview-only retirement has no canonical completion group to own its retry.
/// The actual prepared-resume path must replay nonempty saved target source
/// while original cancelled preview custody is transaction-local, not active
/// on its temporary screen. A late production failure restores original pixels,
/// source and retry intent; acceptance retires only the original projection.
/// Neither branch launches a native process or provider request.
#[test]
fn runtime_completion_pending_preview_allows_resume_and_exact_rollback() {
    for fail_late in [true, false] {
        let root = CompletionRoot(
            temp_root("completion-preview-resume"),
            CompletionRootCleanup::RemoveOwned,
        );
        let store = AgentTranscriptStore::new(root.0.join("transcripts"));
        let mut service = RuntimeServiceFixture::new()
            .control_socket(root.0.join("control.sock"))
            .build();
        service.set_agent_transcript_store(store.clone());
        service
            .attach_primary("primary", true, Size::new(100, 24).unwrap(), 120)
            .unwrap();
        let original = service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap()
            .session_id
            .clone();
        service
            .agent_shell_store_mut()
            .set_log_level("%1", AgentLogLevel::Normal)
            .unwrap();
        service
            .append_agent_status_text_to_terminal_buffer("%1", "F668_ORIGINAL_RESUME_BASE")
            .unwrap();
        let owner = crate::runtime::render::RuntimeAgentShellPreviewOwner {
            turn_id: "display-only-original".into(),
            action_id: "display-only".into(),
            marker: "display-only-original".into(),
        };
        service
            .update_agent_shell_output_preview(
                "%1",
                owner.clone(),
                1,
                &["F668_ORIGINAL_CANCELLED_PREVIEW".into()],
            )
            .unwrap();
        service.hold_agent_presentation_installs_for_tests(true);
        assert!(
            service
                .retire_agent_shell_output_previews_for_turn(&owner.turn_id)
                .is_err()
        );
        service.hold_agent_presentation_installs_for_tests(false);
        let before = service.agent_pane_screen("%1").unwrap().clone();
        let sources = service.agent_shell_output_previews_for_tests("%1");
        let old_snapshot = service.snapshot_agent_resume_presentation("%1");
        store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: "f668-resume-target".into(),
                sequence: 1,
                created_at_unix_seconds: 10,
                role: mez_agent::transcript::TranscriptRole::User,
                turn_id: "saved-target-source".into(),
                agent_id: "agent-%1".into(),
                pane_id: "%1".into(),
                content: "F668_REPLACEMENT_RESUME_SOURCE".into(),
            })
            .unwrap();
        service.checkpoint_agent_session_metadata().unwrap();
        let prepared =
            crate::runtime::commands::read_direct_resume(&store, "f668-resume-target", None)
                .unwrap();
        assert!(
            !prepared.entries.is_empty(),
            "real nonempty target fallback source"
        );
        if fail_late {
            service.fail_next_agent_resume_after_authority_restore_for_tests();
        }
        let result = service.execute_agent_shell_resume_command_with_read(
            "%1",
            store.clone(),
            prepared,
            false,
        );
        if fail_late {
            assert!(
                result
                    .unwrap_err()
                    .message()
                    .contains("post-authority restoration")
            );
            assert_eq!(
                service.agent_shell_store().get("%1").unwrap().session_id,
                original
            );
            assert_eq!(service.agent_pane_screen("%1").unwrap(), &before);
            assert_eq!(service.agent_shell_output_previews_for_tests("%1"), sources);
            assert!(service.shell_preview_retirement_retry_needed());
            assert!(service.retry_completion_publications() > 0);
        } else {
            result.unwrap();
            assert_eq!(
                service.agent_shell_store().get("%1").unwrap().session_id,
                "f668-resume-target"
            );
            assert!(
                service
                    .agent_pane_screen("%1")
                    .unwrap()
                    .normal_content_lines()
                    .join("\n")
                    .contains("F668_REPLACEMENT_RESUME_SOURCE")
            );
            service.restore_agent_resume_presentation("%1", old_snapshot);
            assert!(
                service
                    .agent_shell_output_previews_for_tests("%1")
                    .is_empty()
            );
            assert!(!service.shell_preview_retirement_retry_needed());
        }
    }
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

/// A refused progress promotion cannot acknowledge the canonical terminal body.
///
/// The retained provisional source equals the terminal body, but its screen
/// lineage was replaced through the supported setter, so promotion cannot prove
/// those exact rows are installed. Reconciliation must not claim success: it
/// keeps bounded cleanup intent for the superseded provisional and lets the
/// canonical RESULT publish instead of silently dropping the completed outcome.
#[tokio::test]
async fn runtime_completion_refused_promotion_cannot_acknowledge_terminal_body() {
    let mut fixture = visible_progress_fixture().await;
    let body = fixture.expected.content_text();
    let progress = claimed_read_progress(&fixture, body.clone());
    assert!(
        fixture
            .service
            .apply_action_presentation_progress(progress.clone())
            .unwrap()
    );
    let mut replacement = fixture
        .service
        .agent_pane_screen(&fixture.turn.pane_id)
        .unwrap()
        .clone();
    replacement.feed(b"\r\nF668_REFUSED_PROMOTION_REPLACEMENT\r\n");
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
    let canonical = fixture.retained();
    assert!(
        !fixture
            .service
            .reconcile_action_presentation_progress(&progress, &fixture.expected)
            .unwrap(),
        "a refused promotion cannot acknowledge exactly equal terminal content"
    );
    assert_eq!(
        fixture
            .service
            .action_presentation_progress_counts_for_tests(&fixture.turn.pane_id),
        (1, 0)
    );
    assert!(
        fixture.service.action_progress_retirement_retry_needed(),
        "the superseded provisional must retain its cleanup decision"
    );
    assert_eq!(
        fixture
            .service
            .agent_pane_screen(&fixture.turn.pane_id)
            .unwrap()
            .normal_content_lines(),
        before.normal_content_lines()
    );
    assert_eq!(fixture.retained(), canonical);
    assert!(
        fixture
            .service
            .complete_approved_external_action(fixture.outcome())
            .unwrap()
    );
    assert_eq!(fixture.retained().action_results[0], fixture.expected);
    let published = fixture
        .service
        .agent_pane_screen(&fixture.turn.pane_id)
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        published.contains(body.as_str()),
        "the canonical RESULT must be installed after refused promotion: {published}"
    );
    assert!(
        !fixture
            .service
            .complete_approved_external_action(fixture.outcome())
            .unwrap(),
        "a duplicate completion stays inert after the retained result publishes"
    );
}
