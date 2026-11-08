//! Cold-loader acceptance of the raw-revision proof, independently from the
//! forthcoming archive/edit transaction. These fixtures install fully backed
//! envelopes, never manufacture an ordinary append or controller capability.

use super::*;

fn with_tail() -> (Fixture, Envelope, ContextBlockOccurrenceId) {
    let fixture = Fixture::new();
    let mut owner = source();
    let frozen = plan(&owner);
    owner.append_fact("tail", "original raw bytes").unwrap();
    let selected = owner
        .context()
        .chronology()
        .last()
        .unwrap()
        .record()
        .occurrence_id();
    owner.append_fact("other", "unrelated raw bytes").unwrap();
    let prepared = fixture
        .store
        .prepare_replay_epoch(
            origin().capture(None),
            "conversation-a",
            None,
            &owner,
            frozen,
            ReplayHeadPublication::Summary("original immutable model head".into()),
        )
        .unwrap();
    fixture.store.publish_replay_epoch(&prepared).unwrap();
    (fixture, prepared.envelope, selected)
}

/// Constructs only descriptive proof fixtures. Production content CAS must also
/// own the raw archive, row images, costs, original parent and crash recovery.
fn revised(
    fixture: &Fixture,
    parent: &Envelope,
    id: ContextBlockOccurrenceId,
    content: &str,
) -> (Envelope, BTreeMap<String, String>) {
    let directory = fixture.root.join("conversation-a");
    let owner = parent
        .restore(AgentContext::empty(), |reference| {
            payload::read(&directory, &reference)
        })
        .unwrap();
    let checkpoint =
        super::super::raw_revision::inline_checkpoint(&parent.manifest.checkpoint, &owner)
            .unwrap()
            .revise_raw_member(id, content.into())
            .unwrap();
    let revised_owner = ReplayEpochContext::from_checkpoint(
        ReplayConversationId::new("conversation-a").unwrap(),
        checkpoint.clone(),
        AgentContext::empty(),
    )
    .unwrap();
    let mut payloads = BTreeMap::new();
    let base = match &parent.manifest.raw_revision_origin {
        Some(origin) => origin.base.clone(),
        None => {
            let bytes = String::from_utf8(encode(parent).unwrap()).unwrap();
            let reference = Payload::new(bytes.as_bytes());
            payloads.insert(reference.sha256.clone(), bytes);
            reference
        }
    };
    let mut manifest = parent.manifest.clone();
    manifest.version = 11;
    manifest.revision += 1;
    manifest.publication = ContextBlockOccurrenceId::allocate();
    manifest.expected_parent = Some(parent.identity());
    manifest.parent_head = Some(manifest.head);
    manifest.copy_origin = None;
    manifest.memory_copy_origin = None;
    manifest.prefix_origin = None;
    manifest.raw_revision_origin = Some(super::super::raw_revision::RawRevisionOrigin { base });
    manifest.checkpoint = externalize(checkpoint, &mut payloads).unwrap();
    manifest.source_escrow = manifest.source_escrow.map(|items| {
        items
            .into_iter()
            .map(|item| item.with_revised_payload(&revised_owner))
            .collect()
    });
    let envelope = Envelope {
        sha256: digest(&encode(&manifest).unwrap()),
        manifest,
    };
    (envelope, payloads)
}

fn install(fixture: &Fixture, envelope: &Envelope, payloads: &BTreeMap<String, String>) {
    let directory = fixture.root.join("conversation-a");
    for (hash, content) in payloads {
        payload::publish(&directory, hash, content.as_bytes(), || Ok(())).unwrap();
    }
    let bytes = encode(envelope).unwrap();
    payload::publish(&directory, &digest(&bytes), &bytes, || Ok(())).unwrap();
    fs::write(directory.join(MANIFEST_NAME), bytes).unwrap();
}

/// A content edit is a storage revision, never another model summary. Repeated
/// edits preserve exact records/order outside the selected raw occurrence and
/// retain one direct original proof instead of a lifetime edit chain.
#[test]
fn raw_revision_cold_replay_preserves_head_and_flattens_repeated_edits() {
    let (fixture, original, selected) = with_tail();
    let old = fixture.load().1;
    let (first, payloads) = revised(&fixture, &original, selected, "revised raw 界 bytes");
    install(&fixture, &first, &payloads);
    let (identity, current) = fixture.load();
    assert_eq!(identity.epoch_revision, original.manifest.epoch_revision);
    assert_eq!(identity.revision, original.manifest.revision + 1);
    assert_eq!(current.members(), old.members());
    assert_eq!(
        current.context().chronology()[0],
        old.context().chronology()[0]
    );
    assert_eq!(
        current.context().chronology()[2],
        old.context().chronology()[2]
    );
    assert_eq!(
        current.context().chronology()[1].record().occurrence_id(),
        selected
    );
    assert_eq!(
        current.context().chronology()[1].block().content,
        "revised raw 界 bytes"
    );
    assert_ne!(
        current.context().chronology()[1].record(),
        old.context().chronology()[1].record()
    );
    let (second, payloads) = revised(&fixture, &first, selected, "second valid edit");
    assert_eq!(
        second.manifest.raw_revision_origin,
        first.manifest.raw_revision_origin
    );
    install(&fixture, &second, &payloads);
    // The intermediate receipt is not required to load the second revision.
    fs::remove_file(
        fixture
            .root
            .join("conversation-a/epoch-payloads")
            .join(digest(&encode(&first).unwrap())),
    )
    .unwrap();
    let (identity, current) = fixture.load();
    assert_eq!(identity.revision, 3);
    assert_eq!(current.revision(), old.revision());
    assert_eq!(current.head(), old.head());
    assert_eq!(
        current.context().chronology()[1].block().content,
        "second valid edit"
    );
}

/// Resealing outer hashes cannot authorize unrelated framing, membership,
/// source/backing or lifecycle changes. Every candidate passes through the real
/// loader, including the original proof and current payload integrity checks.
#[test]
fn raw_revision_rejects_resealed_metadata_and_provenance_changes() {
    let (fixture, original, selected) = with_tail();
    let (edited, payloads) = revised(&fixture, &original, selected, "edited payload");
    install(&fixture, &edited, &payloads);
    let mutations: [fn(&mut serde_json::Value); 15] = [
        |m| m["version"] = serde_json::json!(10),
        |m| m["epoch_revision"] = serde_json::json!(2),
        |m| m["parent_head"] = serde_json::json!(ContextBlockOccurrenceId::allocate()),
        |m| m["expected_parent"]["sha256"] = serde_json::json!("0".repeat(64)),
        |m| m["raw_revision_origin"] = serde_json::Value::Null,
        |m| m["checkpoint"]["next_event_sequence"] = serde_json::json!(999),
        |m| m["checkpoint"]["events"][1]["block"]["label"] = serde_json::json!("changed framing"),
        |m| m["checkpoint"]["events"][1]["retention"] = serde_json::json!("exact"),
        |m| m["checkpoint"]["members"].as_array_mut().unwrap().reverse(),
        |m| m["source_checkpoint"]["next_event_sequence"] = serde_json::json!(999),
        |m| m["source_escrow"][1]["origin"]["agent_id"] = serde_json::json!("foreign-agent"),
        |m| m["source_escrow"][1]["archived"] = serde_json::json!(true),
        |m| m["covered"] = serde_json::json!([]),
        |m| m["frozen_arrival_frontier"] = serde_json::json!(999),
        |m| {
            m["archive_cut"] = serde_json::json!({
                "through_sequence":1,"byte_offset":1,"prefix_sha256":"0".repeat(64),
                "anchor_sha256":"0".repeat(64),"last_row":{
                    "sequence":1,"origin":ContextBlockOccurrenceId::allocate(),"digest":"0".repeat(64)
                }
            })
        },
    ];
    for (index, mutate) in mutations.into_iter().enumerate() {
        let mut value = serde_json::to_value(&edited.manifest).unwrap();
        mutate(&mut value);
        let manifest: Manifest = serde_json::from_value(value).unwrap();
        let altered = Envelope {
            sha256: digest(&encode(&manifest).unwrap()),
            manifest,
        };
        install(&fixture, &altered, &BTreeMap::new());
        assert!(
            altered
                .restore(AgentContext::empty(), |reference| {
                    payload::read(&fixture.root.join("conversation-a"), &reference)
                })
                .is_err(),
            "pure proof mutation {index}"
        );
        assert!(
            fixture
                .store
                .load_replay_epoch("conversation-a", prefix())
                .is_err(),
            "mutation {index}"
        );
    }
}

/// Both the original receipt and its current raw bytes remain required, but
/// retired covered source stays outside the read set even after a valid edit.
#[test]
fn raw_revision_requires_original_current_proof_without_loading_covered_payloads() {
    let (fixture, original, selected) = with_tail();
    let covered = original.manifest.source_escrow.as_ref().unwrap()[0]
        .payload()
        .clone();
    let (edited, payloads) = revised(&fixture, &original, selected, "edited payload");
    install(&fixture, &edited, &payloads);
    // This fixture has unsettled covered backing, which remains required by
    // the normal escrow verifier. Verify only the edit proof's exact read set.
    let directory = fixture.root.join("conversation-a");
    let mut resolved = BTreeSet::new();
    edited
        .restore(AgentContext::empty(), |reference| {
            resolved.insert(reference.sha256.clone());
            payload::read(&directory, &reference)
        })
        .unwrap();
    assert!(!resolved.contains(&covered.sha256));
    let base = &edited.manifest.raw_revision_origin.as_ref().unwrap().base;
    fs::remove_file(directory.join("epoch-payloads").join(&base.sha256)).unwrap();
    assert!(
        fixture
            .store
            .load_replay_epoch("conversation-a", prefix())
            .is_err()
    );
    install(&fixture, &edited, &payloads);
    let base_bytes = payload::read(&directory, base).unwrap();
    fs::write(
        directory.join("epoch-payloads").join(&base.sha256),
        format!("{base_bytes} "),
    )
    .unwrap();
    assert!(
        fixture
            .store
            .load_replay_epoch("conversation-a", prefix())
            .is_err()
    );
}

/// The next genuine model rewrite advances the canonical epoch once and uses
/// the edited storage identity as its CAS parent. Forks independently back the
/// edited body and do not inherit a dependency on the parent's edit receipt.
#[test]
fn raw_revision_supports_next_compaction_and_independently_backed_copy() {
    let (fixture, original, selected) = with_tail();
    let (edited, payloads) = revised(&fixture, &original, selected, "edited payload");
    install(&fixture, &edited, &payloads);
    let (identity, owner) = fixture.load();
    {
        let _source = fixture
            .store
            .acquire_conversation_lock("conversation-a")
            .unwrap();
        let _target = fixture
            .store
            .acquire_conversation_lock("conversation-b")
            .unwrap();
        let copy = fixture
            .store
            .prepare_replay_copy_locked("conversation-a", "conversation-b", &[])
            .unwrap()
            .unwrap();
        let (copy, payloads) = copy.into_envelope(cursor::ArchiveCut::empty()).unwrap();
        assert_eq!(
            copy.manifest.copy_origin.as_ref().unwrap().identity,
            identity
        );
        assert!(copy.manifest.raw_revision_origin.is_none());
        assert!(
            !payloads.contains_key(
                &edited
                    .manifest
                    .raw_revision_origin
                    .as_ref()
                    .unwrap()
                    .base
                    .sha256
            )
        );
        let copied =
            ReplayEpochPublication::validate_candidate(&copy, &payloads, AgentContext::empty())
                .unwrap();
        assert_eq!(
            copied.context().chronology()[1].block().content,
            "edited payload"
        );
    }
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
    // Exercise the actual saved-fork transaction and its append accounting,
    // beyond the prepared-copy proof above. Parent rewrite/deletion must not
    // leave the saved child's edited source dependent on the original receipt.
    fixture
        .store
        .fork("conversation-a", "saved-child", 123)
        .unwrap();
    let next = fixture
        .store
        .prepare_replay_epoch(
            origin().capture(Some(&snapshot)),
            "conversation-a",
            Some(identity),
            &owner,
            plan(&owner),
            ReplayHeadPublication::Summary("next genuine model head".into()),
        )
        .unwrap();
    fixture.store.publish_replay_epoch(&next).unwrap();
    let (identity, current) = fixture.load();
    assert_eq!(identity.epoch_revision, 2);
    assert_eq!(identity.revision, 3);
    assert_eq!(
        current.context().chronology()[0].block().content,
        "next genuine model head"
    );
    assert!(next.envelope.manifest.raw_revision_origin.is_none());
    fixture.store.delete("conversation-a").unwrap();
    let (_, child) = fixture
        .store
        .load_replay_epoch("saved-child", prefix())
        .unwrap()
        .unwrap();
    assert_eq!(child.context().chronology(), owner.context().chronology());
    assert_eq!(child.members(), owner.members());
    assert_eq!(child.head().unwrap().conversation().as_str(), "saved-child");
}

/// An edit chain is never a substitute for the direct original receipt. A new
/// valid summary body with a correctly revised estimate is also forbidden even
/// when the attacker reseals all changed references and envelope hashes.
#[test]
fn raw_revision_rejects_nested_edit_proofs_and_replaced_head_content() {
    let (fixture, original, selected) = with_tail();
    let (edited, payloads) = revised(&fixture, &original, selected, "edited payload");
    install(&fixture, &edited, &payloads);
    let mut nested = edited.clone();
    nested.manifest.revision += 1;
    nested.manifest.publication = ContextBlockOccurrenceId::allocate();
    nested.manifest.expected_parent = Some(edited.identity());
    nested.manifest.raw_revision_origin.as_mut().unwrap().base =
        Payload::new(&encode(&edited).unwrap());
    nested.sha256 = digest(&encode(&nested.manifest).unwrap());
    install(&fixture, &nested, &BTreeMap::new());
    assert!(
        fixture
            .store
            .load_replay_epoch("conversation-a", prefix())
            .is_err()
    );

    install(&fixture, &edited, &BTreeMap::new());
    let original = fixture.load().1;
    let mut head = original.context().chronology()[0].block().clone();
    head.content = "substituted but usable summary".into();
    let record = original.context().chronology()[0]
        .record()
        .revised(&head)
        .unwrap();
    let reference = Payload::new(head.content.as_bytes());
    let mut value = serde_json::to_value(&edited.manifest).unwrap();
    value["checkpoint"]["events"][0]["block"]["content"] =
        serde_json::to_value(&reference).unwrap();
    value["checkpoint"]["events"][0]["record"] = serde_json::to_value(record).unwrap();
    let manifest: Manifest = serde_json::from_value(value).unwrap();
    let altered = Envelope {
        sha256: digest(&encode(&manifest).unwrap()),
        manifest,
    };
    let payloads = BTreeMap::from([(reference.sha256, head.content)]);
    install(&fixture, &altered, &payloads);
    assert!(
        fixture
            .store
            .load_replay_epoch("conversation-a", prefix())
            .is_err()
    );
}
