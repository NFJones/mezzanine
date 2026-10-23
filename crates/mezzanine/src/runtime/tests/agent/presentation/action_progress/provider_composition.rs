//! Cross-owner regressions for provider rollback and confirmed executor output.
//!
//! These process-free fixtures use the real provider projection and managed
//! transaction progress ingress. They retain a running canonical action and
//! exercise display-only transitions without executing a provider or patch.

use super::*;

/// Adds provider source through ordinary streaming ingress on the active turn.
fn provider_delta(service: &mut RuntimeSessionService, turn_id: &str, text: &str) {
    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            turn_id,
            &mez_agent::StreamingSayEvent::TextDelta {
                action_index: 0,
                text: text.into(),
            },
        )
        .unwrap();
}

/// Builds and installs a complete provider generation at the current lineage.
fn project_provider(service: &mut RuntimeSessionService, turn_id: &str) {
    let work = service
        .take_agent_streaming_say_projection_work("%1", turn_id)
        .unwrap()
        .unwrap();
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap()
    );
}

/// Returns model-independent visible history for exact-once marker assertions.
fn pane_text(service: &RuntimeSessionService) -> String {
    service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n")
}

/// Starts a live provider projection while a managed patch is still running.
fn start_provider(service: &mut RuntimeSessionService, turn_id: &str) {
    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            turn_id,
            &mez_agent::StreamingSayEvent::Started {
                action_index: 0,
                status: mez_agent::SayStatus::Progress,
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.into(),
            },
        )
        .unwrap();
    provider_delta(service, turn_id, "mutable-provider-one");
    project_provider(service, turn_id);
}

/// A promotion changes the provider's source prefix, not merely its lineage.
/// Pre-promotion workers must reject; workers captured afterward must keep the
/// confirmed diff and its exact receipt. Another executor section and final
/// section matching prove reprojection did not replace erasure with duplication.
/// Provider discard must then remove its mutable text but retain both effects.
#[test]
fn runtime_provider_reprojection_retains_confirmed_prefix_and_settlement_receipts() {
    let action = patch_action();
    let (mut service, turn) = running_action_progress_fixture(
        action.clone(),
        "compose-marker",
        "# __MEZ_APPLY_PATCH_WRITE_PHASE__",
    );
    let store = AgentTranscriptStore::new(temp_root("provider-confirmed-prefix"));
    service.set_agent_transcript_store(store.clone());
    start_provider(&mut service, &turn.turn_id);
    provider_delta(&mut service, &turn.turn_id, " mutable-provider-two");
    let stale = RuntimeSessionService::build_agent_streaming_say_projection(
        service
            .take_agent_streaming_say_projection_work("%1", &turn.turn_id)
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let mut stale = Some(stale);
    let mut sections = Vec::new();
    for (ordinal, path, marker) in [
        (0, "note.txt", "confirmed-alpha"),
        (1, "other.txt", "confirmed-beta"),
    ] {
        let diff = format!("--- a/{path}\n+++ b/{path}\n@@ -1 +1 @@\n-old\n+{marker}\n");
        let progress = transaction_progress(
            &turn.turn_id,
            &action.id,
            "compose-marker",
            1,
            ActionPresentationComponentIdentity::confirmed_mutation(ordinal, path),
            &diff,
        );
        assert!(
            service
                .apply_action_presentation_progress(progress.clone())
                .unwrap()
        );
        assert!(
            service
                .promote_confirmed_action_presentation_progress(&progress)
                .unwrap()
        );
        sections.push(
            mez_agent::semantic_patch_planning::ApplyPatchConfirmedSection {
                ordinal,
                path: path.into(),
                diff,
            },
        );
        if ordinal == 0 {
            assert!(
                !service
                    .apply_agent_streaming_say_projection_result(stale.take().unwrap())
                    .unwrap()
            );
        }
        provider_delta(&mut service, &turn.turn_id, " next-provider-generation");
        project_provider(&mut service, &turn.turn_id);
        let text = pane_text(&service);
        for section in &sections {
            assert_eq!(
                text.matches(section.diff.lines().last().unwrap()).count(),
                1,
                "{text}"
            );
        }
        assert_eq!(
            service.action_presentation_progress_counts_for_tests("%1"),
            (0, sections.len())
        );
        assert!(service.promoted_action_patch_sections_match(
            &turn.turn_id,
            &action.id,
            &ActionPresentationExecutionIdentity::Transaction("compose-marker".into()),
            &sections,
        ));
    }
    assert!(
        service
            .discard_agent_streaming_say_presentation("%1", Some(&turn.turn_id))
            .unwrap()
    );
    let text = pane_text(&service);
    assert!(!text.contains("mutable-provider"), "{text}");
    for marker in ["confirmed-alpha", "confirmed-beta"] {
        assert_eq!(text.matches(marker).count(), 1, "{text}");
    }
    let entries = store.inspect_presentation(&turn.conversation_id).unwrap();
    assert_eq!(entries.len(), 2);
    assert!(service.promoted_action_patch_sections_match(
        &turn.turn_id,
        &action.id,
        &ActionPresentationExecutionIdentity::Transaction("compose-marker".into()),
        &sections,
    ));
    let output = sections
        .iter()
        .map(|section| section.diff.as_str())
        .collect::<String>();
    let execution = service.agent_turn_executions()[&turn.turn_id].clone();
    append_test_execution_assistant_context(&mut service, &turn, &execution);
    service.record_running_shell_transaction_output("%1", output.as_bytes());
    service
        .observe_agent_shell_transaction_end(
            "%1",
            "compose-marker",
            &turn.turn_id,
            &turn.agent_id,
            "%1",
            0,
        )
        .unwrap();
    let result = &service.agent_turn_executions()[&turn.turn_id].action_results[0];
    assert_eq!(result.status, ActionStatus::Succeeded);
    let settled = pane_text(&service);
    for marker in ["confirmed-alpha", "confirmed-beta"] {
        assert_eq!(settled.matches(marker).count(), 1, "{settled}");
    }
    let settled_entries = store.inspect_presentation(&turn.conversation_id).unwrap();
    for section in &sections {
        assert_eq!(
            settled_entries
                .iter()
                .filter(|entry| {
                    presentation_semantic_source(entry).as_deref() == Some(section.diff.as_str())
                })
                .count(),
            1
        );
    }
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 20).unwrap(), 120).unwrap(),
    );
    service
        .replay_agent_presentation_entries_to_terminal_buffer("%1", &entries)
        .unwrap();
    let replay = pane_text(&service);
    for marker in ["confirmed-alpha", "confirmed-beta"] {
        assert_eq!(replay.matches(marker).count(), 1, "{replay}");
    }
    assert_eq!(
        store.inspect_presentation(&turn.conversation_id).unwrap(),
        entries
    );
}

/// A width/height transition must resize the immutable confirmed prefix as well
/// as the mutable provider layer. Work captured before resize is stale, while
/// a subsequent provider update and discard preserve the original source once.
/// This uses the production resize completion boundary without creating a PTY.
#[test]
fn runtime_provider_reprojection_keeps_confirmed_prefix_across_resize() {
    let (mut service, turn) = running_action_progress_fixture(
        patch_action(),
        "resize-marker",
        "# __MEZ_APPLY_PATCH_WRITE_PHASE__",
    );
    start_provider(&mut service, &turn.turn_id);
    let progress = transaction_progress(
        &turn.turn_id,
        "patch-1",
        "resize-marker",
        1,
        ActionPresentationComponentIdentity::confirmed_mutation(0, "note.txt"),
        "--- a/note.txt\n+++ b/note.txt\n@@ -1 +1 @@\n-old\n+resize-confirmed\n",
    );
    assert!(
        service
            .apply_action_presentation_progress(progress.clone())
            .unwrap()
    );
    assert!(
        service
            .promote_confirmed_action_presentation_progress(&progress)
            .unwrap()
    );
    let occurrence = service.action_presentation_progress_entries_for_tests("%1");
    for size in [Size::new(40, 18).unwrap(), Size::new(80, 28).unwrap()] {
        provider_delta(&mut service, &turn.turn_id, " before-resize");
        let stale = RuntimeSessionService::build_agent_streaming_say_projection(
            service
                .take_agent_streaming_say_projection_work("%1", &turn.turn_id)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert!(
            service
                .apply_pane_resize_completion_event("%1", size)
                .unwrap()
        );
        assert!(
            !service
                .apply_agent_streaming_say_projection_result(stale)
                .unwrap()
        );
        provider_delta(&mut service, &turn.turn_id, " after-resize");
        project_provider(&mut service, &turn.turn_id);
        let text = pane_text(&service);
        assert_eq!(text.matches("resize-confirmed").count(), 1, "{text}");
        assert_eq!(service.agent_pane_screen("%1").unwrap().size(), size);
        assert_eq!(
            service.action_presentation_progress_entries_for_tests("%1"),
            occurrence
        );
    }
    assert!(
        service
            .discard_agent_streaming_say_presentation("%1", Some(&turn.turn_id))
            .unwrap()
    );
    let text = pane_text(&service);
    assert_eq!(text.matches("resize-confirmed").count(), 1, "{text}");
    assert!(!text.contains("mutable-provider"), "{text}");
}

/// Confirmed source rebasing must not confer authority over a replacement
/// conversation. A delayed post-promotion worker and old executor progress both
/// reject without changing the replacement screen or importing the old diff.
#[test]
fn runtime_provider_reprojection_rejects_replaced_conversation() {
    let (mut service, turn) = running_action_progress_fixture(
        patch_action(),
        "replacement-marker",
        "# __MEZ_APPLY_PATCH_WRITE_PHASE__",
    );
    start_provider(&mut service, &turn.turn_id);
    let progress = transaction_progress(
        &turn.turn_id,
        "patch-1",
        "replacement-marker",
        1,
        ActionPresentationComponentIdentity::confirmed_mutation(0, "note.txt"),
        "--- a/note.txt\n+++ b/note.txt\n@@ -1 +1 @@\n-old\n+old-confirmation\n",
    );
    assert!(
        service
            .apply_action_presentation_progress(progress.clone())
            .unwrap()
    );
    assert!(
        service
            .promote_confirmed_action_presentation_progress(&progress)
            .unwrap()
    );
    provider_delta(&mut service, &turn.turn_id, " pending-old-provider");
    let stale = RuntimeSessionService::build_agent_streaming_say_projection(
        service
            .take_agent_streaming_say_projection_work("%1", &turn.turn_id)
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    service
        .finish_agent_turn("%1", &turn.turn_id, AgentTurnState::Interrupted)
        .unwrap();
    let interrupted = pane_text(&service);
    assert_eq!(
        interrupted.matches("old-confirmation").count(),
        1,
        "{interrupted}"
    );
    service
        .agent_shell_store_mut()
        .start_new_conversation("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 24).unwrap(), 120).unwrap(),
    );
    service
        .append_agent_status_text_to_terminal_buffer("%1", "replacement-only")
        .unwrap();
    let before = pane_text(&service);
    assert!(
        !service
            .apply_agent_streaming_say_projection_result(stale)
            .unwrap()
    );
    assert!(
        !service
            .apply_action_presentation_progress(progress)
            .unwrap()
    );
    assert_eq!(pane_text(&service), before);
    assert!(!before.contains("old-confirmation"), "{before}");
}

/// Executor progress may arrive before the first provider byte, with or without
/// a shell preview. New response generations must strip both overlays before
/// capturing rollback source, then recompose each once with its original entry.
#[test]
fn runtime_provider_reprojection_strips_executor_first_baseline() {
    for with_preview in [false, true] {
        let (mut service, turn) = running_action_progress_fixture(
            patch_action(),
            "executor-first-marker",
            "# __MEZ_APPLY_PATCH_WRITE_PHASE__",
        );
        if with_preview {
            service
                .update_agent_shell_output_preview(
                    "%1",
                    crate::runtime::render::RuntimeAgentShellPreviewOwner {
                        turn_id: turn.turn_id.clone(),
                        action_id: "independent-shell".into(),
                        marker: "first-preview".into(),
                    },
                    1,
                    &["executor-first-preview".into()],
                )
                .unwrap();
        }
        let progress = transaction_progress(
            &turn.turn_id,
            "patch-1",
            "executor-first-marker",
            1,
            ActionPresentationComponentIdentity::confirmed_mutation(0, "note.txt"),
            "--- a/note.txt\n+++ b/note.txt\n@@ -1 +1 @@\n-old\n+executor-first-effect\n",
        );
        assert!(
            service
                .apply_action_presentation_progress(progress.clone())
                .unwrap()
        );
        let occurrence = service.action_presentation_progress_entries_for_tests("%1");
        start_provider(&mut service, &turn.turn_id);
        for response_index in 1..=2 {
            let text = pane_text(&service);
            assert_eq!(text.matches("executor-first-effect").count(), 1, "{text}");
            if with_preview {
                assert_eq!(text.matches("executor-first-preview").count(), 1, "{text}");
            }
            assert_eq!(
                service.action_presentation_progress_entries_for_tests("%1"),
                occurrence
            );
            service
                .apply_agent_streaming_say_event_to_terminal_buffer(
                    "%1",
                    &turn.turn_id,
                    &mez_agent::StreamingSayEvent::ResponseStarted { response_index },
                )
                .unwrap();
            start_provider(&mut service, &turn.turn_id);
        }
        assert!(
            service
                .discard_agent_streaming_say_presentation("%1", Some(&turn.turn_id))
                .unwrap()
        );
        let text = pane_text(&service);
        assert_eq!(text.matches("executor-first-effect").count(), 1, "{text}");
        assert!(!text.contains("mutable-provider"), "{text}");
        assert!(
            service
                .promote_confirmed_action_presentation_progress(&progress)
                .unwrap()
        );
        let text = pane_text(&service);
        assert_eq!(text.matches("executor-first-effect").count(), 1, "{text}");
    }
}

/// A failed provider install after composition must leave the old screen and
/// both promoted receipts and unpromoted source with their original owner. A
/// later presentation-only projection recovers without repeating any executor
/// operation or persistence of the already-confirmed section.
#[test]
fn runtime_provider_reprojection_failure_retains_executor_custody() {
    let (mut service, turn) = running_action_progress_fixture(
        patch_action(),
        "failure-marker",
        "# __MEZ_APPLY_PATCH_WRITE_PHASE__",
    );
    let store = AgentTranscriptStore::new(temp_root("provider-composite-failure"));
    service.set_agent_transcript_store(store.clone());
    start_provider(&mut service, &turn.turn_id);
    for (ordinal, path, source) in [
        (0, "note.txt", "confirmed-before-failure"),
        (1, "other.txt", "overlay-before-failure"),
    ] {
        let progress = transaction_progress(
            &turn.turn_id,
            "patch-1",
            "failure-marker",
            1,
            ActionPresentationComponentIdentity::confirmed_mutation(ordinal, path),
            &format!("--- a/{path}\n+++ b/{path}\n@@ -1 +1 @@\n-old\n+{source}\n"),
        );
        assert!(
            service
                .apply_action_presentation_progress(progress.clone())
                .unwrap()
        );
        if ordinal == 0 {
            assert!(
                service
                    .promote_confirmed_action_presentation_progress(&progress)
                    .unwrap()
            );
        }
    }
    let before = service.agent_pane_screen("%1").unwrap().clone();
    let occurrence = service.action_presentation_progress_entries_for_tests("%1");
    let durable = store.inspect_presentation(&turn.conversation_id).unwrap();
    provider_delta(&mut service, &turn.turn_id, " after-failure-provider");
    let result = RuntimeSessionService::build_agent_streaming_say_projection(
        service
            .take_agent_streaming_say_projection_work("%1", &turn.turn_id)
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    service.presentation.reject_next_streaming_composite_install = true;
    let error = service
        .apply_agent_streaming_say_projection_result(result)
        .unwrap_err();
    assert!(error.to_string().contains("injected streaming composite"));
    assert_eq!(service.agent_pane_screen("%1").unwrap(), &before);
    assert_eq!(
        service.action_presentation_progress_counts_for_tests("%1"),
        (1, 1)
    );
    assert_eq!(
        service.action_presentation_progress_entries_for_tests("%1"),
        occurrence
    );
    project_provider(&mut service, &turn.turn_id);
    assert!(
        service
            .discard_agent_streaming_say_presentation("%1", Some(&turn.turn_id))
            .unwrap()
    );
    let text = pane_text(&service);
    for marker in ["confirmed-before-failure", "overlay-before-failure"] {
        assert_eq!(text.matches(marker).count(), 1, "{text}");
    }
    assert!(!text.contains("after-failure-provider"), "{text}");
    assert_eq!(
        service.action_presentation_progress_counts_for_tests("%1"),
        (1, 1)
    );
    assert_eq!(
        service.action_presentation_progress_entries_for_tests("%1"),
        occurrence
    );
    assert_eq!(
        store.inspect_presentation(&turn.conversation_id).unwrap(),
        durable
    );
}

/// Provider installation must recompose unpromoted executor rows too, retaining
/// their original occurrence and exact generation until confirmation. Otherwise
/// the next progress update sees a stale owner and loses its settlement receipt.
#[test]
fn runtime_provider_reprojection_recomposes_unpromoted_executor_overlay() {
    let (mut service, turn) = running_action_progress_fixture(
        patch_action(),
        "overlay-marker",
        "# __MEZ_APPLY_PATCH_WRITE_PHASE__",
    );
    start_provider(&mut service, &turn.turn_id);
    let progress = transaction_progress(
        &turn.turn_id,
        "patch-1",
        "overlay-marker",
        1,
        ActionPresentationComponentIdentity::confirmed_mutation(0, "note.txt"),
        "--- a/note.txt\n+++ b/note.txt\n@@ -1 +1 @@\n-old\n+unpromoted-effect\n",
    );
    assert!(
        service
            .apply_action_presentation_progress(progress.clone())
            .unwrap()
    );
    let occurrence = service.action_presentation_progress_entries_for_tests("%1");
    provider_delta(&mut service, &turn.turn_id, " provider-continued");
    project_provider(&mut service, &turn.turn_id);
    let text = pane_text(&service);
    assert_eq!(text.matches("unpromoted-effect").count(), 1, "{text}");
    assert_eq!(
        service.action_presentation_progress_entries_for_tests("%1"),
        occurrence
    );
    assert!(
        service
            .promote_confirmed_action_presentation_progress(&progress)
            .unwrap()
    );
    assert!(
        !service
            .promote_confirmed_action_presentation_progress(&progress)
            .unwrap()
    );
    assert!(
        service
            .discard_agent_streaming_say_presentation("%1", Some(&turn.turn_id))
            .unwrap()
    );
    let text = pane_text(&service);
    assert_eq!(text.matches("unpromoted-effect").count(), 1, "{text}");
    assert!(!text.contains("mutable-provider"), "{text}");
}

/// Provider, preview and executor owners share one lineage while pending
/// steering remains the final suffix. Provider discard and ordinary status
/// writes must preserve a confirmed occurrence and its copy metadata, without
/// making provider text durable or duplicating the independent shell preview.
#[test]
fn runtime_provider_reprojection_preserves_preview_steering_and_ordinary_append() {
    let (mut service, turn) = running_action_progress_fixture(
        patch_action(),
        "mixed-marker",
        "# __MEZ_APPLY_PATCH_WRITE_PHASE__",
    );
    start_provider(&mut service, &turn.turn_id);
    let preview = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: turn.turn_id.clone(),
        action_id: "independent-shell".into(),
        marker: "preview-marker".into(),
    };
    service
        .update_agent_shell_output_preview("%1", preview.clone(), 1, &["shell-before".into()])
        .unwrap();
    let progress = transaction_progress(
        &turn.turn_id,
        "patch-1",
        "mixed-marker",
        1,
        ActionPresentationComponentIdentity::confirmed_mutation(0, "note.txt"),
        "--- a/note.txt\n+++ b/note.txt\n@@ -1 +1 @@\n-old\n+mixed-confirmation\n",
    );
    assert!(
        service
            .apply_action_presentation_progress(progress.clone())
            .unwrap()
    );
    assert!(
        service
            .promote_confirmed_action_presentation_progress(&progress)
            .unwrap()
    );
    let occurrence = service.action_presentation_progress_entries_for_tests("%1");
    provider_delta(&mut service, &turn.turn_id, " provider-continued");
    project_provider(&mut service, &turn.turn_id);
    service
        .update_agent_shell_output_preview("%1", preview, 2, &["shell-after".into()])
        .unwrap();
    // Steering intentionally retires the provisional provider layout, so it
    // enters after the last provider worker has installed its generation.
    service
        .inject_agent_steering_with_display("%1", "exact-guidance", "pending-guidance")
        .unwrap();
    service
        .append_agent_status_text_to_terminal_buffer("%1", "ordinary-status")
        .unwrap();
    let text = pane_text(&service);
    for marker in [
        "mixed-confirmation",
        "ordinary-status",
        "shell-after",
        "pending-guidance",
    ] {
        assert_eq!(text.matches(marker).count(), 1, "{text}");
    }
    assert!(!text.contains("mutable-provider"), "{text}");
    assert!(!text.contains("shell-before"), "{text}");
    assert!(text.find("mixed-confirmation").unwrap() < text.find("pending-guidance").unwrap());
    assert_eq!(
        service.action_presentation_progress_entries_for_tests("%1"),
        occurrence
    );
    let copy = CopyMode::from_screen(service.agent_pane_screen("%1").unwrap(), 24).unwrap();
    assert_eq!(
        copy.lines()
            .iter()
            .filter(|line| line.contains("mixed-confirmation"))
            .count(),
        1
    );
}
