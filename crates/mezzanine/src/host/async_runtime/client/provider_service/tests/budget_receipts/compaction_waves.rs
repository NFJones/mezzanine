//! Recursive model waves use real HTTP, native expense ACKs, actor adoption and
//! the ordered publication worker. Finite siblings retain one original lease
//! across equality, policy release and Stop; unlimited remains a control.

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
    Box::pin(qualify(false, BudgetLimits::default(), None)).await;
}

#[tokio::test]
async fn budget_recursive_compaction_stop_between_waves_preserves_original_expense() {
    Box::pin(qualify(true, BudgetLimits::default(), None)).await;
}

/// A generous finite threshold must serialize real sibling requests without
/// discarding a nonwriting sibling or renewing the original compaction lease.
#[tokio::test]
async fn budget_recursive_compaction_finite_root_adopts_original_head() {
    Box::pin(qualify(
        false,
        BudgetLimits {
            input: mez_agent::token_budget::TokenLimit::Limited(1_000_000),
            output: mez_agent::token_budget::TokenLimit::Limited(1_000_000),
        },
        None,
    ))
    .await;
}

#[tokio::test]
async fn budget_recursive_compaction_equality_release_retains_original_wave() {
    Box::pin(qualify(
        false,
        crossing_limits(),
        Some(HoldControl::Release),
    ))
    .await;
}

#[tokio::test]
async fn budget_recursive_compaction_equality_stop_cannot_revive_wave() {
    Box::pin(qualify(false, crossing_limits(), Some(HoldControl::Stop))).await;
}

/// A real request owns the reader while a positively refused sibling reaches
/// the exclusive-lane await. Stop must retire both without pretending that the
/// still-open remote request supplied known usage or a successful publication.
#[tokio::test]
async fn budget_recursive_compaction_stop_retires_issued_and_waiting_siblings() {
    Box::pin(qualify(
        false,
        crossing_limits(),
        Some(HoldControl::StopIssued),
    ))
    .await;
}

#[derive(Clone, Copy)]
enum HoldControl {
    Release,
    Stop,
    StopIssued,
    StopPartial,
    Active,
    ActiveRelease,
    PrefixRelease,
    PrefixStop,
    StorageRelease,
    StorageExhaust,
}

#[tokio::test]
async fn budget_recursive_compaction_storage_contention_reacquires_original_wave() {
    Box::pin(qualify(
        false,
        BudgetLimits::default(),
        Some(HoldControl::StorageRelease),
    ))
    .await;
}

#[tokio::test]
async fn budget_recursive_compaction_storage_contention_exhausts_without_send() {
    Box::pin(qualify(
        false,
        BudgetLimits::default(),
        Some(HoldControl::StorageExhaust),
    ))
    .await;
}

#[tokio::test]
async fn budget_recursive_compaction_active_context_rejection_resumes_original_turn() {
    Box::pin(qualify(
        false,
        BudgetLimits {
            input: mez_agent::token_budget::TokenLimit::Limited(1_000_000),
            output: mez_agent::token_budget::TokenLimit::Limited(1_000_000),
        },
        Some(HoldControl::Active),
    ))
    .await;
}

/// A captured prefix is not terminal transport/receipt evidence: a held sibling
/// cannot suspend the claim, and Stop must conserve the real prefix once.
#[tokio::test]
async fn budget_recursive_compaction_prefix_cannot_release_issued_lane() {
    Box::pin(qualify(
        false,
        crossing_limits(),
        Some(HoldControl::StopPartial),
    ))
    .await;
}

fn crossing_limits() -> BudgetLimits {
    BudgetLimits {
        input: mez_agent::token_budget::TokenLimit::Limited(11),
        output: mez_agent::token_budget::TokenLimit::Unlimited,
    }
}

#[tokio::test]
async fn budget_recursive_compaction_active_equality_release_resumes_original_turn() {
    Box::pin(qualify(
        false,
        crossing_limits(),
        Some(HoldControl::ActiveRelease),
    ))
    .await;
}

/// A real subtotal transaction has committed while terminal settlement has not.
/// Neither a sibling park nor another send may borrow that incomplete ACK.
#[tokio::test]
async fn budget_recursive_compaction_durable_prefix_keeps_lane_until_terminal_ack() {
    Box::pin(qualify(
        false,
        crossing_limits(),
        Some(HoldControl::PrefixRelease),
    ))
    .await;
}

#[tokio::test]
async fn budget_recursive_compaction_stop_during_prefix_ack_retires_original_lane() {
    Box::pin(qualify(
        false,
        crossing_limits(),
        Some(HoldControl::PrefixStop),
    ))
    .await;
}

async fn qualify(stop_between: bool, limits: BudgetLimits, control: Option<HoldControl>) {
    let storage_case = matches!(
        control,
        Some(HoldControl::StorageRelease | HoldControl::StorageExhaust)
    );
    let storage_exhaust = matches!(control, Some(HoldControl::StorageExhaust));
    let prefix_gate_case = matches!(
        control,
        Some(HoldControl::PrefixRelease | HoldControl::PrefixStop)
    );
    let stop_prefix = matches!(control, Some(HoldControl::PrefixStop));
    let active = matches!(
        control,
        Some(HoldControl::Active | HoldControl::ActiveRelease)
    );
    let partial = matches!(control, Some(HoldControl::StopPartial));
    let stop_issued = matches!(
        control,
        Some(HoldControl::StopIssued | HoldControl::StopPartial)
    );
    let stop_held = match control {
        Some(HoldControl::Release | HoldControl::ActiveRelease | HoldControl::PrefixRelease) => {
            Some(false)
        }
        Some(HoldControl::Stop) => Some(true),
        _ => None,
    };
    let Prepared {
        mut service,
        dispatch,
        store,
        member: original_member,
        mut root,
    } = prepared_case(limits).await;
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
        let successor = crate::test_support::compaction_source::complete_task_and_admit_successor(
            &mut service,
            &primary,
            &"frozen manual source ".repeat(40_000),
            "completed historical evidence",
            "continue after historical task",
            mez_agent::ProviderApiCompatibility::OpenAiChatCompletions,
        );
        let original_turn = if active {
            crate::test_support::compaction_source::complete_task(
                &mut service,
                &successor,
                "completed intermediate task",
                mez_agent::ProviderApiCompatibility::OpenAiChatCompletions,
            );
            service.replace_config_layers(vec![layer(20_000)]).unwrap();
            service
                .execute_agent_shell_command(&primary, "continue original active task")
                .unwrap();
            let turn = service
                .agent_shell_store()
                .get("%1")
                .unwrap()
                .running_turn_id
                .clone()
                .unwrap();
            let error = MezError::invalid_state("provider context length exceeded")
                .with_provider_failure_json(
                    r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
                );
            assert!(
                service
                    .schedule_agent_provider_retry_transition(
                        &AgentId::opaque("agent-%1").unwrap(),
                        &turn,
                        ProviderErrorRetryClass::ContextLimit,
                        &error,
                    )
                    .unwrap()
                    .is_some()
            );
            Some(turn)
        } else {
            service
                .execute_agent_shell_command(&primary, "/stop")
                .unwrap();
            service
                .replace_config_layers(vec![layer(if storage_exhaust {
                    1_000_000
                } else {
                    20_000
                })])
                .unwrap();
            service
                .execute_agent_shell_command(&primary, "/compact")
                .unwrap();
            None
        };
        let generation = service
            .pending_agent_compaction_task_generation("%1")
            .unwrap();
        (generation, original_turn)
    }));
    let (first, original_turn) = match setup {
        Ok(prepared) => prepared,
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
    let checked_store = store.clone();
    let checked_root = original_member.root.clone();
    let issued = Arc::new(tokio::sync::Notify::new());
    let physical_issued = issued.clone();
    let lane_wait = Arc::new(tokio::sync::Notify::new());
    let storage_exhausted = Arc::new(tokio::sync::Notify::new());
    let prefix_witness = Arc::new(tokio::sync::Notify::new());
    let committed_prefix = Arc::new((
        std::sync::atomic::AtomicBool::new(false),
        tokio::sync::Notify::new(),
        tokio::sync::Notify::new(),
    ));
    let ordinary = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ordinary_request = ordinary.clone();
    let mut server = tokio::spawn(async move {
        // The same server task owns both sockets. The no-contention unlimited
        // control requires overlap. Deliberate storage recovery owns an
        // exclusive lane instead and must not wait on that contrary barrier.
        let mut first_socket: Option<tokio::net::TcpStream> = None;
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
            let prior = observed.fetch_add(1, Ordering::SeqCst);
            if prior >= 128 {
                return Err(MezError::invalid_state("recursive request bound"));
            }
            if limits.requires_serial_inference() {
                // Inspect at the physical boundary, before replying. A serial
                // listener alone cannot prove admission or receipt ordering.
                let store = checked_store.clone();
                let root = checked_root.clone();
                tokio::task::spawn_blocking(move || {
                    assert_eq!(
                        store.budget_account(&root).unwrap().known,
                        UsageTotals {
                            input: prior as u64 * 11,
                            output: prior as u64 * 3
                        }
                    );
                    let outstanding: i64 = rusqlite::Connection::open(store.path())
                        .unwrap()
                        .query_row(
                            "SELECT COUNT(*) FROM budget_admissions WHERE retired_receipt IS NULL",
                            [],
                            |row| row.get(0),
                        )
                        .unwrap();
                    assert_eq!(
                        outstanding, 1,
                        "finite wave issued overlapping native grants"
                    );
                })
                .await
                .map_err(|_| MezError::invalid_state("physical receipt ordering failed"))?;
            }
            if stop_issued {
                assert_eq!(prior, 0);
                if partial {
                    let prefix = serde_json::json!({"model":"test-model","choices":[],"usage":{"prompt_tokens":11,"completion_tokens":3}});
                    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {prefix}\n\n").as_bytes()).await?;
                }
                physical_issued.notify_one();
                std::future::pending::<()>().await;
            }
            let content = if ordinary_request.load(Ordering::SeqCst) {
                let request = String::from_utf8_lossy(&bytes);
                assert!(request.contains("one final manual head"));
                assert!(!request.contains("frozen manual source frozen manual source"));
                serde_json::json!({"rationale":"fixture","actions":[{"type":"say","status":"final","content_type":"text/plain; charset=utf-8","text":"original active task completed"}]}).to_string()
            } else {
                "one final manual head".into()
            };
            let body = serde_json::json!({"model":"test-model","choices":[{
                "index":0,"finish_reason":"stop","message":{"role":"assistant","content":content}
            }],"usage":{"prompt_tokens":11,"completion_tokens":3}})
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            if !limits.requires_serial_inference() && !storage_case && prior == 0 {
                first_socket = Some(socket);
                continue;
            }
            if let Some(mut first) = first_socket.take() {
                assert_eq!(prior, 1, "unlimited overlap must precede any response");
                first.write_all(response.as_bytes()).await?;
            }
            socket.write_all(response.as_bytes()).await?;
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
                        .with_streaming_options_for_test(partial)?
                        .with_endpoint(endpoint.clone()),
                    );
                // Consume only timer descriptors; no fake timer callback or
                // archive ACK is needed to drive these genuine wave receipts.
                handle.drain_timer_side_effects(256).await?;
                if stop_issued || prefix_gate_case || storage_case {
                    dispatch
                        .budget_owner
                        .as_mut()
                        .unwrap()
                        .observe_compaction_lane_wait_for_tests(lane_wait.clone());
                }
                if prefix_gate_case {
                    dispatch.budget_owner.as_mut().unwrap()
                        .gate_first_compaction_prefix_for_tests(committed_prefix.clone());
                }
                if storage_exhaust {
                    dispatch.budget_owner.as_mut().unwrap()
                        .observe_storage_exhaustion_for_tests(storage_exhausted.clone());
                }
                let owner = ProviderWorkerOwner::compaction(&dispatch);
                let waiter = handle.clone();
                let decoded = prefix_witness.clone();
                // The real native authorization must encounter failed BEGIN;
                // no mocked grant/refusal or synthetic receipt is installed.
                let blocker = if waves == 0 && storage_case {
                    let db = rusqlite::Connection::open(store.path())?;
                    db.execute_batch("BEGIN IMMEDIATE")?;
                    Some(db)
                } else { None };
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
                            let observer = if partial {
                                observer.with_usage_prefix_witness_for_test(decoded)
                            } else {
                                observer
                            };
                            execute_runtime_agent_compaction_dispatch_with_waiter(
                                dispatch,
                                observer,
                                Some(waiter),
                            )
                        },
                    ),
                );
                assert!(admitted.is_some(), "original monitor admission refused");
                if waves == 0 && storage_case {
                    tokio::time::timeout(Duration::from_secs(10), lane_wait.notified()).await
                        .map_err(|_| MezError::invalid_state("native storage hold unavailable"))?;
                    assert_eq!(calls.load(Ordering::SeqCst), 0);
                    assert_eq!(rows(&store, "budget_admissions"), 0);
                    assert_eq!(rows(&store, "budget_attempts"), 0);
                    if storage_exhaust {
                        tokio::time::timeout(Duration::from_secs(10), storage_exhausted.notified()).await
                            .map_err(|_| MezError::invalid_state("bounded native exhaustion unavailable"))?;
                    }
                    // The original exchange has refused; unblock mandatory
                    // observation settlement, not a fresh authorization.
                    blocker.as_ref().unwrap().execute_batch("ROLLBACK")?;
                }
                if waves == 0 && prefix_gate_case {
                    tokio::time::timeout(Duration::from_secs(10), async {
                        committed_prefix.1.notified().await;
                        lane_wait.notified().await;
                    }).await.map_err(|_| MezError::invalid_state("durable prefix/sibling boundary unavailable"))?;
                    assert_eq!(calls.load(Ordering::SeqCst), 1);
                    assert_eq!(rows(&store, "budget_receipts"), 1);
                    assert_eq!(store.budget_account(&member.root)?.known, UsageTotals { input: 11, output: 3 });
                    let unretired: i64 = rusqlite::Connection::open(store.path())?
                        .query_row("SELECT COUNT(*) FROM budget_admissions WHERE retired_receipt IS NULL", [], |row| row.get(0))?;
                    assert_eq!(unretired, 1, "prefix ACK cannot retire physical exposure");
                    assert!(!handle.budget_compaction_claim_is_quiescent("%1".into(), generation).await?);
                    if stop_prefix {
                        handle.execute_agent_shell_command(primary.clone(), "/stop".into()).await?;
                    }
                    // Stop fences inference, not mandatory original receipt
                    // settlement. Release only this artificial test await.
                    committed_prefix.2.notify_one();
                }
                if stop_issued {
                    tokio::time::timeout(Duration::from_secs(10), async {
                        issued.notified().await;
                        lane_wait.notified().await;
                    })
                    .await
                    .map_err(|_| {
                        MezError::invalid_state("issued/refused sibling boundary unavailable")
                    })?;
                    if partial {
                        tokio::time::timeout(Duration::from_secs(10), prefix_witness.notified())
                            .await
                            .map_err(|_| {
                                MezError::invalid_state("original SSE prefix was not decoded")
                            })?;
                    }
                    assert_eq!(calls.load(Ordering::SeqCst), 1);
                    assert_eq!(rows(&store, "budget_admissions"), 1);
                    assert!(
                        !handle
                            .budget_compaction_claim_is_quiescent("%1".into(), generation)
                            .await?
                    );
                    handle
                        .execute_agent_shell_command(primary.clone(), "/stop".into())
                        .await?;
                }
                if waves == 0
                    && let Some(stop) = stop_held
                {
                    tokio::time::timeout(Duration::from_secs(10), async {
                        loop {
                            if handle
                                .budget_compaction_claim_is_quiescent("%1".into(), generation)
                                .await?
                            {
                                return Ok::<_, MezError>(());
                            }
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    })
                    .await
                    .map_err(|_| MezError::invalid_state("original finite wave did not park"))??;
                    assert_eq!(calls.load(Ordering::SeqCst), 1);
                    assert_eq!(
                        store.budget_account(&member.root)?.known,
                        UsageTotals {
                            input: 11,
                            output: 3
                        }
                    );
                    assert_eq!(
                        workers.len(),
                        1,
                        "the original monitored wave must remain owned"
                    );
                    assert!(
                        handle
                            .claim_agent_compaction_task("%1".into(), generation)
                            .await?
                            .is_none()
                    );
                    if stop {
                        handle
                            .execute_agent_shell_command(primary.clone(), "/stop".into())
                            .await?;
                    }
                    let writer = store.clone();
                    tokio::task::spawn_blocking(move || {
                        let projection = writer.budget_policy_projection()?;
                        let prior = projection.committed.unwrap();
                        let operation = BudgetPolicyPublication {
                            namespace: projection.namespace,
                            id: BudgetReceiptId::new("recursive-finite-release").unwrap(),
                            expected_revision: projection.revision,
                            previous_digest: prior.digest,
                            proposed: BudgetConfigPolicy {
                                source: prior.source,
                                digest: "e".repeat(64),
                                limits: BudgetLimits {
                                    input: mez_agent::token_budget::TokenLimit::Limited(1_000_000),
                                    output: mez_agent::token_budget::TokenLimit::Limited(1_000_000),
                                },
                            },
                            supersedes: None,
                        };
                        writer.begin_budget_policy_publication(&operation)?;
                        writer.reconcile_budget_policy_publication(
                            &operation,
                            &operation.proposed.digest,
                        )?;
                        Ok::<_, MezError>(())
                    })
                    .await
                    .map_err(|_| MezError::invalid_state("finite release writer failed"))??;
                }
                let joined =
                    tokio::time::timeout(Duration::from_secs(30), workers.join_next_with_id())
                        .await
                        .map_err(|_| {
                            MezError::invalid_state("recursive wave observation deadline")
                        })?
                        .ok_or_else(|| MezError::invalid_state("missing original monitor"))?;
                if let Some(db) = blocker {
                    drop(db);
                }
                assert!(
                    if storage_exhaust {
                        matches!(&joined, Ok((_, Ok(Some((RuntimeEvent::AgentCompaction(
                            AgentCompactionEvent::Failed { kind, .. }), _))))) if kind == "budget_suspended")
                    } else if stop_held == Some(true) || stop_issued || stop_prefix {
                        matches!(&joined, Ok((_, Ok(None))))
                            || matches!(
                                &joined,
                                Ok((
                                    _,
                                    Ok(Some((
                                        RuntimeEvent::AgentCompaction(
                                            AgentCompactionEvent::Failed { .. }
                                        ),
                                        _
                                    )))
                                ))
                            )
                    } else {
                        matches!(
                            &joined,
                            Ok((
                                _,
                                Ok(Some((
                                    RuntimeEvent::AgentCompaction(
                                        AgentCompactionEvent::Completed { .. }
                                    ),
                                    _
                                )))
                            ))
                        )
                    },
                    "original wave failure: {:?}",
                    match &joined {
                        Ok((
                            _,
                            Ok(Some((
                                RuntimeEvent::AgentCompaction(AgentCompactionEvent::Failed {
                                    kind,
                                    message,
                                    ..
                                }),
                                _,
                            ))),
                        )) => Some((kind.as_str(), message.as_str())),
                        _ => None,
                    }
                );
                record_owned_agent_provider_worker(
                    joined,
                    &handle,
                    &mut report,
                    &mut owners,
                    &drains,
                )
                .await?;
                waves += 1;
                if storage_exhaust {
                    assert_eq!(calls.load(Ordering::SeqCst), 0);
                    assert_eq!(rows(&store, "budget_attempts"), 0);
                    assert_eq!(store.budget_account(&member.root)?.known, UsageTotals::default());
                    assert_eq!(accepted_heads(&handle).await?, 0);
                    return Ok::<_, MezError>((waves, member.root, false));
                }
                if stop_held == Some(true) || stop_issued || stop_prefix {
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
                    assert_eq!(calls.load(Ordering::SeqCst), 1);
                    assert_eq!(accepted_heads(&handle).await?, 0);
                    assert!(
                        handle
                            .claim_agent_compaction_task("%1".into(), generation)
                            .await?
                            .is_none()
                    );
                    return Ok::<_, MezError>((waves, member.root, false));
                }
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
                        if let Some(turn) = &original_turn {
                            let tasks = handle.pending_agent_provider_tasks().await?;
                            assert_eq!(tasks.len(), 1, "one original continuation after adoption");
                            assert_eq!(&tasks[0].turn_id, turn);
                            let agent = AgentId::opaque(tasks[0].agent_id.clone()).unwrap();
                            let mut resumed = handle.claim_configured_agent_provider_task(agent.clone(), turn.clone()).await?
                                .ok_or_else(|| MezError::invalid_state("original active continuation not claimable"))?;
                            assert_eq!(resumed.budget_owner.as_ref().unwrap().member.root, member.root);
                            resumed.provider = crate::runtime::RuntimeAgentProviderDispatchProvider::OpenAiCompatible(
                                OpenAiCompatibleChatCompletionsProvider::without_auth(ReqwestProviderHttpTransport)?
                                    .with_provider_id("local-chat")?.with_streaming_options_for_test(false)?
                                    .with_endpoint(endpoint.clone()),
                            );
                            ordinary.store(true, Ordering::SeqCst);
                            let before_send = calls.load(Ordering::SeqCst);
                            let owner = ProviderWorkerOwner::inference(agent.clone(), &resumed);
                            assert!(spawn_owned_agent_provider_worker(
                                &mut workers, &mut owners, Some(owner),
                                monitor_runtime_agent_provider_dispatch(handle.clone(), agent, turn.clone(), resumed, drains.clone()),
                            ).is_some());
                            let joined = tokio::time::timeout(Duration::from_secs(30), workers.join_next_with_id()).await
                                .map_err(|_| MezError::invalid_state("original continuation observation deadline"))?
                                .ok_or_else(|| MezError::invalid_state("original continuation monitor unavailable"))?;
                            let Ok((_, Ok(Some((RuntimeEvent::AgentProvider(crate::runtime::AgentProviderEvent::BudgetSettled { completion, settlement }), _))))) = &joined else {
                                panic!("original continuation lacks its budget-settled result");
                            };
                            assert!(matches!(completion.as_ref(), crate::runtime::AgentProviderEvent::Completed { turn_id, .. } if turn_id == turn));
                            assert!(settlement.matches_result(completion));
                            let (first, released) = settlement.observations();
                            assert_eq!(first.known, released.known);
                            assert_eq!(released.known, UsageTotals {
                                input: (before_send as u64 + 1) * 11,
                                output: (before_send as u64 + 1) * 3,
                            });
                            record_owned_agent_provider_worker(joined, &handle, &mut report, &mut owners, &drains).await?;
                            assert_eq!(calls.load(Ordering::SeqCst), before_send + 1);
                            assert_eq!(store.budget_account(&member.root)?.known, UsageTotals {
                                input: (before_send as u64 + 1) * 11,
                                output: (before_send as u64 + 1) * 3,
                            });
                            // Provider completion is not archive completion.
                            // Adopt original required writes before requiring
                            // final turn retirement, never synthesize an ACK.
                            for _ in 0..16 {
                                let settled = run_async_persistence_side_effect_service(
                                    &handle,
                                    AsyncRuntimeSideEffectServiceConfig {
                                        max_polls: 1, drain_limit: 64,
                                        idle_interval: Duration::from_millis(1),
                                    },
                                    |_, _| true,
                                ).await?;
                                assert_eq!(settled.failed, 0);
                                if !handle.agent_turn_is_running(turn).await? { break; }
                            }
                            assert!(!handle.agent_turn_is_running(turn).await?);
                            assert!(handle.claim_configured_agent_provider_task(
                                AgentId::opaque("agent-%1").unwrap(), turn.clone(),
                            ).await?.is_none(), "completed original continuation cannot be claimed twice");
                        }
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
    assert_eq!(
        published,
        !stop_between
            && stop_held != Some(true)
            && !stop_issued
            && !stop_prefix
            && !storage_exhaust
    );
    assert_eq!(claims as usize, count);
    if storage_exhaust {
        assert_eq!(count, 0);
    } else {
        assert!(count > 0);
    }
    if let Some(turn) = &original_turn {
        assert_eq!(
            exit.service.agent_turn_ledger().turn(turn).unwrap().state,
            mez_agent::AgentTurnState::Completed,
            "the original continuation must succeed, not merely retire"
        );
    }
    if stop_issued {
        assert_eq!(
            store.budget_account(&original_member.root).unwrap().known,
            if partial {
                UsageTotals {
                    input: 11,
                    output: 3,
                }
            } else {
                UsageTotals::default()
            }
        );
        let snapshot: String = rusqlite::Connection::open(store.path())
            .unwrap()
            .query_row("SELECT snapshot FROM budget_attempts", [], |row| row.get(0))
            .unwrap();
        let snapshot: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
        assert_eq!(
            snapshot["terminal"],
            serde_json::json!({"input":null,"output":null,"terminal":true}),
            "local retirement cannot fabricate measured zero usage"
        );
    }
    if stop_between || stop_held == Some(true) || stop_issued || stop_prefix || storage_exhaust {
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
