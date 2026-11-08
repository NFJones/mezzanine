//! Common graph through genuine actor admission, wire accounting and durable
//! publication. Only model inference is replaced; source and publication proofs
//! come from the actual command/provider recovery and persistence owners.

use super::auxiliary_actor_faults::{Family, queued_fixture};
use super::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Clone, Copy)]
enum First {
    Valid,
    Empty,
    Cutoff,
    Transport,
}

struct Provider {
    first: First,
    calls: AtomicUsize,
    requests: Mutex<Vec<ModelRequest>>,
    summary: String,
}

impl AsyncModelProvider for Provider {
    fn provider_id(&self) -> &str {
        "local-chat"
    }
    fn api_compatibility(&self) -> mez_agent::ProviderApiCompatibility {
        mez_agent::ProviderApiCompatibility::OpenAiChatCompletions
    }
    fn send_request_async<'a>(
        &'a self,
        request: &'a ModelRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ModelResponse>> + Send + 'a>> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(request.clone());
            let index = self.calls.fetch_add(1, Ordering::SeqCst);
            let usage = mez_agent::ModelTokenUsage {
                input_tokens: 17,
                output_tokens: 3,
                ..Default::default()
            };
            if index == 0 && matches!(self.first, First::Cutoff | First::Transport) {
                let payload = if matches!(self.first, First::Cutoff) {
                    r#"{"error":{"code":"max_tokens"}}"#
                } else {
                    r#"{"status_code":503,"error":{"code":"temporarily_unavailable"}}"#
                };
                return Err(MezError::invalid_state("fixture provider failure")
                    .with_provider_failure_json(payload)
                    .with_provider_incurred_usage(usage));
            }
            Ok(ModelResponse {
                provider: "local-chat".into(),
                model: "test".into(),
                raw_text: if index == 0 && matches!(self.first, First::Empty) {
                    String::new()
                } else {
                    self.summary.clone()
                },
                usage,
                latest_request_usage: None,
                quota_usage: Vec::new(),
                action_batch: None,
                provider_transcript_events: Vec::new(),
            })
        })
    }
}

#[tokio::test]
async fn compaction_wave_fitting_and_nonreducing_finals_publish_once() {
    for summary in [
        "usable continuation".into(),
        format!("{}END", "usable continuation ".repeat(6000)),
    ] {
        qualify(First::Valid, summary, 1).await;
    }
}

#[tokio::test]
async fn compaction_wave_invalid_output_repairs_cross_actor_admission() {
    for first in [First::Empty, First::Cutoff] {
        qualify(first, "repaired usable continuation".into(), 2).await;
    }
}

#[tokio::test]
async fn compaction_wave_transport_retry_preserves_original_request() {
    qualify(First::Transport, "retry accepted continuation".into(), 2).await;
}

/// Drives the actual issued generation and exact wire acknowledgments without
/// scheduling sleeps. Every intermediate wave remains unpublished; only the
/// completed typed graph authorizes the retained persistence attempt.
async fn qualify(first: First, summary: String, expected_calls: usize) {
    let (_root, mut service, _primary, conversation) = queued_fixture(Family::Compaction);
    let provider = Provider {
        first,
        calls: AtomicUsize::new(0),
        requests: Mutex::new(Vec::new()),
        summary: summary.clone(),
    };
    let mut original_source = None;
    let mut generations = Vec::new();
    for wave in 0..expected_calls {
        let generation = service
            .pending_agent_compaction_task_generation("%1")
            .unwrap();
        generations.push(generation);
        if let Some(delay) = service.agent_compaction_retry_delay("%1", generation) {
            assert!(delay > 0);
            // Use the exact production retry-timer reducer. Stale timers cannot
            // release another wave, and ordinary claim policy still applies.
            assert!(!service.release_agent_compaction_retry("%1", generation - 1));
            assert!(service.release_agent_compaction_retry("%1", generation));
        }
        let dispatch = service
            .claim_agent_compaction_task("%1", generation)
            .unwrap()
            .unwrap();
        let source = dispatch.task.replay_source.as_ref().unwrap().clone();
        if let Some(original) = &original_source {
            assert!(Arc::ptr_eq(original, &source));
        } else {
            original_source = Some(source.clone());
        }
        let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
        let pending = PendingProviderWireObservations::default();
        let observer = ProviderWireRequestObserver::new(&conversation, "%1", sender)
            .with_claim_generation(generation)
            .with_pending_observations(pending.clone());
        let observed = ObservedAsyncModelProvider::new(
            &provider,
            &observer,
            ProviderRequestPurpose::Compaction,
        );
        let shape = dispatch.task.compaction_request_shape.clone().unwrap();
        let call = source.execute_model_wave(&observed, &dispatch.task, shape);
        tokio::pin!(call);
        let result = loop {
            tokio::select! {
                result = &mut call => break result,
                observation = receiver.recv() => {
                    let observation = observation.unwrap();
                    // The worker retains its session lock while awaiting this
                    // actual wire receipt's actor ACK. Inspection must remain
                    // nonblocking and cannot confuse claimed work with delivery.
                    let status = service.runtime_compaction_status("%1").unwrap();
                    assert_eq!(status.phase, "inference");
                    assert_eq!(status.dispatch_generation, generation);
                    assert_eq!(status.model_calls, provider.calls.load(Ordering::SeqCst) as u64);
                    assert_eq!(status.serial_depth, 1);
                    let id = observation.request_id.clone();
                    service.apply_provider_wire_request_observation(observation).unwrap();
                    pending.acknowledge(&id);
                }
            }
        };
        assert!(pending.is_empty());
        let calls = provider.calls.load(Ordering::SeqCst) as u64;
        assert_eq!(source.model_progress().model_calls, calls);
        assert_eq!(source.model_progress().serial_depth, 1);
        let model = mez_agent::ModelTokenUsageKey::new("local-chat", "test");
        let checkpoint = service.agent_token_usage_for_conversation(&conversation);
        assert_eq!(
            (
                checkpoint[&model].input_tokens,
                checkpoint[&model].output_tokens
            ),
            (calls * 17, calls * 3)
        );
        let (event, _) = compaction_worker_event("%1".into(), generation, Ok(result));
        let RuntimeEvent::AgentCompaction(event) = event else {
            panic!("compaction event")
        };
        service.apply_agent_compaction_transition(event).unwrap();
        if wave + 1 < expected_calls {
            assert!(service.take_replay_epoch_attempt_for_tests().is_none());
            assert!(service.pending_agent_provider_tasks().is_empty());
            assert!(service.agent_is_compacting("%1"));
        }
    }
    assert_eq!(provider.calls.load(Ordering::SeqCst), expected_calls);
    if matches!(first, First::Transport) {
        let requests = provider.requests.lock().unwrap();
        assert_eq!(
            requests[0], requests[1],
            "transport retry must keep exact logical input"
        );
    }
    let attempt = service
        .take_replay_epoch_attempt_for_tests()
        .expect("one typed final admits publication");
    assert!(service.take_replay_epoch_attempt_for_tests().is_none());
    let completion = attempt.execute();
    service
        .apply_replay_epoch_completion(completion.clone())
        .unwrap();
    service.apply_replay_epoch_completion(completion).unwrap();
    assert!(!service.agent_is_compacting("%1"));
    assert!(service.pending_agent_compaction_tasks().is_empty());
    assert_eq!(
        service.pending_agent_provider_tasks().len(),
        1,
        "one ordinary continuation"
    );
    let turn = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .running_turn_id
        .as_ref()
        .unwrap();
    assert!(
        service.agent_turn_contexts()[turn]
            .chronology()
            .iter()
            .any(|event| event.block().content == summary)
    );
    let latest = service.agent_token_usage_for_conversation(&conversation);
    let model = mez_agent::ModelTokenUsageKey::new("local-chat", "test");
    assert_eq!(
        (latest[&model].input_tokens, latest[&model].output_tokens),
        ((expected_calls as u64) * 17, (expected_calls as u64) * 3)
    );
    assert!(generations.windows(2).all(|pair| pair[0] < pair[1]));
    service.terminate_all_pane_processes().unwrap();
}
