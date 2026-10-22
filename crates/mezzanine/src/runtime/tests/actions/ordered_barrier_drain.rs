//! Ordered provider-log drain controls over controlled execution snapshots.
//!
//! These are deliberately synthetic presentation controls: they install an
//! execution snapshot and register its provider presentation order through the
//! supported owner APIs, without admitting a provider response or executing any
//! command. They pin the two barrier sites that decide whether an inactive
//! failed shell sibling may hold a later component.
//!
//! Explicit limitations: acknowledgment storage is a set, so these controls
//! establish exact key/body acknowledgment *membership* and its stability, not
//! physical exactly-once rendering. A genuine unpublished-header integration
//! window, native/transaction-owned shell negatives and the pending pre-shell
//! hook state remain unqualified and are not claimed here.

use super::*;

/// Synthetic response terminal states.
#[derive(Clone, Copy)]
enum BarrierState {
    /// Running response: an unowned shell could still start.
    Running,
    /// Settled response: deliberate synthetic predicate input.
    Completed,
    /// Failed response: the unowned shell is provably inactive.
    Failed,
}

type OrderedKey = (
    String,
    String,
    String,
    mez_agent::ContextExecutionGroupId,
    usize,
);

struct QueuedHeader {
    action: mez_agent::AgentAction,
    header: String,
    entry: mez_core::presentation::PresentationEntry,
}

struct QueuedResult {
    action: mez_agent::AgentAction,
    result: mez_agent::ActionResult,
    text: String,
    entry: mez_core::presentation::PresentationEntry,
    transaction: Option<String>,
    raw: bool,
}

struct DrainFixture {
    service: crate::runtime::RuntimeSessionService,
    execution: mez_agent::AgentTurnExecution,
    turn: AgentTurnRecord,
    shell_key: OrderedKey,
    header_key: OrderedKey,
    queued_header: QueuedHeader,
    queued_result: QueuedResult,
}

/// Installs one two-action snapshot (earlier shell, later fetch), registers its
/// provider presentation order through the supported owner and queues the later
/// header and RESULT. Returns the owner-complete exact keys and retained sources.
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
    // The expected owner comes from the registered production order, which must
    // be unique for this pane/turn/session chronology; it is never selected from
    // an arbitrary queued component.
    let conversation = service
        .agent_shell_store()
        .get(&turn.pane_id)
        .map(|session| session.session_id.clone())
        .expect("fixture shell session");
    let mut groups = service
        .presentation
        .agent_provider_log_orders
        .keys()
        .filter(|(pane, candidate, session, _)| {
            pane == &turn.pane_id && candidate == &turn.turn_id && session == &conversation
        })
        .map(|(_, _, _, group)| group.clone())
        .collect::<Vec<_>>();
    groups.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    assert_eq!(
        groups.len(),
        1,
        "the fixture chronology owns exactly one provider-log order"
    );
    let group = groups.remove(0);
    let shell_key: OrderedKey = (
        turn.pane_id.clone(),
        turn.turn_id.clone(),
        conversation.clone(),
        group.clone(),
        0,
    );
    let header_key: OrderedKey = (
        turn.pane_id.clone(),
        turn.turn_id.clone(),
        conversation,
        group,
        1,
    );
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

    // Preconditions: both later sources are independently queued under the exact
    // expected key, with their full retained provenance captured for comparison.
    let queued_header = service
        .presentation
        .agent_queued_provider_headers
        .get(&header_key)
        .map(|(action, header, entry)| QueuedHeader {
            action: action.clone(),
            header: header.clone(),
            entry: entry.clone().expect("queued header occurrence entry"),
        })
        .expect("later header queued exactly under the expected owner");
    let queued_result = service
        .presentation
        .agent_queued_provider_results
        .get(&header_key)
        .map(
            |(action, result, text, entry, transaction, raw)| QueuedResult {
                action: action.clone(),
                result: result.clone(),
                text: text.clone(),
                entry: entry.clone(),
                transaction: transaction.clone(),
                raw: *raw,
            },
        )
        .expect("later RESULT queued exactly under the expected owner");
    assert_eq!(
        queued_header.action.id, fetch.id,
        "the queued header belongs to the later fetch"
    );
    assert_eq!(queued_result.action.id, fetch.id);
    assert_eq!(queued_result.result.action_id, fetch.id);
    assert_eq!(queued_result.text, "barrier body");
    assert!(queued_header.header.contains("fetch url"));
    assert!(!queued_header.entry.id.is_empty());
    assert!(!queued_result.entry.id.is_empty());
    DrainFixture {
        service,
        execution,
        turn,
        shell_key,
        header_key,
        queued_header,
        queued_result,
    }
}

fn header_still_queued(fixture: &DrainFixture) -> bool {
    fixture
        .service
        .presentation
        .agent_queued_provider_headers
        .get(&fixture.header_key)
        .is_some_and(|(action, header, entry)| {
            action == &fixture.queued_header.action
                && header == &fixture.queued_header.header
                && entry.as_ref() == Some(&fixture.queued_header.entry)
        })
}

fn result_still_queued(fixture: &DrainFixture) -> bool {
    fixture
        .service
        .presentation
        .agent_queued_provider_results
        .get(&fixture.header_key)
        .is_some_and(|(action, result, text, entry, transaction, raw)| {
            action == &fixture.queued_result.action
                && result == &fixture.queued_result.result
                && text == &fixture.queued_result.text
                && entry == &fixture.queued_result.entry
                && transaction == &fixture.queued_result.transaction
                && *raw == fixture.queued_result.raw
        })
}

fn header_queue_absent(fixture: &DrainFixture) -> bool {
    !fixture
        .service
        .presentation
        .agent_queued_provider_headers
        .contains_key(&fixture.header_key)
}

fn result_queue_absent(fixture: &DrainFixture) -> bool {
    !fixture
        .service
        .presentation
        .agent_queued_provider_results
        .contains_key(&fixture.header_key)
}

fn header_acknowledged(fixture: &DrainFixture) -> bool {
    fixture
        .service
        .presentation
        .agent_published_provider_headers
        .contains(&fixture.header_key)
}

fn body_ack_membership(fixture: &DrainFixture) -> bool {
    fixture
        .service
        .presentation
        .agent_published_provider_results
        .contains(&(fixture.header_key.clone(), "barrier body".to_string()))
}

fn shell_acknowledged(fixture: &DrainFixture) -> bool {
    let presentation = &fixture.service.presentation;
    presentation
        .agent_published_provider_headers
        .contains(&fixture.shell_key)
        || presentation
            .agent_published_provider_results
            .iter()
            .any(|(key, _)| key == &fixture.shell_key)
}

fn drain(fixture: &mut DrainFixture) {
    fixture
        .service
        .flush_ordered_provider_headers(&fixture.turn.pane_id, &fixture.execution)
        .unwrap();
}

/// A Running response keeps the strict barrier: both exact later sources stay
/// queued with full provenance unchanged and nothing acknowledged, and the
/// earlier headerless shell gains no acknowledgment.
#[test]
fn runtime_ordered_drain_running_unowned_shell_holds_later_sources() {
    let mut fixture = drain_fixture(BarrierState::Running);
    drain(&mut fixture);
    assert!(header_still_queued(&fixture));
    assert!(result_still_queued(&fixture));
    assert!(!header_acknowledged(&fixture));
    assert!(!body_ack_membership(&fixture));
    assert!(!shell_acknowledged(&fixture));
    drain(&mut fixture);
    assert!(header_still_queued(&fixture));
    assert!(result_still_queued(&fixture));
    assert!(!header_acknowledged(&fixture));
    assert!(!body_ack_membership(&fixture));
    assert!(!shell_acknowledged(&fixture));
}

/// A Completed response is deliberate synthetic input for the same strictness:
/// only a Failed response may exempt the inactive shell sibling.
#[test]
fn runtime_ordered_drain_completed_unowned_shell_holds_later_sources() {
    let mut fixture = drain_fixture(BarrierState::Completed);
    drain(&mut fixture);
    assert!(header_still_queued(&fixture));
    assert!(result_still_queued(&fixture));
    assert!(!header_acknowledged(&fixture));
    assert!(!body_ack_membership(&fixture));
    assert!(!shell_acknowledged(&fixture));
    drain(&mut fixture);
    assert!(header_still_queued(&fixture));
    assert!(result_still_queued(&fixture));
    assert!(!header_acknowledged(&fixture));
    assert!(!body_ack_membership(&fixture));
    assert!(!shell_acknowledged(&fixture));
}

/// A Failed response exempts the inactive shell sibling: both barrier sites must
/// release, installing the exact later header and acknowledging the exact later
/// body once, while the earlier shell gains no acknowledgment. A repeated drain
/// keeps acknowledgment membership and source state stable.
#[test]
fn runtime_ordered_drain_failed_unowned_shell_releases_later_sources_once() {
    let mut fixture = drain_fixture(BarrierState::Failed);
    drain(&mut fixture);
    assert!(header_queue_absent(&fixture));
    assert!(result_queue_absent(&fixture));
    assert!(header_acknowledged(&fixture));
    assert!(body_ack_membership(&fixture));
    assert!(!shell_acknowledged(&fixture));
    let acknowledged = fixture
        .service
        .presentation
        .agent_published_provider_results
        .iter()
        .filter(|(key, text)| key == &fixture.header_key && text == "barrier body")
        .count();
    assert_eq!(acknowledged, 1, "single acknowledgment membership");
    drain(&mut fixture);
    assert!(header_queue_absent(&fixture));
    assert!(result_queue_absent(&fixture));
    assert!(header_acknowledged(&fixture));
    assert!(body_ack_membership(&fixture));
    assert!(!shell_acknowledged(&fixture));
}
