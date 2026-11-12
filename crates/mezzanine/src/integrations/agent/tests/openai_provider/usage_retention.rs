//! Incurred Responses expense survives fallible MAAP validation after decoding.

use super::*;

fn request() -> ModelRequest {
    assemble_model_request(
        &ModelProfile {
            provider: "openai".into(),
            model: "gpt-test".into(),
            model_capabilities: Default::default(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: Default::default(),
            safety_tier: None,
        },
        &turn(),
        &AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "fixture".into(),
            content: "validate output".into(),
        }])
        .unwrap(),
    )
    .unwrap()
}

fn response(stream: bool) -> ProviderHttpResponse {
    let decoded = serde_json::json!({
        "model":"gpt-test", "status":"completed",
        "output_text": serde_json::json!({
            "rationale":"fixture", "actions":[{"command":"ls"}]
        }).to_string(),
        "usage":{"input_tokens":11,"output_tokens":7}
    });
    ProviderHttpResponse {
        status_code: 200,
        headers: std::collections::BTreeMap::from([(
            "content-type".into(),
            if stream {
                "text/event-stream"
            } else {
                "application/json"
            }
            .into(),
        )]),
        body: if stream {
            format!(
                "event: response.output_text.delta\ndata: {}\n\nevent: response.completed\ndata: {}\n\n",
                serde_json::json!({
                    "type":"response.output_text.delta", "delta":decoded["output_text"]
                }),
                serde_json::json!({
                    "type":"response.completed", "response":decoded
                })
            )
        } else {
            decoded.to_string()
        },
    }
}

fn assert_expense(error: &crate::error::MezError) {
    assert!(error.message().contains("bare command"), "{error:?}");
    let usage = error.provider_incurred_usage();
    assert_eq!((usage.input_tokens, usage.output_tokens), (11, 7));
}

/// A rejected action batch is not a free inference. Test the real adapter
/// suffix rather than manufacturing an error with preattached usage.
#[test]
fn responses_maap_rejection_retains_usage_sync() {
    for stream in [false, true] {
        let provider = OpenAiResponsesProvider::with_endpoint_headers_and_stream(
            "fixture-key",
            "https://example.test/responses",
            1_000,
            Default::default(),
            stream,
            FakeProviderHttpTransport {
                requests: RefCell::new(Vec::new()),
                response: response(stream),
            },
        )
        .unwrap();
        assert_expense(&provider.send_request(&request()).unwrap_err());
        assert_eq!(provider.transport.requests.borrow().len(), 1);
    }
}

#[tokio::test]
async fn responses_maap_rejection_retains_usage_async() {
    for stream in [false, true] {
        let provider = OpenAiResponsesProvider::with_endpoint_headers_and_stream(
            "fixture-key",
            "https://example.test/responses",
            1_000,
            Default::default(),
            stream,
            AsyncFakeProviderHttpTransport {
                requests: std::sync::Mutex::new(Vec::new()),
                response: response(stream),
            },
        )
        .unwrap();
        use crate::integrations::agent::provider::{
            ObservedAsyncModelProvider, ProviderRequestPurpose, ProviderWireRequestObserver,
        };
        let (sender, mut receiver) = tokio::sync::mpsc::channel(2);
        let observer = ProviderWireRequestObserver::new("fixture-conversation", "%1", sender);
        let observed = ObservedAsyncModelProvider::new(
            &provider,
            &observer,
            ProviderRequestPurpose::Execution,
        );
        assert_expense(&observed.send_request_async(&request()).await.unwrap_err());
        let observation = receiver.try_recv().unwrap();
        assert!(!observation.succeeded);
        let usage = observation.usage.unwrap();
        assert_eq!((usage.input_tokens, usage.output_tokens), (11, 7));
        assert!(receiver.try_recv().is_err());
        assert_eq!(provider.transport.requests.lock().unwrap().len(), 1);
    }
}

/// The concrete transport returns no response: destruction must transfer the
/// decoded receipt, without claiming either remote completion or durable ACK.
struct InterruptedResponsesTransport {
    usage: Option<(u64, u64)>,
    sends: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ending: StreamEnding,
    positive_prefix: bool,
    error_before_final_usage: bool,
}

#[derive(Clone, Copy)]
enum StreamEnding {
    Pending,
    Panic,
    Return,
}

type InterruptedHttpFuture<'a> = std::pin::Pin<
    Box<
        dyn std::future::Future<Output = mez_agent::ProviderHttpResult<ProviderHttpResponse>>
            + Send
            + 'a,
    >,
>;

impl AsyncProviderHttpTransport for InterruptedResponsesTransport {
    fn send_async<'a>(&'a self, _: &'a ProviderHttpRequest) -> InterruptedHttpFuture<'a> {
        panic!("fixture must use the Responses streaming boundary")
    }

    fn send_async_with_sse_events<'a>(
        &'a self,
        _: &'a ProviderHttpRequest,
        on_event: &'a mut (
                    dyn FnMut(
            mez_agent::SseEvent,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
                        + Send
                ),
    ) -> InterruptedHttpFuture<'a> {
        Box::pin(async move {
            self.sends.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            for (index, (input, output)) in self
                .positive_prefix
                .then_some((11, 7))
                .into_iter()
                .chain(self.usage)
                .enumerate()
            {
                if self.error_before_final_usage && index == usize::from(self.positive_prefix) {
                    on_event(mez_agent::SseEvent {
                        name: None,
                        data: "not-json".into(),
                    })
                    .await;
                }
                on_event(mez_agent::SseEvent {
                    name: None,
                    data: serde_json::json!({
                        "type":"response.in_progress",
                        "response":{"usage":{"input_tokens":input,"output_tokens":output}}
                    })
                    .to_string(),
                })
                .await;
            }
            if self.error_before_final_usage {
                let text = serde_json::json!({
                    "rationale":"late output is not recovery",
                    "actions":[{"type":"say","status":"final","text":"late success"}]
                })
                .to_string();
                on_event(mez_agent::SseEvent {
                    name: None,
                    data: serde_json::json!({
                        "type":"response.output_text.delta", "delta":text
                    })
                    .to_string(),
                })
                .await;
                on_event(mez_agent::SseEvent {
                    name: None,
                    data: serde_json::json!({
                        "type":"response.completed", "response":{
                            "model":"gpt-test", "status":"completed", "output_text":text
                        }
                    })
                    .to_string(),
                })
                .await;
            }
            match self.ending {
                StreamEnding::Pending => std::future::pending().await,
                StreamEnding::Panic => panic!("injected transport panic after decoded usage"),
                StreamEnding::Return => Ok(ProviderHttpResponse {
                    status_code: 200,
                    headers: std::collections::BTreeMap::from([(
                        "content-type".into(),
                        "text/event-stream".into(),
                    )]),
                    body: String::new(),
                }),
            }
        })
    }
}

#[tokio::test]
async fn responses_stream_cancellation_retains_original_expense() {
    use crate::integrations::agent::provider::{
        ObservedAsyncModelProvider, PendingProviderWireObservations, ProviderReportedUsage,
        ProviderRequestPurpose, ProviderWireRequestObserver,
    };
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    for usage in [None, Some((0, 0)), Some((11, 7))] {
        for poll in [false, true] {
            let sends = Arc::new(AtomicUsize::new(0));
            let provider = OpenAiResponsesProvider::with_endpoint_headers_and_stream(
                "fixture-key",
                "https://example.test/responses",
                1_000,
                Default::default(),
                true,
                InterruptedResponsesTransport {
                    usage,
                    sends: sends.clone(),
                    ending: StreamEnding::Pending,
                    positive_prefix: false,
                    error_before_final_usage: false,
                },
            )
            .unwrap();
            let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
            let pending = PendingProviderWireObservations::default();
            let reported = Arc::new(Mutex::new(ProviderReportedUsage::default()));
            let observer = ProviderWireRequestObserver::new("original-cancellation", "%1", sender)
                .with_pending_observations(pending.clone())
                .with_reported_usage(reported.clone());
            let observed = ObservedAsyncModelProvider::new(
                &provider,
                &observer,
                ProviderRequestPurpose::Memory,
            );
            let request = request();
            let mut future = observed.send_request_async(&request);
            if poll {
                std::future::poll_fn(|cx| {
                    assert!(future.as_mut().poll(cx).is_pending());
                    std::task::Poll::Ready(())
                })
                .await;
            }
            drop(future);
            assert_eq!(sends.load(Ordering::SeqCst), usize::from(poll));
            let retained = pending.snapshot();
            let expected = usage.filter(|_| poll);
            assert_eq!(
                retained.len(),
                usize::from(expected.is_some()),
                "{usage:?}/{poll}"
            );
            let reported = *reported.lock().unwrap();
            assert_eq!(reported.known_observations, u64::from(expected.is_some()));
            if let Some((input, output)) = expected {
                let observation = &retained[0];
                assert_eq!(observation.conversation_id, "original-cancellation");
                assert_eq!(observation.turn_id, request.turn_id);
                assert!(!observation.succeeded);
                assert_eq!(observation.failure_kind.as_deref(), Some("interrupted"));
                let receipt = observation.usage.unwrap();
                assert_eq!(
                    (receipt.input_tokens, receipt.output_tokens),
                    (input, output)
                );
                assert_eq!(reported.usage, receipt);
            }
            assert!(receiver.try_recv().is_err());
        }
    }
}

/// Normal retention before an ACK wait must disarm the prefix fallback; a
/// panic before normal retention must instead transfer that prefix once.
#[tokio::test]
async fn responses_stream_retirement_handoff_conserves_one_observation() {
    use crate::integrations::agent::provider::{
        ObservedAsyncModelProvider, PendingProviderWireObservations, ProviderReportedUsage,
        ProviderRequestPurpose, ProviderWireRequestObserver,
    };
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    for ((input, output), ending, closed, saturated, positive_prefix) in [
        ((0, 0), StreamEnding::Return, false, false, false),
        ((0, 0), StreamEnding::Return, true, false, false),
        ((11, 7), StreamEnding::Panic, false, false, false),
        ((11, 7), StreamEnding::Return, false, false, false),
        ((11, 7), StreamEnding::Return, true, false, false),
        ((0, 0), StreamEnding::Return, false, true, false),
        ((11, 7), StreamEnding::Return, false, true, false),
        ((0, 0), StreamEnding::Return, false, false, true),
        ((0, 0), StreamEnding::Return, true, false, true),
        ((0, 0), StreamEnding::Panic, false, false, true),
    ] {
        let sends = Arc::new(AtomicUsize::new(0));
        let provider = OpenAiResponsesProvider::with_endpoint_headers_and_stream(
            "fixture-key",
            "https://example.test/responses",
            1_000,
            Default::default(),
            true,
            InterruptedResponsesTransport {
                usage: Some((input, output)),
                sends: sends.clone(),
                ending,
                positive_prefix,
                error_before_final_usage: false,
            },
        )
        .unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        if closed {
            receiver.close();
        }
        let pending = PendingProviderWireObservations::default();
        let reported = Arc::new(Mutex::new(ProviderReportedUsage::default()));
        let observer = ProviderWireRequestObserver::new("handoff-original", "%1", sender)
            .with_claim_generation(7)
            .with_pending_observations(pending.clone())
            .with_reported_usage(reported.clone());
        let observed = ObservedAsyncModelProvider::new(
            &provider,
            &observer,
            ProviderRequestPurpose::Compaction,
        );
        let request = request();
        // Fill the actual capacity-one channel with a distinct completed
        // observation. Settle only its journal entry, never drain its channel
        // offer: the next call must retain before blocking on delivery itself.
        if saturated {
            let first = ObservedAsyncModelProvider::new(
                &provider,
                &observer,
                ProviderRequestPurpose::Execution,
            );
            assert!(first.send_request_async(&request).await.is_err());
            let first = pending.snapshot();
            assert_eq!(first.len(), 1);
            pending.acknowledge(&first[0].request_id);
        }
        let mut future = observed.send_request_async(&request);
        std::future::poll_fn(|cx| {
            let polled =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| future.as_mut().poll(cx)));
            match ending {
                StreamEnding::Panic => assert!(polled.is_err()),
                StreamEnding::Return if closed => {
                    assert!(matches!(polled, Ok(std::task::Poll::Ready(Err(_)))))
                }
                StreamEnding::Return => assert!(matches!(polled, Ok(std::task::Poll::Pending))),
                StreamEnding::Pending => unreachable!(),
            }
            std::task::Poll::Ready(())
        })
        .await;
        drop(future);
        assert_eq!(sends.load(Ordering::SeqCst), 1 + usize::from(saturated));
        let retained = pending.snapshot();
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].conversation_id, "handoff-original");
        assert!(!retained[0].succeeded);
        let usage = retained[0].usage.unwrap();
        assert_eq!((usage.input_tokens, usage.output_tokens), (input, output));
        let reported = *reported.lock().unwrap();
        assert_eq!(reported.known_observations, 1 + u64::from(saturated));
        let mut total = usage;
        if saturated {
            total.add_assign(usage);
        }
        assert_eq!(reported.usage, total);
        if matches!(ending, StreamEnding::Panic) || closed {
            assert!(receiver.try_recv().is_err());
        } else if saturated {
            assert_ne!(
                receiver.try_recv().unwrap().request_id,
                retained[0].request_id
            );
            assert!(receiver.try_recv().is_err());
        } else {
            assert_eq!(receiver.try_recv().unwrap(), retained[0]);
            assert!(receiver.try_recv().is_err());
        }
    }
}

/// Already received expense remains attributable after output failed. A later
/// valid completion must neither erase the first error nor authorize success.
#[tokio::test]
async fn responses_stream_late_usage_after_error_is_retained_once() {
    use crate::integrations::agent::provider::{
        ObservedAsyncModelProvider, PendingProviderWireObservations, ProviderReportedUsage,
        ProviderRequestPurpose, ProviderWireRequestObserver,
    };
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    for (input, output) in [(13, 9), (0, 0)] {
        for cancel in [false, true] {
            let sends = Arc::new(AtomicUsize::new(0));
            let provider = OpenAiResponsesProvider::with_endpoint_headers_and_stream(
                "fixture-key",
                "https://example.test/responses",
                1_000,
                Default::default(),
                true,
                InterruptedResponsesTransport {
                    usage: Some((input, output)),
                    sends: sends.clone(),
                    ending: if cancel {
                        StreamEnding::Pending
                    } else {
                        StreamEnding::Return
                    },
                    positive_prefix: true,
                    error_before_final_usage: true,
                },
            )
            .unwrap();
            let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
            let pending = PendingProviderWireObservations::default();
            let reported = Arc::new(Mutex::new(ProviderReportedUsage::default()));
            let observer = ProviderWireRequestObserver::new("late-original", "%1", sender)
                .with_pending_observations(pending.clone())
                .with_reported_usage(reported.clone());
            let observed = ObservedAsyncModelProvider::new(
                &provider,
                &observer,
                ProviderRequestPurpose::Memory,
            );
            let request = request();
            if cancel {
                let mut future = observed.send_request_async(&request);
                std::future::poll_fn(|cx| {
                    assert!(future.as_mut().poll(cx).is_pending());
                    std::task::Poll::Ready(())
                })
                .await;
                drop(future);
                assert!(receiver.try_recv().is_err());
            } else {
                let error = observed.send_request_async(&request).await.unwrap_err();
                assert!(error.message().contains("not JSON"), "{error:?}");
                let usage = error.provider_incurred_usage();
                assert_eq!((usage.input_tokens, usage.output_tokens), (input, output));
                let observation = receiver.try_recv().unwrap();
                assert_eq!(observation, pending.snapshot()[0]);
                assert!(receiver.try_recv().is_err());
            }
            assert_eq!(sends.load(Ordering::SeqCst), 1);
            let retained = pending.snapshot();
            assert_eq!(retained.len(), 1);
            assert_eq!(retained[0].conversation_id, "late-original");
            assert!(!retained[0].succeeded);
            let usage = retained[0].usage.unwrap();
            assert_eq!((usage.input_tokens, usage.output_tokens), (input, output));
            let reported = *reported.lock().unwrap();
            assert_eq!(reported.known_observations, 1);
            assert_eq!(reported.usage, usage);
        }
    }
}
