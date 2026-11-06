//! Real private files, cross-owner locks and exact-proposal recovery. These
//! synthetic fixtures never query private history or dispatch model/native work.

use super::*;
use mez_agent::{ContextBlock, ContextSourceKind, ReplayCompactionPreparation, ReplayTailPolicy};
use std::{
    path::PathBuf,
    sync::{Arc, Barrier},
};

/// Owns an isolated physical temporary directory on Linux and macOS, removed
/// after every scenario. Source and archive bytes are synthetic and task-owned.
struct Fixture {
    root: PathBuf,
    store: AgentTranscriptStore,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "mez-single-head-{}",
            String::from(ContextBlockOccurrenceId::allocate())
        ));
        Self {
            store: AgentTranscriptStore::new(root.clone()),
            root,
        }
    }

    fn load(&self) -> (ReplayEpochIdentity, ReplayEpochContext) {
        self.store
            .load_replay_epoch("conversation-a", prefix())
            .unwrap()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Supplies current policy independently from saved source. The checkpoint may
/// reuse stable accounting, never the caller's policy or session ownership.
fn prefix() -> AgentContext {
    AgentContext::new(vec![ContextBlock::stable_instruction(
        ContextSourceKind::System,
        "policy",
        "current trusted policy",
    )])
    .unwrap()
}

fn source() -> ReplayEpochContext {
    let mut owner = ReplayEpochContext::new(
        ReplayConversationId::new("conversation-a").unwrap(),
        prefix(),
    )
    .unwrap();
    owner
        .append_fact("source", "original covered bytes")
        .unwrap();
    owner
}

fn plan(owner: &ReplayEpochContext) -> ReplayCompactionPlan {
    match owner
        .prepare_compaction(ReplayTailPolicy::new(1000, 0).unwrap())
        .unwrap()
    {
        ReplayCompactionPreparation::Ready(plan) => plan,
        other => panic!("expected source, got {other:?}"),
    }
}

fn proposal(
    owner: &ReplayEpochContext,
    parent: Option<ReplayEpochIdentity>,
    text: &str,
) -> ReplayEpochPublication {
    ReplayEpochPublication::prepare(
        "conversation-a",
        parent,
        owner,
        plan(owner),
        ReplayHeadPublication::Summary(text.into()),
    )
    .unwrap()
}

/// The very first live source has no ordinary archive. Publication persists
/// head/tail independently, creates only a genuine zero-row archive, and a cold
/// store restores original canonical IDs/costs without fabricating append ACKs.
#[test]
fn first_live_epoch_is_durable_without_ordinary_append() {
    let fixture = Fixture::new();
    let mut owner = source();
    let frozen = plan(&owner);
    owner
        .append_fact("arrival", "retained original bytes")
        .unwrap();
    let unchanged = owner.clone();
    let prepared = ReplayEpochPublication::prepare(
        "conversation-a",
        None,
        &owner,
        frozen,
        ReplayHeadPublication::Summary("original model head".into()),
    )
    .unwrap();
    assert!(!fixture.root.join("conversation-a").exists());
    let expected = ReplayEpochPublication::validate_candidate(
        &prepared.envelope,
        &prepared.payloads,
        prefix(),
    )
    .unwrap();
    let identity = fixture
        .store
        .publish_replay_epoch(&prepared)
        .unwrap()
        .identity()
        .clone();
    assert_eq!(owner, unchanged);
    assert_eq!(identity, prepared.identity());
    let directory = fixture.root.join("conversation-a");
    assert_eq!(
        payload::read(&directory, &Payload::new(b"original covered bytes")).unwrap(),
        "original covered bytes"
    );
    let audit = prepared
        .envelope
        .manifest
        .source_checkpoint
        .clone()
        .try_map_payloads(|_, reference| payload::read(&directory, &reference))
        .unwrap();
    let audited = ReplayEpochContext::from_checkpoint(
        ReplayConversationId::new("conversation-a").unwrap(),
        audit,
        prefix(),
    )
    .unwrap();
    assert_eq!(audited.context(), owner.context());
    assert!(fixture.store.inspect("conversation-a").unwrap().is_empty());
    assert_eq!(fixture.store.next_sequence("conversation-a").unwrap(), 1);
    let cold = AgentTranscriptStore::new(fixture.root.clone());
    let (loaded, restored) = cold
        .load_replay_epoch("conversation-a", prefix())
        .unwrap()
        .unwrap();
    assert_eq!(loaded, identity);
    assert_eq!(restored.context(), expected.context());
    assert_eq!(restored.members(), expected.members());
    assert_eq!(restored.input_estimates(), expected.input_estimates());
    assert_eq!(
        restored.members()[0].occurrence_id(),
        owner.members()[1].occurrence_id()
    );
    let manifest =
        fs::read_to_string(fixture.root.join("conversation-a").join(MANIFEST_NAME)).unwrap();
    assert!(!manifest.contains("original model head"));
    assert!(!manifest.contains("retained original bytes"));
    assert!(!manifest.contains("current trusted policy"));
    assert!(fixture.store.compaction_epoch("conversation-a").is_err());
    assert!(
        fixture
            .store
            .save_compaction_epoch("conversation-a", 0, "old writer")
            .is_err()
    );
    assert_eq!(fixture.load().0, identity);
}

/// A storage worker may commit before its reply is observed. Retrying the same
/// retained proposal obtains a real ACK for that head, adopts it without cold
/// reconstruction, and carries an intervening arrival into a second real
/// publication. Wrong store/proposal/body cannot consume that receipt.
#[test]
fn acknowledged_head_adopts_live_arrivals_and_settles_lost_reply() {
    let fixture = Fixture::new();
    let owner = source();
    let prepared = proposal(&owner, None, "exact first head");
    let mut body = owner.into_owned_context();
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let _lease = fixture
                    .store
                    .acquire_conversation_lock("conversation-a")
                    .unwrap();
                fixture
                    .store
                    .publish_replay_epoch_locked(&prepared, |phase| {
                        if phase == Phase::BeforeAcknowledgment {
                            Err(invalid("injected lost reply after commit"))
                        } else {
                            Ok(())
                        }
                    })
            })
            .join()
            .unwrap()
            .unwrap_err();
    });
    body.append_runtime_fact_event("arrival", "received while worker was running")
        .unwrap();
    let arrival = body.chronology()[1].clone();
    let ack = std::thread::scope(|scope| {
        scope
            .spawn(|| fixture.store.publish_replay_epoch(&prepared))
            .join()
            .unwrap()
            .unwrap()
    });
    assert_eq!(ack.identity(), &prepared.identity());
    let other = Fixture::new();
    let before = body.clone();
    assert!(ack.adopt(&other.store, &prepared, &mut body).is_err());
    assert_eq!(body, before);
    let competing = proposal(
        prepared.prepared.source(),
        None,
        "same epoch different proposal",
    );
    assert!(ack.adopt(&fixture.store, &competing, &mut body).is_err());
    assert_eq!(body, before);
    let mut plain = prefix();
    let plain_before = plain.clone();
    assert!(ack.adopt(&fixture.store, &prepared, &mut plain).is_err());
    assert_eq!(plain, plain_before);
    let mut cold = ReplayEpochContext::from_checkpoint(
        ReplayConversationId::new("conversation-a").unwrap(),
        prepared.prepared.source().checkpoint().unwrap(),
        prefix(),
    )
    .unwrap()
    .into_owned_context();
    let cold_before = cold.clone();
    assert!(ack.adopt(&fixture.store, &prepared, &mut cold).is_err());
    assert_eq!(cold, cold_before);
    ack.adopt(&fixture.store, &prepared, &mut body).unwrap();
    assert_eq!(
        body.chronology()[0],
        prepared.prepared.candidate().context().chronology()[0]
    );
    assert_eq!(body.chronology()[1], arrival);
    let before = body.clone();
    ack.adopt(&fixture.store, &prepared, &mut body).unwrap();
    assert_eq!(body, before);
    let live = ReplayEpochContext::from_owned_context(
        ReplayConversationId::new("conversation-a").unwrap(),
        body,
    )
    .unwrap();
    assert_eq!(live.members()[0].born_in().get(), 0);
    let second = proposal(&live, Some(ack.identity().clone()), "exact second head");
    let mut body = live.into_owned_context();
    let second_ack = fixture.store.publish_replay_epoch(&second).unwrap();
    second_ack
        .adopt(&fixture.store, &second, &mut body)
        .unwrap();
    let before = body.clone();
    assert!(ack.adopt(&fixture.store, &prepared, &mut body).is_err());
    assert_eq!(body, before);
    let (identity, loaded) = fixture.load();
    assert_eq!(&identity, second_ack.identity());
    assert_eq!(loaded.context().chronology(), body.chronology());
    assert!(loaded.members().is_empty());
    assert!(fixture.store.inspect("conversation-a").unwrap().is_empty());
}

/// Every named I/O boundary preserves complete old or new authority. Retrying
/// the retained exact proposal settles the committed truth, including a failed
/// callback after swap, without allocating a new summary/publication identity.
#[test]
fn every_publication_fault_preserves_old_or_new_and_exact_retry() {
    for old_exists in [false, true] {
        for fault in [
            Phase::BeforePayloadSync,
            Phase::BeforeSourceReceiptSync,
            Phase::BeforeManifestSwap,
            Phase::AfterManifestSwap,
            Phase::BeforeRequiredMarker,
            Phase::BeforeAcknowledgment,
        ] {
            let fixture = Fixture::new();
            let (parent, mut owner) = if old_exists {
                let first = proposal(&source(), None, "old head");
                fixture.store.publish_replay_epoch(&first).unwrap();
                let (id, mut owner) = fixture.load();
                owner.append_fact("new", "new closed source").unwrap();
                (Some(id), owner)
            } else {
                (None, source())
            };
            let frozen = plan(&owner);
            owner
                .append_fact("arrival", "post-frontier original bytes")
                .unwrap();
            let prepared = ReplayEpochPublication::prepare(
                "conversation-a",
                parent.clone(),
                &owner,
                frozen,
                ReplayHeadPublication::Summary("next head".into()),
            )
            .unwrap();
            {
                let _lease = fixture
                    .store
                    .acquire_conversation_lock("conversation-a")
                    .unwrap();
                assert!(
                    fixture
                        .store
                        .publish_replay_epoch_locked(&prepared, |phase| {
                            if phase == fault {
                                Err(invalid("injected publication failure"))
                            } else {
                                Ok(())
                            }
                        })
                        .is_err()
                );
            }
            let observed = fixture
                .store
                .load_replay_epoch("conversation-a", prefix())
                .unwrap()
                .map(|value| value.0);
            if matches!(
                fault,
                Phase::BeforePayloadSync
                    | Phase::BeforeSourceReceiptSync
                    | Phase::BeforeManifestSwap
            ) {
                assert_eq!(observed, parent, "fault={fault:?}");
            } else {
                assert_eq!(observed, Some(prepared.identity()), "fault={fault:?}");
            }
            assert_eq!(
                fixture
                    .store
                    .publish_replay_epoch(&prepared)
                    .unwrap()
                    .identity(),
                &prepared.identity()
            );
            assert_eq!(fixture.load().0, prepared.identity());
            assert_eq!(fixture.load().1.context().chronology().len(), 2);
            assert!(fixture.store.inspect("conversation-a").unwrap().is_empty());
        }
    }
}

/// Repeated equal payload text keeps distinct original occurrences, while
/// immutable files are reused. New epochs read only the latest member set;
/// older payloads remain audit backing, never extra model-visible raw history.
#[test]
fn three_epochs_reuse_payloads_without_collapsing_occurrences() {
    use std::os::unix::fs::MetadataExt;
    let fixture = Fixture::new();
    let mut owner = source();
    let mut parent = None;
    let mut reused_inode = None;
    let mut first_receipt = None;
    for revision in 1..=3 {
        let frozen = plan(&owner);
        owner.append_fact("arrival", "equal payload bytes").unwrap();
        owner.append_fact("arrival", "equal payload bytes").unwrap();
        let prepared = ReplayEpochPublication::prepare(
            "conversation-a",
            parent,
            &owner,
            frozen,
            ReplayHeadPublication::Summary(format!("head {revision}")),
        )
        .unwrap();
        fixture.store.publish_replay_epoch(&prepared).unwrap();
        if first_receipt.is_none() {
            first_receipt = Some(Payload::new(&encode(&prepared.envelope).unwrap()));
        }
        let (identity, restored) = fixture.load();
        assert_eq!(restored.revision().get(), revision);
        assert_eq!(restored.members().len(), 2);
        assert_ne!(
            restored.members()[0].occurrence_id(),
            restored.members()[1].occurrence_id()
        );
        assert_eq!(restored.members()[0].born_in().get(), revision - 1);
        assert_eq!(restored.context().chronology().len(), 3);
        let payload = fixture
            .root
            .join("conversation-a/epoch-payloads")
            .join(digest(b"equal payload bytes"));
        let inode = fs::metadata(payload).unwrap().ino();
        assert_eq!(*reused_inode.get_or_insert(inode), inode);
        assert!(
            !restored
                .context()
                .chronology()
                .iter()
                .any(|event| event.block().content == "original covered bytes")
        );
        parent = Some(identity);
        owner = restored;
    }
    let directory = fixture.root.join("conversation-a");
    let audit: Envelope =
        serde_json::from_str(&payload::read(&directory, &first_receipt.unwrap()).unwrap()).unwrap();
    let original = audit
        .manifest
        .source_checkpoint
        .try_map_payloads(|_, reference| payload::read(&directory, &reference))
        .unwrap();
    let original = ReplayEpochContext::from_checkpoint(
        ReplayConversationId::new("conversation-a").unwrap(),
        original,
        prefix(),
    )
    .unwrap();
    assert_eq!(
        original.context().chronology()[0].block().content,
        "original covered bytes"
    );
    assert_eq!(original.context().chronology().len(), 3);
}

/// Cooperating writers use the same real flock. Exactly one first publication
/// wins; a competing parent cannot partially replace it or reuse equal text as
/// a receipt. Late callbacks cannot roll back an already newer committed epoch.
#[test]
fn competing_and_late_publications_are_fenced_by_exact_parent() {
    let fixture = Fixture::new();
    let owner = source();
    let first = proposal(&owner, None, "same model text");
    let competing = proposal(&owner, None, "same model text");
    assert_ne!(first.identity(), competing.identity());
    let barrier = Arc::new(Barrier::new(2));
    let results = std::thread::scope(|scope| {
        let tasks = [&first, &competing].map(|prepared| {
            let barrier = barrier.clone();
            let store = fixture.store.clone();
            scope.spawn(move || {
                barrier.wait();
                store.publish_replay_epoch(prepared)
            })
        });
        tasks.map(|task| task.join().unwrap())
    });
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    let winner = if results[0].is_ok() {
        &first
    } else {
        &competing
    };
    assert_eq!(
        fixture
            .store
            .publish_replay_epoch(winner)
            .unwrap()
            .identity(),
        &winner.identity()
    );
    let (identity, mut owner) = fixture.load();
    owner.append_fact("next", "new source").unwrap();
    let second = proposal(&owner, Some(identity), "second head");
    fixture.store.publish_replay_epoch(&second).unwrap();
    assert!(fixture.store.publish_replay_epoch(winner).is_err());
    assert_eq!(fixture.load().0, second.identity());
}

/// Missing/corrupt current payloads and metadata refuse typed load. A retained
/// exact proposal may restore its own missing payload, while an existing corrupt
/// file is never silently blessed or overwritten. Required sidecar loss cannot
/// fall back to an empty/raw history after the marker was committed.
#[test]
fn damaged_required_source_is_explicit_and_exact_missing_payload_can_recover() {
    let fixture = Fixture::new();
    let prepared = proposal(&source(), None, "actual head bytes");
    fixture.store.publish_replay_epoch(&prepared).unwrap();
    let directory = fixture.root.join("conversation-a");
    let path = directory
        .join("epoch-payloads")
        .join(digest(b"actual head bytes"));
    let original = fs::read(&path).unwrap();
    let mut corrupt = original.clone();
    corrupt[0] ^= 1;
    fs::write(&path, &corrupt).unwrap();
    assert!(
        fixture
            .store
            .load_replay_epoch("conversation-a", prefix())
            .is_err()
    );
    assert!(fixture.store.publish_replay_epoch(&prepared).is_err());
    assert_eq!(fs::read(&path).unwrap(), corrupt);
    fs::remove_file(&path).unwrap();
    assert!(
        fixture
            .store
            .load_replay_epoch("conversation-a", prefix())
            .is_err()
    );
    assert_eq!(
        fixture
            .store
            .publish_replay_epoch(&prepared)
            .unwrap()
            .identity(),
        &prepared.identity()
    );
    assert_eq!(fs::read(path).unwrap(), original);
    let manifest_path = directory.join(MANIFEST_NAME);
    let original = fs::read(&manifest_path).unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
    value["manifest"]["revision"] = serde_json::json!(2);
    fs::write(&manifest_path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(
        fixture
            .store
            .load_replay_epoch("conversation-a", prefix())
            .is_err()
    );
    assert!(fixture.store.publish_replay_epoch(&prepared).is_err());
    fs::write(&manifest_path, &original).unwrap();
    assert_eq!(fixture.load().0, prepared.identity());
    fs::remove_file(manifest_path).unwrap();
    assert!(
        fixture
            .store
            .load_replay_epoch("conversation-a", prefix())
            .is_err()
    );
}

/// Old source is a real existing authority, never an absent expected parent.
/// Until the explicit migration adapter is installed, publication fails visibly
/// without rewriting old format, archive bytes or interpretation.
#[test]
fn legacy_authority_requires_explicit_transition() {
    let fixture = Fixture::new();
    fixture
        .store
        .save_compaction_epoch("conversation-a", 0, "legacy head")
        .unwrap();
    let path = fixture.root.join("conversation-a").join(MANIFEST_NAME);
    let original = fs::read(&path).unwrap();
    let prepared = proposal(&source(), None, "new head");
    assert!(fixture.store.publish_replay_epoch(&prepared).is_err());
    assert!(
        fixture
            .store
            .load_replay_epoch("conversation-a", prefix())
            .is_err()
    );
    assert_eq!(fs::read(path).unwrap(), original);
    assert_eq!(
        fixture
            .store
            .compaction_epoch("conversation-a")
            .unwrap()
            .unwrap()
            .summary,
        "legacy head"
    );
}

/// Publication and payload files are private at creation; integrity names are
/// canonical digest components and cannot become arbitrary filesystem paths.
#[test]
fn payload_paths_bounds_and_permissions_are_checked() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let prepared = proposal(&source(), None, "private head");
    fixture.store.publish_replay_epoch(&prepared).unwrap();
    let directory = fixture.root.join("conversation-a");
    assert_eq!(
        fs::metadata(directory.join(MANIFEST_NAME))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(directory.join("epoch-payloads"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    for path in fs::read_dir(directory.join("epoch-payloads")).unwrap() {
        assert_eq!(
            path.unwrap().metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert!(
        payload::read(
            &directory,
            &Payload {
                sha256: "../metadata.json".into(),
                bytes: 1
            }
        )
        .is_err()
    );
    assert!(
        payload::read(
            &directory,
            &Payload {
                sha256: "a".repeat(64),
                bytes: SOURCE_LIMIT + 1
            }
        )
        .is_err()
    );
}

/// Even an integrity-consistent header must agree with actual canonical
/// membership and the frozen frontier. Source from another conversation is
/// rejected during preparation, before any durable mutation can occur.
#[test]
fn header_semantics_and_source_namespace_are_checked() {
    let owner = source();
    assert!(
        ReplayEpochPublication::prepare(
            "foreign",
            None,
            &owner,
            plan(&owner),
            ReplayHeadPublication::Summary("head".into())
        )
        .is_err()
    );
    let mut live = owner;
    let frozen = plan(&live);
    live.append_fact("arrival", "original retained tail")
        .unwrap();
    let prepared = ReplayEpochPublication::prepare(
        "conversation-a",
        None,
        &live,
        frozen,
        ReplayHeadPublication::Summary("head".into()),
    )
    .unwrap();
    for corrupt in 0..5 {
        let mut envelope = prepared.envelope.clone();
        match corrupt {
            0 => envelope.manifest.covered = vec![envelope.manifest.head],
            1 => envelope.manifest.covered = vec![live.members()[1].occurrence_id()],
            2 => envelope.manifest.frozen_arrival_frontier = None,
            3 => envelope.manifest.epoch_revision += 1,
            4 => envelope.manifest.covered = vec![ContextBlockOccurrenceId::allocate()],
            _ => unreachable!(),
        }
        envelope.sha256 = digest(&encode(&envelope.manifest).unwrap());
        assert!(
            ReplayEpochPublication::validate_candidate(&envelope, &prepared.payloads, prefix())
                .is_err()
        );
    }
}

/// A damaged payload entry cannot redirect immutable source reads through a
/// symlink, even when its destination happens to contain matching bytes. The
/// read refuses the affected epoch without touching the destination file.
#[test]
fn required_payload_symlinks_are_refused() {
    let fixture = Fixture::new();
    let prepared = proposal(&source(), None, "private head");
    fixture.store.publish_replay_epoch(&prepared).unwrap();
    let directory = fixture.root.join("conversation-a");
    let path = directory
        .join("epoch-payloads")
        .join(digest(b"private head"));
    let destination = fixture.root.join("other-private-file");
    fs::rename(&path, &destination).unwrap();
    std::os::unix::fs::symlink(&destination, &path).unwrap();
    assert!(
        fixture
            .store
            .load_replay_epoch("conversation-a", prefix())
            .is_err()
    );
    assert!(fixture.store.publish_replay_epoch(&prepared).is_err());
    assert_eq!(fs::read(&destination).unwrap(), b"private head");
}

/// The immutable complete source/proposal receipt must exist before a latest
/// manifest can be loaded. Exact retained retry can repair a missing receipt,
/// while a corrupt existing receipt is never silently overwritten.
#[test]
fn immutable_source_receipt_is_required_and_exact_retry_settles_it() {
    let fixture = Fixture::new();
    let prepared = proposal(&source(), None, "durable head");
    fixture.store.publish_replay_epoch(&prepared).unwrap();
    let bytes = encode(&prepared.envelope).unwrap();
    let path = fixture
        .root
        .join("conversation-a/epoch-payloads")
        .join(digest(&bytes));
    fs::remove_file(&path).unwrap();
    assert!(
        fixture
            .store
            .load_replay_epoch("conversation-a", prefix())
            .is_err()
    );
    assert_eq!(
        fixture
            .store
            .publish_replay_epoch(&prepared)
            .unwrap()
            .identity(),
        &prepared.identity()
    );
    assert_eq!(fs::read(&path).unwrap(), bytes);
    let mut corrupt = bytes.clone();
    corrupt[0] ^= 1;
    fs::write(&path, &corrupt).unwrap();
    assert!(
        fixture
            .store
            .load_replay_epoch("conversation-a", prefix())
            .is_err()
    );
    assert!(fixture.store.publish_replay_epoch(&prepared).is_err());
    assert_eq!(fs::read(path).unwrap(), corrupt);
}

/// Cold adoption of a complete post-swap publication must finish its missing
/// required-state marker before exposing the epoch. Subsequent sidecar loss
/// must therefore refuse replay, even though the original writer never ACKed.
#[test]
fn cold_adoption_finishes_required_marker_after_uncertain_swap() {
    let fixture = Fixture::new();
    let prepared = proposal(&source(), None, "committed without ACK");
    {
        let _lease = fixture
            .store
            .acquire_conversation_lock("conversation-a")
            .unwrap();
        assert!(
            fixture
                .store
                .publish_replay_epoch_locked(&prepared, |phase| {
                    if phase == Phase::BeforeRequiredMarker {
                        Err(invalid("lost worker before marker"))
                    } else {
                        Ok(())
                    }
                })
                .is_err()
        );
    }
    let cold = AgentTranscriptStore::new(fixture.root.clone());
    assert_eq!(
        cold.load_replay_epoch("conversation-a", prefix())
            .unwrap()
            .unwrap()
            .0,
        prepared.identity()
    );
    fs::remove_file(fixture.root.join("conversation-a").join(MANIFEST_NAME)).unwrap();
    assert!(cold.load_replay_epoch("conversation-a", prefix()).is_err());
}

/// Captures publication artifacts by relative name and exact bytes. Parent
/// rejection must occur before adding backing, swapping latest or changing audit
/// metadata; merely retaining the prior visible head is insufficient.
fn publication_files(directory: &std::path::Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(
        root: &std::path::Path,
        directory: &std::path::Path,
        files: &mut BTreeMap<PathBuf, Vec<u8>>,
    ) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root).unwrap().to_owned(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    visit(directory, directory, &mut files);
    files
}

/// A valid checkpoint can describe a different branch with the same head UUID
/// and revision. Storage must bind original head/raw bytes, membership and birth
/// revisions to the actual committed parent, not trust a self-consistent rewrite
/// of that branch. Genuine append-only extension remains publishable.
#[test]
fn committed_parent_source_cannot_be_substituted_or_dropped() {
    let fixture = Fixture::new();
    let mut original = source();
    let frozen = plan(&original);
    original
        .append_fact("tail one", "first original retained bytes")
        .unwrap();
    original
        .append_fact("tail two", "second original retained bytes")
        .unwrap();
    let first = ReplayEpochPublication::prepare(
        "conversation-a",
        None,
        &original,
        frozen,
        ReplayHeadPublication::Summary("original committed head".into()),
    )
    .unwrap();
    fixture.store.publish_replay_epoch(&first).unwrap();
    let (parent, owner) = fixture.load();
    let directory = fixture.root.join("conversation-a");
    let before = publication_files(&directory);
    for alteration in 0..4 {
        let mut value = serde_json::to_value(owner.checkpoint().unwrap()).unwrap();
        match alteration {
            0 => {
                value["events"][0]["block"]["content"] =
                    serde_json::json!("substituted committed head")
            }
            1 => {
                value["events"][1]["block"]["content"] =
                    serde_json::json!("substituted retained bytes")
            }
            2 => {
                value["events"].as_array_mut().unwrap().remove(1);
                value["members"].as_array_mut().unwrap().remove(0);
            }
            3 => value["members"][0]["born_in"] = serde_json::json!(1),
            _ => unreachable!(),
        }
        let altered = ReplayEpochContext::from_checkpoint(
            ReplayConversationId::new("conversation-a").unwrap(),
            serde_json::from_value(value).unwrap(),
            prefix(),
        )
        .unwrap();
        let branch = proposal(
            &altered,
            Some(parent.clone()),
            "self-consistent branch head",
        );
        assert!(
            fixture.store.publish_replay_epoch(&branch).is_err(),
            "accepted altered committed source case {alteration}"
        );
        assert_eq!(publication_files(&directory), before);
        assert_eq!(fixture.load().0, parent);
    }
    let mut extended = owner;
    extended
        .append_fact("genuine arrival", "actual extension bytes")
        .unwrap();
    let next = proposal(&extended, Some(parent), "genuine second epoch");
    fixture.store.publish_replay_epoch(&next).unwrap();
    assert_eq!(fixture.load().0, next.identity());
}

/// The committed-prefix fence permits actual typed lifecycle changes while
/// refusing identical source metadata reimported without transition witnesses.
/// Original protected instructions/peer occurrences stay exact until the live
/// controller completes or hands them off; checkpoint text grants neither.
#[test]
fn actual_lifecycle_extension_is_accepted_but_deserialized_claims_are_refused() {
    let fixture = Fixture::new();
    let mut original = source();
    let frozen = plan(&original);
    let task = original.begin_instruction_task().unwrap();
    let instruction = original
        .append_instruction(
            &task,
            ContextBlock::user_event("user prompt", "active exact instruction"),
        )
        .unwrap();
    let peer = original
        .append_pending_peer("peer", "original peer reference")
        .unwrap();
    let first = ReplayEpochPublication::prepare(
        "conversation-a",
        None,
        &original,
        frozen,
        ReplayHeadPublication::Summary("prior head".into()),
    )
    .unwrap();
    fixture.store.publish_replay_epoch(&first).unwrap();
    let (parent, mut current) = fixture.load();
    let live_task = current.begin_instruction_task().unwrap();
    current
        .bind_instruction_occurrences(&live_task, &[instruction])
        .unwrap();
    current.complete_instruction_task(&live_task).unwrap();
    assert!(current.release_peer(peer).unwrap());
    let imported = ReplayEpochContext::from_checkpoint(
        ReplayConversationId::new("conversation-a").unwrap(),
        current.checkpoint().unwrap(),
        prefix(),
    )
    .unwrap();
    assert_eq!(imported.context(), current.context());
    let before = publication_files(&fixture.root.join("conversation-a"));
    let claimed = proposal(
        &imported,
        Some(parent.clone()),
        "unwitnessed lifecycle claim",
    );
    assert!(fixture.store.publish_replay_epoch(&claimed).is_err());
    assert_eq!(
        publication_files(&fixture.root.join("conversation-a")),
        before
    );
    let actual = proposal(&current, Some(parent), "actual completed lifecycle");
    fixture.store.publish_replay_epoch(&actual).unwrap();
    assert_eq!(fixture.load().0, actual.identity());
}
