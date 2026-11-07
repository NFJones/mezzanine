//! Memory capture binds descriptive source without inventing storage identity.
//! Integration tests separately exercise actual NoStore rewrites and fork gates.

use super::*;

fn owner() -> ReplayEpochContext {
    let source = source();
    let prepared = source
        .prepare_compaction_publication(
            plan(&source),
            ReplayHeadPublication::Summary("memory head".into()),
        )
        .unwrap();
    let mut owner = prepared.candidate().clone();
    owner.append_fact("tail", "same raw bytes").unwrap();
    owner
}

#[test]
fn memory_capture_preserves_original_producers_and_refuses_unknown_inherited_source() {
    let mut owner = owner();
    let first = origin();
    let boundary = owner.context().event_sequence_high_water_mark();
    assert!(
        ReplayMemorySource::capture(&owner, None, None, Some((&first, boundary)), &[]).is_err()
    );
    let captured = ReplayMemorySource::capture(&owner, None, None, Some((&first, 0)), &[]).unwrap();
    owner.append_fact("tail", "same raw bytes").unwrap();
    assert!(captured.fork_source(&owner).is_err());
    let later = ReplaySourceOrigin::new("later-turn", "later-agent", "later-pane").unwrap();
    let next =
        ReplayMemorySource::capture(&owner, Some(&captured), None, Some((&later, boundary)), &[])
            .unwrap();
    let fork = next.fork_source(&owner).unwrap();
    assert_eq!(fork.rows().len(), 2);
    assert_eq!(fork.rows()[0].turn_id, "original-turn");
    assert_eq!(fork.rows()[1].turn_id, "later-turn");
    let a = mez_agent::transcript::TranscriptContextRecord::from_transcript_content(
        &fork.rows()[0].content,
    )
    .unwrap()
    .unwrap();
    let b = mez_agent::transcript::TranscriptContextRecord::from_transcript_content(
        &fork.rows()[1].content,
    )
    .unwrap()
    .unwrap();
    assert_ne!(a.record().occurrence_id(), b.record().occurrence_id());
}

#[test]
fn memory_copy_root_refuses_resealed_durable_claims_and_changed_capture_framing() {
    let owner = owner();
    let source =
        ReplayMemorySource::capture(&owner, None, None, Some((&origin(), 0)), &[]).unwrap();
    let initial = source
        .fork_source(&owner)
        .unwrap()
        .for_target("conversation-b")
        .unwrap();
    assert_eq!(
        initial
            .owner()
            .unwrap()
            .head()
            .unwrap()
            .conversation()
            .as_str(),
        "conversation-b"
    );
    let mutations: [fn(&mut serde_json::Value); 9] = [
        |m| m["version"] = serde_json::json!(8),
        |m| m["memory_copy_origin"]["conversation"] = serde_json::json!("conversation-b"),
        |m| m["memory_copy_origin"]["epoch_revision"] = serde_json::json!(2),
        |m| {
            m["memory_copy_origin"]["head"] =
                serde_json::json!(ContextBlockOccurrenceId::allocate())
        },
        |m| m["covered"] = serde_json::json!([ContextBlockOccurrenceId::allocate()]),
        |m| m["source_checkpoint"]["next_event_sequence"] = serde_json::json!(999999),
        |m| m["memory_copy_origin"] = serde_json::Value::Null,
        |m| m["revision"] = serde_json::json!(2),
        |m| {
            m["copy_origin"] = serde_json::json!({
                "conversation":"conversation-a", "identity": {
                    "epoch_revision":1, "revision":1, "publication":ContextBlockOccurrenceId::allocate(), "sha256":"0".repeat(64),
                }, "head":m["head"].clone(),
            })
        },
    ];
    for mutate in mutations {
        let mut value = serde_json::to_value(&initial).unwrap();
        mutate(&mut value["envelope"]["manifest"]);
        let mut envelope: Envelope = serde_json::from_value(value["envelope"].clone()).unwrap();
        envelope.sha256 = digest(&encode(&envelope.manifest).unwrap());
        value["envelope"] = serde_json::to_value(envelope).unwrap();
        let changed: InitialReplayEpoch = serde_json::from_value(value).unwrap();
        assert!(changed.owner().is_err());
    }
}
