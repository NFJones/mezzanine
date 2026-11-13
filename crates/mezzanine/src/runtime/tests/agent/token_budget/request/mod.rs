//! Actual runtime preparation/claim to frozen worker bookkeeping ports. Usage
//! here is synthetic normalized evidence, not physical provider admission proof.

use super::*;
use crate::runtime::agent::token_budget::request::RuntimeBudgetReceiptWork;
use crate::storage::token_usage::budget::receipts::BudgetAttemptPurpose;
use crate::storage::token_usage::budget::reporting::BudgetReportingObservation;
use mez_agent::token_budget::{UsageSnapshot, UsageTotals};
use mez_agent::{ModelTokenUsage, ModelTokenUsageKey};

mod admission_work;
mod receipt_commit;

async fn issued(scene: &mut Scene) -> crate::runtime::RuntimeAgentProviderDispatch {
    let turn = scene.start();
    let work = scene
        .service
        .prepare_agent_provider_work(&turn.turn_id)
        .unwrap();
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(work).await;
    scene
        .service
        .apply_agent_provider_preparation(outcome)
        .unwrap();
    scene
        .service
        .claim_configured_agent_provider_task(
            &AgentId::opaque(turn.agent_id).unwrap(),
            &turn.turn_id,
        )
        .unwrap()
        .unwrap()
}

async fn settle(
    work: RuntimeBudgetReceiptWork,
) -> Result<crate::storage::token_usage::budget::receipts::BudgetReceiptOutcome> {
    work.commit_owner().commit().await
}

/// Real runtime frozen owner -> worker grant -> receipt transaction, with no
/// physical HTTP or claim of provider-service enforcement in this staged port.
#[tokio::test]
async fn runtime_budget_native_authorization_is_one_use_and_preserves_original_receipt() {
    use crate::runtime::RuntimeBudgetAttemptAdmission;
    use crate::storage::token_usage::budget::admission::BudgetAdmissionHold;
    use crate::storage::token_usage::budget::policy_projection::{
        BudgetConfigPolicy, BudgetPolicyPublication,
    };
    use mez_agent::token_budget::{BudgetLimits, BudgetReceiptId, TokenLimit};
    let mut scene = Scene::new();
    let mut dispatch = issued(&mut scene).await;
    let owner = dispatch.budget_owner.take().unwrap();
    let member = owner.member.clone();
    let work = owner
        .prepare_attempt(
            ModelTokenUsageKey::new("openai", "gpt-test"),
            BudgetAttemptPurpose::Execution,
        )
        .unwrap();
    let pending = work.clone();
    assert!(matches!(
        tokio::task::spawn_blocking(move || pending.authorize())
            .await
            .unwrap()
            .unwrap(),
        RuntimeBudgetAttemptAdmission::Held(BudgetAdmissionHold::PolicyUnavailable)
    ));
    let op = BudgetPolicyPublication {
        namespace: member.namespace.clone(),
        id: BudgetReceiptId::new("test-policy").unwrap(),
        expected_revision: 0,
        previous_digest: "a".repeat(64),
        proposed: BudgetConfigPolicy {
            source: "primary".into(),
            digest: "b".repeat(64),
            limits: BudgetLimits {
                input: TokenLimit::Limited(20),
                output: TokenLimit::Unlimited,
            },
        },
        supersedes: None,
    };
    let store = scene.store.clone();
    tokio::task::spawn_blocking(move || {
        store.begin_budget_policy_publication(&op).unwrap();
        store
            .reconcile_budget_policy_publication(&op, &op.proposed.digest)
            .unwrap();
    })
    .await
    .unwrap();
    let check = work.clone();
    let RuntimeBudgetAttemptAdmission::Authorized(permit) =
        tokio::task::spawn_blocking(move || check.authorize())
            .await
            .unwrap()
            .unwrap()
    else {
        panic!("expected new native permit");
    };
    let admitted = tokio::task::spawn_blocking(move || permit.consume())
        .await
        .unwrap()
        .unwrap();
    let original = admitted.outcome().clone();
    let check = work.clone();
    assert_eq!(
        tokio::task::spawn_blocking(move || check.reconcile_admission())
            .await
            .unwrap()
            .unwrap(),
        Some(original.clone())
    );
    let check = work.clone();
    let RuntimeBudgetAttemptAdmission::PreviouslyAuthorized(replayed) =
        tokio::task::spawn_blocking(move || check.authorize())
            .await
            .unwrap()
            .unwrap()
    else {
        panic!("original authorization must never mint another permit");
    };
    assert_eq!(replayed, original);
    let next = owner
        .prepare_attempt(
            ModelTokenUsageKey::new("openai", "gpt-test"),
            BudgetAttemptPurpose::Execution,
        )
        .unwrap();
    let check = next.clone();
    assert!(matches!(
        tokio::task::spawn_blocking(move || check.authorize())
            .await
            .unwrap()
            .unwrap(),
        RuntimeBudgetAttemptAdmission::Held(BudgetAdmissionHold::OutstandingExposure)
    ));
    let receipt = admitted
        .registered()
        .freeze_receipt(
            UsageSnapshot {
                input: Some(11),
                output: Some(3),
                terminal: true,
            },
            None,
        )
        .unwrap();
    let before = settle(receipt.clone()).await.unwrap();
    assert_eq!(
        before.known,
        UsageTotals {
            input: 11,
            output: 3
        }
    );
    assert_eq!(before, settle(receipt).await.unwrap());
    drop((admitted, dispatch, owner));
    let check = next.clone();
    assert!(matches!(
        tokio::task::spawn_blocking(move || check.authorize())
            .await
            .unwrap()
            .unwrap(),
        RuntimeBudgetAttemptAdmission::Held(BudgetAdmissionHold::OutstandingExposure)
    ));
    // No naked terminal receipt or local object drop grants slot retirement.
    assert_eq!(
        scene.store.budget_account(&member.root).unwrap().known,
        before.known
    );
    drop((work, next));
}

/// Late original usage must neither sample a replacement pane/store nor release
/// its native holder merely because the actor dropped its own membership entry.
#[tokio::test]
async fn runtime_budget_request_receipt_keeps_original_store_and_custody_after_rebind() {
    let mut scene = Scene::new();
    let mut dispatch = issued(&mut scene).await;
    let owner = dispatch.budget_owner.take().unwrap();
    let member = owner.member.clone();
    let attempt = owner
        .prepare_attempt(
            ModelTokenUsageKey::new(
                &dispatch.model_profile.provider,
                &dispatch.model_profile.model,
            ),
            BudgetAttemptPurpose::Execution,
        )
        .unwrap();
    let check = attempt.clone();
    assert!(
        tokio::task::spawn_blocking(move || check.reconcile_registration())
            .await
            .unwrap()
            .unwrap()
            .is_none()
    );
    let registration = attempt.clone();
    let registered = tokio::task::spawn_blocking(move || registration.execute())
        .await
        .unwrap()
        .unwrap();
    // Lose the first ACK value, then recover its sealed descriptor by query.
    drop((registered, owner, dispatch));
    let check = attempt.clone();
    let registered = tokio::task::spawn_blocking(move || check.reconcile_registration())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let report = registered
        .freeze_receipt(
            UsageSnapshot {
                input: Some(11),
                output: Some(3),
                terminal: true,
            },
            Some(BudgetReportingObservation {
                observed_at_unix_seconds: 123,
                usage: ModelTokenUsage {
                    input_tokens: 11,
                    output_tokens: 3,
                    ..Default::default()
                },
            }),
        )
        .unwrap();
    let native = scene
        .service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .native_message_incarnation
        .clone();
    scene.service.stop_agent_turn_for_pane("%1").unwrap();
    scene
        .service
        .agent_shell_store_mut()
        .bind_conversation("%1", "replacement", 0)
        .unwrap();
    scene
        .service
        .retire_runtime_budget_departure("%1", &member.conversation, &native);
    let replacement = TokenUsageStore::new(scene._root.join("replacement.sqlite"));
    replacement.initialize(1).unwrap();
    scene.service.set_token_usage_store(replacement.clone());
    let connection = rusqlite::Connection::open(scene.store.path()).unwrap();
    connection.execute_batch("CREATE TRIGGER refuse_request_receipt BEFORE INSERT ON budget_receipts BEGIN SELECT RAISE(ABORT,'fixture refusal'); END;").unwrap();
    assert!(settle(report.clone()).await.is_err());
    let check = report.clone();
    assert_eq!(
        tokio::task::spawn_blocking(move || check.reconcile())
            .await
            .unwrap()
            .unwrap(),
        None
    );
    assert_eq!(
        scene.store.budget_account(&member.root).unwrap().known,
        UsageTotals::default()
    );
    connection
        .execute_batch("DROP TRIGGER refuse_request_receipt;")
        .unwrap();
    assert!(settle(report.clone()).await.is_err());
    let acknowledged = {
        let receipt = report.commit_owner();
        assert!(receipt.reconcile().await.unwrap().is_none());
        receipt.retry_after_absence().await.unwrap()
    };
    let check = report.clone();
    assert_eq!(
        tokio::task::spawn_blocking(move || check.reconcile())
            .await
            .unwrap()
            .unwrap(),
        Some(acknowledged.clone())
    );
    assert_eq!(settle(report.clone()).await.unwrap(), acknowledged);
    assert_eq!(
        scene.store.budget_account(&member.root).unwrap().known,
        UsageTotals {
            input: 11,
            output: 3
        }
    );
    assert!(
        replacement
            .budget_membership(&member.conversation)
            .unwrap()
            .is_none()
    );
    drop((attempt, registered));
    assert!(
        scene
            .store
            .publish_native_budget_membership(&scene.transcripts, &member, "successor", None)
            .is_err()
    );
    drop(report);
    let successor = scene
        .store
        .publish_native_budget_membership(&scene.transcripts, &member, "successor", None)
        .unwrap();
    drop((successor, connection));
}

/// A refused registration cannot fabricate a registered-attempt descriptor or
/// zero receipt. Recovery reuses the SAME original registration identity.
#[tokio::test]
async fn runtime_budget_request_registration_refusal_then_exact_zero_receipt() {
    let mut scene = Scene::new();
    let dispatch = issued(&mut scene).await;
    let owner = dispatch.budget_owner.as_ref().unwrap();
    let member = owner.member.clone();
    let attempt = owner
        .prepare_attempt(
            ModelTokenUsageKey::new(
                &dispatch.model_profile.provider,
                &dispatch.model_profile.model,
            ),
            BudgetAttemptPurpose::Execution,
        )
        .unwrap();
    let connection = rusqlite::Connection::open(scene.store.path()).unwrap();
    connection.execute_batch("CREATE TRIGGER refuse_request_attempt BEFORE INSERT ON budget_attempts BEGIN SELECT RAISE(ABORT,'fixture refusal'); END;").unwrap();
    let work = attempt.clone();
    assert!(
        tokio::task::spawn_blocking(move || work.execute())
            .await
            .unwrap()
            .is_err()
    );
    let work = attempt.clone();
    assert!(
        tokio::task::spawn_blocking(move || work.reconcile_registration())
            .await
            .unwrap()
            .unwrap()
            .is_none()
    );
    assert_eq!(
        scene
            .store
            .budget_account(&member.root)
            .unwrap()
            .accounting_revision,
        0
    );
    connection
        .execute_batch("DROP TRIGGER refuse_request_attempt;")
        .unwrap();
    let work = attempt.clone();
    let registered = tokio::task::spawn_blocking(move || work.execute())
        .await
        .unwrap()
        .unwrap();
    let report = registered
        .freeze_receipt(
            UsageSnapshot {
                input: Some(0),
                output: Some(0),
                terminal: true,
            },
            None,
        )
        .unwrap();
    let acknowledged = settle(report.clone()).await.unwrap();
    assert_eq!(acknowledged.known, UsageTotals::default());
    assert_eq!(acknowledged.accounting_revision, 1);
    assert_eq!(settle(report).await.unwrap(), acknowledged);
    drop((registered, attempt, dispatch, connection));
}
