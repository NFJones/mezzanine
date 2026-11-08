//! Native content edits preserve full original pairs and descriptor custody;
//! an older closed prefix cannot release later unfinished native source.

use super::raw_edits::{CID, archive_retained_source, load, saved_raw};
use super::*;
use crate::storage::transcript::{
    CompareAndSwapTranscriptEntryResult as Cas, TranscriptAppendBatch,
};
use mez_agent::transcript::{
    TranscriptContextEvent as Event, TranscriptContextRecord as Record,
    TranscriptExecutionStateRecord,
};
use mez_agent::{
    ContextConversationAppend, ContextExecutionGroupId, ProviderApiCompatibility,
    ProviderContinuityOwner, ProviderTranscriptEvent, TranscriptEntry,
};

fn provider() -> ProviderContinuityOwner {
    ProviderContinuityOwner::new(
        ProviderApiCompatibility::OpenAiChatCompletions,
        "offline-provider",
    )
    .unwrap()
}

fn call(id: &str) -> String {
    ProviderTranscriptEvent::validated_openai_chat_completions_assistant_tool_call(
        "offline-provider".into(), "original native declaration".into(),
        vec![serde_json::json!({"id":id,"type":"function","function":{"name":mez_agent::MAAP_ACTION_BATCH_TOOL_NAME,"arguments":"{}"}})],
    ).unwrap().to_transcript_content()
}

fn result(id: &str, content: &str) -> String {
    ProviderTranscriptEvent::OpenAiChatCompletionsToolResult {
        provider_id: "offline-provider".into(),
        tool_call_id: id.into(),
        content: content.into(),
    }
    .to_transcript_content()
}

fn append_native(owner: &mut ReplayEpochContext, pending: bool) -> ContextExecutionGroupId {
    let group = ContextExecutionGroupId::new("native-edit-group").unwrap();
    owner
        .append_execution_events([
            ContextConversationAppend::provider_assistant(
                "call",
                call("original-call"),
                group.clone(),
                provider(),
            ),
            ContextConversationAppend::evidence(
                ContextSourceKind::TranscriptTool,
                "result",
                result("original-call", "original native prose"),
                group.clone(),
                Some(provider()),
                true,
            ),
        ])
        .unwrap();
    if !pending {
        let frontier = owner.context().chronology().last().unwrap().sequence();
        let capture = owner.capture_execution_owner(&group, frontier).unwrap();
        owner.finish_execution_group(&capture).unwrap();
    }
    group
}

fn revised(row: &TranscriptEntry, id: &str, content: &str) -> String {
    let record = Record::from_transcript_content(&row.content)
        .unwrap()
        .unwrap();
    let Event::ExecutionBlock {
        source,
        label,
        execution_group_id: Some(group),
        ordinal: Some(ordinal),
        provider_owner,
        ..
    } = record.event()
    else {
        panic!("expected native result");
    };
    let event = Event::execution_block_with_metadata(
        *source,
        label.clone(),
        result(id, content),
        group.clone(),
        *ordinal,
        provider_owner.clone(),
    )
    .unwrap();
    Record::new(
        event,
        record.record(),
        record.stable_records().map(<[_]>::to_vec),
    )
    .unwrap()
    .to_transcript_content()
    .unwrap()
}

/// SQL version-five descriptors revise only the selected full image. Recovery
/// retries that exact projection; closed source can edit prose but not call IDs,
/// while genuinely pending source remains protected before any raw replacement.
#[test]
fn raw_edit_native_member_preserves_pairs_and_descriptor_through_recovery() {
    for pending in [false, true] {
        let fixture = Fixture::new();
        let mut owner = source();
        let frozen = plan(&owner);
        let group = append_native(&mut owner, pending);
        archive_retained_source(&fixture, &owner, frozen);
        let rows = fixture.store.inspect(CID).unwrap();
        let row = rows
            .iter()
            .find(|row| {
                Record::from_transcript_content(&row.content)
                    .unwrap()
                    .is_some_and(|record| {
                        matches!(
                            record.event(),
                            Event::ExecutionBlock {
                                source: ContextSourceKind::TranscriptTool,
                                ..
                            }
                        )
                    })
            })
            .unwrap();
        let old = load(&fixture);
        if pending {
            assert!(
                fixture
                    .store
                    .compare_and_swap_entry_content(
                        CID,
                        row.sequence,
                        &row.pane_id,
                        &fixture.store.transcript_entry_revision(row).unwrap(),
                        revised(row, "original-call", "revised prose")
                    )
                    .unwrap_err()
                    .to_string()
                    .contains("pending")
            );
            assert_eq!(fixture.store.inspect(CID).unwrap(), rows);
            assert_eq!(load(&fixture).0, old.0);
            continue;
        }
        // A present malformed execution descriptor must fail BEFORE raw/index
        // replacement, not leave a recovery intent stuck after changing bytes.
        let id = Record::from_transcript_content(&row.content)
            .unwrap()
            .unwrap()
            .record()
            .occurrence_id();
        let original_digest = {
            let _lease = fixture.store.acquire_conversation_lock(CID).unwrap();
            let connection = fixture.store.open_row_origins_locked(CID).unwrap();
            let digest: String = connection
                .query_row(
                    "SELECT sha256 FROM execution_sources WHERE origin=?1",
                    [String::from(id)],
                    |row| row.get(0),
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE execution_sources SET sha256=?1 WHERE origin=?2",
                    rusqlite::params!["0".repeat(64), String::from(id)],
                )
                .unwrap();
            digest
        };
        let raw_path = fixture.store.existing_transcript_path_for(CID).unwrap();
        let original_bytes = fs::read(&raw_path).unwrap();
        let manifest = fs::read(fixture.root.join(CID).join(MANIFEST_NAME)).unwrap();
        let error = fixture
            .store
            .compare_and_swap_entry_content(
                CID,
                row.sequence,
                &row.pane_id,
                &fixture.store.transcript_entry_revision(row).unwrap(),
                revised(row, "original-call", "revised prose"),
            )
            .unwrap_err();
        assert!(error.to_string().contains("descriptor"), "{error}");
        assert_eq!(fs::read(&raw_path).unwrap(), original_bytes);
        assert_eq!(
            fs::read(fixture.root.join(CID).join(MANIFEST_NAME)).unwrap(),
            manifest
        );
        assert!(
            !fixture
                .root
                .join(CID)
                .join(".raw-content-edit.json")
                .exists()
        );
        {
            let _lease = fixture.store.acquire_conversation_lock(CID).unwrap();
            let connection = fixture.store.open_row_origins_locked(CID).unwrap();
            connection
                .execute(
                    "UPDATE execution_sources SET sha256=?1 WHERE origin=?2",
                    rusqlite::params![original_digest, String::from(id)],
                )
                .unwrap();
        }
        assert!(
            fixture
                .store
                .compare_and_swap_entry_content(
                    CID,
                    row.sequence,
                    &row.pane_id,
                    &fixture.store.transcript_entry_revision(row).unwrap(),
                    revised(row, "foreign-call", "revised prose")
                )
                .is_err()
        );
        fixture
            .store
            .inject_replay_epoch_fault(Phase::AfterRawEditIndex, 1, false);
        assert!(
            fixture
                .store
                .compare_and_swap_entry_content(
                    CID,
                    row.sequence,
                    &row.pane_id,
                    &fixture.store.transcript_entry_revision(row).unwrap(),
                    revised(row, "original-call", "recovered native prose")
                )
                .is_err()
        );
        let current = load(&fixture);
        assert_eq!(current.1.head(), old.1.head());
        assert!(!current.1.context().execution_group_is_pending(&group));
        let edited = fixture
            .store
            .inspect(CID)
            .unwrap()
            .into_iter()
            .find(|entry| entry.sequence == row.sequence)
            .unwrap();
        assert!(edited.content.contains("recovered native prose"));
        let _lease = fixture.store.acquire_conversation_lock(CID).unwrap();
        let image = fixture
            .store
            .indexed_row_reference_locked(CID, row.sequence)
            .unwrap()
            .unwrap();
        assert_eq!(
            image.digest,
            fixture.store.transcript_entry_revision(&edited).unwrap()
        );
        assert_eq!(
            image.origin,
            Record::from_transcript_content(&row.content)
                .unwrap()
                .unwrap()
                .record()
                .occurrence_id()
        );
    }
}

/// Uses genuine later native rows, a closed source control, and optionally a
/// subsequent real append that renews the same group without settling it. The
/// older control proves only its original prefix, not the later unfinished call.
#[test]
fn raw_edit_native_arrival_requires_the_complete_current_closed_frontier() {
    for renewed in [false, true] {
        let (fixture, template) = saved_raw();
        let mut owner = load(&fixture).1;
        let group = append_native(&mut owner, false);
        let mut rows = Vec::new();
        for (index, event) in owner
            .context()
            .chronology()
            .iter()
            .filter(|event| event.execution_group_id() == Some(&group))
            .enumerate()
        {
            let semantic = Event::execution_block_with_metadata(
                event.block().source,
                event.block().label.clone(),
                event.block().content.clone(),
                group.clone(),
                index as u64 + 1,
                event.provider_owner().cloned(),
            )
            .unwrap();
            let mut row = template.clone();
            row.sequence = 4 + index as u64;
            row.content = Record::new(semantic, event.record(), None)
                .unwrap()
                .to_transcript_content()
                .unwrap();
            rows.push(row);
        }
        let state = owner
            .execution_states()
            .unwrap()
            .into_iter()
            .find(|state| state.group() == group.as_str())
            .unwrap();
        let mut control = template.clone();
        control.sequence = 6;
        control.content = TranscriptExecutionStateRecord::new(state)
            .unwrap()
            .to_transcript_content()
            .unwrap();
        rows.push(control);
        if renewed {
            owner
                .append_execution_events([ContextConversationAppend::provider_assistant(
                    "renewed",
                    call("later-pending-call"),
                    group.clone(),
                    provider(),
                )])
                .unwrap();
            let event = owner.context().chronology().last().unwrap();
            let semantic = Event::execution_block_with_metadata(
                event.block().source,
                event.block().label.clone(),
                event.block().content.clone(),
                group.clone(),
                3,
                event.provider_owner().cloned(),
            )
            .unwrap();
            let mut row = template.clone();
            row.sequence = 7;
            row.content = Record::new(semantic, event.record(), None)
                .unwrap()
                .to_transcript_content()
                .unwrap();
            rows.push(row);
        }
        let batch = TranscriptAppendBatch::owned_rows(rows.clone()).unwrap();
        fixture
            .store
            .accept_transcript_append_receipt(&batch, 1)
            .unwrap();
        fixture
            .store
            .commit_accounted_transcript_batch(&batch, 1)
            .unwrap();
        let target = &rows[1];
        let outcome = fixture.store.compare_and_swap_entry_content(
            CID,
            target.sequence,
            &target.pane_id,
            &fixture.store.transcript_entry_revision(target).unwrap(),
            revised(target, "original-call", "edited native arrival"),
        );
        if renewed {
            assert!(
                outcome.is_err(),
                "older closed control admitted an edit into renewed pending source: {outcome:?}"
            );
        } else {
            assert!(matches!(outcome.unwrap(), Cas::Updated(_)));
            let snapshot = fixture
                .store
                .conversation_replay_snapshot(CID, &[], |_| {
                    Some((
                        crate::storage::transcript::ConversationTranscriptRead::All,
                        true,
                    ))
                })
                .unwrap();
            assert!(snapshot.replay.unwrap().row_is_authored_arrival(5));
        }
    }
}
