//! A native receipt descriptor must reject foreign or reused wire subjects
//! before the actual transport poll, not merely refuse their later accounting.

use super::*;

#[tokio::test]
async fn budget_wire_foreign_subject_is_refused_before_transport() {
    for foreign_model in [true, false] {
        let Case {
            mut service,
            dispatch,
            issued,
            factory,
            store,
            member,
            mut root,
        } = case(BudgetAttemptPurpose::Execution).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = OpenAiCompatibleChatCompletionsProvider::without_auth(Transport {
            usage: serde_json::json!({"prompt_tokens":11,"completion_tokens":3}),
            calls: calls.clone(),
        })
        .unwrap()
        .with_provider_id("local-chat")
        .unwrap();
        let mut request = crate::integrations::agent::context::assemble_model_request(
            &dispatch.model_profile,
            provider.api_compatibility(),
            &dispatch.turn,
            dispatch.context.durable(),
        )
        .unwrap();
        if foreign_model {
            request.model = "foreign".into();
        }
        let pending = PendingProviderWireObservations::default();
        let (sender, _receiver) = tokio::sync::mpsc::channel(1);
        let observer = ProviderWireRequestObserver::new(&member.conversation, "%1", sender)
            .with_pending_observations(pending.clone())
            .with_budget_receipts(factory.clone())
            .unwrap();
        let observed = ObservedAsyncModelProvider::new(
            &provider,
            &observer,
            if foreign_model {
                ProviderRequestPurpose::Execution
            } else {
                ProviderRequestPurpose::Memory
            },
        );
        let mut call = Box::pin(tokio::task::unconstrained(
            observed.send_request_async(&request),
        ));
        let refused = matches!(
            futures_util::poll!(&mut call),
            std::task::Poll::Ready(Err(_))
        );
        drop(call);
        let sends = calls.load(Ordering::SeqCst);
        let no_receipt = rows(&store, "budget_receipts") == 0;
        let exposure_retained = rows(&store, "budget_admissions") == 1;
        service.terminate_all_pane_processes().unwrap();
        drop((
            observer, provider, pending, factory, issued, dispatch, service,
        ));
        root.cleanup();
        assert!(refused, "foreign subject reached observation delivery");
        assert_eq!(sends, 0, "foreign subject reached physical transport");
        assert!(no_receipt && exposure_retained);
    }
}

#[tokio::test]
async fn budget_wire_descriptor_clone_cannot_send_a_second_exchange() {
    let Case {
        mut service,
        dispatch,
        issued,
        factory,
        store,
        member,
        mut root,
    } = case(BudgetAttemptPurpose::Execution).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = OpenAiCompatibleChatCompletionsProvider::without_auth(Transport {
        usage: serde_json::json!({"prompt_tokens":11,"completion_tokens":3}),
        calls: calls.clone(),
    })
    .unwrap()
    .with_provider_id("local-chat")
    .unwrap();
    let request = crate::integrations::agent::context::assemble_model_request(
        &dispatch.model_profile,
        provider.api_compatibility(),
        &dispatch.turn,
        dispatch.context.durable(),
    )
    .unwrap();
    let mut accepted_first = false;
    let mut refused_second = false;
    for first in [true, false] {
        // A new journal and factory must not reset the native descriptor's
        // subject. This is not merely the original factory's local cache.
        let selected = if first {
            factory.clone()
        } else {
            Arc::new(BudgetObservationReceipts::new(issued.registered().clone()))
        };
        let pending = PendingProviderWireObservations::default();
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        let observer = ProviderWireRequestObserver::new(&member.conversation, "%1", sender)
            .with_pending_observations(pending.clone())
            .with_budget_receipts(selected)
            .unwrap();
        let observed = ObservedAsyncModelProvider::new(
            &provider,
            &observer,
            ProviderRequestPurpose::Execution,
        );
        let mut call = Box::pin(tokio::task::unconstrained(
            observed.send_request_async(&request),
        ));
        let polled = futures_util::poll!(&mut call);
        if first {
            if let Ok(observation) = receiver.try_recv() {
                pending.checkpoint(&observation).await.unwrap();
                pending.acknowledge(&observation.request_id);
                accepted_first = polled.is_pending()
                    && matches!(
                        futures_util::poll!(&mut call),
                        std::task::Poll::Ready(Ok(_))
                    );
            }
        } else {
            refused_second = matches!(polled, std::task::Poll::Ready(Err(_)));
        }
        drop(call);
    }
    let sends = calls.load(Ordering::SeqCst);
    let receipts = rows(&store, "budget_receipts");
    let exposure = rows(&store, "budget_admissions");
    let known = store.budget_account(&member.root).unwrap().known;
    service.terminate_all_pane_processes().unwrap();
    drop((provider, factory, issued, dispatch, service));
    root.cleanup();
    assert!(accepted_first && refused_second);
    assert_eq!(sends, 1);
    assert_eq!(receipts, 1);
    assert_eq!(
        exposure, 1,
        "subject binding never releases issued exposure"
    );
    assert_eq!(
        known,
        UsageTotals {
            input: 11,
            output: 3
        }
    );
}
