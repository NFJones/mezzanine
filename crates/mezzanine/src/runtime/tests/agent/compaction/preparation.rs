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
