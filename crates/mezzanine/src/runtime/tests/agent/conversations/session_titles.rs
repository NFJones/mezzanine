//! Runtime tests for generated session-title scheduling and settlement.

use super::*;
use crate::runtime::{
    AgentSessionTitleEvent, AgentSessionTitleOutcome, RuntimeSideEffect, SessionTitleDenial,
};
use crate::session_title::SessionTitlePolicy;
use mez_core::ids::ClientId;

/// Appends one user prompt so a conversation enters the saved-session catalog.
fn append_user_prompt(store: &AgentTranscriptStore, conversation_id: &str, content: &str) {
    store
        .append(&TranscriptEntry {
            conversation_id: conversation_id.to_string(),
            sequence: 1,
            created_at_unix_seconds: 10,
            role: TranscriptRole::User,
            turn_id: format!("turn-{conversation_id}"),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: content.to_string(),
        })
        .unwrap();
}

/// Creates one further live pane with its own conversation.
fn split_title_conversation(
    service: &mut RuntimeSessionService,
    primary: &ClientId,
) -> (String, String) {
    let pane_id = service
        .split_pane_with_process(primary, SplitDirection::Vertical, Some("cat >/dev/null"))
        .unwrap()
        .pane_id;
    service
        .agent_shell_store_mut()
        .enter_or_resume(pane_id.as_str())
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get(pane_id.as_str())
        .unwrap()
        .session_id
        .clone();
    (pane_id, conversation_id)
}

/// Counts the bounded title-denial trace lines retained for one pane.
fn title_skip_trace_count(service: &RuntimeSessionService, pane_id: &str) -> usize {
    service
        .agent_pane_trace_log_text(pane_id)
        .unwrap_or_default()
        .matches("session title skipped")
        .count()
}

/// Builds one runtime service whose live conversation can serve a title request.
///
/// The transcript store supplies the bounded title indices, the auth store makes
/// the configured provider available, and the live pane supplies the
/// conversation model profile and the pane status line.
fn title_ready_service(
    name: &str,
) -> (
    RuntimeSessionService,
    AgentTranscriptStore,
    ClientId,
    String,
) {
    let root = temp_root(name);
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(root.clone());
    service.set_agent_transcript_store(transcript_store.clone());
    service.set_auth_store(AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&root),
    ));
    let primary = service
        .attach_primary("primary", true, Size::new(120, 24).unwrap(), 10)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    (service, transcript_store, primary, conversation_id)
}

/// Appends one numbered user prompt so the bounded summary tracks it.
///
/// The summary sidecar keeps the first user prompt and the most recent one, which
/// is exactly the pair the generated-title inputs read.
fn append_numbered_user_prompt(
    store: &AgentTranscriptStore,
    conversation_id: &str,
    sequence: u64,
    content: &str,
) {
    store
        .append(&TranscriptEntry {
            conversation_id: conversation_id.to_string(),
            sequence,
            created_at_unix_seconds: 10 + sequence,
            role: TranscriptRole::User,
            turn_id: format!("turn-{conversation_id}-{sequence}"),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: content.to_string(),
        })
        .unwrap();
}

/// Completes the prompt turn one test prompt started.
///
/// A pane whose previous turn is still running turns the next prompt into
/// steering input instead of a new accepted prompt turn. This canonical
/// completion path is also what releases the scheduler slot and the turn's
/// provider task, so the next prompt is a new accepted prompt turn and the
/// unrelated saturation gate for optional display work is not what decides the
/// title cadence here.
fn complete_running_prompt_turn(service: &mut RuntimeSessionService, pane_id: &str) {
    let running_turn_id = service
        .agent_shell_store()
        .get(pane_id)
        .and_then(|session| session.running_turn_id.clone())
        .expect("each accepted prompt starts a running turn");
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == running_turn_id)
        .cloned()
        .expect("the running turn is recorded in the ledger");
    service
        .complete_running_agent_turn_and_start_ready(
            &turn,
            mez_agent::AgentTurnState::Completed,
            "test_prompt_turn_completed",
        )
        .unwrap();
}

/// Renders the bounded inputs one dispatched title request carries.
fn title_request_evidence(request: &mez_agent::ModelRequest) -> String {
    request
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Verifies one conversation start queues exactly one generated-title task and
/// that the title path never creates a turn of its own.
#[test]
fn runtime_conversation_start_queues_exactly_one_title_task() {
    let (mut service, _store, primary, conversation_id) =
        title_ready_service("runtime-title-queued");
    assert!(service.pending_agent_session_title_tasks().is_empty());
    assert!(service.agent_turn_ledger().turns().is_empty());

    let start = service
        .execute_agent_shell_command(&primary, "summarize the backlog")
        .unwrap();
    assert!(start.contains("\"state\":\"running\""), "{start}");
    assert_eq!(service.agent_turn_ledger().turns().len(), 1);
    assert_eq!(
        service.pending_agent_session_title_tasks(),
        vec![conversation_id.clone()]
    );
    assert!(service.agent_session_title_task_is_scheduled(&conversation_id));

    // A second objective publish for the same conversation is refused, so one
    // conversation never holds two in-flight title tasks.
    assert!(
        service
            .schedule_runtime_agent_session_title(&conversation_id, Some("summarize the backlog"))
            .is_err()
    );
    assert_eq!(service.pending_agent_session_title_tasks().len(), 1);
    assert_eq!(service.agent_turn_ledger().turns().len(), 1);
}

/// Verifies a successful settlement writes the title mirror, renders the row,
/// and leaves turns, transcript, and the published objective untouched.
#[test]
fn runtime_generated_title_settlement_writes_the_row_title() {
    let (mut service, transcript_store, primary, conversation_id) =
        title_ready_service("runtime-title-settlement");
    service
        .execute_agent_shell_command(&primary, "inspect the backlog")
        .unwrap();
    assert_eq!(service.pending_agent_session_title_tasks().len(), 1);
    // A conversation enters the saved-session catalog once it has transcript
    // rows, which is what the resume browser renders.
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: conversation_id.clone(),
            sequence: 1,
            created_at_unix_seconds: 10,
            role: TranscriptRole::User,
            turn_id: "turn-title-settlement".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "inspect the backlog".to_string(),
        })
        .unwrap();

    // The conversation may have no transcript rows yet; the side channel must
    // leave that count unchanged either way.
    let transcript_before = transcript_store
        .inspect(&conversation_id)
        .unwrap_or_default()
        .len();
    let objective_before = transcript_store
        .session_objective_mirror(&conversation_id)
        .unwrap();
    let turns_before = service.agent_turn_ledger().turns().len();

    assert!(
        service
            .claim_agent_session_title_task(&conversation_id)
            .unwrap()
            .is_some()
    );
    let transition = service
        .apply_agent_session_title_transition(AgentSessionTitleEvent::Settled {
            conversation_id: conversation_id.clone(),
            outcome: AgentSessionTitleOutcome::Generated("Inspect the backlog".to_string()),
        })
        .unwrap();
    assert!(transition.applied);

    assert_eq!(
        transcript_store
            .session_generated_title(&conversation_id)
            .unwrap()
            .map(|mirror| mirror.title),
        Some("Inspect the backlog".to_string())
    );
    let browser = service.saved_sessions_record_browser().unwrap();
    let row = browser
        .records()
        .iter()
        .find(|record| record.id == conversation_id)
        .expect("generated title row should be rendered");
    assert_eq!(
        row.title,
        format!("{conversation_id} - Inspect the backlog")
    );

    // The side channel never appends a transcript entry, creates a turn, or
    // rewrites the published objective that produced the bounded request.
    assert_eq!(
        transcript_store
            .inspect(&conversation_id)
            .unwrap_or_default()
            .len(),
        transcript_before
    );
    assert_eq!(service.agent_turn_ledger().turns().len(), turns_before);
    assert_eq!(
        transcript_store
            .session_objective_mirror(&conversation_id)
            .unwrap(),
        objective_before
    );
    assert!(service.pending_agent_session_title_tasks().is_empty());
    assert!(!service.agent_session_title_task_is_scheduled(&conversation_id));
}

/// Verifies provider failure degrades to the objective-derived title with a
/// bounded reason, allows exactly one retry, then retires the conversation.
#[test]
fn runtime_generated_title_failure_degrades_with_a_bounded_reason() {
    let (mut service, transcript_store, primary, conversation_id) =
        title_ready_service("runtime-title-degrade");
    service
        .execute_agent_shell_command(&primary, "inspect the backlog")
        .unwrap();
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: conversation_id.clone(),
            sequence: 1,
            created_at_unix_seconds: 10,
            role: TranscriptRole::User,
            turn_id: "turn-title-degrade".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "inspect the backlog".to_string(),
        })
        .unwrap();
    let objective = transcript_store
        .session_objective_mirror(&conversation_id)
        .unwrap()
        .expect("objective mirror should be published at conversation start")
        .objective;

    assert!(
        service
            .claim_agent_session_title_task(&conversation_id)
            .unwrap()
            .is_some()
    );

    let first = service
        .apply_agent_session_title_transition(AgentSessionTitleEvent::Settled {
            conversation_id: conversation_id.clone(),
            outcome: AgentSessionTitleOutcome::Rejected("timeout".to_string()),
        })
        .unwrap();
    assert!(first.applied);
    assert!(matches!(
        first.side_effects.as_slice(),
        [RuntimeSideEffect::DispatchAgentSessionTitle { conversation_id: id }]
            if id == &conversation_id
    ));
    assert_eq!(
        service.pending_agent_session_title_tasks(),
        vec![conversation_id.clone()]
    );

    assert!(
        service
            .claim_agent_session_title_task(&conversation_id)
            .unwrap()
            .is_some()
    );
    let terminal = service
        .apply_agent_session_title_transition(AgentSessionTitleEvent::Settled {
            conversation_id: conversation_id.clone(),
            outcome: AgentSessionTitleOutcome::Rejected("timeout".to_string()),
        })
        .unwrap();
    assert!(terminal.applied);
    assert!(terminal.side_effects.is_empty());
    assert!(service.pending_agent_session_title_tasks().is_empty());
    assert!(
        service
            .schedule_runtime_agent_session_title(&conversation_id, Some("inspect the backlog"))
            .is_err()
    );

    let trace = service.agent_pane_trace_log_text("%1").unwrap_or_default();
    assert!(trace.contains("session title degraded"), "{trace}");
    assert!(trace.contains("reason: timeout"), "{trace}");
    assert!(trace.contains("attempts exhausted"), "{trace}");

    // No generated title is stored, so the row falls back to the bounded
    // objective-derived title.
    assert!(
        transcript_store
            .session_generated_title(&conversation_id)
            .unwrap()
            .is_none()
    );
    let browser = service.saved_sessions_record_browser().unwrap();
    let row = browser
        .records()
        .iter()
        .find(|record| record.id == conversation_id)
        .expect("degraded row should be rendered");
    assert_eq!(row.title, format!("{conversation_id} - {objective}"));
}

/// Verifies every policy other than `generated` is a hard opt-out that spends
/// no provider request and queues no title task.
#[test]
fn runtime_generated_title_policy_opt_out_queues_nothing() {
    for policy in [
        SessionTitlePolicy::Objective,
        SessionTitlePolicy::LastPrompt,
        SessionTitlePolicy::FirstPrompt,
    ] {
        let (mut service, _store, primary, conversation_id) =
            title_ready_service("runtime-title-opt-out");
        service.set_agent_session_title_policy(policy);
        service
            .execute_agent_shell_command(&primary, "summarize the backlog")
            .unwrap();
        assert!(
            service.pending_agent_session_title_tasks().is_empty(),
            "policy {} must opt out",
            policy.as_str()
        );
        assert!(!service.agent_session_title_task_is_scheduled(&conversation_id));
    }
}

/// Verifies the configured in-flight cap holds across multiple conversations and
/// that settling one in-flight task releases capacity.
#[test]
fn runtime_generated_title_concurrency_cap_holds() {
    let (mut service, _store, primary, first_conversation) =
        title_ready_service("runtime-title-cap");
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .ok();
    let second_pane = service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some("cat >/dev/null"))
        .unwrap()
        .pane_id;
    service
        .agent_shell_store_mut()
        .enter_or_resume(second_pane.as_str())
        .unwrap();
    let second_conversation = service
        .agent_shell_store()
        .get(second_pane.as_str())
        .unwrap()
        .session_id
        .clone();
    let third_pane = service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some("cat >/dev/null"))
        .unwrap()
        .pane_id;
    service
        .agent_shell_store_mut()
        .enter_or_resume(third_pane.as_str())
        .unwrap();
    let third_conversation = service
        .agent_shell_store()
        .get(third_pane.as_str())
        .unwrap()
        .session_id
        .clone();
    assert_ne!(third_conversation, first_conversation);
    assert_ne!(third_conversation, second_conversation);

    assert!(
        service
            .schedule_runtime_agent_session_title(&first_conversation, Some("first objective"))
            .is_ok()
    );
    assert!(
        service
            .schedule_runtime_agent_session_title(&second_conversation, Some("second objective"))
            .is_ok()
    );
    // The third conversation is refused while the configured cap is saturated.
    assert_eq!(
        service.schedule_runtime_agent_session_title(&third_conversation, Some("third objective")),
        Err(SessionTitleDenial::ConcurrencyCap)
    );
    assert_eq!(service.pending_agent_session_title_tasks().len(), 2);
    let mut pending = service.pending_agent_session_title_tasks();
    let mut expected = vec![first_conversation.clone(), second_conversation.clone()];
    pending.sort();
    expected.sort();
    assert_eq!(pending, expected);

    // Settling one in-flight task releases capacity for the third conversation.
    assert!(
        service
            .claim_agent_session_title_task(&first_conversation)
            .unwrap()
            .is_some()
    );
    assert!(
        service
            .apply_agent_session_title_transition(AgentSessionTitleEvent::Settled {
                conversation_id: first_conversation.clone(),
                outcome: AgentSessionTitleOutcome::Generated("First title".to_string()),
            })
            .unwrap()
            .applied
    );
    assert!(
        service
            .schedule_runtime_agent_session_title(&third_conversation, Some("third objective"))
            .is_ok()
    );
    let mut pending = service.pending_agent_session_title_tasks();
    let mut expected = vec![second_conversation.clone(), third_conversation.clone()];
    pending.sort();
    expected.sort();
    assert_eq!(pending, expected);
}

/// Verifies closing a session leaves no pending title task and no orphaned
/// claim, and that a late worker event cannot write a title afterwards.
#[test]
fn runtime_session_close_cancels_title_tasks() {
    let (mut service, transcript_store, primary, conversation_id) =
        title_ready_service("runtime-title-cancel");
    service
        .execute_agent_shell_command(&primary, "inspect the backlog")
        .unwrap();
    assert!(
        service
            .claim_agent_session_title_task(&conversation_id)
            .unwrap()
            .is_some()
    );

    service.cleanup_removed_pane_runtime_state("%1").unwrap();
    assert!(service.pending_agent_session_title_tasks().is_empty());
    assert!(!service.agent_session_title_task_is_scheduled(&conversation_id));

    let late = service
        .apply_agent_session_title_transition(AgentSessionTitleEvent::Settled {
            conversation_id: conversation_id.clone(),
            outcome: AgentSessionTitleOutcome::Generated("Late title".to_string()),
        })
        .unwrap();
    assert!(!late.applied);
    assert!(
        transcript_store
            .session_generated_title(&conversation_id)
            .unwrap()
            .is_none()
    );
}

/// Verifies the ordinary per-turn objective refresh of a settled conversation
/// reads no title or objective index while its refresh window is not due.
#[test]
fn runtime_settled_conversation_reads_no_title_index_per_turn() {
    let (mut service, transcript_store, primary, conversation_id) =
        title_ready_service("runtime-title-zero-reads");
    service
        .execute_agent_shell_command(&primary, "inspect the backlog")
        .unwrap();
    append_user_prompt(&transcript_store, &conversation_id, "inspect the backlog");
    let objective = transcript_store
        .session_objective_mirror(&conversation_id)
        .unwrap()
        .expect("objective mirror should be published at conversation start")
        .objective;
    assert!(
        service
            .claim_agent_session_title_task(&conversation_id)
            .unwrap()
            .is_some()
    );
    assert!(
        service
            .apply_agent_session_title_transition(AgentSessionTitleEvent::Settled {
                conversation_id: conversation_id.clone(),
                outcome: AgentSessionTitleOutcome::Generated("Inspect the backlog".to_string()),
            })
            .unwrap()
            .applied
    );

    let titles_before = transcript_store.session_title_mirror_status();
    let objectives_before = transcript_store.session_objective_mirror_status();
    for _turn in 0..3 {
        // The per-turn path republishes the same objective and re-attempts
        // admission for the same conversation. Its refresh window is closed and
        // the cadence is not due, so it is refused before any store work.
        service.mirror_runtime_agent_objective(&conversation_id, Some(&objective));
        assert_eq!(
            service.schedule_runtime_agent_session_title(&conversation_id, Some(&objective)),
            Err(SessionTitleDenial::AlreadyScheduled)
        );
    }

    let titles_after = transcript_store.session_title_mirror_status();
    let objectives_after = transcript_store.session_objective_mirror_status();
    assert_eq!(
        titles_after.index_reads, titles_before.index_reads,
        "a settled conversation must not read the title index per turn: {titles_after:?}"
    );
    assert_eq!(titles_after.index_writes, titles_before.index_writes);
    assert_eq!(
        objectives_after.index_reads, objectives_before.index_reads,
        "an unchanged objective refresh must not read the objective index"
    );
    assert!(service.pending_agent_session_title_tasks().is_empty());
}

/// Verifies a title is generated after the first prompt and refreshed once
/// every five accepted inbound prompt turns.
#[test]
fn runtime_title_cadence_refreshes_once_every_five_prompt_turns() {
    let (mut service, transcript_store, primary, conversation_id) =
        title_ready_service("runtime-title-refresh-cadence");
    let prompts = [
        "summarize the first backlog item",
        "summarize the second backlog item",
        "summarize the third backlog item",
        "summarize the fourth backlog item",
        "summarize the fifth backlog item",
        "summarize the sixth backlog item",
    ];

    let mut requested_prompts = Vec::new();
    let mut requests = Vec::new();
    for (index, prompt) in prompts.iter().enumerate() {
        append_numbered_user_prompt(
            &transcript_store,
            &conversation_id,
            u64::try_from(index).unwrap() + 1,
            prompt,
        );
        service
            .execute_agent_shell_command(&primary, prompt)
            .unwrap();
        complete_running_prompt_turn(&mut service, "%1");
        if let Some(dispatch) = service
            .claim_agent_session_title_task(&conversation_id)
            .unwrap()
        {
            requested_prompts.push(index + 1);
            requests.push(dispatch.task.request);
            service
                .apply_agent_session_title_transition(AgentSessionTitleEvent::Settled {
                    conversation_id: conversation_id.clone(),
                    outcome: AgentSessionTitleOutcome::Generated(format!("Backlog title {index}")),
                })
                .unwrap();
        }
    }

    // Six accepted prompts spend exactly two provider requests: the generation
    // after the first prompt and the refresh the fifth further prompt made due.
    assert_eq!(requested_prompts, vec![1, 6]);
    assert_eq!(requests.len(), 2);

    // The first generation keeps its inputs unchanged and carries no refresh
    // summary, while the refresh carries the latest prompt as its summary input.
    let first = title_request_evidence(&requests[0]);
    assert!(
        first.contains("first_prompt: summarize the first backlog item"),
        "{first}"
    );
    assert!(
        !first.contains("summary: "),
        "a first generation must carry no refresh summary: {first}"
    );
    let refresh = title_request_evidence(&requests[1]);
    assert!(
        refresh.contains("summary: summarize the sixth backlog item"),
        "the refresh must carry the latest prompt: {refresh}"
    );

    assert!(service.pending_agent_session_title_tasks().is_empty());
    assert!(!service.agent_session_title_task_is_scheduled(&conversation_id));
    let trace = service.agent_pane_trace_log_text("%1").unwrap_or_default();
    assert!(
        trace.contains("reason: refresh prompt ordinal: 6"),
        "{trace}"
    );
}

/// Verifies a stored title written before this daemon started is adopted once
/// and then refreshed on the ordinary five-prompt cadence, not immediately.
#[test]
fn runtime_stored_title_is_adopted_then_refreshed_after_five_prompts() {
    let (mut service, transcript_store, primary, conversation_id) =
        title_ready_service("runtime-title-restart-adoption");
    assert!(
        transcript_store
            .mirror_session_generated_title(&conversation_id, "Adopted backlog title", 10)
            .unwrap()
    );
    let prompts = [
        "summarize the first backlog item",
        "summarize the second backlog item",
        "summarize the third backlog item",
        "summarize the fourth backlog item",
        "summarize the fifth backlog item",
        "summarize the sixth backlog item",
    ];

    let mut requested_prompts = Vec::new();
    let mut requests = Vec::new();
    for (index, prompt) in prompts.iter().enumerate() {
        append_numbered_user_prompt(
            &transcript_store,
            &conversation_id,
            u64::try_from(index).unwrap() + 1,
            prompt,
        );
        service
            .execute_agent_shell_command(&primary, prompt)
            .unwrap();
        complete_running_prompt_turn(&mut service, "%1");
        if let Some(dispatch) = service
            .claim_agent_session_title_task(&conversation_id)
            .unwrap()
        {
            requested_prompts.push(index + 1);
            requests.push(dispatch.task.request);
            service
                .apply_agent_session_title_transition(AgentSessionTitleEvent::Settled {
                    conversation_id: conversation_id.clone(),
                    outcome: AgentSessionTitleOutcome::Generated(format!("Backlog title {index}")),
                })
                .unwrap();
        }
    }

    // The first prompt adopts the stored title instead of spending a request, so
    // prompt six is the first refresh.
    assert_eq!(requested_prompts, vec![6]);
    assert_eq!(requests.len(), 1);
    let refresh = title_request_evidence(&requests[0]);
    assert!(
        refresh.contains("summary: summarize the sixth backlog item"),
        "{refresh}"
    );
}

/// Verifies an ephemeral conversation queues no title work and touches no title
/// index, even when its prompt path attempts title admission directly.
#[test]
fn runtime_ephemeral_conversation_queues_no_title_task() {
    let (mut service, transcript_store, _primary, _conversation_id) =
        title_ready_service("runtime-title-ephemeral-skip");
    let ephemeral_conversation = "9f0b1c2d-3e4f-4a5b-8c6d-7e8f90a1b2c3".to_string();
    service
        .agent_shell_store_mut()
        .bind_ephemeral_conversation_with_lineage_and_transcript_source(
            "%1",
            &ephemeral_conversation,
            0,
            None,
            None,
            0,
        )
        .unwrap();

    let titles_before = transcript_store.session_title_mirror_status();
    let objectives_before = transcript_store.session_objective_mirror_status();
    assert!(
        !service.mirror_runtime_agent_objective(&ephemeral_conversation, Some("forked loop work"))
    );
    assert_eq!(
        service.schedule_runtime_agent_session_title(
            &ephemeral_conversation,
            Some("forked loop work")
        ),
        Ok(())
    );
    assert!(!service.agent_session_title_task_is_scheduled(&ephemeral_conversation));
    assert!(
        service
            .claim_agent_session_title_task(&ephemeral_conversation)
            .unwrap()
            .is_none()
    );

    let titles_after = transcript_store.session_title_mirror_status();
    let objectives_after = transcript_store.session_objective_mirror_status();
    assert_eq!(titles_after.index_reads, titles_before.index_reads);
    assert_eq!(titles_after.index_writes, titles_before.index_writes);
    assert_eq!(objectives_after.index_reads, objectives_before.index_reads);
    assert_eq!(
        objectives_after.index_writes,
        objectives_before.index_writes
    );
}

/// Verifies an unreadable title sidecar refuses generation as its own bounded
/// denial and that generation recovers on a later admission.
#[test]
fn runtime_unreadable_title_sidecar_refuses_then_recovers() {
    let cases = [
        (
            "runtime-title-corrupt-sidecar",
            "{ not a title index".to_string(),
        ),
        (
            "runtime-title-oversized-sidecar",
            "x".repeat(4 * 1024 * 1024 + 1),
        ),
        (
            "runtime-title-version-sidecar",
            r#"{"version": 99, "titles": []}"#.to_string(),
        ),
    ];
    for (name, contents) in cases {
        let (mut service, transcript_store, _primary, conversation_id) = title_ready_service(name);
        append_user_prompt(&transcript_store, &conversation_id, "inspect the backlog");
        std::fs::create_dir_all(transcript_store.root()).unwrap();
        std::fs::write(
            transcript_store.root().join("session-titles.json"),
            contents,
        )
        .unwrap();

        assert_eq!(
            service.schedule_runtime_agent_session_title(&conversation_id, Some("objective")),
            Err(SessionTitleDenial::StorageUnavailable),
            "{name}"
        );
        assert!(
            service.pending_agent_session_title_tasks().is_empty(),
            "{name}"
        );
        assert_eq!(
            transcript_store.session_title_mirror_status().recoveries,
            1,
            "{name}"
        );

        // The quarantined index reads as empty, so the next admission is admitted
        // and the generation that follows rebuilds the index.
        assert!(
            service
                .schedule_runtime_agent_session_title(&conversation_id, Some("objective"))
                .is_ok(),
            "{name}"
        );
        assert!(
            service
                .claim_agent_session_title_task(&conversation_id)
                .unwrap()
                .is_some(),
            "{name}"
        );
        assert!(
            service
                .apply_agent_session_title_transition(AgentSessionTitleEvent::Settled {
                    conversation_id: conversation_id.clone(),
                    outcome: AgentSessionTitleOutcome::Generated("Inspect the backlog".to_string()),
                })
                .unwrap()
                .applied,
            "{name}"
        );
        assert_eq!(
            transcript_store
                .session_generated_title(&conversation_id)
                .unwrap()
                .map(|mirror| mirror.title),
            Some("Inspect the backlog".to_string()),
            "{name}"
        );
    }
}

/// Verifies switching the policy away from `generated` between attempts ends
/// generation instead of spending a second provider call.
#[test]
fn runtime_policy_switch_between_attempts_prevents_the_second_provider_call() {
    let (mut service, transcript_store, primary, conversation_id) =
        title_ready_service("runtime-title-retry-policy");
    service
        .execute_agent_shell_command(&primary, "inspect the backlog")
        .unwrap();
    append_user_prompt(&transcript_store, &conversation_id, "inspect the backlog");
    assert!(
        service
            .claim_agent_session_title_task(&conversation_id)
            .unwrap()
            .is_some()
    );

    // The opt-out lands between the two attempts, so the retry must be refused.
    service.set_agent_session_title_policy(SessionTitlePolicy::Objective);
    let terminal = service
        .apply_agent_session_title_transition(AgentSessionTitleEvent::Settled {
            conversation_id: conversation_id.clone(),
            outcome: AgentSessionTitleOutcome::Rejected("timeout".to_string()),
        })
        .unwrap();
    assert!(terminal.applied);
    assert!(terminal.side_effects.is_empty(), "{terminal:?}");
    assert!(service.pending_agent_session_title_tasks().is_empty());
    assert!(!service.agent_session_title_task_is_scheduled(&conversation_id));
    assert!(
        service
            .claim_agent_session_title_task(&conversation_id)
            .unwrap()
            .is_none(),
        "a refused retry must leave nothing for a worker to claim"
    );
    let trace = service.agent_pane_trace_log_text("%1").unwrap_or_default();
    assert!(trace.contains("retry denied"), "{trace}");
    assert!(trace.contains("policy opt out"), "{trace}");
}

/// Verifies a manual name between attempts ends generation instead of spending a
/// second provider call.
#[test]
fn runtime_manual_name_between_attempts_prevents_the_second_provider_call() {
    let (mut service, transcript_store, primary, conversation_id) =
        title_ready_service("runtime-title-retry-manual-name");
    service
        .execute_agent_shell_command(&primary, "inspect the backlog")
        .unwrap();
    append_user_prompt(&transcript_store, &conversation_id, "inspect the backlog");
    assert!(
        service
            .claim_agent_session_title_task(&conversation_id)
            .unwrap()
            .is_some()
    );
    transcript_store
        .name_session(&conversation_id, "Operator name", 10, None, false)
        .unwrap();

    let terminal = service
        .apply_agent_session_title_transition(AgentSessionTitleEvent::Settled {
            conversation_id: conversation_id.clone(),
            outcome: AgentSessionTitleOutcome::Rejected("provider_error".to_string()),
        })
        .unwrap();
    assert!(terminal.applied);
    assert!(terminal.side_effects.is_empty(), "{terminal:?}");
    assert!(service.pending_agent_session_title_tasks().is_empty());
    let trace = service.agent_pane_trace_log_text("%1").unwrap_or_default();
    assert!(trace.contains("retry denied"), "{trace}");
    assert!(trace.contains("manual name"), "{trace}");
}

/// Verifies a manual name refuses generation without a provider call and that a
/// post-generation manual name still wins on the rendered row.
#[test]
fn runtime_manual_name_wins_without_a_provider_call() {
    let (mut service, transcript_store, _primary, conversation_id) =
        title_ready_service("runtime-title-manual-wins");
    append_user_prompt(&transcript_store, &conversation_id, "inspect the backlog");
    transcript_store
        .name_session(&conversation_id, "Operator name", 10, None, false)
        .unwrap();

    assert_eq!(
        service.schedule_runtime_agent_session_title(&conversation_id, Some("inspect the backlog")),
        Err(SessionTitleDenial::ManualName)
    );
    assert!(service.pending_agent_session_title_tasks().is_empty());
    assert!(!service.agent_session_title_task_is_scheduled(&conversation_id));
    assert!(
        service
            .claim_agent_session_title_task(&conversation_id)
            .unwrap()
            .is_none(),
        "a manual name must perform no provider call"
    );
    let trace = service.agent_pane_trace_log_text("%1").unwrap_or_default();
    assert!(!trace.contains("session title skipped"), "{trace}");
    let browser = service.saved_sessions_record_browser().unwrap();
    let row = browser
        .records()
        .iter()
        .find(|record| record.id == conversation_id)
        .expect("named row should be rendered");
    assert_eq!(row.title, format!("{conversation_id} - Operator name"));
}

/// Verifies a generated title survives a later manual name and that clearing the
/// name falls back to the stored generated title.
#[test]
fn runtime_post_generation_manual_name_keeps_the_row_name() {
    let (mut service, transcript_store, primary, conversation_id) =
        title_ready_service("runtime-title-name-after-generation");
    append_user_prompt(&transcript_store, &conversation_id, "inspect the backlog");
    assert!(
        service
            .schedule_runtime_agent_session_title(&conversation_id, Some("inspect the backlog"))
            .is_ok()
    );
    assert!(
        service
            .claim_agent_session_title_task(&conversation_id)
            .unwrap()
            .is_some()
    );
    service
        .apply_agent_session_title_transition(AgentSessionTitleEvent::Settled {
            conversation_id: conversation_id.clone(),
            outcome: AgentSessionTitleOutcome::Generated("Inspect the backlog".to_string()),
        })
        .unwrap();

    let named = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"name","method":"agent/shell/command","params":{"idempotency_key":"name-after-title","input":"/name-session Operator name"}}"#,
        &primary,
    );
    assert!(named.contains("named=true"), "{named}");
    let browser = service.saved_sessions_record_browser().unwrap();
    let row = browser
        .records()
        .iter()
        .find(|record| record.id == conversation_id)
        .expect("named row should be rendered");
    assert_eq!(row.title, format!("{conversation_id} - Operator name"));
    assert_eq!(
        transcript_store
            .session_generated_title(&conversation_id)
            .unwrap()
            .map(|mirror| mirror.title),
        Some("Inspect the backlog".to_string())
    );
}

/// Verifies a failed generation with no published objective falls back to the
/// first prompt.
#[test]
fn runtime_failed_generation_falls_back_to_the_first_prompt() {
    let (mut service, transcript_store, _primary, conversation_id) =
        title_ready_service("runtime-title-first-prompt");
    append_user_prompt(&transcript_store, &conversation_id, "first prompt fallback");
    assert!(
        transcript_store
            .session_objective_mirror(&conversation_id)
            .unwrap()
            .is_none(),
        "no objective is published for this conversation"
    );
    assert!(
        service
            .schedule_runtime_agent_session_title(&conversation_id, None)
            .is_ok(),
        "the first prompt is the only bounded input"
    );

    for _attempt in 0..2 {
        assert!(
            service
                .claim_agent_session_title_task(&conversation_id)
                .unwrap()
                .is_some()
        );
        let transition = service
            .apply_agent_session_title_transition(AgentSessionTitleEvent::Settled {
                conversation_id: conversation_id.clone(),
                outcome: AgentSessionTitleOutcome::Rejected("provider_error".to_string()),
            })
            .unwrap();
        assert!(transition.applied);
    }
    assert!(!service.agent_session_title_task_is_scheduled(&conversation_id));
    let browser = service.saved_sessions_record_browser().unwrap();
    let row = browser
        .records()
        .iter()
        .find(|record| record.id == conversation_id)
        .expect("degraded row should be rendered");
    assert_eq!(
        row.title,
        format!("{conversation_id} - first prompt fallback")
    );
}

/// Verifies a failed post-claim step strands neither a claim nor a concurrency slot.
#[test]
fn runtime_failed_post_claim_step_leaves_no_stranded_claim() {
    let (mut service, _store, primary, conversation_id) =
        title_ready_service("runtime-title-claim-rollback");
    service
        .execute_agent_shell_command(&primary, "inspect the backlog")
        .unwrap();
    let dispatch = service
        .claim_agent_session_title_task(&conversation_id)
        .unwrap()
        .expect("first claim");
    assert_eq!(
        service.claimed_agent_session_title_task_ids(),
        vec![conversation_id.clone()]
    );

    // Simulate the post-claim step that failed before the claim was usable.
    service.restore_pending_agent_session_title_task(dispatch.task.clone());
    assert!(service.claimed_agent_session_title_task_ids().is_empty());
    assert_eq!(
        service.pending_agent_session_title_tasks(),
        vec![conversation_id.clone()],
        "the task must stay claimable instead of being lost with the claim"
    );

    // The task is claimable again and settles normally, so the failed step left no
    // claim holding the concurrency slot.
    let reclaimed = service
        .claim_agent_session_title_task(&conversation_id)
        .unwrap()
        .expect("reclaim");
    assert_eq!(reclaimed.task.conversation_id, conversation_id);
    assert!(
        service
            .apply_agent_session_title_transition(AgentSessionTitleEvent::Settled {
                conversation_id: conversation_id.clone(),
                outcome: AgentSessionTitleOutcome::Generated("Inspect the backlog".to_string()),
            })
            .unwrap()
            .applied
    );
    assert!(service.claimed_agent_session_title_task_ids().is_empty());
    assert!(!service.agent_session_title_task_is_scheduled(&conversation_id));
}

/// Verifies an expired title claim is reaped, retried once, then degrades.
#[test]
fn runtime_expired_title_claim_is_reaped_and_settled() {
    let (mut service, transcript_store, primary, conversation_id) =
        title_ready_service("runtime-title-lease");
    service
        .execute_agent_shell_command(&primary, "inspect the backlog")
        .unwrap();
    append_user_prompt(&transcript_store, &conversation_id, "inspect the backlog");
    assert!(
        service
            .claim_agent_session_title_task(&conversation_id)
            .unwrap()
            .is_some()
    );
    // A live lease is never reaped.
    assert!(
        service
            .reap_expired_agent_session_title_claims(0)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        service.claimed_agent_session_title_task_ids(),
        vec![conversation_id.clone()]
    );

    // The lost worker's lease expires: one bounded retry is queued and dispatched.
    let side_effects = service
        .reap_expired_agent_session_title_claims(u64::MAX)
        .unwrap();
    assert!(service.claimed_agent_session_title_task_ids().is_empty());
    assert!(matches!(
        side_effects.as_slice(),
        [RuntimeSideEffect::DispatchAgentSessionTitle { conversation_id: id }]
            if id == &conversation_id
    ));
    assert_eq!(
        service.pending_agent_session_title_tasks(),
        vec![conversation_id.clone()]
    );
    assert!(
        transcript_store
            .session_generated_title(&conversation_id)
            .unwrap()
            .is_none()
    );

    // The retried attempt expires too, so the attempt budget is exhausted and the
    // conversation is retired instead of holding a slot forever.
    assert!(
        service
            .claim_agent_session_title_task(&conversation_id)
            .unwrap()
            .is_some()
    );
    let side_effects = service
        .reap_expired_agent_session_title_claims(i64::MAX as u64)
        .unwrap();
    assert!(side_effects.is_empty());
    assert!(service.pending_agent_session_title_tasks().is_empty());
    assert!(!service.agent_session_title_task_is_scheduled(&conversation_id));
    let trace = service.agent_pane_trace_log_text("%1").unwrap_or_default();
    assert!(trace.contains("claim lease expired"), "{trace}");
    assert!(trace.contains("attempts exhausted"), "{trace}");
}

/// Verifies the provider poll timer keeps ticking while a title claim is
/// outstanding, because that tick is what reaps an expired worker lease.
#[test]
fn runtime_provider_poll_timer_stays_armed_for_a_title_claim() {
    let (mut service, _store, _primary, conversation_id) =
        title_ready_service("runtime-title-poll-timer");
    assert!(
        service
            .provider_poll_timer_transition(false, 1, 100)
            .side_effects
            .is_empty(),
        "an idle runtime schedules no provider poll timer"
    );
    assert!(
        service
            .schedule_runtime_agent_session_title(&conversation_id, Some("inspect the backlog"))
            .is_ok()
    );
    assert!(matches!(
        service
            .provider_poll_timer_transition(false, 1, 100)
            .side_effects
            .as_slice(),
        [RuntimeSideEffect::ScheduleTimer { .. }]
    ));
    assert!(
        service
            .claim_agent_session_title_task(&conversation_id)
            .unwrap()
            .is_some()
    );
    assert!(
        matches!(
            service
                .provider_poll_timer_transition(false, 2, 100)
                .side_effects
                .as_slice(),
            [RuntimeSideEffect::ScheduleTimer { .. }]
        ),
        "a claim leaves the pending queue, so the timer must stay armed to reap it"
    );
}

/// Verifies a routine denial is traced only when its bounded reason changes.
#[test]
fn runtime_denial_trace_only_reports_a_new_reason() {
    let (mut service, _store, primary, first_conversation) =
        title_ready_service("runtime-title-denial-marker");
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .ok();
    let (second_pane, second_conversation) = split_title_conversation(&mut service, &primary);
    assert_ne!(second_pane, "%1");
    let (third_pane, third_conversation) = split_title_conversation(&mut service, &primary);
    assert!(
        service
            .schedule_runtime_agent_session_title(&first_conversation, Some("first objective"))
            .is_ok()
    );
    assert!(
        service
            .schedule_runtime_agent_session_title(&second_conversation, Some("second objective"))
            .is_ok()
    );

    // The saturated cap refuses the third conversation; the same reason is not
    // traced twice.
    assert_eq!(
        service.schedule_runtime_agent_session_title(&third_conversation, Some("third objective")),
        Err(SessionTitleDenial::ConcurrencyCap)
    );
    assert_eq!(title_skip_trace_count(&service, &third_pane), 1);
    assert_eq!(
        service.schedule_runtime_agent_session_title(&third_conversation, Some("third objective")),
        Err(SessionTitleDenial::ConcurrencyCap)
    );
    assert_eq!(title_skip_trace_count(&service, &third_pane), 1);

    // Settling one in-flight task releases capacity, so the next refusal carries a
    // different bounded reason and is traced once.
    assert!(
        service
            .claim_agent_session_title_task(&first_conversation)
            .unwrap()
            .is_some()
    );
    assert!(
        service
            .apply_agent_session_title_transition(AgentSessionTitleEvent::Settled {
                conversation_id: first_conversation.clone(),
                outcome: AgentSessionTitleOutcome::Generated("First title".to_string()),
            })
            .unwrap()
            .applied
    );
    assert_eq!(
        service.schedule_runtime_agent_session_title(&third_conversation, None),
        Err(SessionTitleDenial::NoInputs)
    );
    assert_eq!(title_skip_trace_count(&service, &third_pane), 2);
    assert_eq!(
        service.schedule_runtime_agent_session_title(&third_conversation, None),
        Err(SessionTitleDenial::NoInputs)
    );
    assert_eq!(title_skip_trace_count(&service, &third_pane), 2);
    let trace = service
        .agent_pane_trace_log_text(&third_pane)
        .unwrap_or_default();
    assert!(trace.contains("reason: concurrency cap"), "{trace}");
    assert!(trace.contains("reason: no inputs"), "{trace}");
}

/// Verifies the configured title model-profile override is used for the
/// dispatched title request while the conversation keeps its own profile.
#[test]
fn runtime_title_request_uses_the_configured_model_profile_override() {
    let (mut service, _store, primary, conversation_id) =
        title_ready_service("runtime-title-override");
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "title-override".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\n\
                   default_provider = \"local-chat\"\n\
                   default_model_profile = \"default\"\n\
                   session_title_model_profile = \"title\"\n\
                   \n\
                   [providers.local-chat]\n\
                   kind = \"openai-compatible\"\n\
                   base_url = \"http://127.0.0.1:9/v1\"\n\
                   models = [\"local-chat-model\", \"title-model\"]\n\
                   default_model = \"local-chat-model\"\n\
                   \n\
                   [model_profiles.default]\n\
                   provider = \"local-chat\"\n\
                   model = \"local-chat-model\"\n\
                   \n\
                   [model_profiles.title]\n\
                   provider = \"local-chat\"\n\
                   model = \"title-model\"\n"
                .to_string(),
        }])
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "inspect the backlog")
        .unwrap();

    let dispatch = service
        .claim_agent_session_title_task(&conversation_id)
        .unwrap()
        .expect("title claim");
    assert_eq!(dispatch.task.model_profile_name, "title");
    assert_eq!(dispatch.task.request.model, "title-model");
    assert_eq!(dispatch.task.request.provider, "local-chat");
}

/// Verifies a dispatched title request starts no turn and moves no cache lineage.
#[test]
fn runtime_dispatched_title_request_keeps_cache_lineage_immutable() {
    let (mut service, _store, primary, conversation_id) =
        title_ready_service("runtime-title-lineage");
    service
        .execute_agent_shell_command(&primary, "inspect the backlog")
        .unwrap();
    let lineage_before = service
        .agent_shell_store()
        .get("%1")
        .expect("active session")
        .prompt_cache_lineage_id
        .clone();
    let turns_before = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .map(|turn| turn.turn_id.clone())
        .collect::<Vec<_>>();

    let dispatch = service
        .claim_agent_session_title_task(&conversation_id)
        .unwrap()
        .expect("title claim");
    assert!(dispatch.task.request.turn_id.is_empty());
    assert!(dispatch.task.request.prompt_cache_lineage_id.is_none());
    assert!(dispatch.task.request.prompt_cache_session_id.is_none());
    assert!(dispatch.task.request.prompt_cache_retention.is_none());
    assert!(!dispatch.task.request.interaction_kind.expects_maap_batch());

    let lineage_after = service
        .agent_shell_store()
        .get("%1")
        .expect("active session")
        .prompt_cache_lineage_id
        .clone();
    assert_eq!(lineage_after, lineage_before);
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .map(|turn| turn.turn_id.clone())
            .collect::<Vec<_>>(),
        turns_before
    );
    assert!(
        service
            .agent_turn_contexts()
            .get(&crate::runtime::session_title_task_id(&conversation_id))
            .is_none(),
        "a title request must not register turn context"
    );
}
