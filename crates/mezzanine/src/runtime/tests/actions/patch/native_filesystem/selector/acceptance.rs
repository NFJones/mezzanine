//! Assembled policy, semantic-history and joined dependency acceptance controls.
//!
//! Provider batches select genuine native attempts. Actual actor/descriptor
//! producers and exact transfer acknowledgements supply all completion facts;
//! no fabricated worker outcome or completed slot can satisfy these controls.

use super::*;
use crate::runtime::processes::native_patch::worker::NativePatchWorkerOutcome;

/// Drives the sole actual selected producer through its original completion gates.
/// Returns its immutable observed evidence after the exact transfer is acknowledged.
async fn settle(fixture: Fixture) -> (Fixture, Arc<NativePatchWorkerOutcome>) {
    let offers = fixture.service.pending_native_filesystem_actions();
    assert_eq!(
        offers.len(),
        1,
        "expected one actual provider-selected worker"
    );
    let producer = offers[0].attempt().root_producer().unwrap();
    let actor = ActorFixture::new(fixture);
    assert!(
        claim_and_start_native_filesystem_producer(actor.handle.clone(), &producer)
            .await
            .unwrap()
    );
    let transferred = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Some(ack) = transfer_joined_native_filesystem_producer(&actor.handle, &producer)
                .await
                .unwrap()
            {
                return ack;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await;
    let ack = match transferred {
        Ok(ack) => ack,
        Err(_) => {
            let fixture = actor.finish().await;
            let parent = fixture
                .service
                .agent_turn_ledger()
                .turn(&fixture.turn.turn_id)
                .map(|turn| turn.state);
            let dependencies = fixture.service.joined_subagent_dependency_count();
            panic!(
                "actual transfer stayed pending: phase={:?}; parent={parent:?} dependencies={dependencies}; execution={:?}",
                fixture
                    .service
                    .filesystem_completion_diagnostics_for_tests(producer.attempt()),
                fixture
                    .service
                    .agent_turn_executions()
                    .get(&fixture.turn.turn_id)
                    .map(|execution| execution
                        .action_results
                        .iter()
                        .map(|result| (&result.action_id, result.status))
                        .collect::<Vec<_>>())
            );
        }
    };
    assert!(ack.acknowledges(&producer));
    let raw = producer.joined_outcome().unwrap();
    (actor.finish().await, raw)
}

/// Accepts another real provider batch after consuming the original continuation
/// handoff. This cannot bypass failed storage, unfinished producer or dependency gates.
fn next_provider(
    fixture: &mut Fixture,
    actions: Vec<mez_agent::AgentAction>,
) -> RuntimeBatchProvider {
    let agent = mez_core::ids::AgentId::opaque(fixture.turn.agent_id.clone()).unwrap();
    let dispatch = fixture
        .service
        .claim_configured_agent_provider_task(&agent, &fixture.turn.turn_id)
        .unwrap()
        .expect("exact prior settlement must admit a provider claim");
    let (_, handoff) = fixture
        .service
        .record_claimed_agent_provider_task(&dispatch, 1, 30_000)
        .unwrap();
    assert!(fixture.service.accept_completion_provider_handoff(&handoff));
    fixture
        .service
        .clear_claimed_agent_provider_task(&fixture.turn.turn_id);
    RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".into(),
            model: "test".into(),
            raw_text: "native acceptance continuation".into(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "qualify original native mutation and dependency custody".into(),
                actions,
            }),
            provider_transcript_events: Vec::new(),
        },
    }
}

/// Runs the genuine synchronous provider application after its original handoff.
fn next_response(
    fixture: &mut Fixture,
    actions: Vec<mez_agent::AgentAction>,
) -> mez_agent::AgentTurnExecution {
    let provider = next_provider(fixture, actions);
    fixture
        .service
        .execute_agent_turn_with_provider(
            &fixture.turn.turn_id,
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap()
}

/// Runs the production asynchronous provider application, including accepted
/// spawn/fork source custody, after the same original provider claim boundary.
async fn next_response_asynchronously(
    fixture: &mut Fixture,
    actions: Vec<mez_agent::AgentAction>,
) -> mez_agent::AgentTurnExecution {
    let provider = next_provider(fixture, actions);
    fixture
        .service
        .execute_agent_turn_with_provider_async(
            &fixture.turn.turn_id,
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .await
        .unwrap()
}

/// Confirms neither selection nor completed filesystem work queues executable
/// shell transactions or sends commands to the existing pane's PTY.
fn assert_no_shell(fixture: &mut Fixture) {
    assert!(fixture.service.pending_native_shell_actions().is_empty());
    assert!(
        fixture
            .service
            .running_shell_transactions_for_tests()
            .is_empty()
    );
    assert!(
        pane_input_effects(&fixture.service.drain_pane_io_transition().side_effects).is_empty()
    );
}

/// Native execution stays descriptor based under explicit host access and each
/// configured sandbox intent. A nonexistent Bubblewrap executable must never be
/// resolved or probed; host access changes policy without claiming OS confinement.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_acceptance_policy_intents_and_host_access_do_not_launch_shells() {
    for policy in [
        mez_agent::ApprovalPolicy::FullAccess,
        mez_agent::ApprovalPolicy::HostAccess,
    ] {
        for intent in ["policy-only", "bubblewrap", "seatbelt"] {
            let mut fixture = selected_with_setup(policy, |service, root| {
                let permissions = crate::runtime::config::runtime_configured_permissions_from_config(
                    &serde_json::json!({"permissions":{
                        "sandbox":intent,"read_scopes":[root],"write_scopes":[root],
                        "bubblewrap":{"executable":"/nonexistent/mez-native-must-not-probe","unavailable":"fail"}
                    }}),
                ).unwrap();
                service
                    .integration
                    .replace_configured_permissions(permissions);
                service.set_pane_approval_policy_override("%1", Some(policy));
            });
            assert_eq!(
                fixture
                    .service
                    .permission_policy_for_pane("%1")
                    .approval_policy,
                policy
            );
            assert_no_shell(&mut fixture);
            let (mut fixture, raw) = settle(fixture).await;
            assert_eq!(
                raw.result.certainty,
                NativeEffectCertainty::Confirmed,
                "policy={policy:?} intent={intent} diagnostic={:?}",
                raw.result.diagnostic
            );
            assert_no_shell(&mut fixture);
            assert_eq!(
                fs::read(fixture.root.join("file")).unwrap(),
                b"selected native data\n"
            );
            let result =
                &fixture.service.agent_turn_executions()[&fixture.turn.turn_id].action_results[0];
            let metadata: serde_json::Value =
                serde_json::from_str(result.structured_content_json.as_deref().unwrap()).unwrap();
            assert_eq!(metadata["configured_sandbox_intent"], intent);
            assert_eq!(metadata["execution_target"], "native_runtime");
            assert_eq!(metadata["enforcement"], "runtime_filesystem_capabilities");
            assert_eq!(metadata["sent_to_pane"], false);
            for fabricated in ["stdout", "stderr", "exit_code", "signal", "shell_command"] {
                assert!(metadata.get(fabricated).is_none());
            }
        }
    }
}

/// A different parsed mutation in the same source must receive a real new worker,
/// despite targeting the same path as a successful predecessor. Only the new
/// descriptor commit may change its bytes; the old success is not a blanket guard.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_acceptance_distinct_mutation_is_not_suppressed() {
    let (mut fixture, first) = settle(selected()).await;
    assert_eq!(first.result.certainty, NativeEffectCertainty::Confirmed);
    let action = mez_agent::AgentAction {
        id: "distinct-update".into(),
        payload: mez_agent::AgentActionPayload::ApplyPatch {
            patch: format!(
                "*** Begin Patch\n*** Update File: {}\n@@\n-selected native data\n+distinct update\n*** End Patch",
                fixture.root.join("file").display()
            ),
            strip: None,
        },
    };
    let execution = next_response(&mut fixture, vec![action]);
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    let (mut fixture, raw) = settle(fixture).await;
    assert_eq!(raw.result.certainty, NativeEffectCertainty::Confirmed);
    assert_eq!(raw.receipts.len(), 1);
    assert_eq!(
        fs::read(fixture.root.join("file")).unwrap(),
        b"distinct update\n"
    );
    assert_no_shell(&mut fixture);
}

/// Success in a retired Native/Pane/Native source incarnation cannot suppress a
/// newly accepted mutation. Removing the old file after settlement makes a real
/// second no-replace creation observable without fabricating any outcome.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_acceptance_changed_source_is_not_suppressed() {
    let (mut fixture, _) = settle(selected()).await;
    fs::remove_file(fixture.root.join("file")).unwrap();
    fixture
        .service
        .set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Pane));
    fixture.service.set_agent_native_shell_mode_for_tests("%1");
    let mut repeated = fixture.action.clone();
    repeated.id = "new-source-same-mutation".into();
    let execution = next_response(&mut fixture, vec![repeated]);
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    let (mut fixture, raw) = settle(fixture).await;
    assert_eq!(raw.result.certainty, NativeEffectCertainty::Confirmed);
    assert_eq!(
        fs::read(fixture.root.join("file")).unwrap(),
        b"selected native data\n"
    );
    assert_no_shell(&mut fixture);
}

/// A genuinely failed create against a preexisting file cannot enter successful
/// semantic history. A later explicit provider request after external removal
/// must dispatch once; ordinary completion must never automatically retry it.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_acceptance_failed_attempt_is_not_success_history() {
    let fixture = selected_with_setup(mez_agent::ApprovalPolicy::FullAccess, |_, root| {
        fs::write(root.join("file"), b"competing original\n").unwrap();
    });
    let (mut fixture, raw) = settle(fixture).await;
    assert_eq!(raw.result.certainty, NativeEffectCertainty::Unexecuted);
    assert!(raw.receipts.is_empty());
    assert!(
        fixture
            .service
            .pending_native_filesystem_actions()
            .is_empty()
    );
    assert_eq!(
        fs::read(fixture.root.join("file")).unwrap(),
        b"competing original\n"
    );
    fs::remove_file(fixture.root.join("file")).unwrap();
    let mut retry = fixture.action.clone();
    retry.id = "explicit-new-request-after-failure".into();
    let execution = next_response(&mut fixture, vec![retry]);
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    let (mut fixture, raw) = settle(fixture).await;
    assert_eq!(raw.result.certainty, NativeEffectCertainty::Confirmed);
    assert_eq!(
        fs::read(fixture.root.join("file")).unwrap(),
        b"selected native data\n"
    );
    assert_no_shell(&mut fixture);
}

/// Genuine task-lifetime spawn and filesystem siblings must both settle before
/// the original parent can continue. Both synchronous and asynchronous provider
/// application exercise both completion orders, with one scheduler slot when
/// the file finishes first and two when the child finishes first. Exact file
/// transfer cannot complete the child; its result cannot bypass a file producer.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_acceptance_joined_parent_waits_for_both_real_siblings() {
    for (child_first, asynchronous) in [(false, false), (true, false), (false, true), (true, true)]
    {
        let (mut fixture, _) = settle(selected()).await;
        fixture
            .service
            .attach_primary(
                "filesystem-joined-parent",
                true,
                Size::new(100, 30).unwrap(),
                120,
            )
            .unwrap();
        fixture
            .service
            .configure_agent_scheduler_limit(if child_first { 2 } else { 1 })
            .unwrap();
        mark_test_pane_ready(&mut fixture.service, "%1");
        let patch = mez_agent::AgentAction {
            id: "joined-filesystem-sibling".into(),
            payload: mez_agent::AgentActionPayload::ApplyPatch {
                patch: format!(
                    "*** Begin Patch\n*** Add File: {}\n+joined actual file\n*** End Patch",
                    fixture.root.join("joined/parents/file").display()
                ),
                strip: None,
            },
        };
        let actions = vec![
            runtime_spawn_agent_action("joined-child", "return an actual task result"),
            patch,
        ];
        let execution = if asynchronous {
            next_response_asynchronously(&mut fixture, actions).await
        } else {
            next_response(&mut fixture, actions)
        };
        assert!(
            execution
                .action_results
                .iter()
                .all(|result| result.status == ActionStatus::Running),
            "{execution:?}"
        );
        let child = fixture
            .service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| {
                fixture
                    .service
                    .joined_subagent_dependency(&turn.turn_id)
                    .is_some_and(|dependency| {
                        dependency.parent_turn_id == fixture.turn.turn_id
                            && dependency.parent_action_id == "joined-child"
                    })
            })
            .unwrap_or_else(|| {
                panic!(
                    "missing actual joined child: {execution:?}; turns={:?}",
                    fixture.service.agent_turn_ledger().turns()
                )
            })
            .clone();
        assert!(
            fixture
                .service
                .validate_native_agent_surface_startup(&child.pane_id)
                .unwrap()
        );
        fixture.service.set_pane_approval_policy_override(
            &child.pane_id,
            Some(mez_agent::ApprovalPolicy::AutoAllow),
        );
        fixture.service.start_ready_agent_turns().unwrap();
        assert_eq!(
            fixture
                .service
                .agent_turn_ledger()
                .turn(&fixture.turn.turn_id)
                .unwrap()
                .state,
            AgentTurnState::Running
        );
        assert_parent_held(&mut fixture);
        if child_first {
            finish_child(&mut fixture, &child);
            assert_parent_held(&mut fixture);
            assert!(!fixture.root.join("joined/parents/file").exists());
        }
        let (mut fixture, raw) = settle(fixture).await;
        assert_eq!(raw.result.certainty, NativeEffectCertainty::Confirmed);
        assert_eq!(raw.receipts.len(), 3);
        assert_eq!(
            fs::read(fixture.root.join("joined/parents/file")).unwrap(),
            b"joined actual file\n"
        );
        if !child_first {
            assert_eq!(
                fixture
                    .service
                    .agent_turn_ledger()
                    .turn(&fixture.turn.turn_id)
                    .unwrap()
                    .state,
                AgentTurnState::Blocked
            );
            assert_parent_held(&mut fixture);
            assert_eq!(fixture.service.joined_subagent_dependency_count(), 1);
            fixture.service.start_ready_agent_turns().unwrap();
            finish_child(&mut fixture, &child);
        }
        assert_eq!(fixture.service.joined_subagent_dependency_count(), 0);
        assert!(
            fixture
                .service
                .agent_provider_task_is_pending(&fixture.turn.turn_id)
        );
        assert!(
            fixture
                .service
                .pending_native_filesystem_actions()
                .is_empty()
        );
        let context = &fixture.service.agent_turn_contexts()[&fixture.turn.turn_id];
        for id in ["joined-filesystem-sibling", "joined-child"] {
            assert_eq!(
                context
                    .chronology()
                    .iter()
                    .filter(|event| event.block().source
                        == mez_agent::ContextSourceKind::ActionResult
                        && event.block().label == format!("action result {id}"))
                    .count(),
                1
            );
        }
        // Final completion retires the live context. Inspect its once-only
        // chronology before that boundary, then verify both facts reached the
        // actual final inference request rather than requiring retired storage.
        let final_action = runtime_say_response_for_agent(
            &fixture.turn.turn_id,
            &fixture.turn.agent_id,
            "used both real sibling results",
            true,
        )
        .action_batch
        .unwrap()
        .actions;
        let final_execution = if asynchronous {
            next_response_asynchronously(&mut fixture, final_action).await
        } else {
            next_response(&mut fixture, final_action)
        };
        assert!(
            final_execution
                .request
                .messages
                .iter()
                .any(|message| message.content.contains("directory_created"))
        );
        assert!(final_execution.request.messages.iter().any(|message| {
            message
                .content
                .contains("actual joined filesystem sibling result")
        }));
        assert_eq!(final_execution.terminal_state, AgentTurnState::Completed);
        assert!(
            !fixture
                .service
                .agent_provider_task_is_pending(&fixture.turn.turn_id)
        );
        assert!(
            fixture
                .service
                .pending_native_filesystem_actions()
                .is_empty()
        );
        assert_no_shell(&mut fixture);
    }
}

/// Completes the actual child through normal provider dispatch, preserving the
/// original joined result gate instead of manufacturing scheduler completion.
fn finish_child(fixture: &mut Fixture, child: &AgentTurnRecord) {
    let provider = RuntimeBatchProvider {
        response: runtime_say_response_for_agent(
            &child.turn_id,
            &child.agent_id,
            "actual joined filesystem sibling result",
            true,
        ),
    };
    let execution = fixture
        .service
        .execute_agent_turn_with_provider(
            &child.turn_id,
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
}

/// Queries the production pre-inference gate rather than equating a queued timer
/// with an executable continuation. This also consumes stale pending offers in
/// the same way the normal host poll does while a real sibling remains active.
fn assert_parent_held(fixture: &mut Fixture) {
    let agent = mez_core::ids::AgentId::opaque(fixture.turn.agent_id.clone()).unwrap();
    assert!(
        fixture
            .service
            .ready_agent_provider_turn(&agent, &fixture.turn.turn_id)
            .unwrap()
            .is_none()
    );
    assert!(
        fixture
            .service
            .claim_configured_agent_provider_task(&agent, &fixture.turn.turn_id)
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        fixture
            .service
            .agent_turn_ledger()
            .turn(&fixture.turn.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running | AgentTurnState::Blocked
    ));
}
