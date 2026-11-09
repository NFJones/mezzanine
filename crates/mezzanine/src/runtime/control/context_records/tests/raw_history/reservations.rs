//! Historical synthetic slots must fit around original anchors before ownership.

use super::*;

/// Actual original observations survive all source-custody stages without
/// blocking input or acquiring an execution owner during initial bootstrap.
#[test]
fn raw_history_observation_survives_pending_settled_and_cold_reads() {
    let directory = Directory::new();
    let store = AgentTranscriptStore::new(&directory.0);
    let (_, original) = raw_tasks();
    let mut rows = original.entries().to_vec();
    let group = mez_agent::ContextExecutionGroupId::new("runtime-observation").unwrap();
    let block =
        ContextBlock::evidence_event(ContextSourceKind::CommittedEvidence, "counters", "one call");
    let record = ContextBlockRecord::allocate(&block).unwrap();
    rows.push(TranscriptEntry {
        sequence: 5,
        content: TranscriptContextRecord::new(
            TranscriptContextEvent::execution_block_with_metadata(
                block.source,
                &block.label,
                &block.content,
                group.clone(),
                1,
                None,
            )
            .unwrap(),
            record,
            None,
        )
        .unwrap()
        .to_transcript_content()
        .unwrap(),
        ..rows[3].clone()
    });
    let batch = crate::storage::transcript::TranscriptAppendBatch::owned_rows(rows).unwrap();
    let mut captured = work(store.clone());
    captured.inputs.active_entries = Some(batch.entries().len());
    captured.inputs.pending_entries =
        crate::runtime::persistence::RuntimePendingTranscriptContext::from_batch(&batch).unwrap();
    for stage in 0..3 {
        if stage == 1 {
            store.commit_accounted_transcript_batch(&batch, 1).unwrap();
        } else if stage == 2 {
            captured = work(AgentTranscriptStore::new(&directory.0));
        }
        let history = execute_runtime_agent_history_epoch_work(captured.clone()).unwrap();
        let mut owner = history.replay_owner().unwrap().unwrap();
        let event = &owner.context().chronology()[4];
        assert_eq!(event.record(), record);
        assert_eq!(event.block(), &block);
        assert!(!owner.context().execution_group_is_pending(&group));
        assert!(
            owner
                .capture_execution_owner(&group, event.sequence())
                .is_err()
        );
        assert!(owner.execution_states().unwrap().is_empty());
        let pending = mez_agent::ContextExecutionGroupId::new("original-pending-group").unwrap();
        assert!(owner.context().execution_group_is_pending(&pending));
        owner
            .admit_instruction_task(
                vec![ContextBlock::user_event("user prompt", "continue")],
                &[],
                &[],
                0,
            )
            .unwrap();
        assert!(owner.context().execution_group_is_pending(&pending));
        if stage == 0 {
            assert!(!store.transcript_path(CID).unwrap().exists());
        } else {
            assert_eq!(store.inspect(CID).unwrap(), batch.entries());
        }
    }
}

/// Wire execution blocks have no sequence field. A separately qualified
/// lifecycle control must anchor them before interpolation across a larger gap.
#[test]
fn raw_history_sparse_control_members_keep_original_sequences() {
    for closed in [false, true] {
        let directory = Directory::new();
        let store = AgentTranscriptStore::new(&directory.0);
        let (mut original, batch) = raw_tasks();
        let group = mez_agent::ContextExecutionGroupId::new("original-pending-group").unwrap();
        if closed {
            let frontier = original.context().chronology()[1].sequence();
            let handle = original.capture_execution_owner(&group, frontier).unwrap();
            original.finish_execution_group(&handle).unwrap();
        }
        let mut rows = batch.entries().to_vec();
        // A source anchor beyond the group's original 2048 leaves room for
        // synthetic history, but cannot move the control-backed group to 2560.
        let record = TranscriptContextRecord::from_transcript_content(&rows[2].content)
            .unwrap()
            .unwrap();
        let TranscriptContextEvent::TaskAdmission {
            task_start,
            label,
            content,
            ..
        } = record.event()
        else {
            unreachable!()
        };
        rows[2].content = TranscriptContextRecord::new(
            TranscriptContextEvent::task_admission(4096, *task_start, label, content).unwrap(),
            record.record(),
            None,
        )
        .unwrap()
        .to_transcript_content()
        .unwrap();
        let record = TranscriptContextRecord::from_transcript_content(&rows[3].content)
            .unwrap()
            .unwrap();
        rows[3].content = TranscriptContextRecord::new(
            TranscriptContextEvent::user_event(
                5120,
                "user steering",
                "retain this exact follow-up",
            )
            .unwrap(),
            record.record(),
            None,
        )
        .unwrap()
        .to_transcript_content()
        .unwrap();
        rows.push(execution_control(&original));
        let batch = crate::storage::transcript::TranscriptAppendBatch::owned_rows(rows).unwrap();
        store.commit_accounted_transcript_batch(&batch, 1).unwrap();
        let history = execute_runtime_agent_history_epoch_work(work(store.clone())).unwrap();
        let owner = history.replay_owner().unwrap().unwrap();
        assert_eq!(owner.context().chronology()[1].sequence().get(), 2048);
        assert_eq!(owner.context().execution_group_is_pending(&group), !closed);
        assert_eq!(store.inspect(CID).unwrap(), batch.entries());
    }
}

/// Five unnumbered rows fit before 3072, but allocating a live 1024 stride for
/// each used to manufacture a rewind at the sixth row (next=6144).
#[test]
fn raw_history_sparse_reservation_survives_pending_settlement_and_cold_read() {
    let directory = Directory::new();
    let store = AgentTranscriptStore::new(&directory.0);
    for sequence in 1..=5 {
        store
            .append(&TranscriptEntry {
                conversation_id: CID.into(),
                sequence,
                created_at_unix_seconds: 1,
                role: TranscriptRole::User,
                turn_id: "legacy-turn".into(),
                agent_id: "original-agent".into(),
                pane_id: "original-pane".into(),
                content: format!("legacy input {sequence}"),
            })
            .unwrap();
    }
    let block = ContextBlock::user_event("later input", "accounted source");
    let record = ContextBlockRecord::allocate(&block).unwrap();
    let event = TranscriptContextEvent::user_event(3072, &block.label, &block.content).unwrap();
    let batch =
        crate::storage::transcript::TranscriptAppendBatch::owned_rows(vec![TranscriptEntry {
            conversation_id: CID.into(),
            sequence: 6,
            created_at_unix_seconds: 2,
            role: TranscriptRole::System,
            turn_id: "accounted-turn".into(),
            agent_id: "original-agent".into(),
            pane_id: "original-pane".into(),
            content: TranscriptContextRecord::new(event, record, None)
                .unwrap()
                .to_transcript_content()
                .unwrap(),
        }])
        .unwrap();
    let mut captured = work(store.clone());
    captured.inputs.pending_entries =
        crate::runtime::persistence::RuntimePendingTranscriptContext::from_batch(&batch).unwrap();
    let mut expected = None;
    for stage in 0..3 {
        if stage == 1 {
            store.commit_accounted_transcript_batch(&batch, 1).unwrap();
        } else if stage == 2 {
            captured = work(AgentTranscriptStore::new(&directory.0));
        }
        let history = execute_runtime_agent_history_epoch_work(captured.clone()).unwrap();
        let mut owner = history.replay_owner().unwrap().unwrap();
        let events = owner.context().chronology();
        assert_eq!(events.len(), 6);
        assert_eq!(events[5].record(), record);
        assert_eq!(events[5].sequence().get(), 3072);
        assert_eq!(
            events
                .iter()
                .map(|event| event.sequence().get())
                .collect::<Vec<_>>(),
            [512, 1024, 1536, 2048, 2560, 3072]
        );
        if let Some(expected) = &expected {
            assert_eq!(events, expected);
        } else {
            expected = Some(events.to_vec());
        }
        owner
            .admit_instruction_task(
                vec![ContextBlock::user_event(
                    "user prompt",
                    "new input still works",
                )],
                &[],
                &[],
                0,
            )
            .unwrap();
        assert_eq!(owner.context().chronology()[6].sequence().get(), 4096);
        assert_eq!(
            store.inspect(CID).unwrap().len(),
            if stage == 0 { 5 } else { 6 }
        );
    }
}
