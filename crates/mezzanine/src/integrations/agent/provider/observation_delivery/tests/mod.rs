//! Actor acknowledgment gates every journal-owned provider continuation, while
//! aborted waits and failed channels retain the same original expense evidence.
//! This handoff is not durable budget settlement or native retirement proof.

mod budget;

use super::*;
use crate::error::Result;
use crate::integrations::agent::provider::{
    AsyncModelProvider, ObservedAsyncModelProvider, ProviderRequestPurpose,
    ProviderWireRequestObserver, SharedProviderReportedUsage,
};
use mez_agent::{ModelRequest, ModelResponse};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct ImmediateProvider {
    calls: AtomicUsize,
    fail: bool,
    on_send: Option<Box<dyn Fn() + Send + Sync>>,
}

impl AsyncModelProvider for ImmediateProvider {
    fn provider_id(&self) -> &str {
        "fixture"
    }
    fn api_compatibility(&self) -> mez_agent::ProviderApiCompatibility {
        mez_agent::ProviderApiCompatibility::OpenAiChatCompletions
    }
    fn send_request_async<'a>(
        &'a self,
        _request: &'a ModelRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ModelResponse>> + Send + 'a>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(on_send) = &self.on_send {
                on_send();
            }
            if self.fail {
                return Err(crate::error::MezError::new(
                    crate::error::MezErrorKind::Forbidden,
                    "fixture permission failure",
                )
                .with_provider_failure_json(r#"{"error":{"code":"fixture_denied"}}"#)
                .with_provider_incurred_usage(mez_agent::ModelTokenUsage {
                    input_tokens: 17,
                    output_tokens: 3,
                    ..Default::default()
                }));
            }
            Ok(ModelResponse {
                provider: "fixture".into(),
                model: "model".into(),
                raw_text: "summary".into(),
                usage: mez_agent::ModelTokenUsage {
                    input_tokens: 17,
                    output_tokens: 3,
                    ..Default::default()
                },
                latest_request_usage: None,
                quota_usage: Vec::new(),
                action_batch: None,
                provider_transcript_events: Vec::new(),
            })
        })
    }
}

struct Fixture {
    provider: ImmediateProvider,
    request: ModelRequest,
    observer: ProviderWireRequestObserver,
    receiver: tokio::sync::mpsc::Receiver<ProviderWireRequestObservation>,
    pending: PendingProviderWireObservations,
    reported: SharedProviderReportedUsage,
}

fn fixture() -> Fixture {
    let (sender, receiver) = tokio::sync::mpsc::channel(4);
    let pending = PendingProviderWireObservations::default();
    let reported = SharedProviderReportedUsage::default();
    let observer = ProviderWireRequestObserver::new("conversation", "%1", sender)
        .with_claim_generation(7)
        .with_pending_observations(pending.clone())
        .with_reported_usage(reported.clone());
    let request = mez_agent::epoch_compaction::provider::request_from_source(
        &mez_agent::ModelProfile {
            provider: "fixture".into(),
            model: "model".into(),
            ..Default::default()
        },
        "%1",
        "conversation",
        "frozen source".into(),
        mez_agent::AllowedActionSet::say_only(),
    );
    Fixture {
        provider: Default::default(),
        request,
        observer,
        receiver,
        pending,
        reported,
    }
}

/// Poll the immediate model call before its delivery consumer. Without the
/// acknowledgment barrier, it returns Ready and fails deterministically; no
/// sleep or CPU scheduling race is used as evidence that work remains blocked.
#[tokio::test]
async fn compactor_response_waits_for_exact_actor_acknowledgment() {
    let mut fixture = fixture();
    let provider = ObservedAsyncModelProvider::new(
        &fixture.provider,
        &fixture.observer,
        ProviderRequestPurpose::Compaction,
    );
    let mut call = Box::pin(tokio::task::unconstrained(
        provider.send_request_async(&fixture.request),
    ));
    let observation = tokio::select! {
        biased;
        _ = &mut call => panic!("compactor advanced before its receipt was acknowledged"),
        observation = fixture.receiver.recv() => observation.unwrap(),
    };
    assert_eq!(fixture.pending.len(), 1);
    assert_eq!(fixture.reported.lock().unwrap().usage.input_tokens, 17);
    fixture.pending.acknowledge(&observation.request_id);
    let response = call.await.unwrap();
    assert_eq!(
        (response.usage.input_tokens, response.usage.output_tokens),
        (17, 3)
    );
    assert!(fixture.pending.is_empty());
    // An acknowledgment that preceded registration is also immediately visible.
    fixture
        .pending
        .wait_for_acknowledgment(&observation.request_id)
        .await;
}

/// Every original-owner journal must order continuation after its own actor
/// ACK, including turn-less requests with no execution claim generation.
/// This does not treat actor ingress as a durable budget receipt.
#[tokio::test]
async fn all_provider_purposes_wait_for_exact_original_actor_acknowledgment() {
    for purpose in [
        ProviderRequestPurpose::Execution,
        ProviderRequestPurpose::Routing,
        ProviderRequestPurpose::Auxiliary,
        ProviderRequestPurpose::Compaction,
        ProviderRequestPurpose::Memory,
    ] {
        for claim_generation in [None, Some(7)] {
            let mut fixture = fixture();
            fixture.observer.claim_generation = claim_generation;
            let provider =
                ObservedAsyncModelProvider::new(&fixture.provider, &fixture.observer, purpose);
            let mut call = Box::pin(tokio::task::unconstrained(
                provider.send_request_async(&fixture.request),
            ));
            let observation = tokio::select! {
                biased;
                _ = &mut call => panic!("{purpose:?}/{claim_generation:?} advanced without original ACK"),
                observation = fixture.receiver.recv() => observation.unwrap(),
            };
            assert_eq!(observation.purpose, purpose);
            assert_eq!(fixture.pending.len(), 1);
            fixture.pending.acknowledge("foreign-wire-id");
            // A wrong ACK must neither remove the evidence nor release output.
            std::future::poll_fn(|cx| {
                assert!(call.as_mut().poll(cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            fixture.pending.acknowledge(&observation.request_id);
            assert_eq!(call.await.unwrap().usage.input_tokens, 17);
            assert!(fixture.pending.is_empty());
        }
    }
}

#[tokio::test]
async fn cancelled_acknowledgment_wait_retains_original_receipt() {
    let mut fixture = fixture();
    let provider = ObservedAsyncModelProvider::new(
        &fixture.provider,
        &fixture.observer,
        ProviderRequestPurpose::Compaction,
    );
    let mut call = Box::pin(tokio::task::unconstrained(
        provider.send_request_async(&fixture.request),
    ));
    let observation = tokio::select! {
        biased;
        _ = &mut call => panic!("compactor advanced before its receipt was acknowledged"),
        observation = fixture.receiver.recv() => observation.unwrap(),
    };
    drop(call);
    assert_eq!(fixture.pending.snapshot(), vec![observation.clone()]);
    assert_eq!(fixture.reported.lock().unwrap().known_observations, 1);
    fixture.pending.acknowledge(&observation.request_id);
    assert!(fixture.pending.is_empty());
}

#[tokio::test]
async fn failed_delivery_preserves_expense_and_prevents_another_model_call() {
    let mut fixture = fixture();
    let receiver = std::sync::Mutex::new(Some(fixture.receiver));
    fixture.provider.on_send = Some(Box::new(move || {
        receiver.lock().unwrap().take();
    }));
    let provider = ObservedAsyncModelProvider::new(
        &fixture.provider,
        &fixture.observer,
        ProviderRequestPurpose::Compaction,
    );
    let error = provider
        .send_request_async(&fixture.request)
        .await
        .unwrap_err();
    let incurred = error.provider_incurred_usage();
    assert_eq!((incurred.input_tokens, incurred.output_tokens), (17, 3));
    assert_eq!(fixture.pending.len(), 1);
    assert_eq!(fixture.reported.lock().unwrap().known_observations, 1);
    assert!(provider.send_request_async(&fixture.request).await.is_err());
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 1);
}

/// A received channel item is not an ACK. Losing that consumer must wake the
/// original wait without releasing its evidence or allowing another request.
#[tokio::test]
async fn all_provider_purposes_receiver_loss_wakes_wait_and_preserves_failure() {
    for purpose in [
        ProviderRequestPurpose::Execution,
        ProviderRequestPurpose::Routing,
        ProviderRequestPurpose::Auxiliary,
        ProviderRequestPurpose::Compaction,
        ProviderRequestPurpose::Memory,
    ] {
        for fail in [false, true] {
            let mut fixture = fixture();
            fixture.provider.fail = fail;
            fixture.observer.claim_generation = None;
            let provider =
                ObservedAsyncModelProvider::new(&fixture.provider, &fixture.observer, purpose);
            let mut call = Box::pin(tokio::task::unconstrained(
                provider.send_request_async(&fixture.request),
            ));
            let observation = tokio::select! {
                biased;
                _ = &mut call => panic!("request advanced before ACK"),
                observation = fixture.receiver.recv() => observation.unwrap(),
            };
            drop(fixture.receiver);
            let error = std::future::poll_fn(|cx| match call.as_mut().poll(cx) {
                std::task::Poll::Ready(result) => std::task::Poll::Ready(result.unwrap_err()),
                std::task::Poll::Pending => panic!("closed receiver stranded original wait"),
            })
            .await;
            assert_eq!(error.provider_incurred_usage().input_tokens, 17);
            if fail {
                assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
                assert_eq!(error.message(), "fixture permission failure");
                assert!(
                    error
                        .provider_failure_json()
                        .unwrap()
                        .contains("fixture_denied")
                );
            }
            assert_eq!(fixture.pending.snapshot(), vec![observation]);
            assert!(provider.send_request_async(&fixture.request).await.is_err());
            assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 1);
            assert_eq!(fixture.reported.lock().unwrap().known_observations, 1);
        }
    }
}

/// Dropping a wait retains original evidence for every purpose and does not
/// pretend that cancellation supplied the missing actor acknowledgment.
#[tokio::test]
async fn all_purposes_cancelled_ack_waits_retain_original_evidence() {
    for purpose in [
        ProviderRequestPurpose::Execution,
        ProviderRequestPurpose::Routing,
        ProviderRequestPurpose::Auxiliary,
        ProviderRequestPurpose::Compaction,
        ProviderRequestPurpose::Memory,
    ] {
        let mut fixture = fixture();
        fixture.observer.claim_generation = None;
        let provider =
            ObservedAsyncModelProvider::new(&fixture.provider, &fixture.observer, purpose);
        let mut call = Box::pin(tokio::task::unconstrained(
            provider.send_request_async(&fixture.request),
        ));
        let observation = tokio::select! {
            biased;
            _ = &mut call => panic!("request advanced before ACK"),
            observation = fixture.receiver.recv() => observation.unwrap(),
        };
        drop(call);
        assert_eq!(fixture.pending.snapshot(), vec![observation]);
        assert_eq!(fixture.reported.lock().unwrap().known_observations, 1);
    }
}

/// No journal means optional diagnostics, not an unserviceable required gate.
#[tokio::test]
async fn best_effort_observation_without_journal_needs_no_acknowledgment() {
    for closed in [false, true] {
        let mut fixture = fixture();
        fixture.observer.pending = None;
        if closed {
            fixture.receiver.close();
        }
        let provider = ObservedAsyncModelProvider::new(
            &fixture.provider,
            &fixture.observer,
            ProviderRequestPurpose::Execution,
        );
        let mut call = Box::pin(tokio::task::unconstrained(
            provider.send_request_async(&fixture.request),
        ));
        let response = std::future::poll_fn(|cx| match call.as_mut().poll(cx) {
            std::task::Poll::Ready(result) => std::task::Poll::Ready(result.unwrap()),
            std::task::Poll::Pending => panic!("optional diagnostics acquired a required gate"),
        })
        .await;
        assert_eq!(response.usage.input_tokens, 17);
        assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 1);
    }
}

/// A completed original handoff remains valid, but cannot hide a lost consumer
/// from another call, including a future constructed before that loss.
#[tokio::test]
async fn all_purposes_acknowledged_then_closed_consumer_cannot_admit_next_call() {
    for purpose in [
        ProviderRequestPurpose::Execution,
        ProviderRequestPurpose::Routing,
        ProviderRequestPurpose::Auxiliary,
        ProviderRequestPurpose::Compaction,
        ProviderRequestPurpose::Memory,
    ] {
        for complete_first in [false, true] {
            let mut fixture = fixture();
            fixture.observer.claim_generation = None;
            let provider =
                ObservedAsyncModelProvider::new(&fixture.provider, &fixture.observer, purpose);
            if complete_first {
                let (response, delivered) = observe_and_acknowledge_for_test(
                    provider.send_request_async(&fixture.request),
                    &mut fixture.receiver,
                    &fixture.pending,
                )
                .await;
                assert_eq!(response.unwrap().usage.input_tokens, 17);
                assert_eq!(delivered.len(), 1);
                assert!(fixture.pending.is_empty());
            }
            let call = provider.send_request_async(&fixture.request);
            drop(fixture.receiver);
            assert!(call.await.is_err());
            assert!(provider.send_request_async(&fixture.request).await.is_err());
            assert_eq!(
                fixture.provider.calls.load(Ordering::SeqCst),
                usize::from(complete_first)
            );
            assert!(fixture.pending.is_empty());
        }
    }
}

/// Delivery loss must not replace the first provider failure with a generic
/// local error, while its known receipt remains available to retirement.
#[tokio::test]
async fn failed_delivery_keeps_original_provider_failure_and_expense() {
    let mut fixture = fixture();
    fixture.provider.fail = true;
    let receiver = std::sync::Mutex::new(Some(fixture.receiver));
    fixture.provider.on_send = Some(Box::new(move || {
        receiver.lock().unwrap().take();
    }));
    let provider = ObservedAsyncModelProvider::new(
        &fixture.provider,
        &fixture.observer,
        ProviderRequestPurpose::Compaction,
    );
    let error = provider
        .send_request_async(&fixture.request)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
    assert_eq!(error.message(), "fixture permission failure");
    assert_eq!(
        error.provider_failure_json(),
        Some(r#"{"error":{"code":"fixture_denied"}}"#)
    );
    let incurred = error.provider_incurred_usage();
    assert_eq!((incurred.input_tokens, incurred.output_tokens), (17, 3));
    assert_eq!(fixture.reported.lock().unwrap().known_observations, 1);
    assert_eq!(fixture.pending.len(), 1);
}
