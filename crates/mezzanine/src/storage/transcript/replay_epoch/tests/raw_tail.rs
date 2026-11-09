//! Real source proposals, required authority and cold replay for intentionally
//! absent heads. These fixtures do not dispatch providers or mutate user state.

use super::*;

fn prepare_raw(
    fixture: &Fixture,
    live: &ReplayEpochContext,
    parent: Option<ReplayEpochIdentity>,
    limit: Option<usize>,
) -> ReplayEpochPublication {
    let cid = live.conversation_id().as_str();
    let history = parent.as_ref().map(|_| {
        let _lease = fixture.store.acquire_conversation_lock(cid).unwrap();
        fixture
            .store
            .replay_epoch_snapshot_locked(cid)
            .unwrap()
            .unwrap()
    });
    let policy = ReplayTailPolicy::new(100_000, 100)
        .unwrap()
        .with_raw_tail_limits(19, limit);
    let frozen = match live.prepare_compaction(policy).unwrap() {
        ReplayCompactionPreparation::Ready(plan) => plan,
        other => panic!("expected real raw omission: {other:?}"),
    };
    fixture
        .store
        .prepare_replay_epoch(
            origin().capture(history.as_ref()),
            cid,
            parent,
            live,
            frozen,
            ReplayHeadPublication::IntentionalOmission,
        )
        .unwrap()
}

fn initial_raw(fixture: &Fixture, retained: bool) -> ReplayEpochCommit {
    let mut live = source();
    if retained {
        live.append_fact("tail", "retained original raw member")
            .unwrap();
    }
    let proposal = prepare_raw(fixture, &live, None, Some(usize::from(retained)));
    fixture.store.publish_replay_epoch(&proposal).unwrap()
}

/// A raw publication must retain exact protection and post-frontier arrivals
/// through the same durable receipt and reopen path as a model summary.
#[test]
fn durable_raw_tail_reopens_without_a_summary_head() {
    let fixture = Fixture::new();
    let mut live = source();
    live.append_pending_peer("peer", "exact required source")
        .unwrap();
    live.append_fact("optional", "omit after the protected peer")
        .unwrap();
    let required = live.context().chronology()[1].clone();
    let policy = ReplayTailPolicy::new(10_000, 0)
        .unwrap()
        .with_raw_tail_limits(19, Some(0));
    let frozen = match live.prepare_compaction(policy).unwrap() {
        ReplayCompactionPreparation::Ready(plan) => plan,
        other => panic!("expected real raw omission: {other:?}"),
    };
    live.append_fact("arrival", "exact late original").unwrap();
    let late = live.context().chronology().last().unwrap().clone();
    let prepared = fixture
        .store
        .prepare_replay_epoch(
            origin().capture(None),
            "conversation-a",
            None,
            &live,
            frozen,
            ReplayHeadPublication::IntentionalOmission,
        )
        .unwrap();
    fixture.store.publish_replay_epoch(&prepared).unwrap();
    let (identity, restored) = AgentTranscriptStore::new(fixture.root.clone())
        .load_replay_epoch("conversation-a", prefix())
        .unwrap()
        .unwrap();
    assert_eq!(identity, prepared.identity());
    assert_eq!(restored.revision().get(), 1);
    assert!(restored.head().is_none());
    assert_eq!(
        restored.head_policy(),
        ReplayHeadPolicy::IntentionalOmission
    );
    assert_eq!(restored.context().chronology(), &[required, late]);
    assert_eq!(restored.members().len(), 2);
    assert!(
        restored
            .context()
            .chronology()
            .iter()
            .all(|event| !mez_agent::context_block_is_compaction_summary(event.block()))
    );
    assert!(fixture.store.compaction_epoch("conversation-a").is_err());
}

/// Omitting all frozen raw source can leave only later arrivals. Their first
/// sequence is beyond the frozen frontier, not a fabricated summary anchor.
#[test]
fn durable_raw_tail_preserves_only_post_frontier_arrivals() {
    let fixture = Fixture::new();
    let mut live = source();
    let policy = ReplayTailPolicy::new(10_000, 0)
        .unwrap()
        .with_raw_tail_limits(19, Some(0));
    let frozen = match live.prepare_compaction(policy).unwrap() {
        ReplayCompactionPreparation::Ready(plan) => plan,
        other => panic!("expected original raw omission: {other:?}"),
    };
    let frontier = frozen.arrival_frontier().unwrap();
    let sequence = live
        .append_fact("arrival", "only this later original remains")
        .unwrap();
    assert!(sequence > frontier);
    let expected = live.context().chronology().last().unwrap().clone();
    let prepared = fixture
        .store
        .prepare_replay_epoch(
            origin().capture(None),
            "conversation-a",
            None,
            &live,
            frozen,
            ReplayHeadPublication::IntentionalOmission,
        )
        .unwrap();
    fixture.store.publish_replay_epoch(&prepared).unwrap();
    let (_, restored) = fixture.load();
    assert!(restored.head().is_none());
    assert_eq!(restored.context().chronology(), &[expected]);
    assert_eq!(restored.members()[0].born_in().get(), 0);
}

/// No raw members is a real committed epoch, never permission to fall back to
/// old archive history when its required manifest or immutable receipt is lost.
#[test]
fn durable_raw_tail_empty_epoch_requires_its_original_authority() {
    let fixture = Fixture::new();
    initial_raw(&fixture, false);
    let (_, owner) = fixture.load();
    assert_eq!(owner.revision().get(), 1);
    assert!(owner.head().is_none() && owner.members().is_empty());
    let path = fixture.root.join("conversation-a").join(MANIFEST_NAME);
    let original = fs::read(&path).unwrap();
    fs::remove_file(&path).unwrap();
    assert!(
        fixture
            .store
            .load_replay_epoch("conversation-a", prefix())
            .is_err()
    );
    fs::write(&path, &original).unwrap();
    fs::remove_file(
        fixture
            .root
            .join("conversation-a/epoch-payloads")
            .join(digest(&original)),
    )
    .unwrap();
    assert!(
        fixture
            .store
            .load_replay_epoch("conversation-a", prefix())
            .is_err()
    );
}

/// The actual three policy transitions keep only current membership. Removing
/// a prior head alone is valid; restoring covered ancestors is never required.
#[test]
fn durable_raw_tail_mode_transitions_keep_current_membership_only() {
    let fixture = Fixture::new();
    let first = proposal(&fixture, &source(), None, "old model head");
    fixture.store.publish_replay_epoch(&first).unwrap();
    let (parent, mut live) = fixture.load();
    live.append_fact("raw", "older optional occurrence")
        .unwrap();
    live.append_fact("raw", "newest optional occurrence")
        .unwrap();
    let retained = live.members().last().unwrap().clone();
    let raw = prepare_raw(&fixture, &live, Some(parent), Some(1));
    fixture.store.publish_replay_epoch(&raw).unwrap();
    let (parent, mut live) = fixture.load();
    assert_eq!(live.members(), &[retained]);
    assert_eq!(live.head_policy(), ReplayHeadPolicy::IntentionalOmission);
    live.append_fact("fresh", "source after raw publication")
        .unwrap();
    let snapshot = {
        let _lease = fixture
            .store
            .acquire_conversation_lock("conversation-a")
            .unwrap();
        fixture
            .store
            .replay_epoch_snapshot_locked("conversation-a")
            .unwrap()
            .unwrap()
    };
    let policy = ReplayTailPolicy::new(10_000, 0)
        .unwrap()
        .with_head_policy(ReplayHeadPolicy::Summary);
    let frozen = match live.prepare_compaction(policy).unwrap() {
        ReplayCompactionPreparation::Ready(plan) => plan,
        other => panic!("expected current summary source: {other:?}"),
    };
    assert_eq!(frozen.summary_input().len(), 2);
    assert_eq!(
        frozen.summary_input()[0].block().content,
        "newest optional occurrence"
    );
    let summary = fixture
        .store
        .prepare_replay_epoch(
            origin().capture(Some(&snapshot)),
            "conversation-a",
            Some(parent),
            &live,
            frozen,
            ReplayHeadPublication::Summary("new model head".into()),
        )
        .unwrap();
    fixture.store.publish_replay_epoch(&summary).unwrap();
    let (parent, live) = fixture.load();
    assert_eq!(live.head_policy(), ReplayHeadPolicy::Summary);
    assert!(live.members().is_empty());
    let head_only = prepare_raw(&fixture, &live, Some(parent), None);
    assert!(head_only.envelope.manifest.covered.is_empty());
    fixture.store.publish_replay_epoch(&head_only).unwrap();
    let (_, live) = fixture.load();
    assert_eq!(live.revision().get(), 4);
    assert_eq!(live.head_policy(), ReplayHeadPolicy::IntentionalOmission);
    assert!(live.head().is_none() && live.context().chronology().is_empty());
}

/// Storage failures keep the same prepared source/publication. A lost swap or
/// ACK cannot allocate another raw epoch or require model/effect replay.
#[test]
fn durable_raw_tail_publication_faults_recover_the_exact_receipt() {
    for old_exists in [false, true] {
        for fault in [
            Phase::BeforeManifestSwap,
            Phase::AfterManifestSwap,
            Phase::BeforeRequiredMarker,
            Phase::BeforeAcknowledgment,
        ] {
            let fixture = Fixture::new();
            let (parent, mut live) = if old_exists {
                let first = proposal(&fixture, &source(), None, "old complete head");
                fixture.store.publish_replay_epoch(&first).unwrap();
                let (parent, owner) = fixture.load();
                (Some(parent), owner)
            } else {
                (None, source())
            };
            live.append_fact("new", "original source for this raw epoch")
                .unwrap();
            let proposal = prepare_raw(&fixture, &live, parent.clone(), Some(0));
            {
                let lease = fixture
                    .store
                    .acquire_conversation_lock("conversation-a")
                    .unwrap();
                assert!(
                    fixture
                        .store
                        .publish_replay_epoch_locked(&proposal, &lease, |phase| {
                            if phase == fault {
                                Err(invalid("injected raw publication fault"))
                            } else {
                                Ok(())
                            }
                        })
                        .is_err()
                );
            }
            let visible = fixture
                .store
                .load_replay_epoch("conversation-a", prefix())
                .unwrap();
            if fault == Phase::BeforeManifestSwap {
                assert_eq!(visible.as_ref().map(|(id, _)| id), parent.as_ref());
            } else {
                assert_eq!(
                    visible.as_ref().map(|(id, _)| id),
                    Some(&proposal.identity())
                );
                assert!(visible.unwrap().1.head().is_none());
            }
            fixture.store.publish_replay_epoch(&proposal).unwrap();
            fixture.store.publish_replay_epoch(&proposal).unwrap();
            let (identity, owner) = fixture.load();
            assert_eq!(identity, proposal.identity());
            assert_eq!(owner.head_policy(), ReplayHeadPolicy::IntentionalOmission);
            assert!(owner.head().is_none() && owner.members().is_empty());
        }
    }
}

#[test]
fn durable_raw_tail_initial_forks_keep_empty_and_nonempty_epochs_typed() {
    for retained in [false, true] {
        let fixture = Fixture::new();
        initial_raw(&fixture, retained);
        let (_, owner) = fixture.load();
        let snapshot = {
            let _lease = fixture
                .store
                .acquire_conversation_lock("conversation-a")
                .unwrap();
            fixture
                .store
                .replay_epoch_snapshot_locked("conversation-a")
                .unwrap()
                .unwrap()
        };
        let source = snapshot.fork_source(&owner, &[]).unwrap();
        assert!(source.has_epoch());
        let batch = source.initial_batch("raw-child", 100).unwrap();
        assert!(batch.initial_replay_epoch().is_some());
        assert_eq!(batch.entries().len(), usize::from(retained));
        if retained {
            // Only a genuinely empty headless copy may omit its frontier.
            let mut value = serde_json::to_value(batch.initial_replay_epoch().unwrap()).unwrap();
            value["envelope"]["manifest"]["frozen_arrival_frontier"] = serde_json::Value::Null;
            let mut envelope: Envelope = serde_json::from_value(value["envelope"].clone()).unwrap();
            envelope.sha256 = digest(&encode(&envelope.manifest).unwrap());
            value["envelope"] = serde_json::to_value(envelope).unwrap();
            let invalid: InitialReplayEpoch = serde_json::from_value(value).unwrap();
            assert!(invalid.owner().is_err());
        }
        fixture
            .store
            .accept_transcript_append_receipt(&batch, 1)
            .unwrap();
        fixture
            .store
            .settle_transcript_append_receipt(&batch, 1)
            .unwrap();
        fixture
            .store
            .settle_transcript_append_receipt(&batch, 1)
            .unwrap();
        fixture.store.delete("conversation-a").unwrap();
        let (_, child) = fixture
            .store
            .load_replay_epoch("raw-child", prefix())
            .unwrap()
            .unwrap();
        assert_eq!(child.head_policy(), ReplayHeadPolicy::IntentionalOmission);
        assert_eq!(child.revision(), owner.revision());
        assert_eq!(child.members(), owner.members());
        assert_eq!(child.context().chronology(), owner.context().chronology());
        assert!(child.head().is_none());
    }
}

#[test]
fn durable_raw_tail_saved_forks_keep_empty_and_nonempty_epochs_independent() {
    for retained in [false, true] {
        let fixture = Fixture::new();
        initial_raw(&fixture, retained);
        let (_, expected) = fixture.load();
        fixture
            .store
            .fork("conversation-a", "raw-saved", 100)
            .unwrap();
        fixture.store.fork("raw-saved", "raw-nested", 101).unwrap();
        fixture.store.delete("conversation-a").unwrap();
        fixture.store.delete("raw-saved").unwrap();
        let (_, child) = fixture
            .store
            .load_replay_epoch("raw-nested", prefix())
            .unwrap()
            .unwrap();
        assert_eq!(child.head_policy(), ReplayHeadPolicy::IntentionalOmission);
        assert_eq!(child.revision(), expected.revision());
        assert_eq!(child.members(), expected.members());
        assert_eq!(
            child.context().chronology(),
            expected.context().chronology()
        );
        assert!(child.head().is_none());
    }
}

#[test]
fn durable_raw_tail_memory_forks_keep_epoch_identity_without_a_parent_store() {
    for retained in [false, true] {
        let fixture = Fixture::new();
        let mut live = source();
        if retained {
            live.append_fact("tail", "exact memory member").unwrap();
        }
        let policy = ReplayTailPolicy::new(10_000, 100)
            .unwrap()
            .with_raw_tail_limits(0, Some(usize::from(retained)));
        let frozen = match live.prepare_compaction(policy).unwrap() {
            ReplayCompactionPreparation::Ready(plan) => plan,
            other => panic!("expected actual memory omission: {other:?}"),
        };
        live.publish_compaction(frozen, ReplayHeadPublication::IntentionalOmission)
            .unwrap();
        if !retained {
            // Empty membership still has positive committed epoch identity.
            // It cannot be downgraded to an untyped pre-publication raw fork.
            assert!(
                ReplayForkSource::from_parts(None, &live, Vec::new(), &[], BTreeMap::new(),)
                    .is_err()
            );
        }
        let capture =
            ReplayMemorySource::capture(&live, None, None, Some((&origin(), 0)), &[]).unwrap();
        let source = capture.fork_source(&live).unwrap();
        assert!(source.has_epoch());
        let batch = source.initial_batch("memory-raw-child", 100).unwrap();
        assert!(batch.initial_replay_epoch().is_some());
        fixture
            .store
            .accept_transcript_append_receipt(&batch, 1)
            .unwrap();
        fixture
            .store
            .settle_transcript_append_receipt(&batch, 1)
            .unwrap();
        drop(capture);
        let (_, child) = fixture
            .store
            .load_replay_epoch("memory-raw-child", prefix())
            .unwrap()
            .unwrap();
        assert_eq!(child.head_policy(), ReplayHeadPolicy::IntentionalOmission);
        assert_eq!(child.revision(), live.revision());
        assert_eq!(child.members(), live.members());
        assert_eq!(child.context().chronology(), live.context().chronology());
        assert!(child.head().is_none());
    }
}

/// Optional-head decoding cannot turn a malformed older summary into raw mode,
/// nor can a resealed new header discard its required source qualification.
#[test]
fn durable_raw_tail_rejects_resealed_policy_head_and_source_mismatches() {
    let fixture = Fixture::new();
    let mut live = source();
    live.append_fact("tail", "kept current member").unwrap();
    let proposal = prepare_raw(&fixture, &live, None, Some(1));
    let mutations: [fn(&mut serde_json::Value); 10] = [
        |m| {
            m.as_object_mut().unwrap().remove("head_policy");
        },
        |m| m["head_policy"] = serde_json::json!("summary"),
        |m| m["head"] = serde_json::json!(ContextBlockOccurrenceId::allocate()),
        |m| m["version"] = serde_json::json!(10),
        |m| {
            m["version"] = serde_json::json!(10);
            m.as_object_mut().unwrap().remove("head_policy");
        },
        |m| m["parent_head"] = serde_json::json!(ContextBlockOccurrenceId::allocate()),
        |m| m["covered"] = serde_json::json!([]),
        |m| m["covered"] = serde_json::json!([ContextBlockOccurrenceId::allocate()]),
        |m| m["frozen_arrival_frontier"] = serde_json::json!(0),
        |m| m["checkpoint"]["policy"] = serde_json::json!("summary"),
    ];
    for mutate in mutations {
        let mut value = serde_json::to_value(&proposal.envelope).unwrap();
        mutate(&mut value["manifest"]);
        let mut changed: Envelope = serde_json::from_value(value).unwrap();
        changed.sha256 = digest(&encode(&changed.manifest).unwrap());
        assert!(
            ReplayEpochPublication::validate_candidate(&changed, &proposal.payloads, prefix())
                .is_err()
        );
    }
}

/// Real archived rows survive raw publication byte-for-byte. The user-facing
/// edit transaction must then revise the FIRST raw member, rather than skip it
/// as though it were a model head or treat its original as covered history.
#[test]
fn durable_raw_tail_preserves_archive_and_edits_the_first_retained_member() {
    use super::raw_edits::{CID, load, save, saved_raw};
    let (fixture, row) = saved_raw();
    let archived = fixture.store.inspect(CID).unwrap();
    let (parent, owner) = load(&fixture);
    let proposal = prepare_raw(&fixture, &owner, Some(parent), None);
    fixture.store.publish_replay_epoch(&proposal).unwrap();
    assert_eq!(fixture.store.inspect(CID).unwrap(), archived);
    let (before, owner) = load(&fixture);
    assert!(owner.head().is_none());
    let first = owner.members()[0].occurrence_id();
    let saved = save(&fixture, &row, "edited first raw member é😀").unwrap();
    assert!(matches!(
        saved,
        crate::storage::transcript::CompareAndSwapTranscriptEntryResult::Updated(_)
    ));
    let (after, current) = load(&fixture);
    assert_eq!(before.epoch_revision, after.epoch_revision);
    assert_eq!(after.revision, before.revision + 1);
    assert_eq!(current.head_policy(), ReplayHeadPolicy::IntentionalOmission);
    assert!(current.head().is_none());
    assert_eq!(current.members()[0].occurrence_id(), first);
    assert_eq!(
        current.context().chronology()[0].block().content,
        "edited first raw member é😀"
    );
}

/// An optional Rust field must preserve the original scalar JSON encoding and
/// receipt digest for complete older headed manifests; it must not add a null
/// policy field merely because the current reader knows that field.
#[test]
fn durable_raw_tail_preserves_legacy_headed_manifest_wire_digests() {
    let fixture = Fixture::new();
    let proposal = proposal(&fixture, &source(), None, "actual model summary");
    fixture.store.publish_replay_epoch(&proposal).unwrap();
    let directory = fixture.root.join("conversation-a");
    for version in 6..=10 {
        let mut legacy = proposal.envelope.clone();
        legacy.manifest.version = version;
        legacy.manifest.head_policy = None;
        legacy.sha256 = digest(&encode(&legacy.manifest).unwrap());
        let bytes = encode(&legacy).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(value["manifest"].get("head_policy").is_none());
        assert!(value["manifest"]["head"].is_string());
        let decoded: Envelope = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(encode(&decoded).unwrap(), bytes);
        assert_eq!(decoded.sha256, digest(&encode(&decoded.manifest).unwrap()));
        ReplayEpochPublication::validate_candidate(&decoded, &proposal.payloads, prefix()).unwrap();
        payload::publish(&directory, &digest(&bytes), &bytes, || Ok(())).unwrap();
        fs::write(directory.join(MANIFEST_NAME), &bytes).unwrap();
        let (_, owner) = fixture.load();
        assert_eq!(owner.head_policy(), ReplayHeadPolicy::Summary);
        assert!(owner.head().is_some());
    }
}
