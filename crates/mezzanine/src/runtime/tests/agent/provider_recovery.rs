//! Runtime fake-provider recovery adapter parity and budget regressions.
//!
//! Fake providers execute synchronously, but must choose the same recovery as
//! production without sleeping through real backoff or replaying accepted work.

use super::*;

/// Both fake-provider execution adapters retry a transient envelope under the
/// configured budget and settle the eventual accepted response exactly once.
#[tokio::test]
async fn runtime_test_provider_adapters_retry_transport_with_shared_budget() {
    for asynchronous in [false, true] {
        let mut service = recovery_service();
        let provider = TransportFixture {
            failures: 1,
            requests: RefCell::new(Vec::new()),
        };
        let profile = runtime_model_profile("runtime-batch", "test");
        let execution = if asynchronous {
            service
                .execute_agent_turn_with_provider_async("turn-1", &provider, profile)
                .await
        } else {
            service.execute_agent_turn_with_provider("turn-1", &provider, profile)
        }
        .unwrap();
        assert_eq!(execution.terminal_state, AgentTurnState::Completed);
        let requests = provider.requests.borrow();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[0], requests[1],
            "transport retry must not rewrite context"
        );
        assert!(!service.agent_turn_is_running("turn-1"));
        assert!(service.pending_agent_provider_tasks().is_empty());
    }
}

/// A finite configured retry budget bounds both fake-provider loops rather
/// than restarting retries with each canonical lower-runner invocation.
#[tokio::test]
async fn runtime_test_provider_adapters_stop_at_transport_budget() {
    for asynchronous in [false, true] {
        let mut service = recovery_service();
        let provider = TransportFixture {
            failures: usize::MAX,
            requests: RefCell::new(Vec::new()),
        };
        let profile = runtime_model_profile("runtime-batch", "test");
        let result = if asynchronous {
            service
                .execute_agent_turn_with_provider_async("turn-1", &provider, profile)
                .await
        } else {
            service.execute_agent_turn_with_provider("turn-1", &provider, profile)
        };
        assert!(result.is_err());
        assert_eq!(provider.requests.borrow().len(), 3);
        assert_eq!(
            service.agent_turn_ledger().turn("turn-1").unwrap().state,
            AgentTurnState::Failed
        );
    }
}

/// Creates a running turn with a small finite production retry policy.
fn recovery_service() -> RuntimeSessionService {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "recovery-parity".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text:
                "[agents]\nprovider_error_retry_limit = 2\nprovider_error_retry_unlimited = false\n"
                    .to_string(),
        }])
        .unwrap();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .start_agent_prompt_turn("%1", "recover without replay")
        .unwrap();
    service.remove_pending_agent_provider_task("turn-1");
    service
}

/// Records requests and fails the selected prefix with a retryable envelope.
struct TransportFixture {
    failures: usize,
    requests: RefCell<Vec<mez_agent::ModelRequest>>,
}

impl ModelProvider for TransportFixture {
    fn provider_id(&self) -> &str {
        "runtime-batch"
    }

    fn send_request(&self, request: &mez_agent::ModelRequest) -> Result<mez_agent::ModelResponse> {
        let mut requests = self.requests.borrow_mut();
        requests.push(request.clone());
        if requests.len() <= self.failures {
            return Err(
                MezError::invalid_state("provider HTTP request failed: rate limited")
                    .with_provider_failure_json(r#"{"status_code":429}"#),
            );
        }
        Ok(mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "accepted once".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(runtime_complete_batch(request.turn_id.clone())),
            provider_transcript_events: Vec::new(),
        })
    }
}
