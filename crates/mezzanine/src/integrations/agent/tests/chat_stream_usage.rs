//! Decoder expense must outlive transport/status/output failures in every
//! dialect using the shared chat shell. No provider, auth store or process runs.

use super::*;
use crate::integrations::agent::provider::{
    AnthropicMessagesProvider, DeepSeekChatCompletionsProvider, ObservedAsyncModelProvider,
    ProviderRequestPurpose, ProviderWireRequestObserver,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::{future::Future, pin::Pin};

#[derive(Clone, Copy, Debug)]
enum Fault {
    Transport,
    Status,
    Finish,
    MalformedEvent,
    AbsentUsage,
    PartialCounters,
    PartialZero,
    EmptyCounters,
    CancelAfterUsage,
    LateUsage,
    LateUsageCancel,
    LateUsageZero,
}

struct PartialUsageTransport {
    anthropic: bool,
    fault: Fault,
    sends: Arc<AtomicUsize>,
}

impl AsyncProviderHttpTransport for PartialUsageTransport {
    fn send_async<'a>(
        &'a self,
        _: &'a ProviderHttpRequest,
    ) -> Pin<
        Box<dyn Future<Output = mez_agent::ProviderHttpResult<ProviderHttpResponse>> + Send + 'a>,
    > {
        panic!("fixture must reach the concrete streaming boundary")
    }

    fn send_async_with_sse_events<'a>(
        &'a self,
        _: &'a ProviderHttpRequest,
        on_event: &'a mut (
                    dyn FnMut(mez_agent::SseEvent) -> Pin<Box<dyn Future<Output = ()> + Send>>
                        + Send
                ),
    ) -> Pin<
        Box<dyn Future<Output = mez_agent::ProviderHttpResult<ProviderHttpResponse>> + Send + 'a>,
    > {
        Box::pin(async move {
            self.sends.fetch_add(1, Ordering::SeqCst);
            let value = if self.anthropic {
                serde_json::json!({"type":"message_start", "message":{
                    "model":"fixture", "usage":{
                        "input_tokens":3, "cache_read_input_tokens":5,
                        "cache_creation_input_tokens":3, "output_tokens":7
                    }
                }})
            } else {
                serde_json::json!({"model":"fixture", "choices":[],
                    "usage":{"prompt_tokens":11,"completion_tokens":7}})
            };
            if !matches!(self.fault, Fault::AbsentUsage) {
                on_event(mez_agent::SseEvent {
                    name: None,
                    data: value.to_string(),
                })
                .await;
            }
            if matches!(
                self.fault,
                Fault::PartialCounters | Fault::PartialZero | Fault::EmptyCounters
            ) {
                let output = if matches!(self.fault, Fault::PartialZero) {
                    0
                } else {
                    9
                };
                let usage = if matches!(self.fault, Fault::EmptyCounters) {
                    serde_json::json!({})
                } else if self.anthropic {
                    serde_json::json!({"input_tokens":3,"output_tokens":output})
                } else {
                    serde_json::json!({"completion_tokens":output})
                };
                let partial = if self.anthropic {
                    serde_json::json!({"type":"message_delta", "usage":usage})
                } else {
                    serde_json::json!({"choices":[], "usage":usage})
                };
                on_event(mez_agent::SseEvent {
                    name: None,
                    data: partial.to_string(),
                })
                .await;
            }
            let late_usage = matches!(
                self.fault,
                Fault::LateUsage | Fault::LateUsageCancel | Fault::LateUsageZero
            );
            if matches!(self.fault, Fault::MalformedEvent) || late_usage {
                // OpenAI/Anthropic latch a decoder error here; DeepSeek's
                // tolerant decoder instead rejects missing MAAP at finish.
                on_event(mez_agent::SseEvent {
                    name: None,
                    data: if late_usage {
                        "not-json".into()
                    } else {
                        "{\"type\":\"content_block_delta\"}".into()
                    },
                })
                .await;
            }
            if late_usage {
                // These bytes were actually received on the same exchange,
                // after unusable output. They are not a second inference.
                let (input, output) = if matches!(self.fault, Fault::LateUsageZero) {
                    (0, 0)
                } else {
                    (13, 9)
                };
                let value = if self.anthropic {
                    serde_json::json!({"type":"message_delta", "usage":{
                        "input_tokens":input,"output_tokens":output,
                        "cache_read_input_tokens":0,"cache_creation_input_tokens":0
                    }})
                } else {
                    serde_json::json!({"choices":[], "usage":{
                        "prompt_tokens":input,"completion_tokens":output
                    }})
                };
                on_event(mez_agent::SseEvent {
                    name: None,
                    data: value.to_string(),
                })
                .await;
            }
            if matches!(self.fault, Fault::CancelAfterUsage | Fault::LateUsageCancel) {
                std::future::pending::<()>().await;
            }
            if matches!(self.fault, Fault::Transport | Fault::AbsentUsage) {
                return Err(mez_agent::ProviderHttpError::invalid_state(
                    "fixture transport cutoff",
                ));
            }
            Ok(ProviderHttpResponse {
                status_code: if matches!(self.fault, Fault::Status) {
                    503
                } else {
                    200
                },
                headers: std::collections::BTreeMap::from([(
                    "content-type".into(),
                    "text/event-stream".into(),
                )]),
                body: String::new(),
            })
        })
    }
}

#[tokio::test]
async fn shared_chat_stream_failures_preserve_decoded_expense() {
    for family in ["openai-compatible", "deepseek", "anthropic"] {
        for fault in [
            Fault::Transport,
            Fault::Status,
            Fault::Finish,
            Fault::MalformedEvent,
            Fault::AbsentUsage,
            Fault::PartialCounters,
            Fault::PartialZero,
            Fault::EmptyCounters,
            Fault::CancelAfterUsage,
            Fault::LateUsage,
            Fault::LateUsageCancel,
            Fault::LateUsageZero,
        ] {
            let sends = Arc::new(AtomicUsize::new(0));
            let transport = PartialUsageTransport {
                anthropic: family == "anthropic",
                fault,
                sends: sends.clone(),
            };
            let request = assemble_model_request(
                &ModelProfile {
                    provider: family.into(),
                    model: "fixture".into(),
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
                    content: "inspect failure".into(),
                }])
                .unwrap(),
            )
            .unwrap();
            match family {
                "openai-compatible" => {
                    // The factory reads missing auth metadata but creates no
                    // store. Explicit backend capability, not with_stream,
                    // enables this generic dialect's standard SSE mode.
                    let root = std::env::temp_dir().join(format!(
                        "mez-chat-stream-{}",
                        crate::storage::token_usage::new_token_usage_event_id()
                    ));
                    assert!(!root.exists());
                    let auth =
                        AuthStore::new(crate::security::auth::AuthPaths::under_config_root(&root));
                    let options =
                        std::collections::BTreeMap::from([("streaming".into(), "enabled".into())]);
                    let provider =
                        openai_compatible_provider_from_auth_store_with_provider_options(
                            &auth, family, None, &options, 1_000, transport,
                        )
                        .unwrap();
                    assert_expense(provider, &request, family, fault, sends).await;
                    assert!(!root.exists());
                }
                "deepseek" => {
                    assert_expense(
                        DeepSeekChatCompletionsProvider::without_auth(transport)
                            .unwrap()
                            .with_stream(true),
                        &request,
                        family,
                        fault,
                        sends,
                    )
                    .await
                }
                "anthropic" => {
                    assert_expense(
                        AnthropicMessagesProvider::new("fixture-key", transport)
                            .unwrap()
                            .with_stream(true),
                        &request,
                        family,
                        fault,
                        sends,
                    )
                    .await
                }
                _ => unreachable!(),
            }
        }
    }
}

async fn assert_expense<P: AsyncModelProvider>(
    provider: P,
    request: &ModelRequest,
    family: &str,
    fault: Fault,
    sends: Arc<AtomicUsize>,
) {
    let (sender, mut receiver) = tokio::sync::mpsc::channel(2);
    let pending = crate::integrations::agent::provider::PendingProviderWireObservations::default();
    let reported = Arc::new(std::sync::Mutex::new(
        crate::integrations::agent::provider::ProviderReportedUsage::default(),
    ));
    let observer = ProviderWireRequestObserver::new("fixture-original", "%1", sender)
        .with_pending_observations(pending.clone())
        .with_reported_usage(reported.clone());
    let observed =
        ObservedAsyncModelProvider::new(&provider, &observer, ProviderRequestPurpose::Execution);
    if matches!(fault, Fault::CancelAfterUsage | Fault::LateUsageCancel) {
        let mut future = observed.send_request_async(request);
        std::future::poll_fn(|cx| {
            assert!(future.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        assert_eq!(sends.load(Ordering::SeqCst), 1);
        drop(future);
        // Actual future destruction, not a timeout, transfers the decoded
        // prefix to the original retirement journal without a usable result.
        let retained = pending.snapshot();
        assert_eq!(
            retained.len(),
            1,
            "{family}: cancellation lost decoded expense"
        );
        let usage = retained[0].usage.unwrap();
        assert_eq!(
            (usage.input_tokens, usage.output_tokens),
            if matches!(fault, Fault::LateUsageCancel) {
                (13, 9)
            } else {
                (11, 7)
            }
        );
        assert!(!retained[0].succeeded);
        let reported = *reported.lock().unwrap();
        assert_eq!(reported.known_observations, 1);
        assert_eq!(reported.usage, usage);
        assert!(receiver.try_recv().is_err());
        return;
    }
    let error = observed.send_request_async(request).await.unwrap_err();
    let usage = error.provider_incurred_usage();
    assert_eq!(
        (usage.input_tokens, usage.output_tokens),
        if matches!(fault, Fault::AbsentUsage) {
            (0, 0)
        } else if matches!(fault, Fault::LateUsage) {
            (13, 9)
        } else if matches!(fault, Fault::LateUsageZero) {
            (0, 0)
        } else if matches!(fault, Fault::PartialCounters) {
            (11, 9)
        } else if matches!(fault, Fault::PartialZero) {
            (11, 0)
        } else {
            (11, 7)
        },
        "{family}/{fault:?}: {error:?}"
    );
    if matches!(fault, Fault::Transport | Fault::AbsentUsage) {
        assert_eq!(error.message(), "fixture transport cutoff");
    }
    if matches!(fault, Fault::LateUsage | Fault::LateUsageZero) && family != "deepseek" {
        assert!(error.message().contains("JSON"), "{family}: {error:?}");
    }
    let observation = receiver.try_recv().unwrap();
    assert!(!observation.succeeded);
    assert_eq!(
        observation.usage,
        if matches!(fault, Fault::AbsentUsage) {
            None
        } else {
            Some(usage)
        }
    );
    assert!(receiver.try_recv().is_err());
    assert_eq!(sends.load(Ordering::SeqCst), 1);
    assert_eq!(pending.snapshot().len(), 1);
    let reported = *reported.lock().unwrap();
    assert_eq!(
        reported.known_observations,
        u64::from(!matches!(fault, Fault::AbsentUsage))
    );
    assert_eq!(reported.usage, usage);
}
