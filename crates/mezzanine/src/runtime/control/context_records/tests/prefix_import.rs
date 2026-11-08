//! Migration CAS rejects stale or forged projections from the real renderer.

use super::*;
use crate::runtime::control::context::{
    RuntimeAgentHistoryEpochInputs, RuntimeAgentHistoryEpochWork,
    execute_runtime_agent_history_epoch_with_projection,
};
use crate::storage::transcript::AgentCompactionEpoch;
use mez_agent::{ReplayConversationId, ReplayEpochContext};

const CID: &str = "prefix-source";

/// Historical assistant storage changed independently of lifecycle metadata.
enum TailFormat {
    Plain,
    LegacyEvent,
    AccountedEvent,
    GroupedEvent,
    QuotedEvent,
}

/// The production preview supplies actual row identities/costs without
/// normalizing. It preserves a frozen source for subsequent CAS rejection.
struct Projection {
    directory: Directory,
    store: AgentTranscriptStore,
    epoch: AgentCompactionEpoch,
    owner: ReplayEpochContext,
    captured: Vec<TranscriptEntry>,
}

impl Projection {
    fn new() -> Self {
        Self::with_tail(TailFormat::Plain)
    }

    fn with_tail(format: TailFormat) -> Self {
        let directory = Directory::new();
        let store = AgentTranscriptStore::new(&directory.0);
        let mut rows = (1..=3)
            .map(|sequence| TranscriptEntry {
                conversation_id: CID.into(),
                sequence,
                created_at_unix_seconds: sequence,
                role: if sequence == 2 {
                    TranscriptRole::User
                } else {
                    TranscriptRole::Assistant
                },
                turn_id: format!("original-turn-{sequence}"),
                agent_id: "agent-%1".into(),
                pane_id: "%1".into(),
                content: format!("original source {sequence}"),
            })
            .collect::<Vec<_>>();
        if !matches!(format, TailFormat::Plain) {
            let block =
                ContextBlock::assistant_event("original assistant", rows[2].content.clone());
            let event = mez_agent::TranscriptContextEvent::execution_block(
                block.source,
                block.label.clone(),
                block.content.clone(),
            )
            .unwrap();
            rows[2].role = TranscriptRole::System;
            if matches!(format, TailFormat::QuotedEvent) {
                rows[2].role = TranscriptRole::Assistant;
            }
            rows[2].content = match format {
                TailFormat::LegacyEvent => event.to_transcript_content(),
                TailFormat::GroupedEvent => {
                    let event = mez_agent::TranscriptContextEvent::execution_block_with_metadata(
                        block.source,
                        &block.label,
                        &block.content,
                        mez_agent::ContextExecutionGroupId::new("original-explicit-group").unwrap(),
                        1,
                        None,
                    )
                    .unwrap();
                    mez_agent::transcript::TranscriptContextRecord::new(
                        event,
                        mez_agent::ContextBlockRecord::allocate(&block).unwrap(),
                        None,
                    )
                    .unwrap()
                    .to_transcript_content()
                    .unwrap()
                }
                TailFormat::QuotedEvent => {
                    mez_agent::TranscriptContextEvent::execution_block_with_metadata(
                        block.source,
                        block.label.clone(),
                        block.content.clone(),
                        mez_agent::ContextExecutionGroupId::new("quoted-unrelated-group").unwrap(),
                        37,
                        None,
                    )
                    .unwrap()
                    .to_transcript_content()
                }
                TailFormat::AccountedEvent => mez_agent::transcript::TranscriptContextRecord::new(
                    event,
                    mez_agent::ContextBlockRecord::allocate(&block).unwrap(),
                    None,
                )
                .unwrap()
                .to_transcript_content()
                .unwrap(),
                TailFormat::Plain => unreachable!("plain rows do not enter envelope conversion"),
            };
        }
        store.append_many(&rows).unwrap();
        store
            .save_compaction_epoch(CID, 1, "original head")
            .unwrap();
        let epoch = store.compaction_epoch(CID).unwrap().unwrap();
        let work = RuntimeAgentHistoryEpochWork {
            store: store.clone(),
            pending_capture_error: None,
            inputs: RuntimeAgentHistoryEpochInputs {
                pane_id: "%1".into(),
                conversation_id: CID.into(),
                ephemeral_source_entries: None,
                active_entries: None,
                pending_entries: Default::default(),
            },
        };
        let history =
            execute_runtime_agent_history_epoch_with_projection(work, Some(epoch.clone())).unwrap();
        let owner = history
            .records
            .import_legacy_prefix(
                CID,
                history.blocks,
                &history.execution_events,
                epoch.summary_records.as_ref().unwrap().prefix.unwrap(),
            )
            .unwrap();
        Self {
            directory,
            store,
            epoch,
            owner,
            captured: rows[1..].to_vec(),
        }
    }

    fn files(&self) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
        fn visit(path: &std::path::Path, out: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>) {
            for child in std::fs::read_dir(path).unwrap() {
                let path = child.unwrap().path();
                if path.is_dir() {
                    visit(&path, out);
                } else {
                    out.insert(path.clone(), std::fs::read(path).unwrap());
                }
            }
        }
        let mut files = std::collections::BTreeMap::new();
        visit(&self.directory.0.join(CID), &mut files);
        files
    }
}

/// Actual head replacement and content-CAS edits invalidate the captured
/// proposal before source-image capture or authoritative sidecar replacement.
#[test]
fn legacy_prefix_normalization_rejects_stale_authority_and_raw_source() {
    for change_head in [false, true] {
        let fixture = Projection::new();
        if change_head {
            fixture
                .store
                .save_compaction_epoch(CID, 1, "later independently published head")
                .unwrap();
        } else {
            fixture
                .store
                .compare_and_swap_entry_content(
                    CID,
                    2,
                    "%1",
                    &fixture
                        .store
                        .transcript_entry_revision(&fixture.captured[0])
                        .unwrap(),
                    "changed original raw instruction".into(),
                )
                .unwrap();
        }
        let before = fixture.files();
        let error = fixture
            .store
            .normalize_legacy_prefix(
                CID,
                &fixture.epoch,
                &fixture.owner,
                &fixture.captured,
                RuntimeContextRecords::import_legacy_prefix_backing,
            )
            .unwrap_err();
        assert_eq!(error.kind(), crate::MezErrorKind::Conflict);
        assert_eq!(fixture.files(), before);
    }
}

/// A retained occurrence ID cannot authorize different projected bytes. Even a
/// valid cold candidate must match the original committed projection record.
#[test]
fn legacy_prefix_normalization_rejects_changed_projection_record() {
    for format in [
        TailFormat::Plain,
        TailFormat::LegacyEvent,
        TailFormat::AccountedEvent,
    ] {
        let fixture = Projection::with_tail(format);
        let path = fixture.directory.0.join(CID).join("compaction-epoch.json");
        let archive = fixture.store.inspect(CID).unwrap();
        let before = std::fs::read(&path).unwrap();
        let mut altered = serde_json::to_value(fixture.owner.checkpoint().unwrap()).unwrap();
        altered["events"][2]["block"]["content"] = serde_json::json!("forged projected raw bytes");
        let altered = ReplayEpochContext::from_checkpoint(
            ReplayConversationId::new(CID).unwrap(),
            serde_json::from_value(altered).unwrap(),
            AgentContext::empty(),
        )
        .unwrap();
        let error = fixture
            .store
            .normalize_legacy_prefix(
                CID,
                &fixture.epoch,
                &altered,
                &fixture.captured,
                RuntimeContextRecords::import_legacy_prefix_backing,
            )
            .unwrap_err();
        assert!(error.message().contains("original row record"), "{error}");
        assert_eq!(std::fs::read(path).unwrap(), before);
        assert_eq!(fixture.store.inspect(CID).unwrap(), archive);
        fixture
            .store
            .normalize_legacy_prefix(
                CID,
                &fixture.epoch,
                &fixture.owner,
                &fixture.captured,
                RuntimeContextRecords::import_legacy_prefix_backing,
            )
            .unwrap();
    }
}

/// A committed migration acknowledges only the original frozen proposal. The
/// old head alone cannot authorize a different checkpoint, cut or row image.
#[test]
fn legacy_prefix_normalization_retry_requires_exact_frozen_proposal() {
    let fixture = Projection::new();
    fixture
        .store
        .normalize_legacy_prefix(
            CID,
            &fixture.epoch,
            &fixture.owner,
            &fixture.captured,
            RuntimeContextRecords::import_legacy_prefix_backing,
        )
        .unwrap();
    assert!(fixture.owner.execution_states().unwrap().is_empty());
    let before = fixture.files();
    let mut altered = serde_json::to_value(fixture.owner.checkpoint().unwrap()).unwrap();
    altered["events"][1]["block"]["content"] = serde_json::json!("different retry bytes");
    let altered = ReplayEpochContext::from_checkpoint(
        ReplayConversationId::new(CID).unwrap(),
        serde_json::from_value(altered).unwrap(),
        AgentContext::empty(),
    )
    .unwrap();
    let mut changed_image = fixture.captured.clone();
    changed_image[0].created_at_unix_seconds += 1;
    let mut foreign = fixture.captured.clone();
    foreign[0].conversation_id = "different-conversation".into();
    for (owner, captured) in [
        (&altered, fixture.captured.as_slice()),
        (&fixture.owner, &fixture.captured[..1]),
        (&fixture.owner, changed_image.as_slice()),
        (&fixture.owner, foreign.as_slice()),
    ] {
        let error = fixture
            .store
            .normalize_legacy_prefix(
                CID,
                &fixture.epoch,
                owner,
                captured,
                RuntimeContextRecords::import_legacy_prefix_backing,
            )
            .unwrap_err();
        assert_eq!(error.kind(), crate::MezErrorKind::Conflict);
        assert_eq!(fixture.files(), before);
    }
    fixture
        .store
        .normalize_legacy_prefix(
            CID,
            &fixture.epoch,
            &fixture.owner,
            &fixture.captured,
            RuntimeContextRecords::import_legacy_prefix_backing,
        )
        .unwrap();
    assert_eq!(fixture.files(), before);
}

/// An old execution envelope may carry cached occurrence/cost data without
/// carrying producer lifecycle metadata. Preserve the existing historical
/// grouping, but never promote that envelope into a known execution interval.
#[test]
fn legacy_prefix_normalization_preserves_ungrouped_execution_envelopes() {
    for format in [
        TailFormat::LegacyEvent,
        TailFormat::AccountedEvent,
        TailFormat::QuotedEvent,
    ] {
        let fixture = Projection::with_tail(format);
        let archive = fixture.store.inspect(CID).unwrap();
        fixture
            .store
            .normalize_legacy_prefix(
                CID,
                &fixture.epoch,
                &fixture.owner,
                &fixture.captured,
                RuntimeContextRecords::import_legacy_prefix_backing,
            )
            .unwrap();
        let (_, cold) = AgentTranscriptStore::new(&fixture.directory.0)
            .load_replay_epoch(CID, AgentContext::empty())
            .unwrap()
            .unwrap();
        assert_eq!(
            cold.context().chronology(),
            fixture.owner.context().chronology()
        );
        assert!(cold.execution_states().unwrap().is_empty());
        let group = cold
            .context()
            .chronology()
            .last()
            .unwrap()
            .execution_group_id()
            .unwrap();
        assert!(cold.context().execution_group_is_pending(group));
        assert_eq!(fixture.store.inspect(CID).unwrap(), archive);
    }
}

/// A supported flat archive is original source, not an absent transcript or a
/// reason to manufacture a new directory-format append during normalization.
#[test]
fn legacy_prefix_normalization_preserves_flat_archive() {
    let fixture = Projection::new();
    let path = fixture.store.transcript_path(CID).unwrap();
    let flat = fixture.directory.0.join(format!("{CID}.tsv"));
    let before = std::fs::read(&path).unwrap();
    std::fs::rename(&path, &flat).unwrap();
    fixture
        .store
        .normalize_legacy_prefix(
            CID,
            &fixture.epoch,
            &fixture.owner,
            &fixture.captured,
            RuntimeContextRecords::import_legacy_prefix_backing,
        )
        .unwrap();
    let (_, cold) = AgentTranscriptStore::new(&fixture.directory.0)
        .load_replay_epoch(CID, AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_eq!(
        cold.context().chronology(),
        fixture.owner.context().chronology()
    );
    assert_eq!(std::fs::read(flat).unwrap(), before);
    assert!(!path.exists());
}

/// Unknown lifecycle ancestry is not permission to assign an arbitrary group.
/// Only the historical renderer's grouping can describe plain assistant rows.
#[test]
fn legacy_prefix_normalization_rejects_forged_compatibility_group() {
    let fixture = Projection::new();
    let archive = fixture.store.inspect(CID).unwrap();
    let path = fixture.directory.0.join(CID).join("compaction-epoch.json");
    let before = std::fs::read(&path).unwrap();
    let mut altered = serde_json::to_value(fixture.owner.checkpoint().unwrap()).unwrap();
    altered["events"][2]["execution_group_id"] = serde_json::json!("forged-group");
    altered["pending_execution_groups"][0] = serde_json::json!("forged-group");
    altered["execution_intervals"][0][0] = serde_json::json!("forged-group");
    let altered = ReplayEpochContext::from_checkpoint(
        ReplayConversationId::new(CID).unwrap(),
        serde_json::from_value(altered).unwrap(),
        AgentContext::empty(),
    )
    .unwrap();
    assert!(altered.execution_states().unwrap().is_empty());
    let error = fixture
        .store
        .normalize_legacy_prefix(
            CID,
            &fixture.epoch,
            &altered,
            &fixture.captured,
            RuntimeContextRecords::import_legacy_prefix_backing,
        )
        .unwrap_err();
    assert!(
        error
            .message()
            .contains("original execution or task framing"),
        "{error}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(fixture.store.inspect(CID).unwrap(), archive);
    fixture
        .store
        .normalize_legacy_prefix(
            CID,
            &fixture.epoch,
            &fixture.owner,
            &fixture.captured,
            RuntimeContextRecords::import_legacy_prefix_backing,
        )
        .unwrap();
}

/// Even genuine explicit group rows carry no original producer interval. A
/// structurally valid pending checkpoint claim must not become new provenance
/// merely because its bytes, occurrence and group name match historical rows.
#[test]
fn legacy_prefix_normalization_rejects_forged_explicit_producer_interval() {
    let fixture = Projection::with_tail(TailFormat::GroupedEvent);
    let mut altered = serde_json::to_value(fixture.owner.checkpoint().unwrap()).unwrap();
    altered["execution_intervals"][0][1] = serde_json::json!({
        "generation": 1,
        "identity": mez_agent::ContextBlockOccurrenceId::allocate(),
        "provisional": false,
    });
    let altered = ReplayEpochContext::from_checkpoint(
        ReplayConversationId::new(CID).unwrap(),
        serde_json::from_value(altered).unwrap(),
        AgentContext::empty(),
    )
    .unwrap();
    assert_eq!(altered.execution_states().unwrap().len(), 1);
    let before = fixture.files();
    let error = fixture
        .store
        .normalize_legacy_prefix(
            CID,
            &fixture.epoch,
            &altered,
            &fixture.captured,
            RuntimeContextRecords::import_legacy_prefix_backing,
        )
        .unwrap_err();
    assert!(error.message().contains("producer lifecycle"), "{error}");
    assert_eq!(fixture.files(), before);
    fixture
        .store
        .normalize_legacy_prefix(
            CID,
            &fixture.epoch,
            &fixture.owner,
            &fixture.captured,
            RuntimeContextRecords::import_legacy_prefix_backing,
        )
        .unwrap();
}
