//! Immutable project accounting capture over worker-qualified mappings.
//!
//! Trust is evaluated at request freeze, never inferred from mapping possession.
//! Claim provenance survives cwd/revocation while new work uses current state.

use super::*;
use crate::storage::token_usage::{AccountingOrigin, TokenUsageStore};

/// Retiring memory content ownership must retain exact issued expense. A late
/// result charges the original conversation once but cannot mutate replacement
/// memory work, latest samples, or a replacement pane's view.
#[test]
fn runtime_accounting_memory_cancelled_completion_retains_expense() {
    let base = temp_root("accounting-memory-cancelled");
    fs::create_dir_all(&base).unwrap();
    let mut service = test_runtime_service();
    let store = TokenUsageStore::new(base.join("usage.sqlite"));
    service.set_token_usage_store(store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let task = crate::runtime::RuntimeAgentRememberTask {
        pane_id: "%1".into(),
        conversation_id: conversation.clone(),
        accounting_origin: AccountingOrigin::Unattributed,
        observation_id: "memory-issued".into(),
        model_profile_name: "fixture".into(),
        model_profile: mez_agent::ModelProfile::default(),
        scope: mez_agent::memory::MemoryScope::Global,
        request: runtime_model_request_fixture_for_agent("memory", "agent-%1"),
    };
    service.claim_agent_remember_task_state("%1", task);
    service.fail_agent_remember_task("%1");
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "memory-replacement", 0)
        .unwrap();
    let event = crate::runtime::AgentRememberEvent::Failed {
        pane_id: "%1".into(),
        observation_id: "memory-issued".into(),
        usage: mez_agent::ModelTokenUsage {
            input_tokens: 23,
            output_tokens: 2,
            ..Default::default()
        },
        kind: "invalid_state".into(),
        message: "reported cutoff".into(),
        provider_failure_json: None,
        provider_raw_text: None,
    };
    assert!(
        !service
            .apply_agent_remember_transition(event.clone())
            .unwrap()
            .applied
    );
    assert!(
        !service
            .apply_agent_remember_transition(event)
            .unwrap()
            .applied
    );
    assert_eq!(
        service
            .agent_token_usage_for_conversation(&conversation)
            .values()
            .next()
            .unwrap()
            .input_tokens,
        23
    );
    assert!(
        service
            .agent_token_usage_for_conversation("memory-replacement")
            .is_empty()
    );
    assert!(service.agent_token_usage_for_pane("%1").is_empty());
    assert_eq!(
        store
            .history_snapshot(
                crate::runtime::current_unix_seconds(),
                &[1],
                &crate::storage::token_usage::TokenHistoryScope::default()
            )
            .unwrap()
            .windows[&1]
            .values()
            .next()
            .unwrap()
            .usage
            .input_tokens,
        23
    );
    fs::remove_dir_all(base).unwrap();
}

/// Managed provider continuations must observe standalone persisted revocation
/// after mapping preparation. The earlier issued dispatch keeps its origin;
/// the fresh dispatch must not charge the revoked project. No provider I/O runs.
#[tokio::test]
async fn runtime_accounting_origin_managed_dispatch_observes_persisted_revocation() {
    let base = temp_root("accounting-managed-revocation");
    let project = base.join("project");
    fs::create_dir_all(&project).unwrap();
    let mut service = test_runtime_service();
    service.replace_config_layers(vec![ConfigLayer {
        name: "accounting-managed".to_string(), path: None,
        format: ConfigFormat::Toml, scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\nshell_mode = \"pane\"\ndefault_provider = \"openai\"\ndefault_model_profile = \"accounting-test\"\n[permissions]\nsandbox = \"policy-only\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-test\"]\n[model_profiles.accounting-test]\nprovider = \"openai\"\nmodel = \"gpt-test\"\n".to_string(),
    }]).unwrap();
    let auth = AuthStore::new(crate::security::auth::AuthPaths::under_config_root(
        &base.join("auth"),
    ));
    let credentials = auth.file_credential_store("openai").unwrap();
    auth.login_openai_api_key("accounting-test", "sk-accounting-fixture", &credentials)
        .unwrap();
    service.set_auth_store(auth);
    service.set_token_usage_store(TokenUsageStore::new(base.join("usage.sqlite")));
    let trust_path = base.join("trust.sqlite");
    let mut trust = ProjectTrustStore::default();
    trust
        .decide(project.clone(), TrustDecision::Trusted, None)
        .unwrap();
    trust.save_to_file(&trust_path).unwrap();
    service.set_project_trust_store(trust.clone(), Some(trust_path.clone()));
    service.set_pane_current_working_directory("%1".to_string(), project.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let signature = mez_agent::EnvironmentSignature::new(
        "linux",
        "x86_64",
        None,
        "test-host",
        "test-user",
        None,
        "/bin/sh",
        mez_agent::ShellClassification::PosixSh,
        None,
        Some("/usr/bin:/bin".to_string()),
        project.to_string_lossy(),
        None,
        false,
        None,
        Vec::new(),
    )
    .unwrap();
    service.set_pane_environment_signature_for_tests("%1", signature);
    mark_test_pane_ready(&mut service, "%1");
    let started = service
        .start_agent_prompt_turn("%1", "freeze managed attribution")
        .unwrap();
    let preparation = service
        .prepare_agent_provider_work(&started.turn_id)
        .unwrap();
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(preparation).await;
    service.apply_agent_provider_preparation(outcome).unwrap();
    let request = service
        .primary_path_resolution_request("%1")
        .unwrap()
        .unwrap();
    let command = mez_agent::shell::pane_path_resolution_command(
        &request,
        mez_agent::ShellClassification::PosixSh,
    )
    .unwrap();
    let output = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .current_dir(&project)
        .output()
        .unwrap();
    assert!(output.status.success());
    let key = service.path_resolution_cache_key("%1", &request).unwrap();
    service
        .observe_path_resolution_transaction_end(
            "accounting-managed-resolution",
            "%1",
            0,
            key,
            &String::from_utf8(output.stdout).unwrap(),
            false,
        )
        .unwrap();
    let agent = AgentId::opaque("agent-%1".to_string()).unwrap();
    let first = service
        .claim_configured_agent_provider_task(&agent, &started.turn_id)
        .unwrap()
        .expect("managed request should be claimable");
    assert!(matches!(
        first.accounting_origin,
        AccountingOrigin::Project(_)
    ));
    service
        .record_claimed_agent_provider_task(&first, 1, 30_000)
        .unwrap();
    trust.decide(project, TrustDecision::Revoked, None).unwrap();
    trust.save_to_file(&trust_path).unwrap();
    service.queue_agent_provider_task(started.turn_id.clone());
    let fresh = service
        .claim_configured_agent_provider_task(&agent, &started.turn_id)
        .unwrap()
        .expect("revoked root should not block policy-only provider work");
    assert_eq!(fresh.accounting_origin, AccountingOrigin::Unattributed);
    assert_eq!(
        service.claimed_accounting_origin_for_tests(&started.turn_id),
        Some(&first.accounting_origin)
    );
    fs::remove_dir_all(base).unwrap();
}

/// A worker prepared from a prior trust inventory cannot replace the current
/// mapping cache after revocation. New capture remains unattributed, while the
/// worker may retain historical metadata without granting project authority.
#[tokio::test]
async fn runtime_accounting_origin_rejects_stale_preparation_inventory() {
    let base = temp_root("accounting-stale-inventory");
    fs::create_dir_all(&base).unwrap();
    let mut service = test_runtime_service();
    service.set_token_usage_store(TokenUsageStore::new(base.join("usage.sqlite")));
    let mut trust = ProjectTrustStore::default();
    trust
        .decide(base.clone(), TrustDecision::Trusted, None)
        .unwrap();
    service.set_project_trust_store(trust.clone(), None);
    service.set_pane_current_working_directory("%1".to_string(), base.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "prepare mapping only")
        .unwrap();
    let preparation = service
        .prepare_agent_provider_work(&started.turn_id)
        .unwrap();
    trust
        .decide(base.clone(), TrustDecision::Revoked, None)
        .unwrap();
    service.set_project_trust_store(trust, None);
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(preparation).await;
    service.apply_agent_provider_preparation(outcome).unwrap();
    assert!(service.persistence.accounting_projects().is_none());
    assert_eq!(
        service.capture_accounting_origin_for_pane("%1"),
        AccountingOrigin::Unattributed
    );
    fs::remove_dir_all(base).unwrap();
}

/// A real configured provider dispatch captures project attribution before any
/// provider I/O. The claim retains that origin after cwd changes, independently
/// of its execution generation; no paid request is needed to test this boundary.
#[tokio::test]
async fn runtime_accounting_origin_production_dispatch_retains_frozen_project() {
    let base = temp_root("accounting-dispatch-freeze");
    let a = base.join("a");
    let b = base.join("b");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    let mut service = test_runtime_service();
    service.replace_config_layers(vec![ConfigLayer {
        name: "accounting-dispatch".to_string(), path: None,
        format: ConfigFormat::Toml, scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\nshell_mode = \"native\"\ndefault_provider = \"openai\"\ndefault_model_profile = \"accounting-test\"\n[permissions]\nsandbox = \"policy-only\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-test\"]\n[model_profiles.accounting-test]\nprovider = \"openai\"\nmodel = \"gpt-test\"\n".to_string(),
    }]).unwrap();
    let auth = AuthStore::new(crate::security::auth::AuthPaths::under_config_root(
        &base.join("auth"),
    ));
    let credentials = auth.file_credential_store("openai").unwrap();
    auth.login_openai_api_key("accounting-test", "sk-accounting-fixture", &credentials)
        .unwrap();
    service.set_auth_store(auth);
    service.set_token_usage_store(TokenUsageStore::new(base.join("usage.sqlite")));
    let mut trust = ProjectTrustStore::default();
    trust
        .decide(a.clone(), TrustDecision::Trusted, None)
        .unwrap();
    trust
        .decide(b.clone(), TrustDecision::Trusted, None)
        .unwrap();
    service.set_project_trust_store(trust, None);
    service
        .start_initial_pane_process_with_start_directory(None, &a)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "freeze attribution only")
        .unwrap();
    let preparation = service
        .prepare_agent_provider_work(&started.turn_id)
        .unwrap();
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(preparation).await;
    service.apply_agent_provider_preparation(outcome).unwrap();
    let dispatch = service
        .claim_configured_agent_provider_task(
            &AgentId::opaque("agent-%1".to_string()).unwrap(),
            &started.turn_id,
        )
        .unwrap()
        .expect("configured native dispatch should be claimable");
    let frozen = dispatch.accounting_origin.clone();
    assert!(matches!(frozen, AccountingOrigin::Project(_)));
    service.set_pane_current_working_directory("%1".to_string(), b);
    assert_ne!(service.capture_accounting_origin_for_pane("%1"), frozen);
    service
        .record_claimed_agent_provider_task(&dispatch, 7, 30_000)
        .unwrap();
    assert_eq!(
        service.claimed_accounting_origin_for_tests(&started.turn_id),
        Some(&frozen)
    );
    let agent = AgentId::opaque("agent-%1".to_string()).unwrap();
    let key = mez_agent::ModelTokenUsageKey::new(
        &dispatch.model_profile.provider,
        &dispatch.model_profile.model,
    );
    let usage = std::collections::BTreeMap::from([(
        key.clone(),
        mez_agent::ModelTokenUsage {
            input_tokens: 17,
            output_tokens: 3,
            cached_input_tokens: Some(0),
            ..Default::default()
        },
    )]);
    // Retiring content ownership must not retire exact incurred-expense evidence.
    service.clear_claimed_agent_provider_task(&started.turn_id);
    assert!(!service.settle_provider_request_usage(&agent, &started.turn_id, 8, &usage));
    assert!(service.settle_provider_request_usage(&agent, &started.turn_id, 7, &usage));
    assert!(!service.settle_provider_request_usage(&agent, &started.turn_id, 7, &usage));
    let partitions = service.project_usage_for_conversation(&dispatch.turn.conversation_id);
    assert_eq!(partitions.len(), 1);
    assert_eq!(
        partitions[0].project_id.as_deref(),
        frozen.project_id().map(|id| id.as_str())
    );
    assert_eq!(partitions[0].usage, usage[&key]);
    assert_eq!(
        service.agent_token_usage_for_conversation(&dispatch.turn.conversation_id)[&key],
        usage[&key]
    );
    assert!(
        service
            .agent_latest_request_usage(&dispatch.turn.conversation_id)
            .is_none()
    );
    assert!(service.reset_agent_token_usage_for_pane("%1"));
    assert!(service.project_usage_for_pane("%1").is_empty());
    assert_eq!(
        service.project_usage_for_conversation(&dispatch.turn.conversation_id),
        partitions
    );
    let store = TokenUsageStore::new(base.join("usage.sqlite"));
    let history = store
        .history_snapshot(
            crate::runtime::current_unix_seconds(),
            &[1],
            &crate::storage::token_usage::TokenHistoryScope::default(),
        )
        .unwrap();
    assert_eq!(
        history.windows[&1].values().next().unwrap().usage,
        usage[&key]
    );
    let mut restored = test_runtime_service();
    restored.replace_restored_agent_token_usage(
        &dispatch.turn.conversation_id,
        "%1",
        usage.clone(),
    );
    restored.restore_project_usage(
        &dispatch.turn.conversation_id,
        "%1",
        partitions.clone(),
        false,
    );
    assert_eq!(
        restored.project_usage_for_conversation(&dispatch.turn.conversation_id),
        partitions
    );
    assert_eq!(restored.project_usage_for_pane("%1"), partitions);
    assert_eq!(
        store
            .history_snapshot(
                crate::runtime::current_unix_seconds(),
                &[1],
                &crate::storage::token_usage::TokenHistoryScope::default()
            )
            .unwrap()
            .windows[&1]
            .values()
            .next()
            .unwrap()
            .usage,
        usage[&key]
    );
    service
        .record_claimed_agent_provider_task(&dispatch, 9, 30_000)
        .unwrap();
    service
        .agent_turn_contexts_mut()
        .get_mut(&started.turn_id)
        .unwrap()
        .append_user_event("steering", "new instruction after dispatch")
        .unwrap();
    let mut request =
        runtime_model_request_fixture_for_agent(&started.turn_id, &dispatch.turn.agent_id);
    request.provider = dispatch.model_profile.provider.clone();
    request.model = dispatch.model_profile.model.clone();
    let execution = mez_agent::AgentTurnExecution {
        request,
        response: mez_agent::ModelResponse {
            provider: dispatch.model_profile.provider.clone(),
            model: dispatch.model_profile.model.clone(),
            raw_text: "stale content".into(),
            usage: mez_agent::ModelTokenUsage {
                input_tokens: 5,
                ..Default::default()
            },
            latest_request_usage: None,
            quota_usage: Vec::new(),
            action_batch: None,
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: Vec::new(),
        final_turn: true,
        terminal_state: AgentTurnState::Completed,
    };
    let (handle, actor) = crate::host::async_runtime::AsyncRuntimeSessionActor::new(
        service,
        crate::host::async_runtime::AsyncRuntimeActorConfig::default(),
    )
    .unwrap();
    let client = async {
        for expected in [1, 0] {
            let mut batch = crate::runtime::RuntimeEventBatch::new();
            batch.push(crate::runtime::RuntimeEvent::AgentProvider(
                crate::runtime::AgentProviderEvent::Completed {
                    agent_id: agent.clone(),
                    turn_id: started.turn_id.clone(),
                    claim_generation: 9,
                    execution: Box::new(execution.clone()),
                },
            ));
            assert_eq!(
                handle.submit_runtime_events(batch).await.unwrap().applied,
                expected
            );
        }
        handle.shutdown().await.unwrap();
    };
    let ((), mut exit) = tokio::join!(client, actor.run());
    assert_eq!(
        exit.service
            .agent_token_usage_for_conversation(&dispatch.turn.conversation_id)[&key]
            .input_tokens,
        22
    );
    let partitions = exit
        .service
        .project_usage_for_conversation(&dispatch.turn.conversation_id);
    assert_eq!(
        partitions[0].project_id.as_deref(),
        frozen.project_id().map(|id| id.as_str())
    );
    assert_eq!(partitions[0].usage.input_tokens, 22);
    assert!(
        exit.service
            .agent_latest_request_usage(&dispatch.turn.conversation_id)
            .is_none()
    );
    // A router is a distinct paid producer, not the ordinary execution model.
    struct CutoffRouter;
    impl crate::integrations::agent::provider::AsyncModelProvider for CutoffRouter {
        fn provider_id(&self) -> &str {
            "router-provider"
        }
        fn api_compatibility(&self) -> mez_agent::ProviderApiCompatibility {
            mez_agent::ProviderApiCompatibility::OpenAiResponses
        }
        fn send_request_async<'a>(
            &'a self,
            _request: &'a mez_agent::ModelRequest,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<mez_agent::ModelResponse>> + Send + 'a>,
        > {
            Box::pin(async {
                Err(MezError::invalid_state("router cutoff")
                    .with_provider_failure_json(r#"{"error":{"code":"max_tokens"}}"#)
                    .with_provider_output_limit_state(mez_agent::ProviderOutputLimitState::new(
                        "router-provider",
                        "openai-responses",
                        "max_tokens",
                        None,
                        "",
                        0,
                        0,
                        mez_agent::ModelTokenUsage {
                            input_tokens: 31,
                            output_tokens: 4,
                            ..Default::default()
                        },
                        mez_agent::ProviderOutputLimitContinuationDisposition::ContinueVisibleText,
                    )))
            })
        }
    }
    let router_profile = mez_agent::ModelProfile {
        provider: "router-provider".into(),
        model: "router-model".into(),
        ..Default::default()
    };
    let target = |size: &str| mez_agent::AutoSizingTargetProfile {
        size: size.into(),
        profile_name: "accounting-test".into(),
        profile: dispatch.model_profile.clone(),
        supported_reasoning_efforts: Vec::new(),
    };
    let routing = mez_agent::AutoSizingDispatch {
        router_profile_name: "router-fixture".into(),
        router_profile: router_profile.clone(),
        default_profile_name: "accounting-test".into(),
        default_profile: dispatch.model_profile.clone(),
        small: target("small"),
        medium: target("medium"),
        large: target("large"),
        turn_metadata: None,
        allowed_reasoning_efforts: Vec::new(),
        fallback_policy: mez_agent::AutoSizingFallbackPolicy::UseDefaultProfile,
    };
    let mut routed_dispatch = dispatch.clone();
    routed_dispatch.auto_sizing = Some(routing.clone());
    exit.service
        .record_claimed_agent_provider_task(&routed_dispatch, 10, 30_000)
        .unwrap();
    let error = crate::runtime::runtime_execute_auto_sizing_with_async_provider(
        &CutoffRouter,
        &routing,
        &dispatch.turn,
        dispatch.context.durable(),
        mez_agent::AllowedActionSet::say_only(),
    )
    .await
    .unwrap_err();
    assert_eq!(
        error
            .provider_output_limit_state()
            .unwrap()
            .usage
            .input_tokens,
        31
    );
    exit.service
        .clear_claimed_agent_provider_task(&started.turn_id);
    assert!(exit.service.settle_provider_cutoff_usage(
        &agent,
        &started.turn_id,
        10,
        error.provider_output_limit_state()
    ));
    assert!(!exit.service.settle_provider_cutoff_usage(
        &agent,
        &started.turn_id,
        10,
        error.provider_output_limit_state()
    ));
    let router_key = mez_agent::ModelTokenUsageKey::new("router-provider", "router-model");
    let partitions = exit
        .service
        .project_usage_for_conversation(&dispatch.turn.conversation_id);
    let router = partitions
        .iter()
        .find(|row| row.model == router_key)
        .unwrap();
    assert_eq!(
        router.project_id.as_deref(),
        frozen.project_id().map(|id| id.as_str())
    );
    assert_eq!(router.usage.input_tokens, 31);
    assert_eq!(
        exit.service
            .agent_token_usage_for_conversation(&dispatch.turn.conversation_id)[&key]
            .input_tokens,
        22
    );
    assert!(
        exit.service
            .agent_latest_request_usage(&dispatch.turn.conversation_id)
            .is_none()
    );
    for effect in exit.service.persistence.take_token_usage_effects() {
        if let RuntimeSideEffect::PersistTokenUsage { store, event } = effect {
            store.append(&event).unwrap();
        }
    }
    let history = store
        .history_snapshot(
            crate::runtime::current_unix_seconds(),
            &[1],
            &crate::storage::token_usage::TokenHistoryScope::default(),
        )
        .unwrap();
    assert_eq!(
        history.windows[&1]
            .iter()
            .find(|(key, _)| key.model == router_key)
            .unwrap()
            .1
            .usage
            .input_tokens,
        31
    );
    exit.service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(base).unwrap();
}

/// Compatibility ingress without an issued accounting owner rejects stale
/// content without manufacturing expense from an unowned completion payload.
#[tokio::test]
async fn runtime_accounting_claim_only_stale_completion_does_not_invent_expense() {
    let base = temp_root("accounting-stale-completion");
    fs::create_dir_all(&base).unwrap();
    let mut service = test_runtime_service();
    let store = TokenUsageStore::new(base.join("usage.sqlite"));
    service.set_token_usage_store(store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "observe expense only")
        .unwrap();
    service
        .record_claimed_agent_provider_context_for_tests(&started.turn_id, 0)
        .unwrap();
    // A test claim has no issued-accounting owner; install one from a real
    // dispatch is covered above. This case qualifies the compatibility ingress
    // and verifies that stale-context refusal itself leaves totals untouched.
    let turn = service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .unwrap()
        .clone();
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&started.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "fixture".into(),
            model: "fixture".into(),
            raw_text: "stale content".into(),
            usage: mez_agent::ModelTokenUsage {
                input_tokens: 5,
                ..Default::default()
            },
            latest_request_usage: None,
            quota_usage: Vec::new(),
            action_batch: None,
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: Vec::new(),
        final_turn: true,
        terminal_state: AgentTurnState::Completed,
    };
    execution.request.agent_id = turn.agent_id.clone();
    service
        .agent_turn_contexts_mut()
        .get_mut(&started.turn_id)
        .unwrap()
        .append_user_event("steering", "new instruction")
        .unwrap();
    assert!(
        service
            .apply_agent_provider_completed_event(
                &AgentId::opaque(turn.agent_id).unwrap(),
                &started.turn_id,
                execution
            )
            .await
            .unwrap()
    );
    assert!(
        service
            .agent_token_usage_for_conversation(&turn.conversation_id)
            .is_empty()
    );
    assert!(
        store
            .history_snapshot(
                crate::runtime::current_unix_seconds(),
                &[1],
                &crate::storage::token_usage::TokenHistoryScope::default()
            )
            .unwrap()
            .windows[&1]
            .is_empty()
    );
    fs::remove_dir_all(base).unwrap();
}

/// Production preparation qualifies inventory off actor ownership. A captured
/// claim remains in project A after cwd moves to B; a fresh origin uses B, and
/// deepest rejection/revocation withholds attribution without ancestor fallback.
#[tokio::test]
async fn runtime_accounting_origin_freezes_claim_and_rechecks_current_trust() {
    let base = temp_root("accounting-origin-freeze");
    let a = base.join("a");
    let b = base.join("b");
    let nested = a.join("nested");
    fs::create_dir_all(&nested).unwrap();
    fs::create_dir_all(&b).unwrap();
    let mut service = test_runtime_service();
    let store = TokenUsageStore::new(base.join("usage.sqlite"));
    service.set_token_usage_store(store);
    let mut trust = ProjectTrustStore::default();
    trust
        .decide(a.clone(), TrustDecision::Trusted, None)
        .unwrap();
    trust
        .decide(b.clone(), TrustDecision::Trusted, None)
        .unwrap();
    service.set_project_trust_store(trust.clone(), None);
    service.set_pane_current_working_directory("%1".to_string(), a.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "capture accounting origin")
        .unwrap();
    assert_eq!(
        service.capture_accounting_origin_for_pane("%1"),
        AccountingOrigin::Unattributed
    );
    let preparation = service
        .prepare_agent_provider_work(&started.turn_id)
        .unwrap();
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(preparation).await;
    service.apply_agent_provider_preparation(outcome).unwrap();
    let origin_a = service.capture_accounting_origin_for_pane("%1");
    assert!(matches!(origin_a, AccountingOrigin::Project(_)));
    service
        .record_claimed_agent_provider_context_for_tests(&started.turn_id, 0)
        .unwrap();
    service.set_pane_current_working_directory("%1".to_string(), b.clone());
    let origin_b = service.capture_accounting_origin_for_pane("%1");
    assert!(matches!(origin_b, AccountingOrigin::Project(_)));
    assert_ne!(origin_a, origin_b);
    assert_eq!(
        service.claimed_accounting_origin_for_tests(&started.turn_id),
        Some(&origin_a)
    );
    for decision in [TrustDecision::Rejected, TrustDecision::Revoked] {
        trust.decide(nested.clone(), decision, None).unwrap();
        service.set_project_trust_store(trust.clone(), None);
        service.set_pane_current_working_directory("%1".to_string(), nested.clone());
        assert_eq!(
            service.capture_accounting_origin_for_pane("%1"),
            AccountingOrigin::Unattributed
        );
        assert_eq!(
            service.claimed_accounting_origin_for_tests(&started.turn_id),
            Some(&origin_a)
        );
    }
    service.set_project_trust_store(ProjectTrustStore::default(), None);
    assert_eq!(
        service.capture_accounting_origin_for_pane("%1"),
        AccountingOrigin::Unattributed
    );
    fs::remove_dir_all(base).unwrap();
}
