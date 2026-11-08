//! Replay format selection through real resume, history and fork boundaries.
//!
//! Historical selective fixtures are genuine flat v2 files or the result of
//! their existing v3 accounting import. No typed epoch or controller witness
//! is fabricated from those distinct summaries.

use super::*;
use crate::storage::transcript::{AgentCompactionEpoch, AgentCompactionRange};
use std::{collections::BTreeMap, path::PathBuf};

const LEGACY: &str = "legacy-selective";

struct LegacyFixture {
    root: PathBuf,
    store: AgentTranscriptStore,
    rows: Vec<mez_agent::TranscriptEntry>,
}

impl LegacyFixture {
    fn raw() -> Self {
        let root = temp_root("selective-replay-compatibility");
        let store = AgentTranscriptStore::new(&root);
        let rows = (1..=7)
            .map(|sequence| {
                let (role, content, turn) = match sequence {
                    2 | 3 | 5 | 6 => {
                        let first = if sequence < 4 { 2 } else { 5 };
                        let group = format!("legacy-group-{first}");
                        let content =
                            mez_agent::TranscriptContextEvent::execution_block_with_metadata(
                                ContextSourceKind::TranscriptAssistant,
                                "original assistant",
                                format!("original execution {sequence}"),
                                mez_agent::ContextExecutionGroupId::new(&group).unwrap(),
                                sequence - first + 1,
                                None,
                            )
                            .unwrap()
                            .to_transcript_content();
                        (mez_agent::TranscriptRole::System, content, group)
                    }
                    _ => (
                        mez_agent::TranscriptRole::User,
                        format!("original instruction {sequence}"),
                        format!("legacy-user-{sequence}"),
                    ),
                };
                mez_agent::TranscriptEntry {
                    conversation_id: LEGACY.into(),
                    sequence,
                    created_at_unix_seconds: sequence,
                    role,
                    turn_id: turn,
                    agent_id: "agent-%1".into(),
                    pane_id: "%1".into(),
                    content,
                }
            })
            .collect::<Vec<_>>();
        store.append_many(&rows).unwrap();
        Self { root, store, rows }
    }

    fn epoch() -> AgentCompactionEpoch {
        AgentCompactionEpoch {
            version: 2,
            conversation_id: LEGACY.into(),
            through_sequence: 1,
            summary: "original independent prefix summary".into(),
            ranges: vec![
                AgentCompactionRange {
                    first_sequence: 2,
                    through_sequence: 3,
                    summary: "original first selective summary".into(),
                },
                AgentCompactionRange {
                    first_sequence: 5,
                    through_sequence: 6,
                    summary: "original second selective summary".into(),
                },
            ],
            summary_records: None,
        }
    }

    fn legacy(version: u64) -> Self {
        let fixture = Self::raw();
        std::fs::write(
            fixture.root.join(LEGACY).join("compaction-epoch.json"),
            serde_json::to_vec(&Self::epoch()).unwrap(),
        )
        .unwrap();
        if version == 3 {
            // Assign original v3 summary records through the real historical
            // accounting import before taking the immutability snapshot.
            assert_eq!(
                fixture
                    .store
                    .compaction_epoch(LEGACY)
                    .unwrap()
                    .unwrap()
                    .version,
                3
            );
        } else {
            assert_eq!(version, 2);
        }
        fixture
    }

    fn source_files(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        fn visit(path: &std::path::Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in std::fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    visit(&path, out);
                } else {
                    out.insert(path.clone(), std::fs::read(path).unwrap());
                }
            }
        }
        let mut files = BTreeMap::new();
        visit(&self.root.join(LEGACY), &mut files);
        files
    }
}

impl Drop for LegacyFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Actual direct resume must use the typed authority even when its independent
/// archive count is zero. The next prompt retains its head and exact raw tail.
#[test]
fn direct_resume_loads_single_head_authority_without_legacy_decoding() {
    use crate::test_support::replay_epoch::{CONVERSATION, Fixture};

    let fixture = Fixture::new();
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(fixture.store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .execute_agent_shell_command(&primary, &format!("/resume {CONVERSATION}"))
        .unwrap();
    let outcome = service
        .run_pending_deferred_agent_command_for_tests()
        .unwrap()
        .unwrap();
    assert_eq!(
        service.agent_shell_store().get("%1").unwrap().session_id,
        CONVERSATION,
        "{outcome}"
    );
    let restored = execute_runtime_agent_prompt_history_work(
        service.prepare_runtime_agent_prompt_history_work("%1"),
    )
    .unwrap()
    .replay_owner()
    .unwrap()
    .unwrap();
    assert_eq!(
        restored.context().chronology(),
        fixture.source.context().chronology()
    );
    let context = service
        .agent_context_for_pane_prompt("%1", "new current task", 0)
        .unwrap();
    // The explicit new task releases the previous task's instruction protection
    // while preserving all original source records, bytes and native framing.
    for (actual, original) in context
        .chronology()
        .iter()
        .zip(fixture.source.context().chronology())
    {
        assert_eq!(actual.record(), original.record());
        assert_eq!(actual.block(), original.block());
        assert_eq!(actual.sequence(), original.sequence());
        assert_eq!(actual.execution_group_id(), original.execution_group_id());
        assert_eq!(actual.provider_owner(), original.provider_owner());
    }
    service.terminate_all_pane_processes().unwrap();
}

/// A saved prefix crosses the real resume and history worker boundaries without
/// another model call. Cold replay preserves its original summary occurrence,
/// raw roles/groups and chronology while covered source stays absent.
#[test]
fn legacy_prefix_history_normalizes_without_model_or_source_expansion() {
    for version in [1, 3] {
        let fixture = LegacyFixture::raw();
        fixture
            .store
            .save_compaction_epoch(LEGACY, 3, "original single prefix head")
            .unwrap();
        if version == 1 {
            let old = AgentCompactionEpoch {
                version: 1,
                conversation_id: LEGACY.into(),
                through_sequence: 3,
                summary: "original single prefix head".into(),
                ranges: Vec::new(),
                summary_records: None,
            };
            std::fs::write(
                fixture.root.join(LEGACY).join("compaction-epoch.json"),
                serde_json::to_vec(&old).unwrap(),
            )
            .unwrap();
        }
        let old = fixture.store.compaction_epoch(LEGACY).unwrap().unwrap();
        let record = old.summary_records.as_ref().unwrap().prefix.unwrap();
        let archive = std::fs::read(fixture.store.transcript_path(LEGACY).unwrap()).unwrap();
        let mut original = None;
        for _ in 0..2 {
            let mut service = resumed_prefix(&fixture.store);
            let history = execute_runtime_agent_prompt_history_work(
                service.prepare_runtime_agent_prompt_history_work("%1"),
            )
            .unwrap()
            .replay_owner()
            .unwrap()
            .unwrap();
            assert_eq!(
                history.head().unwrap().occurrence_id(),
                record.occurrence_id()
            );
            assert_eq!(history.context().chronology()[0].record(), record);
            assert_eq!(
                history.context().chronology()[0].block().content,
                old.summary
            );
            assert_eq!(history.members().len(), 4);
            let group = mez_agent::ContextExecutionGroupId::new("legacy-group-5").unwrap();
            assert!(history.context().execution_group_is_pending(&group));
            assert!(
                history
                    .execution_states()
                    .unwrap()
                    .iter()
                    .all(|state| state.is_pending())
            );
            for row in &fixture.rows[3..] {
                let content = match mez_agent::TranscriptContextEvent::from_transcript_content(
                    &row.content,
                ) {
                    Some(mez_agent::TranscriptContextEvent::ExecutionBlock { content, .. }) => {
                        content
                    }
                    _ => row.content.clone(),
                };
                assert_eq!(
                    history
                        .context()
                        .chronology()
                        .iter()
                        .filter(|event| event.block().content == content)
                        .count(),
                    1
                );
            }
            assert!(!history.context().chronology().iter().any(|event| {
                event.block().content.contains("original execution 2")
                    || event.block().content.contains("original instruction 1")
            }));
            if let Some(previous) = &original {
                assert!(history.context().chronology() == previous);
            } else {
                original = Some(history.context().chronology().to_vec());
            }
            assert!(service.pending_agent_provider_tasks().is_empty());
            assert!(service.pending_agent_compaction_tasks().is_empty());
            service.terminate_all_pane_processes().unwrap();
        }
        let header: serde_json::Value = serde_json::from_slice(
            &std::fs::read(fixture.root.join(LEGACY).join("compaction-epoch.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(header["manifest"]["version"], 10);
        assert!(header["manifest"]["prefix_origin"].is_object());
        let images =
            rusqlite::Connection::open(fixture.root.join(LEGACY).join("row-origins.sqlite3"))
                .unwrap();
        let descriptors: u32 = images.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='execution_sources'",
            [], |row| row.get(0),
        ).unwrap();
        assert_eq!(
            descriptors, 0,
            "legacy image capture cannot invent producer descriptors"
        );
        assert_eq!(
            std::fs::read(fixture.store.transcript_path(LEGACY).unwrap()).unwrap(),
            archive
        );
    }
}

/// Uses actual command/deferred resume ownership, with no provider dispatch.
fn resumed_prefix(store: &AgentTranscriptStore) -> RuntimeSessionService {
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(store.clone());
    let primary = service
        .attach_primary("prefix-controller", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .execute_agent_shell_command(&primary, &format!("/resume {LEGACY}"))
        .unwrap();
    service
        .run_pending_deferred_agent_command_for_tests()
        .unwrap()
        .unwrap();
    assert_eq!(
        service.agent_shell_store().get("%1").unwrap().session_id,
        LEGACY
    );
    service
}

/// Every existing publication fault seam leaves a complete old or migrated
/// authority. Retrying source loading reuses the original summary record; after
/// an ambiguous committed swap it preserves the exact publication identity.
#[test]
fn legacy_prefix_normalization_faults_preserve_complete_authority() {
    use crate::storage::transcript::replay_epoch::Phase;
    for phase in [
        Phase::BeforePayloadSync,
        Phase::BeforeSourceReceiptSync,
        Phase::BeforeManifestSwap,
        Phase::AfterManifestSwap,
        Phase::BeforeRequiredMarker,
        Phase::BeforeAcknowledgment,
    ] {
        let fixture = LegacyFixture::raw();
        fixture
            .store
            .save_compaction_epoch(LEGACY, 3, "fault-safe original head")
            .unwrap();
        let record = fixture
            .store
            .compaction_epoch(LEGACY)
            .unwrap()
            .unwrap()
            .summary_records
            .unwrap()
            .prefix
            .unwrap();
        let archive = std::fs::read(fixture.store.transcript_path(LEGACY).unwrap()).unwrap();
        let path = fixture.root.join(LEGACY).join("compaction-epoch.json");
        let before = std::fs::read(&path).unwrap();
        let mut service = resumed_prefix(&fixture.store);
        fixture.store.inject_replay_epoch_fault(phase, 1, false);
        assert!(
            execute_runtime_agent_prompt_history_work(
                service.prepare_runtime_agent_prompt_history_work("%1")
            )
            .is_err()
        );
        let interrupted = std::fs::read(&path).unwrap();
        let header: serde_json::Value = serde_json::from_slice(&interrupted).unwrap();
        let committed = header.get("manifest").is_some();
        if !committed {
            assert_eq!(interrupted, before);
        }
        let owner = execute_runtime_agent_prompt_history_work(
            service.prepare_runtime_agent_prompt_history_work("%1"),
        )
        .unwrap()
        .replay_owner()
        .unwrap()
        .unwrap();
        assert_eq!(
            owner.head().unwrap().occurrence_id(),
            record.occurrence_id()
        );
        if committed {
            assert_eq!(std::fs::read(&path).unwrap(), interrupted);
        }
        assert_eq!(
            std::fs::read(fixture.store.transcript_path(LEGACY).unwrap()).unwrap(),
            archive
        );
        assert!(service.pending_agent_provider_tasks().is_empty());
        assert!(service.pending_agent_compaction_tasks().is_empty());
        service.terminate_all_pane_processes().unwrap();
    }
}

/// A typed root's required images are not a rebuildable cache. Their loss must
/// refuse repeated cold/history loads rather than reentering legacy capture.
#[test]
fn legacy_prefix_normalization_never_recaptures_missing_required_images() {
    let fixture = LegacyFixture::raw();
    fixture
        .store
        .save_compaction_epoch(LEGACY, 3, "original head")
        .unwrap();
    let mut service = resumed_prefix(&fixture.store);
    execute_runtime_agent_prompt_history_work(
        service.prepare_runtime_agent_prompt_history_work("%1"),
    )
    .unwrap();
    let path = fixture.root.join(LEGACY).join("compaction-epoch.json");
    let before = std::fs::read(&path).unwrap();
    let images =
        rusqlite::Connection::open(fixture.root.join(LEGACY).join("row-origins.sqlite3")).unwrap();
    assert_eq!(
        images
            .execute("DELETE FROM row_digests WHERE sequence='5'", [])
            .unwrap(),
        1
    );
    for _ in 0..2 {
        let error = execute_runtime_agent_prompt_history_work(
            service.prepare_runtime_agent_prompt_history_work("%1"),
        )
        .unwrap_err();
        assert!(error.message().contains("image"), "{error}");
        assert!(
            AgentTranscriptStore::new(&fixture.root)
                .load_replay_epoch(LEGACY, mez_agent::AgentContext::empty())
                .is_err()
        );
        let missing: u32 = images
            .query_row(
                "SELECT count(*) FROM row_digests WHERE sequence='5'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(missing, 0);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    service.terminate_all_pane_processes().unwrap();
}

/// Original prefix provenance remains required after migration. Missing or
/// changed bytes cannot cause a second migration, provider call or raw fallback.
#[test]
fn legacy_prefix_normalization_requires_original_authority_payload() {
    for missing in [false, true] {
        let fixture = LegacyFixture::raw();
        fixture
            .store
            .save_compaction_epoch(LEGACY, 3, "original head")
            .unwrap();
        let mut service = resumed_prefix(&fixture.store);
        execute_runtime_agent_prompt_history_work(
            service.prepare_runtime_agent_prompt_history_work("%1"),
        )
        .unwrap();
        let path = fixture.root.join(LEGACY).join("compaction-epoch.json");
        let before = std::fs::read(&path).unwrap();
        let header: serde_json::Value = serde_json::from_slice(&before).unwrap();
        let hash = header["manifest"]["prefix_origin"]["authority"]["sha256"]
            .as_str()
            .unwrap();
        let payload = fixture.root.join(LEGACY).join("epoch-payloads").join(hash);
        if missing {
            std::fs::remove_file(&payload).unwrap();
        } else {
            let mut bytes = std::fs::read(&payload).unwrap();
            bytes[0] ^= 1;
            std::fs::write(&payload, bytes).unwrap();
        }
        for _ in 0..2 {
            assert!(
                execute_runtime_agent_prompt_history_work(
                    service.prepare_runtime_agent_prompt_history_work("%1"),
                )
                .is_err()
            );
            assert!(
                AgentTranscriptStore::new(&fixture.root)
                    .load_replay_epoch(LEGACY, mez_agent::AgentContext::empty())
                    .is_err()
            );
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert!(service.pending_agent_provider_tasks().is_empty());
            assert!(service.pending_agent_compaction_tasks().is_empty());
        }
        service.terminate_all_pane_processes().unwrap();
    }
}

/// Successful typed admission still verifies required payloads before changing
/// the pane binding; a damaged head cannot degrade into raw-only resume.
#[test]
fn direct_resume_rejects_damaged_single_head_before_rebinding() {
    use crate::test_support::replay_epoch::{CONVERSATION, Fixture};
    use sha2::{Digest, Sha256};

    let fixture = Fixture::new();
    let source = fixture.source.context().chronology()[0]
        .block()
        .content
        .as_bytes();
    let hash = Sha256::digest(source)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let payload = fixture
        .root
        .join(CONVERSATION)
        .join("epoch-payloads")
        .join(hash);
    assert_eq!(std::fs::read(&payload).unwrap(), source);
    let mut damaged = source.to_vec();
    damaged[0] ^= 1;
    std::fs::write(payload, damaged).unwrap();
    let manifest = fixture
        .root
        .join(CONVERSATION)
        .join("compaction-epoch.json");
    let before = std::fs::read(&manifest).unwrap();
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(fixture.store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let original = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    service
        .execute_agent_shell_command(&primary, &format!("/resume {CONVERSATION}"))
        .unwrap();
    let outcome = service
        .run_pending_deferred_agent_command_for_tests()
        .unwrap()
        .unwrap();
    assert!(outcome.contains("integrity"), "{outcome}");
    assert_eq!(
        service.agent_shell_store().get("%1").unwrap().session_id,
        original
    );
    assert_eq!(std::fs::read(manifest).unwrap(), before);
    service.terminate_all_pane_processes().unwrap();
}

fn selective_replay_refusal(version: u64) {
    let fixture = LegacyFixture::legacy(version);
    let before = fixture.source_files();
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(fixture.store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let original = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    service
        .execute_agent_shell_command(&primary, &format!("/resume {LEGACY}"))
        .unwrap();
    let outcome = service
        .run_pending_deferred_agent_command_for_tests()
        .unwrap()
        .unwrap();
    assert!(outcome.contains("normalization"), "v{version}: {outcome}");
    assert_eq!(
        service.agent_shell_store().get("%1").unwrap().session_id,
        original
    );
    assert_eq!(fixture.source_files(), before);

    service
        .agent_shell_store_mut()
        .bind_conversation("%1", LEGACY, 7)
        .unwrap();
    for _ in 0..2 {
        let error = execute_runtime_agent_prompt_history_work(
            service.prepare_runtime_agent_prompt_history_work("%1"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("normalization"), "{error}");
        assert_eq!(fixture.source_files(), before);
    }
    let error = fixture
        .store
        .fork(LEGACY, "unsupported-child", 123)
        .unwrap_err();
    assert!(error.to_string().contains("normalization"), "{error}");
    assert!(!fixture.root.join("unsupported-child").exists());
    assert_eq!(fixture.source_files(), before);

    // A legacy prefix replacement is not an explicit normalization operation.
    // It must not silently consume the incompatible selective authority either.
    let error = fixture
        .store
        .save_compaction_epoch(LEGACY, 7, "not an admitted normalization")
        .unwrap_err();
    assert!(error.to_string().contains("normalization"), "{error}");
    assert_eq!(fixture.source_files(), before);

    // Refusing one source must not disable other history in the same store.
    let mut other = fixture.rows[0].clone();
    other.conversation_id = "unaffected".into();
    other.content = "unaffected conversation remains usable".into();
    fixture.store.append(&other).unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "unaffected", 1)
        .unwrap();
    let history = execute_runtime_agent_prompt_history_work(
        service.prepare_runtime_agent_prompt_history_work("%1"),
    )
    .unwrap();
    assert!(
        history
            .blocks
            .iter()
            .any(|block| block.content == other.content)
    );
    assert_eq!(fixture.source_files(), before);
    service.terminate_all_pane_processes().unwrap();
}

#[test]
fn selective_v2_replay_requires_explicit_normalization() {
    selective_replay_refusal(2);
}

#[test]
fn selective_v3_replay_requires_explicit_normalization() {
    selective_replay_refusal(3);
}

/// No publication API may produce another selective sidecar. Refusal must
/// precede summary-record allocation or any durable source/epoch replacement.
#[test]
fn selective_writer_refuses_before_any_source_change() {
    let fixture = LegacyFixture::raw();
    let before = fixture.source_files();
    let selected = fixture
        .rows
        .iter()
        .filter(|row| matches!(row.sequence, 2 | 3 | 5 | 6))
        .cloned()
        .collect::<Vec<_>>();
    let error = fixture
        .store
        .save_compaction_ranges_with_proof(LegacyFixture::epoch(), &selected)
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("selective compaction publication"),
        "{error}"
    );
    assert_eq!(fixture.source_files(), before);
}
