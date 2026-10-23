//! Native/provider-free objective publication through the real actor owner.
//!
//! A separate OS-thread watchdog owns the real store lock and always releases
//! it, including on unwind. Tokio timers are not the rescue mechanism: the
//! baseline actor blocks its executor in flock, so only pre-release results
//! qualify as responsiveness evidence.

use super::super::*;
use rustix::fs::{FlockOperation, flock};

/// Owns one actual advisory lock and an independent bounded release watchdog.
struct MirrorLockWatchdog {
    release: Option<std::sync::mpsc::SyncSender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    released: StdArc<AtomicBool>,
}

impl MirrorLockWatchdog {
    /// Releases on the normal test path; Drop still joins the independent owner.
    fn release_now(&self) {
        if let Some(release) = &self.release {
            let _ = release.try_send(());
        }
    }

    /// Acquires the store's real mirror lock before starting the watchdog.
    fn hold(root: &Path) -> Self {
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join(".session-objectives.json.lock"))
            .unwrap();
        flock(&lock, FlockOperation::LockExclusive).unwrap();
        let (release, receiver) = std::sync::mpsc::sync_channel(1);
        let released = StdArc::new(AtomicBool::new(false));
        let observed = released.clone();
        let thread = std::thread::spawn(move || {
            let _ = receiver.recv_timeout(Duration::from_secs(3));
            // Flag before releasing the descriptor: false proves the lock is
            // still held, never a late runtime result after watchdog rescue.
            observed.store(true, Ordering::Release);
            drop(lock);
        });
        Self {
            release: Some(release),
            thread: Some(thread),
            released,
        }
    }
}

impl Drop for MirrorLockWatchdog {
    /// Releases and joins without replacing the test's primary panic.
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.try_send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A completed ordinary response must publish its in-memory objective and let
/// an unrelated actor read and cancellation settle BEFORE another descriptor
/// releases the objective cache lock. The fake execution uses the actual claim
/// and result-adoption path, but creates no shell, provider, MCP or sandbox job.
#[tokio::test(flavor = "current_thread")]
async fn async_objective_mirror_lock_does_not_stall_provider_adoption_or_actor() {
    objective_mirror_actor_case(false).await;
}

/// Objective-only traffic must retry a busy real lock and converge after its
/// release through the existing persistence worker's bounded idle polls. No
/// later provider publication, user kick or runtime timer schedules the retry.
#[tokio::test(flavor = "current_thread")]
async fn async_objective_only_cache_converges_after_busy_lock_release() {
    objective_mirror_actor_case(true).await;
}

/// Shares the exact native-free provider adoption with or without the real
/// persistence service; only the latter may execute the optional mirror job.
async fn objective_mirror_actor_case(run_service: bool) {
    let root = std::env::temp_dir().join(format!(
        "mez-objective-lock-{:032x}",
        rand::random::<u128>()
    ));
    std::fs::create_dir(&root).unwrap();
    let store = AgentTranscriptStore::new(root.clone());
    let mut service = test_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "objective-lock".into(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\nsession_title_policy=\"objective\"\n".into(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let conversation = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    // Direct turn setup intentionally avoids the ordinary prompt/native path;
    // register its real pane identity before the same publication owner runs.
    service
        .ensure_runtime_message_identity(
            "agent-%1",
            mez_core::ids::PaneId::opaque("%1"),
            "agent",
            &["agent-harness"],
            1,
        )
        .unwrap();
    let turn = mez_agent::AgentTurnRecord {
        turn_id: "objective-turn".into(),
        conversation_id: conversation.clone(),
        agent_id: "agent-%1".into(),
        pane_id: "%1".into(),
        trigger: mez_agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 1,
        deadline_at_unix_millis: 0,
        policy_profile: "default".into(),
        model_profile: "default".into(),
        parent_turn_id: None,
        state: mez_agent::AgentTurnState::Running,
        cooperation_mode: None,
        initial_capability: None,
    };
    service.start_agent_turn(turn.clone()).unwrap();
    let profile = mez_agent::ModelProfile {
        provider: "openai".into(),
        model: "gpt-fixture".into(),
        model_capabilities: Default::default(),
        reasoning_profile: None,
        latency_preference: None,
        multimodal_required: false,
        provider_options: Default::default(),
        safety_tier: None,
    };
    let context = mez_agent::AgentContext::new(vec![mez_agent::ContextBlock::user_event(
        "user prompt",
        "continue native-free fixture",
    )])
    .unwrap();
    let request = crate::integrations::agent::context::assemble_model_request(
        &profile,
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
        &turn,
        &context,
    )
    .unwrap();
    service
        .agent_turn_contexts_mut()
        .insert(turn.turn_id.clone(), context);
    service.set_agent_turn_model_profile(turn.turn_id.clone(), profile.clone());
    service.queue_agent_provider_task(turn.turn_id.clone());
    service.set_agent_transcript_store(store.clone());
    assert!(service.tracked_runtime_pane_process_ids().is_empty());
    let action = mez_agent::AgentAction {
        id: "say-objective".into(),
        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Progress,
            text: "fixture progress".into(),
            content_type: "text/plain; charset=utf-8".into(),
        },
    };
    let execution = mez_agent::AgentTurnExecution {
        request,
        response: mez_agent::ModelResponse {
            provider: profile.provider,
            model: profile.model,
            raw_text: r#"{"objective":"new durable objective","rationale":"fixture","actions":[]}"#
                .into(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "fixture".into(),
                actions: vec![action.clone()],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: vec![mez_agent::ActionResult::succeeded(
            &turn,
            &action,
            vec!["fixture progress".into()],
            None,
        )],
        final_turn: false,
        terminal_state: mez_agent::AgentTurnState::Running,
    };
    let watchdog = MirrorLockWatchdog::hold(&root);
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let stop = AtomicBool::new(false);
    let stopped = tokio::sync::Notify::new();
    let persistence = async {
        if !run_service {
            return None;
        }
        let report = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 1_000,
                drain_limit: 16,
                idle_interval: Duration::from_millis(5),
            },
            |_, _| stop.load(Ordering::Acquire),
        )
        .await;
        stopped.notify_one();
        Some(report)
    };
    let client = async {
        handle
            .record_claimed_agent_provider_task_for_tests(turn.turn_id.clone(), 1)
            .await
            .unwrap();
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Completed {
            agent_id: AgentId::opaque(turn.agent_id.clone()).unwrap(),
            turn_id: turn.turn_id.clone(),
            claim_generation: 1,
            execution: Box::new(execution),
        }));
        let adoption = handle.submit_runtime_events(batch).await;
        let adopted_before_release = !watchdog.released.load(Ordering::Acquire);
        let read = handle.lifecycle_state().await;
        let read_before_release = !watchdog.released.load(Ordering::Acquire);
        let cancel = handle
            .execute_agent_shell_command(primary, "/stop".into())
            .await;
        let cancel_before_release = !watchdog.released.load(Ordering::Acquire);
        let busy_observed = if run_service {
            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    let status = handle.metrics().await.unwrap().objective_mirror;
                    if watchdog.released.load(Ordering::Acquire) {
                        break false;
                    }
                    if status.busy > 0 && status.pending_keys > 0 {
                        break true;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap_or(false)
        } else {
            true
        };
        let convergence = if run_service {
            watchdog.release_now();
            let observed = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let status = handle.metrics().await.unwrap().objective_mirror;
                    if status.confirmed > 0
                        && status.pending_keys == 0
                        && status.slot_phase == "idle"
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await;
            stop.store(true, Ordering::Release);
            stopped.notified().await;
            observed
        } else {
            Ok(())
        };
        let shutdown = handle.shutdown().await;
        (
            adoption,
            adopted_before_release,
            read,
            read_before_release,
            cancel,
            cancel_before_release,
            busy_observed,
            convergence,
            shutdown,
        )
    };
    let (
        (
            adoption,
            adopted,
            read,
            responsive,
            cancel,
            cancelled,
            busy_observed,
            convergence,
            shutdown,
        ),
        persistence,
        mut exit,
    ) = tokio::join!(client, persistence, actor.run());
    // Release and join before any assertions or filesystem cleanup, including
    // the baseline failure. Drop is also the unwind fallback for early errors.
    drop(watchdog);
    let idle = !run_service || settle::settle_objective_mirrors(&mut exit.service);
    let mirrored = if run_service && idle {
        store
            .session_objective_mirror(&conversation)
            .unwrap()
            .map(|record| record.objective)
    } else {
        None
    };
    if idle {
        std::fs::remove_dir_all(&root).unwrap();
    }
    assert_eq!(adoption.unwrap().applied, 1);
    read.unwrap();
    cancel.unwrap();
    shutdown.unwrap();
    convergence.unwrap();
    assert!(
        busy_observed,
        "retry qualification must observe Busy while the real lock is held"
    );
    if let Some(report) = persistence {
        assert_eq!(report.unwrap().failed, 0);
    }
    assert!(
        idle,
        "optional mirror worker did not settle; retained fixture root {}",
        root.display()
    );
    if run_service {
        assert_eq!(mirrored.as_deref(), Some("new durable objective"));
    }
    assert!(
        adopted && responsive && cancelled,
        "objective cache lock stalled actor until release: adoption={adopted} read={responsive} cancel={cancelled}"
    );
    assert!(exit.service.tracked_runtime_pane_process_ids().is_empty());
    assert_eq!(
        exit.service
            .message_service_mut()
            .registered_identity(&AgentId::opaque("agent-%1").unwrap())
            .and_then(|identity| identity.objective.as_deref()),
        Some("new durable objective")
    );
}

/// Optional work already admitted past its final revision observation must not
/// delay an essential write/completion batch. Stopping its launching service
/// and replacing the actor/service preserves physical custody, then a newer
/// objective converges without replaying the essential write. The probe has an
/// independent blocking-thread deadline; its release guard also handles unwind.
#[tokio::test(flavor = "current_thread")]
async fn async_objective_write_does_not_starve_essential_persistence_or_replacement() {
    struct Release(std::sync::mpsc::SyncSender<()>);
    impl Drop for Release {
        fn drop(&mut self) {
            let _ = self.0.try_send(());
        }
    }
    let root = std::env::temp_dir().join(format!(
        "mez-mirror-essential-{:032x}",
        rand::random::<u128>()
    ));
    std::fs::create_dir(&root).unwrap();
    let store = AgentTranscriptStore::new(root.clone());
    let mut service = test_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "objective-essential".into(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\nsession_title_policy=\"objective\"\n".into(),
        }])
        .unwrap();
    service.set_agent_transcript_store(store.clone());
    let (entered, waiting) = std::sync::mpsc::sync_channel(1);
    let (release, gate) = std::sync::mpsc::sync_channel(1);
    let release = Release(release);
    let released = StdArc::new(AtomicBool::new(false));
    service.gate_objective_mirror_write_for_tests(entered, gate, released.clone());
    assert!(service.mirror_runtime_agent_objective("cache-only", Some("older admitted")));
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let stop = AtomicBool::new(false);
    let stopped = tokio::sync::Notify::new();
    let persistence = async {
        let report = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 1_000,
                drain_limit: 16,
                idle_interval: Duration::from_millis(5),
            },
            |_, _| stop.load(Ordering::Acquire),
        )
        .await;
        stopped.notify_one();
        report
    };
    let path = root.join("essential-write");
    let client = async {
        tokio::time::timeout(Duration::from_secs(2), async {
            while waiting.try_recv().is_err() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        let entered_held = !released.load(Ordering::Acquire);
        let admitted = handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::Persist {
                target: PersistenceTarget::Config,
                path: path.clone(),
                bytes: b"essential once".to_vec(),
                mode: PersistenceWriteMode::CreateNew,
            }])
            .await;
        let written = tokio::time::timeout(Duration::from_secs(1), async {
            while tokio::fs::read(&path).await.is_err() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await;
        stop.store(true, Ordering::Release);
        stopped.notified().await;
        let essential_before_release = !released.load(Ordering::Acquire);
        let status = handle.metrics().await.unwrap().objective_mirror;
        let shutdown = handle.shutdown().await;
        (
            entered_held,
            admitted,
            written,
            essential_before_release,
            status,
            shutdown,
        )
    };
    let ((entered_held, admitted, written, essential_held, status, shutdown), persisted, mut exit) =
        tokio::join!(client, persistence, actor.run());
    // Keep the original physical job held while replacing its launcher/actor.
    // A newer pending value cannot free the old slot or issue a second job.
    let newest = exit
        .service
        .mirror_runtime_agent_objective("cache-only", Some("replacement newest"));
    let duplicated = exit.service.poll_runtime_objective_mirror(true).is_some();
    drop(release);
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(exit.service)
        .build()
        .unwrap();
    let stop = AtomicBool::new(false);
    let stopped = tokio::sync::Notify::new();
    let persistence = async {
        let report = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 1_000,
                drain_limit: 16,
                idle_interval: Duration::from_millis(5),
            },
            |_, _| stop.load(Ordering::Acquire),
        )
        .await;
        stopped.notify_one();
        report
    };
    let client = async {
        let converged = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let status = handle.metrics().await.unwrap().objective_mirror;
                if status.confirmed > 0 && status.pending_keys == 0 && status.slot_phase == "idle" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        stop.store(true, Ordering::Release);
        stopped.notified().await;
        let shutdown = handle.shutdown().await;
        (converged, shutdown)
    };
    let ((converged, final_shutdown), replacement, mut exit) =
        tokio::join!(client, persistence, actor.run());
    let idle = settle::settle_objective_mirrors(&mut exit.service);
    let mirror = if idle {
        store
            .session_objective_mirror("cache-only")
            .unwrap()
            .map(|record| record.objective)
    } else {
        None
    };
    let essential = std::fs::read(&path).unwrap();
    if idle {
        std::fs::remove_dir_all(&root).unwrap();
    }
    assert!(
        entered_held && essential_held,
        "essential persistence waited for optional write release"
    );
    assert_eq!(admitted.unwrap(), 1);
    written.unwrap();
    shutdown.unwrap();
    final_shutdown.unwrap();
    converged.unwrap();
    let persisted = persisted.unwrap();
    // Actor construction queues one normal saved-session retention pass. It
    // and the single CreateNew effect must both settle while the mirror is held.
    assert_eq!(persisted.drained, 2);
    assert_eq!(persisted.completed, 2);
    assert_eq!(persisted.submitted_events, 2);
    assert_eq!(persisted.applied_events, 2);
    assert_eq!(persisted.bytes_written, b"essential once".len());
    assert_eq!(persisted.failed, 0);
    assert_eq!(status.slot_phase, "in_flight");
    assert!(newest && !duplicated);
    assert_eq!(replacement.unwrap().failed, 0);
    assert!(
        idle,
        "retained unresolved optional fixture {}",
        root.display()
    );
    assert_eq!(mirror.as_deref(), Some("replacement newest"));
    assert_eq!(essential, b"essential once");
    assert!(exit.service.tracked_runtime_pane_process_ids().is_empty());
}
#[cfg(test)]
mod settle {
    /// Polls the production mirror poller until it reports idle.
    ///
    /// A single poll assumes one cycle is enough, but a transiently busy store
    /// re-dirties the entry and schedules a later eligible instant, so the
    /// mirror legitimately needs another cycle. The bound keeps a genuine stall
    /// a failure instead of a wait.
    pub(super) fn settle_objective_mirrors(
        service: &mut crate::runtime::RuntimeSessionService,
    ) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            service.poll_runtime_objective_mirror(false);
            if service.objective_mirrors_are_idle_for_tests() {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
