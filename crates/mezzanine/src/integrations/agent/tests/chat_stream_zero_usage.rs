//! Explicit cache Option fields are existing receipt evidence even at zero.
//! An authoritative concrete absence must not erase that evidence on success
//! or future destruction; genuinely absent usage must remain absent.

use super::*;
use crate::integrations::agent::provider::{
    AnthropicMessagesProvider, DeepSeekChatCompletionsProvider, ObservedAsyncModelProvider,
    PendingProviderWireObservations, ProviderReportedUsage, ProviderRequestPurpose,
    ProviderWireRequestObserver,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::{future::Future, pin::Pin};

struct ZeroCacheTransport {
    family: &'static str,
    write: bool,
    absent: bool,
    bare_zero: Option<mez_agent::ModelTokenUsagePresence>,
    cancel: bool,
    sends: Arc<AtomicUsize>,
}

impl AsyncProviderHttpTransport for ZeroCacheTransport {
    fn send_async<'a>(
        &'a self,
        _: &'a ProviderHttpRequest,
    ) -> Pin<
        Box<dyn Future<Output = mez_agent::ProviderHttpResult<ProviderHttpResponse>> + Send + 'a>,
    > {
        panic!("fixture requires the concrete SSE boundary")
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
            let text = serde_json::json!({"rationale":"fixture", "actions":[{
                "type":"say", "status":"final", "content_type":"text/plain; charset=utf-8", "text":"done"
            }]}).to_string();
            let usage = if self.absent {
                serde_json::Value::Null
            } else if let Some(presence) = self.bare_zero {
                let mut usage = serde_json::json!({});
                if presence.input {
                    usage[if self.family == "anthropic" {
                        "input_tokens"
                    } else {
                        "prompt_tokens"
                    }] = 0.into();
                }
                if presence.output {
                    usage[if self.family == "anthropic" {
                        "output_tokens"
                    } else {
                        "completion_tokens"
                    }] = 0.into();
                }
                usage
            } else if self.family == "anthropic" {
                if self.write {
                    serde_json::json!({"input_tokens":0,"output_tokens":0,"cache_creation_input_tokens":0})
                } else {
                    serde_json::json!({"input_tokens":0,"output_tokens":0,"cache_read_input_tokens":0})
                }
            } else if self.family == "deepseek" {
                serde_json::json!({"prompt_tokens":0,"completion_tokens":0,"prompt_cache_hit_tokens":0})
            } else {
                serde_json::json!({"prompt_tokens":0,"completion_tokens":0,"prompt_tokens_details":{"cached_tokens":0}})
            };
            let events = if self.family == "anthropic" {
                vec![
                    serde_json::json!({"type":"message_start","message":{"model":"fixture","usage":usage}}).to_string(),
                    serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":text}}).to_string(),
                    serde_json::json!({"type":"content_block_stop","index":0}).to_string(),
                    serde_json::json!({"type":"message_delta","delta":{"stop_reason":"tool_use"}}).to_string(),
                    serde_json::json!({"type":"message_stop"}).to_string(),
                ]
            } else {
                vec![
                    serde_json::json!({"model":"fixture","choices":[{
                    "index":0,"delta":{"content":text},"finish_reason":"stop"
                }],"usage":usage})
                    .to_string(),
                    "[DONE]".into(),
                ]
            };
            for (index, data) in events.into_iter().enumerate() {
                on_event(mez_agent::SseEvent { name: None, data }).await;
                if self.cancel && index == 0 {
                    std::future::pending::<()>().await;
                }
            }
            Ok(ProviderHttpResponse {
                status_code: 200,
                headers: Default::default(),
                body: String::new(),
            })
        })
    }
}

#[tokio::test]
async fn shared_chat_stream_zero_cache_receipts_survive_success_and_cancellation() {
    for family in ["openai-compatible", "deepseek", "anthropic"] {
        for (write, absent, bare_zero) in [
            (false, false, None),
            (false, true, None),
            (true, false, None),
            (
                false,
                false,
                Some(mez_agent::ModelTokenUsagePresence {
                    input: true,
                    output: true,
                }),
            ),
            (
                false,
                false,
                Some(mez_agent::ModelTokenUsagePresence {
                    input: true,
                    output: false,
                }),
            ),
            (
                false,
                false,
                Some(mez_agent::ModelTokenUsagePresence {
                    input: false,
                    output: true,
                }),
            ),
        ] {
            if write && family != "anthropic" {
                continue;
            }
            for cancel in [false, true] {
                let sends = Arc::new(AtomicUsize::new(0));
                let transport = ZeroCacheTransport {
                    family,
                    write,
                    absent,
                    bare_zero,
                    cancel,
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
                        content: "inspect zero cache evidence".into(),
                    }])
                    .unwrap(),
                )
                .unwrap();
                match family {
                    "openai-compatible" => {
                        let root = std::env::temp_dir().join(format!(
                            "mez-zero-cache-{}",
                            crate::storage::token_usage::new_token_usage_event_id()
                        ));
                        assert!(!root.exists());
                        let auth = AuthStore::new(
                            crate::security::auth::AuthPaths::under_config_root(&root),
                        );
                        let options = std::collections::BTreeMap::from([(
                            "streaming".into(),
                            "enabled".into(),
                        )]);
                        let provider =
                            openai_compatible_provider_from_auth_store_with_provider_options(
                                &auth, family, None, &options, 1000, transport,
                            )
                            .unwrap();
                        assert_zero_receipt(
                            provider, &request, write, absent, bare_zero, cancel, sends,
                        )
                        .await;
                        assert!(!root.exists());
                    }
                    "deepseek" => {
                        assert_zero_receipt(
                            DeepSeekChatCompletionsProvider::without_auth(transport)
                                .unwrap()
                                .with_stream(true),
                            &request,
                            write,
                            absent,
                            bare_zero,
                            cancel,
                            sends,
                        )
                        .await
                    }
                    "anthropic" => {
                        assert_zero_receipt(
                            AnthropicMessagesProvider::new("fixture-key", transport)
                                .unwrap()
                                .with_stream(true),
                            &request,
                            write,
                            absent,
                            bare_zero,
                            cancel,
                            sends,
                        )
                        .await
                    }
                    _ => unreachable!(),
                }
            }
        }
    }
}

async fn assert_zero_receipt<P: AsyncModelProvider>(
    provider: P,
    request: &ModelRequest,
    write: bool,
    absent: bool,
    bare_zero: Option<mez_agent::ModelTokenUsagePresence>,
    cancel: bool,
    sends: Arc<AtomicUsize>,
) {
    let (sender, mut receiver) = tokio::sync::mpsc::channel(2);
    let pending = PendingProviderWireObservations::default();
    let reported = Arc::new(Mutex::new(ProviderReportedUsage::default()));
    let observer = ProviderWireRequestObserver::new("zero-original", "%1", sender)
        .with_pending_observations(pending.clone())
        .with_reported_usage(reported.clone());
    let observed =
        ObservedAsyncModelProvider::new(&provider, &observer, ProviderRequestPurpose::Execution);
    let retained = if cancel {
        let mut future = observed.send_request_async(request);
        std::future::poll_fn(|cx| {
            assert!(future.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        drop(future);
        assert!(receiver.try_recv().is_err());
        pending.snapshot()
    } else {
        let (result, delivered) =
            crate::integrations::agent::provider::observe_and_acknowledge_for_test(
                observed.send_request_async(request),
                &mut receiver,
                &pending,
            )
            .await;
        let response = result.unwrap();
        assert!(response.action_batch.is_some());
        assert!(pending.is_empty());
        assert!(receiver.try_recv().is_err());
        delivered
    };
    if absent && cancel {
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].usage, None);
        assert_eq!(retained[0].failure_kind.as_deref(), Some("interrupted"));
    } else {
        assert_eq!(retained.len(), 1);
        let expected = (!absent).then_some(ModelTokenUsage {
            cached_input_tokens: (!write && bare_zero.is_none()).then_some(0),
            cache_write_input_tokens: (write && bare_zero.is_none()).then_some(0),
            ..Default::default()
        });
        assert_eq!(
            retained[0].usage_presence,
            Some(bare_zero.unwrap_or(mez_agent::ModelTokenUsagePresence {
                input: !absent,
                output: !absent
            }))
        );
        assert_eq!(
            retained[0].usage, expected,
            "{} write={write} cancel={cancel}",
            request.provider
        );
        assert_eq!(retained[0].succeeded, !cancel);
        assert_eq!(retained[0].conversation_id, "zero-original");
    }
    assert_eq!(sends.load(Ordering::SeqCst), 1);
    assert_eq!(
        reported.lock().unwrap().known_observations,
        u64::from(!absent)
    );
}
