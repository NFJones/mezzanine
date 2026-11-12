//! Unary expense belongs to the physical response, not to accepted output.
//! Exercise the shared transport/observer boundary without provider credentials,
//! processes, sockets, or an on-disk accounting store.

use super::*;
use crate::integrations::agent::provider::{
    AnthropicMessagesProvider, DeepSeekChatCompletionsProvider, ObservedAsyncModelProvider,
    PendingProviderWireObservations, ProviderReportedUsage, ProviderRequestPurpose,
    ProviderWireRequestObserver, observe_and_acknowledge_for_test,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::{future::Future, pin::Pin};

#[derive(Clone, Copy, Debug)]
enum Outcome {
    Transport,
    MissingContent,
    MalformedMaap,
    OutputLimit,
    ProviderError,
    HttpStatus,
    AbsentUsage,
    ZeroUsage,
    PartialUsage,
    PartialInputUsage,
    InvalidUsage,
    InvalidJson,
    MissingMaap,
    Valid,
    ValidAbsentUsage,
    ValidZeroUsage,
    ReasoningAlias,
}

struct UnaryUsageTransport {
    anthropic: bool,
    outcome: Outcome,
    sends: Arc<AtomicUsize>,
}

impl AsyncProviderHttpTransport for UnaryUsageTransport {
    fn send_async<'a>(
        &'a self,
        _: &'a ProviderHttpRequest,
    ) -> Pin<
        Box<dyn Future<Output = mez_agent::ProviderHttpResult<ProviderHttpResponse>> + Send + 'a>,
    > {
        Box::pin(async move {
            self.sends.fetch_add(1, Ordering::SeqCst);
            if matches!(self.outcome, Outcome::Transport) {
                return Err(mez_agent::ProviderHttpError::invalid_state(
                    "fixture transport cutoff",
                ));
            }
            let text = if matches!(
                self.outcome,
                Outcome::Valid | Outcome::ValidAbsentUsage | Outcome::ValidZeroUsage
            ) {
                "{\"rationale\":\"fixture\",\"actions\":[{\"type\":\"say\",\"status\":\"final\",\"content_type\":\"text/plain; charset=utf-8\",\"text\":\"done\"}]}"
            } else if matches!(self.outcome, Outcome::MissingMaap) {
                "plain response without MAAP"
            } else {
                "{not valid MAAP"
            };
            let mut body = if self.anthropic {
                serde_json::json!({"model":"fixture", "content":[{"type":"text","text":text}],
                    "stop_reason":"tool_use", "usage":{"input_tokens":3,
                    "cache_read_input_tokens":5,"cache_creation_input_tokens":3,"output_tokens":7}})
            } else {
                serde_json::json!({"model":"fixture", "choices":[{"index":0,
                    "finish_reason":"stop", "message":{"role":"assistant","content":text}}],
                    "usage":{"prompt_tokens":11,"completion_tokens":7}})
            };
            match self.outcome {
                Outcome::MissingContent => {
                    body.as_object_mut().unwrap().remove(if self.anthropic {
                        "content"
                    } else {
                        "choices"
                    });
                }
                Outcome::ProviderError | Outcome::HttpStatus => {
                    body["error"] =
                        serde_json::json!({"type":"overloaded_error","message":"fixture refusal"});
                }
                Outcome::OutputLimit => {
                    if self.anthropic {
                        body["stop_reason"] = "max_tokens".into();
                    } else {
                        body["choices"][0]["finish_reason"] = "length".into();
                    }
                }
                Outcome::AbsentUsage | Outcome::ValidAbsentUsage => {
                    body.as_object_mut().unwrap().remove("usage");
                }
                Outcome::ZeroUsage | Outcome::ValidZeroUsage => {
                    body["usage"] = if self.anthropic {
                        serde_json::json!({"input_tokens":0,"output_tokens":0})
                    } else {
                        serde_json::json!({"prompt_tokens":0,"completion_tokens":0})
                    };
                }
                Outcome::PartialUsage => {
                    body["usage"] = if self.anthropic {
                        serde_json::json!({"output_tokens":7})
                    } else {
                        serde_json::json!({"completion_tokens":7})
                    };
                }
                Outcome::PartialInputUsage => {
                    body["usage"] = if self.anthropic {
                        serde_json::json!({"input_tokens":11})
                    } else {
                        serde_json::json!({"prompt_tokens":11})
                    };
                }
                Outcome::InvalidUsage => {
                    body["usage"] = serde_json::json!({"input_tokens":-1,
                        "output_tokens":"7", "prompt_tokens":false,
                        "completion_tokens":null, "unknown_counter":11});
                }
                Outcome::ReasoningAlias => {
                    body["usage"]["completion_tokens_details"] =
                        serde_json::json!({"reasoning_tokens":null});
                    body["usage"]["reasoning_tokens"] = 7.into();
                }
                Outcome::MalformedMaap
                | Outcome::Transport
                | Outcome::InvalidJson
                | Outcome::MissingMaap
                | Outcome::Valid => {}
            }
            Ok(ProviderHttpResponse {
                status_code: if matches!(self.outcome, Outcome::HttpStatus) {
                    503
                } else {
                    200
                },
                headers: std::collections::BTreeMap::from([(
                    "content-type".into(),
                    "application/json".into(),
                )]),
                body: if matches!(self.outcome, Outcome::InvalidJson) {
                    "not JSON".into()
                } else {
                    body.to_string()
                },
            })
        })
    }
}

#[tokio::test]
async fn shared_chat_unary_failures_preserve_response_expense() {
    for family in ["openai-compatible", "deepseek", "anthropic"] {
        for outcome in [
            Outcome::MissingContent,
            Outcome::MalformedMaap,
            Outcome::OutputLimit,
            Outcome::ProviderError,
            Outcome::HttpStatus,
            Outcome::AbsentUsage,
            Outcome::ZeroUsage,
            Outcome::PartialUsage,
            Outcome::PartialInputUsage,
            Outcome::InvalidUsage,
            Outcome::InvalidJson,
            Outcome::MissingMaap,
            Outcome::Valid,
            Outcome::ValidAbsentUsage,
            Outcome::ValidZeroUsage,
            Outcome::ReasoningAlias,
        ] {
            let sends = Arc::new(AtomicUsize::new(0));
            let transport = UnaryUsageTransport {
                anthropic: family == "anthropic",
                outcome,
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
                    content: "inspect unary failure".into(),
                }])
                .unwrap(),
            )
            .unwrap();
            match family {
                "openai-compatible" => {
                    let root = std::env::temp_dir().join(format!(
                        "mez-chat-unary-{}",
                        crate::storage::token_usage::new_token_usage_event_id()
                    ));
                    assert!(!root.exists());
                    let auth =
                        AuthStore::new(crate::security::auth::AuthPaths::under_config_root(&root));
                    let provider =
                        openai_compatible_provider_from_auth_store_with_provider_options(
                            &auth,
                            family,
                            None,
                            &Default::default(),
                            1_000,
                            transport,
                        )
                        .unwrap();
                    assert_unary_expense(provider, &request, family, outcome, sends).await;
                    assert!(!root.exists());
                }
                "deepseek" => {
                    assert_unary_expense(
                        DeepSeekChatCompletionsProvider::without_auth(transport).unwrap(),
                        &request,
                        family,
                        outcome,
                        sends,
                    )
                    .await
                }
                "anthropic" => {
                    assert_unary_expense(
                        AnthropicMessagesProvider::new("fixture-key", transport).unwrap(),
                        &request,
                        family,
                        outcome,
                        sends,
                    )
                    .await
                }
                _ => unreachable!(),
            }
        }
    }
}

async fn assert_unary_expense<P: AsyncModelProvider>(
    provider: P,
    request: &ModelRequest,
    family: &str,
    outcome: Outcome,
    sends: Arc<AtomicUsize>,
) {
    let (sender, mut receiver) = tokio::sync::mpsc::channel(2);
    let pending = PendingProviderWireObservations::default();
    let reported = Arc::new(Mutex::new(ProviderReportedUsage::default()));
    let observer = ProviderWireRequestObserver::new("fixture-original", "%1", sender)
        .with_pending_observations(pending.clone())
        .with_reported_usage(reported.clone());
    let observed =
        ObservedAsyncModelProvider::new(&provider, &observer, ProviderRequestPurpose::Execution);
    let (result, mut delivered) = observe_and_acknowledge_for_test(
        observed.send_request_async(request),
        &mut receiver,
        &pending,
    )
    .await;
    assert_eq!(
        result.is_ok(),
        matches!(
            outcome,
            Outcome::Valid | Outcome::ValidAbsentUsage | Outcome::ValidZeroUsage
        ) || (family != "deepseek" && matches!(outcome, Outcome::MissingMaap)),
        "{family}/{outcome:?}: {result:?}"
    );
    let present = !matches!(
        outcome,
        Outcome::AbsentUsage
            | Outcome::ValidAbsentUsage
            | Outcome::InvalidUsage
            | Outcome::InvalidJson
    );
    let expected = if !present || matches!(outcome, Outcome::ZeroUsage | Outcome::ValidZeroUsage) {
        (0, 0)
    } else if matches!(outcome, Outcome::PartialUsage) {
        (0, 7)
    } else if matches!(outcome, Outcome::PartialInputUsage) {
        (11, 0)
    } else {
        (11, 7)
    };
    let usage = match &result {
        Ok(response) => response.usage,
        Err(error) => error.provider_incurred_usage(),
    };
    if matches!(outcome, Outcome::ReasoningAlias) {
        assert_eq!(
            usage.reasoning_tokens,
            if family == "deepseek" { 7 } else { 0 }
        );
    }
    assert_eq!(
        (usage.input_tokens, usage.output_tokens),
        expected,
        "{family}/{outcome:?}: {result:?}"
    );
    assert_eq!(delivered.len(), 1);
    let observation = delivered.pop().unwrap();
    assert_eq!(observation.succeeded, result.is_ok());
    assert_eq!(
        observation.usage_presence,
        Some(mez_agent::ModelTokenUsagePresence {
            input: present && !matches!(outcome, Outcome::PartialUsage),
            output: present && !matches!(outcome, Outcome::PartialInputUsage),
        })
    );
    assert_eq!(
        observation.usage.map(|u| (u.input_tokens, u.output_tokens)),
        present.then_some(expected),
        "{family}/{outcome:?}"
    );
    assert_eq!(observation.conversation_id, "fixture-original");
    assert!(pending.is_empty());
    assert!(receiver.try_recv().is_err());
    assert_eq!(sends.load(Ordering::SeqCst), 1);
    let reported = *reported.lock().unwrap();
    assert_eq!(reported.known_observations, u64::from(present));
    assert_eq!(
        (reported.usage.input_tokens, reported.usage.output_tokens),
        expected
    );
}

/// A streaming request may return a non-SSE JSON error response. No decoder
/// callback is made, but that body's incurred expense must still be retained.
#[tokio::test]
async fn shared_chat_stream_request_json_error_preserves_response_expense() {
    for family in ["deepseek", "anthropic"] {
        let sends = Arc::new(AtomicUsize::new(0));
        let transport = UnaryUsageTransport {
            anthropic: family == "anthropic",
            outcome: Outcome::HttpStatus,
            sends: sends.clone(),
        };
        let request = unary_retry_request(family);
        if family == "deepseek" {
            assert_unary_expense(
                DeepSeekChatCompletionsProvider::without_auth(transport)
                    .unwrap()
                    .with_stream(true),
                &request,
                family,
                Outcome::HttpStatus,
                sends,
            )
            .await;
        } else {
            assert_unary_expense(
                AnthropicMessagesProvider::new("fixture-key", transport)
                    .unwrap()
                    .with_stream(true),
                &request,
                family,
                Outcome::HttpStatus,
                sends,
            )
            .await;
        }
    }
}

fn unary_retry_request(family: &str) -> ModelRequest {
    assemble_model_request(
        &ModelProfile {
            provider: family.into(),
            model: if family == "deepseek" {
                "deepseek-v4-pro".into()
            } else {
                "fixture".into()
            },
            model_capabilities: Default::default(),
            reasoning_profile: Some("high".into()),
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
            content: "inspect retry".into(),
        }])
        .unwrap(),
    )
    .unwrap()
}

/// The actual DeepSeek forced-MAAP exchange must not overtake the first wire
/// observation's ACK, and neither dropped consumers nor cancellation may send
/// attempt two. No provider, filesystem or subprocess participates.
#[tokio::test]
async fn deepseek_second_physical_send_waits_for_ack_and_refuses_delivery_loss() {
    for ending in 0..5 {
        let sends = Arc::new(AtomicUsize::new(0));
        let transport = ForcedRetryTransport {
            first: true.into(),
            second: UnaryUsageTransport {
                anthropic: false,
                outcome: Outcome::Valid,
                sends: sends.clone(),
            },
        };
        let provider = DeepSeekChatCompletionsProvider::without_auth(transport).unwrap();
        let request = unary_retry_request("deepseek");
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        let pending = PendingProviderWireObservations::default();
        let reported = Arc::new(Mutex::new(ProviderReportedUsage::default()));
        let observer = ProviderWireRequestObserver::new("retry-ack-original", "%1", sender)
            .with_pending_observations(pending.clone())
            .with_reported_usage(reported.clone());
        let observed = ObservedAsyncModelProvider::new(
            &provider,
            &observer,
            ProviderRequestPurpose::Execution,
        );
        let mut call = Box::pin(tokio::task::unconstrained(
            observed.send_request_async(&request),
        ));
        if ending == 0 {
            // An already-closed required sink cannot admit even attempt one.
            drop(receiver);
            let error = call.await.unwrap_err();
            assert_eq!(error.provider_incurred_usage().input_tokens, 0);
            assert_eq!(sends.load(Ordering::SeqCst), 0);
            assert!(pending.is_empty());
            assert!(observed.send_request_async(&request).await.is_err());
            assert_eq!(sends.load(Ordering::SeqCst), 0);
            continue;
        }
        let first = tokio::select! {
            biased;
            _ = &mut call => panic!("retry advanced before first observation ACK"),
            observation = receiver.recv() => observation.unwrap(),
        };
        assert_eq!(first.attempt_index, 1);
        assert_eq!(first.conversation_id, "retry-ack-original");
        assert_eq!(sends.load(Ordering::SeqCst), 1);
        assert_eq!(reported.lock().unwrap().usage.input_tokens, 10);
        pending.acknowledge("foreign-wire-id");
        std::future::poll_fn(|cx| {
            assert!(call.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        assert_eq!(sends.load(Ordering::SeqCst), 1);
        if ending == 1 {
            drop(call);
            assert_eq!(pending.snapshot(), vec![first]);
            assert_eq!(reported.lock().unwrap().known_observations, 1);
            continue;
        }
        if ending == 2 {
            drop(receiver);
            let error = call.await.unwrap_err();
            assert_eq!(error.provider_incurred_usage().input_tokens, 10);
            assert_eq!(pending.snapshot(), vec![first]);
            assert_eq!(sends.load(Ordering::SeqCst), 1);
            continue;
        }
        pending.acknowledge(&first.request_id);
        if ending == 4 {
            // Both exact ACK and closure are ready before the producer resumes.
            drop(receiver);
            assert!(call.await.is_err());
            assert_eq!(
                sends.load(Ordering::SeqCst),
                1,
                "ACK concealed consumer loss"
            );
            assert!(pending.is_empty(), "the original ACK remains valid");
            assert!(observed.send_request_async(&request).await.is_err());
            assert_eq!(sends.load(Ordering::SeqCst), 1);
            continue;
        }
        let second = tokio::select! {
            biased;
            _ = &mut call => panic!("retry advanced before second observation ACK"),
            observation = receiver.recv() => observation.unwrap(),
        };
        assert_eq!(second.attempt_index, 2);
        assert_ne!(second.request_id, first.request_id);
        assert_eq!(second.conversation_id, first.conversation_id);
        assert_eq!(sends.load(Ordering::SeqCst), 2);
        assert_eq!(pending.snapshot(), vec![second.clone()]);
        pending.acknowledge(&second.request_id);
        assert!(call.await.unwrap().action_batch.is_some());
        assert!(pending.is_empty());
        assert_eq!(reported.lock().unwrap().known_observations, 2);
    }
}

struct ForcedRetryTransport {
    first: std::sync::atomic::AtomicBool,
    second: UnaryUsageTransport,
}

impl AsyncProviderHttpTransport for ForcedRetryTransport {
    fn send_async<'a>(
        &'a self,
        request: &'a ProviderHttpRequest,
    ) -> Pin<
        Box<dyn Future<Output = mez_agent::ProviderHttpResult<ProviderHttpResponse>> + Send + 'a>,
    > {
        Box::pin(async move {
            if self.first.swap(false, Ordering::SeqCst) {
                self.second.sends.fetch_add(1, Ordering::SeqCst);
                Ok(ProviderHttpResponse {
                    status_code: 200,
                    headers: Default::default(),
                    body: serde_json::json!({"model":"deepseek-v4-pro", "choices":[{
                        "message":{"role":"assistant","content":"plain first response"}}],
                        "usage":{"prompt_tokens":10,"completion_tokens":4}})
                    .to_string(),
                })
            } else {
                let body: serde_json::Value = serde_json::from_str(&request.body).unwrap();
                assert_eq!(body["stream"], false);
                assert_eq!(body["thinking"]["type"], "disabled");
                assert!(body.get("tool_choice").is_some());
                self.second.send_async(request).await
            }
        })
    }
}

/// Genuine auto-thinking -> forced MAAP retry: report each concrete attempt
/// separately, and retain second-send expense even when its output is refused.
#[tokio::test]
async fn deepseek_unary_retry_failures_preserve_distinct_attempt_expense() {
    for outcome in [
        Outcome::Transport,
        Outcome::MissingContent,
        Outcome::MalformedMaap,
        Outcome::OutputLimit,
        Outcome::HttpStatus,
        Outcome::AbsentUsage,
        Outcome::ZeroUsage,
        Outcome::PartialUsage,
        Outcome::InvalidUsage,
        Outcome::InvalidJson,
        Outcome::MissingMaap,
        Outcome::Valid,
        Outcome::ValidAbsentUsage,
        Outcome::ValidZeroUsage,
    ] {
        let sends = Arc::new(AtomicUsize::new(0));
        let transport = ForcedRetryTransport {
            first: true.into(),
            second: UnaryUsageTransport {
                anthropic: false,
                outcome,
                sends: sends.clone(),
            },
        };
        let provider = DeepSeekChatCompletionsProvider::without_auth(transport).unwrap();
        let request = unary_retry_request("deepseek");
        let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
        let pending = PendingProviderWireObservations::default();
        let reported = Arc::new(Mutex::new(ProviderReportedUsage::default()));
        let observer = ProviderWireRequestObserver::new("retry-original", "%1", sender)
            .with_pending_observations(pending.clone())
            .with_reported_usage(reported.clone());
        let observed = ObservedAsyncModelProvider::new(
            &provider,
            &observer,
            ProviderRequestPurpose::Execution,
        );
        let (result, delivered) = observe_and_acknowledge_for_test(
            observed.send_request_async(&request),
            &mut receiver,
            &pending,
        )
        .await;
        assert_eq!(
            result.is_ok(),
            matches!(
                outcome,
                Outcome::Valid | Outcome::ValidAbsentUsage | Outcome::ValidZeroUsage
            ),
            "{outcome:?}: {result:?}"
        );
        let [first, second]: [_; 2] = delivered.try_into().unwrap();
        assert_eq!((first.attempt_index, second.attempt_index), (1, 2));
        assert_ne!(first.request_id, second.request_id);
        assert_eq!(second.retry_reason.as_deref(), Some("provider_forced_maap"));
        assert_eq!(first.conversation_id, "retry-original");
        assert_eq!(second.conversation_id, "retry-original");
        assert_eq!(
            first.usage_presence,
            Some(mez_agent::ModelTokenUsagePresence {
                input: true,
                output: true
            })
        );
        assert_eq!(
            first.usage.map(|u| (u.input_tokens, u.output_tokens)),
            Some((10, 4))
        );
        let present = !matches!(
            outcome,
            Outcome::Transport
                | Outcome::AbsentUsage
                | Outcome::ValidAbsentUsage
                | Outcome::InvalidUsage
                | Outcome::InvalidJson
        );
        let expected =
            if !present || matches!(outcome, Outcome::ZeroUsage | Outcome::ValidZeroUsage) {
                (0, 0)
            } else if matches!(outcome, Outcome::PartialUsage) {
                (0, 7)
            } else {
                (11, 7)
            };
        assert_eq!(
            second.usage.map(|u| (u.input_tokens, u.output_tokens)),
            present.then_some(expected),
            "{outcome:?}: {result:?}"
        );
        assert_eq!(
            second.usage_presence,
            Some(mez_agent::ModelTokenUsagePresence {
                input: present && !matches!(outcome, Outcome::PartialUsage),
                output: present,
            }),
            "{outcome:?}"
        );
        assert_eq!(second.succeeded, result.is_ok());
        assert!(pending.is_empty());
        assert!(receiver.try_recv().is_err());
        assert_eq!(sends.load(Ordering::SeqCst), 2);
        let reported = *reported.lock().unwrap();
        assert_eq!(reported.known_observations, 1 + u64::from(present));
        assert_eq!(
            (reported.usage.input_tokens, reported.usage.output_tokens),
            (10 + expected.0, 4 + expected.1)
        );
    }
}
