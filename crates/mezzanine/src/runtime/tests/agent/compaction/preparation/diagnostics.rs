//! Manual compaction no-work explanation and narrow closed-prefix eligibility.
//!
//! Disposable durable rows exercise the real command selector without a model
//! call. No-work must preserve archive/epoch and queue no provider; fitting
//! eligible groups still queue the established model compactor. Exact/open
//! leading groups remain barriers, not selectively skipped summarization input.

use super::*;

/// Supplies one exact fixture row with stable conversation/group ownership.
fn row(sequence: u64, role: TranscriptRole, turn: &str, content: String) -> TranscriptEntry {
    TranscriptEntry {
        conversation_id: "diagnostic-compact".into(),
        sequence,
        created_at_unix_seconds: sequence,
        role,
        turn_id: turn.into(),
        agent_id: "agent-%1".into(),
        pane_id: "%1".into(),
        content,
    }
}

/// Shares model policy but uses only the specifically supplied source archive.
fn setup(
    label: &str,
    rows: &[TranscriptEntry],
    logical: u64,
) -> (RuntimeSessionService, ClientId, AgentTranscriptStore) {
    let (mut service, primary, _) = fixture_with_adapter(label, false);
    let store = AgentTranscriptStore::new(temp_root(&format!("source-{label}")));
    for entry in rows {
        store.append(entry).unwrap();
    }
    service.set_agent_transcript_store(store.clone());
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "diagnostic-compact", logical)
        .unwrap();
    (service, primary, store)
}

/// Forced manual selection already tries to expose a prefix even under budget.
/// Fitting exact/open history is therefore not a budget no-op: feedback must
/// explain that no eligible contiguous closed prefix exists. A leading barrier
/// also prevents later completed groups from being skipped into summary input.
#[test]
fn runtime_manual_compaction_no_work_reports_prefix_eligibility() {
    for later_complete in [false, true] {
        let mut rows = vec![row(
            1,
            TranscriptRole::User,
            "open",
            "retain exact unfinished instruction".into(),
        )];
        if later_complete {
            rows.push(row(
                2,
                TranscriptRole::Assistant,
                "later-complete",
                "completed later work".into(),
            ));
        }
        let (mut service, primary, store) = setup(
            &format!("no-prefix-{later_complete}"),
            &rows,
            rows.len() as u64,
        );
        let response = service
            .execute_agent_shell_command(&primary, "/compact")
            .unwrap();
        assert!(
            response.contains("reason=no-eligible-closed-prefix"),
            "{response}"
        );
        assert!(!response.contains("within-retained-context-tail"));
        assert!(!response.contains("state=queued"));
        assert!(response.contains("compacted=false"));
        assert!(service.pending_agent_compaction_task_ids().is_empty());
        assert!(!service.agent_is_compacting("%1"));
        let events = service
            .event_log()
            .unwrap()
            .replay_for(&crate::protocol::event::EventAudience::AllPrimaries);
        let event = events
            .iter()
            .find(|event| {
                serde_json::from_str::<serde_json::Value>(&event.payload)
                    .ok()
                    .is_some_and(|value| value["kind"] == "manual_compaction_no_work")
            })
            .unwrap();
        let diagnostic: serde_json::Value = serde_json::from_str(&event.payload).unwrap();
        assert_eq!(diagnostic["reason"], "no-eligible-closed-prefix");
        assert_eq!(diagnostic["compacted"], false);
        assert_eq!(diagnostic["provider_queued"], false);
        assert!(
            !event
                .payload
                .contains("retain exact unfinished instruction")
        );
        assert!(!event.payload.contains("completed later work"));
        assert_eq!(store.inspect("diagnostic-compact").unwrap(), rows);
        assert!(
            store
                .compaction_epoch("diagnostic-compact")
                .unwrap()
                .is_none()
        );
    }
}

/// Awaited command ingress must report the same typed skip and must not broaden
/// arbitrary system metadata or an MCP/open-user mixed group into model input.
#[tokio::test]
async fn runtime_manual_compaction_no_work_awaited_ingress_keeps_narrow_groups() {
    for rows in [
        vec![row(
            1,
            TranscriptRole::System,
            "unknown-system",
            "unqualified metadata remains exact".into(),
        )],
        vec![
            row(
                1,
                TranscriptRole::System,
                "mixed",
                mez_agent::TranscriptContextEvent::McpCompactionEpoch.to_transcript_content(),
            ),
            row(
                2,
                TranscriptRole::User,
                "mixed",
                "exact unfinished user goal".into(),
            ),
        ],
    ] {
        let (mut service, primary, store) = setup("awaited-no-prefix", &rows, rows.len() as u64);
        let response = service
            .execute_agent_shell_command_async(&primary, "/compact")
            .await
            .unwrap();
        assert!(
            response.contains("reason=no-eligible-closed-prefix"),
            "{response}"
        );
        assert!(service.pending_agent_compaction_task_ids().is_empty());
        assert!(!service.agent_is_compacting("%1"));
        assert_eq!(store.inspect("diagnostic-compact").unwrap(), rows);
        assert!(
            store
                .compaction_epoch("diagnostic-compact")
                .unwrap()
                .is_none()
        );
    }
}

/// Missing logical/durable sources and irreducible exact tails remain distinct
/// outcomes. Every skip preserves existing rows, leaves the durable replay epoch
/// absent and queues no model; a fitting single closed group still queues work.
#[test]
fn runtime_manual_compaction_no_work_source_and_exact_tail_matrix() {
    for (label, rows, logical, reason) in [
        ("logical-empty", Vec::new(), 0, "no-transcript-entries"),
        ("durable-empty", Vec::new(), 1, "no-durable-transcript"),
        (
            "irreducible",
            vec![row(
                1,
                TranscriptRole::User,
                "open",
                "protected exact ".repeat(14000),
            )],
            1,
            "irreducible-exact-retained-tail",
        ),
    ] {
        let (mut service, primary, store) = setup(label, &rows, logical);
        let response = service
            .execute_agent_shell_command(&primary, "/compact")
            .unwrap();
        assert!(response.contains(&format!("reason={reason}")), "{response}");
        assert!(response.contains("compacted=false"));
        assert!(!response.contains("state=queued"));
        assert!(!service.agent_is_compacting("%1"));
        assert!(service.pending_agent_compaction_task_ids().is_empty());
        assert!(
            store
                .compaction_epoch("diagnostic-compact")
                .unwrap()
                .is_none()
        );
        if !rows.is_empty() {
            assert_eq!(store.inspect("diagnostic-compact").unwrap(), rows);
        }
    }
    let rows = vec![row(
        1,
        TranscriptRole::Assistant,
        "one-closed",
        "eligible completed answer".into(),
    )];
    let (mut service, primary, store) = setup("single-closed", &rows, 1);
    let response = service
        .execute_agent_shell_command(&primary, "/compact")
        .unwrap();
    assert!(response.contains("state=queued"), "{response}");
    assert_eq!(
        service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap()
            .summarized_entries,
        1
    );
    assert!(
        store
            .compaction_epoch("diagnostic-compact")
            .unwrap()
            .is_none()
    );
}

/// Worker preparation may legitimately own a logical operation while discovering
/// no eligible model prefix. After that discovery it must retire visible work,
/// report the typed content-free reason, and never queue a provider or mutate
/// the persisted replay epoch. Logical admission fencing is not summary commit.
#[test]
fn runtime_manual_compaction_no_work_worker_retires_without_provider() {
    let rows = vec![row(
        1,
        TranscriptRole::User,
        "open",
        "exact retained source".into(),
    )];
    let (mut service, primary, store) = setup("worker-no-prefix", &rows, 1);
    service.use_manual_compaction_preparation_adapter();
    let response = service
        .execute_agent_shell_command(&primary, "/compact")
        .unwrap();
    assert!(response.contains("state=preparing"));
    let work = service.take_manual_compaction_preparations().pop().unwrap();
    let source = work.execute_source();
    assert!(
        service
            .complete_manual_compaction_preparation(&work, source)
            .unwrap()
    );
    assert!(!service.agent_is_compacting("%1"));
    assert!(service.take_manual_compaction_requests().is_empty());
    assert!(service.pending_agent_compaction_task_ids().is_empty());
    let events = service
        .event_log()
        .unwrap()
        .replay_for(&crate::protocol::event::EventAudience::AllPrimaries);
    assert!(events.iter().any(|event| {
        serde_json::from_str::<serde_json::Value>(&event.payload)
            .ok()
            .is_some_and(|value| {
                value["kind"] == "manual_compaction_no_work"
                    && value["reason"] == "no-eligible-closed-prefix"
                    && value["provider_queued"] == false
            })
    }));
    assert_eq!(store.inspect("diagnostic-compact").unwrap(), rows);
    assert!(
        store
            .compaction_epoch("diagnostic-compact")
            .unwrap()
            .is_none()
    );
}

/// Malformed durable source must stay an error rather than a no-source/eligible
/// skip diagnostic. No model, archive rewrite or replay boundary is fabricated.
#[test]
fn runtime_manual_compaction_no_work_does_not_mask_corrupt_source() {
    let rows = vec![row(
        1,
        TranscriptRole::Assistant,
        "closed",
        "complete source".into(),
    )];
    let (mut service, primary, store) = setup("corrupt-diagnostic", &rows, 1);
    let path = store.transcript_path("diagnostic-compact").unwrap();
    std::fs::write(&path, b"malformed transcript\n").unwrap();
    let response = service
        .execute_agent_shell_command(&primary, "/compact")
        .unwrap();
    assert!(response.contains("error"), "{response}");
    assert!(!response.contains("reason=no-durable-transcript"));
    assert!(service.pending_agent_compaction_task_ids().is_empty());
    assert_eq!(std::fs::read(path).unwrap(), b"malformed transcript\n");
    assert!(
        store
            .compaction_epoch("diagnostic-compact")
            .unwrap()
            .is_none()
    );
}

/// The final selector already permits one intact dedicated MCP epoch group.
/// Forced under-budget selection must use that same narrow predicate; otherwise
/// explicit compaction overlooks valid metadata merely for lacking an assistant.
#[test]
fn runtime_manual_compaction_forces_eligible_mcp_only_epoch_group() {
    let rows = vec![row(
        1,
        TranscriptRole::System,
        "mcp-epoch",
        mez_agent::TranscriptContextEvent::McpCompactionEpoch.to_transcript_content(),
    )];
    let (mut service, primary, store) = setup("mcp-only-prefix", &rows, 1);
    let response = service
        .execute_agent_shell_command(&primary, "/compact")
        .unwrap();
    assert!(response.contains("state=queued"), "{response}");
    let task = service.take_pending_agent_compaction_task("%1").unwrap();
    assert_eq!(task.summarized_entries, 1);
    assert_eq!(task.compacted_through_sequence, Some(1));
    assert_eq!(store.inspect("diagnostic-compact").unwrap(), rows);
    assert!(
        store
            .compaction_epoch("diagnostic-compact")
            .unwrap()
            .is_none()
    );
}
