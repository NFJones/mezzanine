//! Exact candidate ownership witnesses, not archive or disk receipts.
//!
//! The existing stopped-turn fixture produces a real immutable candidate. Its
//! owned pane child is terminated before any oracle or error injection below;
//! these tests never submit providers or repeat action/native execution.

use super::*;
use crate::runtime::RuntimeBookkeepingCandidate;
use crate::runtime::persistence::RuntimeCompletionCandidateAcceptance;

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
