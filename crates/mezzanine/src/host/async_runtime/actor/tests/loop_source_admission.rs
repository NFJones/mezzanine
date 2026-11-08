//! Actual command acceptance, blocking history work and guarded loop binding.
//! Synthetic sources never dispatch inference or native model actions.

use super::super::*;
use super::replay_epoch_publication::setup;
use crate::host::async_runtime::{
    AsyncAgentProviderServiceConfig, run_async_agent_command_service,
};
use crate::host::terminal::TerminalClientLoopAction;
use crate::runtime::{RuntimeAgentCommandAsyncOutcome, RuntimeAgentCommandAsyncWork};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

/// Always releases and joins the owned lease thread, including on assertion
/// failure. The finite fallback also bounds an accidentally inline actor read.
pub(super) struct Lease {
    release: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Lease {
    pub(super) fn hold(root: &std::path::Path) -> Self {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.join(".conversation-locks").join(format!(
                "{}.lock",
                crate::test_support::replay_epoch::CONVERSATION
            )))
            .unwrap();
        rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive).unwrap();
        let (release, wait) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let _ = wait.recv_timeout(Duration::from_secs(10));
            drop(file);
        });
        Self {
            release: Some(release),
            thread: Some(thread),
        }
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

fn claim(service: &mut crate::runtime::RuntimeSessionService) -> RuntimeAgentCommandAsyncWork {
    let dispatch = service.take_pending_deferred_agent_commands().remove(0);
    service
        .claim_agent_command_work(
            &dispatch.primary_client_id,
            &dispatch.pane_id,
            &dispatch.command,
            &dispatch.input,
            dispatch.claim_generation,
            &dispatch.conversation_id,
        )
        .unwrap()
        .unwrap()
}

/// While the actual source lease excludes the blocking reader, the actor must
/// acknowledge acceptance, render and process another pane's input. Only the
/// actual worker result may subsequently switch the original pane binding.
#[tokio::test(flavor = "current_thread")]
async fn loop_source_admission_real_worker_keeps_actor_responsive() {
    let (fixture, mut service, primary) = setup();
    let other = service
        .session_mut_for_tests()
        .split_active_pane(&primary, mez_mux::layout::SplitDirection::Vertical)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume(other.as_str())
        .unwrap();
    let other_conversation = service
        .agent_shell_store()
        .get(other.as_str())
        .unwrap()
        .session_id
        .clone();
    service
        .session_mut_for_tests()
        .select_pane(&primary, "%1")
        .unwrap();
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let done = Arc::new(tokio::sync::Notify::new());
    let settled = Arc::new(tokio::sync::Notify::new());
    service.set_deferred_agent_command_probe_for_tests(started.clone(), release.clone());
    service.set_deferred_agent_command_completion_probe_for_tests(done.clone(), settled.clone());
    let lease = Lease::hold(&fixture.root);
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let client = async {
        let worker_handle = handle.clone();
        let stopping = stop.clone();
        let worker = tokio::spawn(async move {
            run_async_agent_command_service(
                &worker_handle,
                AsyncAgentProviderServiceConfig::new(1)
                    .unwrap()
                    .with_idle_interval(Duration::from_millis(2))
                    .unwrap(),
                move |_, state| {
                    stopping.load(Ordering::Acquire)
                        || matches!(state, crate::runtime::RuntimeLifecycleState::Stopping)
                },
            )
            .await
        });
        let now = Instant::now();
        let ack = tokio::time::timeout(
            Duration::from_secs(3),
            handle.execute_agent_shell_command(
                primary.clone(),
                "/loop --fork --limit 1 inspect original source".into(),
            ),
        )
        .await;
        let elapsed = now.elapsed();
        let claimed = tokio::time::timeout(Duration::from_secs(3), started.notified()).await;
        release.notify_one();
        let physical = tokio::time::timeout(Duration::from_secs(3), started.notified()).await;
        let alive = handle.lifecycle_state().await;
        let selected = handle
            .execute_terminal_command(primary.clone(), format!("select-pane -t {other}"))
            .await;
        let draft = handle
            .apply_attached_terminal_step_plan(
                primary.clone(),
                AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::ForwardToPane(
                        b"unaffected draft".to_vec(),
                    )],
                    output_lines: Vec::new(),
                    output_line_style_spans: Vec::new(),
                    input_hangup: false,
                    output_hangup: false,
                    error_roles: Vec::new(),
                },
            )
            .await;
        drop(lease);
        let returned = tokio::time::timeout(Duration::from_secs(3), done.notified()).await;
        let adopted = tokio::time::timeout(Duration::from_secs(3), settled.notified()).await;
        stop.store(true, Ordering::Release);
        let worker = tokio::time::timeout(Duration::from_secs(3), worker).await;
        let shutdown = handle.shutdown().await;
        (
            ack, elapsed, claimed, physical, alive, selected, draft, returned, adopted, worker,
            shutdown,
        )
    };
    let (
        (
            ack,
            elapsed,
            claimed,
            physical,
            alive,
            selected,
            draft,
            returned,
            adopted,
            worker,
            shutdown,
        ),
        exit,
    ) = tokio::join!(client, actor.run());
    assert!(
        elapsed < Duration::from_millis(500),
        "loop acceptance blocked for {elapsed:?}"
    );
    let ack = ack.unwrap().unwrap();
    let ack: serde_json::Value = serde_json::from_str(&ack).unwrap();
    assert_eq!(ack["command"], "loop");
    assert!(ack["body"].is_null() && ack["turn"].is_null(), "{ack}");
    claimed.unwrap();
    physical.unwrap();
    alive.unwrap();
    selected.unwrap();
    draft.unwrap();
    returned.unwrap();
    adopted.unwrap();
    worker.unwrap().unwrap().unwrap();
    shutdown.unwrap();
    let current = exit.service.agent_shell_store().get("%1").unwrap();
    assert!(current.ephemeral);
    assert_ne!(
        current.session_id,
        crate::test_support::replay_epoch::CONVERSATION
    );
    assert_eq!(
        exit.service
            .agent_loop_state("%1")
            .unwrap()
            .parent_conversation_id,
        crate::test_support::replay_epoch::CONVERSATION
    );
    assert_eq!(
        exit.service
            .agent_shell_store()
            .get(other.as_str())
            .unwrap()
            .session_id,
        other_conversation
    );
}

/// The returned history crosses actual command completion without reading the
/// original source again. Exercise typed and legacy parents in every mode;
/// late legacy appends must not enter the originally admitted raw window.
#[tokio::test(flavor = "current_thread")]
async fn loop_source_admission_adopts_prepared_history_without_source_read() {
    use crate::test_support::replay_epoch::CONVERSATION;
    for legacy in [false, true] {
        for mode in ["", "--fork", "--new"] {
            let (fixture, mut service, primary) = setup();
            let source_root = if legacy {
                fixture.root.join("legacy")
            } else {
                fixture.root.clone()
            };
            let legacy_store = crate::storage::transcript::AgentTranscriptStore::new(&source_root);
            let row = |sequence, content: &str| mez_agent::TranscriptEntry {
                conversation_id: CONVERSATION.into(),
                sequence,
                created_at_unix_seconds: sequence,
                role: mez_agent::TranscriptRole::User,
                turn_id: format!("source-{sequence}"),
                agent_id: "original-agent".into(),
                pane_id: "%1".into(),
                content: content.into(),
            };
            if legacy {
                legacy_store
                    .append(&row(1, "original legacy loop source"))
                    .unwrap();
                service.set_agent_transcript_store(legacy_store.clone());
                service
                    .agent_shell_store_mut()
                    .bind_conversation("%1", CONVERSATION, 1)
                    .unwrap();
            }
            let (_, mut actor) =
                AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
            actor
                .service
                .execute_agent_shell_command(
                    &primary,
                    &format!("/loop {mode} --limit 1 inspect captured source"),
                )
                .unwrap();
            let work = claim(&mut actor.service);
            if legacy {
                legacy_store
                    .append(&row(2, "LATE_PARENT_MUST_NOT_ENTER_LOOP"))
                    .unwrap();
            }
            let result =
                crate::runtime::RuntimeSessionService::execute_deferred_agent_command(&work);
            let lease = Lease::hold(&source_root);
            let now = Instant::now();
            let adopted = actor.service.complete_agent_command_work(&work, result);
            let elapsed = now.elapsed();
            drop(lease);
            assert!(
                elapsed < Duration::from_millis(500),
                "legacy={legacy} mode={mode}: {elapsed:?}"
            );
            assert!(adopted.unwrap());
            let session = actor.service.agent_shell_store().get("%1").unwrap();
            let turn = session.running_turn_id.as_ref().unwrap();
            let context = actor.service.agent_turn_contexts().get(turn).unwrap();
            let contents = context
                .blocks()
                .iter()
                .map(|block| block.content.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                !contents.contains("LATE_PARENT_MUST_NOT_ENTER_LOOP"),
                "legacy={legacy} mode={mode}"
            );
            if mode == "--new" {
                assert!(!contents.contains("original legacy loop source"));
                assert!(context.has_replay_owner());
                let conversation = actor
                    .service
                    .agent_shell_store()
                    .get("%1")
                    .unwrap()
                    .session_id
                    .clone();
                let owner = mez_agent::ReplayEpochContext::from_owned_context(
                    mez_agent::ReplayConversationId::new(conversation).unwrap(),
                    context.clone(),
                )
                .unwrap();
                assert_eq!(owner.revision().get(), 0);
                assert!(owner.head().is_none());
            } else if legacy {
                assert!(contents.contains("original legacy loop source"));
                // The raw migration now gives the original leased projection
                // canonical ownership without creating a summary or reopening
                // later parent rows. It must not retain an unowned legacy mode.
                let owner = mez_agent::ReplayEpochContext::from_owned_context(
                    mez_agent::ReplayConversationId::new(&session.session_id).unwrap(),
                    context.clone(),
                )
                .unwrap();
                assert_eq!(owner.revision().get(), 0);
                assert!(owner.head().is_none());
                assert_eq!(
                    context
                        .blocks()
                        .iter()
                        .filter(|block| block.content == "original legacy loop source")
                        .count(),
                    1
                );
            } else {
                assert!(context.has_replay_owner());
                assert!(contents.contains("original long task"));
            }
        }
    }
}

/// Legacy source preparation keeps its active summary instead of expanding
/// covered raw rows. A newer summary crossing the admitted frontier cannot be
/// shortened or used to smuggle later parent content into that source.
#[tokio::test(flavor = "current_thread")]
async fn loop_source_admission_preserves_legacy_summary_and_refuses_crossing_range() {
    use crate::test_support::replay_epoch::CONVERSATION;
    // Preserve the supported prefix case, explicit selective refusal, and the
    // independent original-frontier guard for an otherwise supported prefix.
    for case in 0..3 {
        let selective = case == 1;
        let crossing = case != 0;
        let (fixture, mut service, primary) = setup();
        let store =
            crate::storage::transcript::AgentTranscriptStore::new(fixture.root.join("legacy"));
        let group = mez_agent::ContextExecutionGroupId::new("original-legacy-loop-group").unwrap();
        for sequence in 1..=3 {
            store
                .append(&mez_agent::TranscriptEntry {
                    conversation_id: CONVERSATION.into(),
                    sequence,
                    created_at_unix_seconds: sequence,
                    role: if sequence == 1 {
                        mez_agent::TranscriptRole::Assistant
                    } else {
                        mez_agent::TranscriptRole::System
                    },
                    turn_id: format!("original-{sequence}"),
                    agent_id: "original-agent".into(),
                    pane_id: "%1".into(),
                    content: if sequence == 1 {
                        format!("original raw row {sequence}")
                    } else {
                        mez_agent::TranscriptContextEvent::execution_block_with_metadata(
                            if sequence == 2 {
                                mez_agent::ContextSourceKind::TranscriptAssistant
                            } else {
                                mez_agent::ContextSourceKind::ActionResult
                            },
                            "original execution",
                            format!("original raw row {sequence}"),
                            group.clone(),
                            sequence - 1,
                            None,
                        )
                        .unwrap()
                        .to_transcript_content()
                    },
                })
                .unwrap();
        }
        store
            .install_legacy_compaction_ranges_fixture(
                CONVERSATION,
                if case == 2 { 3 } else { 1 },
                "original prefix summary",
                if selective {
                    vec![crate::storage::transcript::AgentCompactionRange {
                        first_sequence: 2,
                        through_sequence: 3,
                        summary: "later crossing summary".into(),
                    }]
                } else {
                    Vec::new()
                },
            )
            .unwrap();
        let authority_path = store
            .root()
            .join(CONVERSATION)
            .join("compaction-epoch.json");
        let authority = std::fs::read(&authority_path).unwrap();
        let archive = store.inspect(CONVERSATION).unwrap();
        service.set_agent_transcript_store(store.clone());
        service
            .agent_shell_store_mut()
            .bind_conversation("%1", CONVERSATION, 2)
            .unwrap();
        let (_, mut actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        actor
            .service
            .execute_agent_shell_command(&primary, "/loop --fork inspect exact projection")
            .unwrap();
        let work = claim(&mut actor.service);
        let result = crate::runtime::RuntimeSessionService::execute_deferred_agent_command(&work);
        if crossing {
            assert!(
                matches!(&result, RuntimeAgentCommandAsyncOutcome::Failed {message, ..}
                    if message.contains(if selective { "explicit single-head normalization" }
                    else { "crossed its captured frontier" })),
                "{result:?}"
            );
        } else {
            assert!(
                matches!(&result, RuntimeAgentCommandAsyncOutcome::LoopSource(_)),
                "{result:?}"
            );
        }
        assert!(
            actor
                .service
                .complete_agent_command_work(&work, result)
                .unwrap()
        );
        if crossing {
            assert_eq!(std::fs::read(&authority_path).unwrap(), authority);
            assert_eq!(store.inspect(CONVERSATION).unwrap(), archive);
            assert!(actor.service.agent_loop_state("%1").is_none());
            assert_eq!(
                actor
                    .service
                    .agent_shell_store()
                    .get("%1")
                    .unwrap()
                    .session_id,
                CONVERSATION
            );
        } else {
            let turn = actor
                .service
                .agent_shell_store()
                .get("%1")
                .unwrap()
                .running_turn_id
                .clone()
                .unwrap_or_else(|| {
                    panic!(
                        "loop source adoption failed: {:?}",
                        actor.service.drain_deferred_effects_transition()
                    )
                });
            let context = &actor.service.agent_turn_contexts()[&turn];
            let contents = context
                .blocks()
                .iter()
                .map(|block| block.content.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                contents.contains("original prefix summary")
                    && contents.contains("original raw row 2")
            );
            assert!(
                !contents.contains("original raw row 1")
                    && !contents.contains("original raw row 3")
            );
            assert!(context.has_replay_owner());
        }
    }
}

/// A count-only NoStore binding cannot discover an arbitrary latest typed
/// checkpoint through the new loop worker; it must carry its original capsule.
#[tokio::test(flavor = "current_thread")]
async fn loop_source_admission_refuses_count_only_typed_source() {
    let (_fixture, mut service, primary) = setup();
    service
        .agent_shell_store_mut()
        .bind_ephemeral_conversation_with_lineage_and_transcript_source(
            "%1",
            "unsaved-count-only-loop",
            0,
            None,
            Some(crate::test_support::replay_epoch::CONVERSATION.into()),
            0,
        )
        .unwrap();
    let (_, mut actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    actor
        .service
        .execute_agent_shell_command(&primary, "/loop --fork inspect exact source")
        .unwrap();
    let work = claim(&mut actor.service);
    let result = crate::runtime::RuntimeSessionService::execute_deferred_agent_command(&work);
    assert!(
        matches!(&result, RuntimeAgentCommandAsyncOutcome::Failed {message, ..} if message.contains("requires its original replay checkpoint")),
        "{result:?}"
    );
    assert!(
        actor
            .service
            .complete_agent_command_work(&work, result)
            .unwrap()
    );
    assert!(actor.service.agent_loop_state("%1").is_none());
    assert_eq!(
        actor
            .service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .session_id,
        "unsaved-count-only-loop"
    );
}

/// A cancelled logical command cannot free a worker's physical read permit or
/// bind from its late result. The existing shared source pool remains finite.
#[tokio::test(flavor = "current_thread")]
async fn loop_source_admission_cancelled_reader_retains_capacity_until_retirement() {
    let (fixture, service, primary) = setup();
    let (_, mut actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let _occupied = (0..7)
        .map(|_| actor.service.reserve_agent_source_preparation().unwrap())
        .collect::<Vec<_>>();
    let lease = Lease::hold(&fixture.root);
    actor
        .service
        .execute_agent_shell_command(&primary, "/loop --fork inspect cancellation")
        .unwrap();
    let work = claim(&mut actor.service);
    let (started, ready) = tokio::sync::oneshot::channel();
    let worker = tokio::task::spawn_blocking(move || {
        let _ = started.send(());
        let result = crate::runtime::RuntimeSessionService::execute_deferred_agent_command(&work);
        (work, result)
    });
    ready.await.unwrap();
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    let bounded = actor.service.reserve_agent_source_preparation().is_err();
    actor.service.use_agent_source_preparation_adapter();
    let registration_bounded = actor.service.reserve_agent_source_preparation().is_err();
    let waiting = !worker.is_finished();
    drop(lease);
    let (work, result) = worker.await.unwrap();
    assert!(bounded && registration_bounded && waiting);
    assert!(
        !actor
            .service
            .complete_agent_command_work(&work, result)
            .unwrap()
    );
    drop(work);
    assert!(actor.service.reserve_agent_source_preparation().is_ok());
    assert!(actor.service.agent_loop_state("%1").is_none());
    assert_eq!(
        actor
            .service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .session_id,
        crate::test_support::replay_epoch::CONVERSATION
    );
}

/// Both invalidation points discard the original result rather than rebuilding
/// history or binding a different pane/configuration under its old claim.
#[tokio::test(flavor = "current_thread")]
async fn loop_source_admission_fences_config_and_binding_replacement() {
    for replacement in [false, true] {
        let (_fixture, service, primary) = setup();
        let (_, mut actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        actor
            .service
            .execute_agent_shell_command(&primary, "/loop --fork inspect original")
            .unwrap();
        let work = claim(&mut actor.service);
        let outcome = crate::runtime::RuntimeSessionService::execute_deferred_agent_command(&work);
        if replacement {
            actor
                .service
                .agent_shell_store_mut()
                .bind_conversation("%1", "replacement-conversation", 0)
                .unwrap();
        } else {
            actor
                .service
                .replace_config_layers(vec![crate::config::ConfigLayer {
                    name: "replacement".into(),
                    path: None,
                    format: crate::config::ConfigFormat::Toml,
                    scope: crate::config::ConfigScope::Primary,
                    trusted: true,
                    text: "[agents]\nshell_mode=\"pane\"\n".into(),
                }])
                .unwrap();
        }
        let before = actor.service.agent_shell_store().get("%1").unwrap().clone();
        actor
            .service
            .complete_agent_command_work(&work, outcome)
            .unwrap();
        assert_eq!(
            actor
                .service
                .agent_shell_store()
                .get("%1")
                .unwrap()
                .session_id,
            before.session_id
        );
        assert!(actor.service.agent_loop_state("%1").is_none());
        assert!(
            actor
                .service
                .agent_shell_store()
                .get("%1")
                .unwrap()
                .running_turn_id
                .is_none()
        );
    }
}

/// Damaged required state fails this command visibly without a partial loop or
/// loss of the original binding; ordinary control remains usable afterward.
#[tokio::test(flavor = "current_thread")]
async fn loop_source_admission_corrupt_source_is_local_failure() {
    let (fixture, service, primary) = setup();
    let (_, mut actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    std::fs::write(
        fixture
            .root
            .join(crate::test_support::replay_epoch::CONVERSATION)
            .join("compaction-epoch.json"),
        b"broken required authority",
    )
    .unwrap();
    actor
        .service
        .execute_agent_shell_command(&primary, "/loop --fork inspect corruption")
        .unwrap();
    let work = claim(&mut actor.service);
    let outcome = crate::runtime::RuntimeSessionService::execute_deferred_agent_command(&work);
    assert!(matches!(
        outcome,
        RuntimeAgentCommandAsyncOutcome::Failed { .. }
    ));
    assert!(
        actor
            .service
            .complete_agent_command_work(&work, outcome)
            .unwrap()
    );
    assert!(actor.service.agent_loop_state("%1").is_none());
    assert_eq!(
        actor
            .service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .session_id,
        crate::test_support::replay_epoch::CONVERSATION
    );
    actor
        .service
        .execute_agent_shell_command(&primary, "/directive still available")
        .unwrap();
    assert!(!actor.service.agent_command_is_active("%1"));
}
