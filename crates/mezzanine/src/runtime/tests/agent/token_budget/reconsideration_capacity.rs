//! Actor readiness capacity using genuine native-held captures of one actual
//! title claim. This is metadata pressure, not thousands of physical producers.
//! A refused candidate must not secretly retain its waiter or evict originals.

use crate::runtime::{RuntimeBudgetAttemptAdmission, RuntimeBudgetHeldAttempt};
use crate::storage::token_usage::budget::receipts::BudgetAttemptPurpose;
use std::sync::Arc;

#[tokio::test]
async fn runtime_budget_reconsideration_capacity_refuses_without_retention_and_recovers() {
    Box::pin(qualify()).await;
}

async fn qualify() {
    const CAPACITY: usize = 4096;
    let (mut scene, _, conversation) = super::auxiliary::setup();
    let mut futures: Vec<Option<Arc<()>>> = (0..=CAPACITY).map(|_| Some(Arc::new(()))).collect();
    let title = super::title_capacity::parked_title(
        &mut scene,
        &conversation,
        futures[0].as_ref().unwrap(),
    )
    .await;
    let owner = title.0.budget_owner.as_ref().unwrap();
    let mut admissions = vec![title.1.clone()];
    let mut captures: Vec<RuntimeBudgetHeldAttempt> = vec![title.2.clone()];
    for _ in 1..=CAPACITY {
        let admission = owner
            .prepare_attempt(
                mez_agent::ModelTokenUsageKey::new("openai", "gpt-test"),
                BudgetAttemptPurpose::Title,
            )
            .unwrap()
            .admission_owner();
        assert!(matches!(
            admission.claim().await.unwrap(),
            RuntimeBudgetAttemptAdmission::Held(_)
        ));
        captures.push(admission.capture_held().await.unwrap());
        admissions.push(admission);
    }
    let service = std::mem::replace(&mut scene.service, super::test_runtime_service());
    let (handle, actor) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
            .unwrap();
    let client = async {
        let selected_window = Arc::new(());
        let mut checks = vec![
            handle
                .select_budget_reconsideration(
                    captures[0].clone(),
                    Arc::downgrade(futures[0].as_ref().unwrap()),
                    Arc::downgrade(&selected_window),
                )
                .await
                .unwrap(),
        ];
        // Every fitting candidate enrolls behind an exact selected original.
        // Each caller has a distinct window; sharing one would be a duplicate
        // observation of that window, not independent selection authority.
        for index in 1..CAPACITY {
            let window = Arc::new(());
            checks.push(
                !handle
                    .select_budget_reconsideration(
                        captures[index].clone(),
                        Arc::downgrade(futures[index].as_ref().unwrap()),
                        Arc::downgrade(&window),
                    )
                    .await
                    .unwrap(),
            );
        }
        let other_future = Arc::new(());
        let window = Arc::new(());
        // A retained capture refuses a different lifetime witness. At the
        // exact last fitting entry this proves enrollment, not just false
        // because another selected window temporarily owns the root.
        checks.push(
            handle
                .select_budget_reconsideration(
                    captures[CAPACITY - 1].clone(),
                    Arc::downgrade(&other_future),
                    Arc::downgrade(&window),
                )
                .await
                .is_err(),
        );
        for future in [futures[CAPACITY].as_ref().unwrap(), &other_future] {
            checks.push(matches!(
                handle
                    .select_budget_reconsideration(
                        captures[CAPACITY].clone(),
                        Arc::downgrade(future),
                        Arc::downgrade(&window),
                    )
                    .await,
                Ok(false)
            ));
        }
        // Only this exact original retires; the title claim's own park witness
        // (entry zero) remains alive, so other captures stay eligible.
        futures[1].take();
        checks.push(matches!(
            handle
                .select_budget_reconsideration(
                    captures[CAPACITY].clone(),
                    Arc::downgrade(futures[CAPACITY].as_ref().unwrap()),
                    Arc::downgrade(&window),
                )
                .await,
            Ok(false)
        ));
        checks.push(
            handle
                .select_budget_reconsideration(
                    captures[CAPACITY].clone(),
                    Arc::downgrade(&other_future),
                    Arc::downgrade(&window),
                )
                .await
                .is_err(),
        );
        checks.push(
            handle
                .select_budget_reconsideration(
                    captures[CAPACITY - 1].clone(),
                    Arc::downgrade(&other_future),
                    Arc::downgrade(&window),
                )
                .await
                .is_err(),
        );
        for future in &mut futures[2..CAPACITY] {
            future.take();
        }
        drop(selected_window);
        checks.push(
            handle
                .select_budget_reconsideration(
                    captures[0].clone(),
                    Arc::downgrade(futures[0].as_ref().unwrap()),
                    Arc::downgrade(&window),
                )
                .await
                .unwrap(),
        );
        drop(window);
        let window = Arc::new(());
        checks.push(
            handle
                .select_budget_reconsideration(
                    captures[CAPACITY].clone(),
                    Arc::downgrade(futures[CAPACITY].as_ref().unwrap()),
                    Arc::downgrade(&window),
                )
                .await
                .unwrap(),
        );
        handle.shutdown().await.unwrap();
        checks
    };
    let (checks, exit) = tokio::join!(client, actor.run());
    scene.service = exit.service;
    drop((handle, captures, admissions, futures, title));
    drop(scene); // Join original workers and remove the exact root before oracles.
    assert!(
        checks.iter().all(|check| *check),
        "readiness capacity or original retention changed: {checks:?}"
    );
}
