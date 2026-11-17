//! Actual HTTP response -> durable terminal receipt -> original monitor hold.
//! Adoption controls also execute the original harmless native candidate once.

use super::*;
use crate::host::async_runtime::AsyncRuntimeSessionActor;
use crate::integrations::http::ReqwestProviderHttpTransport;
use futures_util::FutureExt;
use mez_agent::token_budget::TokenLimit;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn budget_completed_response_equality_holds_original_until_sufficient_raise() {
    Box::pin(qualify(CompletionCase::Release)).await;
}

#[tokio::test]
async fn budget_completed_response_stop_cannot_publish_after_policy_raise() {
    Box::pin(qualify(CompletionCase::Stop)).await;
}

#[tokio::test]
async fn budget_completed_response_read_refusal_retains_original_until_recovery() {
    Box::pin(qualify(CompletionCase::ReadFailure)).await;
}

#[tokio::test]
async fn budget_completed_response_adopts_and_executes_original_native_candidate_once() {
    Box::pin(qualify(CompletionCase::Adopt)).await;
}

/// A previously offered action is not an issued effect. Lowering after response
/// adoption must fence the real native claim, retaining it for a sufficient edit.
#[tokio::test]
async fn budget_completed_response_prepared_native_claim_rechecks_lowered_policy() {
    Box::pin(qualify(CompletionCase::ClaimRecross)).await;
}

/// A descriptive read can pass a reserved SQLite writer. Native effect
/// admission must instead order after that writer, without issuing the offer.
#[tokio::test]
async fn budget_completed_response_native_claim_orders_after_existing_writer() {
    Box::pin(qualify(CompletionCase::ClaimWriterContention)).await;
}

/// An approved fetch is still only an offer until its budget claim is ordered
/// against the original accounting writer. The HTTP effect must happen once.
#[tokio::test]
async fn budget_completed_response_external_claim_orders_after_existing_writer() {
    Box::pin(qualify(CompletionCase::ExternalWriterContention)).await;
}

#[tokio::test]
async fn budget_completed_response_external_claim_boundary_and_withdrawal_are_once_only() {
    for mode in [
        CompletionCase::ExternalBeforeRecross,
        CompletionCase::ExternalClaimDrop,
        CompletionCase::ExternalCaptureDrop,
        CompletionCase::ExternalObserverDrop,
        CompletionCase::ExternalPause,
        CompletionCase::ExternalStop,
    ] {
        Box::pin(qualify(mode)).await;
    }
}

/// A native claim linearizes before the later policy writer, even if actor
/// handoff follows it. Dropped untransferred claims instead release only their
/// original reservation and leave the offer eligible for a fresh claim.
#[tokio::test]
async fn budget_completed_response_native_claim_boundary_and_withdrawal_are_once_only() {
    for mode in [
        CompletionCase::NativeClaimBeforeRecross,
        CompletionCase::NativeClaimDrop,
        CompletionCase::NativeCaptureDrop,
        CompletionCase::NativeReadObserverDrop,
    ] {
        Box::pin(qualify(mode)).await;
    }
}

#[tokio::test]
async fn budget_completed_response_claimed_native_cannot_clear_human_pause() {
    Box::pin(qualify(CompletionCase::NativeClaimPause)).await;
}

#[tokio::test]
async fn budget_completed_response_native_claim_cannot_revive_stopped_owner() {
    Box::pin(qualify(CompletionCase::NativeClaimStop)).await;
}

/// A native transaction can observe an internally valid but regressed account.
/// Adoption must preserve the previously observed integrity fence, not mistake
/// its acknowledgment for permission to transfer the original effect claim.
#[tokio::test]
async fn budget_completed_response_native_claim_refuses_regressed_account() {
    Box::pin(qualify(CompletionCase::NativeClaimRegression)).await;
}

#[tokio::test]
async fn budget_completed_response_capture_and_reader_refusal_retain_original_native_offer() {
    for mode in [CompletionCase::CaptureRefusal, CompletionCase::ReaderPanic] {
        Box::pin(qualify(mode)).await;
    }
}

#[tokio::test]
async fn budget_completed_response_stop_after_producer_release_refuses_actor_adoption() {
    Box::pin(qualify(CompletionCase::StopBeforeAdoption)).await;
}

#[tokio::test]
async fn budget_completed_response_rejects_nested_settlement_without_losing_original() {
    Box::pin(qualify(CompletionCase::EnvelopeRefusal)).await;
}

#[tokio::test]
async fn budget_completed_response_rejects_substituted_payload_and_expense() {
    Box::pin(qualify(CompletionCase::PayloadRefusal)).await;
}

#[tokio::test]
async fn budget_completed_response_human_pause_is_independent_in_both_release_orders() {
    for mode in [CompletionCase::BudgetFirst, CompletionCase::HumanFirst] {
        Box::pin(qualify(mode)).await;
    }
}

/// A genuine resume/timer adoption cannot authorize publication using the read
/// from before that adoption. Repark the same response, then execute it once.
#[tokio::test]
async fn budget_completed_response_policy_recross_after_timer_adoption_reparks_original() {
    Box::pin(qualify(CompletionCase::Recross)).await;
}

#[tokio::test]
async fn budget_completed_response_stop_after_timer_adoption_cannot_offer_candidate() {
    Box::pin(qualify(CompletionCase::StopAfterResume)).await;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CompletionCase {
    Release,
    Stop,
    ReadFailure,
    Adopt,
    ClaimRecross,
    ClaimWriterContention,
    ExternalWriterContention,
    ExternalBeforeRecross,
    ExternalClaimDrop,
    ExternalCaptureDrop,
    ExternalObserverDrop,
    ExternalPause,
    ExternalStop,
    NativeClaimBeforeRecross,
    NativeClaimDrop,
    NativeCaptureDrop,
    NativeClaimPause,
    NativeClaimStop,
    NativeClaimRegression,
    NativeReadObserverDrop,
    CaptureRefusal,
    ReaderPanic,
    StopBeforeAdoption,
    EnvelopeRefusal,
    PayloadRefusal,
    BudgetFirst,
    HumanFirst,
    MonitorError,
    Recross,
    StopAfterResume,
}

#[tokio::test]
async fn budget_completed_response_native_monitor_error_retires_retained_writer() {
    Box::pin(qualify(CompletionCase::MonitorError)).await;
}

/// Runs the real original append/ACK owner, never fabricates a history receipt.
async fn settle_external_fixture_bookkeeping(
    handle: &crate::host::async_runtime::AsyncRuntimeSessionHandle,
) -> crate::Result<()> {
    let report = crate::host::async_runtime::run_async_persistence_side_effect_service(
        handle,
        crate::host::async_runtime::AsyncRuntimeSideEffectServiceConfig {
            max_polls: 16,
            drain_limit: 64,
            idle_interval: Duration::from_millis(1),
        },
        |polls, _| polls >= 16,
    )
    .await?;
    if report.failed != 0 {
        return Err(MezError::invalid_state(
            "external fixture original bookkeeping failed",
        ));
    }
    Ok(())
}

async fn qualify(mode: CompletionCase) {
    let external = matches!(
        mode,
        CompletionCase::ExternalWriterContention
            | CompletionCase::ExternalBeforeRecross
            | CompletionCase::ExternalClaimDrop
            | CompletionCase::ExternalCaptureDrop
            | CompletionCase::ExternalObserverDrop
            | CompletionCase::ExternalPause
            | CompletionCase::ExternalStop
    );
    let stop = matches!(mode, CompletionCase::Stop | CompletionCase::StopAfterResume);
    let read_failure = mode == CompletionCase::ReadFailure;
    let adopt = matches!(
        mode,
        CompletionCase::Adopt
            | CompletionCase::ClaimRecross
            | CompletionCase::ClaimWriterContention
            | CompletionCase::ExternalWriterContention
            | CompletionCase::ExternalBeforeRecross
            | CompletionCase::ExternalClaimDrop
            | CompletionCase::ExternalCaptureDrop
            | CompletionCase::ExternalObserverDrop
            | CompletionCase::ExternalPause
            | CompletionCase::ExternalStop
            | CompletionCase::NativeClaimBeforeRecross
            | CompletionCase::NativeClaimDrop
            | CompletionCase::NativeCaptureDrop
            | CompletionCase::NativeClaimPause
            | CompletionCase::NativeClaimStop
            | CompletionCase::NativeClaimRegression
            | CompletionCase::NativeReadObserverDrop
            | CompletionCase::CaptureRefusal
            | CompletionCase::ReaderPanic
            | CompletionCase::StopBeforeAdoption
            | CompletionCase::EnvelopeRefusal
            | CompletionCase::PayloadRefusal
            | CompletionCase::BudgetFirst
            | CompletionCase::HumanFirst
            | CompletionCase::MonitorError
            | CompletionCase::Recross
    );
    let human = matches!(
        mode,
        CompletionCase::BudgetFirst | CompletionCase::HumanFirst
    );
    let Prepared {
        mut service,
        mut dispatch,
        store,
        member,
        mut root,
    } = prepared_case(BudgetLimits {
        input: TokenLimit::Limited(11),
        output: TokenLimit::Limited(3),
    })
    .await;
    let resume_gate = if matches!(
        mode,
        CompletionCase::Recross | CompletionCase::StopAfterResume
    ) {
        let gate = Arc::new((tokio::sync::Notify::new(), tokio::sync::Notify::new()));
        dispatch
            .budget_owner
            .as_mut()
            .unwrap()
            .gate_completion_resume_for_tests(gate.clone());
        Some(gate)
    } else {
        None
    };
    // Only this private native-effect control bypasses interactive approval;
    // policy/scope admission and actual executor/retirement remain production.
    if adopt {
        service.permission_policy_mut().set_approval_bypass(true);
    }
    let counter = root.0.as_ref().unwrap().join("retained-candidate-count");
    let command = if adopt {
        format!(
            "printf x >> '{}'; printf retained-native-candidate",
            counter.display()
        )
    } else {
        "printf retained-native-candidate".to_owned()
    };
    let primary = service
        .session()
        .clients()
        .iter()
        .find(|client| service.session().is_attached_primary(&client.id))
        .unwrap()
        .id
        .clone();
    let turn = dispatch.turn.turn_id.clone();
    let agent = AgentId::opaque(dispatch.turn.agent_id.clone()).unwrap();
    let external_count = Arc::new(AtomicUsize::new(0));
    let accepted_external_result = Arc::new(std::sync::Mutex::new(None));
    let external_listener = if external {
        Some(tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap())
    } else {
        None
    };
    let external_url = external_listener
        .as_ref()
        .map(|listener| format!("http://{}/original", listener.local_addr().unwrap()));
    let expected_external_url = external_url.clone();
    let fetch_count = external_count.clone();
    let external_server = external_listener.map(|listener| tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            assert!(socket.read(&mut request).await.unwrap() > 0);
            fetch_count.fetch_add(1, Ordering::SeqCst);
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 8\r\nConnection: close\r\n\r\noriginal").await.unwrap();
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider =
        OpenAiCompatibleChatCompletionsProvider::without_auth(ReqwestProviderHttpTransport)
            .unwrap()
            .with_provider_id("local-chat")
            .unwrap()
            .with_streaming_options_for_test(false)
            .unwrap()
            .with_endpoint(format!("http://{}/v1", listener.local_addr().unwrap()));
    dispatch.provider =
        crate::runtime::RuntimeAgentProviderDispatchProvider::OpenAiCompatible(provider);
    let sends = Arc::new(AtomicUsize::new(0));
    let count = sends.clone();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        loop {
            let mut chunk = [0; 2048];
            let n = socket.read(&mut chunk).await.unwrap();
            assert_ne!(n, 0, "request closed before body");
            bytes.extend_from_slice(&chunk[..n]);
            if let Some(end) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                let length: usize = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:").map(str::trim))
                    .unwrap()
                    .parse()
                    .unwrap();
                if bytes.len() >= end + 4 + length {
                    break;
                }
            }
            assert!(bytes.len() < 1024 * 1024);
        }
        count.fetch_add(1, Ordering::SeqCst);
        let content = if let Some(url) = external_url {
            serde_json::json!({"rationale":"fixture", "actions":[{"type":"fetch_url", "url":url}]})
                .to_string()
        } else {
            serde_json::json!({"rationale":"fixture", "actions":[{"type":"shell_command", "summary":"Original retained candidate", "command":command}]}).to_string()
        };
        let body = serde_json::json!({"id":"original-completion", "choices":[{"index":0,"message":{"role":"assistant","content":content},"finish_reason":"stop"}],"usage":{"prompt_tokens":11,"completion_tokens":3,"total_tokens":14}}).to_string();
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        // Observe any unexpected retry instead of disappearing after reply one.
        while let Ok((mut extra, _)) = listener.accept().await {
            count.fetch_add(1, Ordering::SeqCst);
            let _ = extra.shutdown().await;
        }
    });
    let (handle, actor) = AsyncRuntimeSessionActor::new(service, Default::default()).unwrap();
    let drains = ProviderObservationDrains::default();
    let native_finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let control = async {
        let mut native_workers = JoinSet::new();
        let mut native_owners = AgentProviderWorkerOwners::default();
        let mut fault_owner = None;
        let mut report = AsyncAgentProviderPollReport {
            polls: 0,
            executions: 0,
            idle_polls: 0,
            terminal_state: RuntimeLifecycleState::Running,
        };
        let mut worker = tokio::spawn(monitor_runtime_agent_provider_dispatch(
            handle.clone(),
            agent,
            turn.clone(),
            dispatch,
            drains.clone(),
        ));
        let mut joined = false;
        let exercise = std::panic::AssertUnwindSafe(async {
            tokio::time::timeout(Duration::from_secs(8), async {
                loop {
                    if worker.is_finished() { return Err("completion escaped before equality hold".to_owned()); }
                    let parked = handle.drain_timer_side_effects(64).await.unwrap().iter().any(|effect| matches!(effect,
                        crate::runtime::RuntimeSideEffect::CancelTimer { key }
                        if key.kind == crate::runtime::RuntimeTimerKind::ProviderClaim && key.owner_id == turn && key.generation == 7
                    ));
                    if parked { return Ok(()); }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }).await.map_err(|_| "completed original did not park".to_owned())??;
            if store.budget_account(&member.root).unwrap().known != (UsageTotals { input: 11, output: 3 }) {
                return Err("original expense not settled before completion hold".to_owned());
            }
            if rows(&store, "budget_receipts") != 2 || !handle.agent_turn_is_running(&turn).await.unwrap() {
                return Err("original terminal receipt or claim missing".to_owned());
            }
            if mode == CompletionCase::Release {
                waiting::assert_budget_notice_once(&handle).await;
                tokio::time::sleep(Duration::from_millis(350)).await;
                waiting::assert_budget_notice_once(&handle).await;
            }
            if counter.exists() || handle.drain_runtime_side_effects(256).await.unwrap().iter().any(|effect| matches!(effect,
                crate::runtime::RuntimeSideEffect::DispatchNativeShellAction { turn_id, .. } if turn_id == &turn
            )) {
                return Err("candidate reached native action admission while completion held".to_owned());
            }
            if human {
                waiting::browser_input(&handle, &primary, b"/list-agents --all\r").await;
                waiting::browser_input(&handle, &primary, b"p").await;
                waiting::assert_browser_paused(&handle, "%1", true).await;
                if mode == CompletionCase::HumanFirst {
                    waiting::browser_input(&handle, &primary, b"p").await;
                    waiting::assert_browser_paused(&handle, "%1", false).await;
                }
            }
            // Clearing input alone cannot release the exhausted output side.
            super::remember_retirement::publish_policy(&store, "completion-insufficient", 'c', BudgetLimits {
                input: TokenLimit::Unlimited, output: TokenLimit::Limited(3),
            }).await;
            if tokio::time::timeout(Duration::from_millis(450), &mut worker).await.is_ok() {
                joined = true;
                return Err("insufficient raise published original candidate".to_owned());
            }
            let original = if read_failure {
                let db = rusqlite::Connection::open(store.path()).unwrap();
                let original: (String, String) = db.query_row(
                    "SELECT id,body FROM budget_receipts WHERE id=(SELECT retired_receipt FROM budget_admissions LIMIT 1)",
                    [], |row| Ok((row.get(0)?, row.get(1)?)),
                ).unwrap();
                db.execute("UPDATE budget_receipts SET body='{}' WHERE id=?1", [&original.0]).unwrap();
                Some(original)
            } else { None };
            if mode == CompletionCase::Stop { handle.execute_agent_shell_command(primary.clone(), "/stop".into()).await.unwrap(); }
            super::remember_retirement::publish_policy(&store, "completion-release", 'd', BudgetLimits::default()).await;
            if let Some(gate) = &resume_gate {
                tokio::time::timeout(Duration::from_secs(8), gate.0.notified()).await
                    .map_err(|_| "original completion did not reach adopted resume".to_owned())?;
                let timers = handle.drain_timer_side_effects(64).await.unwrap();
                if !timers.iter().any(|effect| matches!(effect,
                    crate::runtime::RuntimeSideEffect::ScheduleTimer { key, .. }
                    if key.kind == crate::runtime::RuntimeTimerKind::ProviderClaim && key.owner_id == turn && key.generation == 7)) {
                    return Err("resume gate preceded actual original timer adoption".to_owned());
                }
                if mode == CompletionCase::StopAfterResume {
                    handle.execute_agent_shell_command(primary.clone(), "/stop".into()).await.unwrap();
                    gate.1.notify_one();
                } else {
                    super::remember_retirement::publish_policy(&store, "completion-recross", 'e', BudgetLimits {
                        input: TokenLimit::Limited(11), output: TokenLimit::Limited(3),
                    }).await;
                    gate.1.notify_one();
                    tokio::time::timeout(Duration::from_secs(8), async {
                        loop {
                            if worker.is_finished() { return Err("recross escaped instead of reparking original".to_owned()); }
                            let parked = handle.drain_timer_side_effects(64).await.unwrap().iter().any(|effect| matches!(effect,
                                crate::runtime::RuntimeSideEffect::CancelTimer { key }
                                if key.kind == crate::runtime::RuntimeTimerKind::ProviderClaim && key.owner_id == turn && key.generation == 7));
                            if parked { return Ok(()); }
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    }).await.map_err(|_| "recross did not park original after adopted timer".to_owned())??;
                    if !handle.agent_turn_is_running(&turn).await.unwrap() || counter.exists() {
                        return Err("recross lost original claim or executed candidate".to_owned());
                    }
                    super::remember_retirement::publish_policy(&store, "completion-second-release", 'f', BudgetLimits::default()).await;
                    tokio::time::timeout(Duration::from_secs(8), gate.0.notified()).await
                        .map_err(|_| "reparked original did not reacquire".to_owned())?;
                    gate.1.notify_one();
                }
            }
            if mode == CompletionCase::BudgetFirst {
                let early = tokio::time::timeout(Duration::from_millis(2300), &mut worker).await;
                joined |= early.is_ok();
                if early.is_ok() { return Err("budget lift cleared independent human Pause".to_owned()); }
                waiting::assert_browser_paused(&handle, "%1", true).await;
                waiting::browser_input(&handle, &primary, b"p").await;
                waiting::assert_browser_paused(&handle, "%1", false).await;
            }
            if let Some((id, body)) = original {
                let early = tokio::time::timeout(Duration::from_millis(2300), &mut worker).await;
                // A ready JoinHandle has already been consumed, even if the
                // following exact restoration fails and cleanup unwinds.
                joined |= early.is_ok();
                // Restore this fixture's exact before-image even if the
                // regression returned an early error/completion.
                rusqlite::Connection::open(store.path()).unwrap().execute(
                    "UPDATE budget_receipts SET body=?1 WHERE id=?2", [&body, &id],
                ).unwrap();
                if early.is_ok() {
                    return Err("read refusal discarded or published original response".to_owned());
                }
            }
            let event = tokio::time::timeout(Duration::from_secs(8), &mut worker).await
                .map_err(|_| "original completion did not settle after policy/Stop".to_owned())?;
            joined = true;
            let event = event
                .map_err(|error| error.to_string())?.map_err(|error| error.to_string())?;
            if mode == CompletionCase::StopAfterResume {
                // Either the monitor notices Stop, or its authentic completed
                // envelope loses current adoption. Neither can offer an effect.
                if let Some((event, _)) = event {
                    let mut batch = RuntimeEventBatch::new();
                    batch.push(event);
                    if handle.submit_runtime_events(batch).await.map_err(|error| error.to_string())?.applied != 0 {
                        return Err("stopped resumed completion adopted".to_owned());
                    }
                }
                if handle.drain_runtime_side_effects(256).await.unwrap().iter().any(|effect| matches!(effect,
                    crate::runtime::RuntimeSideEffect::DispatchNativeShellAction { turn_id, .. } if turn_id == &turn)) {
                    return Err("Stop after timer adoption offered native candidate".to_owned());
                }
                return Ok(false);
            }
            let published = match &event {
                Some((RuntimeEvent::AgentProvider(AgentProviderEvent::BudgetSettled { completion, settlement }), _)) => {
                    use crate::storage::token_usage::budget::receipts::BudgetSettlementState;
                    let (first, released) = settlement.observations();
                    if first.state() != (BudgetSettlementState::Exhausted { input: true, output: true })
                        || released.state() != BudgetSettlementState::WithinKnownLimits
                        || first.receipt != released.receipt
                        || released.known.input != 11 || released.known.output != 3
                        || released.revisions.projection <= first.revisions.projection {
                        return Err("original crossing/release observation was not delivered".to_owned());
                    }
                    matches!(completion.as_ref(), AgentProviderEvent::Completed { execution, .. }
                        if if external {
                            execution.response.action_batch.as_ref().is_some_and(|batch|
                                batch.actions.len() == 1 && matches!(&batch.actions[0].payload,
                                    mez_agent::AgentActionPayload::FetchUrl { url, .. } if Some(url) == expected_external_url.as_ref()))
                        } else { execution.response.action_batch.is_some() && execution.response.raw_text.contains("retained-native-candidate") })
                }
                _ => false,
            };
            if adopt {
                let (event, _) = event.ok_or_else(|| "original completion missing at adoption".to_owned())?;
                if mode == CompletionCase::PayloadRefusal {
                    for change in 0..4 {
                        let mut substituted = event.clone();
                        let RuntimeEvent::AgentProvider(AgentProviderEvent::BudgetSettled { completion, .. }) = &mut substituted else { unreachable!() };
                        let AgentProviderEvent::Completed { agent_id, turn_id, claim_generation, execution } = completion.as_mut() else { unreachable!() };
                        match change {
                            0 => execution.response.raw_text.push_str(" substituted"),
                            1 => execution.response.usage.output_tokens += 1000,
                            2 => *claim_generation += 1,
                            _ => {
                                let replacement = AgentProviderEvent::RoutingSelected {
                                    agent_id: agent_id.clone(), turn_id: turn_id.clone(), claim_generation: *claim_generation,
                                    selection: Box::new(mez_agent::AutoSizingRoutingSelection {
                                        selected_profile: mez_agent::ModelProfile {
                                            provider: execution.request.provider.clone(), model: execution.request.model.clone(),
                                            ..Default::default()
                                        },
                                        selected_profile_name: "substituted".into(), routing_token_usage_by_model: Default::default(),
                                        decision_summary: None, fallback: None,
                                    }),
                                };
                                **completion = replacement;
                            }
                        }
                        let mut invalid = RuntimeEventBatch::new();
                        invalid.push(substituted);
                        if handle.submit_runtime_events(invalid).await.is_ok() {
                            return Err("same-generation substituted settlement was accepted".to_owned());
                        }
                    }
                    if handle.drain_runtime_side_effects(256).await.unwrap().iter().any(|effect| matches!(effect,
                        crate::runtime::RuntimeSideEffect::DispatchNativeShellAction { turn_id, .. } if turn_id == &turn)) {
                        return Err("substituted settlement offered a native effect".to_owned());
                    }
                }
                if mode == CompletionCase::EnvelopeRefusal {
                    let RuntimeEvent::AgentProvider(AgentProviderEvent::BudgetSettled { settlement, .. }) = &event else {
                        return Err("original settlement envelope unavailable".to_owned());
                    };
                    // A real envelope is not authority to wrap another envelope.
                    // Rejection must precede mutation and leave the original
                    // claim able to adopt its exact result once afterward.
                    let mut malformed = RuntimeEventBatch::new();
                    let RuntimeEvent::AgentProvider(original) = event.clone() else { unreachable!() };
                    malformed.push(RuntimeEvent::AgentProvider(AgentProviderEvent::BudgetSettled {
                        completion: Box::new(original), settlement: settlement.clone(),
                    }));
                    if handle.submit_runtime_events(malformed).await.is_ok() {
                        return Err("nested settlement was accepted".to_owned());
                    }
                    let premature = handle.drain_runtime_side_effects(256).await.unwrap();
                    if premature.iter().any(|effect| matches!(effect, crate::runtime::RuntimeSideEffect::DispatchNativeShellAction { turn_id, .. } if turn_id == &turn)) {
                        return Err("rejected settlement offered a native effect".to_owned());
                    }
                }
                if mode == CompletionCase::StopBeforeAdoption {
                    handle.execute_agent_shell_command(primary.clone(), "/stop".into()).await.unwrap();
                }
                let mut batch = RuntimeEventBatch::new();
                batch.push(event.clone());
                let adopted = handle.submit_runtime_events(batch).await.map_err(|error| error.to_string())?;
                let expected = mode != CompletionCase::StopBeforeAdoption;
                if (adopted.applied > 0) != expected { return Err("original Completed adoption disposition changed".to_owned()); }
                let offers = handle.drain_runtime_side_effects(256).await.unwrap().into_iter().filter_map(|effect| match effect {
                    crate::runtime::RuntimeSideEffect::DispatchNativeShellAction { turn_id, action_id } if turn_id == turn => Some(action_id),
                    crate::runtime::RuntimeSideEffect::DispatchApprovedExternalAction { turn_id, action_id } if external && turn_id == turn => Some(action_id),
                    _ => None,
                }).collect::<Vec<_>>();
                if offers.len() != usize::from(expected) { return Err(format!("native candidate offers: expected {expected}, got {}", offers.len())); }
                if let Some(action) = offers.first() {
                    if external {
                        if mode == CompletionCase::ExternalWriterContention {
                            let writer = rusqlite::Connection::open(store.path()).unwrap();
                            writer.execute_batch("BEGIN IMMEDIATE").unwrap();
                            let refused = handle.claim_approved_external_action(turn.clone(), action.clone()).await.unwrap().is_none();
                            writer.execute_batch("ROLLBACK").unwrap();
                            drop(writer);
                            if !refused { return Err("external claim bypassed the original accounting/policy writer".to_owned()); }
                        }
                        let preclaimed = if mode != CompletionCase::ExternalWriterContention {
                            let crate::runtime::RuntimeBudgetNativeClaimPreparation::Work(mut work) = handle.prepare_budget_external_claim(turn.clone(), action.clone()).await.unwrap() else {
                                return Err("original external offer did not prepare".to_owned());
                            };
                            if mode == CompletionCase::ExternalCaptureDrop {
                                drop(work);
                                None
                            } else {
                                let claim = if mode == CompletionCase::ExternalObserverDrop {
                                    let (entered, release) = work.gate_native_writer_for_tests();
                                    let mut writer = tokio::task::spawn_blocking(move || work.execute());
                                    tokio::time::timeout(Duration::from_secs(5), entered.notified()).await.unwrap();
                                    let mut observer = Box::pin(async { (&mut writer).await });
                                    let pending = observer.as_mut().now_or_never().is_none();
                                    drop(observer);
                                    let coalesced = matches!(handle.prepare_budget_external_claim(turn.clone(), action.clone()).await.unwrap(), crate::runtime::RuntimeBudgetNativeClaimPreparation::Refused);
                                    release.send(()).unwrap();
                                    let claim = writer.await.unwrap();
                                    if !pending || !coalesced { return Err("cancelled external observer released active writer custody".to_owned()); }
                                    claim
                                } else { tokio::task::spawn_blocking(move || work.execute()).await.unwrap() };
                                if !matches!(handle.prepare_budget_external_claim(turn.clone(), action.clone()).await.unwrap(), crate::runtime::RuntimeBudgetNativeClaimPreparation::Refused) {
                                    return Err("duplicate external preparation borrowed the original claim".to_owned());
                                }
                                if mode == CompletionCase::ExternalClaimDrop {
                                    drop(claim);
                                    None
                                } else if matches!(mode, CompletionCase::ExternalPause | CompletionCase::ExternalStop) {
                                    if mode == CompletionCase::ExternalPause {
                                        waiting::browser_input(&handle, &primary, b"/list-agents --all\r").await;
                                        waiting::browser_input(&handle, &primary, b"p").await;
                                        waiting::assert_browser_paused(&handle, "%1", true).await;
                                    } else { handle.execute_agent_shell_command(primary.clone(), "/stop".into()).await.unwrap(); }
                                    if handle.finish_budget_external_claim(claim).await.unwrap().is_some() || external_count.load(Ordering::SeqCst) != 0 {
                                        return Err("paused/stopped original external claim revived".to_owned());
                                    }
                                    if mode == CompletionCase::ExternalPause {
                                        waiting::browser_input(&handle, &primary, b"p").await;
                                        waiting::assert_browser_paused(&handle, "%1", false).await;
                                        if handle.claim_approved_external_action(turn.clone(), action.clone()).await.unwrap().is_some() {
                                            return Err("Resume revived a superseded approval allocation".to_owned());
                                        }
                                    }
                                    return Ok(published);
                                } else {
                                    if mode == CompletionCase::ExternalBeforeRecross {
                                        super::remember_retirement::publish_policy(&store, "post-external-claim", 'e', BudgetLimits {
                                            input: TokenLimit::Limited(0), output: TokenLimit::Limited(0),
                                        }).await;
                                    }
                                    Some(handle.finish_budget_external_claim(claim).await.unwrap()
                                        .ok_or_else(|| "writer-claimed original external offer did not transfer".to_owned())?)
                                }
                            }
                        } else { None };
                        let mut dispatch = if let Some(dispatch) = preclaimed { dispatch } else {
                            handle.claim_approved_external_action(turn.clone(), action.clone()).await.unwrap()
                                .ok_or_else(|| "original external offer did not recover".to_owned())?
                        };
                        dispatch.accepted_result_for_tests = Some(accepted_external_result.clone());
                        spawn_owned_agent_provider_worker(&mut native_workers, &mut native_owners, None,
                            execute_approved_external_action(handle.clone(), dispatch));
                        let result = tokio::time::timeout(Duration::from_secs(8), native_workers.join_next_with_id()).await
                            .map_err(|_| "original external monitor did not finish".to_owned())?.unwrap();
                        record_owned_agent_provider_worker(result, &handle, &mut report, &mut native_owners, &drains).await.map_err(|error| error.to_string())?;
                        if !accepted_external_result.lock().unwrap().as_ref().is_some_and(|(owner, id, status)| owner == &turn && id == action && *status == mez_agent::ActionStatus::Succeeded) {
                            return Err("original external result was not successfully actor-accepted".to_owned());
                        }
                        if handle.claim_approved_external_action(turn.clone(), action.clone()).await.unwrap().is_some() {
                            return Err("external offer was claimable twice".to_owned());
                        }
                        settle_external_fixture_bookkeeping(&handle).await.map_err(|error| error.to_string())?;
                        return Ok(published);
                    }
                    if mode == CompletionCase::NativeClaimRegression {
                        if !handle.reconcile_budget_activity(&turn).await.unwrap() {
                            return Err("original root observation was not adopted".to_owned());
                        }
                        let original = store.budget_account(&member.root).unwrap();
                        let mut regressed = original.clone();
                        regressed.known.input = 0;
                        let writer = rusqlite::Connection::open(store.path()).unwrap();
                        writer.execute("UPDATE budget_roots SET body=?1 WHERE id=?2", rusqlite::params![serde_json::to_string(&regressed).unwrap(), member.root.as_str()]).unwrap();
                        let refused = handle.claim_native_shell_action(turn.clone(), action.clone()).await.unwrap().is_none();
                        writer.execute("UPDATE budget_roots SET body=?1 WHERE id=?2", rusqlite::params![serde_json::to_string(&original).unwrap(), member.root.as_str()]).unwrap();
                        drop(writer);
                        if !refused { return Err("native claim bypassed the observed accounting-integrity fence".to_owned()); }
                        if counter.exists() { return Err("integrity-refused native offer executed".to_owned()); }
                    }
                    if mode == CompletionCase::ClaimWriterContention {
                        let writer = rusqlite::Connection::open(store.path()).unwrap();
                        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
                        let refused = handle.claim_native_shell_action(turn.clone(), action.clone()).await.unwrap().is_none();
                        writer.execute_batch("ROLLBACK").unwrap();
                        if !refused { return Err("native claim bypassed the original accounting/policy writer".to_owned()); }
                        if counter.exists() { return Err("writer-held native offer executed".to_owned()); }
                    }
                    if matches!(mode, CompletionCase::CaptureRefusal | CompletionCase::ReaderPanic) {
                        handle.inject_budget_activity_fault(turn.clone(), mode == CompletionCase::ReaderPanic).await.unwrap();
                        if handle.claim_native_shell_action(turn.clone(), action.clone()).await.unwrap().is_some() {
                            return Err("unavailable budget qualification granted native action".to_owned());
                        }
                        if counter.exists() { return Err("refused original offer executed".to_owned()); }
                        if !handle.agent_turn_is_running(&turn).await.unwrap() {
                            return Err("local budget qualification terminalized original owner".to_owned());
                        }
                    }
                    if mode == CompletionCase::ClaimRecross {
                        super::remember_retirement::publish_policy(&store, "claim-recross", 'e', BudgetLimits {
                            input: TokenLimit::Limited(11), output: TokenLimit::Limited(3),
                        }).await;
                        if handle.claim_native_shell_action(turn.clone(), action.clone()).await.unwrap().is_some() {
                            return Err("prepared native action claimed after budget recross".to_owned());
                        }
                        if counter.exists() { return Err("unclaimed native candidate executed".to_owned()); }
                        super::remember_retirement::publish_policy(&store, "claim-insufficient", 'f', BudgetLimits {
                            input: TokenLimit::Unlimited, output: TokenLimit::Limited(3),
                        }).await;
                        if handle.claim_native_shell_action(turn.clone(), action.clone()).await.unwrap().is_some() {
                            return Err("one exhausted dimension did not retain native candidate".to_owned());
                        }
                        super::remember_retirement::publish_policy(&store, "claim-sufficient", 'a', BudgetLimits::default()).await;
                    }
                    if mode == CompletionCase::NativeCaptureDrop {
                        let crate::runtime::RuntimeBudgetNativeClaimPreparation::Work(work) = handle.prepare_budget_native_shell_claim(turn.clone(), action.clone()).await.unwrap() else {
                            return Err("original offer did not capture native claim work".to_owned());
                        };
                        drop(work);
                    }
                    let preclaimed = if matches!(mode, CompletionCase::NativeClaimBeforeRecross | CompletionCase::NativeClaimDrop | CompletionCase::NativeClaimPause | CompletionCase::NativeClaimStop | CompletionCase::NativeReadObserverDrop) {
                        let crate::runtime::RuntimeBudgetNativeClaimPreparation::Work(mut work) = handle.prepare_budget_native_shell_claim(turn.clone(), action.clone()).await.unwrap() else {
                            return Err("original offer did not prepare a native claim".to_owned());
                        };
                        let claim = if mode == CompletionCase::NativeReadObserverDrop {
                            let (entered, release) = work.gate_native_writer_for_tests();
                            let mut writer = tokio::task::spawn_blocking(move || work.execute());
                            tokio::time::timeout(Duration::from_secs(5), entered.notified()).await.unwrap();
                            let mut borrowed = Box::pin(async { (&mut writer).await });
                            let pending = borrowed.as_mut().now_or_never().is_none();
                            drop(borrowed);
                            let coalesced = matches!(handle.prepare_budget_native_shell_claim(turn.clone(), action.clone()).await.unwrap(), crate::runtime::RuntimeBudgetNativeClaimPreparation::Refused);
                            release.send(()).unwrap();
                            let claim = writer.await.unwrap();
                            if !pending || !coalesced { return Err("borrowed observer cancellation released a live original writer".to_owned()); }
                            claim
                        } else { tokio::task::spawn_blocking(move || work.execute()).await.unwrap() };
                        if !matches!(handle.prepare_budget_native_shell_claim(turn.clone(), action.clone()).await.unwrap(), crate::runtime::RuntimeBudgetNativeClaimPreparation::Refused) {
                            return Err("a second query borrowed the original native claim".to_owned());
                        }
                        if mode == CompletionCase::NativeClaimDrop {
                            drop(claim);
                            None
                        } else if mode == CompletionCase::NativeClaimStop {
                            handle.execute_agent_shell_command(primary.clone(), "/stop".into()).await.unwrap();
                            if handle.finish_budget_native_shell_claim(claim).await.unwrap().is_some() || counter.exists() {
                                return Err("native budget claim revived a stopped owner".to_owned());
                            }
                            return Ok(true);
                        } else if mode == CompletionCase::NativeClaimPause {
                            waiting::browser_input(&handle, &primary, b"/list-agents --all\r").await;
                            waiting::browser_input(&handle, &primary, b"p").await;
                            waiting::assert_browser_paused(&handle, "%1", true).await;
                            if handle.finish_budget_native_shell_claim(claim).await.unwrap().is_some() || counter.exists() {
                                return Err("native budget claim bypassed human pause".to_owned());
                            }
                            waiting::browser_input(&handle, &primary, b"p").await;
                            waiting::assert_browser_paused(&handle, "%1", false).await;
                            // Explicit human Resume intentionally supersedes
                            // unissued candidates; it must not revive this offer.
                            if handle.claim_native_shell_action(turn.clone(), action.clone()).await.unwrap().is_some() || counter.exists() {
                                return Err("human Resume revived a superseded native offer".to_owned());
                            }
                            return Ok(true);
                        } else {
                            super::remember_retirement::publish_policy(&store, "post-native-claim", 'e', BudgetLimits {
                                input: TokenLimit::Limited(0), output: TokenLimit::Limited(0),
                            }).await;
                            Some(handle.finish_budget_native_shell_claim(claim).await.unwrap()
                                .ok_or_else(|| "later budget edit revoked an already native-claimed effect".to_owned())?)
                        }
                    } else { None };
                    let native = if let Some(native) = preclaimed { native } else {
                        handle.claim_native_shell_action(turn.clone(), action.clone()).await.unwrap()
                            .ok_or_else(|| "original native offer did not claim".to_owned())?
                    };
                    if mode == CompletionCase::MonitorError {
                        let owner = NativeWorkerOwner::new(&native);
                        fault_owner = Some(owner.clone());
                        let monitor_owner = owner.clone();
                        let finished = native_finished.clone();
                        spawn_owned_native_agent_worker(&mut native_workers, &mut native_owners, owner, async move {
                            let (progress, _) = tokio::sync::watch::channel(None);
                            let (entered, started) = tokio::sync::oneshot::channel();
                            monitor_owner.start(native, progress, move |dispatch, progress| {
                                let _ = entered.send(());
                                // A real retained blocking writer remains live until
                                // exact owner retirement cancels this genuine dispatch.
                                let deadline = std::time::Instant::now() + Duration::from_secs(8);
                                let cancelled = dispatch.cancellation.flag();
                                while !cancelled.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
                                    std::thread::sleep(Duration::from_millis(1));
                                }
                                let result = crate::runtime::execute_native_shell_dispatch_with_progress(dispatch, progress);
                                finished.store(true, Ordering::SeqCst);
                                result
                            }).await?;
                            // Retiring a still-queued blocking task is valid but
                            // does not exercise this running-closure cleanup cut.
                            tokio::time::timeout(Duration::from_secs(5), started).await
                                .map_err(|_| MezError::invalid_state("native fixture closure entry timed out"))?
                                .map_err(|_| MezError::invalid_state("native fixture closure entry lost"))?;
                            Err(MezError::invalid_state("injected completion native monitor error"))
                        });
                    } else {
                        spawn_native_shell_action_worker(&mut native_workers, &mut native_owners, handle.clone(), native,
                            crate::runtime::execute_native_shell_dispatch_with_progress);
                    }
                    let result = tokio::time::timeout(Duration::from_secs(8), native_workers.join_next_with_id()).await
                        .map_err(|_| "original native monitor did not finish".to_owned())?.unwrap();
                    let recorded = record_owned_agent_provider_worker(result, &handle, &mut report, &mut native_owners, &drains).await;
                    if mode == CompletionCase::MonitorError
                        && (native_owners.is_empty() || native_finished.load(Ordering::SeqCst)) {
                        return Err("fault did not retain outstanding native custody".to_owned());
                    }
                    recorded.map_err(|error| error.to_string())?;
                    if handle.claim_native_shell_action(turn.clone(), action.clone()).await.unwrap().is_some() {
                        return Err("native candidate was claimable twice".to_owned());
                    }
                }
                let mut duplicate = RuntimeEventBatch::new();
                duplicate.push(event);
                if handle.submit_runtime_events(duplicate).await.unwrap().applied != 0 {
                    return Err("duplicate Completed reapplied response".to_owned());
                }
            }
            Ok(published)
        }).catch_unwind().await;
        // Own and join the actual monitor before collecting producer drains.
        if !joined {
            if !worker.is_finished() {
                worker.abort();
            }
            let _ = worker.await;
        }
        // Join the actual native monitor (including on exercise failure), then
        // settle its retained worker outcome before snapshotting request drains.
        // A consumed error join can leave its actual producer in the owner
        // map. The full retirement boundary visits those retained entries too.
        let mut failures = ProviderCleanupFailures::default();
        let mut native_cleanup_error = retire_agent_provider_workers(
            &mut native_workers,
            &handle,
            &drains,
            &mut native_owners,
            &mut failures,
        )
        .await
        .err();
        let retired_without_fallback = native_owners.is_empty()
            && (fault_owner.is_none() || native_finished.load(Ordering::SeqCst));
        drop(native_owners);
        // Safety-only fallback makes the RED assertion cleanup-safe. It is
        // deliberately excluded from the primary retirement oracle above.
        if !retired_without_fallback {
            let owner = fault_owner.as_ref().unwrap();
            let result = owner.retire("completion fixture safety retirement").await;
            match result {
                Ok(outcome) => {
                    let mut batch = RuntimeEventBatch::new();
                    batch.push(RuntimeEvent::NativeShell(outcome));
                    if let Err(error) = handle.submit_runtime_events(batch).await {
                        native_cleanup_error.get_or_insert(error);
                    }
                }
                Err(error) => {
                    native_cleanup_error.get_or_insert(error);
                }
            }
        }
        drop(fault_owner);
        server.abort();
        let server_result = server.await;
        if let Some(server) = external_server {
            server.abort();
            let _ = server.await;
        }
        let cleanup = async {
            let pending: Vec<_> = drains.lock().unwrap().values().cloned().collect();
            for drain in &pending {
                drain.abort.abort();
                drain.completed.observe().await;
            }
            for drain in &pending {
                flush_pending_provider_observations(&handle, &drain.pending).await?;
            }
            handle
                .execute_agent_shell_command(primary, "/stop".into())
                .await?;
            if external {
                settle_external_fixture_bookkeeping(&handle).await?;
            }
            handle.shutdown().await
        }
        .await;
        (
            exercise,
            server_result,
            cleanup.and(native_cleanup_error.map_or(Ok(()), Err)),
            retired_without_fallback,
        )
    };
    let ((exercise, server_result, cleanup, retired_without_fallback), mut exit) =
        tokio::join!(control, actor.run());
    exit.service.terminate_all_pane_processes().unwrap();
    let known = store.budget_account(&member.root).unwrap().known;
    let effects = std::fs::read(&counter).unwrap_or_default();
    let outstanding: i64 = rusqlite::Connection::open(store.path()).unwrap().query_row(
        "SELECT COUNT(*) FROM (SELECT id FROM budget_attempts WHERE terminal=0 UNION SELECT attempt FROM budget_admissions WHERE retired_receipt IS NULL)", [], |row| row.get(0),
    ).unwrap();
    drop((exit, handle, drains));
    root.cleanup();
    cleanup.unwrap();
    assert!(server_result.unwrap_err().is_cancelled());
    let published = match exercise {
        Ok(Err(error)) if mode == CompletionCase::MonitorError => {
            assert!(
                error.contains("injected completion native monitor error"),
                "original monitor cause lost: {error}"
            );
            assert!(
                native_finished.load(Ordering::SeqCst),
                "safety retirement did not destroy writer"
            );
            assert!(
                retired_without_fallback,
                "primary cleanup dropped retained native writer"
            );
            true
        }
        Ok(result) => result.unwrap(),
        Err(panic) => std::panic::resume_unwind(panic),
    };
    assert_eq!(published, !stop);
    assert_eq!(
        sends.load(Ordering::SeqCst),
        1,
        "hold/release must not repeat inference"
    );
    assert_eq!(
        known,
        UsageTotals {
            input: 11,
            output: 3
        }
    );
    assert_eq!(outstanding, 0);
    if external {
        assert_eq!(
            external_count.load(Ordering::SeqCst),
            usize::from(!matches!(
                mode,
                CompletionCase::ExternalPause | CompletionCase::ExternalStop
            )),
            "only the original external HTTP effect may execute"
        );
    }
    assert_eq!(
        effects,
        if adopt
            && !external
            && !matches!(
                mode,
                CompletionCase::StopBeforeAdoption
                    | CompletionCase::MonitorError
                    | CompletionCase::NativeClaimPause
                    | CompletionCase::NativeClaimStop
            )
        {
            b"x".as_slice()
        } else {
            b"".as_slice()
        },
        "only the once-adopted original candidate may execute"
    );
}
