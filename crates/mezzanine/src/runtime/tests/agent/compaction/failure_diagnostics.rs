//! Terminal diagnostic provenance survives distinct real lifecycle boundaries.
//! Counters describe the original operation; missing source cannot fabricate
//! observations or grant another request. No model network is contacted.

use super::*;

fn queued_source(manual: bool) -> (RuntimeSessionService, AgentTranscriptStore, String) {
    let (mut service, store, turn) = replay_observed::queued();
    if manual {
        service.cancel_current_agent_compaction_task("%1");
        crate::test_support::compaction_source::complete_task(
            &mut service,
            &turn,
            "actual foreground completion",
            mez_agent::ProviderApiCompatibility::OpenAiResponses,
        );
        let primary = service.session.layout_owner_client_id().unwrap().clone();
        let command = service
            .execute_agent_shell_command(&primary, "/compact")
            .unwrap();
        assert!(command.contains("state=queued"), "{command}");
    }
    service.set_auth_store(crate::security::auth::AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&store.root().join("diagnostics-auth")),
    ));
    (service, store, turn)
}

fn visible(service: &RuntimeSessionService) -> String {
    service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("")
        .split_whitespace()
        .collect()
}

fn assert_facts(service: &RuntimeSessionService, generation: u64, calls: &str, depth: &str) {
    let text = visible(service);
    for expected in [
        "Auxiliarycompaction:".to_owned(),
        "phase=".to_owned(),
        "operation=".to_owned(),
        format!("dispatch={generation};"),
        format!("model_calls={calls};"),
        format!("serial_depth={depth};"),
        "elapsed_seconds=".to_owned(),
    ] {
        assert!(text.contains(&expected), "missing {expected}");
    }
}

/// A genuine admitted dispatch remains held across logical deadline expiry.
/// Both automatic and idle manual paths must expose original counters, while
/// expense custody survives until the actual adapter is destroyed and settled.
#[test]
fn runtime_compaction_failure_diagnostics_cover_auto_and_manual_expiry() {
    for manual in [false, true] {
        let (mut service, _, _) = queued_source(manual);
        let generation = service
            .pending_agent_compaction_task_generation("%1")
            .unwrap();
        let dispatch = service
            .claim_agent_compaction_task("%1", generation)
            .unwrap()
            .unwrap();
        assert!(
            service
                .expire_claimed_agent_compaction_task("%1", generation)
                .unwrap()
        );
        assert!(!service.agent_is_compacting("%1"));
        assert!(service.retains_compaction_expense_for_tests("%1", generation));
        assert_facts(&service, generation, "0", "0");
        drop(dispatch);
        assert!(
            !service
                .apply_agent_compaction_transition(
                    crate::test_support::compaction_source::model_result_event(
                        "%1",
                        generation,
                        Err(MezError::invalid_state("expired dispatch retired")),
                    ),
                )
                .unwrap()
                .applied
        );
        assert!(!service.retains_compaction_expense_for_tests("%1", generation));
    }
}

/// Both known and malformed worker error kinds preserve one actual invocation
/// and once-only known expense. Manual errors expose the same bounded facts
/// without inventing an ordinary turn or accepting any response as a summary.
#[test]
fn runtime_compaction_failure_diagnostics_cover_manual_and_unknown_worker_errors() {
    for manual in [false, true] {
        for unknown_kind in [false, true] {
            let (mut service, store, turn) = queued_source(manual);
            let generation = service
                .pending_agent_compaction_task_generation("%1")
                .unwrap();
            let dispatch = service
                .claim_agent_compaction_task("%1", generation)
                .unwrap()
                .unwrap();
            let conversation = dispatch.task.conversation_id.clone();
            let key = mez_agent::ModelTokenUsageKey::new("runtime-batch", "test");
            let before = service
                .agent_token_usage_for_conversation(&conversation)
                .get(&key)
                .copied()
                .unwrap_or_default();
            let (result, requests) = crate::test_support::compaction_source::execute_wave(
                &service,
                &dispatch.task,
                Err(
                    MezError::new(crate::MezErrorKind::Forbidden, "offline request refused")
                        .with_provider_incurred_usage(mez_agent::ModelTokenUsage {
                            input_tokens: 17,
                            output_tokens: 3,
                            ..Default::default()
                        }),
                ),
            );
            assert_eq!(requests.len(), 1);
            assert!(result.is_err());
            let mut event = crate::test_support::compaction_source::model_result_event(
                "%1", generation, result,
            );
            if unknown_kind {
                let crate::runtime::AgentCompactionEvent::Failed { kind, .. } = &mut event else {
                    panic!("original failed inference result");
                };
                *kind = "unsupported-fixture-kind".into();
            }
            drop(dispatch);
            assert!(
                service
                    .apply_agent_compaction_transition(event.clone())
                    .unwrap()
                    .applied
            );
            assert_facts(&service, generation, "1", "1");
            let after = service.agent_token_usage_for_conversation(&conversation);
            let charged = after.get(&key).copied().unwrap_or_default();
            assert_eq!(charged.input_tokens, before.input_tokens + 17);
            assert_eq!(charged.output_tokens, before.output_tokens + 3);
            assert!(
                !service
                    .apply_agent_compaction_transition(event)
                    .unwrap()
                    .applied
            );
            assert_eq!(
                service.agent_token_usage_for_conversation(&conversation),
                after
            );
            assert!(!service.agent_is_compacting("%1"));
            assert!(service.pending_agent_provider_tasks().is_empty());
            assert!(store.compaction_epoch(&conversation).unwrap().is_none());
            assert_eq!(
                service.agent_turn_ledger().turn(&turn).unwrap().state,
                if manual {
                    AgentTurnState::Completed
                } else {
                    AgentTurnState::Failed
                }
            );
        }
    }
}

/// A failed concrete invocation retains its count across supervised backoff;
/// refusing the retry's timer cannot erase the original operation diagnostics.
#[test]
fn runtime_compaction_failure_diagnostics_survive_retry_timer_refusal() {
    let (mut service, _, _) = replay_observed::queued();
    let calls = run_runtime_test_compaction_wave(
        &mut service,
        "%1",
        Err(MezError::new(
            crate::MezErrorKind::Io,
            "offline transport failed",
        )),
    );
    assert_eq!(calls.len(), 1);
    let generation = service
        .pending_agent_compaction_task_generation("%1")
        .unwrap();
    assert!(
        service
            .agent_compaction_retry_delay("%1", generation)
            .is_some()
    );
    assert!(
        service
            .fail_agent_compaction_retry_admission("%1", generation)
            .unwrap()
    );
    assert!(!service.agent_is_compacting("%1"));
    assert_facts(&service, generation, "1", "1");
}

/// Internal source loss refuses before credentials. The surviving task identity
/// supplies operation/dispatch observations; missing model counters stay unknown.
#[test]
fn runtime_compaction_failure_diagnostics_mark_lost_source_counters_unknown() {
    let (mut service, _, _) = replay_observed::queued();
    let mut task = service.take_pending_agent_compaction_task("%1").unwrap();
    task.replay_source = None;
    service.queue_agent_compaction_task(task);
    let generation = service
        .pending_agent_compaction_task_generation("%1")
        .unwrap();
    let error = service
        .claim_agent_compaction_task("%1", generation)
        .err()
        .expect("missing source cannot construct a worker");
    assert!(error.message().contains("source"));
    assert!(!service.agent_is_compacting("%1"));
    assert_facts(&service, generation, "unknown", "unknown");
    assert!(visible(&service).contains("phase=unavailable"));
}
