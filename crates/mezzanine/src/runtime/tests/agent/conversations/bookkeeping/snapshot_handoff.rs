//! Exact candidate ownership witnesses, not archive or disk receipts.
//!
//! The existing stopped-turn fixture produces a real immutable candidate. Its
//! owned pane child is terminated before any oracle or error injection below;
//! these tests never submit providers or repeat action/native execution.

use super::*;
use crate::runtime::RuntimeBookkeepingCandidate;
use crate::runtime::persistence::RuntimeCompletionCandidateAcceptance;
use crate::storage::transcript::compaction_handoff::{
    TerminalCompactionHandoff, TerminalCompactionReplacement,
};

/// Both accepted namespaces must remain bound to the exact original turn. A
/// foreign interruption key or syntactically valid foreign conversation row is
/// not an original-scope handoff, even if all other candidate metadata matches.
#[test]
fn runtime_completion_handoff_rejects_foreign_key_and_evidence_before_enqueue() {
    for wrong in ["key", "entry", "directory"] {
        let (mut service, mut candidate) = snapshot_candidate(wrong);
        match wrong {
            "key" => candidate.persistence_key.1 = "interrupted:foreign-turn".to_string(),
            "entry" => {
                candidate.entries.last_mut().unwrap().conversation_id =
                    "foreign-evidence".to_string()
            }
            "directory" => {
                let mut directory = candidate.entries.last().unwrap().clone();
                directory.conversation_id = "foreign-directory".to_string();
                candidate.directory = Some(directory);
            }
            _ => unreachable!(),
        }
        assert!(
            service
                .persistence
                .retain_completion_bookkeeping_candidate(candidate)
                .is_err()
        );
        assert!(service.claim_bookkeeping_candidates().is_empty());
    }
}

/// Returns frozen original evidence after cleaning the only fixture-owned child.
fn snapshot_candidate(label: &str) -> (RuntimeSessionService, RuntimeBookkeepingCandidate) {
    let (mut service, _, _) = stopped_candidate_fixture(label);
    let work = service.claim_bookkeeping_candidates().pop().unwrap();
    let mut candidate = service
        .persistence
        .take_bookkeeping_candidate(work.candidate.generation)
        .unwrap();
    service.terminate_all_pane_processes().unwrap();
    candidate.read = candidate.read.freeze_completion();
    candidate.entries.push(TranscriptEntry {
        conversation_id: candidate.turn.conversation_id.clone(),
        sequence: candidate.entries.len() as u64 + 2,
        created_at_unix_seconds: 1,
        role: TranscriptRole::System,
        turn_id: candidate.turn.turn_id.clone(), agent_id: candidate.turn.agent_id.clone(),
        pane_id: candidate.turn.pane_id.clone(),
        content: "{\"completion_settlement\":{\"version\":1,\"group\":\"fixture-group\",\"actions\":[{\"id\":\"fixture-action\",\"attempt\":\"fixture-attempt\"}],\"required_failure\":{\"stage\":\"audit\",\"kind\":\"Io\",\"possible_effects\":true,\"automatic_replay\":false}}}".to_string(),
    });
    (service, candidate)
}

/// Identical evidence retrieves the exact existing producer witness without a
/// second candidate. Changing even safe gate evidence must produce a distinct
/// identity/generation, rather than acknowledge the old partial snapshot.
#[test]
fn runtime_completion_handoff_witness_is_exact_and_snapshot_bound() {
    let (mut service, candidate) = snapshot_candidate("completion-snapshot");
    let first = service
        .persistence
        .retain_completion_bookkeeping_candidate(candidate.clone())
        .unwrap();
    assert!(matches!(
        first,
        RuntimeCompletionCandidateAcceptance::Accepted(_)
    ));
    assert_eq!(first.witness().snapshot_id().len(), 64);
    assert!(
        first
            .witness()
            .snapshot_id()
            .bytes()
            .all(|byte| { byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte) })
    );
    let duplicate = service
        .persistence
        .retain_completion_bookkeeping_candidate(candidate.clone())
        .unwrap();
    assert!(matches!(
        duplicate,
        RuntimeCompletionCandidateAcceptance::AlreadyAccepted(_)
    ));
    assert_eq!(first.witness(), duplicate.witness());
    assert_eq!(service.claim_bookkeeping_candidates().len(), 1);
    let mut changed = candidate;
    changed
        .entries
        .last_mut()
        .unwrap()
        .content
        .push_str("\nchanged safe evidence snapshot");
    let second = service
        .persistence
        .retain_completion_bookkeeping_candidate(changed)
        .unwrap();
    assert!(matches!(
        second,
        RuntimeCompletionCandidateAcceptance::Accepted(_)
    ));
    assert_ne!(
        first.witness().snapshot_id(),
        second.witness().snapshot_id()
    );
    assert_ne!(first.witness().generation(), second.witness().generation());
}

/// Original immutable read/evidence ownership survives replacement of the live
/// conversation. The returned witness does not certify any replacement history
/// or prove that the original candidate has been checked or written to disk.
#[test]
fn runtime_completion_handoff_keeps_original_snapshot_after_rebind() {
    let (mut service, candidate) = snapshot_candidate("completion-rebind");
    let original = candidate.turn.conversation_id.clone();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "replacement-witness-fixture", 0)
        .unwrap();
    let accepted = service
        .persistence
        .retain_completion_bookkeeping_candidate(candidate.clone())
        .unwrap();
    let duplicate = service
        .persistence
        .retain_completion_bookkeeping_candidate(candidate)
        .unwrap();
    assert_eq!(accepted.witness(), duplicate.witness());
    let work = service.claim_bookkeeping_candidates().pop().unwrap();
    assert_eq!(work.candidate.turn.conversation_id, original);
    assert_eq!(
        service.agent_shell_store().get("%1").unwrap().session_id,
        "replacement-witness-fixture"
    );
}

/// Exhaustion refuses a new exact witness before queue mutation. It must not
/// alias the saturated legacy generation or drop/acknowledge changed evidence.
#[test]
fn runtime_completion_handoff_exhaustion_does_not_enqueue_or_alias() {
    let (mut service, candidate) = snapshot_candidate("completion-exhaustion");
    service
        .persistence
        .set_bookkeeping_generation_for_tests(u64::MAX);
    let error = service
        .persistence
        .retain_completion_bookkeeping_candidate(candidate)
        .unwrap_err();
    assert!(error.message().contains("generation exhausted"));
    assert!(service.claim_bookkeeping_candidates().is_empty());
}

/// A same-CID live frontier advance is not permission to replace the original
/// read hashed by the completion witness. The checked worker must receive the
/// exact frozen store/frontier/epoch and pending rows, not fresh pane metadata.
#[test]
fn runtime_completion_handoff_claim_keeps_frozen_read_after_frontier_advance() {
    let (mut service, candidate) = snapshot_candidate("completion-frozen-frontier");
    service
        .persistence
        .retain_completion_bookkeeping_candidate(candidate.clone())
        .unwrap();
    service
        .agent_shell_store_mut()
        .record_transcript_entries(&candidate.turn.pane_id, 1)
        .unwrap();
    let work = service.claim_bookkeeping_candidates().pop().unwrap();
    assert_eq!(work.read, candidate.read);
    assert_eq!(work.candidate.read, work.read);
}

/// A worker reply checked against different read metadata cannot consume the
/// original completion candidate. Retain it blocked for truthful supervision
/// rather than admitting a checked append under an unrelated snapshot.
#[test]
fn runtime_completion_handoff_rejects_changed_worker_read() {
    let (mut service, candidate) = snapshot_candidate("completion-wrong-worker-read");
    service
        .persistence
        .retain_completion_bookkeeping_candidate(candidate.clone())
        .unwrap();
    service
        .agent_shell_store_mut()
        .record_transcript_entries(&candidate.turn.pane_id, 1)
        .unwrap();
    let mut work = service.claim_bookkeeping_candidates().pop().unwrap();
    work.read = service.capture_bookkeeping_transcript_read(
        service.persistence.cloned_transcript_store().unwrap(),
        &candidate.turn,
    );
    let error = service
        .complete_bookkeeping_candidate(work, Ok((Vec::new(), 1)))
        .unwrap_err();
    assert!(error.message().contains("frozen candidate owner"));
    assert!(
        service
            .persistence
            .bookkeeping_pending(&candidate.turn.conversation_id)
    );
    assert!(service.claim_bookkeeping_candidates().is_empty());
}

/// Exact immutable dispatch identity does not certify that checked history is
/// fresh at actor acceptance. An authentic result arriving after the original
/// live frontier advanced must fail closed and retain its candidate, without
/// painting an error into a potentially replaced pane.
#[test]
fn runtime_completion_handoff_rejects_authentic_reply_after_frontier_advance() {
    let (mut service, candidate) = snapshot_candidate("completion-stale-reply");
    service
        .persistence
        .retain_completion_bookkeeping_candidate(candidate.clone())
        .unwrap();
    let work = service.claim_bookkeeping_candidates().pop().unwrap();
    let checked = work.execute();
    assert!(
        checked.is_ok(),
        "fixture must reach a successful checked read"
    );
    service
        .agent_shell_store_mut()
        .record_transcript_entries(&candidate.turn.pane_id, 1)
        .unwrap();
    let error = service
        .complete_bookkeeping_candidate(work, checked)
        .unwrap_err();
    assert!(error.message().contains("history changed"));
    assert!(
        service
            .persistence
            .bookkeeping_pending(&candidate.turn.conversation_id)
    );
    assert!(service.claim_bookkeeping_candidates().is_empty());
}

/// The immutable completion owner also retains the original compaction epoch.
/// A successful archive read cannot authorize acceptance after compaction has
/// changed that epoch, even if the transcript counter has not changed.
#[test]
fn runtime_completion_handoff_rejects_authentic_reply_after_epoch_advance() {
    let (mut service, candidate) = snapshot_candidate("completion-stale-epoch");
    service
        .persistence
        .retain_completion_bookkeeping_candidate(candidate.clone())
        .unwrap();
    let work = service.claim_bookkeeping_candidates().pop().unwrap();
    let checked = work.execute();
    assert!(
        checked.is_ok(),
        "fixture must reach a successful checked read"
    );
    service.mark_agent_compacting_for_tests(&candidate.turn.pane_id, 1);
    let error = service
        .complete_bookkeeping_candidate(work, checked)
        .unwrap_err();
    assert!(error.message().contains("history changed"));
    assert!(
        service
            .persistence
            .bookkeeping_pending(&candidate.turn.conversation_id)
    );
    assert!(service.claim_bookkeeping_candidates().is_empty());
}

/// Freezing belongs to the actual completion producer port, not its caller.
/// Initially ordinary candidate metadata must become immutable before it is
/// hashed/enqueued, while retaining all original evidence and target fields.
#[test]
fn runtime_completion_handoff_producer_freezes_ordinary_read() {
    let (mut service, _, _) = stopped_candidate_fixture("completion-port-freeze");
    let original_work = service.claim_bookkeeping_candidates().pop().unwrap();
    let candidate = service
        .persistence
        .take_bookkeeping_candidate(original_work.candidate.generation)
        .unwrap();
    service.terminate_all_pane_processes().unwrap();
    service
        .persistence
        .retain_completion_bookkeeping_candidate(candidate.clone())
        .unwrap();
    let work = service.claim_bookkeeping_candidates().pop().unwrap();
    assert_ne!(work.read, candidate.read);
    assert_eq!(work.read, candidate.read.freeze_completion());
    assert_eq!(work.read, work.candidate.read);
}

/// Frozen archive evidence does not grant live metadata authority, even when a
/// later binding uses exactly the original conversation label and counter.
/// Checked append admission may own old rows, but cannot increment or attach
/// transcript references to the replacement pane session.
#[test]
fn runtime_completion_handoff_same_label_rebind_has_no_live_metadata_authority() {
    let (mut service, candidate) = snapshot_candidate("completion-same-label");
    service
        .persistence
        .retain_completion_bookkeeping_candidate(candidate.clone())
        .unwrap();
    let work = service.claim_bookkeeping_candidates().pop().unwrap();
    let checked = work.execute();
    assert!(
        checked.is_ok(),
        "fixture must reach a successful checked read"
    );
    let before = service
        .agent_shell_store()
        .get(&candidate.turn.pane_id)
        .unwrap()
        .clone();
    let references = service
        .persistence
        .pane_transcript_refs(&candidate.turn.pane_id);
    service
        .agent_shell_store_mut()
        .bind_conversation(
            &candidate.turn.pane_id,
            &candidate.turn.conversation_id,
            before.transcript_entries,
        )
        .unwrap();
    assert!(
        service
            .complete_bookkeeping_candidate(work, checked)
            .unwrap()
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get(&candidate.turn.pane_id)
            .unwrap()
            .transcript_entries,
        before.transcript_entries,
    );
    assert_eq!(
        service
            .persistence
            .pane_transcript_refs(&candidate.turn.pane_id),
        references,
    );
}

/// Builds a valid prospective certificate with two unique typed occurrences
/// separated by an ordinary display row. It is unsupported, not corrupt; both
/// admission paths must retain the identical existing raw-authority marker.
fn interleaved_handoff_entries(candidate: &RuntimeBookkeepingCandidate) -> Vec<TranscriptEntry> {
    let source = |group| {
        mez_agent::TranscriptContextEvent::execution_block_with_metadata(
            ContextSourceKind::TranscriptAssistant,
            "owned source",
            "same source text",
            mez_agent::ContextExecutionGroupId::new(group).unwrap(),
            1,
            None,
        )
        .unwrap()
        .to_transcript_content()
    };
    let sources = vec![
        source("first-completion-source"),
        source("second-completion-source"),
    ];
    let content = TerminalCompactionHandoff {
        baseline: None,
        replacements: vec![TerminalCompactionReplacement {
            sources: sources.clone(),
            summary: "accepted local summary".to_string(),
        }],
    }
    .to_content()
    .unwrap();
    [
        sources[0].clone(),
        "intermediate display".to_string(),
        sources[1].clone(),
        content,
    ]
    .into_iter()
    .enumerate()
    .map(|(ordinal, content)| TranscriptEntry {
        conversation_id: candidate.turn.conversation_id.clone(),
        sequence: ordinal as u64 + 1,
        created_at_unix_seconds: 1,
        role: if ordinal == 1 {
            TranscriptRole::Assistant
        } else {
            TranscriptRole::System
        },
        turn_id: candidate.turn.turn_id.clone(),
        agent_id: candidate.turn.agent_id.clone(),
        pane_id: candidate.turn.pane_id.clone(),
        content,
    })
    .collect()
}

/// Frozen completion acceptance must not call live status/trace projection,
/// including a same-label replacement with an armed preinstall fault. Actual
/// checked work admits the unchanged original archive evidence and leaves the
/// fault available for a later independent live publication attempt.
#[test]
fn runtime_completion_handoff_interleaving_has_no_replacement_paint_authority() {
    let (mut service, mut candidate) = snapshot_candidate("completion-detached-interleaved");
    candidate.entries = interleaved_handoff_entries(&candidate);
    service
        .persistence
        .retain_completion_bookkeeping_candidate(candidate.clone())
        .unwrap();
    let work = service.claim_bookkeeping_candidates().pop().unwrap();
    let checked = work.execute();
    assert!(
        checked.is_ok(),
        "fixture must reach a successful checked read"
    );
    let before = service
        .agent_shell_store()
        .get(&candidate.turn.pane_id)
        .unwrap()
        .clone();
    service
        .agent_shell_store_mut()
        .bind_conversation(
            &candidate.turn.pane_id,
            &candidate.turn.conversation_id,
            before.transcript_entries,
        )
        .unwrap();
    service.fail_agent_presentation_install_for_tests(0);
    assert!(
        service
            .complete_bookkeeping_candidate(work, checked)
            .unwrap()
    );
    let rows = candidate_rows(&mut service);
    assert!(rows.iter().any(|entry| entry.content
        == "[mez-terminal-compaction-handoff/unsupported]\nreason=noncontiguous_archive_source"));
    assert!(
        !service
            .persistence
            .bookkeeping_pending(&candidate.turn.conversation_id)
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get(&candidate.turn.pane_id)
            .unwrap()
            .transcript_entries,
        before.transcript_entries
    );
    assert!(
        service
            .append_agent_status_text_to_terminal_buffer(
                &candidate.turn.pane_id,
                "independent live publication control",
            )
            .is_err(),
        "detached archive admission must not consume the UI fault"
    );
}

/// Extraction cannot weaken malformed, missing, ambiguous, or reordered source
/// validation. All remain required errors in both paths, before the certificate
/// can be downgraded to an unsupported marker or accepted by persistence.
#[test]
fn runtime_completion_handoff_pure_validation_preserves_required_source_errors() {
    for fault in ["malformed", "missing", "ambiguous", "reordered"] {
        let (mut service, candidate) = snapshot_candidate(fault);
        let mut entries = interleaved_handoff_entries(&candidate);
        match fault {
            "malformed" => {
                entries.last_mut().unwrap().content =
                    "[mez-terminal-compaction-handoff/v1]\nnot-json".to_string()
            }
            "missing" => {
                entries.remove(0);
            }
            "ambiguous" => {
                entries.insert(0, entries[0].clone());
            }
            "reordered" => entries.swap(0, 2),
            _ => unreachable!(),
        }
        let mut detached = entries.clone();
        let detached_error =
            RuntimeSessionService::check_detached_terminal_compaction_handoff_admission(
                &[],
                &mut detached,
            )
            .unwrap_err();
        let live_error = service
            .check_terminal_compaction_handoff_admission(&candidate.turn, &[], &mut entries)
            .unwrap_err();
        assert_eq!(detached_error.kind(), live_error.kind(), "{fault}");
        assert_eq!(detached_error.message(), live_error.message(), "{fault}");
        assert_eq!(detached, entries, "{fault}");
        assert!(
            detached
                .last()
                .unwrap()
                .content
                .starts_with("[mez-terminal-compaction-handoff/v1]")
        );
    }
}

/// The legacy wrapper still reports unsupported interleaving to its current
/// conversation and propagates that installation failure. Only the detached
/// completion caller uses the pure boundary; no universal UI-error suppression
/// or changed legacy diagnostic contract is introduced by the extraction.
#[test]
fn runtime_completion_handoff_legacy_wrapper_retains_live_publication_error() {
    let (mut service, candidate) = snapshot_candidate("completion-legacy-ui");
    let mut live = interleaved_handoff_entries(&candidate);
    let mut detached = live.clone();
    RuntimeSessionService::check_detached_terminal_compaction_handoff_admission(&[], &mut detached)
        .unwrap();
    service.fail_agent_presentation_install_for_tests(0);
    assert!(
        service
            .check_terminal_compaction_handoff_admission(&candidate.turn, &[], &mut live,)
            .is_err()
    );
    assert_eq!(
        live, detached,
        "both paths retain the exact raw-authority rewrite"
    );
}
