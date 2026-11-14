//! Distinct router HTTP -> real actor selection -> genuine managed child HTTP.
//! Membership activation is fixture-only; no selection or lineage is injected.

use super::*;
use crate::integrations::http::ReqwestProviderHttpTransport;
use futures_util::FutureExt;
use mez_agent::token_budget::TokenLimit;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Equality reached by the router must prevent the selected child's physical
/// send. The control proves that the same real handoff can reach a different
/// provider/model, charged to the original immutable root, when allowance fits.
#[tokio::test]
async fn budget_router_actor_handoff_rechecks_original_root_before_selected_http() {
    for (input, output, allowed) in [(11, None, false), (100, Some(3), false), (100, None, true)] {
        Box::pin(qualify(
            BudgetLimits {
                input: TokenLimit::limited(input).unwrap(),
                output: output
                    .map(|limit| TokenLimit::limited(limit).unwrap())
                    .unwrap_or(TokenLimit::Unlimited),
            },
            allowed,
        ))
        .await;
    }
}

async fn qualify(limits: BudgetLimits, allowed: bool) {
    let Prepared {
        mut service,
        dispatch: original,
        store,
        member,
        mut root,
    } = prepared_case(limits).await;
    // Replace the component fixture's provisional claim with normal actor
    // admission; the claim generation below comes from the actual actor.
    service.clear_claimed_agent_provider_task(&original.turn.turn_id);
    service.queue_agent_provider_task(original.turn.turn_id.clone());
    let router = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let execution = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let router_endpoint = format!("http://{}/v1", router.local_addr().unwrap());
    let execution_endpoint = format!("http://{}/v1", execution.local_addr().unwrap());
    use crate::config::{ConfigFormat, ConfigLayer, ConfigScope};
    service.replace_config_layers(vec![ConfigLayer {
        name: "budget-routing-handoff".into(), path: None,
        format: ConfigFormat::Toml, scope: ConfigScope::Primary, trusted: true,
        text: format!("[agents]\nrouting=false\nshell_mode=\"native\"\ndefault_provider=\"local-chat\"\ndefault_model_profile=\"default\"\n[permissions]\nsandbox=\"policy-only\"\n[providers.local-chat]\nkind=\"openai-compatible\"\nbase_url=\"http://127.0.0.1:1/v1\"\nmodels=[\"test-model\"]\ndefault_model=\"test-model\"\n[providers.selected-http]\nkind=\"openai-compatible\"\nbase_url=\"{execution_endpoint}\"\nmodels=[\"selected-model\"]\ndefault_model=\"selected-model\"\n[model_profiles.default]\nprovider=\"local-chat\"\nmodel=\"test-model\"\n[model_profiles.selected]\nprovider=\"selected-http\"\nmodel=\"selected-model\"\n"),
    }]).unwrap();
    let parent_turn = original.turn.turn_id.clone();
    let parent_agent = AgentId::opaque(original.turn.agent_id.clone()).unwrap();
    drop(original);
    let (router_stop, router_server) = serve(router, store.clone(), member.clone(), true);
    let (execution_stop, execution_server) = serve(execution, store.clone(), member.clone(), false);
    let (handle, actor) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
            .unwrap();
    // Cancellation destroys the inline producer first, but cannot destroy the
    // only owner of its independent native authorization/receipt workers.
    let mut journals = Vec::new();
    let control = async {
        let client = async {
            let mut dispatch = handle
                .claim_configured_agent_provider_task(parent_agent.clone(), parent_turn.clone())
                .await
                .unwrap()
                .unwrap();
            let generation = dispatch.claim_generation;
            let mut selected = dispatch.model_profile.clone();
            selected.provider = "selected-http".into();
            selected.model = "selected-model".into();
            let mut router_profile = dispatch.model_profile.clone();
            router_profile.provider = "router-http".into();
            router_profile.model = "router-model".into();
            let target = |size: &str| mez_agent::AutoSizingTargetProfile {
                size: size.into(),
                profile_name: "selected".into(),
                profile: selected.clone(),
                supported_reasoning_efforts: vec!["high".into()],
            };
            dispatch.auto_sizing = Some(mez_agent::AutoSizingDispatch {
                router_profile_name: "fixture-router".into(),
                router_profile,
                default_profile_name: "selected".into(),
                default_profile: selected.clone(),
                small: target("small"),
                medium: target("medium"),
                large: target("large"),
                turn_metadata: None,
                allowed_reasoning_efforts: vec!["high".into()],
                fallback_policy: mez_agent::AutoSizingFallbackPolicy::UseDefaultProfile,
            });
            dispatch.auto_sizing_provider = Some(
                crate::runtime::RuntimeAgentProviderDispatchProvider::OpenAiCompatible(
                    OpenAiCompatibleChatCompletionsProvider::without_auth(
                        ReqwestProviderHttpTransport,
                    )
                    .unwrap()
                    .with_provider_id("router-http")
                    .unwrap()
                    .with_streaming_options_for_test(false)
                    .unwrap()
                    .with_endpoint(router_endpoint),
                ),
            );
            let outcome = execute(&handle, dispatch, &mut journals).await.unwrap();
            assert!(matches!(
                outcome,
                RuntimeAgentProviderWorkerOutcome::RoutingSelected(_)
            ));
            assert_eq!(
                store.budget_account(&member.root).unwrap().known,
                UsageTotals {
                    input: 11,
                    output: 3
                }
            );
            assert_eq!(rows(&store, "budget_receipts"), 2);
            let (event, completed) = provider_worker_event(
                parent_agent.clone(),
                parent_turn.clone(),
                generation,
                Ok(Ok(outcome)),
            );
            assert!(!completed);
            let mut batch = RuntimeEventBatch::new();
            batch.push(event);
            handle.submit_runtime_events(batch).await.unwrap();
            // Actual root routing creates and queues its managed child; claiming
            // it executes normal off-actor membership preparation before admission.
            let tasks = handle.pending_agent_provider_tasks().await.unwrap();
            assert_eq!(
                tasks.len(),
                1,
                "actual routed child must be queued: {tasks:?}"
            );
            let child = &tasks[0];
            assert_ne!(child.turn_id, parent_turn);
            let child_agent = AgentId::opaque(child.agent_id.clone()).unwrap();
            let mut successor = handle
                .claim_configured_agent_provider_task(child_agent, child.turn_id.clone())
                .await
                .unwrap()
                .unwrap();
            let child_member = &successor.budget_owner.as_ref().unwrap().member;
            assert_eq!(child_member.root, member.root);
            assert_eq!(child_member.namespace, member.namespace);
            assert_ne!(child_member.conversation, member.conversation);
            assert!(
                successor.auto_sizing.is_none(),
                "selected execution must not route again"
            );
            assert_eq!(successor.model_profile.provider, "selected-http");
            assert_eq!(successor.model_profile.model, "selected-model");
            // Only disable streaming on the real configured selected adapter; no
            // fixture receipt factory or borrowed router authority is installed.
            if let crate::runtime::RuntimeAgentProviderDispatchProvider::OpenAiCompatible(
                provider,
            ) = successor.provider
            {
                successor.provider =
                    crate::runtime::RuntimeAgentProviderDispatchProvider::OpenAiCompatible(
                        provider.with_streaming_options_for_test(false).unwrap(),
                    );
            } else {
                panic!("configured selected provider changed");
            }
            let result = execute(&handle, successor, &mut journals).await;
            (
                result,
                store.budget_account(&member.root).unwrap().known,
                rows(&store, "budget_receipts"),
            )
        };
        let result = tokio::time::timeout(
            Duration::from_secs(30),
            std::panic::AssertUnwindSafe(client).catch_unwind(),
        )
        .await;
        // The timeout/catch above has dropped or completed every producer
        // future before any original native work is drained.
        let cleanup = tokio::time::timeout(Duration::from_secs(10), drain(&journals)).await;
        let _ = router_stop.send(());
        let _ = execution_stop.send(());
        let router = retire(router_server).await;
        let execution = retire(execution_server).await;
        handle.shutdown().await.unwrap();
        (result, cleanup, router, execution)
    };
    let ((result, cleanup, router, execution), mut exit) = tokio::join!(control, actor.run());
    exit.service.terminate_all_pane_processes().unwrap();
    if matches!(cleanup, Ok(Ok(()))) {
        drop(journals);
        drop((exit, handle));
        root.cleanup();
    }
    cleanup
        .expect("original native journal drain timed out; fixture retained")
        .expect("original native journal drain failed; fixture retained");
    let router = router.unwrap();
    let execution = execution.unwrap();
    let (result, known, receipts) = match result.unwrap() {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    assert_eq!(router, 1);
    if !allowed {
        assert_eq!(result.unwrap_err().kind(), MezErrorKind::BudgetSuspended);
        assert_eq!(
            (execution, known, receipts),
            (
                0,
                UsageTotals {
                    input: 11,
                    output: 3
                },
                2
            )
        );
    } else {
        assert!(matches!(
            result.unwrap(),
            RuntimeAgentProviderWorkerOutcome::Execution(_)
        ));
        assert_eq!(
            (execution, known, receipts),
            (
                1,
                UsageTotals {
                    input: 22,
                    output: 6
                },
                4
            )
        );
    }
}

async fn execute(
    handle: &AsyncRuntimeSessionHandle,
    dispatch: RuntimeAgentProviderDispatch,
    journals: &mut Vec<PendingProviderWireObservations>,
) -> Result<RuntimeAgentProviderWorkerOutcome> {
    let pending = retain_journal(journals);
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    let mut call = Box::pin(tokio::task::unconstrained(
        execute_runtime_agent_provider_dispatch(dispatch, None, sender, pending.clone()),
    ));
    let result = loop {
        tokio::select! {
            biased;
            result = &mut call => break result,
            Some(observation) = receiver.recv() => {
                submit_pending_provider_observation(handle, &pending, observation).await?;
            }
        }
    };
    flush_pending_provider_observations(handle, &pending).await?;
    assert!(pending.is_empty());
    result
}

fn retain_journal(
    journals: &mut Vec<PendingProviderWireObservations>,
) -> PendingProviderWireObservations {
    let pending = PendingProviderWireObservations::default();
    journals.push(pending.clone());
    pending
}

/// Producer destruction finalizes its original stack evidence before this
/// drain. A failed/conflicting/planned entry prevents cleanup, never fabricates
/// a successful join or permits deleting a store still owned by native work.
async fn drain(journals: &[PendingProviderWireObservations]) -> Result<()> {
    for pending in journals {
        for observation in pending.snapshot() {
            pending.checkpoint(&observation).await?;
            pending.acknowledge(&observation.request_id);
        }
        if !pending.is_empty() {
            return Err(MezError::invalid_state(
                "original routing journal remains unsettled",
            ));
        }
    }
    Ok(())
}

/// Cancel a real adapter's borrowed admission wait while its ORIGINAL native
/// writer is gated. The same outer drain used above must remain pending until
/// that worker returns, then withdraw its never-sent grant without HTTP replay.
#[tokio::test]
async fn budget_routing_canceled_native_wait_drains_original_journal_before_cleanup() {
    let Prepared {
        mut service,
        dispatch,
        store,
        member,
        mut root,
    } = prepared_case(BudgetLimits::default()).await;
    let work = dispatch
        .budget_owner
        .as_ref()
        .unwrap()
        .prepare_attempt(
            mez_agent::ModelTokenUsageKey::new("local-chat", "test-model"),
            BudgetAttemptPurpose::Execution,
        )
        .unwrap();
    let owner = work.admission_owner();
    let original = work.clone();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, gate) = std::sync::mpsc::channel();
    let waiter = tokio::spawn(async move {
        owner
            .claim_with_for_test(move || {
                let _ = started.send(());
                gate.recv()
                    .map_err(|_| MezError::invalid_state("native gate closed"))?;
                original.authorize()
            })
            .await
    });
    ready.await.unwrap();
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = OpenAiCompatibleChatCompletionsProvider::without_auth(Transport {
        usage: serde_json::json!({}),
        calls: calls.clone(),
    })
    .unwrap()
    .with_provider_id("local-chat")
    .unwrap();
    let request = crate::integrations::agent::context::assemble_model_request(
        &dispatch.model_profile,
        provider.api_compatibility(),
        &dispatch.turn,
        dispatch.context.durable(),
    )
    .unwrap();
    let mut journals = Vec::new();
    let pending = retain_journal(&mut journals);
    let (sender, _receiver) = tokio::sync::mpsc::channel(1);
    let observer = ProviderWireRequestObserver::new(&member.conversation, "%1", sender)
        .with_pending_observations(pending.clone())
        .with_budget_receipts(Arc::new(
            super::super::super::budget_admission::BudgetAttemptAdmission::new(work.clone()),
        ))
        .unwrap();
    {
        let observed = ObservedAsyncModelProvider::new(
            &provider,
            &observer,
            ProviderRequestPurpose::Execution,
        );
        let call = observed.send_request_async(&request);
        tokio::pin!(call);
        assert!(futures_util::poll!(&mut call).is_pending());
    }
    assert_eq!(
        pending.len(),
        1,
        "canceled producer must finalize original evidence"
    );
    {
        let cleanup = drain(&journals);
        tokio::pin!(cleanup);
        assert!(
            futures_util::poll!(&mut cleanup).is_pending(),
            "drain abandoned the gated native writer"
        );
    }
    assert!(root.0.as_ref().unwrap().exists());
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), drain(&journals))
        .await
        .unwrap()
        .unwrap();
    let actual = (
        calls.load(Ordering::SeqCst),
        rows(&store, "budget_receipts"),
        store.budget_account(&member.root).unwrap().known,
    );
    service.terminate_all_pane_processes().unwrap();
    drop((
        observer, pending, journals, provider, work, dispatch, service,
    ));
    root.cleanup();
    assert_eq!(actual, (0, 2, UsageTotals::default()));
}

fn serve(
    listener: tokio::net::TcpListener,
    store: TokenUsageStore,
    member: BudgetMembership,
    router: bool,
) -> (
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<usize>,
) {
    let (stop, mut stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut calls = 0;
        loop {
            let (mut socket, _) = tokio::select! {
                biased;
                _ = &mut stopped => break,
                accepted = listener.accept() => accepted.unwrap(),
            };
            calls += 1;
            assert_eq!(calls, 1, "unexpected physical retry");
            let mut bytes = Vec::new();
            let end = loop {
                if let Some(index) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                    break index + 4;
                }
                let mut chunk = [0; 4096];
                let n = socket.read(&mut chunk).await.unwrap();
                assert!(n > 0 && bytes.len() + n <= 1024 * 1024);
                bytes.extend_from_slice(&chunk[..n]);
            };
            let length: usize = std::str::from_utf8(&bytes[..end])
                .unwrap()
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().unwrap())
                })
                .unwrap();
            assert!(end + length <= 1024 * 1024);
            while bytes.len() < end + length {
                let mut chunk = [0; 4096];
                let n = socket.read(&mut chunk).await.unwrap();
                assert!(n > 0 && bytes.len() + n <= 1024 * 1024);
                bytes.extend_from_slice(&chunk[..n]);
            }
            let request: serde_json::Value =
                serde_json::from_slice(&bytes[end..end + length]).unwrap();
            assert_eq!(
                request["model"],
                if router {
                    "router-model"
                } else {
                    "selected-model"
                }
            );
            assert_eq!(rows(&store, "budget_attempts"), if router { 1 } else { 2 });
            let body: String = rusqlite::Connection::open(store.path())
                .unwrap()
                .query_row(
                    "SELECT body FROM budget_attempts ORDER BY rowid DESC LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            let attempt: crate::storage::token_usage::budget::receipts::BudgetAttempt =
                serde_json::from_str(&body).unwrap();
            assert_eq!(attempt.namespace, member.namespace);
            assert_eq!(attempt.root, member.root);
            assert_eq!(
                attempt.model,
                mez_agent::ModelTokenUsageKey::new(
                    if router {
                        "router-http"
                    } else {
                        "selected-http"
                    },
                    if router {
                        "router-model"
                    } else {
                        "selected-model"
                    },
                )
            );
            if router {
                assert_eq!(attempt.purpose, BudgetAttemptPurpose::Routing);
                assert_eq!(attempt.conversation, member.conversation);
            } else {
                assert_eq!(attempt.purpose, BudgetAttemptPurpose::Execution);
                assert_ne!(attempt.conversation, member.conversation);
            }
            assert_eq!(
                store.budget_account(&member.root).unwrap().known,
                if router {
                    UsageTotals::default()
                } else {
                    UsageTotals {
                        input: 11,
                        output: 3,
                    }
                }
            );
            let text = if router {
                serde_json::json!({"version":1,"size":"small","reasoning_effort":"high","confidence":0.9,"rationale":"fixture"}).to_string()
            } else {
                serde_json::json!({"rationale":"fixture","actions":[{"type":"say","status":"final","content_type":"text/plain; charset=utf-8","text":"done"}]}).to_string()
            };
            let body = serde_json::json!({"model":"reported-model","choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":text}}],"usage":{"prompt_tokens":11,"completion_tokens":3}}).to_string();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
        calls
    });
    (stop, server)
}

async fn retire(mut server: tokio::task::JoinHandle<usize>) -> std::result::Result<usize, String> {
    match tokio::time::timeout(Duration::from_secs(5), &mut server).await {
        Ok(result) => result.map_err(|_| "loopback server failed".into()),
        Err(_) => {
            server.abort();
            let _ = server.await;
            Err("loopback server timed out".into())
        }
    }
}
