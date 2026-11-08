//! The engine is exercised through real provider request serialization and
//! response parsing. Only HTTP is fake; no live inference or semantic quality
//! claim is made. Dispatch/expense ownership remains external to this port.

use crate::integrations::agent::provider::{AsyncModelProvider, OpenAiResponsesProvider};
use crate::integrations::http::AsyncProviderHttpTransport;
use mez_agent::epoch_compaction::provider::*;
use mez_agent::epoch_compaction::{EpochCompactionLimits, FrozenEpochCompactionSource};
use mez_agent::{
    AgentCapability, AgentContext, AllowedActionSet, ContextBlock, ContextSourceKind, ModelProfile,
    ModelTokenUsage, ProviderApiCompatibility, ProviderHttpError, ProviderHttpRequest,
    ProviderHttpResponse, ProviderHttpResult, ReplayCompactionPreparation, ReplayConversationId,
    ReplayEpochContext, ReplayHeadPublication, ReplayTailPolicy,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    future::Future,
    pin::Pin,
    sync::Mutex,
};

/// One-consumer fixture stays beside these production-boundary tests.
struct Http {
    requests: Mutex<Vec<ProviderHttpRequest>>,
    responses: Mutex<VecDeque<ProviderHttpResult<ProviderHttpResponse>>>,
}

impl AsyncProviderHttpTransport for Http {
    fn send_async<'a>(
        &'a self,
        request: &'a ProviderHttpRequest,
    ) -> Pin<Box<dyn Future<Output = ProviderHttpResult<ProviderHttpResponse>> + Send + 'a>> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(request.clone());
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected provider call")
        })
    }
}

fn provider(
    responses: Vec<ProviderHttpResult<ProviderHttpResponse>>,
) -> OpenAiResponsesProvider<Http> {
    provider_for_stream(responses, false)
}

fn provider_for_stream(
    responses: Vec<ProviderHttpResult<ProviderHttpResponse>>,
    stream: bool,
) -> OpenAiResponsesProvider<Http> {
    OpenAiResponsesProvider::with_endpoint_headers_and_stream(
        "test-key",
        "https://example.test/responses",
        10_000,
        BTreeMap::new(),
        stream,
        Http {
            requests: Mutex::new(Vec::new()),
            responses: Mutex::new(responses.into()),
        },
    )
    .unwrap()
}

fn http_summary(text: &str) -> ProviderHttpResult<ProviderHttpResponse> {
    Ok(ProviderHttpResponse {
        status_code: 200,
        headers: BTreeMap::from([("content-type".into(), "application/json".into())]),
        body: json!({
            "model": "test-model", "status": "completed",
            "output": [{"type":"message", "role":"assistant", "content":[{"type":"output_text", "text":text}]}],
            "usage": {"input_tokens": 11, "output_tokens": 7}
        }).to_string(),
    })
}

fn profile() -> ModelProfile {
    ModelProfile {
        provider: "openai".into(),
        model: "test-model".into(),
        model_capabilities: Default::default(),
        reasoning_profile: Some("high".into()),
        latency_preference: None,
        multimodal_required: false,
        safety_tier: None,
        provider_options: BTreeMap::from([("max_output_tokens".into(), "16".into())]),
    }
}

fn limits(input: usize) -> EpochCompactionLimits {
    EpochCompactionLimits {
        input_tokens: input,
        framing_tokens: 0,
        source_bytes: 1024 * 1024,
        summary_bytes: 128 * 1024,
        buffered_summary_bytes: 1024 * 1024,
        work_nodes: 10_000,
        in_flight: 2,
        expected_summary_tokens: 100,
    }
}

fn session(text: &str, input: usize) -> (ReplayEpochContext, EpochCompactionProviderSession) {
    session_for_stream(text, input, false)
}

fn session_for_stream(
    text: &str,
    input: usize,
    stream: bool,
) -> (ReplayEpochContext, EpochCompactionProviderSession) {
    let mut owner = ReplayEpochContext::new(
        ReplayConversationId::new("provider-engine").unwrap(),
        AgentContext::new(vec![ContextBlock::stable_instruction(
            ContextSourceKind::System,
            "policy",
            "EXACT SYSTEM POLICY OUTSIDE SOURCE",
        )])
        .unwrap(),
    )
    .unwrap();
    owner.append_fact("source", text).unwrap();
    let plan = match owner
        .prepare_compaction(ReplayTailPolicy::new(1000, 0).unwrap())
        .unwrap()
    {
        ReplayCompactionPreparation::Ready(plan) => plan,
        other => panic!("expected canonical source, got {other:?}"),
    };
    let source = FrozenEpochCompactionSource::render(plan, 1024 * 1024, |events| {
        Ok(events
            .iter()
            .map(|event| event.block().content.as_str())
            .collect())
    })
    .unwrap();
    let template = request_from_source(
        &profile(),
        "%1",
        "provider-engine",
        String::new(),
        AllowedActionSet::for_capability(AgentCapability::RespondOnly),
    );
    let session = EpochCompactionProviderSession::new(
        source,
        template,
        ProviderApiCompatibility::OpenAiResponses,
        BTreeMap::new(),
        stream,
        limits(input),
    )
    .unwrap();
    (owner, session)
}

fn claim(session: &mut EpochCompactionProviderSession) -> EpochCompactionProviderWork {
    match session.claim().unwrap() {
        EpochCompactionProviderStep::Work(work) => work,
        step => panic!("expected necessary model work, got {step:?}"),
    }
}

fn assert_openai_controls(request: &ProviderHttpRequest) {
    let body: Value = serde_json::from_str(&request.body).unwrap();
    for absent in ["max_output_tokens", "max_completion_tokens", "max_tokens"] {
        assert!(
            body.get(absent).is_none(),
            "unexpected OpenAI output control {absent}"
        );
    }
    assert_eq!(body["reasoning"]["effort"], "high");
    assert!(request.url.ends_with("/responses"));
}

#[tokio::test]
async fn actual_provider_fitting_and_long_nonreducing_final_each_use_one_http_call() {
    for summary in [
        "current goal and next action".to_string(),
        "usable continuation\n".repeat(300),
    ] {
        let provider = provider(vec![http_summary(&summary)]);
        let (mut owner, mut session) = session("small source", 5000);
        let work = claim(&mut session);
        let response = provider.send_request_async(work.request()).await.unwrap();
        let usage = response.usage;
        assert!(matches!(
            session.complete(work.id(), &response).unwrap(),
            EpochCompactionProviderCompletion::Accepted
        ));
        assert!(session.complete(work.id(), &response).is_err());
        assert!(matches!(
            session.claim().unwrap(),
            EpochCompactionProviderStep::Complete
        ));
        let result = session.into_result().unwrap();
        assert_eq!(result.summary(), summary.trim());
        assert_eq!(result.work_count(), 1);
        let sent = provider.transport.requests.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_openai_controls(&sent[0]);
        assert_eq!((usage.input_tokens, usage.output_tokens), (11, 7));
        // A complete typed result, not a summary string, preserves original
        // source ownership through actual canonical publication.
        let (plan, summary) = result.into_parts();
        owner
            .publish_compaction(plan, ReplayHeadPublication::Summary(summary))
            .unwrap();
        assert_eq!(owner.revision().get(), 1);
    }
}

#[tokio::test]
async fn actual_http_transport_retry_preserves_body_and_does_not_repeat_accepted_final() {
    let provider = provider(vec![
        Err(ProviderHttpError::io("connection reset")),
        http_summary("valid"),
    ]);
    let (_, mut session) = session("frozen source", 5000);
    let work = claim(&mut session);
    assert!(provider.send_request_async(work.request()).await.is_err());
    let retry = session.retry(work.id()).unwrap();
    assert_eq!(retry.id(), work.id());
    let response = provider.send_request_async(retry.request()).await.unwrap();
    session.complete(retry.id(), &response).unwrap();
    let result = session.into_result().unwrap();
    assert_eq!(result.work_count(), 1);
    let sent = provider.transport.requests.lock().unwrap();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].body, sent[1].body);
    sent.iter().for_each(assert_openai_controls);
}

#[tokio::test]
async fn completed_empty_and_malformed_frames_repair_finitely_without_guidance_halving() {
    let malformed =
        r#"{"rationale":"invalid","actions":[{"type":"say","status":"working","text":"partial"}]}"#;
    let provider = provider(vec![
        http_summary(" "),
        http_summary(malformed),
        http_summary("valid final"),
    ]);
    let (_, mut session) = session("frozen source", 5000);
    let original = claim(&mut session);
    let mut next = original.clone();
    let mut usage = ModelTokenUsage::default();
    for attempt in 0..3 {
        let response = provider.send_request_async(next.request()).await.unwrap();
        // This test models the existing dispatch owner: settle physical usage
        // once before delivering output. The port creates no usage events.
        usage.add_assign(response.usage);
        match session.complete(next.id(), &response).unwrap() {
            EpochCompactionProviderCompletion::Repair(work) => {
                assert!(attempt < 2);
                assert_eq!(
                    work.request().messages.last(),
                    original.request().messages.last()
                );
                assert_eq!(work.request().max_output_tokens, Some(16));
                next = work;
            }
            EpochCompactionProviderCompletion::Accepted => assert_eq!(attempt, 2),
        }
    }
    assert_eq!((usage.input_tokens, usage.output_tokens), (33, 21));
    assert_eq!(session.into_result().unwrap().work_count(), 1);
    let sent = provider.transport.requests.lock().unwrap();
    assert_eq!(sent.len(), 3);
    sent.iter().for_each(assert_openai_controls);
}

#[tokio::test]
async fn exhausted_invalid_output_is_terminal_without_publication() {
    let provider = provider(vec![
        http_summary(" "),
        http_summary("\n"),
        http_summary("\t"),
    ]);
    let (owner, mut session) = session("frozen source", 5000);
    let mut next = claim(&mut session);
    for attempt in 0..3 {
        let response = provider.send_request_async(next.request()).await.unwrap();
        let completion = session.complete(next.id(), &response);
        if attempt == 2 {
            assert!(completion.is_err());
        } else {
            let EpochCompactionProviderCompletion::Repair(work) = completion.unwrap() else {
                panic!("invalid output accepted");
            };
            next = work;
        }
    }
    assert!(session.claim().is_err());
    assert!(session.retry(next.id()).is_err());
    assert!(session.into_result().is_err());
    assert_eq!(owner.revision().get(), 0);
    assert_eq!(provider.transport.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn real_context_rejection_subdivides_same_source_and_keeps_partial_results_private() {
    let rejection = Ok(ProviderHttpResponse {
        status_code: 400,
        headers: BTreeMap::new(),
        body: json!({"error":{"code":"context_length_exceeded","message":"input too large"}})
            .to_string(),
    });
    let provider = provider(vec![
        rejection,
        http_summary("first"),
        http_summary("final"),
    ]);
    let text = format!(
        "{} AFTER4096💡 {} TAIL-SENTINEL\n",
        "x".repeat(5000),
        "y".repeat(5000)
    );
    let (owner, mut session) = session(&text, 5000);
    let first = claim(&mut session);
    let error = provider
        .send_request_async(first.request())
        .await
        .unwrap_err();
    assert_eq!(
        crate::integrations::agent::provider::provider_error_retry_class(&error),
        mez_agent::ProviderErrorRetryClass::ContextLimit
    );
    let initial = mez_agent::provider_request_input_estimate(
        first.request(),
        ProviderApiCompatibility::OpenAiResponses,
        &BTreeMap::new(),
        false,
    )
    .unwrap()
    .input_tokens;
    session.reject_input(first.id(), initial / 2 + 500).unwrap();
    assert!(session.retry(first.id()).is_err());
    let mut raw_inputs = String::new();
    let mut calls = 1;
    loop {
        match session.claim().unwrap() {
            EpochCompactionProviderStep::Work(work) => {
                let input = &work.request().messages.last().unwrap().content;
                let expected = work
                    .raw_coverage()
                    .iter()
                    .map(|range| &text[range.clone()])
                    .collect::<String>();
                assert!(input.ends_with(&expected));
                raw_inputs.push_str(&input[input.len() - expected.len()..]);
                let response = provider.send_request_async(work.request()).await.unwrap();
                session.complete(work.id(), &response).unwrap();
                calls += 1;
                assert_eq!(owner.revision().get(), 0);
            }
            EpochCompactionProviderStep::Complete => break,
            EpochCompactionProviderStep::Waiting => panic!("no outstanding claims"),
        }
    }
    assert_eq!(raw_inputs, text);
    assert_eq!(calls, 3);
    assert_eq!(session.into_result().unwrap().work_count(), 3);
    let sent = provider.transport.requests.lock().unwrap();
    assert!(sent[1].body.len() < sent[0].body.len());
    sent.iter().for_each(assert_openai_controls);
}

#[tokio::test]
async fn genuine_summary_parser_overflow_fails_once_without_shortening_or_publication() {
    let provider = provider(vec![http_summary(&"x".repeat(128 * 1024 + 1))]);
    let (owner, mut session) = session("frozen source", 5000);
    let work = claim(&mut session);
    let response = provider.send_request_async(work.request()).await.unwrap();
    assert!(session.complete(work.id(), &response).is_err());
    assert!(session.retry(work.id()).is_err());
    assert!(session.claim().is_err());
    assert!(session.into_result().is_err());
    assert_eq!(owner.revision().get(), 0);
    assert_eq!(provider.transport.requests.lock().unwrap().len(), 1);
}

/// The provider rejects an actually absent output before the summary port can
/// repair it. That finite failure still owes its known physical-dispatch cost.
#[tokio::test]
async fn empty_http200_provider_rejection_preserves_expense_without_publishing() {
    let provider = provider(vec![http_summary("")]);
    let (owner, mut session) = session("frozen source", 5000);
    let work = claim(&mut session);
    let error = provider
        .send_request_async(work.request())
        .await
        .unwrap_err();
    let usage = error.provider_incurred_usage();
    assert_eq!((usage.input_tokens, usage.output_tokens), (11, 7));
    assert!(error.provider_output_limit_state().is_none());
    assert!(session.into_result().is_err());
    assert_eq!(owner.revision().get(), 0);
    let sent = provider.transport.requests.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_openai_controls(&sent[0]);
}

#[tokio::test]
async fn actual_http_incomplete_output_repairs_without_changing_supported_allocation() {
    let incomplete = Ok(ProviderHttpResponse {
        status_code: 200,
        headers: BTreeMap::from([("content-type".into(), "application/json".into())]),
        body: json!({
            "model":"test-model", "status":"incomplete",
            "incomplete_details":{"reason":"max_output_tokens"},
            "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"PARTIAL MUST NOT PUBLISH"}]}],
            "usage":{"input_tokens":11,"output_tokens":7}
        }).to_string(),
    });
    let provider = provider(vec![incomplete, http_summary("completed continuation")]);
    let (owner, mut session) = session("frozen source", 5000);
    let work = claim(&mut session);
    let error = provider
        .send_request_async(work.request())
        .await
        .unwrap_err();
    assert_eq!(
        crate::integrations::agent::provider::provider_error_retry_class(&error),
        mez_agent::ProviderErrorRetryClass::OutputLimit
    );
    let mut usage = error.provider_incurred_usage();
    let EpochCompactionProviderCompletion::Repair(repair) = session
        .repair(work.id(), EpochCompactionOutputDefect::Incomplete)
        .unwrap()
    else {
        panic!("partial output accepted");
    };
    assert_eq!(
        repair.request().max_output_tokens,
        work.request().max_output_tokens
    );
    assert_eq!(
        repair.request().messages.last(),
        work.request().messages.last()
    );
    let response = provider.send_request_async(repair.request()).await.unwrap();
    usage.add_assign(response.usage);
    session.complete(repair.id(), &response).unwrap();
    assert_eq!(
        session.into_result().unwrap().summary(),
        "completed continuation"
    );
    assert_eq!((usage.input_tokens, usage.output_tokens), (22, 14));
    assert_eq!(owner.revision().get(), 0);
    let sent = provider.transport.requests.lock().unwrap();
    assert_eq!(sent.len(), 2);
    sent.iter().for_each(assert_openai_controls);
}

#[tokio::test]
async fn interrupted_http200_sse_cannot_publish_and_retries_exact_work() {
    let interrupted = Ok(ProviderHttpResponse {
        status_code: 200,
        headers: BTreeMap::from([("content-type".into(), "text/event-stream".into())]),
        body: concat!(
            "event: response.in_progress\ndata: {\"type\":\"response.in_progress\",\"response\":{\"usage\":{\"input_tokens\":13,\"output_tokens\":5}}}\n\n",
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"PARTIAL\"}\n\n"
        ).into(),
    });
    let provider = provider_for_stream(vec![interrupted, http_summary("completed")], true);
    let (owner, mut session) = session_for_stream("frozen source", 5000, true);
    let work = claim(&mut session);
    let error = provider
        .send_request_async(work.request())
        .await
        .unwrap_err();
    let usage = error.provider_incurred_usage();
    assert_eq!((usage.input_tokens, usage.output_tokens), (13, 5));
    assert_eq!(owner.revision().get(), 0);
    let retry = session.retry(work.id()).unwrap();
    let response = provider.send_request_async(retry.request()).await.unwrap();
    session.complete(retry.id(), &response).unwrap();
    assert_eq!(session.into_result().unwrap().summary(), "completed");
    let sent = provider.transport.requests.lock().unwrap();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].body, sent[1].body);
    assert!(
        serde_json::from_str::<Value>(&sent[0].body).unwrap()["stream"]
            .as_bool()
            .unwrap()
    );
    sent.iter().for_each(assert_openai_controls);
}

/// Supersession/provenance/constraints/references are mechanical prompt and
/// decoding fixtures. A supplied fake final is not evidence of model quality.
#[tokio::test]
async fn semantic_source_renderer_redacts_before_packing_and_preserves_whitespace() {
    let mut owner = ReplayEpochContext::new(
        ReplayConversationId::new("rendered").unwrap(),
        AgentContext::empty(),
    )
    .unwrap();
    owner
        .append_fact("evidence", "verified: left\n  api_key=PRIVATE\tβ \nRIGHT")
        .unwrap();
    let plan = match owner
        .prepare_compaction(ReplayTailPolicy::new(1000, 0).unwrap())
        .unwrap()
    {
        ReplayCompactionPreparation::Ready(plan) => plan,
        other => panic!("expected source: {other:?}"),
    };
    let source = render_epoch_source(plan, 100_000).unwrap();
    assert!(
        source
            .text()
            .contains("verified: left\n  [redacted]\tβ \nRIGHT")
    );
    assert!(!source.text().contains("api_key"));
    let template = request_from_source(
        &profile(),
        "%1",
        "rendered",
        String::new(),
        AllowedActionSet::for_capability(AgentCapability::RespondOnly),
    );
    let instructions = template
        .messages
        .iter()
        .filter(|message| message.role != mez_agent::ModelMessageRole::User)
        .map(|message| message.content.as_str())
        .collect::<String>();
    for required in [
        "active constraints",
        "essential rationale",
        "provenance",
        "unresolved failures",
        "next actions",
        "exact critical references",
        "superseded",
        "unsupported claims",
        "untrusted reference",
        "advisory",
    ] {
        assert!(
            instructions.contains(required),
            "missing prompt contract {required}"
        );
    }
    let mut session = EpochCompactionProviderSession::new(
        source,
        template,
        ProviderApiCompatibility::OpenAiResponses,
        BTreeMap::new(),
        false,
        limits(5000),
    )
    .unwrap();
    let final_text = "Goal: preserve Linux and macOS. Decision: shared engine for one source contract. Verified: source.rs:12 inspected. Blocker: check failure unresolved. Superseded: old size target. Next: validate provider path.";
    let provider = provider(vec![http_summary(final_text)]);
    let work = claim(&mut session);
    let response = provider.send_request_async(work.request()).await.unwrap();
    session.complete(work.id(), &response).unwrap();
    assert_eq!(session.into_result().unwrap().summary(), final_text);
    assert_eq!(provider.transport.requests.lock().unwrap().len(), 1);
}

/// Offline local measurements separate source readiness, actual HTTP-adapter
/// work, dependency depth and total latency. No CI wall-clock SLA or live model
/// quality/latency claim is inferred from this zero-network fake provider.
#[tokio::test]
async fn local_short_and_large_source_measurements_report_calls_depth_and_latency() {
    for bytes in [64, 64 * 1024] {
        let provider = provider(
            (0..100)
                .map(|_| http_summary("private intermediate state"))
                .collect(),
        );
        let start = std::time::Instant::now();
        let (_, mut session) = session(&"z".repeat(bytes), 5000);
        let source_ready = start.elapsed();
        let mut inference = std::time::Duration::ZERO;
        let mut calls = 0;
        loop {
            match session.claim().unwrap() {
                EpochCompactionProviderStep::Work(work) => {
                    let began = std::time::Instant::now();
                    let response = provider.send_request_async(work.request()).await.unwrap();
                    inference += began.elapsed();
                    session.complete(work.id(), &response).unwrap();
                    calls += 1;
                }
                EpochCompactionProviderStep::Complete => break,
                EpochCompactionProviderStep::Waiting => panic!("no outstanding claim"),
            }
        }
        let result = session.into_result().unwrap();
        assert_eq!(calls, result.work_count());
        if bytes == 64 {
            assert_eq!(calls, 1);
        }
        assert_eq!(
            provider.transport.requests.lock().unwrap().len() as u64,
            calls
        );
        eprintln!(
            "{}",
            json!({"fixture":"offline-epoch-compaction", "source_bytes":bytes,
            "source_ready_us":source_ready.as_micros(), "inference_calls":calls,
            "serial_depth":result.serial_depth(), "fake_inference_us":inference.as_micros(),
            "total_us":start.elapsed().as_micros()})
        );
    }
}

/// A real streaming transport can deliver a usage event and then fail before
/// returning a body. Reuse the ordinary fake HTTP port for the successful retry
/// rather than reimplementing its SSE/JSON dispatch behavior in this fixture.
struct EventsThenFailure {
    http: Http,
    fail_once: std::sync::atomic::AtomicBool,
}

impl AsyncProviderHttpTransport for EventsThenFailure {
    fn send_async<'a>(
        &'a self,
        request: &'a ProviderHttpRequest,
    ) -> Pin<Box<dyn Future<Output = ProviderHttpResult<ProviderHttpResponse>> + Send + 'a>> {
        self.http.send_async(request)
    }

    fn send_async_with_sse_events<'a>(
        &'a self,
        request: &'a ProviderHttpRequest,
        on_event: &'a mut (
                    dyn FnMut(mez_agent::SseEvent) -> Pin<Box<dyn Future<Output = ()> + Send>>
                        + Send
                ),
    ) -> Pin<Box<dyn Future<Output = ProviderHttpResult<ProviderHttpResponse>> + Send + 'a>> {
        Box::pin(async move {
            if self
                .fail_once
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                self.http.requests.lock().unwrap().push(request.clone());
                on_event(mez_agent::SseEvent {
                    name: Some("response.in_progress".into()),
                    data: json!({"type":"response.in_progress","response":{"usage":{"input_tokens":13,"output_tokens":5}}}).to_string(),
                }).await;
                Err(ProviderHttpError::io("connection reset after usage event"))
            } else {
                self.http
                    .send_async_with_sse_events(request, on_event)
                    .await
            }
        })
    }
}

#[tokio::test]
async fn transport_failure_after_sse_usage_preserves_known_expense_and_exact_retry() {
    let provider = OpenAiResponsesProvider::with_endpoint_headers_and_stream(
        "test-key",
        "https://example.test/responses",
        10_000,
        BTreeMap::new(),
        true,
        EventsThenFailure {
            http: Http {
                requests: Mutex::new(Vec::new()),
                responses: Mutex::new(vec![http_summary("final")].into()),
            },
            fail_once: std::sync::atomic::AtomicBool::new(true),
        },
    )
    .unwrap();
    let (owner, mut session) = session_for_stream("frozen source", 5000, true);
    let work = claim(&mut session);
    let error = provider
        .send_request_async(work.request())
        .await
        .unwrap_err();
    let mut usage = error.provider_incurred_usage();
    assert_eq!((usage.input_tokens, usage.output_tokens), (13, 5));
    assert_eq!(owner.revision().get(), 0);
    let retry = session.retry(work.id()).unwrap();
    let response = provider.send_request_async(retry.request()).await.unwrap();
    usage.add_assign(response.usage);
    session.complete(retry.id(), &response).unwrap();
    assert_eq!((usage.input_tokens, usage.output_tokens), (24, 12));
    assert_eq!(session.into_result().unwrap().work_count(), 1);
    let sent = provider.transport.http.requests.lock().unwrap();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].body, sent[1].body);
    sent.iter().for_each(assert_openai_controls);
}
