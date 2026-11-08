//! Fake inference through the real common model-work operation. Runtime and
//! actor/storage fixtures share this boundary instead of fabricating accepted
//! history from a raw completion event. No publication proof is constructed here.

use crate::integrations::agent::provider::AsyncModelProvider;
use crate::runtime::{AgentCompactionEvent, RuntimeAgentCompactionTask, RuntimeSessionService};
use crate::{MezError, Result};
use mez_agent::{ModelRequest, ModelResponse, ProviderApiCompatibility};
use std::{future::Future, pin::Pin, sync::mpsc};

enum Inference<R> {
    Request(R, tokio::sync::oneshot::Sender<Result<ModelResponse>>),
    Finished(Result<ModelResponse>),
}

struct Provider {
    api: ProviderApiCompatibility,
    sender: mpsc::Sender<Inference<ModelRequest>>,
}

impl AsyncModelProvider for Provider {
    fn provider_id(&self) -> &str {
        "compaction-fixture"
    }
    fn api_compatibility(&self) -> ProviderApiCompatibility {
        self.api
    }
    fn send_request_async<'a>(
        &'a self,
        request: &'a ModelRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ModelResponse>> + Send + 'a>> {
        Box::pin(async move {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            self.sender
                .send(Inference::Request(request.clone(), sender))
                .map_err(|_| MezError::invalid_state("fixture inference owner disappeared"))?;
            receiver
                .await
                .map_err(|_| MezError::invalid_state("fixture inference reply disappeared"))?
        })
    }
}

/// Runs one real bounded wave, substituting only the provider's result. Source,
/// packing, output validity, private intermediates and typed proof remain owned
/// by production code. Each admitted request is returned for exact assertions.
pub(crate) fn execute_wave(
    service: &RuntimeSessionService,
    task: &RuntimeAgentCompactionTask,
    result: Result<ModelResponse>,
) -> (Result<ModelResponse>, Vec<ModelRequest>) {
    execute_with_inference(service, task, |_| result.clone())
}

/// Drives the common operation with inference on the caller's thread. A test
/// ModelProvider may own RefCell state and need not be Send or Sync. Only owned
/// requests and replies cross the worker boundary; its runtime is fully joined
/// before actor completion, including when called from an async test.
pub(crate) fn execute_with_inference(
    service: &RuntimeSessionService,
    task: &RuntimeAgentCompactionTask,
    mut infer: impl FnMut(&ModelRequest) -> Result<ModelResponse>,
) -> (Result<ModelResponse>, Vec<ModelRequest>) {
    let configured = service
        .provider_registry()
        .provider(&task.model_profile.provider)
        .unwrap();
    let api = mez_agent::resolve_provider_api(&configured.kind, configured.api.as_deref()).unwrap();
    let shape = task.compaction_request_shape.clone().unwrap_or_else(|| {
        (
            api,
            crate::runtime::runtime_effective_provider_options(configured, &task.model_profile),
            false,
        )
    });
    let source = task
        .replay_source
        .as_ref()
        .expect("original canonical source")
        .clone();
    let task = task.clone();
    let (sender, receiver) = mpsc::channel();
    let mut requests = Vec::new();
    let result = std::thread::scope(|scope| {
        let worker = scope.spawn(move || {
            let provider = Provider {
                api,
                sender: sender.clone(),
            };
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(source.execute_model_wave(&provider, &task, shape));
            sender.send(Inference::Finished(result)).unwrap();
        });
        let (result, panic) = pump_inference(&receiver, &mut infer, &mut requests);
        worker.join().unwrap();
        if let Some(panic) = panic {
            std::panic::resume_unwind(panic);
        }
        result
    });
    (result, requests)
}

type InferencePanic = Box<dyn std::any::Any + Send>;

/// A fake-provider assertion must still release all admitted siblings before
/// propagating its panic. The channel can already hold their reply senders;
/// unwinding straight into scoped-thread join would leave the worker waiting.
fn pump_inference<R>(
    receiver: &mpsc::Receiver<Inference<R>>,
    mut infer: impl FnMut(&R) -> Result<ModelResponse>,
    requests: &mut Vec<R>,
) -> (Result<ModelResponse>, Option<InferencePanic>) {
    let mut panic = None;
    loop {
        match receiver.recv().expect("common wave worker result") {
            Inference::Request(request, reply) => {
                let result = if panic.is_none() {
                    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| infer(&request)))
                    {
                        Ok(result) => result,
                        Err(payload) => {
                            panic = Some(payload);
                            Err(MezError::invalid_state("fixture inference panicked"))
                        }
                    }
                } else {
                    Err(MezError::invalid_state("fixture inference panicked"))
                };
                requests.push(request);
                reply.send(result).expect("common wave inference receiver");
            }
            Inference::Finished(result) => return (result, panic),
        }
    }
}

#[test]
fn compaction_fixture_inference_panic_drains_already_admitted_siblings() {
    let (sender, receiver) = mpsc::channel();
    std::thread::scope(|scope| {
        let worker = scope.spawn(move || {
            let mut replies = Vec::new();
            for _ in 0..2 {
                let (reply, pending) = tokio::sync::oneshot::channel();
                sender.send(Inference::Request((), reply)).unwrap();
                replies.push(pending);
            }
            for reply in replies {
                assert_eq!(
                    reply.blocking_recv().unwrap().unwrap_err().message(),
                    "fixture inference panicked"
                );
            }
            sender
                .send(Inference::Finished(Err(MezError::invalid_state(
                    "wave retired",
                ))))
                .unwrap();
        });
        let mut requests = Vec::new();
        let (result, panic) = pump_inference(
            &receiver,
            |_| panic!("fake inference assertion"),
            &mut requests,
        );
        worker.join().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(panic.is_some());
        assert_eq!(result.unwrap_err().message(), "wave retired");
    });
}

/// Uses the same canonical event identifiers as production provider workers,
/// rather than JSON-RPC error codes (which collapse transport into internal_error).
pub(crate) fn model_result_event(
    pane: &str,
    generation: u64,
    result: Result<ModelResponse>,
) -> AgentCompactionEvent {
    match result {
        Ok(response) => AgentCompactionEvent::Completed {
            pane_id: pane.into(),
            task_generation: generation,
            response: Box::new(response),
        },
        Err(error) => AgentCompactionEvent::Failed {
            pane_id: pane.into(),
            task_generation: generation,
            usage: error.provider_incurred_usage(),
            kind: mez_agent::ProviderErrorKind::from(error.kind())
                .as_str()
                .into(),
            message: error.message().into(),
            provider_failure_json: error.provider_failure_json().map(str::to_owned),
            provider_raw_text: None,
        },
    }
}

/// Read-only preview of selected model source, not the intentionally empty
/// queued template and not evidence that any request was dispatched. Actual
/// packing/dispatch tests use execute_wave and inspect its returned requests.
pub(crate) fn rendered_source(task: &RuntimeAgentCompactionTask) -> Result<String> {
    let plan = task
        .replay_source
        .as_ref()
        .ok_or_else(|| MezError::invalid_state("source preview has no canonical owner"))?
        .frozen_plan()
        .clone();
    Ok(
        mez_agent::epoch_compaction::provider::render_epoch_source(plan, 64 * 1024 * 1024)?
            .text()
            .to_owned(),
    )
}
