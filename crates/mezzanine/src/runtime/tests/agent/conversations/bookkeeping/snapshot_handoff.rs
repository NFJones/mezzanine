//! Exact candidate ownership witnesses, not archive or disk receipts.
//!
//! Most witness controls retain the existing stopped-turn fixture and terminate
//! its owned child before their oracles. The six strong-claim/coherent-read
//! controls instead use an isolated ordinary producer with no native child.
//! Neither fixture submits providers or repeats action/native execution.

use super::*;
use crate::runtime::RuntimeBookkeepingCandidate;
use crate::runtime::persistence::RuntimeCompletionCandidateAcceptance;
use crate::storage::transcript::compaction_handoff::{
    TerminalCompactionHandoff, TerminalCompactionReplacement,
};

/// Owns only generated fixture files; declare before service/work bindings so
/// cleanup follows their teardown, including persistence reset and unwinding.
struct NativeFreeBookkeepingRoot(PathBuf);

impl NativeFreeBookkeepingRoot {
    /// Allocates an isolated store/socket parent without touching shared state.
    fn new(label: &str) -> Self {
        let root = Self(temp_root(label));
        std::fs::create_dir_all(&root.0).unwrap();
        root
    }
}

impl Drop for NativeFreeBookkeepingRoot {
    /// Best-effort local cleanup must not mask a primary assertion failure.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Projects only the actual admitted prompt's immutable event identity for
/// assertions. The request fixture's synthetic User message is not chronology.
fn admitted_fixture_prompt(
    service: &RuntimeSessionService,
    turn_id: &str,
) -> mez_agent::TranscriptContextEvent {
    let event = service
        .agent_turn_contexts()
        .get(turn_id)
        .unwrap()
        .chronology()
        .iter()
        .find(|event| {
            event.block().source == ContextSourceKind::UserInstruction
                && event.block().label == "user prompt"
        })
        .unwrap();
    assert_eq!(
        event.block().content,
        "native-free retained candidate prompt"
    );
    mez_agent::TranscriptContextEvent::user_event(
        event.sequence().get(),
        event.block().label.clone(),
        event.block().content.clone(),
    )
    .unwrap()
}

/// Produces actual ordinary chronology from an admitted prompt, then obtains
/// its frozen form through the existing retained owner and exact retirement.
/// The synthetic nonfinal response is projection input, not provider output or
/// execution authority. No child, Stop, executor, hook override, or forged
/// settlement evidence is needed. The caller keeps `root` alive past all work.
fn native_free_frozen_candidate(
    root: &NativeFreeBookkeepingRoot,
) -> (RuntimeSessionService, RuntimeBookkeepingCandidate) {
    let mut service = RuntimeServiceFixture::new()
        .control_socket(root.0.join("control.sock"))
        .build();
    service.set_agent_transcript_store(AgentTranscriptStore::new(root.0.join("transcripts")));
    service.persistence.enable_transcript_adapter();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let conversation = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let started = service
        .start_agent_prompt_turn("%1", "native-free retained candidate prompt")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .unwrap()
        .clone();
    assert_eq!(turn.state, AgentTurnState::Running);
    assert_eq!(turn.conversation_id, conversation);
    assert!(service.agent_turn_contexts().contains_key(&turn.turn_id));
    let expected_prompt = admitted_fixture_prompt(&service, &turn.turn_id);
    assert!(!service.agent_shell_store().get("%1").unwrap().ephemeral);
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .running_turn_id
            .as_deref(),
        Some(turn.turn_id.as_str())
    );
    assert!(!service.agent_provider_task_is_claimed(&turn.turn_id));
    assert!(service.remove_pending_agent_provider_task(&turn.turn_id));
    assert!(!service.agent_provider_task_is_owned(&turn.turn_id));
    assert!(service.tracked_runtime_pane_process_ids().is_empty());
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "inert ordinary bookkeeping projection".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: None,
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: Vec::new(),
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    assert!(!service.agent_turn_executions().contains_key(&turn.turn_id));
    assert_eq!(
        service
            .persist_runtime_agent_turn_execution_transcript(&turn, &execution)
            .unwrap(),
        0
    );
    // Actual production of exactly one ordinary candidate, not return zero,
    // proves the no-group producer branch was taken without private-map access.
    let mut work = service.claim_bookkeeping_candidates();
    assert_eq!(work.len(), 1);
    let original = work.pop().unwrap();
    assert_eq!(original.candidate.turn, turn);
    assert_eq!(
        original.candidate.persistence_key,
        (conversation.clone(), turn.turn_id)
    );
    assert_eq!(
        original
            .candidate
            .entries
            .iter()
            .filter(|row| {
                row.role == TranscriptRole::System
                    && mez_agent::TranscriptContextEvent::from_transcript_content(&row.content)
                        .as_ref()
                        == Some(&expected_prompt)
            })
            .count(),
        1
    );
    assert!(
        original
            .candidate
            .entries
            .iter()
            .all(|row| row.conversation_id == conversation
                && row.turn_id == original.candidate.turn.turn_id
                && row.agent_id == original.candidate.turn.agent_id
                && row.pane_id == original.candidate.turn.pane_id)
    );
    let candidate = service
        .persistence
        .take_bookkeeping_candidate(original.candidate.generation)
        .unwrap();
    let expected_read = candidate.read.clone().freeze_completion();
    let retained = service
        .persistence
        .retain_completion_bookkeeping_candidate(candidate)
        .unwrap();
    assert!(matches!(
        retained,
        RuntimeCompletionCandidateAcceptance::Accepted(_)
    ));
    let mut work = service.claim_bookkeeping_candidates();
    assert_eq!(work.len(), 1);
    let frozen = work.pop().unwrap();
    assert_eq!(frozen.candidate.generation, retained.witness().generation());
    assert_eq!(frozen.read, expected_read);
    assert_eq!(frozen.candidate.read, frozen.read);
    let candidate = service
        .persistence
        .take_bookkeeping_candidate(frozen.candidate.generation)
        .unwrap();
    assert!(!service.persistence.bookkeeping_pending(&conversation));
    assert!(service.tracked_runtime_pane_process_ids().is_empty());
    (service, candidate)
}

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

/// Retired work and its clones cannot consume a later claim even when the
/// legacy saturating generator repeats MAX and every candidate byte matches.
/// Resetting the component also creates new authority rather than restoring
/// an old strong handle. No archive read or append is needed for this oracle:
/// stale work must reject before even considering its supplied read failure.
/// The native-free ordinary producer supplies the genuinely retained frozen
/// candidate; legacy generation MAX is not archive sequence exhaustion, and
/// exact retirement here is not a distinct cancellation-callback test.
#[test]
fn runtime_completion_handoff_legacy_alias_keeps_new_claim_after_retire_or_reset() {
    for reset in [false, true] {
        let root = NativeFreeBookkeepingRoot::new("completion-legacy-claim");
        let (mut service, candidate) = native_free_frozen_candidate(&root);
        let conversation = candidate.turn.conversation_id.clone();
        service
            .persistence
            .set_bookkeeping_generation_for_tests(u64::MAX);
        service
            .persistence
            .queue_bookkeeping_candidate(candidate.clone());
        let stale = service.claim_bookkeeping_candidates().pop().unwrap();
        let stale_clone = stale.clone();
        let old_identity = service
            .persistence
            .bookkeeping_claim_identity(stale.candidate.generation)
            .unwrap();
        assert_eq!(stale.candidate.generation, u64::MAX);
        if reset {
            service.persistence = Default::default();
            service
                .persistence
                .set_bookkeeping_generation_for_tests(u64::MAX);
        } else {
            service
                .persistence
                .take_bookkeeping_candidate(stale.candidate.generation)
                .unwrap();
        }
        service.persistence.queue_bookkeeping_candidate(candidate);
        let current = service.claim_bookkeeping_candidates().pop().unwrap();
        let current_identity = service
            .persistence
            .bookkeeping_claim_identity(current.candidate.generation)
            .unwrap();
        assert_ne!(old_identity, current_identity);
        assert_eq!(current.candidate.generation, stale.candidate.generation);
        for old in [stale, stale_clone] {
            assert!(
                !service
                    .complete_bookkeeping_candidate(
                        old,
                        Err(MezError::invalid_state("stale read must not be admitted")),
                    )
                    .unwrap(),
                "reset={reset}"
            );
        }
        assert!(service.claim_bookkeeping_candidates().is_empty());
        assert_eq!(
            service
                .persistence
                .bookkeeping_claim_identity(current.candidate.generation),
            Some(current_identity)
        );
        let error = service
            .complete_bookkeeping_candidate(
                current.clone(),
                Err(MezError::invalid_state("current claim remains owned")),
            )
            .unwrap_err();
        assert_eq!(error.message(), "current claim remains owned");
        assert!(service.persistence.bookkeeping_pending(&conversation));
        assert!(service.claim_bookkeeping_candidates().is_empty());
        assert!(
            !service
                .complete_bookkeeping_candidate(
                    current,
                    Err(MezError::invalid_state("duplicate must not reenter")),
                )
                .unwrap()
        );
    }
}

/// A real coherent read cannot be admitted after a same-frontier CAS revokes
/// its original path/CID stamp. This tests the worker consumer; the editor test
/// separately checks that its production CAS invalidates before fallible I/O.
/// The checked candidate remains blocked and the changed file gets no old-read
/// suffix, even though the old worker history was successful and contiguous.
/// Setup uses only the ordinary native-free producer and owned temporary store;
/// stale rejection is not rollback of a previously accepted durable suffix.
#[test]
fn runtime_completion_handoff_checked_read_rejects_same_frontier_cas() {
    let root = NativeFreeBookkeepingRoot::new("completion-cas-read-race");
    let (mut service, candidate) = native_free_frozen_candidate(&root);
    let store = service.persistence.cloned_transcript_store().unwrap();
    let row = TranscriptEntry {
        conversation_id: candidate.turn.conversation_id.clone(),
        sequence: 1,
        created_at_unix_seconds: 1,
        role: TranscriptRole::User,
        turn_id: candidate.turn.turn_id.clone(),
        agent_id: candidate.turn.agent_id.clone(),
        pane_id: candidate.turn.pane_id.clone(),
        content: "before CAS".to_string(),
    };
    store.append(&row).unwrap();
    service
        .persistence
        .retain_completion_bookkeeping_candidate(candidate.clone())
        .unwrap();
    let work = service.claim_bookkeeping_candidates().pop().unwrap();
    let checked = work.execute();
    assert!(
        checked.is_ok(),
        "fixture must reach a coherent successful read"
    );
    let revision = store.transcript_entry_revision(&row).unwrap();
    let path = store.transcript_path(&row.conversation_id).unwrap();
    service
        .persistence
        .invalidate_transcript_admission_at_path(&path, &row.conversation_id);
    let changed = store
        .compare_and_swap_entry_content(
            &row.conversation_id,
            row.sequence,
            &row.pane_id,
            &revision,
            "after CAS".to_string(),
        )
        .unwrap();
    assert!(matches!(
        changed,
        crate::storage::transcript::CompareAndSwapTranscriptEntryResult::Updated(_)
    ));
    let after_cas = store.inspect(&row.conversation_id).unwrap();
    let error = service
        .complete_bookkeeping_candidate(work, checked)
        .unwrap_err();
    assert!(error.message().contains("history changed"));
    assert!(
        service
            .persistence
            .bookkeeping_pending(&row.conversation_id)
    );
    assert!(service.claim_bookkeeping_candidates().is_empty());
    assert!(
        service
            .persistence
            .pending_transcript_entries_at_path(&path, &row.conversation_id)
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.inspect(&row.conversation_id).unwrap(), after_cas);
}

/// A same-CID live frontier advance is not permission to replace the original
/// read hashed by the completion witness. The checked worker must receive the
/// exact frozen store/frontier/epoch and pending rows, not fresh pane metadata.
/// Unlike the separate after-check stale-reply control, this advance precedes
/// claim. Real coherent execution and exact queued-row acceptance are required;
/// immutable-read equality alone is insufficient and queue acceptance is not
/// a disk receipt. All setup is native-free and fixture-local.
#[test]
fn runtime_completion_handoff_claim_keeps_frozen_read_after_frontier_advance() {
    let root = NativeFreeBookkeepingRoot::new("completion-frozen-frontier");
    let (mut service, candidate) = native_free_frozen_candidate(&root);
    let expected_prompt = admitted_fixture_prompt(&service, &candidate.turn.turn_id);
    let store = service.persistence.cloned_transcript_store().unwrap();
    let conversation = candidate.turn.conversation_id.clone();
    let path = store.transcript_path(&conversation).unwrap();
    let mut expected: Vec<_> = candidate
        .directory
        .iter()
        .chain(candidate.entries.iter())
        .cloned()
        .collect();
    for (index, row) in expected.iter_mut().enumerate() {
        row.sequence = u64::try_from(index).unwrap().checked_add(1).unwrap();
    }
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
    // Advance happened BEFORE claim: current accepted-row ownership is sampled
    // there, while immutable read metadata must still come from the old owner.
    // Live metadata changes after execution are also independent of admission.
    let checked = work.execute();
    assert!(
        checked.is_ok(),
        "the real original-target coherent read must succeed"
    );
    let (history, next) = checked.as_ref().unwrap();
    assert!(history.is_empty());
    assert_eq!(*next, 1);
    assert!(
        service
            .complete_bookkeeping_candidate(work.clone(), checked)
            .unwrap()
    );
    assert!(
        !service
            .complete_bookkeeping_candidate(
                work,
                Err(MezError::invalid_state("duplicate must not reenter"))
            )
            .unwrap()
    );
    assert!(!service.persistence.bookkeeping_pending(&conversation));
    let accepted = service
        .persistence
        .pending_transcript_entries_at_path(&path, &conversation)
        .unwrap();
    assert_eq!(accepted, expected);
    assert!(!accepted.is_empty());
    assert!(
        accepted
            .iter()
            .all(|row| row.conversation_id == conversation)
    );
    assert_eq!(
        accepted
            .iter()
            .filter(|row| {
                row.role == TranscriptRole::System
                    && mez_agent::TranscriptContextEvent::from_transcript_content(&row.content)
                        .as_ref()
                        == Some(&expected_prompt)
            })
            .count(),
        1
    );
    assert!(accepted.iter().all(|row| row.validate().is_ok()));
    assert!(
        accepted
            .windows(2)
            .all(|rows| rows[0].sequence.checked_add(1) == Some(rows[1].sequence))
    );
    // This is checked append QUEUE acceptance at the original path, not a disk
    // receipt. Never use the live session's counter as this acceptance oracle.
    // No append worker ran: require typed absence, not a precreated empty file
    // or a generic read failure mistaken for an empty durable transcript.
    assert_eq!(
        store.inspect(&conversation).unwrap_err().kind(),
        crate::error::MezErrorKind::NotFound
    );
    assert!(!path.try_exists().unwrap());
}

/// Requires typed disk absence as well as path absence: canonical queue
/// acceptance is not durability, and an unrelated read error is not absence.
fn assert_native_free_transcript_absent(store: &AgentTranscriptStore, conversation: &str) {
    assert_eq!(
        store.inspect(conversation).unwrap_err().kind(),
        crate::error::MezErrorKind::NotFound
    );
    assert!(
        !store
            .transcript_path(conversation)
            .unwrap()
            .try_exists()
            .unwrap()
    );
}

/// A real successful checked read cannot be transported under recaptured live
/// metadata instead of its frozen owner. Change only the transport read after
/// execution, retain blocked custody, and reject duplicate delivery without
/// accepting any candidate suffix. No synthetic successful history is supplied.
#[test]
fn runtime_completion_handoff_rejects_changed_worker_read() {
    let root = NativeFreeBookkeepingRoot::new("completion-wrong-worker-read");
    let (mut service, candidate) = native_free_frozen_candidate(&root);
    let store = service.persistence.cloned_transcript_store().unwrap();
    let conversation = candidate.turn.conversation_id.clone();
    let path = store.transcript_path(&conversation).unwrap();
    service
        .persistence
        .retain_completion_bookkeeping_candidate(candidate.clone())
        .unwrap();
    let mut work = service.claim_bookkeeping_candidates().pop().unwrap();
    let checked = work.execute();
    assert!(checked.is_ok(), "fixture must reach a real checked read");
    service
        .agent_shell_store_mut()
        .record_transcript_entries(&candidate.turn.pane_id, 1)
        .unwrap();
    work.read = service.capture_bookkeeping_transcript_read(store.clone(), &candidate.turn);
    assert_ne!(work.read, candidate.read);
    assert_eq!(work.candidate.read, candidate.read);
    let error = service
        .complete_bookkeeping_candidate(work.clone(), checked)
        .unwrap_err();
    assert!(error.message().contains("frozen candidate owner"));
    assert!(service.persistence.bookkeeping_pending(&conversation));
    assert!(service.claim_bookkeeping_candidates().is_empty());
    assert!(
        !service
            .complete_bookkeeping_candidate(
                work,
                Err(MezError::invalid_state("duplicate must not reenter")),
            )
            .unwrap()
    );
    assert!(
        service
            .persistence
            .pending_transcript_entries_at_path(&path, &conversation)
            .unwrap()
            .is_empty()
    );
    assert_native_free_transcript_absent(&store, &conversation);
}

/// A real original-path/CID queue acceptance after checked execution rotates
/// admission identity. The old reply must block without discarding, servicing,
/// or rolling back the exact newly accepted row. Neither that row nor the
/// rejected candidate suffix is claimed durable by this native-free control.
#[test]
fn runtime_completion_handoff_rejects_reply_after_original_row_admission() {
    let root = NativeFreeBookkeepingRoot::new("completion-original-row-race");
    let (mut service, candidate) = native_free_frozen_candidate(&root);
    let store = service.persistence.cloned_transcript_store().unwrap();
    let conversation = candidate.turn.conversation_id.clone();
    let path = store.transcript_path(&conversation).unwrap();
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
    let (_, next) = checked.as_ref().unwrap();
    let row = TranscriptEntry {
        conversation_id: conversation.clone(),
        sequence: *next,
        created_at_unix_seconds: 1,
        role: TranscriptRole::User,
        turn_id: candidate.turn.turn_id.clone(),
        agent_id: candidate.turn.agent_id.clone(),
        pane_id: candidate.turn.pane_id.clone(),
        content: "new original-path admission after checked read".to_string(),
    };
    assert!(
        service
            .persistence
            .queue_checked_completion_transcript(
                crate::runtime::RuntimeSideEffect::PersistTranscriptEntries {
                    path: path.clone(),
                    store: store.clone(),
                    entries: vec![row.clone()],
                },
            )
            .unwrap()
    );
    assert_eq!(
        service
            .persistence
            .pending_transcript_entries_at_path(&path, &conversation)
            .unwrap(),
        vec![row.clone()]
    );
    let error = service
        .complete_bookkeeping_candidate(work.clone(), checked)
        .unwrap_err();
    assert!(error.message().contains("history changed"));
    assert!(service.persistence.bookkeeping_pending(&conversation));
    assert!(service.claim_bookkeeping_candidates().is_empty());
    assert!(
        !service
            .complete_bookkeeping_candidate(
                work,
                Err(MezError::invalid_state("duplicate must not reenter")),
            )
            .unwrap()
    );
    assert_eq!(
        service
            .persistence
            .pending_transcript_entries_at_path(&path, &conversation)
            .unwrap(),
        vec![row]
    );
    assert_native_free_transcript_absent(&store, &conversation);
}

/// Frozen archive admission is independent of live count and rendering epoch
/// changes after checked execution. Each variant accepts exactly the original
/// candidate rows and admitted prompt once, retires the candidate, and leaves
/// changed live metadata untouched. The epoch setter is only a rendering probe,
/// not production compaction, reset, or replacement permission. Acceptance here
/// is queued canonical ownership only; no append worker or native child runs.
#[test]
fn runtime_completion_handoff_frozen_read_is_independent_of_live_metadata() {
    for variant in ["count", "render_epoch"] {
        let root = NativeFreeBookkeepingRoot::new(&format!("completion-live-{variant}"));
        let (mut service, candidate) = native_free_frozen_candidate(&root);
        let expected_prompt = admitted_fixture_prompt(&service, &candidate.turn.turn_id);
        let store = service.persistence.cloned_transcript_store().unwrap();
        let conversation = candidate.turn.conversation_id.clone();
        let pane = &candidate.turn.pane_id;
        let path = store.transcript_path(&conversation).unwrap();
        let mut expected: Vec<_> = candidate
            .directory
            .iter()
            .chain(candidate.entries.iter())
            .cloned()
            .collect();
        for (index, row) in expected.iter_mut().enumerate() {
            row.sequence = u64::try_from(index).unwrap().checked_add(1).unwrap();
        }
        service
            .persistence
            .retain_completion_bookkeeping_candidate(candidate.clone())
            .unwrap();
        let work = service.claim_bookkeeping_candidates().pop().unwrap();
        let checked = work.execute();
        assert!(checked.is_ok(), "variant={variant} must reach checked read");
        let (history, next) = checked.as_ref().unwrap();
        assert!(history.is_empty());
        assert_eq!(*next, 1);
        let count = service
            .agent_shell_store()
            .get(pane)
            .unwrap()
            .transcript_entries;
        let epoch = service.agent_compaction_epoch(pane);
        match variant {
            "count" => {
                service
                    .agent_shell_store_mut()
                    .record_transcript_entries(pane, 1)
                    .unwrap();
            }
            "render_epoch" => service.mark_agent_compacting_for_tests(pane, 1),
            _ => unreachable!(),
        }
        let changed_count = service
            .agent_shell_store()
            .get(pane)
            .unwrap()
            .transcript_entries;
        let changed_epoch = service.agent_compaction_epoch(pane);
        assert_eq!(changed_count, count + u64::from(variant == "count"));
        assert_eq!(changed_epoch, epoch + u64::from(variant == "render_epoch"));
        assert_eq!(work.read, candidate.read);
        assert_eq!(work.candidate.read, candidate.read);
        assert!(
            service
                .complete_bookkeeping_candidate(work.clone(), checked)
                .unwrap()
        );
        assert!(
            !service
                .complete_bookkeeping_candidate(
                    work,
                    Err(MezError::invalid_state("duplicate must not reenter")),
                )
                .unwrap()
        );
        assert!(!service.persistence.bookkeeping_pending(&conversation));
        assert!(service.claim_bookkeeping_candidates().is_empty());
        let accepted = service
            .persistence
            .pending_transcript_entries_at_path(&path, &conversation)
            .unwrap();
        assert!(!accepted.is_empty());
        assert_eq!(accepted, expected);
        assert_eq!(
            accepted
                .iter()
                .filter(|row| {
                    row.role == TranscriptRole::System
                        && mez_agent::TranscriptContextEvent::from_transcript_content(&row.content)
                            .as_ref()
                            == Some(&expected_prompt)
                })
                .count(),
            1
        );
        assert!(accepted.iter().all(|row| row.validate().is_ok()));
        assert_eq!(
            service
                .agent_shell_store()
                .get(pane)
                .unwrap()
                .transcript_entries,
            changed_count
        );
        assert_eq!(service.agent_compaction_epoch(pane), changed_epoch);
        assert_native_free_transcript_absent(&store, &conversation);
    }
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
