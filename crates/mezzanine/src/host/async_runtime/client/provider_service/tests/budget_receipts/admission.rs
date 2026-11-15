//! Actual Chat/Responses transport polling behind original native authorization.
//! These tests install the staged factory explicitly; production activation,
//! fair service scheduling and physical exposure release are not asserted.

use super::*;
use crate::host::async_runtime::client::provider_service::budget_admission::BudgetAttemptAdmission;
use crate::host::async_runtime::client::provider_service::budget_admission::BudgetRequestAdmission;
use crate::integrations::agent::provider::OpenAiResponsesProvider;
use crate::integrations::agent::provider::{
    ProviderObservationReceiptFactory, ProviderObservationReceiptSource,
};
use futures_util::FutureExt;
use mez_agent::token_budget::TokenLimit;

/// Retain the original factory for an explicit late checkpoint, without
/// replacing production allocation or receipt/native authorization behavior.
struct CapturedRequestSource {
    inner: Arc<BudgetRequestAdmission>,
    first: std::sync::Mutex<Option<Arc<dyn ProviderObservationReceiptFactory>>>,
}

impl ProviderObservationReceiptSource for CapturedRequestSource {
    fn for_exchange(
        &self,
        observation: &ProviderWireRequestObservation,
    ) -> Result<Arc<dyn ProviderObservationReceiptFactory>> {
        let factory = self.inner.for_exchange(observation)?;
        let mut first = self.first.lock().unwrap();
        if first.is_none() {
            *first = Some(factory.clone());
        }
        Ok(factory)
    }
}

struct GateTransport {
    pending: PendingProviderWireObservations,
    store: TokenUsageStore,
    calls: Arc<AtomicUsize>,
    responses: bool,
}

fn text() -> String {
    serde_json::json!({"rationale":"fixture", "actions":[{
        "type":"say", "status":"final", "content_type":"text/plain; charset=utf-8", "text":"done"
    }]})
    .to_string()
}

impl GateTransport {
    fn entered(&self) {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(rows(&self.store, "budget_admissions"), 1);
        assert_eq!(
            self.pending.len(),
            1,
            "authorization lost pre-send journal custody"
        );
        let original = self.pending.snapshot().pop().unwrap();
        self.pending.acknowledge(&original.request_id);
        assert_eq!(
            self.pending.len(),
            1,
            "planned identity was ACKed as a receipt"
        );
    }
}

impl AsyncProviderHttpTransport for GateTransport {
    fn send_async<'a>(
        &'a self,
        _: &'a mez_agent::ProviderHttpRequest,
    ) -> Pin<
        Box<
            dyn Future<Output = mez_agent::ProviderHttpResult<mez_agent::ProviderHttpResponse>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            self.entered();
            Ok(mez_agent::ProviderHttpResponse {
                status_code: 200,
                headers: Default::default(),
                body: if self.responses {
                    serde_json::json!({"model":"test-model", "status":"completed", "output_text":text(),
                        "usage":{"input_tokens":11,"output_tokens":3}}).to_string()
                } else {
                    serde_json::json!({"model":"test-model", "usage":{"prompt_tokens":11,"completion_tokens":3},
                        "choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":text()}}]}).to_string()
                },
            })
        })
    }

    fn send_async_with_sse_events<'a>(
        &'a self,
        _: &'a mez_agent::ProviderHttpRequest,
        on_event: &'a mut (
                    dyn FnMut(mez_agent::SseEvent) -> Pin<Box<dyn Future<Output = ()> + Send>>
                        + Send
                ),
    ) -> Pin<
        Box<
            dyn Future<Output = mez_agent::ProviderHttpResult<mez_agent::ProviderHttpResponse>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            self.entered();
            assert!(self.responses);
            for value in [
                serde_json::json!({"type":"response.in_progress","response":{"usage":{"input_tokens":11,"output_tokens":3}}}),
                serde_json::json!({"type":"response.output_text.delta","delta":text()}),
                serde_json::json!({"type":"response.completed","response":{"model":"test-model","status":"completed","output_text":text(),"usage":{"input_tokens":11,"output_tokens":3}}}),
            ] {
                on_event(mez_agent::SseEvent {
                    name: None,
                    data: value.to_string(),
                })
                .await;
            }
            Ok(mez_agent::ProviderHttpResponse {
                status_code: 200,
                headers: std::collections::BTreeMap::from([(
                    "content-type".into(),
                    "text/event-stream".into(),
                )]),
                body: String::new(),
            })
        })
    }
}

async fn boundary(
    responses: bool,
    stream: bool,
    held: bool,
    request_scoped: bool,
    identity: Option<(BudgetAttemptPurpose, &str, Option<u64>)>,
) {
    let limits = BudgetLimits {
        input: if held {
            TokenLimit::limited(0).unwrap()
        } else {
            TokenLimit::Unlimited
        },
        output: TokenLimit::Unlimited,
    };
    let Prepared {
        service,
        dispatch,
        store,
        member,
        mut root,
    } = prepared_case(limits).await;
    let pending = PendingProviderWireObservations::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let transport = GateTransport {
        pending: pending.clone(),
        store: store.clone(),
        calls: calls.clone(),
        responses,
    };
    let (chat, responses_provider) = if responses {
        (
            None,
            Some(
                OpenAiResponsesProvider::with_endpoint_headers_and_stream(
                    "fixture-key",
                    "https://example.test/responses",
                    1000,
                    Default::default(),
                    stream,
                    transport,
                )
                .unwrap(),
            ),
        )
    } else {
        (
            Some(
                OpenAiCompatibleChatCompletionsProvider::without_auth(transport)
                    .unwrap()
                    .with_provider_id("local-chat")
                    .unwrap(),
            ),
            None,
        )
    };
    let provider: &dyn AsyncModelProvider = if let Some(provider) = &chat {
        provider
    } else {
        responses_provider.as_ref().unwrap()
    };
    let mut request = crate::integrations::agent::context::assemble_model_request(
        &dispatch.model_profile,
        provider.api_compatibility(),
        &dispatch.turn,
        dispatch.context.durable(),
    )
    .unwrap();
    request.provider = provider.provider_id().into();
    let (attempt_purpose, suffix, generation) =
        identity.unwrap_or((BudgetAttemptPurpose::Execution, "", Some(7)));
    request.turn_id.push_str(suffix);
    let wire_purpose = match attempt_purpose {
        BudgetAttemptPurpose::Routing => ProviderRequestPurpose::Routing,
        BudgetAttemptPurpose::Execution => ProviderRequestPurpose::Execution,
        _ => unreachable!("identity fixture only covers ordinary provider purposes"),
    };
    let work = dispatch
        .budget_owner
        .as_ref()
        .unwrap()
        .prepare_attempt(
            mez_agent::ModelTokenUsageKey::new(&request.provider, &request.model),
            attempt_purpose,
        )
        .unwrap();
    let factory = Arc::new(BudgetAttemptAdmission::new(work.clone()));
    let source = Arc::new(BudgetRequestAdmission::new(
        dispatch.budget_owner.as_ref().unwrap().clone(),
    ));
    let captured = Arc::new(CapturedRequestSource {
        inner: source.clone(),
        first: Default::default(),
    });
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    let observer = ProviderWireRequestObserver::new(&member.conversation, "%1", sender);
    let observer = match generation {
        Some(generation) => observer.with_claim_generation(generation),
        None => observer,
    }
    .with_pending_observations(pending.clone());
    let observer = if request_scoped {
        observer.with_budget_receipt_source(captured.clone())
    } else {
        observer.with_budget_receipts(factory.clone())
    }
    .unwrap();
    let chat_observed = chat
        .as_ref()
        .map(|provider| ObservedAsyncModelProvider::new(provider, &observer, wire_purpose));
    let responses_observed = responses_provider
        .as_ref()
        .map(|provider| ObservedAsyncModelProvider::new(provider, &observer, wire_purpose));
    let observed: &dyn AsyncModelProvider = if let Some(provider) = &chat_observed {
        provider
    } else {
        responses_observed.as_ref().unwrap()
    };
    let (handle, actor) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
            .unwrap();
    let client = async {
        let mut call = Box::pin(tokio::task::unconstrained(
            observed.send_request_async(&request),
        ));
        assert_eq!(
            rows(&store, "budget_admissions"),
            0,
            "fixture preauthorized its exchange"
        );
        if held {
            // Responses offers its failed observation before returning; Chat
            // retains the same unknown evidence synchronously in stack Drop.
            // Both must keep their existing actor-handoff contract.
            let error = tokio::select! {
                result = &mut call => result.unwrap_err(),
                observation = receiver.recv() => {
                    let observation = observation.unwrap();
                    assert_eq!(calls.load(Ordering::SeqCst), 0);
                    assert!(observation.usage.is_none());
                    submit_pending_provider_observation(&handle, &pending, observation).await.unwrap();
                    call.await.unwrap_err()
                }
            };
            assert_eq!(error.kind(), MezErrorKind::BudgetSuspended);
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert_eq!(rows(&store, "budget_admissions"), 0);
            if let Some(unknown) = pending.snapshot().pop() {
                assert!(unknown.usage.is_none());
                pending.checkpoint(&unknown).await.unwrap();
                pending.acknowledge(&unknown.request_id);
            }
            assert!(pending.is_empty());
            assert_eq!(rows(&store, "budget_receipts"), 0);
            assert!(source.completed_receipt().unwrap().is_none());
        } else {
            let observation = tokio::select! {
                biased;
                _ = &mut call => panic!("provider advanced before original durable receipt"),
                observation = receiver.recv() => observation.unwrap(),
            };
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(rows(&store, "budget_receipts"), 0);
            assert!(
                source.completed_receipt().unwrap().is_none(),
                "uncommitted output acquired a completion receipt"
            );
            submit_pending_provider_observation(&handle, &pending, observation.clone())
                .await
                .unwrap();
            let response = call.await.unwrap();
            assert!(response.action_batch.is_some());
            assert_eq!(
                store.budget_account(&member.root).unwrap().known,
                UsageTotals {
                    input: 11,
                    output: 3
                }
            );
            assert_eq!(rows(&store, "budget_receipts"), 2);
            let retirement = if request_scoped {
                source
                    .completed_receipt()
                    .unwrap()
                    .expect("durable original terminal receipt missing")
            } else {
                factory.receipt_stages_for_test(&observation).1.unwrap()
            };
            let settled = retirement.commit_owner();
            let producer = Arc::new(());
            let capture = retirement
                .capture_settled_response(Arc::downgrade(&producer))
                .await
                .unwrap();
            if identity.is_some() {
                let snapshot = settled.observe_settlement().await.unwrap().unwrap();
                // Callback data is synthetic here; its expense/capture comes
                // from the actual issued adapter exchange above. Real router
                // selection and parent handoff are covered in routing.rs.
                let selection = mez_agent::AutoSizingRoutingSelection {
                    selected_profile: dispatch.model_profile.clone(),
                    selected_profile_name: "default".into(),
                    routing_token_usage_by_model: std::collections::BTreeMap::from([(
                        mez_agent::ModelTokenUsageKey::new(
                            &observation.provider,
                            &observation.model,
                        ),
                        observation.usage.unwrap(),
                    )]),
                    decision_summary: None,
                    fallback: None,
                };
                let execution = mez_agent::AgentTurnExecution {
                    request: request.clone(),
                    latest_response_usage: response.usage,
                    response,
                    routing_token_usage_by_model: Default::default(),
                    action_results: Vec::new(),
                    final_turn: true,
                    terminal_state: mez_agent::AgentTurnState::Completed,
                };
                let delivery = capture
                    .clone()
                    .into_provider_settlement(producer.clone(), snapshot.clone(), snapshot)
                    .unwrap();
                let routing = attempt_purpose == BudgetAttemptPurpose::Routing;
                let delivery = if routing {
                    delivery.bind_routing(&selection).unwrap()
                } else {
                    delivery.bind_execution(&execution).unwrap()
                };
                // This uses the genuine issued-terminal capture and original
                // current actor claim, not a constructed inference Held epoch.
                for (agent, event_generation) in [
                    (dispatch.turn.agent_id.as_str(), 7),
                    (dispatch.turn.agent_id.as_str(), 8),
                    ("foreign-agent", 7),
                ] {
                    let wire_matches = agent == dispatch.turn.agent_id
                        && generation == Some(event_generation)
                        && if routing {
                            suffix == ":auto-sizing"
                        } else {
                            suffix.is_empty()
                        };
                    let expected = wire_matches && event_generation == 7;
                    let completion = if routing {
                        crate::runtime::AgentProviderEvent::RoutingSelected {
                            agent_id: AgentId::opaque(agent.to_owned()).unwrap(),
                            turn_id: dispatch.turn.turn_id.clone(),
                            claim_generation: event_generation,
                            selection: Box::new(selection.clone()),
                        }
                    } else {
                        crate::runtime::AgentProviderEvent::Completed {
                            agent_id: AgentId::opaque(agent.to_owned()).unwrap(),
                            turn_id: dispatch.turn.turn_id.clone(),
                            claim_generation: event_generation,
                            execution: Box::new(execution.clone()),
                        }
                    };
                    assert_eq!(delivery.matches_result(&completion), wire_matches);
                    if !expected {
                        let mut batch = crate::runtime::RuntimeEventBatch::new();
                        batch.push(crate::runtime::RuntimeEvent::AgentProvider(
                            crate::runtime::AgentProviderEvent::BudgetSettled {
                                completion: Box::new(completion),
                                settlement: delivery.clone(),
                            },
                        ));
                        let applied = handle.submit_runtime_events(batch).await;
                        if wire_matches {
                            // Genuine stale expense is still original evidence,
                            // but cannot adopt content into the current claim.
                            assert_eq!(applied.unwrap().applied, 0);
                        } else {
                            assert!(applied.is_err(), "changed wire identity reached adoption");
                        }
                    }
                    let parked = handle
                        .park_budget_provider_claim(
                            AgentId::opaque(agent.to_owned()).unwrap(),
                            dispatch.turn.turn_id.clone(),
                            event_generation,
                            capture.clone(),
                        )
                        .await;
                    if expected {
                        assert!(parked.unwrap(), "valid original identity did not park");
                    } else {
                        assert!(parked.is_err(), "invalid original identity did not refuse");
                    }
                }
            }
            assert!(
                handle
                    .park_budget_provider_claim(
                        AgentId::opaque(observation.agent_id.clone()).unwrap(),
                        observation.turn_id.clone(),
                        8,
                        crate::runtime::RuntimeBudgetProviderCapture::from(capture.clone()),
                    )
                    .await
                    .is_err(),
                "a terminal receipt cannot borrow a newer claim generation"
            );
            drop(producer);
            assert!(
                handle
                    .park_budget_provider_claim(
                        AgentId::opaque(observation.agent_id.clone()).unwrap(),
                        observation.turn_id.clone(),
                        7,
                        crate::runtime::RuntimeBudgetProviderCapture::from(capture),
                    )
                    .await
                    .is_err(),
                "expense custody cannot keep a destroyed result runnable"
            );
            if !request_scoped {
                let prefix = factory.receipt_stages_for_test(&observation).0;
                let witness = Arc::new(());
                assert!(
                    prefix
                        .capture_settled_response(Arc::downgrade(&witness))
                        .await
                        .is_err()
                );
            }
            let before = settled.observe_settlement().await.unwrap().unwrap();
            assert_eq!(
                before.known,
                UsageTotals {
                    input: 11,
                    output: 3
                }
            );
            assert_eq!(before.receipt.root, member.root);
            assert_eq!(before.state(), crate::storage::token_usage::budget::receipts::BudgetSettlementState::WithinKnownLimits);
            super::remember_retirement::publish_policy(
                &store,
                "settled-exact-crossing",
                'd',
                BudgetLimits {
                    input: TokenLimit::Limited(11),
                    output: TokenLimit::Unlimited,
                },
            )
            .await;
            let crossed = settled.observe_settlement().await.unwrap().unwrap();
            assert_eq!(crossed.receipt, before.receipt);
            assert!(crossed.revisions.projection > before.revisions.projection);
            assert_eq!(
                crossed.state(),
                crate::storage::token_usage::budget::receipts::BudgetSettlementState::Exhausted {
                    input: true,
                    output: false
                }
            );
            assert_eq!(
                rows(&store, "budget_admissions"),
                1,
                "retirement erased the original replay-only grant"
            );
            let next = Arc::new(BudgetAttemptAdmission::new(work.clone()));
            let mut changed = observation.clone();
            changed.request_id.push_str("-next");
            if request_scoped {
                let original = captured.first.lock().unwrap().as_ref().unwrap().clone();
                assert!(original.plan(&changed).is_err());
                // A new allocation is not a send. A late original checkpoint
                // may settle its expense, but cannot qualify the newer output.
                let newer = source.for_exchange(&changed).unwrap();
                assert!(source.completed_receipt().unwrap().is_none());
                original
                    .freeze(&observation)
                    .unwrap()
                    .checkpoint()
                    .await
                    .unwrap();
                assert!(
                    source.completed_receipt().unwrap().is_none(),
                    "late old checkpoint replaced newer request custody"
                );
                assert_eq!(rows(&store, "budget_receipts"), 2);
                assert_eq!(rows(&store, "budget_admissions"), 1);
                drop(newer);
            } else {
                assert!(next.plan(&changed).is_err());
            }
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
    };
    let control = async {
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            std::panic::AssertUnwindSafe(client).catch_unwind(),
        )
        .await;
        // Dropping the timed-out/panicked adapter does not join its retained
        // native authorization or receipt writer. Reconcile the exact journal
        // before shutdown; failed/planned custody forbids deleting this root.
        let cleanup = tokio::time::timeout(Duration::from_secs(10), async {
            for observation in pending.snapshot() {
                pending.checkpoint(&observation).await?;
                pending.acknowledge(&observation.request_id);
            }
            if !pending.is_empty() {
                return Err(MezError::invalid_state(
                    "original admission journal remains unsettled",
                ));
            }
            Ok(())
        })
        .await;
        tokio::time::timeout(Duration::from_secs(5), handle.shutdown())
            .await
            .expect("admission actor shutdown timed out; fixture retained")
            .unwrap();
        (result, cleanup)
    };
    let ((result, cleanup), mut exit) = tokio::time::timeout(Duration::from_secs(30), async {
        // The bounded outer join must not duplicate these large adapter/actor
        // state machines on the default test thread's poll stack.
        tokio::join!(Box::pin(control), Box::pin(actor.run()))
    })
    .await
    .expect("admission retirement timed out; fixture retained");
    exit.service.terminate_all_pane_processes().unwrap();
    cleanup
        .expect("admission journal retirement timed out; fixture retained")
        .expect("admission journal retirement failed; fixture retained");
    drop((
        exit,
        observer,
        factory,
        source,
        captured,
        work,
        dispatch,
        chat,
        responses_provider,
        handle,
    ));
    root.cleanup();
    if let Err(panic) = result.expect("native admission boundary did not settle") {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn budget_wire_actual_admission_controls_chat_and_responses_physical_polls() {
    for (responses, stream) in [(false, false), (true, false), (true, true)] {
        for held in [false, true] {
            for request_scoped in [false, true] {
                boundary(responses, stream, held, request_scoped, None).await;
            }
        }
    }
}

/// Qualified issued retirement must project only the exact Routing derivation
/// for completion, just as the real native Held matrix does before sending.
#[tokio::test]
async fn budget_wire_settled_routing_requires_exact_original_claim_derivation() {
    for suffix in [
        "",
        ":auto-sizing",
        ":auto-sizing:extra",
        ":auto-sizing:auto-sizing",
    ] {
        boundary(
            false,
            false,
            false,
            true,
            Some((BudgetAttemptPurpose::Routing, suffix, Some(7))),
        )
        .await;
    }
    // Execution is literal even when its suffix looks like a router's. Missing
    // wire generations retain expense, never the current actor's authority.
    for (purpose, suffix, generation) in [
        (BudgetAttemptPurpose::Execution, "", Some(7)),
        (BudgetAttemptPurpose::Execution, ":auto-sizing", Some(7)),
        (BudgetAttemptPurpose::Execution, "", Some(8)),
        (BudgetAttemptPurpose::Routing, ":auto-sizing", Some(8)),
        (BudgetAttemptPurpose::Execution, "", None),
        (BudgetAttemptPurpose::Routing, ":auto-sizing", None),
    ] {
        boundary(
            false,
            false,
            false,
            true,
            Some((purpose, suffix, generation)),
        )
        .await;
    }
}

async fn cancelled_authorization(fault: bool) {
    let Prepared {
        mut service,
        dispatch,
        store,
        member,
        mut root,
    } = prepared_case(BudgetLimits {
        input: TokenLimit::Limited(100),
        output: TokenLimit::Unlimited,
    })
    .await;
    let pending = PendingProviderWireObservations::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = OpenAiCompatibleChatCompletionsProvider::without_auth(GateTransport {
        pending: pending.clone(),
        store: store.clone(),
        calls: calls.clone(),
        responses: false,
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
    let work = dispatch
        .budget_owner
        .as_ref()
        .unwrap()
        .prepare_attempt(
            mez_agent::ModelTokenUsageKey::new(&request.provider, &request.model),
            BudgetAttemptPurpose::Execution,
        )
        .unwrap();
    let factory = Arc::new(BudgetAttemptAdmission::new(work.clone()));
    let (sender, _receiver) = tokio::sync::mpsc::channel(1);
    let observer = ProviderWireRequestObserver::new(&member.conversation, "%1", sender)
        .with_pending_observations(pending.clone())
        .with_budget_receipts(factory.clone())
        .unwrap();
    let observed =
        ObservedAsyncModelProvider::new(&provider, &observer, ProviderRequestPurpose::Execution);
    let connection = rusqlite::Connection::open(store.path()).unwrap();
    if fault {
        connection.execute_batch("CREATE TRIGGER refuse_unused_retirement BEFORE UPDATE OF retired_receipt ON budget_admissions BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    }
    connection.execute_batch("BEGIN IMMEDIATE;").unwrap();
    {
        let mut call = Box::pin(tokio::task::unconstrained(
            observed.send_request_async(&request),
        ));
        assert!(futures_util::poll!(&mut call).is_pending());
        assert_eq!(pending.len(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    let unknown = pending.snapshot().pop().unwrap();
    assert!(unknown.usage.is_none());
    {
        let checkpoint = pending.checkpoint(&unknown);
        tokio::pin!(checkpoint);
        assert!(
            futures_util::poll!(&mut checkpoint).is_pending(),
            "canceled observer abandoned authorization writer"
        );
    }
    connection.execute_batch("ROLLBACK;").unwrap();
    let next = dispatch
        .budget_owner
        .as_ref()
        .unwrap()
        .prepare_attempt(
            mez_agent::ModelTokenUsageKey::new(&request.provider, &request.model),
            BudgetAttemptPurpose::Execution,
        )
        .unwrap()
        .admission_owner();
    if fault {
        assert!(pending.checkpoint(&unknown).await.is_err());
        assert_eq!(rows(&store, "budget_receipts"), 1);
        assert!(
            matches!(next.claim().await.unwrap(), RuntimeBudgetAttemptAdmission::Held(
            crate::storage::token_usage::budget::admission::BudgetAdmissionHold::OutstandingExposure
        ))
        );
        pending.acknowledge(&unknown.request_id);
        assert_eq!(pending.len(), 1, "failed terminal stage must fence ACK");
        let marked: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM budget_admissions WHERE retired_receipt IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(marked, 0);
        let terminal = factory.unused_retirement_for_test(&unknown).unwrap();
        let mut foreign = unknown.clone();
        foreign.conversation_id = "foreign".into();
        assert!(factory.unused_retirement_for_test(&foreign).is_err());
        let mut expense = unknown.clone();
        expense.usage = Some(Default::default());
        assert!(factory.unused_retirement_for_test(&expense).is_err());
        connection
            .execute_batch("DROP TRIGGER refuse_unused_retirement;")
            .unwrap();
        assert!(
            pending.checkpoint(&unknown).await.is_err(),
            "no automatic write retry"
        );
        let owner = terminal.commit_owner();
        assert!(owner.reconcile().await.unwrap().is_none());
        owner.retry_after_absence().await.unwrap();
    }
    pending.checkpoint(&unknown).await.unwrap();
    pending.checkpoint(&unknown).await.unwrap();
    let admissions = rows(&store, "budget_admissions");
    let receipts = rows(&store, "budget_receipts");
    let retired: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM budget_admissions WHERE retired_receipt IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let known = store.budget_account(&member.root).unwrap().known;
    let transport_calls = calls.load(Ordering::SeqCst);
    let next_admission = if fault {
        next.retry_held().await
    } else {
        next.claim().await
    };
    let released_slot = matches!(
        next_admission.unwrap(),
        RuntimeBudgetAttemptAdmission::Authorized(_)
    );
    pending.acknowledge(&unknown.request_id);
    assert!(pending.is_empty());
    service.terminate_all_pane_processes().unwrap();
    drop((connection, observer, factory, work, next, dispatch, service));
    root.cleanup();
    assert_eq!(admissions, 1, "original grant must remain replay evidence");
    assert_eq!(
        receipts, 2,
        "unused original grant needs terminal settlement"
    );
    assert_eq!(
        retired, 1,
        "joined worker and original unused permit qualify release"
    );
    assert_eq!(known, UsageTotals::default());
    assert_eq!(transport_calls, 0);
    assert!(
        released_slot,
        "qualified retirement must free the finite root slot"
    );
}

#[tokio::test]
async fn budget_wire_cancelled_authorization_keeps_original_worker_and_unknown_receipt() {
    cancelled_authorization(false).await;
}

#[tokio::test]
async fn budget_wire_cancelled_authorization_retains_unused_custody_after_terminal_refusal() {
    cancelled_authorization(true).await;
}
