//! Ordered provider-log drain controls over controlled execution snapshots.
//!
//! These are deliberately synthetic presentation controls: they install an
//! execution snapshot and register its provider presentation order through the
//! supported owner APIs, without admitting a provider response or executing any
//! command. They exist to pin the two barrier sites that decide whether an
//! inactive failed shell sibling may hold a later component. A genuine
//! unpublished-header integration window and the pending pre-shell-hook state
//! remain explicitly unqualified and are not claimed here.

use super::*;

/// Synthetic response terminal states that must keep the strict barrier.
#[derive(Clone, Copy)]
enum BarrierState {
    /// Running response: an unowned shell could still start.
    Running,
    /// Settled response: the same shape is deliberate synthetic input.
    Completed,
    /// Failed response: the unowned shell is provably inactive.
    Failed,
}

struct DrainFixture {
    service: crate::runtime::RuntimeSessionService,
    execution: mez_agent::AgentTurnExecution,
    turn: AgentTurnRecord,
}

/// Installs one two-action snapshot (earlier shell, later fetch) and registers
/// its provider presentation order without admitting or dispatching work.
fn drain_fixture(state: BarrierState) -> DrainFixture {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Debug)
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "ordered drain fixture")
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
        id: "drain-shell".to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "Inspect drain evidence".to_string(),
            command: "printf drain".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    let fetch = mez_agent::AgentAction {
        id: "drain-fetch".to_string(),
        payload: mez_agent::AgentActionPayload::FetchUrl {
            url: "https://example.test/drain".to_string(),
            format: None,
            max_bytes: None,
        },
    };
    let terminal_state = match state {
        BarrierState::Running => AgentTurnState::Running,
        BarrierState::Completed => AgentTurnState::Completed,
        BarrierState::Failed => AgentTurnState::Failed,
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
                rationale: "inspect ordered drain".to_string(),
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
        .agent_turn_executions_mut()
        .insert(turn.turn_id.clone(), execution.clone());
    service
        .retain_provider_presentation_order("%1", &execution)
        .unwrap();
    assert!(
        !service
            .queue_ordered_provider_header("%1", &execution, &shell)
            .unwrap(),
        "the earlier shell result is deliberately headerless"
    );
    assert!(
        service
            .queue_ordered_provider_header("%1", &execution, &fetch)
            .unwrap(),
        "the later fetch owns a real display header"
    );
    let body =
        mez_agent::ActionResult::succeeded(&turn, &fetch, vec!["barrier body".to_string()], None);
    service
        .queue_ordered_provider_result("%1", &execution, &fetch, &body)
        .unwrap();
    DrainFixture {
        service,
        execution,
        turn,
    }
}

/// Reports queued later components for the exact turn and ordinal.
fn later_component_queued(fixture: &DrainFixture) -> bool {
    let presentation = &fixture.service.presentation;
    presentation
        .agent_queued_provider_headers
        .keys()
        .any(|key| key.1 == fixture.turn.turn_id && key.4 == 1)
        || presentation
            .agent_queued_provider_results
            .keys()
            .any(|key| key.1 == fixture.turn.turn_id && key.4 == 1)
}

/// Reports the exact later header acknowledgment.
fn later_header_published(fixture: &DrainFixture) -> bool {
    fixture
        .service
        .presentation
        .agent_published_provider_headers
        .iter()
        .any(|key| key.1 == fixture.turn.turn_id && key.4 == 1)
}

/// Counts the exact later RESULT acknowledgment by body text.
fn later_body_acknowledgments(fixture: &DrainFixture) -> usize {
    fixture
        .service
        .presentation
        .agent_published_provider_results
        .iter()
        .filter(|(key, text)| key.1 == fixture.turn.turn_id && key.4 == 1 && text == "barrier body")
        .count()
}

/// Reports whether the earlier headerless shell gained any acknowledgment.
fn earlier_shell_acknowledged(fixture: &DrainFixture) -> bool {
    let presentation = &fixture.service.presentation;
    presentation
        .agent_published_provider_headers
        .iter()
        .any(|key| key.1 == fixture.turn.turn_id && key.4 == 0)
        || presentation
            .agent_published_provider_results
            .iter()
            .any(|(key, _)| key.1 == fixture.turn.turn_id && key.4 == 0)
}

fn drain(fixture: &mut DrainFixture) {
    fixture
        .service
        .flush_ordered_provider_headers(&fixture.turn.pane_id, &fixture.execution)
        .unwrap();
}

/// A Running response keeps the strict barrier: the headerless earlier shell
/// could still start and render later, so both later sources stay queued.
#[test]
fn runtime_ordered_drain_running_unowned_shell_holds_later_sources() {
    let mut fixture = drain_fixture(BarrierState::Running);
    drain(&mut fixture);
    assert!(later_component_queued(&fixture));
    assert!(!later_header_published(&fixture));
    assert_eq!(later_body_acknowledgments(&fixture), 0);
    assert!(!earlier_shell_acknowledged(&fixture));
}

/// A Completed response is deliberate synthetic input for the same strictness:
/// only a Failed response may exempt the inactive shell sibling.
#[test]
fn runtime_ordered_drain_completed_unowned_shell_holds_later_sources() {
    let mut fixture = drain_fixture(BarrierState::Completed);
    drain(&mut fixture);
    assert!(later_component_queued(&fixture));
    assert_eq!(later_body_acknowledgments(&fixture), 0);
}

/// A Failed response exempts the inactive shell sibling: both barrier sites must
/// release, installing the exact later header and acknowledging the exact body
/// once while the earlier shell gains no acknowledgment.
#[test]
fn runtime_ordered_drain_failed_unowned_shell_releases_later_sources_once() {
    let mut fixture = drain_fixture(BarrierState::Failed);
    drain(&mut fixture);
    assert!(!later_component_queued(&fixture));
    assert!(later_header_published(&fixture));
    assert_eq!(later_body_acknowledgments(&fixture), 1);
    assert!(!earlier_shell_acknowledged(&fixture));

    // A repeated drain must not acknowledge or re-queue anything again.
    drain(&mut fixture);
    assert!(!later_component_queued(&fixture));
    assert_eq!(later_body_acknowledgments(&fixture), 1);
    assert!(!earlier_shell_acknowledged(&fixture));
}
