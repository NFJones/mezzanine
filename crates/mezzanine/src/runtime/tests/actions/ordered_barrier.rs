//! Ordered provider-log barrier ownership matrix.
//!
//! The barrier may exempt a failed response's shell sibling from holding later
//! log components only when that sibling provably cannot render: no running
//! transaction, no native owner and no pending pre-shell hook. These controls
//! exercise the authentic states reachable through the real provider/claim
//! producers; no private ownership record is forged and no command executes.

use super::*;

/// Authentic shell-sibling ownership states reachable without executing work.
#[derive(Clone, Copy)]
enum ShellOwnership {
    /// Provider response registered, native claim not taken: no owner yet.
    Unowned,
    /// Native dispatch claimed, so the action is owned but never executed.
    ClaimedNative,
    /// An observed running shell transaction owns the action.
    RunningTransaction,
}

struct BarrierFixture {
    service: crate::runtime::RuntimeSessionService,
    turn: AgentTurnRecord,
    shell: mez_agent::AgentAction,
    fetch: mez_agent::AgentAction,
}

/// Admits the response through the real provider event and records the requested
/// ownership state using only supported producers.
async fn barrier_fixture(
    terminal_state: AgentTurnState,
    ownership: ShellOwnership,
) -> BarrierFixture {
    let mut service = test_runtime_service();
    configure_unmanaged_pane_shell_protocol_fixture(&mut service);
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Debug)
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "ordered barrier fixture")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|row| row.turn_id == started.turn_id)
        .unwrap()
        .clone();
    service.remove_pending_agent_provider_task(&turn.turn_id);
    let shell = mez_agent::AgentAction {
        id: "barrier-shell".to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "Inspect barrier evidence".to_string(),
            command: "printf barrier".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    let fetch = mez_agent::AgentAction {
        id: "barrier-fetch".to_string(),
        payload: mez_agent::AgentActionPayload::FetchUrl {
            url: "https://example.test/barrier".to_string(),
            format: None,
            max_bytes: None,
        },
    };
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "inspect ordered barrier".to_string(),
                actions: vec![shell.clone(), fetch.clone()],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: [&shell, &fetch]
            .into_iter()
            .map(|action| {
                mez_agent::ActionResult::running(&turn, action, vec!["queued".to_string()], None)
            })
            .collect(),
        final_turn: true,
        terminal_state,
    };
    service
        .apply_agent_provider_completed_event(
            &AgentId::opaque(turn.agent_id.clone()).unwrap(),
            &turn.turn_id,
            execution,
        )
        .await
        .unwrap();
    match ownership {
        ShellOwnership::Unowned => {}
        ShellOwnership::ClaimedNative => {
            service
                .claim_native_shell_action(&turn.turn_id, &shell.id)
                .unwrap()
                .expect("native dispatch must be claimable without execution");
        }
        ShellOwnership::RunningTransaction => {
            let marker = service
                .running_shell_transactions_for_tests()
                .iter()
                .find_map(|(marker, transaction)| {
                    matches!(
                        &transaction.kind,
                        RunningShellTransactionKind::AgentAction { action_id }
                            if action_id == &shell.id
                    )
                    .then(|| marker.clone())
                })
                .expect("shell sibling registered a transaction");
            service
                .observe_agent_shell_transaction_start(
                    "%1",
                    &marker,
                    &turn.turn_id,
                    &turn.agent_id,
                    "%1",
                )
                .unwrap();
        }
    }
    BarrierFixture {
        service,
        turn,
        shell,
        fetch,
    }
}

/// Completes the later fetch sibling without executing any network effect.
fn complete_fetch(fixture: &mut BarrierFixture) {
    let dispatch = fixture
        .service
        .claim_approved_external_action(&fixture.turn.turn_id, &fixture.fetch.id)
        .unwrap()
        .expect("fetch sibling retains ownership");
    assert!(
        fixture
            .service
            .complete_approved_external_action(
                crate::runtime::RuntimeApprovedExternalActionOutcome {
                    turn_id: fixture.turn.turn_id.clone(),
                    action_id: fixture.fetch.id.clone(),
                    attempt: dispatch.attempt,
                    result: Ok(mez_agent::ActionResult::succeeded(
                        &fixture.turn,
                        &fixture.fetch,
                        vec!["barrier body".to_string()],
                        None,
                    )),
                    mcp_transport: None,
                }
            )
            .unwrap()
    );
}

/// Reports whether the later fetch still owns an unpublished ordered header.
fn fetch_header_queued(fixture: &BarrierFixture) -> bool {
    fixture
        .service
        .presentation
        .agent_queued_provider_headers
        .keys()
        .any(|key| key.1 == fixture.turn.turn_id && key.4 == 1)
}

/// Reports whether the exact later body was acknowledged exactly once.
fn fetch_body_published(fixture: &BarrierFixture) -> bool {
    fixture
        .service
        .presentation
        .agent_published_provider_results
        .iter()
        .any(|(key, text)| key.1 == fixture.turn.turn_id && text == "barrier body")
}

/// A still-running response keeps the strict barrier: an unowned shell sibling
/// can still start and render later, so the following fetch stays queued. Once
/// the response is failed the same sibling is provably inactive and the barrier
/// releases the later component. No shell or fetch effect is ever executed.
#[tokio::test]
async fn runtime_ordered_barrier_running_unowned_shell_holds_until_failed() {
    let mut fixture = barrier_fixture(AgentTurnState::Running, ShellOwnership::Unowned).await;
    complete_fetch(&mut fixture);
    assert!(
        fetch_header_queued(&fixture),
        "a running unowned shell sibling must hold later ordered components"
    );
    assert!(!fetch_body_published(&fixture));
    assert!(
        !fixture
            .service
            .agent_provider_task_is_pending(&fixture.turn.turn_id)
    );

    let mut execution = fixture
        .service
        .agent_turn_executions()
        .get(&fixture.turn.turn_id)
        .cloned()
        .expect("canonical execution");
    execution.terminal_state = AgentTurnState::Failed;
    fixture
        .service
        .agent_turn_executions_mut()
        .insert(fixture.turn.turn_id.clone(), execution.clone());
    fixture
        .service
        .flush_ordered_provider_headers(&fixture.turn.pane_id, &execution)
        .unwrap();
    assert!(
        !fetch_header_queued(&fixture),
        "the failed response releases the later ordered header"
    );
    assert!(fetch_body_published(&fixture));
}

/// A failed response with no shell ownership releases its later component on the
/// first drain and acknowledges the exact body exactly once.
#[tokio::test]
async fn runtime_ordered_barrier_failed_unowned_shell_drains_once() {
    let mut fixture = barrier_fixture(AgentTurnState::Failed, ShellOwnership::Unowned).await;
    complete_fetch(&mut fixture);
    assert!(!fetch_header_queued(&fixture));
    assert!(
        fixture
            .service
            .agent_provider_task_is_pending(&fixture.turn.turn_id)
    );
    assert_eq!(
        fixture
            .service
            .presentation
            .agent_published_provider_results
            .iter()
            .filter(|(key, text)| key.1 == fixture.turn.turn_id && text == "barrier body")
            .count(),
        1,
        "the exact body is acknowledged exactly once"
    );
}

/// A failed response whose shell sibling is genuinely owned by a native claim
/// keeps the strict barrier: the owner could still produce output.
#[tokio::test]
async fn runtime_ordered_barrier_failed_claimed_native_shell_holds() {
    let mut fixture = barrier_fixture(AgentTurnState::Failed, ShellOwnership::ClaimedNative).await;
    assert!(
        fixture
            .service
            .agent_action_has_native_shell_owner(&fixture.turn.turn_id, &fixture.shell.id),
        "the native claim must own the shell sibling"
    );
    complete_fetch(&mut fixture);
    assert!(
        fetch_header_queued(&fixture),
        "a claimed native shell owner must not be exempted from the barrier"
    );
    assert!(!fetch_body_published(&fixture));
}

/// A failed response whose shell sibling has a running transaction keeps the
/// strict barrier; ending the transaction releases the later component once.
#[tokio::test]
async fn runtime_ordered_barrier_failed_running_transaction_shell_holds_then_releases() {
    let mut fixture =
        barrier_fixture(AgentTurnState::Failed, ShellOwnership::RunningTransaction).await;
    complete_fetch(&mut fixture);
    assert!(
        fetch_header_queued(&fixture),
        "an executing shell sibling must hold later ordered components"
    );
    assert!(!fetch_body_published(&fixture));

    let marker = fixture
        .service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| {
            matches!(
                &transaction.kind,
                RunningShellTransactionKind::AgentAction { action_id }
                    if action_id == &fixture.shell.id
            )
            .then(|| marker.clone())
        })
        .expect("running transaction marker");
    fixture
        .service
        .observe_agent_shell_transaction_end(
            &fixture.turn.pane_id,
            &marker,
            &fixture.turn.turn_id,
            &fixture.turn.agent_id,
            &fixture.turn.pane_id,
            0,
        )
        .unwrap();
    let execution = fixture
        .service
        .agent_turn_executions()
        .get(&fixture.turn.turn_id)
        .cloned()
        .expect("canonical execution");
    fixture
        .service
        .flush_ordered_provider_headers(&fixture.turn.pane_id, &execution)
        .unwrap();
    assert!(!fetch_header_queued(&fixture));
    assert!(fetch_body_published(&fixture));
}
