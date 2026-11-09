//! Sparse summaries retain exact source and use the same durable receipt/CAS.

use super::*;

/// Even a self-consistent receipt with an exact retained complement must refuse
/// a late retained group spanning the selection. This bypasses pure preparation
/// only inside the adversarial fixture to exercise the storage verifier itself.
#[test]
fn protected_selection_receipt_rejects_a_retained_span_crossing_coverage() {
    use mez_agent::{ContextConversationAppend, ContextExecutionGroupId};
    let fixture = Fixture::new();
    let mut source = ReplayEpochContext::new(
        ReplayConversationId::new("conversation-a").unwrap(),
        prefix(),
    )
    .unwrap();
    let pending = ContextExecutionGroupId::new("retained-span").unwrap();
    source
        .append_execution_events([ContextConversationAppend::assistant(
            "pending",
            "original pending source",
            pending.clone(),
        )])
        .unwrap();
    source
        .append_fact("closed", "completed independent source")
        .unwrap();
    let prepared = proposal(&fixture, &source, None, "reference head");
    source
        .append_execution_events([ContextConversationAppend::assistant(
            "late",
            "retained source now spans coverage",
            pending,
        )])
        .unwrap();
    let extended = serde_json::to_value(source.checkpoint().unwrap()).unwrap();
    let mut candidate =
        serde_json::to_value(prepared.prepared.candidate().checkpoint().unwrap()).unwrap();
    candidate["events"].as_array_mut().unwrap().push(
        extended["events"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .clone(),
    );
    candidate["members"].as_array_mut().unwrap().push(
        extended["members"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .clone(),
    );
    candidate["next_event_sequence"] = extended["next_event_sequence"].clone();
    let mut forged = prepared.envelope.clone();
    let mut payloads = prepared.payloads.clone();
    forged.manifest.source_checkpoint =
        externalize(source.checkpoint().unwrap(), &mut payloads).unwrap();
    forged.manifest.checkpoint =
        externalize(serde_json::from_value(candidate).unwrap(), &mut payloads).unwrap();
    forged.manifest.source_escrow = Some(
        escrow::publication_source(
            &[],
            &source,
            &origin(),
            &[],
            &prepared.backing.archive.archived,
        )
        .unwrap(),
    );
    forged.sha256 = digest(&encode(&forged.manifest).unwrap());
    let error =
        ReplayEpochPublication::validate_candidate(&forged, &payloads, prefix()).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("protected or unfrozen covered members"),
        "{error}"
    );
    assert!(!fixture.root.join("conversation-a").exists());
}

/// A saved fork independently archives retained exact islands. A later sparse
/// summary must leave its original source editable through real content CAS;
/// repeated edits flatten to the same immutable base receipt and reopen cold.
#[test]
fn protected_selection_saved_fork_and_content_edits_preserve_the_head() {
    use crate::storage::transcript::CompareAndSwapTranscriptEntryResult as Cas;
    use mez_agent::transcript::{
        TranscriptContextEvent as Event, TranscriptContextRecord as Record,
    };
    let fixture = Fixture::new();
    let source = protected_source();
    raw_edits::archive_retained_source(&fixture, &source, plan(&source));
    let (parent, mut child) = raw_edits::load(&fixture);
    let inherited = child.context().chronology().to_vec();
    assert_eq!(inherited[1].block().content, "exact root");
    child
        .append_fact("new", "completed work in the fork")
        .unwrap();
    let snapshot = {
        let _lease = fixture
            .store
            .acquire_conversation_lock(raw_edits::CID)
            .unwrap();
        fixture
            .store
            .replay_epoch_snapshot_locked(raw_edits::CID)
            .unwrap()
            .unwrap()
    };
    let next = fixture
        .store
        .prepare_replay_epoch(
            origin().capture(Some(&snapshot)),
            raw_edits::CID,
            Some(parent),
            &child,
            plan(&child),
            ReplayHeadPublication::Summary("fork sparse head".into()),
        )
        .unwrap();
    assert_eq!(next.envelope.manifest.version, ISLAND_MANIFEST_VERSION);
    fixture.store.publish_replay_epoch(&next).unwrap();
    let (_, cold) = raw_edits::load(&fixture);
    let head = cold.head().unwrap().clone();
    assert_eq!(&cold.context().chronology()[1..], &inherited[1..]);
    let mut row = fixture
        .store
        .inspect(raw_edits::CID)
        .unwrap()
        .into_iter()
        .find(|row| {
            Record::from_transcript_content(&row.content)
                .unwrap()
                .is_some_and(|record| {
                    record.record().occurrence_id() == cold.members()[0].occurrence_id()
                })
        })
        .unwrap();
    for content in ["revised exact root", "revised exact root again 界"] {
        let record = Record::from_transcript_content(&row.content)
            .unwrap()
            .unwrap();
        let mut event = record.event().clone();
        match &mut event {
            Event::UserEvent { content: body, .. } | Event::TaskAdmission { content: body, .. } => {
                *body = content.into()
            }
            other => panic!("expected retained instruction, got {other:?}"),
        }
        let updated = Record::new(
            event,
            record.record(),
            record.stable_records().map(<[_]>::to_vec),
        )
        .unwrap()
        .to_transcript_content()
        .unwrap();
        let result = fixture
            .store
            .compare_and_swap_entry_content(
                raw_edits::CID,
                row.sequence,
                &row.pane_id,
                &fixture.store.transcript_entry_revision(&row).unwrap(),
                updated,
            )
            .unwrap();
        let Cas::Updated(updated) = result else {
            panic!("content CAS did not update")
        };
        row = updated;
        let (_, restored) = AgentTranscriptStore::new(fixture.store.root())
            .load_replay_epoch(raw_edits::CID, prefix())
            .unwrap()
            .unwrap();
        assert_eq!(restored.head(), Some(&head));
        assert_eq!(restored.context().chronology()[1].block().content, content);
        assert_eq!(
            restored.context().chronology()[1].retention(),
            mez_agent::ContextRetention::Exact
        );
        assert_eq!(restored.context().chronology()[2], inherited[2]);
    }
}

fn protected_source() -> ReplayEpochContext {
    let mut source = ReplayEpochContext::new(
        ReplayConversationId::new("conversation-a").unwrap(),
        prefix(),
    )
    .unwrap();
    let task = source.begin_instruction_task().unwrap();
    source
        .append_instruction(&task, ContextBlock::user_event("task", "exact root"))
        .unwrap();
    source
        .append_fact("completed", "first covered evidence")
        .unwrap();
    source
        .append_instruction(
            &task,
            ContextBlock::user_event("steering", "exact steering"),
        )
        .unwrap();
    source
        .append_fact("completed", "second covered evidence")
        .unwrap();
    source
}

#[test]
fn protected_selection_receipt_retains_islands_and_rejects_legacy_relabeling() {
    let fixture = Fixture::new();
    let source = protected_source();
    let selected = plan(&source);
    let retained = selected.retained_raw_events().cloned().collect::<Vec<_>>();
    let prepared = proposal(&fixture, &source, None, "completed evidence snapshot");
    assert_eq!(prepared.envelope.manifest.version, ISLAND_MANIFEST_VERSION);
    for version in [MODE_MANIFEST_VERSION, 11] {
        let mut forged = prepared.envelope.clone();
        forged.manifest.version = version;
        if version == 11 {
            forged.manifest.head_policy = None;
        }
        forged.sha256 = digest(&encode(&forged.manifest).unwrap());
        assert!(
            ReplayEpochPublication::validate_candidate(&forged, &prepared.payloads, prefix())
                .is_err()
        );
    }
    let mut forged = prepared.envelope.clone();
    forged.manifest.covered.reverse();
    forged.sha256 = digest(&encode(&forged.manifest).unwrap());
    assert!(
        ReplayEpochPublication::validate_candidate(&forged, &prepared.payloads, prefix()).is_err()
    );
    fixture.store.publish_replay_epoch(&prepared).unwrap();
    let (identity, cold) = fixture.load();
    assert_eq!(identity, prepared.identity());
    assert_eq!(&cold.context().chronology()[1..], retained);
    assert_eq!(
        cold.context().chronology()[0].sequence().get() + 1,
        retained[0].sequence().get()
    );
    assert!(
        !cold
            .automatic_compaction_is_fresh(ReplayTailPolicy::new(1000, 0).unwrap())
            .unwrap()
    );
}

#[test]
fn protected_selection_uncertain_publication_retries_only_the_original_receipt() {
    for phase in [
        Phase::BeforeManifestSwap,
        Phase::AfterManifestSwap,
        Phase::BeforeAcknowledgment,
    ] {
        let fixture = Fixture::new();
        let source = protected_source();
        let prepared = proposal(&fixture, &source, None, "completed evidence snapshot");
        fixture.store.inject_replay_epoch_fault(phase, 1, false);
        assert!(fixture.store.publish_replay_epoch(&prepared).is_err());
        let receipt = fixture.store.publish_replay_epoch(&prepared).unwrap();
        let mut live = source.into_owned_context();
        receipt.adopt(&fixture.store, &prepared, &mut live).unwrap();
        let accepted = live.clone();
        receipt.adopt(&fixture.store, &prepared, &mut live).unwrap();
        assert_eq!(live, accepted);
        assert_eq!(fixture.load().1.context().chronology(), live.chronology());
        assert_eq!(fixture.load().1.revision().get(), 1);
    }
}
