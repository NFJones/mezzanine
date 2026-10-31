//! Genuine auxiliary task admission, request-local panics and actor settlement.
//!
//! Generated commands/history enter the real schedulers and actor claim APIs.
//! Only the request executor is replaced, so captured completion identity and
//! monitor/actor delivery remain production code. These functional unwind
//! controls qualify bounded default-stack runtime profiles in owned processes;
//! they do not establish a universal stack bound or arbitrary workload safety.

use super::*;
use crate::host::async_runtime::{AsyncRuntimeSessionActor, ClientId, RuntimeTimerKind};
use crate::runtime::RuntimeSessionService;

/// Auxiliary scheduler whose actual task is admitted by one fixture.
#[derive(Clone, Copy, Debug)]
enum Family {
    Compaction,
    Remember,
    Title,
}

/// Immutable identity copied from a genuinely claimed dispatch before faulting.
#[derive(Debug)]
enum Owner {
    Compaction(u64),
    Remember(String),
    Title { attempt: String, dispatch: String },
}

/// Keeps generated config/auth/transcript storage alive through actor teardown.
struct FixtureRoot(std::path::PathBuf);
impl Drop for FixtureRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Retains the exact abort-capable harness through assertion and OS errors.
/// Reaping this child alone does not prove native descendant fate on failure.
struct ProfileChild(std::process::Child);
impl Drop for ProfileChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

/// Construction and polling panics must retire actual auxiliary claims on
/// production-flavor Tokio runtimes with default stacks and 1/2/4 workers.
/// Separately owned processes contain a hard abort; successful exit and a fresh
/// exact-profile receipt are both required, so selecting zero tests cannot pass.
#[test]
fn auxiliary_actor_panics_retire_exact_claims_and_keep_ingress_live() {
    const CHILD: &str = "MEZ_AUXILIARY_FAULT_PROFILE_CHILD";
    const RECEIPT: &str = "MEZ_AUXILIARY_FAULT_PROFILE_RECEIPT";
    if let Some(workers) = std::env::var_os(CHILD) {
        let workers = workers.into_string().unwrap().parse::<usize>().unwrap();
        assert!([1, 2, 4].contains(&workers));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(workers)
            .enable_all()
            .build()
            .unwrap();
        let (cases, roots) = runtime.block_on(Box::pin(async {
            let mut cases = 0;
            let mut roots = 0;
            for family in [Family::Compaction, Family::Remember, Family::Title] {
                for construction in [false, true] {
                    roots += Box::pin(qualify_panic(family, construction)).await;
                    cases += 1;
                }
            }
            (cases, roots)
        }));
        assert_eq!(cases, 6);
        assert_eq!(roots, 2);
        drop(runtime);
        let receipt = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(std::env::var_os(RECEIPT).unwrap())
            .unwrap();
        serde_json::to_writer(
            receipt,
            &serde_json::json!({
                "schema": 1, "workers": workers, "cases": cases,
                "native_roots_reaped": roots, "completed": true,
                "debug_assertions": cfg!(debug_assertions),
            }),
        )
        .unwrap();
        eprintln!(
            "AUXILIARY_FAULT_PROFILE workers={workers} cases={cases} roots={roots} stage=complete debug_assertions={}",
            cfg!(debug_assertions)
        );
        return;
    }
    let receipts = std::env::temp_dir().join(format!(
        "mez-auxiliary-fault-profiles-{:032x}",
        rand::random::<u128>()
    ));
    std::fs::create_dir(&receipts).unwrap();
    for workers in [1, 2, 4] {
        let receipt = receipts.join(format!("{workers}.json"));
        let mut child = ProfileChild(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    concat!(
                        module_path!(),
                        "::auxiliary_actor_panics_retire_exact_claims_and_keep_ingress_live"
                    )
                    .strip_prefix("mezzanine::")
                    .unwrap(),
                    "--nocapture",
                ])
                .env(CHILD, workers.to_string())
                .env(RECEIPT, &receipt)
                .env_remove("RUST_MIN_STACK")
                .env_remove("RUST_TEST_THREADS")
                .env_remove("TOKIO_WORKER_THREADS")
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(45);
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.0.kill();
                let status = child.0.wait().unwrap();
                panic!(
                    "owned auxiliary profile timed out: workers={workers} exit={status}; native fate unproven"
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(
            status.success(),
            "auxiliary profile workers={workers} exit={status}; no native cleanup inferred on failure"
        );
        let actual: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&receipt).unwrap()).unwrap();
        assert_eq!(
            actual,
            serde_json::json!({
                "schema": 1, "workers": workers, "cases": 6,
                "native_roots_reaped": 2, "completed": true,
                "debug_assertions": cfg!(debug_assertions),
            })
        );
        eprintln!("AUXILIARY_FAULT_PROFILE workers={workers} exit={status} receipt=verified");
    }
    std::fs::remove_dir_all(receipts).unwrap();
}

/// Creates a request future whose construction or first resumed poll unwinds.
/// No task identity, actor access or successful result is supplied by this seam.
fn request_fault<Output: Send + 'static>(
    construction: bool,
) -> impl Future<Output = Output> + Send {
    assert!(!construction, "private auxiliary construction payload");
    async {
        tokio::task::yield_now().await;
        panic!("private auxiliary polling payload")
    }
}

/// Admits one actual family task with a real configured, credential-free local
/// provider constructor. The loopback endpoint is never contacted by fault work.
fn queued_fixture(family: Family) -> (FixtureRoot, RuntimeSessionService, ClientId, String) {
    use crate::config::{ConfigFormat, ConfigLayer, ConfigScope};
    let root = FixtureRoot(std::env::temp_dir().join(format!(
        "mez-auxiliary-actor-fault-{:032x}",
        rand::random::<u128>()
    )));
    std::fs::create_dir(&root.0).unwrap();
    let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    service.set_config_root(root.0.clone());
    service.set_auth_store(crate::security::auth::AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&root.0),
    ));
    service.set_agent_transcript_store(crate::storage::transcript::AgentTranscriptStore::new(
        root.0.join("transcripts"),
    ));
    let title_policy = if matches!(family, Family::Title) {
        "generated"
    } else {
        "first_prompt"
    };
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "auxiliary-actor-fault".into(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                r#"[agents]
routing = false
shell_mode = "pane"
default_provider = "local-chat"
default_model_profile = "default"
session_title_policy = "{title_policy}"
[permissions]
sandbox = "policy-only"
[providers.local-chat]
kind = "openai-compatible"
base_url = "http://127.0.0.1:9/v1"
models = ["test"]
default_model = "test"
[model_profiles.default]
provider = "local-chat"
model = "test"
context_window_tokens = 40000
"#
            ),
        }])
        .unwrap();
    let primary = service
        .attach_primary(
            "auxiliary-fault",
            true,
            mez_mux::layout::Size::new(80, 24).unwrap(),
            120,
        )
        .unwrap();
    if matches!(family, Family::Title) {
        service
            .start_initial_pane_process(Some("cat >/dev/null"))
            .unwrap();
    }
    let conversation = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    match family {
        Family::Remember => {
            service
                .execute_agent_shell_command(
                    &primary,
                    "/remember Keep the generated fixture checklist current.",
                )
                .unwrap();
            assert_eq!(service.pending_agent_remember_tasks(), vec!["%1"]);
        }
        Family::Title => {
            service
                .schedule_runtime_agent_session_title(
                    &conversation,
                    Some("Maintain the generated fixture checklist."),
                )
                .unwrap();
            assert_eq!(
                service.pending_agent_session_title_tasks(),
                vec![conversation.clone()]
            );
        }
        Family::Compaction => {
            service
                .execute_agent_shell_command(&primary, "continue after compaction")
                .unwrap();
            let context = service.agent_turn_contexts_mut().get_mut("turn-1").unwrap();
            for label in ["older", "newer"] {
                let group = mez_agent::ContextExecutionGroupId::new(format!(
                    "auxiliary-fault:{}",
                    context.event_sequence_high_water_mark().saturating_add(1)
                ))
                .unwrap();
                context
                    .append_assistant_event("generated evidence", "inspect evidence", group.clone())
                    .unwrap();
                context
                    .append_evidence_event(
                        ContextSourceKind::ActionResult,
                        label,
                        "evidence ".repeat(3000),
                        group,
                        None,
                        true,
                    )
                    .unwrap();
            }
            let error = MezError::invalid_state("provider context length exceeded")
                .with_provider_failure_json(
                    r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
                );
            service
                .schedule_agent_provider_retry_transition(
                    &AgentId::opaque("agent-%1").unwrap(),
                    "turn-1",
                    ProviderErrorRetryClass::ContextLimit,
                    &error,
                )
                .unwrap()
                .unwrap();
            assert!(
                service
                    .pending_agent_compaction_task_generation("%1")
                    .is_some()
            );
        }
    }
    (root, service, primary, conversation)
}

/// Faults actual admitted request ownership, submits its real monitor result,
/// and inspects final service state after a normal actor shutdown. The actor
/// continues to answer an ordinary command after the request-local unwind.
async fn qualify_panic(family: Family, construction: bool) -> usize {
    let (_root, service, primary, conversation) = queued_fixture(family);
    let compaction_generation = service.pending_agent_compaction_task_generation("%1");
    let observation = service
        .pending_agent_remember_observation_id("%1")
        .map(str::to_owned);
    let title_dispatch = service
        .pending_agent_session_title_dispatch_id(&conversation)
        .map(str::to_owned);
    let (handle, actor) = AsyncRuntimeSessionActor::new(service, Default::default()).unwrap();
    let actor = tokio::spawn(actor.run());
    let client = async move {
        let drains: ProviderObservationDrains = Default::default();
        let (owner, result) = match family {
            Family::Compaction => {
                let dispatch = handle
                    .claim_agent_compaction_task("%1".into(), compaction_generation.unwrap())
                    .await
                    .unwrap()
                    .unwrap();
                let owner = Owner::Compaction(dispatch.task.task_generation);
                let result = monitor_runtime_agent_compaction_dispatch(
                    handle.clone(),
                    "%1".into(),
                    dispatch,
                    drains.clone(),
                    move |_, _| request_fault::<Result<ModelResponse>>(construction),
                )
                .await
                .unwrap();
                (owner, result)
            }
            Family::Remember => {
                let dispatch = handle
                    .claim_agent_remember_task("%1".into(), observation.unwrap())
                    .await
                    .unwrap()
                    .unwrap();
                let owner = Owner::Remember(dispatch.task.observation_id.clone());
                let result = monitor_runtime_agent_remember_dispatch(
                    handle.clone(),
                    "%1".into(),
                    dispatch,
                    drains.clone(),
                    move |_, _| request_fault::<Result<ModelResponse>>(construction),
                )
                .await
                .unwrap();
                (owner, result)
            }
            Family::Title => {
                let dispatch = handle
                    .claim_agent_session_title_task(conversation.clone(), title_dispatch.unwrap())
                    .await
                    .unwrap()
                    .unwrap();
                let owner = Owner::Title {
                    attempt: dispatch.task.attempt_id.clone(),
                    dispatch: dispatch.task.dispatch_id.clone(),
                };
                let result = monitor_runtime_agent_session_title_dispatch(
                    handle.clone(),
                    dispatch,
                    drains.clone(),
                    move |_, _| {
                        request_fault::<(AgentSessionTitleOutcome, mez_agent::ModelTokenUsage)>(
                            construction,
                        )
                    },
                )
                .await
                .unwrap();
                (owner, result)
            }
        };
        assert!(drains.lock().unwrap().is_empty());
        let original_failure = settle_failure(&handle, &owner, &conversation, result).await;
        if let Owner::Title {
            attempt,
            dispatch: original_dispatch,
        } = &owner
        {
            let effects = handle
                .drain_agent_provider_dispatch_side_effects(64)
                .await
                .unwrap();
            let mut retries = effects.into_iter().filter_map(|effect| match effect {
                RuntimeSideEffect::DispatchAgentSessionTitle {
                    conversation_id,
                    dispatch_id,
                } if conversation_id == conversation && dispatch_id != *original_dispatch => {
                    Some(dispatch_id)
                }
                _ => None,
            });
            let retry_id = retries
                .next()
                .expect("bound failure retains its planned retry");
            assert!(retries.next().is_none());
            let retry = handle
                .claim_agent_session_title_task(conversation.clone(), retry_id)
                .await
                .unwrap()
                .unwrap();
            assert_ne!(&retry.task.attempt_id, attempt);
            let retry_owner = Owner::Title {
                attempt: retry.task.attempt_id.clone(),
                dispatch: retry.task.dispatch_id.clone(),
            };
            // The old event cannot consume an actually claimed replacement.
            assert_replay_ignored(&handle, original_failure).await;
            let result = monitor_runtime_agent_session_title_dispatch(
                handle.clone(),
                retry,
                drains.clone(),
                move |_, _| {
                    request_fault::<(AgentSessionTitleOutcome, mez_agent::ModelTokenUsage)>(
                        construction,
                    )
                },
            )
            .await
            .unwrap();
            settle_failure(&handle, &retry_owner, &conversation, result).await;
        }
        if let Owner::Compaction(generation) = owner {
            let key = handle
                .drain_timer_side_effects(1024)
                .await
                .unwrap()
                .into_iter()
                .find_map(|effect| match effect {
                    RuntimeSideEffect::ScheduleTimer { key, .. }
                        if key.kind == RuntimeTimerKind::CompactionClaim
                            && key.generation == generation =>
                    {
                        Some(key)
                    }
                    _ => None,
                })
                .unwrap();
            let mut late = RuntimeEventBatch::new();
            late.push(RuntimeEvent::Timer(crate::runtime::TimerEvent {
                key,
                now_ms: 0,
            }));
            assert_eq!(handle.submit_runtime_events(late).await.unwrap().applied, 0);
        }
        let help = handle
            .execute_agent_shell_command(primary, "/help".into())
            .await
            .unwrap();
        assert!(!help.is_empty());
        handle.shutdown().await.unwrap();
        owner
    };
    let owner = tokio::spawn(client).await.unwrap();
    let mut exit = actor.await.unwrap();
    match owner {
        Owner::Compaction(generation) => {
            assert!(
                !exit
                    .service
                    .agent_compaction_task_is_claimed("%1", generation)
            );
            assert!(!exit.service.agent_is_compacting("%1"));
            assert_eq!(
                exit.service
                    .agent_turn_ledger()
                    .turn("turn-1")
                    .unwrap()
                    .state,
                AgentTurnState::Failed
            );
        }
        Owner::Remember(observation) => {
            assert!(
                !exit
                    .service
                    .agent_remember_observation_matches("%1", &observation)
            );
            assert!(!exit.service.agent_is_remembering("%1"));
            assert!(exit.service.agent_turn_ledger().turns().is_empty());
        }
        Owner::Title { .. } => {
            assert!(
                exit.service
                    .claimed_agent_session_title_task_ids()
                    .is_empty()
            );
            assert!(exit.service.pending_agent_session_title_tasks().is_empty());
            assert!(exit.service.agent_turn_ledger().turns().is_empty());
        }
    }
    let roots = exit.service.terminate_all_pane_processes().unwrap().len();
    assert_eq!(roots, usize::from(matches!(family, Family::Title)));
    roots
}

/// Applies the actual monitor result through provider-service delivery and
/// verifies that a duplicate cannot consume the settled accounting owner twice.
async fn settle_failure(
    handle: &AsyncRuntimeSessionHandle,
    owner: &Owner,
    conversation: &str,
    result: AsyncAgentProviderWorkerResult,
) -> RuntimeEvent {
    let (event, completed) = result.as_ref().unwrap();
    assert!(!completed);
    assert_exact_failure(owner, conversation, event);
    let duplicate = event.clone();
    let mut report = AsyncAgentProviderPollReport {
        polls: 0,
        executions: 0,
        idle_polls: 0,
        terminal_state: RuntimeLifecycleState::Running,
    };
    record_joined_agent_provider_worker(Ok(Ok(result)), handle, &mut report)
        .await
        .unwrap();
    assert_replay_ignored(handle, duplicate.clone()).await;
    duplicate
}

/// Replays the original internal worker event through real actor ingress.
async fn assert_replay_ignored(handle: &AsyncRuntimeSessionHandle, event: RuntimeEvent) {
    let mut replay = RuntimeEventBatch::new();
    replay.push(event);
    assert_eq!(
        handle.submit_runtime_events(replay).await.unwrap().applied,
        0
    );
}

/// Verifies that the actual family monitor retained the claim's immutable owner
/// and published only the bounded failure vocabulary, not the injected payload.
fn assert_exact_failure(owner: &Owner, conversation: &str, event: &RuntimeEvent) {
    match (owner, event) {
        (
            Owner::Compaction(generation),
            RuntimeEvent::AgentCompaction(AgentCompactionEvent::Failed {
                pane_id,
                task_generation,
                kind,
                message,
                provider_raw_text,
                provider_failure_json,
                ..
            }),
        ) => {
            assert_eq!(pane_id, "%1");
            assert_eq!(task_generation, generation);
            assert_eq!(kind, "invalid_state");
            assert_eq!(message, "provider worker join failed: panic");
            assert!(provider_raw_text.is_none() && provider_failure_json.is_none());
        }
        (
            Owner::Remember(observation),
            RuntimeEvent::AgentRemember(AgentRememberEvent::Failed {
                pane_id,
                observation_id,
                kind,
                message,
                provider_raw_text,
                provider_failure_json,
                ..
            }),
        ) => {
            assert_eq!(pane_id, "%1");
            assert_eq!(observation_id, observation);
            assert_eq!(kind, "invalid_state");
            assert_eq!(message, "provider worker join failed: panic");
            assert!(provider_raw_text.is_none() && provider_failure_json.is_none());
        }
        (
            Owner::Title { attempt, .. },
            RuntimeEvent::AgentSessionTitle(AgentSessionTitleEvent::WorkerSettled {
                conversation_id,
                attempt_id,
                usage,
                outcome,
            }),
        ) => {
            assert_eq!(conversation_id, conversation);
            assert_eq!(attempt_id, attempt);
            assert_eq!(usage, &mez_agent::ModelTokenUsage::default());
            assert_eq!(
                outcome,
                &AgentSessionTitleOutcome::Rejected("worker_panic".into())
            );
        }
        _ => panic!("wrong auxiliary failure family or ownership"),
    }
}

/// A completed, reported request whose response content is never published.
/// Its counters are synthetic; the observed product adapter owns wire identity.
struct ReportedMemoryProvider(std::sync::Arc<std::sync::atomic::AtomicUsize>);
impl AsyncModelProvider for ReportedMemoryProvider {
    fn provider_id(&self) -> &str {
        "local-chat"
    }
    fn api_compatibility(&self) -> mez_agent::ProviderApiCompatibility {
        mez_agent::ProviderApiCompatibility::OpenAiResponses
    }
    fn send_request_async<'a>(
        &'a self,
        _request: &'a ModelRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ModelResponse>> + Send + 'a>> {
        Box::pin(async move {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(ModelResponse {
                provider: "local-chat".into(),
                model: "test".into(),
                raw_text: "unpublished generated response".into(),
                usage: mez_agent::ModelTokenUsage {
                    input_tokens: 17,
                    output_tokens: 3,
                    ..Default::default()
                },
                latest_request_usage: None,
                quota_usage: Vec::new(),
                action_batch: None,
                provider_transcript_events: Vec::new(),
            })
        })
    }
}

/// Service cancellation must observe request destruction, release the genuine
/// memory claim and conserve already reported expense even if the terminal
/// response never reaches its actor. A surviving actor must remain usable.
#[tokio::test]
async fn auxiliary_service_cancellation_settles_claim_and_reported_expense() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    /// Acknowledges actual inner request destruction after abort, not its signal.
    struct Destroyed(Option<tokio::sync::oneshot::Sender<()>>);
    impl Drop for Destroyed {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }
    let (_root, service, primary, conversation) = queued_fixture(Family::Remember);
    let observation = service
        .pending_agent_remember_observation_id("%1")
        .unwrap()
        .to_string();
    let (handle, actor) = AsyncRuntimeSessionActor::new(service, Default::default()).unwrap();
    let actor = tokio::spawn(actor.run());
    let dispatch = handle
        .claim_agent_remember_task("%1".into(), observation.clone())
        .await
        .unwrap()
        .unwrap();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (destroyed, destruction) = tokio::sync::oneshot::channel();
    let requests = Arc::new(AtomicUsize::new(0));
    let worker_requests = requests.clone();
    let mut workers = JoinSet::new();
    let mut owners = AgentProviderWorkerOwners::default();
    let drains: ProviderObservationDrains = Default::default();
    let owner = ProviderWorkerOwner::remember(&dispatch);
    spawn_owned_agent_provider_worker(
        &mut workers,
        &mut owners,
        Some(owner),
        monitor_runtime_agent_remember_dispatch(
            handle.clone(),
            "%1".into(),
            dispatch,
            drains.clone(),
            move |dispatch, observer| async move {
                let _destroyed = Destroyed(Some(destroyed));
                let provider = ReportedMemoryProvider(worker_requests);
                let _unpublished = observed_provider_request(
                    &provider,
                    &observer,
                    ProviderRequestPurpose::Memory,
                    &dispatch.task.request,
                )
                .await
                .unwrap();
                let _ = started.send(());
                std::future::pending::<Result<ModelResponse>>().await
            },
        ),
    );
    ready.await.unwrap();
    abort_agent_provider_workers(&mut workers, &handle, &drains, &mut owners)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), destruction)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert!(owners.is_empty());
    assert!(drains.lock().unwrap().is_empty());
    handle
        .execute_agent_shell_command(primary, "/help".into())
        .await
        .unwrap();
    handle.shutdown().await.unwrap();
    let mut exit = actor.await.unwrap();
    assert!(
        !exit
            .service
            .agent_remember_observation_matches("%1", &observation),
        "destroyed auxiliary request must not strand its genuine claim"
    );
    assert!(!exit.service.agent_is_remembering("%1"));
    let usage = exit
        .service
        .agent_token_usage_for_conversation(&conversation);
    let key = mez_agent::ModelTokenUsageKey::new("local-chat", "test");
    assert_eq!(
        usage
            .get(&key)
            .map(|usage| (usage.input_tokens, usage.output_tokens)),
        Some((17, 3))
    );
    assert!(
        exit.service
            .terminate_all_pane_processes()
            .unwrap()
            .is_empty()
    );
}

/// A task cancelled before its first poll has no producer or reported cost, but
/// its genuine actor claim still requires retirement. The outside-future owner
/// must suffice; executing its request factory would violate this precondition.
#[tokio::test]
async fn auxiliary_cancellation_before_first_poll_releases_genuine_claim() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let (_root, service, primary, conversation) = queued_fixture(Family::Remember);
    let observation = service
        .pending_agent_remember_observation_id("%1")
        .unwrap()
        .to_string();
    let (handle, actor) = AsyncRuntimeSessionActor::new(service, Default::default()).unwrap();
    let actor = tokio::spawn(actor.run());
    let dispatch = handle
        .claim_agent_remember_task("%1".into(), observation.clone())
        .await
        .unwrap()
        .unwrap();
    let owner = ProviderWorkerOwner::remember(&dispatch);
    let evaluated = Arc::new(AtomicUsize::new(0));
    let worker_evaluated = evaluated.clone();
    let mut workers = JoinSet::new();
    let mut owners = AgentProviderWorkerOwners::default();
    let drains: ProviderObservationDrains = Default::default();
    spawn_owned_agent_provider_worker(
        &mut workers,
        &mut owners,
        Some(owner),
        monitor_runtime_agent_remember_dispatch(
            handle.clone(),
            "%1".into(),
            dispatch,
            drains.clone(),
            move |_, _| {
                worker_evaluated.fetch_add(1, Ordering::SeqCst);
                std::future::pending::<Result<ModelResponse>>()
            },
        ),
    );
    assert_eq!(
        abort_agent_provider_workers(&mut workers, &handle, &drains, &mut owners)
            .await
            .unwrap(),
        0
    );
    assert_eq!(evaluated.load(Ordering::SeqCst), 0);
    assert!(owners.is_empty());
    assert!(drains.lock().unwrap().is_empty());
    handle
        .execute_agent_shell_command(primary, "/help".into())
        .await
        .unwrap();
    handle.shutdown().await.unwrap();
    let exit = actor.await.unwrap();
    assert!(
        !exit
            .service
            .agent_remember_observation_matches("%1", &observation)
    );
    assert!(!exit.service.agent_is_remembering("%1"));
    assert!(
        exit.service
            .agent_token_usage_for_conversation(&conversation)
            .is_empty()
    );
}

/// A request can report incurred expense and then unwind before returning its
/// terminal result. Normal monitor panic settlement must retain that expense
/// exactly once just as service cancellation does, without publishing response
/// content or borrowing another task's accounting owner.
#[tokio::test]
async fn auxiliary_request_panic_after_report_conserves_known_expense() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let (_root, service, primary, conversation) = queued_fixture(Family::Remember);
    let observation = service
        .pending_agent_remember_observation_id("%1")
        .unwrap()
        .to_string();
    let (handle, actor) = AsyncRuntimeSessionActor::new(service, Default::default()).unwrap();
    let actor = tokio::spawn(actor.run());
    let dispatch = handle
        .claim_agent_remember_task("%1".into(), observation.clone())
        .await
        .unwrap()
        .unwrap();
    let owner = ProviderWorkerOwner::remember(&dispatch);
    let drains: ProviderObservationDrains = Default::default();
    let mut workers = JoinSet::new();
    let mut owners = AgentProviderWorkerOwners::default();
    let requests = Arc::new(AtomicUsize::new(0));
    let worker_requests = requests.clone();
    spawn_owned_agent_provider_worker(
        &mut workers,
        &mut owners,
        Some(owner),
        monitor_runtime_agent_remember_dispatch(
            handle.clone(),
            "%1".into(),
            dispatch,
            drains.clone(),
            move |dispatch, observer| async move {
                let provider = ReportedMemoryProvider(worker_requests);
                let _unpublished = observed_provider_request(
                    &provider,
                    &observer,
                    ProviderRequestPurpose::Memory,
                    &dispatch.task.request,
                )
                .await
                .unwrap();
                panic!("private after-report request payload");
            },
        ),
    );
    let joined = workers.join_next_with_id().await.unwrap();
    let duplicate = match &joined {
        Ok((_, Ok(Some((event, false))))) => event.clone(),
        _ => panic!("request panic must remain a task-local failure"),
    };
    let mut report = AsyncAgentProviderPollReport {
        polls: 0,
        executions: 0,
        idle_polls: 0,
        terminal_state: RuntimeLifecycleState::Running,
    };
    record_owned_agent_provider_worker(joined, &handle, &mut report, &mut owners, &drains)
        .await
        .unwrap();
    assert_replay_ignored(&handle, duplicate).await;
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert!(owners.is_empty());
    assert!(drains.lock().unwrap().is_empty());
    handle
        .execute_agent_shell_command(primary, "/help".into())
        .await
        .unwrap();
    handle.shutdown().await.unwrap();
    let exit = actor.await.unwrap();
    assert!(
        !exit
            .service
            .agent_remember_observation_matches("%1", &observation)
    );
    assert!(!exit.service.agent_is_remembering("%1"));
    let usage = exit
        .service
        .agent_token_usage_for_conversation(&conversation);
    let key = mez_agent::ModelTokenUsageKey::new("local-chat", "test");
    assert_eq!(
        usage
            .get(&key)
            .map(|usage| (usage.input_tokens, usage.output_tokens)),
        Some((17, 3))
    );
}
