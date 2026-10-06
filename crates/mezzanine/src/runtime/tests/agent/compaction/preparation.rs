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
    let mut service = test_runtime_service();
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
    service.use_manual_compaction_preparation_adapter();
    let primary = service
        .attach_primary("preparation", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    (service, primary, store)
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
