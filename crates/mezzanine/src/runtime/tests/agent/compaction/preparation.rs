//! Manual source-preparation capacity and immutable result-adoption fences.
//!
//! These deterministic runtime tests retain exact work owners independently of
//! logical cancellation. They exercise source/store/count and conversation
//! freshness without provider requests or changed permission policy. Actor
//! responsiveness and actual worker execution are qualified by separate fixtures.

use super::*;
use mez_core::ids::ClientId;

/// Captures one genuine preparation through the command entry point. The store
/// has closed eligible rows; no injected model task or completion is needed.
fn fixture(label: &str) -> (RuntimeSessionService, ClientId, AgentTranscriptStore) {
    fixture_with_adapter(label, true)
}

/// Shares exact manual input policy between synchronous and worker fixtures.
fn fixture_with_adapter(
    label: &str,
    adapter: bool,
) -> (RuntimeSessionService, ClientId, AgentTranscriptStore) {
    let mut service = test_runtime_service();
    service.replace_config_layers(vec![ConfigLayer {name:"manual-request-parity".into(),path:None,format:ConfigFormat::Toml,scope:ConfigScope::Primary,trusted:true,
        text:"[agents]\ndefault_provider=\"openai\"\ndefault_model_profile=\"default\"\n[providers.openai]\nkind=\"openai\"\nmodels=[\"fixture-model\"]\ndefault_model=\"fixture-model\"\n[model_profiles.default]\nprovider=\"openai\"\nmodel=\"fixture-model\"\ncontext_window_tokens=128000\n".into()}]).unwrap();
    let store = AgentTranscriptStore::new(temp_root(label));
    for sequence in 1..=3 {
        store
            .append(&TranscriptEntry {
                conversation_id: "prepare-owned".into(),
                sequence,
                created_at_unix_seconds: sequence,
                role: TranscriptRole::Assistant,
                turn_id: format!("owned-{sequence}"),
                agent_id: "agent-%1".into(),
                pane_id: "%1".into(),
                content: format!("completed source {sequence}"),
            })
            .unwrap();
    }
    service.set_agent_transcript_store(store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "prepare-owned", 3)
        .unwrap();
    if adapter {
        service.use_manual_compaction_preparation_adapter();
    }
    let primary = service
        .attach_primary("preparation", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    (service, primary, store)
}

/// Uses real command/source adoption to obtain frozen request construction.
/// The logical owner remains compacting, but no provider task exists yet.
fn request_work(
    service: &mut RuntimeSessionService,
    primary: &ClientId,
) -> crate::runtime::RuntimeManualCompactionRequestWork {
    assert!(
        service
            .execute_agent_shell_command(primary, "/compact")
            .unwrap()
            .contains("state=preparing")
    );
    let source = service.take_manual_compaction_preparations().pop().unwrap();
    let result = source.execute_source();
    assert!(
        service
            .complete_manual_compaction_preparation(&source, result)
            .unwrap()
    );
    assert!(service.agent_is_compacting("%1"));
    assert!(service.pending_agent_compaction_task_ids().is_empty());
    service.take_manual_compaction_requests().pop().unwrap()
}

/// Manual compaction cannot overtake a queued or claimed accepted history
/// operation while it still has no ordinary running turn. Reject before changing
/// the epoch, then complete the original history work and require one real turn
/// and provider task rather than retiring the accepted input as stale.
#[test]
fn runtime_manual_compaction_preserves_queued_and_claimed_history_input() {
    for claimed in [false, true] {
        let (mut service, primary, store) = fixture(&format!("manual-history-overlap-{claimed}"));
        service
            .begin_agent_prompt_history_preparation(
                primary.clone(),
                "%1",
                "accepted history must survive compact refusal",
            )
            .unwrap();
        let dispatch = service.take_pending_agent_prompt_history().pop().unwrap();
        if claimed {
            assert!(service.claim_agent_prompt_history_preparation(&dispatch));
        }
        let epoch = service.agent_compaction_epoch("%1");
        let refused = service
            .execute_agent_shell_command(&primary, "/compact")
            .unwrap();
        assert!(
            refused.contains("accepted command/history preparation is active"),
            "{refused}"
        );
        assert_eq!(service.agent_compaction_epoch("%1"), epoch);
        assert!(!service.agent_is_compacting("%1"));
        assert!(service.take_manual_compaction_preparations().is_empty());
        if !claimed {
            assert!(service.claim_agent_prompt_history_preparation(&dispatch));
        }
        let history = execute_runtime_agent_prompt_history_work(dispatch.history_work.clone());
        assert!(
            service
                .complete_agent_prompt_history_preparation(&dispatch, history)
                .unwrap()
        );
        assert_eq!(service.agent_turn_ledger().turns().len(), 1);
        assert_eq!(service.pending_agent_provider_tasks().len(), 1);
        let turn_id = &service.agent_turn_ledger().turns()[0].turn_id;
        assert_eq!(
            service
                .agent_turn_contexts()
                .get(turn_id)
                .unwrap()
                .blocks()
                .iter()
                .filter(|block| block.content == "accepted history must survive compact refusal")
                .count(),
            1
        );
        assert!(store.compaction_epoch("prepare-owned").unwrap().is_none());
    }
}

/// A real manual operation without an ordinary turn must agree across the frame
/// and control projection in source preparation and provider queuing. Cancelling
/// the operation removes current compacting status without inventing a turn or
/// persisting a summary merely to rebuild display state.
#[test]
fn runtime_manual_compaction_state_matches_control_and_frame() {
    for preparing in [false, true] {
        let (mut service, primary, _) =
            fixture_with_adapter(&format!("manual-status-{preparing}"), preparing);
        service
            .execute_agent_shell_command(&primary, "/compact")
            .unwrap();
        assert!(service.agent_is_compacting("%1"));
        assert!(
            service
                .agent_shell_store()
                .get("%1")
                .unwrap()
                .running_turn_id
                .is_none()
        );
        let config = service
            .terminal_client_loop_config(TerminalClientLoopConfig::default())
            .unwrap();
        assert_eq!(
            config
                .frame_context
                .panes
                .get("%1")
                .and_then(|pane| pane.agent_status.as_deref()),
            Some("compacting")
        );
        let body = service.dispatch_runtime_control_body(
            r#"{"jsonrpc":"2.0","id":"compact-state","method":"agent/list","params":{}}"#,
            &primary,
        );
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        let row = json["result"]["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["pane_id"] == "%1")
            .unwrap();
        assert_eq!(row["status"], "compacting", "{body}");
        assert!(row["last_turn_id"].is_null());
        service.cancel_current_agent_compaction_task("%1");
        let body = service.dispatch_runtime_control_body(
            r#"{"jsonrpc":"2.0","id":"after-stop","method":"agent/list","params":{}}"#,
            &primary,
        );
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["result"]["agents"][0]["status"], "idle");
    }
}

/// The worker must preserve the established manual compactor's source boundary,
/// retention and request contract even after the operation clock is separated.
///
/// Logical elapsed time is qualified independently of provider claim deadlines.
#[test]
fn runtime_manual_compaction_elapsed_survives_claim_retry_and_new_operation() {
    let (mut service, primary, _) = fixture_with_adapter("manual-operation-clock", false);
    service
        .execute_agent_shell_command(&primary, "/compact")
        .unwrap();
    let original_epoch = service.agent_compaction_epoch("%1");
    let started = current_unix_seconds().saturating_sub(120).max(1);
    service.set_compaction_operation_start_for_tests("%1", started);
    let original = service.take_pending_agent_compaction_task("%1").unwrap();
    let old_generation = original.task_generation;
    service.claim_agent_compaction_task_state("%1", original.clone());
    assert_eq!(
        service.runtime_compaction_status("%1").unwrap().phase,
        "claimed"
    );
    assert_eq!(
        service.runtime_compaction_status("%1").unwrap().started_at,
        started
    );
    let retry = service
        .finish_agent_compaction_task("%1", old_generation)
        .unwrap();
    assert!(service.runtime_compaction_status("%1").is_none());
    service.queue_agent_compaction_task(retry);
    let operation = service.runtime_compaction_status("%1").unwrap();
    assert_eq!(operation.phase, "queued");
    assert_eq!(operation.epoch, original_epoch);
    assert_eq!(operation.started_at, started);
    let retry = service.take_pending_agent_compaction_task("%1").unwrap();
    let retry_generation = retry.task_generation;
    service.claim_agent_compaction_task_state("%1", retry);
    let mut fresh = service
        .finish_agent_compaction_task("%1", retry_generation)
        .unwrap();
    fresh.compaction_epoch = 0;
    service.queue_agent_compaction_task(fresh);
    let fresh = service.runtime_compaction_status("%1").unwrap();
    assert!(fresh.epoch > original_epoch);
    assert!(fresh.started_at > started);
    service.fail_agent_compaction_task("%1", old_generation);
    assert_eq!(
        service.runtime_compaction_status("%1").unwrap(),
        fresh,
        "stale retirement cannot reset a newer operation clock"
    );
    service.cancel_current_agent_compaction_task("%1");
    assert!(service.runtime_compaction_status("%1").is_none());
}

/// An allowed read-only inspection may own the generic command lane while
/// the worker handoff retains the admitted clock independently of task claims.
#[test]
fn runtime_manual_compaction_elapsed_survives_source_request_handoff() {
    let (mut service, primary, _) = fixture("manual-handoff-clock");
    service
        .execute_agent_shell_command(&primary, "/compact")
        .unwrap();
    let epoch = service.agent_compaction_epoch("%1");
    let started = current_unix_seconds().saturating_sub(90).max(1);
    service.set_compaction_operation_start_for_tests("%1", started);
    let source = service.take_manual_compaction_preparations().pop().unwrap();
    let rows = source.execute_source();
    service
        .complete_manual_compaction_preparation(&source, rows)
        .unwrap();
    assert_eq!(
        service.runtime_compaction_status("%1").unwrap().started_at,
        started
    );
    let request = service.take_manual_compaction_requests().pop().unwrap();
    let rendered = request.execute_request();
    service
        .complete_manual_compaction_request(&request, rendered)
        .unwrap();
    let operation = service.runtime_compaction_status("%1").unwrap();
    assert_eq!(operation.phase, "queued");
    assert_eq!(operation.epoch, epoch);
    assert_eq!(operation.started_at, started);
    service.cancel_current_agent_compaction_task("%1");
}

/// Current operation projection must remain pane-scoped across a genuine split;
/// a sibling conversation with no compactor stays idle in control and frame data.
#[test]
fn runtime_manual_compaction_state_does_not_leak_to_sibling() {
    let (mut service, primary, _) = fixture_with_adapter("manual-status-sibling", false);
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "/compact")
        .unwrap();
    let second = service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some("cat >/dev/null"))
        .unwrap()
        .pane_id;
    service
        .agent_shell_store_mut()
        .enter_or_resume(second.as_str())
        .unwrap();
    let body = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"independent","method":"agent/list","params":{}}"#,
        &primary,
    );
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let rows = json["result"]["agents"].as_array().unwrap();
    assert_eq!(
        rows.iter().find(|row| row["pane_id"] == "%1").unwrap()["status"],
        "compacting"
    );
    assert_eq!(
        rows.iter()
            .find(|row| row["pane_id"] == second.as_str())
            .unwrap()["status"],
        "idle"
    );
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    assert_eq!(
        config
            .frame_context
            .panes
            .get(second.as_str())
            .and_then(|pane| pane.agent_status.as_deref()),
        Some("idle")
    );
    service.cancel_current_agent_compaction_task("%1");
    service.terminate_all_pane_processes().unwrap();
}

/// An allowed read-only inspection may own the generic command lane while
/// compaction remains active. Footer and /status must retain discoverable
/// compaction ownership and honest queued/preparing phase instead of masking it
/// with command-running or an ordinary turn/claim that does not exist.
#[test]
fn runtime_manual_compaction_footer_and_status_survive_inspection_overlap() {
    for preparing in [false, true] {
        let (mut service, primary, _) =
            fixture_with_adapter(&format!("manual-inspection-status-{preparing}"), preparing);
        service
            .execute_agent_shell_command(&primary, "/compact")
            .unwrap();
        let operation = service.runtime_compaction_status("%1").unwrap();
        assert_eq!(
            operation.phase,
            if preparing { "preparing" } else { "queued" }
        );
        let status = service
            .execute_agent_shell_command(&primary, "/status")
            .unwrap();
        let status: serde_json::Value = serde_json::from_str(&status).unwrap();
        let body = status["body"].as_str().unwrap();
        assert!(body.contains("Compaction phase"));
        assert!(body.contains(operation.phase));
        assert!(body.contains("compacting"));
        service
            .begin_agent_command_claim("%1", "prepare-owned")
            .unwrap();
        let view = service
            .render_client_view(
                ClientViewRole::Primary,
                Size::new(80, 24).unwrap(),
                &TerminalClientLoopConfig::default(),
            )
            .unwrap()
            .unwrap();
        assert!(
            view.lines.iter().any(|line| line.contains("compacting (")),
            "{}",
            view.lines.join("\n")
        );
        assert!(
            !view
                .lines
                .iter()
                .any(|line| line.contains("command running"))
        );
        service.cancel_current_agent_compaction_task("%1");
    }
}

/// Existing human pause ownership must remain intact while manual queued
/// compaction is still discoverable. Phase stays queued, claim admission remains
/// inhibited, and inspecting control/status/footer does not silently resume it.
#[test]
fn runtime_manual_compaction_status_preserves_human_pause() {
    let (mut service, primary, _) = fixture_with_adapter("manual-pause-status", false);
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "establish native identity")
        .unwrap();
    service.stop_agent_turn_for_pane("%1").unwrap();
    service
        .execute_agent_shell_command(&primary, "/compact")
        .unwrap();
    let target = service
        .capture_agent_lifecycle_target(&primary, "%1")
        .unwrap();
    let pause = service
        .pause_agent_lifecycle_target(&primary, &target)
        .unwrap();
    let operation = service.runtime_compaction_status("%1").unwrap();
    assert_eq!(operation.phase, "queued");
    assert_eq!(operation.pause, Some("paused"));
    let generation = service
        .pending_agent_compaction_task_generation("%1")
        .unwrap();
    assert!(
        service
            .claim_agent_compaction_task("%1", generation)
            .unwrap()
            .is_none()
    );
    let status = service
        .execute_agent_shell_command(&primary, "/status")
        .unwrap();
    assert!(status.contains("Compaction pause"));
    assert!(status.contains("paused"));
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert!(
        view.lines
            .iter()
            .any(|line| line.contains("compacting (queued") && line.contains("paused"))
    );
    assert_eq!(service.agent_human_pause_generation("%1"), Some(pause));
    assert!(service.agent_is_human_paused("%1"));
    service.cancel_current_agent_compaction_task("%1");
    assert!(service.runtime_compaction_status("%1").is_none());
    service.terminate_all_pane_processes().unwrap();
}

/// The worker must preserve the established manual compactor's source boundary,
/// retained count, configured model/reasoning and derived output ceiling. Exact
/// fixed instructions and redacted source text match the synchronous path; only
/// operation generations differ because admission now has preparation phases.
#[test]
fn runtime_manual_compaction_request_matches_synchronous_task() {
    let (mut sync, primary, _) = fixture_with_adapter("manual-request-sync", false);
    assert!(
        sync.execute_agent_shell_command(&primary, "/compact")
            .unwrap()
            .contains("state=queued")
    );
    let expected = sync.take_pending_agent_compaction_task("%1").unwrap();
    let (mut asynchronous, primary, _) = fixture("manual-request-worker");
    let work = request_work(&mut asynchronous, &primary);
    let actual = work.execute_request().unwrap();
    assert_eq!(
        actual.compacted_through_sequence,
        expected.compacted_through_sequence
    );
    assert_eq!(actual.summarized_entries, expected.summarized_entries);
    assert_eq!(
        actual.retained_transcript_entries,
        expected.retained_transcript_entries
    );
    assert_eq!(actual.model_profile_name, expected.model_profile_name);
    assert_eq!(actual.request.provider, expected.request.provider);
    assert_eq!(actual.request.model, expected.request.model);
    assert_eq!(
        actual.request.reasoning_effort,
        expected.request.reasoning_effort
    );
    assert_eq!(
        actual.request.max_output_tokens,
        expected.request.max_output_tokens
    );
    assert_eq!(
        actual.request.messages[0].content,
        expected.request.messages[0].content
    );
    assert_eq!(
        actual.request.messages[1].content,
        expected.request.messages[1].content
    );
    assert_eq!(
        actual.request.messages.last().unwrap().content,
        expected.request.messages.last().unwrap().content
    );
}

/// Recheck actual final-request callbacks under store/count/config changes and
/// worker error. A valid previously rendered request cannot override fresh actor
/// evidence. Errors retire exactly this operation with no provider dispatch or
/// durable replacement, preserving the original archive.
#[test]
fn runtime_manual_compaction_request_adoption_rechecks_freshness_and_errors() {
    for change in ["store", "count", "config", "worker-error"] {
        let (mut service, primary, store) = fixture(&format!("manual-request-final-{change}"));
        let original = store.inspect("prepare-owned").unwrap();
        let work = request_work(&mut service, &primary);
        let result = work.execute_request();
        match change {
            "store" => service.set_agent_transcript_store(AgentTranscriptStore::new(temp_root(
                "manual-request-new-store",
            ))),
            "count" => {
                service
                    .agent_shell_store_mut()
                    .bind_conversation("%1", "prepare-owned", 4)
                    .unwrap();
            }
            "config" => {
                let mut layers = service.config_layers().to_vec();
                layers[0]
                    .text
                    .push_str("\n# changed captured project policy\n");
                service.replace_config_layers(layers).unwrap();
            }
            _ => {}
        }
        let result = if change == "worker-error" {
            Err(MezError::invalid_state("request worker fixture failure"))
        } else {
            result
        };
        assert!(
            service
                .complete_manual_compaction_request(&work, result)
                .is_err()
        );
        assert!(!service.agent_is_compacting("%1"));
        assert!(service.pending_agent_compaction_task_ids().is_empty());
        assert!(store.compaction_epoch("prepare-owned").unwrap().is_none());
        assert_eq!(store.inspect("prepare-owned").unwrap(), original);
    }
}

/// A late rendered request from a cancelled operation cannot clear or queue
/// into a newer preparation on the same pane, even with the same conversation.
/// The exact generation, not source equality, controls adoption and retirement.
#[test]
fn runtime_manual_compaction_request_late_result_preserves_newer_owner() {
    let (mut service, primary, _) = fixture("manual-request-newer-owner");
    let old = request_work(&mut service, &primary);
    let result = old.execute_request();
    service.cancel_current_agent_compaction_task("%1");
    service
        .execute_agent_shell_command(&primary, "/compact")
        .unwrap();
    let current = service.take_manual_compaction_preparations().pop().unwrap();
    assert!(current.task_generation > old.owner.task_generation);
    assert!(
        !service
            .complete_manual_compaction_request(&old, result)
            .unwrap()
    );
    assert!(service.agent_compaction_task_is_current("%1", current.task_generation));
    assert!(service.agent_is_compacting("%1"));
    assert!(service.pending_agent_compaction_task_ids().is_empty());
    service.cancel_current_agent_compaction_task("%1");
}

/// Eight cancelled operations still occupy eight worker slots while their owned
/// work is retained. Logical stop is not actual I/O retirement. Dropping one exact
/// retired work owner permits one new preparation, without resetting generations
/// or relaxing the finite bound for the remaining owners.
#[test]
fn runtime_manual_compaction_preparation_capacity_survives_cancellation() {
    let (mut service, primary, store) = fixture("manual-preparation-capacity");
    let mut workers = Vec::new();
    for _ in 0..8 {
        let response = service
            .execute_agent_shell_command(&primary, "/compact")
            .unwrap();
        assert!(response.contains("state=preparing"), "{response}");
        workers.push(service.take_manual_compaction_preparations().pop().unwrap());
        assert!(
            service
                .cancel_current_agent_compaction_task("%1")
                .had_task()
        );
    }
    assert!(service.reserve_manual_compaction_preparation().is_err());
    let denied = service
        .execute_agent_shell_command(&primary, "/compact")
        .unwrap();
    assert!(denied.contains("capacity unavailable"), "{denied}");
    assert!(!service.agent_is_compacting("%1"));
    drop(workers.remove(0));
    let admitted = service
        .execute_agent_shell_command(&primary, "/compact")
        .unwrap();
    assert!(admitted.contains("state=preparing"), "{admitted}");
    let new_work = service.take_manual_compaction_preparations().pop().unwrap();
    assert!(new_work.task_generation > workers.last().unwrap().task_generation);
    assert!(service.reserve_manual_compaction_preparation().is_err());
    service.cancel_current_agent_compaction_task("%1");
    drop((new_work, workers));
    assert!(service.reserve_manual_compaction_preparation().is_ok());
    assert!(store.compaction_epoch("prepare-owned").unwrap().is_none());
}

/// A changed installed source or logical count invalidates the captured history
/// even when the pane/conversation labels still match. Rejection clears only the
/// preparation owner and queues no provider work or durable epoch.
#[test]
fn runtime_manual_compaction_preparation_rejects_changed_source_ownership() {
    for change in ["store", "count"] {
        let (mut service, primary, store) = fixture(&format!("manual-preparation-stale-{change}"));
        assert!(
            service
                .execute_agent_shell_command(&primary, "/compact")
                .unwrap()
                .contains("state=preparing")
        );
        let work = service.take_manual_compaction_preparations().pop().unwrap();
        let rows = work.execute_source();
        if change == "store" {
            service.set_agent_transcript_store(AgentTranscriptStore::new(temp_root(
                "replacement-prepare-store",
            )));
        } else {
            service
                .agent_shell_store_mut()
                .bind_conversation("%1", "prepare-owned", 4)
                .unwrap();
        }
        assert!(
            service
                .complete_manual_compaction_preparation(&work, rows)
                .is_err()
        );
        assert!(!service.agent_is_compacting("%1"));
        assert!(service.pending_agent_compaction_task_ids().is_empty());
        assert!(store.compaction_epoch("prepare-owned").unwrap().is_none());
    }
}

/// A cancelled old source callback cannot clear or adopt into a replacement
/// conversation's newer preparation. Stable operation identities, not equal
/// source text or pane labels, fence exactly which owner may change state.
#[test]
fn runtime_manual_compaction_preparation_late_result_preserves_replacement() {
    let (mut service, primary, _) = fixture("manual-preparation-replacement");
    service
        .execute_agent_shell_command(&primary, "/compact")
        .unwrap();
    let old = service.take_manual_compaction_preparations().pop().unwrap();
    let result = old.execute_source();
    service.cancel_current_agent_compaction_task("%1");
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "replacement-owned", 3)
        .unwrap();
    assert!(
        service
            .execute_agent_shell_command(&primary, "/compact")
            .unwrap()
            .contains("state=preparing")
    );
    let current = service.take_manual_compaction_preparations().pop().unwrap();
    assert!(current.task_generation > old.task_generation);
    assert!(
        !service
            .complete_manual_compaction_preparation(&old, result)
            .unwrap()
    );
    assert!(service.agent_compaction_task_is_current("%1", current.task_generation));
    assert!(service.agent_is_compacting("%1"));
    assert!(service.pending_agent_compaction_task_ids().is_empty());
    service.cancel_current_agent_compaction_task("%1");
}
