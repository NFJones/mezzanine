//! Assembled provider selection and actual process-free filesystem execution.
//!
//! These cases accept a real provider action batch before native selection;
//! unlike direct queue fixtures they cannot bypass shell lowering accidentally.
//! The original actor/producer owns actual descriptor execution and settlement.

use super::async_producer::ActorFixture;
use super::*;
use crate::host::async_runtime::native_filesystem::{
    claim_and_start_native_filesystem_producer, transfer_joined_native_filesystem_producer,
};

/// Accepts a provider patch through ordinary runtime finalization, with an actual
/// pane root and explicit private filesystem grants. No manual queue is used.
fn selected() -> Fixture {
    selected_with_approval(false)
}

/// Controls only the real initial policy gate; source admission remains ordinary.
fn selected_with_approval(prompt: bool) -> Fixture {
    selected_with_policy(if prompt {
        mez_agent::ApprovalPolicy::Ask
    } else {
        mez_agent::ApprovalPolicy::FullAccess
    })
}

/// Selects an actual approval policy before the accepted provider response.
fn selected_with_policy(policy: mez_agent::ApprovalPolicy) -> Fixture {
    selected_with_setup(policy, |_, _| {})
}

/// Installs a policy/source fixture before the actual provider response is accepted.
/// Setup may configure intent or seed a real competing file, never outcome facts.
fn selected_with_setup(
    policy: mez_agent::ApprovalPolicy,
    setup: impl FnOnce(&mut RuntimeSessionService, &std::path::Path),
) -> Fixture {
    let prompt = matches!(
        policy,
        mez_agent::ApprovalPolicy::Ask | mez_agent::ApprovalPolicy::AutoAllow
    );
    let root = temp_root("native-patch-selector");
    fs::create_dir_all(&root).unwrap();
    let root = fs::canonicalize(root).unwrap();
    let config_root = root.join("configuration");
    for catalog in ["skills", "macros"] {
        fs::create_dir_all(config_root.join(catalog)).unwrap();
    }
    let mut service = test_runtime_service();
    service.set_config_root(config_root.clone());
    service.set_auth_store(crate::security::auth::AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&config_root),
    ));
    service.replace_config_layers(vec![ConfigLayer {
        name: "native-filesystem-offline-provider".into(), path: None,
        format: ConfigFormat::Toml, scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider=\"runtime-batch\"\ndefault_model_profile=\"default\"\nshell_mode=\"native\"\n[permissions]\napproval_policy=\"ask\"\nsandbox=\"policy-only\"\n[providers.runtime-batch]\nkind=\"openai-compatible\"\nbase_url=\"http://127.0.0.1:9/v1\"\nmodels=[\"test\"]\ndefault_model=\"test\"\n[model_profiles.default]\nprovider=\"runtime-batch\"\nmodel=\"test\"\ncontext_window_tokens=128000\n".into(),
    }]).unwrap();
    service
        .start_initial_pane_process_with_launch_context(Some("cat"), Some(&root), None)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_agent_native_shell_mode_for_tests("%1");
    let permissions = crate::runtime::config::runtime_configured_permissions_from_config(
        &serde_json::json!({"permissions":{"read_scopes":[root],"write_scopes":[root]}}),
    )
    .unwrap();
    service
        .integration
        .replace_configured_permissions(permissions);
    service.permission_policy_mut().set_approval_bypass(!prompt);
    if prompt {
        *service.permission_policy_mut() = service
            .permission_policy()
            .clone()
            .with_approval_policy(policy);
        service.permission_policy_mut().add_rule(
            mez_agent::permissions::CommandRule::new(
                ["apply_patch"],
                RuleDecision::Prompt,
                RuleMatch::Exact,
            )
            .unwrap(),
        );
    }
    service.set_pane_approval_policy_override("%1", Some(policy));
    setup(&mut service, &root);
    let start = service
        .start_agent_prompt_turn("%1", "create a native file")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turn(&start.turn_id)
        .unwrap()
        .clone();
    service.remove_pending_agent_provider_task(&turn.turn_id);
    let action = mez_agent::AgentAction {
        id: "selected-native-patch".into(),
        payload: mez_agent::AgentActionPayload::ApplyPatch {
            patch: format!(
                "*** Begin Patch\n*** Add File: {}\n+selected native data\n*** End Patch",
                root.join("file").display()
            ),
            strip: None,
        },
    };
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".into(),
            model: "test".into(),
            raw_text: "selected native patch".into(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "native selection".into(),
                actions: vec![action.clone()],
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    let execution = service
        .execute_agent_turn_with_provider(
            &turn.turn_id,
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    Fixture {
        service,
        root,
        turn,
        action,
        execution,
    }
}

/// An actual provider-selected native patch must enter filesystem ownership
/// before any shell transaction or PTY payload. Its actual descriptor worker
/// confirms one file and returns native metadata without fabricated shell facts.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_selector_provider_patch_never_queues_shell_work() {
    let mut fixture = selected();
    assert_eq!(fixture.execution.terminal_state, AgentTurnState::Running);
    let offers = fixture.service.pending_native_filesystem_actions();
    assert_eq!(
        offers.len(),
        1,
        "provider patch did not select filesystem ownership: {:?}",
        fixture.execution
    );
    assert!(
        fixture
            .service
            .claim_native_shell_action(&fixture.turn.turn_id, &fixture.action.id)
            .unwrap()
            .is_none()
    );
    assert!(
        fixture
            .service
            .running_shell_transactions_for_tests()
            .is_empty()
    );
    assert!(
        pane_input_effects(&fixture.service.drain_pane_io_transition().side_effects).is_empty()
    );
    let producer = offers[0].attempt().root_producer().unwrap();
    let actor = ActorFixture::new(fixture);
    assert!(
        claim_and_start_native_filesystem_producer(actor.handle.clone(), &producer)
            .await
            .unwrap()
    );
    let ack = transfer_joined_native_filesystem_producer(&actor.handle, &producer)
        .await
        .unwrap()
        .unwrap();
    assert!(ack.acknowledges(&producer));
    let fixture = actor.finish().await;
    assert!(
        fixture
            .service
            .agent_provider_task_is_pending(&fixture.turn.turn_id),
        "exact native transfer did not admit the ordinary provider continuation"
    );
    assert_eq!(
        fs::read(fixture.root.join("file")).unwrap(),
        b"selected native data\n"
    );
    let raw = producer.joined_outcome().unwrap();
    assert_eq!(raw.result.certainty, NativeEffectCertainty::Confirmed);
    assert_eq!(raw.receipts.len(), 1);
    let result = &fixture.service.agent_turn_executions()[&fixture.turn.turn_id].action_results[0];
    assert_eq!(result.status, mez_agent::ActionStatus::Succeeded);
    let metadata: serde_json::Value =
        serde_json::from_str(result.structured_content_json.as_ref().unwrap()).unwrap();
    assert_eq!(metadata["execution_target"], "native_runtime");
    assert_eq!(metadata["enforcement"], "runtime_filesystem_capabilities");
    assert_eq!(
        metadata["configured_sandbox_intent"],
        if cfg!(target_os = "linux") {
            "bubblewrap"
        } else {
            "seatbelt"
        }
    );
    assert_eq!(metadata["sent_to_pane"], false);
    for fabricated in ["stdout", "stderr", "exit_code", "signal", "shell_command"] {
        assert!(metadata.get(fabricated).is_none());
    }
}

/// A retained Prompt evaluation cannot manufacture approval. Without an actual
/// decided original request, even a manually pending canonical slot must remain
/// unissued and cannot create its target or a native worker result.
#[test]
fn native_filesystem_selector_prompt_evaluation_is_not_an_approval() {
    let mut fixture = Fixture::new();
    *fixture.service.permission_policy_mut() = fixture
        .service
        .permission_policy()
        .clone()
        .with_approval_policy(mez_agent::permissions::ApprovalPolicy::Ask);
    fixture
        .service
        .permission_policy_mut()
        .set_approval_bypass(false);
    fixture.service.permission_policy_mut().add_rule(
        mez_agent::permissions::CommandRule::new(
            ["apply_patch"],
            RuleDecision::Prompt,
            RuleMatch::Exact,
        )
        .unwrap(),
    );
    let policy = fixture
        .service
        .permission_policy()
        .evaluate_shell_command_structured("apply_patch");
    assert_eq!(policy.decision, RuleDecision::Prompt);
    fixture.execution.action_results[0].permission_evaluation = Some(Box::new(policy));
    fixture
        .service
        .agent_turn_executions_mut()
        .insert(fixture.turn.turn_id.clone(), fixture.execution.clone());
    let attempt = fixture.queue();
    let producer = attempt.root_producer().unwrap();
    let error = fixture
        .service
        .claim_native_filesystem_producer(&producer)
        .unwrap_err();
    assert!(error.message().contains("fresh exact approval"));
    assert!(!producer.is_claimed().unwrap());
    assert!(producer.joined_outcome().is_none());
    assert!(!fixture.root.join("file").exists());
}

/// The actual approval decision resumes one original blocked native patch into
/// process-free ownership. A second use of the same decided UI request cannot
/// dispatch again after the exact producer/canonical/storage handoff.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_selector_human_approval_releases_exact_original_patch() {
    let mut fixture = selected_with_approval(true);
    assert_eq!(
        fixture.execution.action_results[0].status,
        ActionStatus::Blocked
    );
    assert!(
        fixture
            .service
            .pending_native_filesystem_actions()
            .is_empty()
    );
    let primary = fixture
        .service
        .attach_primary("approval-controller", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let approval = fixture
        .service
        .blocked_approvals()
        .requests()
        .find(|request| request.action_kind == "apply_patch")
        .unwrap()
        .clone();
    fixture
        .service
        .integration
        .blocked_approvals_mut()
        .decide_with_client_at(
            &approval.id,
            mez_agent::permissions::ApprovalDecision::Approve,
            None,
            Some(primary.as_str().to_string()),
            crate::runtime::current_unix_seconds(),
        )
        .unwrap();
    let decided = fixture
        .service
        .blocked_approvals()
        .get(&approval.id)
        .unwrap()
        .clone();
    fixture
        .service
        .resume_approved_blocked_agent_action(&approval.id, &decided, &primary)
        .unwrap();
    let offers = fixture.service.pending_native_filesystem_actions();
    assert_eq!(offers.len(), 1);
    let producer = offers[0].attempt().root_producer().unwrap();
    let actor = ActorFixture::new(fixture);
    assert!(
        claim_and_start_native_filesystem_producer(actor.handle.clone(), &producer)
            .await
            .unwrap()
    );
    assert!(
        transfer_joined_native_filesystem_producer(&actor.handle, &producer)
            .await
            .unwrap()
            .unwrap()
            .acknowledges(&producer)
    );
    let mut fixture = actor.finish().await;
    assert_eq!(
        fs::read(fixture.root.join("file")).unwrap(),
        b"selected native data\n"
    );
    assert!(
        fixture
            .service
            .resume_approved_blocked_agent_action(&approval.id, &decided, &primary)
            .unwrap()
            .is_none()
    );
    assert!(
        fixture
            .service
            .pending_native_filesystem_actions()
            .is_empty()
    );
}

/// A mode generation change while the human decides cannot rebase an
/// approval onto a replacement native context, even after native mode returns.
/// Original pending proof remains attributable and no native work is issued.
#[test]
fn native_filesystem_selector_human_approval_rejects_mode_aba() {
    let mut fixture = selected_with_approval(true);
    assert_eq!(
        fixture.execution.action_results[0].status,
        ActionStatus::Blocked
    );
    let primary = fixture
        .service
        .attach_primary("approval-controller", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let approval = fixture
        .service
        .blocked_approvals()
        .requests()
        .find(|request| request.action_kind == "apply_patch")
        .unwrap()
        .clone();
    fixture
        .service
        .set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Pane));
    fixture.service.set_agent_native_shell_mode_for_tests("%1");
    fixture
        .service
        .integration
        .blocked_approvals_mut()
        .decide_with_client_at(
            &approval.id,
            mez_agent::permissions::ApprovalDecision::Approve,
            None,
            Some(primary.as_str().to_string()),
            crate::runtime::current_unix_seconds(),
        )
        .unwrap();
    let decided = fixture
        .service
        .blocked_approvals()
        .get(&approval.id)
        .unwrap()
        .clone();
    assert!(
        fixture
            .service
            .resume_approved_blocked_agent_action(&approval.id, &decided, &primary)
            .is_err()
    );
    assert!(
        fixture
            .service
            .pending_native_filesystem_actions()
            .is_empty()
    );
    assert!(!fixture.root.join("file").exists());
}

/// A one-way native-to-pane change must reject the original native approval
/// before scheduler reacquisition, shell lowering or pane readiness work. The
/// accepted human decision cannot authorize a replacement execution domain.
#[test]
fn native_filesystem_selector_human_approval_rejects_one_way_mode_change() {
    let mut fixture = selected_with_approval(true);
    assert_eq!(
        fixture.execution.action_results[0].status,
        ActionStatus::Blocked
    );
    let primary = fixture
        .service
        .attach_primary("approval-controller", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let approval = fixture
        .service
        .blocked_approvals()
        .requests()
        .find(|request| request.action_kind == "apply_patch")
        .unwrap()
        .clone();
    fixture
        .service
        .set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Pane));
    fixture
        .service
        .integration
        .blocked_approvals_mut()
        .decide_with_client_at(
            &approval.id,
            mez_agent::permissions::ApprovalDecision::Approve,
            None,
            Some(primary.as_str().to_string()),
            crate::runtime::current_unix_seconds(),
        )
        .unwrap();
    let decided = fixture
        .service
        .blocked_approvals()
        .get(&approval.id)
        .unwrap()
        .clone();
    let original_turn = fixture
        .service
        .agent_turn_ledger()
        .turn(&fixture.turn.turn_id)
        .unwrap()
        .clone();
    let error = fixture
        .service
        .resume_approved_blocked_agent_action(&approval.id, &decided, &primary)
        .unwrap_err();
    assert!(
        error
            .message()
            .contains("native filesystem source is not in native mode"),
        "{error}"
    );
    assert_eq!(
        fixture
            .service
            .agent_turn_ledger()
            .turn(&fixture.turn.turn_id),
        Some(&original_turn)
    );
    assert!(
        fixture
            .service
            .running_shell_transactions_for_tests()
            .is_empty()
    );
    assert!(
        pane_input_effects(&fixture.service.drain_pane_io_transition().side_effects).is_empty()
    );
    assert!(
        fixture
            .service
            .pending_native_filesystem_actions()
            .is_empty()
    );
    assert!(!fixture.root.join("file").exists());
}

/// Lost volatile approval evidence cannot erase the original native domain
/// recorded by the actual blocked request. Even after a one-way pane change,
/// missing proof must reject rather than choose the executable pane adapter.
#[test]
fn native_filesystem_selector_lost_approval_proof_cannot_change_domain() {
    let mut fixture = selected_with_approval(true);
    assert_eq!(
        fixture.execution.action_results[0].status,
        ActionStatus::Blocked
    );
    let primary = fixture
        .service
        .attach_primary("approval-controller", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let approval = fixture
        .service
        .blocked_approvals()
        .requests()
        .find(|request| request.action_kind == "apply_patch")
        .unwrap()
        .clone();
    fixture
        .service
        .discard_native_filesystem_approval_proof_for_tests(
            &fixture.turn.turn_id,
            &fixture.execution.action_results[0].action_id,
        );
    fixture
        .service
        .set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Pane));
    fixture
        .service
        .integration
        .blocked_approvals_mut()
        .decide_with_client_at(
            &approval.id,
            mez_agent::permissions::ApprovalDecision::Approve,
            None,
            Some(primary.as_str().to_string()),
            crate::runtime::current_unix_seconds(),
        )
        .unwrap();
    let decided = fixture
        .service
        .blocked_approvals()
        .get(&approval.id)
        .unwrap()
        .clone();
    let original_turn = fixture
        .service
        .agent_turn_ledger()
        .turn(&fixture.turn.turn_id)
        .unwrap()
        .clone();
    let error = fixture
        .service
        .resume_approved_blocked_agent_action(&approval.id, &decided, &primary)
        .unwrap_err();
    assert!(
        error
            .message()
            .contains("native filesystem source is not in native mode"),
        "{error}"
    );
    assert_eq!(
        fixture
            .service
            .agent_turn_ledger()
            .turn(&fixture.turn.turn_id),
        Some(&original_turn)
    );
    assert!(
        fixture
            .service
            .running_shell_transactions_for_tests()
            .is_empty()
    );
    assert!(
        pane_input_effects(&fixture.service.drain_pane_io_transition().side_effects).is_empty()
    );
    assert!(
        fixture
            .service
            .pending_native_filesystem_actions()
            .is_empty()
    );
    assert!(!fixture.root.join("file").exists());
}

/// Original native provenance also survives a corrupted/replaced mutable
/// action projection. Changing the current family to shell_command cannot hide
/// the original source and inherit a human decision made for a semantic patch.
#[test]
fn native_filesystem_selector_human_approval_rejects_changed_action_family() {
    let mut fixture = selected_with_approval(true);
    assert_eq!(
        fixture.execution.action_results[0].status,
        ActionStatus::Blocked
    );
    let primary = fixture
        .service
        .attach_primary("approval-controller", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let approval = fixture
        .service
        .blocked_approvals()
        .requests()
        .find(|request| request.action_kind == "apply_patch")
        .unwrap()
        .clone();
    fixture
        .execution
        .response
        .action_batch
        .as_mut()
        .unwrap()
        .actions[0]
        .payload = mez_agent::AgentActionPayload::ShellCommand {
        summary: "must not inherit patch approval".into(),
        command: "printf approval-retargeted".into(),
        interactive: false,
        stateful: false,
        timeout_ms: None,
    };
    fixture
        .service
        .agent_turn_executions_mut()
        .insert(fixture.turn.turn_id.clone(), fixture.execution.clone());
    fixture
        .service
        .integration
        .blocked_approvals_mut()
        .decide_with_client_at(
            &approval.id,
            mez_agent::permissions::ApprovalDecision::Approve,
            None,
            Some(primary.as_str().to_string()),
            crate::runtime::current_unix_seconds(),
        )
        .unwrap();
    let decided = fixture
        .service
        .blocked_approvals()
        .get(&approval.id)
        .unwrap()
        .clone();
    let original_turn = fixture
        .service
        .agent_turn_ledger()
        .turn(&fixture.turn.turn_id)
        .unwrap()
        .clone();
    let error = fixture
        .service
        .resume_approved_blocked_agent_action(&approval.id, &decided, &primary)
        .unwrap_err();
    assert!(
        error
            .message()
            .contains("native approval requires a semantic patch"),
        "{error}"
    );
    assert_eq!(
        fixture
            .service
            .agent_turn_ledger()
            .turn(&fixture.turn.turn_id),
        Some(&original_turn)
    );
    assert!(
        fixture
            .service
            .running_shell_transactions_for_tests()
            .is_empty()
    );
    assert!(fixture.service.pending_native_shell_actions().is_empty());
    assert!(
        pane_input_effects(&fixture.service.drain_pane_io_transition().side_effects).is_empty()
    );
    assert!(
        fixture
            .service
            .pending_native_filesystem_actions()
            .is_empty()
    );
    assert!(!fixture.root.join("file").exists());
}

/// An actual committed original mutation is recognized from parsed semantics,
/// independent of a later action ID and CRLF spelling. A subsequent external
/// edit survives suppression, proving no redispatch or content-based repair occurs.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_selector_semantic_success_suppresses_only_same_mutation() {
    let fixture = selected();
    let offer = fixture
        .service
        .pending_native_filesystem_actions()
        .remove(0);
    let producer = offer.attempt().root_producer().unwrap();
    let actor = ActorFixture::new(fixture);
    assert!(
        claim_and_start_native_filesystem_producer(actor.handle.clone(), &producer)
            .await
            .unwrap()
    );
    assert!(
        transfer_joined_native_filesystem_producer(&actor.handle, &producer)
            .await
            .unwrap()
            .unwrap()
            .acknowledges(&producer)
    );
    let mut fixture = actor.finish().await;
    fs::write(fixture.root.join("file"), b"subsequent external edit\n").unwrap();
    let mut duplicate = fixture.action.clone();
    duplicate.id = "same-semantics-new-id".into();
    let mez_agent::AgentActionPayload::ApplyPatch { patch, .. } = &mut duplicate.payload else {
        unreachable!()
    };
    *patch = patch.replace('\n', "\r\n");
    let agent = mez_core::ids::AgentId::opaque(fixture.turn.agent_id.clone()).unwrap();
    let dispatch = fixture
        .service
        .claim_configured_agent_provider_task(&agent, &fixture.turn.turn_id)
        .unwrap()
        .expect("native completion must admit the actual provider claim");
    let (_, handoff) = fixture
        .service
        .record_claimed_agent_provider_task(&dispatch, 1, 30_000)
        .unwrap();
    assert!(fixture.service.accept_completion_provider_handoff(&handoff));
    fixture
        .service
        .clear_claimed_agent_provider_task(&fixture.turn.turn_id);
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".into(),
            model: "test".into(),
            raw_text: "same semantic mutation".into(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "check original success custody".into(),
                actions: vec![duplicate],
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    let execution = fixture
        .service
        .execute_agent_turn_with_provider(
            &fixture.turn.turn_id,
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    let metadata: serde_json::Value = serde_json::from_str(
        execution.action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(metadata["guard"], "native_patch_semantic_success");
    assert_eq!(metadata["dispatch_state"], "suppressed_duplicate");
    assert!(
        fixture
            .service
            .pending_native_filesystem_actions()
            .is_empty()
    );
    assert!(
        fixture
            .service
            .running_shell_transactions_for_tests()
            .is_empty()
    );
    assert_eq!(
        fs::read(fixture.root.join("file")).unwrap(),
        b"subsequent external edit\n"
    );
}

/// Accepts five genuine provider turns against the four admitted owner slots.
/// No capacity counter or producer result is fabricated by either test owner.
fn capacity_selected() -> (
    Fixture,
    crate::runtime::processes::native_filesystem_producer::NativeFilesystemProducer,
    AgentTurnRecord,
) {
    let mut fixture = selected();
    let primary = fixture
        .service
        .attach_primary(
            "capacity-controller",
            true,
            Size::new(100, 30).unwrap(),
            120,
        )
        .unwrap();
    fixture.service.configure_agent_scheduler_limit(8).unwrap();
    let first = fixture.service.pending_native_filesystem_actions()[0]
        .attempt()
        .root_producer()
        .unwrap();
    let mut turns = Vec::new();
    for index in 1..5 {
        let pane = fixture
            .service
            .create_window_with_pane_process_with_options(
                &primary,
                format!("capacity-{index}"),
                false,
                Some("cat"),
                Some(&fixture.root),
                None,
            )
            .unwrap();
        fixture
            .service
            .agent_shell_store_mut()
            .enter_or_resume(&pane.pane_id)
            .unwrap();
        fixture
            .service
            .set_agent_native_shell_mode_for_tests(&pane.pane_id);
        let started = fixture
            .service
            .start_agent_prompt_turn(&pane.pane_id, "create bounded native file")
            .unwrap();
        let turn = fixture
            .service
            .agent_turn_ledger()
            .turn(&started.turn_id)
            .unwrap()
            .clone();
        fixture
            .service
            .remove_pending_agent_provider_task(&turn.turn_id);
        let provider = RuntimeBatchProvider {
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".into(),
                model: "test".into(),
                raw_text: format!("capacity response {index}"),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    rationale: "bounded native worker admission".into(),
                    actions: vec![mez_agent::AgentAction {
                        id: "capacity-patch".into(),
                        payload: mez_agent::AgentActionPayload::ApplyPatch {
                            patch: format!(
                                "*** Begin Patch\n*** Add File: {}\n+bounded native data\n*** End Patch",
                                fixture.root.join(format!("file-{index}")).display()
                            ),
                            strip: None,
                        },
                    }],
                }),
                provider_transcript_events: vec![],
            },
        };
        let execution = fixture
            .service
            .execute_agent_turn_with_provider(
                &turn.turn_id,
                &provider,
                runtime_model_profile("runtime-batch", "test"),
            )
            .unwrap();
        assert_eq!(execution.action_results[0].status, ActionStatus::Running);
        turns.push(turn);
    }
    let offers = fixture.service.pending_native_filesystem_actions();
    assert_eq!(
        offers.len(),
        3,
        "the first enrolled root is not offered twice"
    );
    assert_eq!(
        fixture
            .service
            .recoverable_native_filesystem_actions()
            .len(),
        4
    );
    assert!(!fixture.root.join("file-4").exists());
    assert!(
        fixture
            .service
            .running_shell_transactions_for_tests()
            .is_empty()
    );
    (fixture, first, turns.pop().unwrap())
}

/// Five actual provider turns exceed the four-worker admission bound. The fifth
/// retains its original selection and is admitted automatically by another
/// turn's exact transfer, without a shell fallback or a manual redispatch.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_selector_capacity_transfer_wakes_original_waiter() {
    let (fixture, first, _) = capacity_selected();
    let actor = ActorFixture::new(fixture);
    assert!(
        claim_and_start_native_filesystem_producer(actor.handle.clone(), &first)
            .await
            .unwrap()
    );
    assert!(
        transfer_joined_native_filesystem_producer(&actor.handle, &first)
            .await
            .unwrap()
            .unwrap()
            .acknowledges(&first)
    );
    let fixture = actor.finish().await;
    let resumed = fixture.service.pending_native_filesystem_actions();
    assert_eq!(
        resumed.len(),
        4,
        "the released slot must hold the original fifth turn"
    );
    let mut producers = resumed
        .into_iter()
        .map(|offer| offer.attempt().root_producer().unwrap())
        .collect::<Vec<_>>();
    let actor = ActorFixture::new(fixture);
    for producer in producers.drain(..) {
        assert!(
            claim_and_start_native_filesystem_producer(actor.handle.clone(), &producer)
                .await
                .unwrap()
        );
        assert!(
            transfer_joined_native_filesystem_producer(&actor.handle, &producer)
                .await
                .unwrap()
                .unwrap()
                .acknowledges(&producer)
        );
    }
    let fixture = actor.finish().await;
    assert!(
        fixture
            .service
            .pending_native_filesystem_actions()
            .is_empty()
    );
    assert_eq!(
        fs::read(fixture.root.join("file-4")).unwrap(),
        b"bounded native data\n"
    );
}

/// Auto-allow is controller planning evidence for the exact accepted patch,
/// distinct from forging a Prompt evaluation or bypassing an explicit denial.
/// The real actor can claim/commit it without a synthetic human approval record.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_selector_actual_auto_allow_can_commit() {
    let fixture = selected_with_policy(mez_agent::ApprovalPolicy::AutoAllow);
    assert_eq!(
        fixture.execution.action_results[0].status,
        ActionStatus::Running
    );
    assert!(
        fixture
            .service
            .blocked_approvals()
            .requests()
            .next()
            .is_none()
    );
    let metadata: serde_json::Value = serde_json::from_str(
        fixture.execution.action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(metadata["approval"]["state"], "auto_allowed");
    let producer = fixture.service.pending_native_filesystem_actions()[0]
        .attempt()
        .root_producer()
        .unwrap();
    let actor = ActorFixture::new(fixture);
    assert!(
        claim_and_start_native_filesystem_producer(actor.handle.clone(), &producer)
            .await
            .unwrap()
    );
    assert!(
        transfer_joined_native_filesystem_producer(&actor.handle, &producer)
            .await
            .unwrap()
            .unwrap()
            .acknowledges(&producer)
    );
    let fixture = actor.finish().await;
    assert_eq!(
        fs::read(fixture.root.join("file")).unwrap(),
        b"selected native data\n"
    );
}

/// Capacity waiting retains the original native source. Switching its mode
/// while another turn owns all slots cannot cause the later wake to choose a
/// pane shell adapter or silently authorize the replacement context.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_selector_capacity_waiter_rejects_changed_mode() {
    let (mut fixture, first, fifth) = capacity_selected();
    fixture.service.set_agent_shell_mode_override(
        &fifth.pane_id,
        Some(crate::runtime::config::ShellMode::Pane),
    );
    let actor = ActorFixture::new(fixture);
    assert!(
        claim_and_start_native_filesystem_producer(actor.handle.clone(), &first)
            .await
            .unwrap()
    );
    assert!(
        transfer_joined_native_filesystem_producer(&actor.handle, &first)
            .await
            .unwrap()
            .unwrap()
            .acknowledges(&first)
    );
    let fixture = actor.finish().await;
    assert!(
        fixture
            .service
            .agent_provider_task_is_pending(&fifth.turn_id)
    );
    let context = runtime_prepared_context_for_turn(&fixture.service, &fifth.turn_id);
    assert!(
        context.blocks().iter().any(|block| {
            block.source == ContextSourceKind::ActionResult
                && block
                    .content
                    .contains("native filesystem source is not in native mode")
        }),
        "the original unissued rejection must reach the actual recovery context"
    );
    assert!(
        fixture
            .service
            .running_shell_transactions_for_tests()
            .is_empty()
    );
    assert!(!fixture.root.join("file-4").exists());
    let producers = fixture
        .service
        .pending_native_filesystem_actions()
        .into_iter()
        .map(|offer| offer.attempt().root_producer().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(producers.len(), 3);
    let actor = ActorFixture::new(fixture);
    for producer in producers {
        assert!(
            claim_and_start_native_filesystem_producer(actor.handle.clone(), &producer)
                .await
                .unwrap()
        );
        assert!(
            transfer_joined_native_filesystem_producer(&actor.handle, &producer)
                .await
                .unwrap()
                .unwrap()
                .acknowledges(&producer)
        );
    }
    let fixture = actor.finish().await;
    assert!(
        fixture
            .service
            .pending_native_filesystem_actions()
            .is_empty()
    );
}

mod acceptance;
