//! Provider continuation after genuine completed source and acknowledged epochs.
//! Offline transport records the rejected request, auxiliary inference and retry;
//! publication still executes the actual writer and adopts its actual receipt.

use super::*;

/// The synchronous compatibility path must finish the same physical publication
/// as the actor before rebuilding a rejected request. Actual predecessor task
/// completion supplies eligible source, without replacing the current task body.
#[test]
fn runtime_synchronous_context_limit_recovery_waits_for_compaction() {
    let (mut service, store, turn) = provider_limit_replay::recovery_source(
        &format!(
            "synchronous-oversized-marker {}",
            "oversized ".repeat(10_000)
        ),
        "the original task completed",
        1,
    );
    let conversation = service
        .agent_turn_ledger()
        .turn(&turn)
        .unwrap()
        .conversation_id
        .clone();
    let before = service.agent_turn_contexts()[&turn].clone();
    let current = before
        .chronology()
        .iter()
        .find(|event| {
            event.block().source == ContextSourceKind::UserInstruction
                && event.retention() == mez_agent::ContextRetention::Exact
        })
        .unwrap()
        .clone();
    let audit = store.inspect(&conversation).unwrap();
    assert!(service.remove_pending_agent_provider_task(&turn));
    let provider = RuntimeContextLimitThenCompactionProvider {
        requests: RefCell::new(Vec::new()),
    };
    let profile = service.agent_turn_model_profile(&turn).unwrap().clone();
    let execution = service
        .execute_agent_turn_with_provider(&turn, &provider, profile)
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 3);
    let request_text = |index: usize| {
        requests[index]
            .messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(request_text(0).contains("synchronous-oversized-marker"));
    assert!(request_text(1).contains("synchronous-oversized-marker"));
    assert!(!request_text(1).contains(&current.block().content));
    assert!(request_text(2).contains("synchronous model-authored context summary"));
    assert!(!request_text(2).contains("synchronous-oversized-marker"));
    assert!(request_text(2).contains(&current.block().content));
    assert!(service.pending_agent_compaction_tasks().is_empty());
    assert!(!service.agent_is_compacting("%1"));
    assert!(service.take_replay_epoch_attempt_for_tests().is_none());
    let (_, cold) = AgentTranscriptStore::new(store.root())
        .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_eq!(cold.revision().get(), 1);
    assert_eq!(
        cold.context().chronology()[0].block().content,
        "synchronous model-authored context summary"
    );
    assert!(cold.context().chronology().contains(&current));
    assert!(store.inspect(&conversation).unwrap().starts_with(&audit));
    assert!(
        !cold
            .context()
            .blocks()
            .iter()
            .any(|block| block.content.contains("synchronous-oversized-marker"))
    );
}
