//! Actual parent effects through actor, producer, canonical facts and storage.
//!
//! Real commits supply every confirmation. Directory facts are distinct from
//! file deltas, remain after cancellation, and survive checked physical archive
//! settlement and exact original producer transfer without mutation replay.

use super::async_producer::ActorFixture;
use super::*;
use crate::host::async_runtime::native_filesystem::{
    claim_and_start_native_filesystem_producer, project_joined_native_filesystem_facts,
    retain_joined_native_filesystem_outcome, seal_joined_native_filesystem_archive,
    transfer_joined_native_filesystem_producer,
};
use crate::runtime::processes::native_filesystem_producer::NativeFilesystemProducer;

/// Actual async root/journal/canonical projection and ordered writer retain both
/// directory and file facts. Only the two files change file statistics; duplicate
/// projection and transfer cannot repeat either filesystem or transcript effects.
#[tokio::test(flavor = "current_thread")]
async fn parent_effects_survive_actual_producer_progress_and_checked_durable_transfer() {
    let mut fixture = Fixture::with_patch(false, |root| {
        format!(
            "*** Begin Patch\n*** Add File: {}\n+first\n*** Add File: {}\n+second\n*** End Patch",
            root.join("one/two/a").display(),
            root.join("one/two/b").display()
        )
    });
    let store = AgentTranscriptStore::new(fixture.root.join("archive"));
    fixture.service.set_agent_transcript_store(store.clone());
    fixture.service.persistence.enable_transcript_adapter();
    let attempt = fixture.queue();
    let producer = NativeFilesystemProducer::new(attempt.clone());
    let actor = ActorFixture::new(fixture);
    assert!(
        claim_and_start_native_filesystem_producer(actor.handle.clone(), &producer)
            .await
            .unwrap()
    );
    let raw = retain_joined_native_filesystem_outcome(&actor.handle, &producer)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        raw.result.certainty,
        NativeEffectCertainty::Confirmed,
        "{:?}",
        raw.result.diagnostic
    );
    assert_eq!(raw.receipts.len(), 4);
    producer.validate_confirmed_outcome(&raw).unwrap();
    let delivery = producer.progress_snapshot().unwrap();
    assert_eq!(delivery.confirmed().unwrap().len(), 4);
    project_joined_native_filesystem_facts(&actor.handle, &producer)
        .await
        .unwrap()
        .unwrap();
    super::archive_storage::checked(&actor, &producer).await;
    super::archive_storage::persist(&actor).await;
    let sealed = seal_joined_native_filesystem_archive(&actor.handle, &producer)
        .await
        .unwrap()
        .unwrap();
    assert!(
        actor
            .handle
            .validate_native_filesystem_archive_settlement(sealed)
            .await
            .unwrap()
    );
    let ack = transfer_joined_native_filesystem_producer(&actor.handle, &producer)
        .await
        .unwrap()
        .unwrap();
    assert!(ack.acknowledges(&producer));
    assert!(
        transfer_joined_native_filesystem_producer(&actor.handle, &producer)
            .await
            .unwrap()
            .unwrap()
            .acknowledges(&producer)
    );
    let mut fixture = actor.finish().await;
    let result = &fixture.service.agent_turn_executions()[&fixture.turn.turn_id].action_results[0];
    assert_eq!(result.status, mez_agent::ActionStatus::Succeeded);
    let data: serde_json::Value =
        serde_json::from_str(result.structured_content_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        data["confirmed_endpoints"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["kind"] == "directory_created")
            .count(),
        2
    );
    let modified = fixture.service.retained_agent_modified_files("%1").unwrap();
    assert_eq!(modified.len(), 2);
    assert_eq!(
        modified[fixture.root.join("one/two/a").to_str().unwrap()].added,
        1
    );
    assert!(!modified.contains_key(fixture.root.join("one").to_str().unwrap()));
    assert!(!modified.contains_key(fixture.root.join("one/two").to_str().unwrap()));
    assert_eq!(super::canonical::result_events(&fixture), 1);
    assert!(
        !fixture
            .service
            .apply_native_filesystem_progress(&delivery)
            .unwrap()
    );
    assert_eq!(
        fs::read(fixture.root.join("one/two/a")).unwrap(),
        b"first\n"
    );
    let rows = store.inspect(&fixture.turn.conversation_id).unwrap();
    // Ordinary display and exact execution history each retain one original
    // result; counting substrings across both would mistake preservation for replay.
    for role in [
        mez_agent::transcript::TranscriptRole::Tool,
        mez_agent::transcript::TranscriptRole::System,
    ] {
        assert_eq!(
            rows.iter()
                .filter(|row| row.role == role && row.content.contains("directory_created"))
                .count(),
            1,
            "duplicate or missing directory facts for {role:?}"
        );
    }
    assert!(
        fixture
            .service
            .retained_native_filesystem_outcome(&attempt)
            .is_none()
    );
}

/// Cancellation after the first real directory commit must yield a canonical
/// failed/partial action containing that directory fact, no file counts, and one
/// original context event. Reprojection cannot erase or repeat the effect.
#[test]
fn canonical_parent_only_partial_is_not_file_success() {
    let mut fixture = Fixture::with_patch(false, |root| {
        format!(
            "*** Begin Patch\n*** Add File: {}\n+data\n*** End Patch",
            root.join("parent/file").display()
        )
    });
    let (attempt, raw) = super::canonical::execute_and_retain(&mut fixture, true);
    assert_eq!(raw.result.certainty, NativeEffectCertainty::Partial);
    assert_eq!(raw.receipts.len(), 1);
    assert!(!raw.receipts[0].is_file());
    assert!(
        fixture
            .service
            .project_native_filesystem_completion(&attempt)
            .unwrap()
    );
    let result = &fixture.service.agent_turn_executions()[&fixture.turn.turn_id].action_results[0];
    assert_eq!(result.status, mez_agent::ActionStatus::Failed);
    let data: serde_json::Value =
        serde_json::from_str(result.structured_content_json.as_deref().unwrap()).unwrap();
    assert_eq!(data["effect_certainty"], "partial");
    assert_eq!(data["confirmed_endpoints"][0]["kind"], "directory_created");
    assert!(
        fixture
            .service
            .retained_agent_modified_files("%1")
            .is_none_or(|files| files.is_empty())
    );
    assert!(fixture.root.join("parent").is_dir());
    assert!(!fixture.root.join("parent/file").exists());
    assert!(
        !fixture
            .service
            .project_native_filesystem_completion(&attempt)
            .unwrap()
    );
    assert_eq!(super::canonical::result_events(&fixture), 1);
}

/// Physical parent facts retain their real resolved path even when the authored
/// file traverses an existing symlink alias. The alias grants no outside access
/// and the canonical result preserves exact authored destination attribution.
#[test]
fn canonical_parent_fact_preserves_resolved_alias_relationship() {
    use std::os::unix::fs::symlink;
    let mut fixture = Fixture::with_context(
        false,
        |root| {
            vec![mez_agent::AgentAction {
                id: "native-file".into(),
                payload: mez_agent::AgentActionPayload::ApplyPatch {
                    patch: format!(
                        "*** Begin Patch\n*** Add File: {}\n+data\n*** End Patch",
                        root.join("alias/new/file").display()
                    ),
                    strip: None,
                },
            }]
        },
        |_, _| {},
    );
    fs::create_dir(fixture.root.join("real")).unwrap();
    symlink(fixture.root.join("real"), fixture.root.join("alias")).unwrap();
    let (attempt, raw) = super::canonical::execute_and_retain(&mut fixture, false);
    assert_eq!(
        raw.result.certainty,
        NativeEffectCertainty::Confirmed,
        "{:?}",
        raw.result.diagnostic
    );
    assert_eq!(
        raw.receipts[0].resolved_path,
        fixture.root.join("real/new").to_str().unwrap()
    );
    assert_eq!(
        raw.receipts[0].parent_target.as_ref().unwrap().path,
        fixture.root.join("alias/new/file").to_str().unwrap()
    );
    assert!(
        fixture
            .service
            .project_native_filesystem_completion(&attempt)
            .unwrap()
    );
    assert_eq!(
        fs::read(fixture.root.join("real/new/file")).unwrap(),
        b"data\n"
    );
}

/// Display overflow may coalesce notification prefixes, but the original journal
/// and final canonical result must retain the directory and every later file.
#[tokio::test(flavor = "current_thread")]
async fn parent_and_file_confirmations_survive_live_prefix_overflow() {
    let mut fixture = Fixture::with_patch(false, |root| {
        let mut source = "*** Begin Patch\n".to_string();
        for index in 0..70 {
            source.push_str(&format!(
                "*** Add File: {}\n+x\n",
                root.join(format!("parent/f{index}")).display()
            ));
        }
        source.push_str("*** End Patch");
        source
    });
    let attempt = fixture.queue();
    let producer = NativeFilesystemProducer::new(attempt.clone());
    let actor = ActorFixture::new(fixture);
    claim_and_start_native_filesystem_producer(actor.handle.clone(), &producer)
        .await
        .unwrap();
    let raw = project_joined_native_filesystem_facts(&actor.handle, &producer)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        raw.result.certainty,
        NativeEffectCertainty::Confirmed,
        "{:?}",
        raw.result.diagnostic
    );
    assert_eq!(raw.receipts.len(), 71);
    producer.validate_confirmed_outcome(&raw).unwrap();
    assert_eq!(
        producer
            .progress_snapshot()
            .unwrap()
            .confirmed()
            .unwrap()
            .len(),
        64
    );
    fixture = actor.finish().await;
    let result = &fixture.service.agent_turn_executions()[&fixture.turn.turn_id].action_results[0];
    let data: serde_json::Value =
        serde_json::from_str(result.structured_content_json.as_deref().unwrap()).unwrap();
    assert_eq!(data["confirmed_endpoints"].as_array().unwrap().len(), 71);
    assert_eq!(
        fixture
            .service
            .retained_agent_modified_files("%1")
            .unwrap()
            .len(),
        70
    );
    assert_eq!(fs::read(fixture.root.join("parent/f69")).unwrap(), b"x\n");
}
