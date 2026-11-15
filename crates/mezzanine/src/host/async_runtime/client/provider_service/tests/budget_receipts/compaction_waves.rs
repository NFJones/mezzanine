//! Recursive model waves use real HTTP, native expense ACKs, actor adoption and
//! the ordered publication worker. Unlimited is an explicit concurrency control,
//! not qualification of multiple held nodes sharing one finite-root lease.

use super::*;
use crate::config::{ConfigFormat, ConfigLayer, ConfigScope};
use crate::host::async_runtime::{
    AsyncRuntimeSessionActor, AsyncRuntimeSideEffectServiceConfig,
    run_async_persistence_side_effect_service,
};
use crate::integrations::http::ReqwestProviderHttpTransport;
use futures_util::FutureExt;
use mez_mux::presentation::ClientViewRole;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Keep provider identity and the bound store unchanged while reducing only
/// the configured packing allowance after genuine historical completion.
fn layer(input: u64) -> ConfigLayer {
    ConfigLayer {
        name: "budget-recursive-compaction".into(),
        path: None,
        format: ConfigFormat::Toml,
        scope: ConfigScope::Primary,
        trusted: true,
        text: format!(
            "[agents]\nrouting=false\nshell_mode=\"native\"\ncompaction_raw_retention_percent=1\ndefault_provider=\"local-chat\"\ndefault_model_profile=\"default\"\n[permissions]\nsandbox=\"policy-only\"\n[providers.local-chat]\nkind=\"openai-compatible\"\nbase_url=\"http://127.0.0.1:1/v1\"\nmodels=[\"test-model\"]\ndefault_model=\"test-model\"\n[model_profiles.default]\nprovider=\"local-chat\"\nmodel=\"test-model\"\ncontext_window_tokens={input}\nmax_input_tokens={input}\n"
        ),
    }
}

#[tokio::test]
async fn budget_recursive_compaction_waves_adopt_one_durable_head() {
    Box::pin(qualify(false)).await;
}

#[tokio::test]
async fn budget_recursive_compaction_stop_between_waves_preserves_original_expense() {
    Box::pin(qualify(true)).await;
}

async fn qualify(stop_between: bool) {
    let Prepared {
        mut service,
        dispatch,
        store,
        mut root,
        ..
    } = prepared_case(BudgetLimits::default()).await;
    let primary = service
        .session()
        .clients()
        .iter()
        .find(|client| service.session().is_attached_primary(&client.id))
        .unwrap()
        .id
        .clone();
    let conversation = dispatch.turn.conversation_id.clone();
    service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    drop(dispatch);
    let transcripts = service.agent_transcript_store().unwrap().clone();
    let setup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        service
            .replace_config_layers(vec![layer(1_000_000)])
            .unwrap();
        crate::test_support::compaction_source::complete_task_and_admit_successor(
            &mut service,
            &primary,
            &"frozen manual source ".repeat(40_000),
            "completed historical evidence",
            "continue after historical task",
            mez_agent::ProviderApiCompatibility::OpenAiChatCompletions,
        );
        service
            .execute_agent_shell_command(&primary, "/stop")
            .unwrap();
        service.replace_config_layers(vec![layer(20_000)]).unwrap();
        service
            .execute_agent_shell_command(&primary, "/compact")
            .unwrap();
        service
            .pending_agent_compaction_task_generation("%1")
            .unwrap()
    }));
    let first = match setup {
        Ok(first) => first,
        Err(panic) => {
            service.terminate_all_pane_processes().unwrap();
            drop((service, transcripts, store));
            // Setup failure is not a custody/acceptance witness. The guard
            // retains this exact private root for diagnosis on unwind.
            std::panic::resume_unwind(panic);
        }
    };
    let before = transcripts.inspect(&conversation).unwrap();
    service.use_transcript_effect_adapter();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let mut server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await?;
            let mut bytes = Vec::new();
            loop {
                let mut buffer = [0; 8192];
                let count = socket.read(&mut buffer).await?;
                if count == 0 {
                    return Err(MezError::invalid_state(
                        "compactor closed an incomplete request",
                    ));
                }
                bytes.extend_from_slice(&buffer[..count]);
                if bytes.len() > 1024 * 1024 {
                    return Err(MezError::invalid_state("fixture request bound"));
                }
                if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]);
                    let length: usize = headers
                        .lines()
                        .filter_map(|line| line.split_once(':'))
                        .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                        .ok_or_else(|| MezError::invalid_state("missing request length"))?
                        .1
                        .trim()
                        .parse()
                        .map_err(|_| MezError::invalid_state("invalid request length"))?;
                    if bytes.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            if observed.fetch_add(1, Ordering::SeqCst) >= 128 {
                return Err(MezError::invalid_state("recursive request bound"));
            }
            let body = serde_json::json!({"model":"test-model","choices":[{
                "index":0,"finish_reason":"stop","message":{"role":"assistant","content":"one final manual head"}
            }],"usage":{"prompt_tokens":11,"completion_tokens":3}}).to_string();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).as_bytes()).await?;
        }
        #[allow(unreachable_code)]
        Ok::<(), MezError>(())
    });
    let (handle, actor) = AsyncRuntimeSessionActor::new(service, Default::default()).unwrap();
    let drains = ProviderObservationDrains::default();
    let mut workers = tokio::task::JoinSet::new();
    let mut owners = Default::default();
    let control = async {
        let exercise = std::panic::AssertUnwindSafe(async {
            let mut generation = first;
            let mut root_id = None;
            let mut waves = 0;
            let mut report = AsyncAgentProviderPollReport {
                polls: 0,
                executions: 0,
                idle_polls: 0,
                terminal_state: *handle.lifecycle_state_watcher().borrow(),
            };
            for _ in 0..64 {
                let mut dispatch = handle
                    .claim_agent_compaction_task("%1".into(), generation)
                    .await?
                    .ok_or_else(|| MezError::invalid_state("original queued wave unavailable"))?;
                let member = dispatch
                    .budget_owner
                    .as_ref()
                    .ok_or_else(|| MezError::invalid_state("missing original budget owner"))?
                    .member
                    .clone();
                if let Some(id) = &root_id {
                    assert_eq!(id, &member.root);
                } else {
                    root_id = Some(member.root.clone());
                }
                dispatch.provider =
                    crate::runtime::RuntimeAgentProviderDispatchProvider::OpenAiCompatible(
                        OpenAiCompatibleChatCompletionsProvider::without_auth(
                            ReqwestProviderHttpTransport,
                        )?
                        .with_provider_id("local-chat")?
                        .with_streaming_options_for_test(false)?
                        .with_endpoint(endpoint.clone()),
                    );
                // Consume only timer descriptors; no fake timer callback or
                // archive ACK is needed to drive these genuine wave receipts.
                handle.drain_timer_side_effects(256).await?;
                let owner = ProviderWorkerOwner::compaction(&dispatch);
                let waiter = handle.clone();
                let admitted = spawn_owned_agent_provider_worker(
                    &mut workers,
                    &mut owners,
                    Some(owner),
                    monitor_runtime_agent_compaction_dispatch(
                        handle.clone(),
                        "%1".into(),
                        dispatch,
                        drains.clone(),
                        move |dispatch, observer| {
                            execute_runtime_agent_compaction_dispatch_with_waiter(
                                dispatch,
                                observer,
                                Some(waiter),
                            )
                        },
                    ),
                );
                assert!(admitted.is_some(), "original monitor admission refused");
                let joined =
                    tokio::time::timeout(Duration::from_secs(30), workers.join_next_with_id())
                        .await
                        .map_err(|_| {
                            MezError::invalid_state("recursive wave observation deadline")
                        })?
                        .ok_or_else(|| MezError::invalid_state("missing original monitor"))?;
                assert!(matches!(
                    &joined,
                    Ok((
                        _,
                        Ok(Some((
                            RuntimeEvent::AgentCompaction(AgentCompactionEvent::Completed { .. }),
                            _
                        )))
                    ))
                ));
                record_owned_agent_provider_worker(
                    joined,
                    &handle,
                    &mut report,
                    &mut owners,
                    &drains,
                )
                .await?;
                waves += 1;
                assert_eq!(
                    store.budget_account(&member.root)?.known,
                    UsageTotals {
                        input: calls.load(Ordering::SeqCst) as u64 * 11,
                        output: calls.load(Ordering::SeqCst) as u64 * 3,
                    }
                );
                assert_eq!(transcripts.inspect(&conversation)?, before);
                assert!(
                    transcripts
                        .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())?
                        .is_none()
                );
                let effects = handle
                    .drain_agent_provider_dispatch_side_effects(256)
                    .await?;
                let next = effects
                    .into_iter()
                    .filter_map(|effect| match effect {
                        RuntimeSideEffect::DispatchAgentCompaction {
                            pane_id,
                            task_generation,
                        } if pane_id == "%1" => Some(task_generation),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                if stop_between {
                    assert_eq!(next.len(), 1, "source must genuinely need another wave");
                    handle
                        .execute_agent_shell_command(primary.clone(), "/stop".into())
                        .await?;
                    assert!(
                        handle
                            .claim_agent_compaction_task("%1".into(), next[0])
                            .await?
                            .is_none()
                    );
                    // Run the real ordered writer after Stop, not merely a
                    // cold read before a publication could have been drained.
                    let persistence = run_async_persistence_side_effect_service(
                        &handle,
                        AsyncRuntimeSideEffectServiceConfig {
                            max_polls: 16,
                            drain_limit: 64,
                            idle_interval: Duration::from_millis(1),
                        },
                        |_, _| true,
                    )
                    .await?;
                    assert_eq!(persistence.failed, 0);
                    assert!(
                        transcripts
                            .load_replay_epoch(&conversation, mez_agent::AgentContext::empty(),)?
                            .is_none()
                    );
                    assert_eq!(accepted_heads(&handle).await?, 0);
                    return Ok::<_, MezError>((waves, member.root, false));
                }
                if let Some(next_generation) = next.first() {
                    assert_eq!(next.len(), 1);
                    assert_ne!(generation, *next_generation);
                    generation = *next_generation;
                    continue;
                }
                for _ in 0..8 {
                    let persistence = run_async_persistence_side_effect_service(
                        &handle,
                        AsyncRuntimeSideEffectServiceConfig {
                            max_polls: 1,
                            drain_limit: 64,
                            idle_interval: Duration::from_millis(1),
                        },
                        |_, _| true,
                    )
                    .await?;
                    assert_eq!(persistence.failed, 0);
                    if transcripts
                        .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())?
                        .is_some()
                        && accepted_heads(&handle).await? == 1
                    {
                        // A second real drain must not adopt another head or
                        // admit an ordinary turn for this idle manual command.
                        let again = run_async_persistence_side_effect_service(
                            &handle,
                            AsyncRuntimeSideEffectServiceConfig {
                                max_polls: 4,
                                drain_limit: 64,
                                idle_interval: Duration::from_millis(1),
                            },
                            |_, _| true,
                        )
                        .await?;
                        assert_eq!(again.failed, 0);
                        assert_eq!(accepted_heads(&handle).await?, 1);
                        assert!(handle.pending_agent_provider_tasks().await?.is_empty());
                        assert!(
                            handle
                                .claim_agent_compaction_task("%1".into(), generation,)
                                .await?
                                .is_none()
                        );
                        return Ok((waves, member.root, true));
                    }
                }
                return Err(MezError::invalid_state(
                    "final wave did not publish its exact head",
                ));
            }
            Err(MezError::invalid_state("recursive wave bound"))
        })
        .catch_unwind()
        .await;
        let mut failures = ProviderCleanupFailures::default();
        let cleanup = tokio::time::timeout(
            Duration::from_secs(15),
            retire_agent_provider_workers(
                &mut workers,
                &handle,
                &drains,
                &mut owners,
                &mut failures,
            ),
        )
        .await;
        // retire_agent_provider_workers returns every accumulated cleanup
        // failure, including errors kept outside a canceled monitor. Do not
        // substitute empty owner maps for that returned retirement verdict.
        drop(failures);
        server.abort();
        let server_join = tokio::time::timeout(Duration::from_secs(5), &mut server).await;
        let stop = tokio::time::timeout(
            Duration::from_secs(5),
            handle.execute_agent_shell_command(primary.clone(), "/stop".into()),
        )
        .await;
        let shutdown = tokio::time::timeout(Duration::from_secs(5), handle.shutdown()).await;
        (exercise, cleanup, server_join, stop, shutdown)
    };
    let joined = tokio::time::timeout(Duration::from_secs(90), async {
        tokio::join!(Box::pin(control), Box::pin(actor.run()))
    })
    .await;
    let ((exercise, cleanup, server_join, stop, shutdown), mut exit) =
        joined.expect("fixture deadline; exact root retained, retirement not proven");
    exit.service.terminate_all_pane_processes().unwrap();
    let complete_cleanup = matches!(cleanup, Ok(Ok(_)))
        && owners.is_empty()
        && drains.lock().unwrap().is_empty()
        && matches!(&server_join, Ok(Err(error)) if error.is_cancelled())
        && matches!(stop, Ok(Ok(_)))
        && matches!(shutdown, Ok(Ok(_)));
    let outstanding: i64 = rusqlite::Connection::open(store.path()).unwrap().query_row(
        "SELECT COUNT(*) FROM (SELECT id FROM budget_attempts WHERE terminal=0 UNION SELECT attempt FROM budget_admissions WHERE retired_receipt IS NULL)", [], |row| row.get(0)).unwrap();
    let restored = transcripts
        .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
        .unwrap();
    let count = calls.load(Ordering::SeqCst);
    let claims = rows(&store, "budget_admissions");
    assert!(
        complete_cleanup,
        "original writer/monitor retirement incomplete"
    );
    assert_eq!(outstanding, 0);
    let (waves, _, published) = exercise
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
        .unwrap();
    assert_eq!(published, !stop_between);
    assert_eq!(claims as usize, count);
    assert!(count > 0);
    if stop_between {
        assert_eq!(waves, 1);
        assert!(restored.is_none());
    } else {
        assert!(
            waves > 1 && count > 4,
            "must qualify recursive physical inference"
        );
        let (_, restored) = restored.unwrap();
        assert_eq!(
            restored.context().chronology()[0].block().content,
            "one final manual head"
        );
    }
    // Success and every acceptance assertion precede deletion. On panic,
    // timeout or uncertain retirement FixtureRoot diagnoses and retains.
    drop((exit, handle, drains, owners, transcripts, store, server));
    root.cleanup();
}

/// This status is emitted only after the original publication receipt has
/// passed the actor's source/adoption checks and retired its compaction task.
async fn accepted_heads(handle: &AsyncRuntimeSessionHandle) -> Result<usize> {
    let view = handle
        .render_client_view(
            ClientViewRole::Primary,
            mez_mux::layout::Size::new(180, 60)?,
            Default::default(),
        )
        .await?
        .ok_or_else(|| MezError::invalid_state("missing adoption view"))?;
    Ok(view
        .lines
        .iter()
        .filter(|line| line.contains("canonical compaction head accepted"))
        .count())
}
