//! Stable accounting follows actual producers across prefix and epoch changes.
//!
//! Both controls persist a real stopped turn through its checked append owner,
//! then build a new runtime over that archive. Configuration remains current
//! authority; snapshot carriers and prefix ordering are descriptive provenance.

use super::*;

const SETTINGS: &str = "[agents]\ncustom_system_prompt = \"initial prefix\"\ndefault_personality = \"careful\"\n[personalities.careful]\nname = \"Careful\"\nsystem_prompt = \"current guidance 雪 {}\"\nresponse_style = \"terse\"\n";

/// Owns the real config/archive shared by independent runtime instances.
struct Fixture {
    root: PathBuf,
    store: AgentTranscriptStore,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = temp_root(label);
        let store = AgentTranscriptStore::new(root.join("transcripts"));
        Self { root, store }
    }

    /// Supplies current trusted configuration before activating a new pane.
    fn service(&self, settings: &str) -> RuntimeSessionService {
        let path = self.root.join("config.toml");
        fs::write(&path, settings).unwrap();
        let mut service = test_runtime_service();
        service
            .replace_config_layers(vec![ConfigLayer {
                name: "primary".into(),
                path: Some(path),
                format: ConfigFormat::Toml,
                scope: ConfigScope::Primary,
                trusted: true,
                text: settings.into(),
            }])
            .unwrap();
        service.set_agent_transcript_store(self.store.clone());
        service.persistence.enable_transcript_adapter();
        service
            .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        service
    }

    /// Completes original stopped-turn bookkeeping and its exact durable append.
    fn persist(&self) -> (String, mez_agent::AgentContext, u64) {
        let mut service = self.service(SETTINGS);
        let turn = service
            .start_agent_prompt_turn("%1", "original prompt 雪")
            .unwrap();
        let context = service
            .agent_turn_contexts()
            .get(&turn.turn_id)
            .unwrap()
            .clone();
        let cid = service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .session_id
            .clone();
        service.stop_agent_turn_for_pane("%1").unwrap();
        let work = service.claim_bookkeeping_candidates().pop().unwrap();
        let history = work.execute();
        assert!(
            service
                .complete_bookkeeping_candidate(work, history)
                .unwrap()
        );
        let mut committed = 0;
        for effect in service
            .drain_transcript_persistence_transition()
            .side_effects
        {
            if let RuntimeSideEffect::PersistTranscriptEntries { store, path, batch } = effect {
                let bytes = store.commit_accounted_transcript_batch(&batch, 1).unwrap();
                assert!(
                    service
                        .apply_persistence_transition(
                            crate::runtime::PersistenceEvent::TranscriptCompleted {
                                conversation_id: batch.conversation_id().into(),
                                first_sequence: batch.first_sequence(),
                                batch,
                                path,
                                bytes,
                            }
                        )
                        .unwrap()
                        .applied
                );
                committed += 1;
            }
        }
        assert!(committed > 0);
        let count = self.store.inspect(&cid).unwrap().len() as u64;
        (cid, context, count)
    }

    /// Uses the actual public prompt admission/composition over a cold store.
    fn resume(&self, settings: &str, cid: &str, count: u64) -> mez_agent::AgentContext {
        let mut service = self.service(settings);
        service
            .agent_shell_store_mut()
            .bind_conversation("%1", cid, count)
            .unwrap();
        let turn = service
            .start_agent_prompt_turn("%1", "next prompt")
            .unwrap();
        service
            .agent_turn_contexts()
            .get(&turn.turn_id)
            .unwrap()
            .clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Removing an earlier configured slot does not create a new occurrence for
/// unchanged personality/style producers. Their source bytes and roles remain
/// current, while the deleted instruction stays absent on actual cold replay.
#[test]
fn runtime_stable_preference_records_survive_preceding_slot_removal() {
    let fixture = Fixture::new("stable-prefix-removal");
    let (cid, original, count) = fixture.persist();
    let settings = SETTINGS.replace("custom_system_prompt = \"initial prefix\"\n", "");
    let restored = fixture.resume(&settings, &cid, count);
    assert!(
        restored
            .stable_slots()
            .iter()
            .all(|slot| slot.block().label != "configured agent system prompt")
    );
    for label in ["agent personality system prompt", "agent shell personality"] {
        let before = original
            .stable_slots()
            .iter()
            .find(|slot| slot.block().label == label)
            .unwrap();
        let after = restored
            .stable_slots()
            .iter()
            .find(|slot| slot.block().label == label)
            .unwrap();
        assert_eq!(before.block(), after.block());
        assert_eq!(
            before.record(),
            after.record(),
            "unchanged producer lost its original record after preceding slot removal: {label}"
        );
    }
}

/// Covering the sole raw snapshot carrier changes chronological replay, not
/// the original stable occurrence. Current guidance must reuse that saved
/// record even when the carrier itself is represented only by a summary.
#[test]
fn runtime_stable_preference_records_survive_epoch_covered_carrier() {
    let fixture = Fixture::new("stable-covered-carrier");
    let (cid, original, count) = fixture.persist();
    fixture
        .store
        .save_compaction_epoch(&cid, count, "compacted original work")
        .unwrap();
    let restored = fixture.resume(SETTINGS, &cid, count);
    assert!(
        restored
            .blocks()
            .iter()
            .any(|block| block.source == ContextSourceKind::CompactionPrefix)
    );
    assert!(
        restored
            .chronology()
            .iter()
            .all(|event| event.block().label != "interrupted turn context")
    );
    for label in [
        "configured agent system prompt",
        "agent personality system prompt",
        "agent shell personality",
    ] {
        let before = original
            .stable_slots()
            .iter()
            .find(|slot| slot.block().label == label)
            .unwrap();
        let after = restored
            .stable_slots()
            .iter()
            .find(|slot| slot.block().label == label)
            .unwrap();
        assert_eq!(before.slot_id(), after.slot_id());
        assert_eq!(before.block(), after.block());
        assert_eq!(
            before.record(),
            after.record(),
            "epoch coverage discarded stable record: {label}"
        );
    }
}
