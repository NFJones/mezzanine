//! Actual terminal settlement, existing history lane and guarded next iteration.
//! An offline final response requests another goal iteration without native
//! actions or paid inference. Original typed history crosses the real worker.

use super::super::*;
use super::loop_source_admission::Lease;
use super::replay_epoch_publication::setup;
use crate::integrations::agent::provider::ModelProvider;
use crate::runtime::{RuntimeAgentHistoryPreparationDispatch, RuntimeSessionService};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

struct UnmetGoal(AtomicUsize);
impl ModelProvider for UnmetGoal {
    fn provider_id(&self) -> &str {
        "runtime-batch"
    }
    fn send_request(&self, _: &mez_agent::ModelRequest) -> Result<mez_agent::ModelResponse> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(mez_agent::ModelResponse {
            provider: "runtime-batch".into(),
            model: "test".into(),
            raw_text: "one actual completed iteration\n[loop goal: unmet]".into(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "offline goal assessment".into(),
                actions: vec![mez_agent::AgentAction {
                    id: "goal-assessment".into(),
                    payload: mez_agent::AgentActionPayload::Say {
                        status: mez_agent::SayStatus::Final,
                        text: "one actual completed iteration\n[loop goal: unmet]".into(),
                        content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.into(),
                    },
                }],
            }),
            provider_transcript_events: Vec::new(),
        })
    }
}

/// Only source preparation is async during the deterministic first provider
/// execution. That real terminal cleanup queues the next iteration; it must
/// leave the running slot empty and inference count at one until source ACK.
fn queued(
    probe: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
) -> (
    crate::test_support::replay_epoch::Fixture,
    RuntimeSessionService,
    ClientId,
    String,
) {
    queued_with_policy(probe, false)
}

/// The same actual terminal producer runs against saved and frozen NoStore
/// ownership. NoStore captures before rebinding and never claims durable ACK.
fn queued_with_policy(
    probe: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
    ephemeral: bool,
) -> (
    crate::test_support::replay_epoch::Fixture,
    RuntimeSessionService,
    ClientId,
    String,
) {
    let (fixture, mut service, primary) = setup();
    if ephemeral {
        let frozen = service.freeze_agent_replay_history("%1").unwrap().unwrap();
        let target = String::from(mez_agent::ContextBlockOccurrenceId::allocate());
        service
            .bind_ephemeral_replay_history("%1", &target, &frozen, None)
            .unwrap();
    }
    service
        .execute_agent_shell_command(
            &primary,
            "/loop --limit 3 --goal 'finish the objective' inspect exact reuse source",
        )
        .unwrap();
    let predecessor = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .running_turn_id
        .clone()
        .unwrap();
    let profile = service
        .agent_turn_model_profile(&predecessor)
        .unwrap()
        .clone();
    service.use_agent_source_preparation_adapter();
    if let Some((started, release)) = probe {
        service.set_prompt_history_preparation_probe_for_tests(started, release);
    }
    let provider = UnmetGoal(AtomicUsize::new(0));
    let execution = service
        .execute_agent_turn_with_provider(&predecessor, &provider, profile)
        .unwrap();
    assert_eq!(
        execution.terminal_state,
        mez_agent::AgentTurnState::Completed
    );
    assert_eq!(provider.0.load(Ordering::SeqCst), 1);
    assert_eq!(service.agent_loop_state("%1").unwrap().iteration, 2);
    assert!(
        service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .running_turn_id
            .is_none()
    );
    (fixture, service, primary, predecessor)
}

fn take(service: &mut RuntimeSessionService) -> crate::runtime::LoopSourceWork {
    let mut pending = service.take_pending_agent_history_preparations();
    assert_eq!(pending.len(), 1);
    let RuntimeAgentHistoryPreparationDispatch::LoopContinuation(work) = pending.remove(0) else {
        panic!("internal continuation was submitted as a user prompt");
    };
    assert!(service.claim_loop_continuation_source(&work).unwrap());
    work
}

/// The actual actor's existing history drain starts the blocking source worker.
/// The physical flock excludes that read while actor control remains usable;
/// release admits exactly the next controller iteration without HTTP/actions.
#[tokio::test(flavor = "current_thread")]
async fn loop_reuse_source_real_worker_keeps_actor_responsive() {
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (fixture, service, primary, predecessor) = queued(Some((started.clone(), release.clone())));
    let lease = Lease::hold(&fixture.root);
    let (handle, mut actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let now = Instant::now();
    actor.dispatch_pending_agent_prompt_history();
    assert!(now.elapsed() < Duration::from_millis(500));
    let client = async {
        let claimed = tokio::time::timeout(Duration::from_secs(3), started.notified()).await;
        release.notify_one();
        let physical = tokio::time::timeout(Duration::from_secs(3), started.notified()).await;
        let alive = handle.lifecycle_state().await;
        let listed = handle
            .execute_terminal_command(primary, "list-panes".into())
            .await;
        let before = handle.pending_agent_provider_tasks().await;
        drop(lease);
        let ready = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let tasks = handle.pending_agent_provider_tasks().await.unwrap();
                if tasks.iter().any(|task| task.turn_id != predecessor) {
                    break tasks;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        let shutdown = handle.shutdown().await;
        (claimed, physical, alive, listed, before, ready, shutdown)
    };
    let ((claimed, physical, alive, listed, before, ready, shutdown), exit) =
        tokio::join!(client, actor.run());
    claimed.unwrap();
    physical.unwrap();
    alive.unwrap();
    listed.unwrap();
    shutdown.unwrap();
    assert!(
        before
            .unwrap()
            .iter()
            .all(|task| task.turn_id == predecessor)
    );
    let diagnostic = exit
        .service
        .pane_screen("%1")
        .map(|screen| {
            screen
                .normal_content_lines()
                .into_iter()
                .filter(|line| {
                    line.contains("loop:") || line.contains("failed") || line.contains("error")
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let next = ready
        .unwrap_or_else(|error| panic!("next iteration not ready: {error}; {diagnostic}"))
        .into_iter()
        .find(|task| task.turn_id != predecessor)
        .unwrap();
    assert_eq!(
        exit.service
            .agent_loop_turn(&next.turn_id)
            .unwrap()
            .iteration,
        2
    );
    assert_eq!(
        exit.service
            .agent_turn_ledger()
            .turn(&predecessor)
            .unwrap()
            .state,
        mez_agent::AgentTurnState::Completed
    );
    let context = &exit.service.agent_turn_contexts()[&next.turn_id];
    assert!(context.has_replay_owner());
    assert!(
        context
            .blocks()
            .iter()
            .any(|block| block.content.contains("[loop goal: unmet]"))
    );
}

/// Adoption cannot read the original source under its held lease. Duplicate
/// original reads/callbacks leave the already accepted next turn/controller intact.
#[tokio::test(flavor = "current_thread")]
async fn loop_reuse_source_adoption_is_read_free_and_duplicate_is_inert() {
    let (fixture, service, _, predecessor) = queued(None);
    let (_, mut actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let service = &mut actor.service;
    let work = take(service);
    let read = work.execute();
    let duplicate = work.execute();
    assert!(read.is_ok(), "{read:?}");
    let turns = service.agent_turn_ledger().turns().len();
    let lease = Lease::hold(&fixture.root);
    let now = Instant::now();
    let adopted = service.complete_loop_continuation_source(&work, read);
    let elapsed = now.elapsed();
    drop(lease);
    assert!(elapsed < Duration::from_millis(500));
    assert!(adopted.unwrap());
    let next = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .running_turn_id
        .clone()
        .unwrap();
    assert_ne!(next, predecessor);
    assert!(
        !service
            .complete_loop_continuation_source(&work, duplicate)
            .unwrap()
    );
    assert_eq!(service.agent_turn_ledger().turns().len(), turns + 1);
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .running_turn_id
            .as_deref(),
        Some(next.as_str())
    );
    assert_eq!(service.agent_loop_state("%1").unwrap().iteration, 2);
}

/// Terminal export must retain each imported original occurrence in its source
/// domain and account new diagnostics once. The actual second history read
/// validates those rows against unchanged escrow and preserves their records.
#[tokio::test(flavor = "current_thread")]
async fn loop_reuse_source_terminal_export_preserves_originals_and_accounts_recovery() {
    use crate::test_support::replay_epoch::CONVERSATION;
    use mez_agent::transcript::{TranscriptContextEvent, TranscriptContextRecord};
    let (fixture, mut service, _, predecessor) = queued(None);
    let original = fixture
        .source
        .context()
        .chronology()
        .iter()
        .find(|event| event.sequence() == fixture.instruction)
        .unwrap()
        .record()
        .occurrence_id();
    let rows = fixture.store.inspect(CONVERSATION).unwrap();
    let mut recovery = Vec::new();
    let mut users = 0;
    for row in rows
        .iter()
        .filter(|row| row.role == mez_agent::TranscriptRole::System)
    {
        let Some(record) = TranscriptContextRecord::from_transcript_content(&row.content).unwrap()
        else {
            assert!(
                TranscriptContextEvent::from_transcript_content(&row.content).is_none(),
                "unaccounted semantic row: {}",
                row.content
            );
            continue;
        };
        assert_ne!(
            record.record().occurrence_id(),
            original,
            "imported user occurrence was reattributed to the current producer"
        );
        match record.event() {
            TranscriptContextEvent::TaskAdmission { .. } => {
                users += 1;
                assert_eq!(row.turn_id, predecessor);
            }
            TranscriptContextEvent::ExecutionBlock {
                label,
                execution_group_id,
                ordinal,
                ..
            } if label == "canonical recovery facts" => {
                assert!(execution_group_id.is_some());
                assert_eq!(*ordinal, Some(1));
                recovery.push(record.record());
            }
            _ => {}
        }
    }
    assert_eq!(users, 1);
    assert_eq!(recovery.len(), 1);
    let before = std::fs::read(fixture.store.transcript_path(CONVERSATION).unwrap()).unwrap();
    let work = take(&mut service);
    assert!(work.execute().is_ok());
    assert!(work.execute().is_ok());
    assert_eq!(
        std::fs::read(fixture.store.transcript_path(CONVERSATION).unwrap()).unwrap(),
        before
    );
    let (identity, _) = fixture
        .store
        .load_replay_epoch(CONVERSATION, mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_eq!(
        identity, fixture.identity,
        "terminal diagnostics must not publish a new head"
    );
}

/// Source failure retires the controller locally and preserves the completed
/// predecessor/binding. It cannot rerun that provider or poison inline control.
#[tokio::test(flavor = "current_thread")]
async fn loop_reuse_source_corruption_is_local_failure() {
    let (fixture, mut service, primary, predecessor) = queued(None);
    let work = take(&mut service);
    std::fs::write(
        fixture
            .root
            .join(crate::test_support::replay_epoch::CONVERSATION)
            .join("compaction-epoch.json"),
        b"broken required source",
    )
    .unwrap();
    let read = work.execute();
    assert!(read.is_err());
    assert!(
        service
            .complete_loop_continuation_source(&work, read)
            .unwrap()
    );
    assert!(service.agent_loop_state("%1").is_none());
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&predecessor)
            .unwrap()
            .state,
        mez_agent::AgentTurnState::Completed
    );
    assert!(
        service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .running_turn_id
            .is_none()
    );
    service
        .execute_agent_shell_command(&primary, "/directive unaffected inline control")
        .unwrap();
}

/// Exact cancellation/configuration/binding/controller fences remain live after
/// the read. Failure removes only the original controller; a different current
/// controller or independently replaced conversation is never adopted over.
#[tokio::test(flavor = "current_thread")]
async fn loop_reuse_source_late_results_preserve_replacement_ownership() {
    for change in ["stop", "config", "binding", "controller"] {
        let (_fixture, mut service, primary, predecessor) = queued(None);
        let work = take(&mut service);
        let read = work.execute();
        assert!(read.is_ok(), "{change}: {read:?}");
        let turns = service.agent_turn_ledger().turns().len();
        match change {
            "stop" => {
                service
                    .execute_agent_shell_command(&primary, "/stop")
                    .unwrap();
            }
            "config" => {
                service.replace_config_layers(Vec::new()).unwrap();
            }
            "binding" => {
                service
                    .agent_shell_store_mut()
                    .bind_conversation("%1", "replacement-reuse-conversation", 0)
                    .unwrap();
            }
            "controller" => {
                let id = service.agent_loop_state("%1").unwrap().loop_id.clone();
                service.agent_loop_state_mut_by_id(&id).unwrap().iteration += 1;
            }
            _ => unreachable!(),
        }
        let _ = service
            .complete_loop_continuation_source(&work, read)
            .unwrap();
        assert_eq!(service.agent_turn_ledger().turns().len(), turns, "{change}");
        assert_eq!(
            service
                .agent_turn_ledger()
                .turn(&predecessor)
                .unwrap()
                .state,
            mez_agent::AgentTurnState::Completed
        );
        assert!(
            service
                .agent_shell_store()
                .get("%1")
                .unwrap()
                .running_turn_id
                .is_none()
        );
        if change == "controller" {
            assert_eq!(service.agent_loop_state("%1").unwrap().iteration, 3);
        } else {
            assert!(service.agent_loop_state("%1").is_none(), "{change}");
        }
        if change == "binding" {
            assert_eq!(
                service.agent_shell_store().get("%1").unwrap().session_id,
                "replacement-reuse-conversation"
            );
        }
    }
}

/// A second pane can accept a genuine interruption candidate for the same
/// conversation after the primary worker read, without changing its binding
/// counters. That original chronology custody must still fence adoption.
#[tokio::test(flavor = "current_thread")]
async fn runtime_loop_reuse_source_rechecks_late_bookkeeping_at_adoption() {
    let (_fixture, service, primary, predecessor) = queued(None);
    let (_, mut actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let service = &mut actor.service;
    let work = take(service);
    let read = work.execute();
    assert!(read.is_ok(), "{read:?}");
    let conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let other = service
        .session_mut_for_tests()
        .split_active_pane(&primary, mez_mux::layout::SplitDirection::Vertical)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume(other.as_str())
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation(other.as_str(), &conversation, 0)
        .unwrap();
    service
        .session_mut_for_tests()
        .select_pane(&primary, other.as_str())
        .unwrap();
    service
        .begin_agent_prompt_history_preparation(
            primary.clone(),
            other.as_str(),
            "retain this actual interrupted secondary prompt",
        )
        .unwrap();
    let prompt = service
        .take_pending_agent_prompt_history_for_tests()
        .pop()
        .unwrap();
    assert!(service.claim_agent_prompt_history_preparation(&prompt));
    let history =
        crate::runtime::execute_runtime_agent_prompt_history_work(prompt.history_work.clone());
    assert!(
        service
            .complete_agent_prompt_history_preparation(&prompt, history)
            .unwrap()
    );
    service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    assert!(service.check_agent_history_admission("%1").is_err());
    assert!(
        service
            .validate_loop_source_work(&work)
            .unwrap_err()
            .message()
            .contains("bookkeeping must settle")
    );
    let turns = service.agent_turn_ledger().turns().len();
    assert!(
        service
            .complete_loop_continuation_source(&work, read)
            .unwrap()
    );
    assert!(service.agent_loop_state("%1").is_none());
    assert!(
        service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .running_turn_id
            .is_none()
    );
    assert_eq!(service.agent_turn_ledger().turns().len(), turns);
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&predecessor)
            .unwrap()
            .state,
        mez_agent::AgentTurnState::Completed
    );
    assert_eq!(
        service.claim_bookkeeping_candidates().len(),
        1,
        "original unchecked candidate was consumed by loop failure"
    );
}

/// Actual NoStore terminal cleanup retains the latest canonical body. Reuse
/// continuation consumes its accounted recovery observation once without an
/// archive read/write or a durable head for the ephemeral target.
#[tokio::test(flavor = "current_thread")]
async fn loop_reuse_source_no_store_preserves_latest_body_and_parent() {
    use crate::test_support::replay_epoch::CONVERSATION;
    let (fixture, mut service, _, predecessor) = queued_with_policy(None, true);
    let target = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    assert_ne!(target, CONVERSATION);
    assert!(service.agent_shell_store().get("%1").unwrap().ephemeral);
    let preview = service
        .agent_context_for_pane_prompt("%1", "inspect latest frozen body", 0)
        .unwrap();
    let recovery = preview
        .chronology()
        .iter()
        .filter(|event| {
            event.block().source == mez_agent::ContextSourceKind::CommittedEvidence
                && event.block().label == "canonical recovery facts"
        })
        .map(|event| (event.block().clone(), event.record()))
        .collect::<Vec<_>>();
    assert_eq!(recovery.len(), 1);
    let (_, mut actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let work = take(&mut actor.service);
    let lease = Lease::hold(&fixture.root);
    let now = Instant::now();
    let read = work.execute();
    assert!(read.is_ok(), "{read:?}");
    actor
        .service
        .complete_loop_continuation_source(&work, read)
        .unwrap();
    assert!(now.elapsed() < Duration::from_millis(500));
    drop(lease);
    let next = actor
        .service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .running_turn_id
        .as_ref()
        .unwrap();
    assert_ne!(next, &predecessor);
    let context = &actor.service.agent_turn_contexts()[next];
    let actual = context
        .chronology()
        .iter()
        .filter(|event| {
            event.block().source == mez_agent::ContextSourceKind::CommittedEvidence
                && event.block().label == "canonical recovery facts"
        })
        .map(|event| (event.block().clone(), event.record()))
        .collect::<Vec<_>>();
    assert_eq!(actual, recovery);
    assert!(context.has_replay_owner());
    assert!(fixture.store.inspect(CONVERSATION).unwrap().is_empty());
    assert!(
        !fixture
            .root
            .join(&target)
            .join("compaction-epoch.json")
            .exists()
    );
    assert_eq!(
        fixture
            .store
            .load_replay_epoch(CONVERSATION, mez_agent::AgentContext::empty())
            .unwrap()
            .unwrap()
            .0,
        fixture.identity
    );
}

/// Exact stale-claim cancellation and actual runtime shutdown release retained
/// map entries when queued work retires, with no source read or new turn.
#[tokio::test(flavor = "current_thread")]
async fn runtime_loop_reuse_source_queued_cancellation_retires_without_read() {
    for shutdown in [false, true] {
        let (_fixture, service, _, _) = queued(None);
        let (_, mut actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        let held = (0..7)
            .map(|_| actor.service.reserve_agent_source_preparation().unwrap())
            .collect::<Vec<_>>();
        let pending = actor
            .service
            .take_pending_agent_history_preparations()
            .pop()
            .unwrap();
        let RuntimeAgentHistoryPreparationDispatch::LoopContinuation(work) = pending else {
            panic!("internal continuation was submitted as a user prompt");
        };
        let turns = actor.service.agent_turn_ledger().turns().len();
        if shutdown {
            actor
                .service
                .apply_supervisor_shutdown_event("queued source cancellation test", false)
                .unwrap();
        } else {
            actor.service.replace_config_layers(Vec::new()).unwrap();
        }
        assert!(!actor.service.claim_loop_continuation_source(&work).unwrap());
        assert_eq!(actor.service.agent_turn_ledger().turns().len(), turns);
        if !shutdown {
            assert!(actor.service.agent_loop_state("%1").is_none());
        }
        drop(work);
        assert!(actor.service.reserve_agent_source_preparation().is_ok());
        drop(held);
    }
}

/// A cancelled physical reader retains its source slot until the actual work
/// and late result retire, including when the logical controller already ended.
#[tokio::test(flavor = "current_thread")]
async fn loop_reuse_source_cancelled_reader_retains_capacity() {
    let (fixture, service, primary, _) = queued(None);
    let (_, mut actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let service = &mut actor.service;
    let held = (0..7)
        .map(|_| service.reserve_agent_source_preparation().unwrap())
        .collect::<Vec<_>>();
    let work = take(service);
    let lease = Lease::hold(&fixture.root);
    let (started, ready) = tokio::sync::oneshot::channel();
    let worker = tokio::task::spawn_blocking(move || {
        let _ = started.send(());
        let read = work.execute();
        (work, read)
    });
    ready.await.unwrap();
    service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    assert!(service.agent_loop_state("%1").is_none());
    let bounded = service.reserve_agent_source_preparation().is_err();
    let waiting = !worker.is_finished();
    drop(lease);
    let (work, read) = worker.await.unwrap();
    assert!(bounded && waiting);
    assert!(
        !service
            .complete_loop_continuation_source(&work, read)
            .unwrap()
    );
    drop(work);
    drop(held);
    assert!(service.reserve_agent_source_preparation().is_ok());
}
