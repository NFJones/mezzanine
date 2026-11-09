//! Exact issued network results after turn-local provider failure refusal.
//! The production network executor uses a counted offline transport; no real
//! HTTP request is made. Actor admission, attempts and completion custody are
//! genuine. Error injection models lost local result handling after the call.

use super::*;
use crate::integrations::http::AsyncProviderHttpTransport;
use std::sync::atomic::{AtomicUsize, Ordering};

struct CountedTransport(AtomicUsize);

impl AsyncProviderHttpTransport for CountedTransport {
    fn send_async<'a>(
        &'a self,
        _request: &'a mez_agent::ProviderHttpRequest,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = mez_agent::ProviderHttpResult<mez_agent::ProviderHttpResponse>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(mez_agent::ProviderHttpResponse {
                status_code: 200,
                headers: Default::default(),
                body: "exact external result after refusal".into(),
            })
        })
    }
}

#[tokio::test]
async fn runtime_provider_failure_refusal_retains_issued_network_outcome() {
    for returned_error in [false, true] {
        let mut fixture = CompletionFixture::fetch().await;
        let transport = CountedTransport(AtomicUsize::new(0));
        let actual = crate::integrations::agent::network::execute_network_action_with_transport_async_with_progress(
            &fixture.turn, &fixture.action, &transport, &mut |_| {},
        ).await.unwrap();
        assert_eq!(transport.0.load(Ordering::SeqCst), 1);
        assert_eq!(actual.status, ActionStatus::Succeeded);
        let execution = fixture.retained();
        let context = fixture.service.agent_turn_contexts()[&fixture.turn.turn_id].clone();
        let cause = MezError::from(mez_agent::AgentContextError::new(
            "request context refused after external issue",
        ));
        fixture
            .service
            .fail_configured_agent_provider_task(&fixture.turn.turn_id, &cause)
            .unwrap();
        let result = if returned_error {
            Err(MezError::invalid_state(
                "local result handling failed after the issued call",
            ))
        } else {
            Ok(actual.clone())
        };
        let outcome = || crate::runtime::RuntimeApprovedExternalActionOutcome {
            turn_id: fixture.turn.turn_id.clone(),
            action_id: fixture.action.id.clone(),
            attempt: fixture.attempt.clone(),
            result: result.clone(),
            mcp_transport: None,
        };
        let mut foreign = outcome();
        foreign.attempt.push_str("-foreign");
        assert!(
            !fixture
                .service
                .complete_approved_external_action(foreign)
                .unwrap()
        );
        let original = outcome();
        let duplicate = outcome();
        assert!(
            fixture
                .service
                .complete_approved_external_action(original)
                .unwrap(),
            "new raw evidence must remain owned after refusal"
        );
        let retained = fixture
            .service
            .retained_external_outcome_for_tests(
                &fixture.turn.turn_id,
                &fixture.action.id,
                &fixture.attempt,
            )
            .unwrap();
        match (retained, &result) {
            (Ok(retained), Ok(expected)) => assert_eq!(&retained, expected),
            (Err(retained), Err(expected)) => {
                assert_eq!(retained.kind(), expected.kind());
                assert_eq!(retained.message(), expected.message());
            }
            _ => panic!("changed original result classification"),
        }
        assert!(
            !fixture
                .service
                .complete_approved_external_action(duplicate)
                .unwrap()
        );
        assert!(
            fixture
                .service
                .claim_approved_external_action(&fixture.turn.turn_id, &fixture.action.id)
                .unwrap()
                .is_none()
        );
        assert!(
            fixture
                .service
                .claim_approved_external_action(&fixture.turn.turn_id, "pending-sibling")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            fixture.retained(),
            execution,
            "raw custody cannot adopt a new canonical execution"
        );
        assert_eq!(
            fixture.service.agent_turn_contexts()[&fixture.turn.turn_id],
            context
        );
        assert_eq!(
            fixture
                .service
                .agent_turn_ledger()
                .turn(&fixture.turn.turn_id)
                .unwrap()
                .state,
            AgentTurnState::Blocked
        );
        assert_eq!(
            transport.0.load(Ordering::SeqCst),
            1,
            "duplicate ingress cannot replay external work"
        );
    }
}
