//! Exact current remember claim plus a positively observed native hold. These
//! tests do not synthesize an ordinary turn or grant provider permission.

use crate::runtime::{RuntimeBudgetAttemptAdmission, RuntimeBudgetAuxiliarySource};
use crate::storage::token_usage::budget::receipts::BudgetAttemptPurpose;

#[tokio::test]
async fn runtime_budget_remember_wait_fences_epoch_waiter_and_retired_claim() {
    let (mut scene, primary, _) = super::auxiliary::setup();
    scene
        .service
        .execute_agent_shell_command(&primary, "/remember Preserve exact wait custody.")
        .unwrap();
    let observation = scene
        .service
        .pending_agent_remember_observation_id("%1")
        .unwrap()
        .to_owned();
    super::auxiliary::prepare(
        &mut scene,
        &RuntimeBudgetAuxiliarySource::Remember {
            pane: "%1".into(),
            observation: observation.clone(),
        },
    )
    .await
    .unwrap();
    let dispatch = scene
        .service
        .claim_agent_remember_task("%1", &observation)
        .unwrap()
        .unwrap();
    let work = dispatch
        .budget_owner
        .as_ref()
        .unwrap()
        .prepare_attempt(
            mez_agent::ModelTokenUsageKey::new("openai", "gpt-test"),
            BudgetAttemptPurpose::Memory,
        )
        .unwrap();
    let admission = work.admission_owner();
    assert!(matches!(
        admission.claim().await.unwrap(),
        RuntimeBudgetAttemptAdmission::Held(_)
    ));
    let first = admission.capture_held().await.unwrap();
    let waiter = std::sync::Arc::new(());
    let other = std::sync::Arc::new(());
    assert!(
        scene
            .service
            .park_budget_auxiliary_claim(first.clone(), std::sync::Arc::downgrade(&waiter), 1)
            .unwrap()
    );
    assert!(
        scene
            .service
            .park_budget_auxiliary_claim(first.clone(), std::sync::Arc::downgrade(&waiter), 9)
            .unwrap()
    );
    assert!(scene.service.budget_remember_claim_is_quiescent("%1"));
    assert!(
        scene
            .service
            .park_budget_auxiliary_claim(first.clone(), std::sync::Arc::downgrade(&other), 9)
            .is_err()
    );
    assert!(
        scene
            .service
            .resume_budget_auxiliary_claim(&first, 10)
            .unwrap()
    );
    assert!(
        scene
            .service
            .resume_budget_auxiliary_claim(&first, 11)
            .unwrap()
    );
    assert!(!scene.service.budget_remember_claim_is_quiescent("%1"));
    assert!(
        scene
            .service
            .park_budget_auxiliary_claim(first.clone(), std::sync::Arc::downgrade(&waiter), 12)
            .is_err()
    );

    // Fresh reconsideration revokes the first capture, even if the actual
    // policy still holds. Only the new positive epoch can park again.
    assert!(matches!(
        admission.retry_held().await.unwrap(),
        RuntimeBudgetAttemptAdmission::Held(_)
    ));
    assert!(
        scene
            .service
            .resume_budget_auxiliary_claim(&first, 13)
            .is_err()
    );
    let second = admission.capture_held().await.unwrap();
    assert!(
        scene
            .service
            .park_budget_auxiliary_claim(second.clone(), std::sync::Arc::downgrade(&waiter), 14)
            .unwrap()
    );
    assert!(scene.service.budget_remember_claim_is_quiescent("%1"));
    drop(waiter);
    assert!(!scene.service.budget_remember_claim_is_quiescent("%1"));
    assert!(
        scene
            .service
            .resume_budget_auxiliary_claim(&second, 15)
            .is_err()
    );
    scene
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    assert!(
        scene
            .service
            .park_budget_auxiliary_claim(second.clone(), std::sync::Arc::downgrade(&other), 16)
            .is_err()
    );
    assert!(
        scene
            .service
            .resume_budget_auxiliary_claim(&second, 17)
            .is_err()
    );
    assert!(scene.service.agent_turn_ledger().turns().is_empty());
}
