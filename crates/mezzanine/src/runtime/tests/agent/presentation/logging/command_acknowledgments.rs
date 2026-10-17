//! Per-entry acknowledgment of render-only queued command publications.
//!
//! The existing accepted response owner retains exact entry identities and
//! occurrence metadata. These tests inject only the common pre-install fault;
//! no shell, provider, or action executor is run, and no post-install ambiguity
//! or broader lifecycle-drain repair is claimed.

use super::*;
use crate::storage::transcript::activity::{
    ACTIVITY_CONTENT_TYPE, ActivityComponentKind, ActivitySource,
};
use mez_core::presentation::PresentationEntry;

/// Keeps the accepted source and original observations beside their log owner.
struct CommandFixture {
    service: RuntimeSessionService,
    execution: mez_agent::AgentTurnExecution,
    store: AgentTranscriptStore,
    entries: Vec<PresentationEntry>,
    commands: Vec<String>,
    ordinal: usize,
    response_id: String,
}

/// Builds a local publication owner. The direct-vector variant isolates the
/// drain fault; the transaction variant uses the production queue API behind an
/// earlier action's header barrier, allowing multiple commands to accumulate.
fn command_fixture(label: &str, commands: &[&str], transaction_queue: bool) -> CommandFixture {
    let mut service = test_runtime_service();
    let store = AgentTranscriptStore::new(temp_root(&format!("command-ack-{label}")));
    service.set_agent_transcript_store(store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "retain command publications")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .unwrap()
        .clone();
    service.remove_pending_agent_provider_task(&turn.turn_id);
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(120, 24).unwrap(), 120).unwrap(),
    );
    let action = mez_agent::AgentAction {
        id: "command-owner".into(),
        payload: if transaction_queue {
            mez_agent::AgentActionPayload::ShellCommand {
                summary: "ACK_SUMMARY".into(),
                command: commands[0].into(),
                interactive: false,
                stateful: false,
                timeout_ms: None,
            }
        } else {
            mez_agent::AgentActionPayload::ListAgents {
                agent_type: None,
                scope: None,
            }
        },
    };
    let result =
        mez_agent::ActionResult::succeeded(&turn, &action, vec!["ACK_RESULT".into()], None);
    let say = mez_agent::AgentAction {
        id: "later-say".into(),
        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Progress,
            text: "ACK_SAY".into(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.into(),
        },
    };
    let mut actions = vec![action.clone(), say.clone()];
    let mut results = vec![
        result.clone(),
        mez_agent::ActionResult::succeeded(&turn, &say, Vec::new(), None),
    ];
    if transaction_queue {
        let predecessor = mez_agent::AgentAction {
            id: "earlier-owner".into(),
            payload: mez_agent::AgentActionPayload::ListAgents {
                agent_type: None,
                scope: None,
            },
        };
        results.insert(
            0,
            mez_agent::ActionResult::running(&turn, &predecessor, Vec::new(), None),
        );
        actions.insert(0, predecessor);
    }
    let ordinal = usize::from(transaction_queue);
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".into(),
            model: "test".into(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: String::new(),
                actions,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: results,
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    service
        .present_agent_response_actions_to_terminal_buffer("%1", &execution)
        .unwrap();
    service
        .queue_ordered_provider_header_with_text("%1", &execution, &action, "ACK_HEADER".into())
        .unwrap();
    if !transaction_queue {
        service
            .queue_ordered_provider_result("%1", &execution, &action, &result)
            .unwrap();
    }
    let key = service
        .presentation
        .agent_queued_provider_headers
        .keys()
        .find(|key| key.4 == ordinal)
        .unwrap()
        .clone();
    let response_id = key.3.as_str().to_string();
    let mut entries = Vec::new();
    for (index, command) in commands.iter().enumerate() {
        service.presentation.occurrence_clock_millis = Some(Some(1_000 + index as u64 * 1_000));
        if transaction_queue {
            service
                .queue_ordered_provider_transaction_command(
                    "%1",
                    &execution,
                    &action,
                    command,
                    &format!("marker-{index}"),
                )
                .unwrap();
        } else {
            entries.push(service.presentation_entry(
                "%1",
                mez_core::presentation::PresentationOccurrenceBasis::LocalAcceptance,
            ));
        }
    }
    if transaction_queue {
        entries = service.presentation.agent_queued_provider_commands[&key]
            .iter()
            .map(|(_, entry)| entry.clone())
            .collect();
    } else {
        service.presentation.agent_queued_provider_commands.insert(
            key,
            commands
                .iter()
                .zip(&entries)
                .map(|(command, entry)| (command.to_string(), entry.clone()))
                .collect(),
        );
    }
    service.presentation.occurrence_clock_millis = Some(Some(99_000));
    CommandFixture {
        service,
        execution,
        store,
        entries,
        commands: commands.iter().map(|command| command.to_string()).collect(),
        ordinal,
        response_id,
    }
}

/// Checks visible/copy occurrences and the exact durable command identities,
/// original observations, accepted source order, and response/action ownership.
fn assert_command_publications(fixture: &CommandFixture) {
    let conversation = &fixture
        .service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id;
    let persisted = fixture.store.inspect_presentation(conversation).unwrap();
    let commands = persisted
        .iter()
        .filter(|entry| entry.source_content_type.as_deref() == Some(ACTIVITY_CONTENT_TYPE))
        .filter_map(|entry| {
            let source = ActivitySource::decode(entry.source_text.as_deref().unwrap()).unwrap();
            (source.kind == ActivityComponentKind::Command).then_some((entry, source))
        })
        .collect::<Vec<_>>();
    assert_eq!(commands.len(), fixture.commands.len());
    let screen = fixture.service.agent_pane_screen("%1").unwrap();
    let visible = screen.normal_content_lines().join("\n");
    let copy = CopyMode::from_screen(screen, 24)
        .unwrap()
        .lines()
        .join("\n");
    let styled = screen.normal_styled_content_lines();
    for (index, ((entry, source), expected)) in commands.iter().zip(&fixture.entries).enumerate() {
        assert_eq!(entry.presentation.as_ref(), Some(expected));
        assert_eq!(source.source, fixture.commands[index]);
        assert_eq!(source.status, "accepted");
        assert_eq!(source.turn_id, fixture.execution.request.turn_id);
        assert_eq!(source.conversation_id, *conversation);
        assert_eq!(source.response_id, fixture.response_id);
        assert_eq!(source.action_id.as_deref(), Some("command-owner"));
        assert_eq!(source.action_ordinal, Some(fixture.ordinal));
        assert!(
            styled
                .iter()
                .any(|row| row.presentation.as_ref().is_some_and(
                    |entry| entry.id == expected.id && entry.occurrence == expected.occurrence
                ))
        );
        let copy_sources = styled
            .iter()
            .filter(|row| {
                row.presentation
                    .as_ref()
                    .is_some_and(|entry| entry.id == expected.id)
            })
            .filter_map(|row| row.copy_source.as_ref())
            .collect::<Vec<_>>();
        assert!(!copy_sources.is_empty());
        assert!(copy_sources.iter().all(|source| {
            source.id.owner() == expected.id && source.text.as_ref() == fixture.commands[index]
        }));
        let count = fixture
            .commands
            .iter()
            .filter(|command| *command == &fixture.commands[index])
            .count();
        assert_eq!(
            visible.matches(&fixture.commands[index]).count(),
            count,
            "{visible}"
        );
        assert_eq!(
            copy.matches(&fixture.commands[index]).count(),
            count,
            "{copy}"
        );
    }
    assert!(fixture.service.pending_agent_provider_tasks().is_empty());
    assert!(fixture.service.pending_native_shell_actions().is_empty());
    assert!(
        fixture
            .service
            .running_shell_transactions_for_tests()
            .is_empty()
    );
}

/// First, middle, and last command pre-install errors retain exactly the failed
/// entry and suffix. Repeated failure cannot republish an installed prefix or
/// advance result/say siblings. Successful retries preserve source, observation,
/// copy and persistence identities, and replay creates no new durable records.
#[test]
fn runtime_ordered_command_ack_preserves_failed_suffix_and_original_entries() {
    for failed in 0..3 {
        let mut fixture = command_fixture(
            &format!("suffix-{failed}"),
            &["FIRST_ACK", "SECOND_ACK", "THIRD_ACK"],
            false,
        );
        let key = fixture
            .service
            .presentation
            .agent_queued_provider_commands
            .keys()
            .next()
            .unwrap()
            .clone();
        let context =
            fixture.service.agent_turn_contexts()[&fixture.execution.request.turn_id].clone();
        fixture
            .service
            .fail_agent_presentation_install_for_tests(1 + failed);
        assert!(
            fixture
                .service
                .drain_ordered_provider_headers_once("%1", &fixture.execution)
                .is_err()
        );
        let retained = fixture.service.presentation.agent_queued_provider_commands[&key].clone();
        assert_eq!(
            retained.iter().map(|(_, entry)| entry).collect::<Vec<_>>(),
            fixture.entries[failed..].iter().collect::<Vec<_>>()
        );
        assert_eq!(
            retained
                .iter()
                .map(|(command, _)| command)
                .collect::<Vec<_>>(),
            fixture.commands[failed..].iter().collect::<Vec<_>>()
        );
        for (index, entry) in fixture.entries.iter().enumerate() {
            assert_eq!(
                fixture
                    .service
                    .presentation
                    .agent_published_provider_commands
                    .contains(&(key.clone(), entry.id.clone())),
                index < failed
            );
        }
        let partial = fixture
            .service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        assert!(
            !partial.contains("ACK_RESULT") && !partial.contains("ACK_SAY"),
            "{partial}"
        );
        let records = fixture.store.inspect_presentation(&key.2).unwrap();
        for _ in 0..2 {
            fixture.service.fail_agent_presentation_install_for_tests(0);
            assert!(
                fixture
                    .service
                    .drain_ordered_provider_headers_once("%1", &fixture.execution)
                    .is_err()
            );
            assert_eq!(fixture.store.inspect_presentation(&key.2).unwrap(), records);
            assert_eq!(
                fixture.service.presentation.agent_queued_provider_commands[&key],
                retained
            );
        }
        fixture
            .service
            .flush_ordered_provider_headers("%1", &fixture.execution)
            .unwrap();
        fixture
            .service
            .flush_ordered_provider_headers("%1", &fixture.execution)
            .unwrap();
        assert!(
            !fixture
                .service
                .presentation
                .agent_queued_provider_commands
                .contains_key(&key)
        );
        assert_command_publications(&fixture);
        let rows = fixture
            .service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        let mut position = 0;
        for marker in [
            "ACK_HEADER",
            "FIRST_ACK",
            "SECOND_ACK",
            "THIRD_ACK",
            "ACK_RESULT",
            "ACK_SAY",
        ] {
            assert_eq!(rows.matches(marker).count(), 1, "{rows}");
            let next = rows.find(marker).unwrap();
            assert!(next >= position, "{rows}");
            position = next;
        }
        assert_eq!(
            fixture.service.agent_turn_contexts()[&fixture.execution.request.turn_id],
            context
        );
        let records = fixture.store.inspect_presentation(&key.2).unwrap();
        set_agent_pane_screen_for_test(
            &mut fixture.service,
            "%1",
            TerminalScreen::new(Size::new(120, 24).unwrap(), 120).unwrap(),
        );
        fixture
            .service
            .replay_agent_presentation_entries_to_terminal_buffer("%1", &records)
            .unwrap();
        assert_command_publications(&fixture);
        assert_eq!(fixture.store.inspect_presentation(&key.2).unwrap(), records);
    }
}

/// Equal command text is not an idempotency identity. Two accepted entries keep
/// separate original observations and durable/copy records across a partial
/// failure; only the installed identity is acknowledged, not its equal text.
#[test]
fn runtime_ordered_command_ack_keeps_equal_text_distinct() {
    let mut fixture = command_fixture("equal", &["EQUAL_ACK", "EQUAL_ACK"], false);
    assert_ne!(fixture.entries[0].id, fixture.entries[1].id);
    assert_ne!(fixture.entries[0].occurrence, fixture.entries[1].occurrence);
    fixture.service.fail_agent_presentation_install_for_tests(2);
    assert!(
        fixture
            .service
            .drain_ordered_provider_headers_once("%1", &fixture.execution)
            .is_err()
    );
    fixture
        .service
        .flush_ordered_provider_headers("%1", &fixture.execution)
        .unwrap();
    fixture
        .service
        .flush_ordered_provider_headers("%1", &fixture.execution)
        .unwrap();
    assert_command_publications(&fixture);
}

/// The production transaction queue accumulates equal text with different
/// markers behind an earlier ordinal barrier. Repeated callbacks for an exact
/// queued or published marker remain no-ops; a distinct later marker is a new
/// publication, not text-based deduplication or an execution retry.
#[test]
fn runtime_ordered_command_ack_transaction_markers_are_exact() {
    let mut fixture = command_fixture("transaction", &["TX_EQUAL_ACK", "TX_EQUAL_ACK"], true);
    let key = fixture
        .service
        .presentation
        .agent_queued_provider_commands
        .keys()
        .next()
        .unwrap()
        .clone();
    let actions = fixture
        .execution
        .response
        .action_batch
        .as_ref()
        .unwrap()
        .actions
        .clone();
    fixture
        .service
        .queue_ordered_provider_transaction_command(
            "%1",
            &fixture.execution,
            &actions[1],
            "TX_EQUAL_ACK",
            "marker-0",
        )
        .unwrap();
    assert_eq!(
        fixture.service.presentation.agent_queued_provider_commands[&key].len(),
        2
    );
    fixture
        .service
        .queue_ordered_provider_header_with_text(
            "%1",
            &fixture.execution,
            &actions[0],
            "EARLIER_HEADER".into(),
        )
        .unwrap();
    fixture
        .service
        .flush_ordered_provider_headers("%1", &fixture.execution)
        .unwrap();
    assert_command_publications(&fixture);
    let before = fixture.store.inspect_presentation(&key.2).unwrap();
    fixture
        .service
        .queue_ordered_provider_transaction_command(
            "%1",
            &fixture.execution,
            &actions[1],
            "TX_EQUAL_ACK",
            "marker-0",
        )
        .unwrap();
    assert_eq!(fixture.store.inspect_presentation(&key.2).unwrap(), before);
    fixture.service.presentation.occurrence_clock_millis = Some(Some(123_000));
    fixture
        .service
        .queue_ordered_provider_transaction_command(
            "%1",
            &fixture.execution,
            &actions[1],
            "TX_EQUAL_ACK",
            "marker-new",
        )
        .unwrap();
    let after = fixture.store.inspect_presentation(&key.2).unwrap();
    let new = after
        .iter()
        .find(|record| {
            record
                .presentation
                .as_ref()
                .is_some_and(|entry| entry.id.ends_with(":marker-new"))
        })
        .unwrap();
    fixture.entries.push(new.presentation.clone().unwrap());
    fixture.commands.push("TX_EQUAL_ACK".into());
    assert_eq!(fixture.entries[2].occurrence.unix_millis, Some(123_000));
    assert_command_publications(&fixture);
}

/// A replacement conversation cannot consume or publish an older failed suffix.
/// Existing terminal cleanup then retires only that old turn's queued and
/// published receipts. This tests isolation without changing lifecycle draining
/// or claiming to fix deferred-log transfer during teardown.
#[test]
fn runtime_ordered_command_ack_rebind_fences_retained_suffix() {
    let mut fixture = command_fixture("rebind", &["OLD_FIRST_ACK", "OLD_SECOND_ACK"], false);
    let key = fixture
        .service
        .presentation
        .agent_queued_provider_commands
        .keys()
        .next()
        .unwrap()
        .clone();
    fixture.service.fail_agent_presentation_install_for_tests(2);
    assert!(
        fixture
            .service
            .drain_ordered_provider_headers_once("%1", &fixture.execution)
            .is_err()
    );
    let retained = fixture.service.presentation.agent_queued_provider_commands[&key].clone();
    fixture
        .service
        .agent_shell_store_mut()
        .bind_conversation("%1", "ack-replacement", 0)
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut fixture.service,
        "%1",
        TerminalScreen::new(Size::new(120, 24).unwrap(), 120).unwrap(),
    );
    fixture
        .service
        .append_agent_status_text_to_terminal_buffer("%1", "REPLACEMENT_ACK")
        .unwrap();
    let before = fixture.service.agent_pane_screen("%1").unwrap().clone();
    fixture
        .service
        .flush_ordered_provider_headers("%1", &fixture.execution)
        .unwrap();
    assert_eq!(fixture.service.agent_pane_screen("%1").unwrap(), &before);
    assert_eq!(
        fixture.service.presentation.agent_queued_provider_commands[&key],
        retained
    );
    assert!(
        fixture
            .store
            .inspect_presentation("ack-replacement")
            .unwrap()
            .iter()
            .all(|record| record
                .source_text
                .as_deref()
                .is_none_or(|source| !source.contains("OLD_SECOND_ACK")))
    );
    fixture
        .service
        .clear_settled_provider_log_groups_for_turn(&fixture.execution.request.turn_id);
    assert!(
        !fixture
            .service
            .presentation
            .agent_queued_provider_commands
            .contains_key(&key)
    );
    assert!(
        !fixture
            .service
            .presentation
            .agent_published_provider_commands
            .iter()
            .any(|(owner, _)| owner == &key)
    );
    assert_eq!(fixture.service.agent_pane_screen("%1").unwrap(), &before);
}
