//! Real loopback HTTP -> exact local future destruction -> native factory ->
//! durable subtotal + qualified terminal retirement -> actor ACK. Reported partial
//! counters never become complete merely because the local transport retired.

use super::*;
use crate::host::async_runtime::client::provider_service::budget_admission::BudgetAttemptAdmission;
use crate::integrations::agent::provider::{
    AnthropicMessagesProvider, DeepSeekChatCompletionsProvider, OpenAiResponsesProvider,
    ProviderObservationReceipt, ProviderTransportRetirement,
};
use crate::integrations::http::ReqwestProviderHttpTransport;
use futures_util::FutureExt;
use std::task::{Context, Poll};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// This transparent fixture wrapper observes destruction of the real Reqwest
/// future, including scoped SSE callbacks, without replacing network I/O.
struct Tracked<'a> {
    inner: Option<
        Pin<
            Box<
                dyn Future<Output = mez_agent::ProviderHttpResult<mez_agent::ProviderHttpResponse>>
                    + Send
                    + 'a,
            >,
        >,
    >,
    dropped: Arc<AtomicUsize>,
}
impl Future for Tracked<'_> {
    type Output = mez_agent::ProviderHttpResult<mez_agent::ProviderHttpResponse>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.get_mut().inner.as_mut().unwrap().as_mut().poll(cx)
    }
}
impl Drop for Tracked<'_> {
    fn drop(&mut self) {
        drop(self.inner.take());
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

struct Transport {
    dropped: Arc<AtomicUsize>,
    event: Arc<tokio::sync::Notify>,
    blocked_callback: bool,
    callback_dropped: Arc<AtomicUsize>,
    hold_after_terminal: bool,
}

struct CallbackGuard(Arc<AtomicUsize>);
impl Drop for CallbackGuard {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
impl AsyncProviderHttpTransport for Transport {
    fn send_async<'a>(
        &'a self,
        request: &'a mez_agent::ProviderHttpRequest,
    ) -> Pin<
        Box<
            dyn Future<Output = mez_agent::ProviderHttpResult<mez_agent::ProviderHttpResponse>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(Tracked {
            inner: Some(ReqwestProviderHttpTransport.send_async(request)),
            dropped: self.dropped.clone(),
        })
    }
    fn send_async_with_sse_events<'a>(
        &'a self,
        request: &'a mez_agent::ProviderHttpRequest,
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
        Box::pin(Tracked {
            inner: Some(Box::pin(async move {
                let mut observed = |event: mez_agent::SseEvent| {
                    let selected = !self.hold_after_terminal
                        || serde_json::from_str::<serde_json::Value>(&event.data)
                            .ok()
                            .is_some_and(|value| {
                                value["type"] == "response.completed"
                                    || (value["type"] == "message_delta"
                                        && value
                                            .pointer("/delta/stop_reason")
                                            .and_then(serde_json::Value::as_str)
                                            .is_some())
                                    || (value["choices"].as_array().is_some_and(Vec::is_empty)
                                        && value.get("usage").is_some())
                            });
                    let callback = on_event(event);
                    let event = self.event.clone();
                    let blocked = self.blocked_callback && selected;
                    let guard = blocked.then(|| CallbackGuard(self.callback_dropped.clone()));
                    Box::pin(async move {
                        let _guard = guard;
                        callback.await;
                        if selected {
                            event.notify_one();
                        }
                        if blocked {
                            std::future::pending::<()>().await;
                        }
                    }) as Pin<Box<dyn Future<Output = ()> + Send>>
                };
                ReqwestProviderHttpTransport
                    .send_async_with_sse_events(request, &mut observed)
                    .await
            })),
            dropped: self.dropped.clone(),
        })
    }
}

struct Factory {
    native: BudgetAttemptAdmission,
    dropped: Arc<AtomicUsize>,
    retired: AtomicUsize,
    callback_dropped: Option<Arc<AtomicUsize>>,
    stages: std::sync::OnceLock<(
        crate::runtime::RuntimeBudgetReceiptWork,
        Result<crate::runtime::RuntimeBudgetReceiptWork>,
    )>,
}
impl ProviderObservationReceiptFactory for Factory {
    fn plan(&self, o: &ProviderWireRequestObservation) -> Result<()> {
        self.native.plan(o)
    }
    fn before_send<'a>(
        &'a self,
        o: &'a ProviderWireRequestObservation,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        self.native.before_send(o)
    }
    fn usage_finalized(
        &self,
        o: &ProviderWireRequestObservation,
        usage: crate::integrations::agent::provider::ProviderFinalUsage,
    ) -> Result<()> {
        self.native.usage_finalized(o, usage)
    }
    fn transport_retired(
        &self,
        o: &ProviderWireRequestObservation,
        proof: ProviderTransportRetirement,
    ) {
        assert_eq!(
            self.dropped.load(Ordering::SeqCst),
            1,
            "native factory preceded real transport destruction"
        );
        if let Some(callback) = &self.callback_dropped {
            assert_eq!(
                callback.load(Ordering::SeqCst),
                1,
                "active callback outlived transport witness"
            );
        }
        assert!(!self.native.has_transport_retirement(o));
        self.native.transport_retired(o, proof);
        assert!(self.native.has_transport_retirement(o));
        self.retired.fetch_add(1, Ordering::SeqCst);
    }
    fn freeze(
        &self,
        o: &ProviderWireRequestObservation,
    ) -> Result<Arc<dyn ProviderObservationReceipt>> {
        assert!(
            self.native.has_transport_retirement(o),
            "receipt preceded local transport retirement"
        );
        let receipt = self.native.freeze(o)?;
        assert!(
            self.stages
                .set(self.native.receipt_stages_for_test(o))
                .is_ok()
        );
        Ok(receipt)
    }
}

#[tokio::test]
async fn budget_wire_loopback_transport_retirement_precedes_native_receipt_and_ack() {
    for (responses, stream) in [(false, false), (true, false), (true, true)] {
        for cancel in [false, true] {
            qualify(responses, stream, cancel, false, 0).await;
        }
    }
}

#[tokio::test]
async fn budget_wire_loopback_transport_destroys_actively_blocked_sse_callback() {
    qualify(true, true, true, true, 0).await;
}

/// The original real-HTTP request stays at its actor ACK barrier while exact
/// terminal bookkeeping fails, loses its result, or outlives a canceled waiter.
/// A queued lane writer must remain blocked even after explicit storage-only
/// recovery, until the original receipt checkpoint adopts terminal evidence.
#[tokio::test]
async fn budget_wire_loopback_retirement_writer_faults_keep_original_custody() {
    for fault in 1..=3 {
        qualify(true, false, false, false, fault).await;
    }
}

async fn qualify(responses: bool, stream: bool, cancel: bool, blocked_callback: bool, fault: u8) {
    qualify_usage(
        responses,
        stream,
        cancel,
        blocked_callback,
        fault,
        UnaryUsage::Totals,
    )
    .await;
}

#[derive(Clone, Copy, Debug)]
enum UnaryUsage {
    Totals,
    InputOnly,
    OutputOnly,
    CacheOnly,
    ReasoningOnly,
    Zero,
    Nonterminal,
    InvalidMaap,
    Incomplete,
    Failed,
    Cancelled,
    HttpFailure,
    JsonInsteadOfSse,
    MissingFinalUsage,
    DoneOnly,
    ConflictingFinal,
    RegressedFinal,
    ConflictingFinalRepeat,
    NonregressingConflict,
    RegressedThenCancel,
    MissingChoiceIndex,
    ExcessChoices,
    MissingCacheFields,
    PartialInputChange,
    AnthropicConflict,
    MixedRegression,
}

impl UnaryUsage {
    fn counters(self, responses: bool) -> serde_json::Value {
        let (input, output, details_in, details_out) = if responses {
            (
                "input_tokens",
                "output_tokens",
                "input_tokens_details",
                "output_tokens_details",
            )
        } else {
            (
                "prompt_tokens",
                "completion_tokens",
                "prompt_tokens_details",
                "completion_tokens_details",
            )
        };
        match self {
            Self::InputOnly => serde_json::json!({input:11}),
            Self::OutputOnly => serde_json::json!({output:3}),
            Self::CacheOnly => serde_json::json!({details_in:{"cached_tokens":11}}),
            Self::ReasoningOnly => serde_json::json!({details_out:{"reasoning_tokens":3}}),
            Self::Zero => serde_json::json!({input:0,output:0}),
            _ => serde_json::json!({input:11,output:3}),
        }
    }

    fn totals(self) -> UsageTotals {
        match self {
            Self::InputOnly | Self::CacheOnly => UsageTotals {
                input: 11,
                output: 0,
            },
            Self::OutputOnly | Self::ReasoningOnly => UsageTotals {
                input: 0,
                output: 3,
            },
            Self::Zero => UsageTotals::default(),
            _ => UsageTotals {
                input: 11,
                output: 3,
            },
        }
    }

    fn complete(self) -> (Option<u64>, Option<u64>) {
        match self {
            Self::InputOnly => (Some(11), None),
            Self::OutputOnly => (None, Some(3)),
            Self::CacheOnly
            | Self::ReasoningOnly
            | Self::Nonterminal
            | Self::HttpFailure
            | Self::MissingFinalUsage
            | Self::DoneOnly
            | Self::MissingChoiceIndex
            | Self::ExcessChoices
            | Self::ConflictingFinal
            | Self::ConflictingFinalRepeat
            | Self::NonregressingConflict => (None, None),
            Self::Zero => (Some(0), Some(0)),
            _ => (Some(11), Some(3)),
        }
    }
}

#[tokio::test]
async fn budget_wire_loopback_unary_complete_dimensions_do_not_promote_subsets_or_prefixes() {
    for responses in [false, true] {
        for usage in [
            UnaryUsage::InputOnly,
            UnaryUsage::OutputOnly,
            UnaryUsage::CacheOnly,
            UnaryUsage::ReasoningOnly,
            UnaryUsage::Zero,
            UnaryUsage::Nonterminal,
            UnaryUsage::InvalidMaap,
            UnaryUsage::HttpFailure,
        ] {
            qualify_usage(responses, false, false, false, 0, usage).await;
        }
    }
}

#[tokio::test]
async fn budget_wire_loopback_responses_terminal_failure_and_actual_json_shape_keep_final_usage() {
    for usage in [
        UnaryUsage::Incomplete,
        UnaryUsage::Failed,
        UnaryUsage::Cancelled,
    ] {
        qualify_usage(true, false, false, false, 0, usage).await;
    }
    qualify_usage(true, true, false, false, 0, UnaryUsage::JsonInsteadOfSse).await;
}

#[tokio::test]
async fn budget_wire_loopback_stream_final_fields_remain_distinct_from_prefixes() {
    for usage in [
        UnaryUsage::InputOnly,
        UnaryUsage::OutputOnly,
        UnaryUsage::CacheOnly,
        UnaryUsage::ReasoningOnly,
        UnaryUsage::Zero,
        UnaryUsage::MissingFinalUsage,
        UnaryUsage::DoneOnly,
        UnaryUsage::Incomplete,
        UnaryUsage::Failed,
        UnaryUsage::InvalidMaap,
        UnaryUsage::HttpFailure,
        UnaryUsage::ConflictingFinal,
        UnaryUsage::ConflictingFinalRepeat,
        UnaryUsage::NonregressingConflict,
        UnaryUsage::RegressedFinal,
    ] {
        qualify_usage(true, true, false, false, 0, usage).await;
    }
}

#[tokio::test]
async fn budget_wire_loopback_stream_regression_survives_active_callback_cancellation() {
    qualify_usage(true, true, true, false, 0, UnaryUsage::RegressedThenCancel).await;
}

/// Chat usage-only chunks must qualify their own dimensions after a choice
/// finish, never borrow primary presence from an earlier cumulative prefix.
#[tokio::test]
async fn budget_wire_loopback_chat_stream_final_fields_are_independent() {
    for usage in [
        UnaryUsage::MissingChoiceIndex,
        UnaryUsage::ExcessChoices,
        UnaryUsage::Totals,
        UnaryUsage::InputOnly,
        UnaryUsage::OutputOnly,
        UnaryUsage::CacheOnly,
        UnaryUsage::ReasoningOnly,
        UnaryUsage::Zero,
        UnaryUsage::MissingFinalUsage,
        UnaryUsage::InvalidMaap,
        UnaryUsage::HttpFailure,
        UnaryUsage::RegressedFinal,
        UnaryUsage::ConflictingFinal,
        UnaryUsage::ConflictingFinalRepeat,
        UnaryUsage::NonregressingConflict,
    ] {
        qualify_usage(false, true, false, false, 0, usage).await;
    }
}

async fn qualify_usage(
    responses: bool,
    stream: bool,
    cancel: bool,
    blocked_callback: bool,
    fault: u8,
    usage_case: UnaryUsage,
) {
    qualify_dialect_usage(
        responses,
        stream,
        cancel,
        blocked_callback,
        fault,
        usage_case,
        WireDialect::Chat,
    )
    .await;
}

#[derive(Clone, Copy)]
enum WireDialect {
    Chat,
    DeepSeek,
    Anthropic,
}

/// Anthropic start-input and terminal-delta output are separate evidence;
/// actual HTTP, native receipt commit and actor ACK remain mandatory.
#[tokio::test]
async fn budget_wire_loopback_anthropic_final_dimensions() {
    for stream in [false, true] {
        for usage in [
            UnaryUsage::Totals,
            UnaryUsage::InputOnly,
            UnaryUsage::OutputOnly,
            UnaryUsage::CacheOnly,
            UnaryUsage::MissingCacheFields,
            UnaryUsage::Zero,
            UnaryUsage::Nonterminal,
            UnaryUsage::InvalidMaap,
            UnaryUsage::HttpFailure,
        ] {
            qualify_dialect_usage(
                false,
                stream,
                false,
                false,
                0,
                usage,
                WireDialect::Anthropic,
            )
            .await;
        }
    }
}

#[tokio::test]
async fn budget_wire_loopback_anthropic_partial_cache_change_keeps_output_final() {
    qualify_dialect_usage(
        false,
        true,
        false,
        false,
        0,
        UnaryUsage::PartialInputChange,
        WireDialect::Anthropic,
    )
    .await;
}

#[tokio::test]
async fn budget_wire_loopback_anthropic_terminal_conflicts_and_missing_fields() {
    for usage in [
        UnaryUsage::MissingFinalUsage,
        UnaryUsage::DoneOnly,
        UnaryUsage::ConflictingFinal,
        UnaryUsage::RegressedFinal,
        UnaryUsage::AnthropicConflict,
    ] {
        qualify_dialect_usage(false, true, false, false, 0, usage, WireDialect::Anthropic).await;
    }
}

#[tokio::test]
async fn budget_wire_loopback_anthropic_regression_survives_callback_cancellation() {
    qualify_dialect_usage(
        false,
        true,
        true,
        false,
        0,
        UnaryUsage::RegressedThenCancel,
        WireDialect::Anthropic,
    )
    .await;
}

#[tokio::test]
async fn budget_wire_loopback_anthropic_mixed_regression_conserves_larger_dimension() {
    qualify_dialect_usage(
        false,
        true,
        true,
        false,
        0,
        UnaryUsage::MixedRegression,
        WireDialect::Anthropic,
    )
    .await;
}

#[tokio::test]
async fn budget_wire_loopback_deepseek_stream_finality_and_cancellation() {
    for usage in [
        UnaryUsage::MissingChoiceIndex,
        UnaryUsage::ExcessChoices,
        UnaryUsage::Totals,
        UnaryUsage::InputOnly,
        UnaryUsage::OutputOnly,
        UnaryUsage::CacheOnly,
        UnaryUsage::ReasoningOnly,
        UnaryUsage::Zero,
        UnaryUsage::MissingFinalUsage,
        UnaryUsage::HttpFailure,
        UnaryUsage::RegressedFinal,
        UnaryUsage::ConflictingFinal,
        UnaryUsage::ConflictingFinalRepeat,
        UnaryUsage::NonregressingConflict,
    ] {
        qualify_dialect_usage(false, true, false, false, 0, usage, WireDialect::DeepSeek).await;
    }
    for deepseek in [false, true] {
        qualify_dialect_usage(
            false,
            true,
            true,
            false,
            0,
            UnaryUsage::RegressedThenCancel,
            if deepseek {
                WireDialect::DeepSeek
            } else {
                WireDialect::Chat
            },
        )
        .await;
    }
}

async fn qualify_dialect_usage(
    responses: bool,
    stream: bool,
    cancel: bool,
    blocked_callback: bool,
    fault: u8,
    usage_case: UnaryUsage,
    dialect: WireDialect,
) {
    let deepseek = matches!(dialect, WireDialect::DeepSeek);
    let anthropic = matches!(dialect, WireDialect::Anthropic);
    let Prepared {
        service,
        dispatch,
        store,
        member,
        mut root,
    } = prepared_case(BudgetLimits::default()).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let (entered, received) = tokio::sync::oneshot::channel();
    let (release, gate) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        // Read the actual entire submitted request before gating the response.
        // Bounded generated input only; no provider/user payload is printed.
        let mut bytes = Vec::new();
        let end = loop {
            if let Some(index) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                break index + 4;
            }
            let mut chunk = [0; 4096];
            let read = socket.read(&mut chunk).await.unwrap();
            assert!(read > 0 && bytes.len() + read <= 1024 * 1024);
            bytes.extend_from_slice(&chunk[..read]);
        };
        let headers = String::from_utf8(bytes[..end].to_vec()).unwrap();
        let length: usize = headers
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
            let read = socket.read(&mut chunk).await.unwrap();
            assert!(read > 0);
            bytes.extend_from_slice(&chunk[..read]);
        }
        entered.send(()).unwrap();
        if anthropic {
            let text = if matches!(usage_case, UnaryUsage::InvalidMaap) {
                serde_json::json!({"rationale":"fixture","actions":[{"type":"invalid_fixture_action"}]}).to_string()
            } else {
                serde_json::json!({"rationale":"fixture","actions":[{"type":"say","status":"final","content_type":"text/plain; charset=utf-8","text":"done"}]}).to_string()
            };
            let input = match usage_case {
                UnaryUsage::OutputOnly => serde_json::json!({}),
                UnaryUsage::CacheOnly => serde_json::json!({"cache_read_input_tokens":11}),
                UnaryUsage::MissingCacheFields => serde_json::json!({"input_tokens":11}),
                UnaryUsage::Zero => {
                    serde_json::json!({"input_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0})
                }
                _ => {
                    serde_json::json!({"input_tokens":3,"cache_read_input_tokens":5,"cache_creation_input_tokens":3})
                }
            };
            let output = match usage_case {
                UnaryUsage::InputOnly | UnaryUsage::CacheOnly => None,
                UnaryUsage::Zero => Some(0),
                _ => Some(3),
            };
            let stop = if matches!(usage_case, UnaryUsage::Nonterminal) {
                None
            } else {
                Some("tool_use")
            };
            let status = if matches!(usage_case, UnaryUsage::HttpFailure) {
                "500 Internal Server Error"
            } else {
                "200 OK"
            };
            if stream {
                let mut input = input;
                if let Some(output) = output {
                    input["output_tokens"] = serde_json::json!(output);
                }
                let start = serde_json::json!({"type":"message_start","message":{"type":"message","role":"assistant","model":"test-model","usage":input}});
                let block = serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"fixture-tool","name":"submit_maap_action_batch","input":serde_json::from_str::<serde_json::Value>(&text).unwrap()}});
                let mut delta = serde_json::json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":output.map(|output|serde_json::json!({"output_tokens":output})).unwrap_or(serde_json::json!({}))});
                if matches!(usage_case, UnaryUsage::PartialInputChange) {
                    delta["usage"]["cache_read_input_tokens"] = serde_json::json!(7);
                }
                if matches!(usage_case, UnaryUsage::MixedRegression) {
                    delta["usage"] = serde_json::json!({"input_tokens":2,"cache_read_input_tokens":5,"cache_creation_input_tokens":3,"output_tokens":5});
                }
                if matches!(
                    usage_case,
                    UnaryUsage::RegressedThenCancel | UnaryUsage::RegressedFinal
                ) {
                    delta["usage"]["output_tokens"] = serde_json::json!(2);
                }
                if matches!(usage_case, UnaryUsage::MissingFinalUsage) {
                    delta["usage"] = serde_json::json!({});
                }
                let mut data = format!(
                    "data: {start}\n\ndata: {block}\n\ndata: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n"
                );
                if !matches!(usage_case, UnaryUsage::DoneOnly) {
                    data.push_str(&format!("data: {delta}\n\n"));
                }
                if matches!(
                    usage_case,
                    UnaryUsage::ConflictingFinal | UnaryUsage::AnthropicConflict
                ) {
                    delta["usage"]["output_tokens"] =
                        serde_json::json!(if matches!(usage_case, UnaryUsage::AnthropicConflict) {
                            5
                        } else {
                            2
                        });
                    data.push_str(&format!("data: {delta}\n\n"));
                }
                data.push_str("data: {\"type\":\"message_stop\"}\n\n");
                socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{data}\r\n0\r\n\r\n",data.len()).as_bytes()).await.unwrap();
            } else {
                let mut usage = input;
                if let Some(output) = output {
                    usage["output_tokens"] = serde_json::json!(output);
                }
                let body = serde_json::json!({"type":"message","role":"assistant","model":"test-model","stop_reason":stop,"content":[{"type":"tool_use","id":"fixture-tool","name":"submit_maap_action_batch","input":serde_json::from_str::<serde_json::Value>(&text).unwrap()}],"usage":usage}).to_string();
                socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
        } else if stream && !responses {
            let mut text = serde_json::json!({"rationale":"fixture","actions":[{"type":"say","status":"final","content_type":"text/plain; charset=utf-8","text":"done"}]}).to_string();
            if matches!(usage_case, UnaryUsage::InvalidMaap) {
                text = serde_json::json!({"rationale":"fixture","actions":[{"type":"invalid_fixture_action"}]}).to_string();
            }
            let prefix_usage = if matches!(usage_case, UnaryUsage::Zero) {
                serde_json::json!({"prompt_tokens":0,"completion_tokens":0})
            } else {
                serde_json::json!({"prompt_tokens":11,"completion_tokens":3})
            };
            let mut prefix = serde_json::json!({"model":"test-model","choices":[{"index":0,"delta":{"content":text},"finish_reason":null}],"usage":prefix_usage});
            let mut finish =
                serde_json::json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]});
            if matches!(usage_case, UnaryUsage::MissingChoiceIndex) {
                prefix["choices"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("index");
            } else if matches!(usage_case, UnaryUsage::ExcessChoices) {
                for index in 1..65 {
                    prefix["choices"]
                        .as_array_mut()
                        .unwrap()
                        .push(serde_json::json!({"index":index,"delta":{},"finish_reason":null}));
                    finish["choices"]
                        .as_array_mut()
                        .unwrap()
                        .push(serde_json::json!({"index":index,"delta":{},"finish_reason":"stop"}));
                }
            }
            let usage = if matches!(usage_case, UnaryUsage::MissingFinalUsage) {
                serde_json::json!({})
            } else if matches!(
                usage_case,
                UnaryUsage::RegressedFinal | UnaryUsage::RegressedThenCancel
            ) {
                serde_json::json!({"prompt_tokens":10,"completion_tokens":3})
            } else if deepseek && matches!(usage_case, UnaryUsage::CacheOnly) {
                serde_json::json!({"prompt_cache_hit_tokens":11})
            } else {
                usage_case.counters(false)
            };
            let terminal = serde_json::json!({"choices":[],"usage":usage});
            let mut data = format!("data: {prefix}\n\n");
            if !cancel || matches!(usage_case, UnaryUsage::RegressedThenCancel) {
                data.push_str(&format!("data: {finish}\n\ndata: {terminal}\n\n"));
                if matches!(
                    usage_case,
                    UnaryUsage::ConflictingFinal
                        | UnaryUsage::ConflictingFinalRepeat
                        | UnaryUsage::NonregressingConflict
                ) {
                    let mut changed = terminal.clone();
                    changed["usage"]["prompt_tokens"] = serde_json::json!(if matches!(
                        usage_case,
                        UnaryUsage::NonregressingConflict
                    ) {
                        13
                    } else {
                        10
                    });
                    data.push_str(&format!("data: {changed}\n\n"));
                    if matches!(usage_case, UnaryUsage::ConflictingFinalRepeat) {
                        data.push_str(&format!("data: {terminal}\n\n"));
                    }
                }
                data.push_str("data: [DONE]\n\n");
            }
            let status = if matches!(usage_case, UnaryUsage::HttpFailure) {
                "500 Internal Server Error"
            } else {
                "200 OK"
            };
            socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{data}\r\n",data.len()).as_bytes()).await.unwrap();
            if !cancel {
                socket.write_all(b"0\r\n\r\n").await.unwrap();
            }
        } else if stream && !matches!(usage_case, UnaryUsage::JsonInsteadOfSse) {
            let prefix_usage = if matches!(usage_case, UnaryUsage::Zero) {
                serde_json::json!({"input_tokens":0,"output_tokens":0})
            } else {
                serde_json::json!({"input_tokens":11,"output_tokens":3})
            };
            let partial = serde_json::json!({"type":"response.in_progress","response":{"usage":prefix_usage}});
            let text = serde_json::json!({"rationale":"fixture","actions":[{"type":"say","status":"final","content_type":"text/plain; charset=utf-8","text":"done"}]}).to_string();
            let text = if matches!(usage_case, UnaryUsage::InvalidMaap) {
                serde_json::json!({"rationale":"fixture","actions":[{"type":"invalid_fixture_action"}]}).to_string()
            } else {
                text
            };
            let delta = serde_json::json!({"type":"response.output_text.delta","delta":text});
            let usage = match usage_case {
                UnaryUsage::MissingFinalUsage => serde_json::json!({}),
                UnaryUsage::RegressedFinal | UnaryUsage::RegressedThenCancel => {
                    serde_json::json!({"input_tokens":10,"output_tokens":3})
                }
                _ => usage_case.counters(true),
            };
            let kind = match usage_case {
                UnaryUsage::Incomplete => "response.incomplete",
                UnaryUsage::Failed => "response.failed",
                _ => "response.completed",
            };
            let terminal = serde_json::json!({"type":kind,"response":{"model":"test-model","status":"completed","output_text":text,"usage":usage}});
            let data = if cancel && !matches!(usage_case, UnaryUsage::RegressedThenCancel) {
                format!("data: {partial}\n\n")
            } else if matches!(usage_case, UnaryUsage::DoneOnly) {
                format!("data: {partial}\n\ndata: {delta}\n\ndata: [DONE]\n\n")
            } else if matches!(
                usage_case,
                UnaryUsage::ConflictingFinal
                    | UnaryUsage::ConflictingFinalRepeat
                    | UnaryUsage::NonregressingConflict
            ) {
                let mut changed = terminal.clone();
                changed["response"]["usage"]["input_tokens"] =
                    serde_json::json!(if matches!(usage_case, UnaryUsage::NonregressingConflict) {
                        13
                    } else {
                        10
                    });
                let repeated = if matches!(usage_case, UnaryUsage::ConflictingFinalRepeat) {
                    format!("data: {terminal}\n\n")
                } else {
                    String::new()
                };
                format!(
                    "data: {partial}\n\ndata: {delta}\n\ndata: {terminal}\n\ndata: {changed}\n\n{repeated}"
                )
            } else {
                format!("data: {partial}\n\ndata: {delta}\n\ndata: {terminal}\n\n")
            };
            let status = if matches!(usage_case, UnaryUsage::HttpFailure) {
                "500 Internal Server Error"
            } else {
                "200 OK"
            };
            socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{data}\r\n", data.len()).as_bytes()).await.unwrap();
            if !cancel {
                socket.write_all(b"0\r\n\r\n").await.unwrap();
            }
        } else if !cancel {
            let text = serde_json::json!({"rationale":"fixture","actions":[{"type":"say","status":"final","content_type":"text/plain; charset=utf-8","text":"done"}]}).to_string();
            let text = if matches!(usage_case, UnaryUsage::InvalidMaap) {
                serde_json::json!({"rationale":"fixture","actions":[{"type":"invalid_fixture_action"}]}).to_string()
            } else {
                text
            };
            let nonterminal = matches!(usage_case, UnaryUsage::Nonterminal);
            let usage = usage_case.counters(responses);
            let status = match usage_case {
                UnaryUsage::Nonterminal => "in_progress",
                UnaryUsage::Incomplete => "incomplete",
                UnaryUsage::Failed => "failed",
                UnaryUsage::Cancelled => "cancelled",
                _ => "completed",
            };
            let body = if responses { serde_json::json!({"model":"test-model","status":status,"output_text":text,"usage":usage}) }
                else { serde_json::json!({"model":"test-model","choices":[{"index":0,"finish_reason":if nonterminal {None} else {Some("stop")},"message":{"role":"assistant","content":text}}],"usage":usage}) }.to_string();
            let http_status = if matches!(usage_case, UnaryUsage::HttpFailure) {
                "500 Internal Server Error"
            } else {
                "200 OK"
            };
            socket.write_all(format!("HTTP/1.1 {http_status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
        // Keep the exact server fixture owned until the client has explicitly
        // observed/destroyed its request future, even on ordinary success.
        let _ = gate.await;
    });
    let dropped = Arc::new(AtomicUsize::new(0));
    let event = Arc::new(tokio::sync::Notify::new());
    let callback_dropped = Arc::new(AtomicUsize::new(0));
    let hold_after_terminal = matches!(
        usage_case,
        UnaryUsage::RegressedThenCancel | UnaryUsage::MixedRegression
    );
    let blocked_callback = blocked_callback || hold_after_terminal;
    let transport = Transport {
        dropped: dropped.clone(),
        event: event.clone(),
        blocked_callback,
        callback_dropped: callback_dropped.clone(),
        hold_after_terminal,
    };
    let native_anthropic = anthropic.then(|| {
        AnthropicMessagesProvider::without_auth(Transport {
            dropped: dropped.clone(),
            event: event.clone(),
            blocked_callback,
            callback_dropped: callback_dropped.clone(),
            hold_after_terminal,
        })
        .unwrap()
        .with_endpoint(endpoint.clone())
        .with_stream(stream)
    });
    let (chat, response, native_deepseek) = if responses {
        (
            None,
            Some(
                OpenAiResponsesProvider::with_endpoint_headers_and_stream(
                    "fixture-key",
                    &endpoint,
                    10_000,
                    Default::default(),
                    stream,
                    transport,
                )
                .unwrap(),
            ),
            None,
        )
    } else if deepseek {
        (
            None,
            None,
            Some(
                DeepSeekChatCompletionsProvider::without_auth(transport)
                    .unwrap()
                    .with_endpoint(endpoint)
                    .with_stream(stream),
            ),
        )
    } else {
        (
            Some(
                OpenAiCompatibleChatCompletionsProvider::without_auth(transport)
                    .unwrap()
                    .with_provider_id("local-chat")
                    .unwrap()
                    .with_streaming_options_for_test(stream)
                    .unwrap()
                    .with_endpoint(endpoint),
            ),
            None,
            None,
        )
    };
    let provider: &dyn AsyncModelProvider = if let Some(provider) = &native_anthropic {
        provider
    } else {
        match &chat {
            Some(p) => p,
            None => match &response {
                Some(p) => p,
                None => native_deepseek.as_ref().unwrap(),
            },
        }
    };
    let mut request = crate::integrations::agent::context::assemble_model_request(
        &dispatch.model_profile,
        provider.api_compatibility(),
        &dispatch.turn,
        dispatch.context.durable(),
    )
    .unwrap();
    request.provider = provider.provider_id().into();
    let work = dispatch
        .budget_owner
        .as_ref()
        .unwrap()
        .prepare_attempt(
            mez_agent::ModelTokenUsageKey::new(&request.provider, &request.model),
            BudgetAttemptPurpose::Execution,
        )
        .unwrap();
    let lane = Arc::new(tokio::sync::RwLock::new(()));
    let factory = Arc::new(Factory {
        native: if fault == 0 {
            BudgetAttemptAdmission::new(work)
        } else {
            BudgetAttemptAdmission::with_lane_for_test(work, lane.clone())
        },
        dropped: dropped.clone(),
        retired: AtomicUsize::new(0),
        callback_dropped: blocked_callback.then(|| callback_dropped.clone()),
        stages: Default::default(),
    });
    let pending = PendingProviderWireObservations::default();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    let observer = ProviderWireRequestObserver::new(&member.conversation, "%1", sender)
        .with_claim_generation(7)
        .with_pending_observations(pending.clone())
        .with_budget_receipts(factory.clone())
        .unwrap();
    let chat_observed = chat.as_ref().map(|provider| {
        ObservedAsyncModelProvider::new(provider, &observer, ProviderRequestPurpose::Execution)
    });
    let responses_observed = response.as_ref().map(|provider| {
        ObservedAsyncModelProvider::new(provider, &observer, ProviderRequestPurpose::Execution)
    });
    let deepseek_observed = native_deepseek.as_ref().map(|provider| {
        ObservedAsyncModelProvider::new(provider, &observer, ProviderRequestPurpose::Execution)
    });
    let anthropic_observed = native_anthropic.as_ref().map(|provider| {
        ObservedAsyncModelProvider::new(provider, &observer, ProviderRequestPurpose::Execution)
    });
    let observed: &dyn AsyncModelProvider = if let Some(provider) = &anthropic_observed {
        provider
    } else {
        match &chat_observed {
            Some(provider) => provider,
            None => match &responses_observed {
                Some(p) => p,
                None => deepseek_observed.as_ref().unwrap(),
            },
        }
    };
    let (handle, actor) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
            .unwrap();
    let client = async {
        let mut call = Box::pin(tokio::task::unconstrained(
            observed.send_request_async(&request),
        ));
        tokio::select! { biased; _ = &mut call => panic!("exchange finished before request received"), r = received => r.unwrap(), }
        if cancel {
            if stream {
                tokio::select! { biased; _ = &mut call => panic!("partial SSE completed"), () = event.notified() => {} }
            }
            assert_eq!(dropped.load(Ordering::SeqCst), 0);
            assert_eq!(factory.retired.load(Ordering::SeqCst), 0);
            assert_eq!(callback_dropped.load(Ordering::SeqCst), 0);
            drop(call);
        } else {
            let observation = tokio::select! { biased; _ = &mut call => panic!("exchange bypassed actor ACK"), o = receiver.recv() => o.unwrap() };
            assert_eq!(factory.retired.load(Ordering::SeqCst), 1);
            assert!(factory.native.has_transport_retirement(&observation));
            assert_eq!(rows(&store, "budget_receipts"), 0);
            if matches!(
                usage_case,
                UnaryUsage::RegressedFinal
                    | UnaryUsage::ConflictingFinal
                    | UnaryUsage::ConflictingFinalRepeat
            ) {
                assert!(pending.checkpoint(&observation).await.is_err());
                assert_eq!(rows(&store, "budget_receipts"), 1);
                assert_eq!(
                    store.budget_account(&member.root).unwrap().known,
                    UsageTotals {
                        input: 11,
                        output: 3
                    }
                );
                let db = rusqlite::Connection::open(store.path()).unwrap();
                assert_eq!(
                    db.query_row(
                        "SELECT COUNT(*) FROM budget_admissions WHERE retired_receipt IS NULL",
                        [],
                        |r| r.get::<_, i64>(0)
                    )
                    .unwrap(),
                    1
                );
                pending.acknowledge(&observation.request_id);
                assert_eq!(
                    pending.len(),
                    1,
                    "contradictory final totals cannot authorize ACK"
                );
                drop(call);
                return;
            }
            let mut lane_waiter = Box::pin(lane.clone().write_owned());
            if fault != 0 {
                let (prefix, terminal) = factory.stages.get().unwrap();
                prefix.commit_owner().commit().await.unwrap();
                assert_eq!(rows(&store, "budget_receipts"), 1);
                assert!(
                    futures_util::poll!(&mut lane_waiter).is_pending(),
                    "subtotal ACK released the original reader"
                );
                let terminal = terminal.as_ref().unwrap();
                let owner = terminal.commit_owner();
                match fault {
                    1 => {
                        let db = rusqlite::Connection::open(store.path()).unwrap();
                        db.execute_batch("CREATE TRIGGER refuse_retirement BEFORE UPDATE OF retired_receipt ON budget_admissions BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
                        assert!(pending.checkpoint(&observation).await.is_err());
                        assert_eq!(rows(&store, "budget_receipts"), 1);
                        assert!(
                            futures_util::poll!(&mut lane_waiter).is_pending(),
                            "failed terminal transaction released the original reader"
                        );
                        assert_eq!(db.query_row("SELECT COUNT(*) FROM budget_admissions WHERE retired_receipt IS NULL", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
                        db.execute_batch("DROP TRIGGER refuse_retirement;").unwrap();
                        assert!(
                            pending.checkpoint(&observation).await.is_err(),
                            "refusal must remain sticky without explicit bookkeeping recovery"
                        );
                        assert!(owner.reconcile().await.unwrap().is_none());
                        owner.retry_after_absence().await.unwrap();
                    }
                    2 => {
                        let execute = terminal.clone();
                        assert!(
                            owner
                                .commit_with_for_test(move || {
                                    execute.execute()?;
                                    panic!("fixture terminal result lost after physical commit");
                                })
                                .await
                                .is_err()
                        );
                        assert_eq!(rows(&store, "budget_receipts"), 2);
                        assert!(owner.reconcile().await.unwrap().is_some());
                    }
                    3 => {
                        let execute = terminal.clone();
                        let borrowed = owner.clone();
                        let (started, ready) = tokio::sync::oneshot::channel();
                        let (release_writer, blocked) = std::sync::mpsc::channel();
                        let waiter = tokio::spawn(async move {
                            borrowed
                                .commit_with_for_test(move || {
                                    let _ = started.send(());
                                    blocked.recv().map_err(|_| {
                                        MezError::invalid_state("fixture writer cleanup")
                                    })?;
                                    execute.execute()
                                })
                                .await
                        });
                        ready.await.unwrap();
                        waiter.abort();
                        assert!(waiter.await.unwrap_err().is_cancelled());
                        assert_eq!(rows(&store, "budget_receipts"), 1);
                        {
                            let checkpoint = pending.checkpoint(&observation);
                            tokio::pin!(checkpoint);
                            assert!(futures_util::poll!(&mut checkpoint).is_pending());
                        }
                        release_writer.send(()).unwrap();
                        assert!(owner.reconcile().await.unwrap().is_some());
                    }
                    _ => unreachable!(),
                }
                assert!(futures_util::poll!(&mut call).is_pending());
                assert_eq!(factory.retired.load(Ordering::SeqCst), 1);
                assert_eq!(pending.len(), 1);
                assert!(
                    futures_util::poll!(&mut lane_waiter).is_pending(),
                    "storage recovery alone bypassed original receipt adoption"
                );
            }
            submit_pending_provider_observation(&handle, &pending, observation)
                .await
                .unwrap();
            if fault != 0 {
                let writer = tokio::time::timeout(Duration::from_secs(5), &mut lane_waiter)
                    .await
                    .expect("terminal checkpoint did not release the original lane");
                drop(writer);
            }
            let result = call.await;
            if matches!(
                usage_case,
                UnaryUsage::InvalidMaap
                    | UnaryUsage::HttpFailure
                    | UnaryUsage::Incomplete
                    | UnaryUsage::Failed
                    | UnaryUsage::Cancelled
                    | UnaryUsage::ConflictingFinal
                    | UnaryUsage::NonregressingConflict
                    | UnaryUsage::AnthropicConflict
            ) || (!anthropic && matches!(usage_case, UnaryUsage::DoneOnly))
                || (responses && matches!(usage_case, UnaryUsage::Nonterminal))
            {
                assert!(
                    result.is_err(),
                    "invalid/nonterminal output accepted: {usage_case:?}"
                );
            } else {
                assert!(result.unwrap().action_batch.is_some());
            }
        }
        if cancel {
            let observation = pending.snapshot().pop().unwrap();
            assert!(factory.native.has_transport_retirement(&observation));
            assert_eq!(observation.usage.is_some(), stream);
            if matches!(
                usage_case,
                UnaryUsage::RegressedThenCancel | UnaryUsage::MixedRegression
            ) {
                assert!(pending.checkpoint(&observation).await.is_err());
                assert_eq!(rows(&store, "budget_receipts"), 1);
                assert_eq!(
                    store.budget_account(&member.root).unwrap().known,
                    UsageTotals {
                        input: 11,
                        output: if matches!(usage_case, UnaryUsage::MixedRegression) {
                            5
                        } else {
                            3
                        }
                    }
                );
                let db = rusqlite::Connection::open(store.path()).unwrap();
                assert_eq!(
                    db.query_row(
                        "SELECT COUNT(*) FROM budget_admissions WHERE retired_receipt IS NULL",
                        [],
                        |r| r.get::<_, i64>(0)
                    )
                    .unwrap(),
                    1
                );
                pending.acknowledge(&observation.request_id);
                assert_eq!(pending.len(), 1);
                return;
            }
            submit_pending_provider_observation(&handle, &pending, observation)
                .await
                .unwrap();
        }
        assert!(pending.is_empty());
        assert_eq!(
            rows(&store, "budget_receipts"),
            2,
            "retired transport must durably transfer subtotal and terminal-unknown evidence"
        );
        assert_eq!(
            (
                dropped.load(Ordering::SeqCst),
                factory.retired.load(Ordering::SeqCst)
            ),
            (1, 1)
        );
        assert_eq!(
            rows(&store, "budget_admissions"),
            1,
            "retirement must preserve the original replay-only grant"
        );
        let db = rusqlite::Connection::open(store.path()).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM budget_admissions WHERE retired_receipt IS NULL",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        let snapshot: String = db
            .query_row("SELECT snapshot FROM budget_attempts", [], |r| r.get(0))
            .unwrap();
        let snapshot: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
        let complete = if !cancel {
            if anthropic && matches!(usage_case, UnaryUsage::MissingFinalUsage) {
                (Some(11), None)
            } else if matches!(usage_case, UnaryUsage::AnthropicConflict) {
                (None, None)
            } else if anthropic
                && matches!(
                    usage_case,
                    UnaryUsage::MissingCacheFields | UnaryUsage::PartialInputChange
                )
            {
                (None, Some(3))
            } else {
                usage_case.complete()
            }
        } else {
            (None, None)
        };
        assert_eq!(
            snapshot["terminal"]["input"].as_u64(),
            complete.0,
            "{usage_case:?}"
        );
        assert_eq!(
            snapshot["terminal"]["output"].as_u64(),
            complete.1,
            "{usage_case:?}"
        );
        let known = store.budget_account(&member.root).unwrap().known;
        assert_eq!(
            known,
            if cancel && !stream {
                UsageTotals::default()
            } else if matches!(usage_case, UnaryUsage::AnthropicConflict) {
                UsageTotals {
                    input: 11,
                    output: 5,
                }
            } else if matches!(
                usage_case,
                UnaryUsage::PartialInputChange | UnaryUsage::NonregressingConflict
            ) {
                UsageTotals {
                    input: 13,
                    output: 3,
                }
            } else if stream && !anthropic && !matches!(usage_case, UnaryUsage::Zero) {
                UsageTotals {
                    input: 11,
                    output: 3,
                }
            } else {
                usage_case.totals()
            }
        );
    };
    let control = async {
        let result = tokio::time::timeout(
            Duration::from_secs(15),
            std::panic::AssertUnwindSafe(client).catch_unwind(),
        )
        .await;
        let _ = release.send(());
        let mut server = server;
        let server_result = match tokio::time::timeout(Duration::from_secs(10), &mut server).await {
            Ok(result) => result.is_ok(),
            Err(_) => {
                server.abort();
                let _ = server.await;
                false
            }
        };
        handle.shutdown().await.unwrap();
        (result, server_result)
    };
    let (result, mut exit) = tokio::join!(control, actor.run());
    exit.service.terminate_all_pane_processes().unwrap();
    drop((
        exit, observer, factory, dispatch, chat, response, handle, pending,
    ));
    root.cleanup();
    let (result, server_result) = result;
    assert!(
        server_result,
        "loopback server failed or exceeded its bounded join"
    );
    if let Err(panic) = result.unwrap() {
        std::panic::resume_unwind(panic);
    }
}
