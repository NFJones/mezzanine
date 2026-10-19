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
        let shutdown = handle.shutdown().await;
        (
            adoption,
            adopted_before_release,
            read,
            read_before_release,
            cancel,
            cancel_before_release,
            shutdown,
        )
    };
    let ((adoption, adopted, read, responsive, cancel, cancelled, shutdown), mut exit) =
        tokio::join!(client, actor.run());
    // Release and join before any assertions or filesystem cleanup, including
    // the baseline failure. Drop is also the unwind fallback for early errors.
    drop(watchdog);
    std::fs::remove_dir_all(&root).unwrap();
    assert_eq!(adoption.unwrap().applied, 1);
    read.unwrap();
    cancel.unwrap();
    shutdown.unwrap();
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
