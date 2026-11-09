//! Manual compaction no-work explanation and whole closed-unit eligibility.
//!
//! Disposable durable rows exercise the real command selector without a model
//! call. No-work must preserve archive/epoch and queue no provider; fitting
//! eligible groups still queue the established model compactor. Exact/open
//! groups remain intact islands outside selected summarization input.

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

/// Produces original instruction/task records and, when requested, an actual
/// fenced assistant completion. Role text alone supplies neither protection
/// nor closure. The runtime still reads and qualifies these original records.
fn authored_rows(prompt: Option<&str>, completed: bool) -> Vec<TranscriptEntry> {
    let mut owner = mez_agent::ReplayEpochContext::new(
        mez_agent::ReplayConversationId::new("diagnostic-compact").unwrap(),
        mez_agent::AgentContext::empty(),
    )
    .unwrap();
    if let Some(prompt) = prompt {
        owner
            .admit_instruction_task(
                vec![ContextBlock::user_event("user prompt", prompt)],
                &[],
                &[],
                0,
            )
            .unwrap();
    }
    let group = mez_agent::ContextExecutionGroupId::new("original-diagnostic-group").unwrap();
    if completed {
        let frontier = owner
            .append_execution_events([mez_agent::ContextConversationAppend::assistant(
                "original assistant",
                "completed later work",
                group.clone(),
            )])
            .unwrap()
            .pop()
            .unwrap();
        let receipt = owner.capture_execution_owner(&group, frontier).unwrap();
        owner.finish_execution_group(&receipt).unwrap();
    }
    let mut rows = owner
        .context()
        .chronology()
        .iter()
        .enumerate()
        .map(|(index, event)| {
            let block = event.block();
            let semantic =
                if let Some(start) = owner.task_admission_start(event.record().occurrence_id()) {
                    mez_agent::TranscriptContextEvent::task_admission(
                        event.sequence().get(),
                        start,
                        &block.label,
                        &block.content,
                    )
                    .unwrap()
                } else {
                    mez_agent::TranscriptContextEvent::execution_block_with_metadata(
                        block.source,
                        &block.label,
                        &block.content,
                        group.clone(),
                        1,
                        None,
                    )
                    .unwrap()
                };
            row(
                index as u64 + 1,
                TranscriptRole::System,
                "original-diagnostic",
                mez_agent::transcript::TranscriptContextRecord::new(semantic, event.record(), None)
                    .unwrap()
                    .to_transcript_content()
                    .unwrap(),
            )
        })
        .collect::<Vec<_>>();
    if completed {
        rows.push(row(
            rows.len() as u64 + 1,
            TranscriptRole::System,
            "original-diagnostic",
            mez_agent::transcript::TranscriptExecutionStateRecord::new(
                owner.execution_states().unwrap().pop().unwrap(),
            )
            .unwrap()
            .to_transcript_content()
            .unwrap(),
        ));
    }
    rows
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

/// Manual no-work requires genuinely protected-only source. A leading exact
/// instruction does not veto independent later completed execution groups.
#[test]
fn runtime_manual_compaction_no_work_reports_prefix_eligibility() {
    for later_complete in [false, true] {
        let rows = authored_rows(Some("retain exact unfinished instruction"), later_complete);
        let (mut service, primary, store) = setup(
            &format!("no-prefix-{later_complete}"),
            &rows,
            rows.len() as u64,
        );
        let response = service
            .execute_agent_shell_command(&primary, "/compact")
            .unwrap();
        if later_complete {
            assert!(response.contains("state=queued"), "{response}");
            let task = service
                .pending_agent_compaction_task_for_tests("%1")
                .unwrap();
            let source = crate::test_support::compaction_source::rendered_source(task).unwrap();
            assert!(source.contains("completed later work"));
            assert!(!source.contains("retain exact unfinished instruction"));
            assert_eq!(store.inspect("diagnostic-compact").unwrap(), rows);
            continue;
        }
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
        {
            let mut rows = vec![row(
                1,
                TranscriptRole::System,
                "mixed",
                mez_agent::TranscriptContextEvent::McpCompactionEpoch.to_transcript_content(),
            )];
            let mut task = authored_rows(Some("exact unfinished user goal"), false);
            for entry in &mut task {
                entry.sequence += 1;
            }
            rows.extend(task);
            rows
        },
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

/// Missing logical source, absent configured storage, missing required history,
/// and irreducible exact tails remain distinct outcomes. A required archive is
/// an integrity error rather than a no-work success. Every refusal preserves
/// existing rows and queues no model; a fitting closed group still queues work.
#[test]
fn runtime_manual_compaction_no_work_source_and_exact_tail_matrix() {
    for (label, rows, logical, reason) in [
        ("logical-empty", Vec::new(), 0, "no-transcript-entries"),
        (
            "durable-empty",
            Vec::new(),
            1,
            "required transcript archive is missing",
        ),
        (
            "irreducible",
            authored_rows(Some(&"protected exact ".repeat(14_000)), false),
            1,
            "irreducible-exact-retained-tail",
        ),
    ] {
        let (mut service, primary, store) = setup(label, &rows, logical);
        let response = service
            .execute_agent_shell_command(&primary, "/compact")
            .unwrap();
        if label == "durable-empty" {
            assert!(response.contains(reason), "{response}");
            assert!(response.contains("agent command error"), "{response}");
            assert!(!response.contains("reason=no-durable-transcript"));
        } else {
            assert!(response.contains(&format!("reason={reason}")), "{response}");
            assert!(response.contains("compacted=false"));
        }
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
    let mut no_store = test_runtime_service();
    assert!(no_store.persistence.transcript_store().is_none());
    no_store
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    no_store
        .agent_shell_store_mut()
        .bind_conversation("%1", "no-configured-store", 1)
        .unwrap();
    let no_store_primary = no_store
        .attach_primary("no-store", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let response = no_store
        .execute_agent_shell_command(&no_store_primary, "/compact")
        .unwrap();
    assert!(
        response.contains("reason=no-durable-transcript"),
        "{response}"
    );
    assert!(response.contains("compacted=false"));
    assert!(!no_store.agent_is_compacting("%1"));
    assert!(no_store.pending_agent_compaction_task_ids().is_empty());
    let rows = authored_rows(None, true);
    let (mut service, primary, store) = setup("single-closed", &rows, rows.len() as u64);
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
    let rows = authored_rows(Some("exact retained source"), false);
    let (mut service, primary, store) = setup("worker-no-prefix", &rows, 1);
    service.use_agent_source_preparation_adapter();
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

/// Retains the historical regression name while checking the canonical policy:
/// an MCP boundary has no model source, and a catalog remains exact. Neither
/// may fabricate an eligible model group or a summary merely to force progress.
#[test]
fn runtime_manual_compaction_forces_eligible_mcp_only_epoch_group() {
    for mode in ["boundary", "catalog"] {
        let mut rows = Vec::new();
        if mode == "catalog" {
            rows.push(row(
                1,
                TranscriptRole::System,
                "mcp-epoch",
                mez_agent::TranscriptContextEvent::mcp_catalog_snapshot(
                    "original catalog content",
                    1024,
                )
                .unwrap()
                .to_transcript_content(),
            ));
        }
        if mode == "boundary" {
            // An old reset marker is a zero-model boundary, not a replacement
            // for the current catalog or the original task's retired prelude.
            rows.push(row(
                1,
                TranscriptRole::System,
                "mcp-epoch",
                mez_agent::TranscriptContextEvent::McpCompactionEpoch.to_transcript_content(),
            ));
        }
        let (mut service, primary, store) = setup("mcp-only-prefix", &rows, rows.len() as u64);
        let response = service
            .execute_agent_shell_command(&primary, "/compact")
            .unwrap();
        assert!(
            response.contains("reason=no-eligible-closed-prefix"),
            "{response}"
        );
        assert!(service.pending_agent_compaction_task_ids().is_empty());
        assert!(
            store
                .compaction_epoch("diagnostic-compact")
                .unwrap()
                .is_none()
        );
        assert!(!service.agent_is_compacting("%1"));
        assert_eq!(store.inspect("diagnostic-compact").unwrap(), rows);
    }
}
