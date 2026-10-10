//! Canonical failure provenance at the accepted actor settlement boundary.
//!
//! Synthetic worker events exercise claim fencing and terminal conversion, not
//! provider parsing. The real provider pipeline has separate recovery fixtures.
//! Facts cannot reopen a recovery budget or disappear from terminal evidence.

use super::*;

/// An accepted exhausted canonical failure must settle once even with unused
/// runtime repair capacity, retaining the known facts in its failed execution
/// and append-only transcript. Failed best-effort summaries cannot erase those
/// facts or requeue provider work; the vendor/model cause remains intact.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_canonical_failure_preserves_terminal_recovery_facts() {
    use mez_agent::recovery_attribution::RecoveryDisposition;
    for disposition in [
        RecoveryDisposition::BudgetExhausted,
        RecoveryDisposition::InteractionLimit,
        RecoveryDisposition::RepairUnavailable,
        RecoveryDisposition::RepairRequestFailed,
    ] {
        qualify_terminal_facts(disposition, None, false).await;
    }
}

/// Request identity mismatch and stale claim completion cannot attach another
/// worker's diagnostic facts. Duplicate accepted failure delivery is inert;
/// only the first exact claim can create terminal execution/transcript evidence.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_canonical_failure_fences_mismatched_stale_and_duplicate_facts() {
    use mez_agent::recovery_attribution::RecoveryDisposition;
    for field in ["turn", "agent", "provider", "model"] {
        qualify_terminal_facts(RecoveryDisposition::BudgetExhausted, Some(field), false).await;
    }
    qualify_terminal_facts(RecoveryDisposition::BudgetExhausted, None, true).await;
}

/// Owns one finite actor/persistence fixture; no sleeps or provider work are
/// needed to select each exact claim and terminal provenance branch.
async fn qualify_terminal_facts(
    disposition: mez_agent::recovery_attribution::RecoveryDisposition,
    mismatch: Option<&str>,
    stale: bool,
) {
    let root = std::env::temp_dir().join(format!(
        "mez-terminal-facts-{:032x}",
        rand::random::<u128>()
    ));
    std::fs::create_dir(&root).unwrap();
    let store = AgentTranscriptStore::new(root.join("transcripts"));
    let mut service = test_service();
    service.set_agent_transcript_store(store.clone());
    service.set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(
        root.join("usage.sqlite"),
    ));
    service.set_auth_store(crate::security::auth::AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&root.join("auth")),
    ));
    service.replace_config_layers(vec![ConfigLayer {
        name: "canonical-failure-claim".into(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\nrouting=false\nshell_mode=\"native\"\ndefault_provider=\"local-chat\"\ndefault_model_profile=\"default\"\n[permissions]\nsandbox=\"policy-only\"\n[providers.local-chat]\nkind=\"openai-compatible\"\nbase_url=\"http://127.0.0.1:1/v1\"\nmodels=[\"test-model\"]\ndefault_model=\"test-model\"\n[model_profiles.default]\nprovider=\"local-chat\"\nmodel=\"test-model\"\n".into(),
    }]).unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "settle failed correction")
        .unwrap();
    let task = service.pending_agent_provider_tasks().remove(0);
    let turn = service
        .agent_turn_ledger()
        .turn(&task.turn_id)
        .unwrap()
        .clone();
    let facts = mez_agent::recovery_attribution::RecoveryFacts {
        turn_id: turn.turn_id.clone(),
        agent_id: turn.agent_id.clone(),
        provider: task.model_profile.provider.clone(),
        model: task.model_profile.model.clone(),
        phase: 0,
        attempts_reserved: 2,
        attempt_limit: 2,
        provider_calls_started: 3,
        provider_call_limit: 16,
        repair_calls_started: 2,
        last_request_kind: mez_agent::ModelInteractionKind::MaapRepair,
        stage: Some(mez_agent::recovery_attribution::RecoveryStage::ProviderOutput),
        disposition,
        batch_planned: false,
    };
    let mut emitted = facts.clone();
    match mismatch {
        Some("turn") => emitted.turn_id = "another-turn".into(),
        Some("agent") => emitted.agent_id = "agent-%999".into(),
        Some("provider") => emitted.provider = "another-provider".into(),
        Some("model") => emitted.model = "another-model".into(),
        None => {}
        _ => unreachable!("fixture names only four identity fields"),
    }
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let client = async {
        let dispatch = handle
            .claim_configured_agent_provider_task(
                AgentId::opaque(turn.agent_id.clone()).unwrap(),
                turn.turn_id.clone(),
            )
            .await
            .unwrap()
            .unwrap();
        let event = RuntimeEvent::AgentProvider(AgentProviderEvent::CanonicalFailure {
            failure: Box::new(AgentProviderEvent::Failed {
                agent_id: AgentId::opaque(turn.agent_id.clone()).unwrap(),
                turn_id: turn.turn_id.clone(),
                claim_generation: dispatch.claim_generation + u64::from(stale),
                kind: "invalid_state".into(),
                message: if mismatch.is_some() {
                    "mock nonretryable authentication failure"
                } else {
                    "provider MAAP output is malformed: invalid JSON"
                }
                .into(),
                provider_failure_json: None,
                provider_raw_text: None,
                provider_output_limit_state: None,
            }),
            recovery: Box::new(emitted.clone()),
            usage: mez_agent::ModelTokenUsage {
                input_tokens: 7,
                output_tokens: 3,
                ..Default::default()
            },
        });
        let mut batch = RuntimeEventBatch::new();
        batch.push(event.clone());
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.applied, usize::from(!stale));
        assert_eq!(
            handle.agent_turn_is_running(&turn.turn_id).await.unwrap(),
            stale
        );
        if !stale {
            let mut duplicate = RuntimeEventBatch::new();
            duplicate.push(event);
            assert_eq!(
                handle
                    .submit_runtime_events(duplicate)
                    .await
                    .unwrap()
                    .applied,
                0
            );
            assert!(
                handle
                    .pending_agent_provider_tasks()
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
        run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 4,
                drain_limit: 64,
                idle_interval: Duration::from_millis(1),
            },
            |_, _| false,
        )
        .await
        .unwrap();
        if !stale {
            let view = handle
                .render_client_view(
                    ClientViewRole::Primary,
                    Size::new(80, 24).unwrap(),
                    TerminalClientLoopConfig::default(),
                )
                .await
                .unwrap()
                .unwrap();
            let text = view.lines.join("\n");
            if mismatch.is_none() {
                assert!(
                    text.contains(&format!("disposition={disposition:?}")),
                    "{text}"
                );
            } else {
                assert!(text.contains("provenance unavailable"), "{text}");
            }
        }
        handle.shutdown().await.unwrap();
    };
    let ((), mut exit) = tokio::join!(client, actor.run());
    assert_eq!(
        exit.service
            .agent_turn_ledger()
            .turn(&turn.turn_id)
            .unwrap()
            .state,
        if stale {
            mez_agent::AgentTurnState::Running
        } else {
            mez_agent::AgentTurnState::Failed
        }
    );
    let entries = if stale {
        assert_eq!(
            store.inspect(&turn.conversation_id).unwrap_err().kind(),
            crate::error::MezErrorKind::NotFound
        );
        Vec::new()
    } else {
        store.inspect(&turn.conversation_id).unwrap()
    };
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry.content.contains("canonical recovery facts"))
            .count(),
        usize::from(!stale && mismatch.is_none())
    );
    if !stale && mismatch.is_none() {
        assert!(
            entries
                .iter()
                .any(|entry| entry.content.contains(&format!("{disposition:?}")))
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry.content.contains("provider MAAP output is malformed"))
        );
    }
    let usage = exit
        .service
        .agent_token_usage_for_conversation(&turn.conversation_id);
    assert_eq!(
        usage
            .values()
            .map(|sample| sample.input_tokens)
            .sum::<u64>(),
        if stale { 0 } else { 7 }
    );
    assert_eq!(
        usage
            .values()
            .map(|sample| sample.output_tokens)
            .sum::<u64>(),
        if stale { 0 } else { 3 }
    );
    assert!(
        exit.service
            .agent_latest_request_usage(&turn.conversation_id)
            .is_none()
    );
    assert!(
        exit.service
            .agent_context_usage_snapshot(&turn.conversation_id)
            .is_none()
    );
    exit.service.terminate_all_pane_processes().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
