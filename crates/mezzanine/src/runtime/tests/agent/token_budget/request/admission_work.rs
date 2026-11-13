//! Actual native authorization through retained worker custody. Gates control
//! scheduling only; the original SQLite/lease transactions remain the oracle.

use super::*;
use crate::runtime::{
    RuntimeBudgetAttemptAdmission, RuntimeBudgetAttemptWork, RuntimeBudgetRequestOwner,
};
use crate::storage::token_usage::budget::{
    admission::BudgetAdmissionHold,
    policy_projection::{BudgetConfigPolicy, BudgetPolicyPublication},
};
use mez_agent::token_budget::{BudgetLimits, BudgetReceiptId, TokenLimit};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

async fn publish(scene: &Scene, limits: BudgetLimits) {
    let projection = scene.store.budget_policy_projection().unwrap();
    let operation = BudgetPolicyPublication {
        namespace: projection.namespace,
        id: BudgetReceiptId::new(crate::storage::token_usage::new_token_usage_event_id()).unwrap(),
        expected_revision: projection.revision,
        previous_digest: projection
            .committed
            .as_ref()
            .map_or_else(|| "a".repeat(64), |p| p.digest.clone()),
        proposed: BudgetConfigPolicy {
            source: "primary".into(),
            digest: format!("{:064x}", projection.revision + 1),
            limits,
        },
        supersedes: None,
    };
    let store = scene.store.clone();
    tokio::task::spawn_blocking(move || {
        store.begin_budget_policy_publication(&operation).unwrap();
        store
            .reconcile_budget_policy_publication(&operation, &operation.proposed.digest)
            .unwrap();
    })
    .await
    .unwrap();
}

async fn case() -> (Scene, RuntimeBudgetRequestOwner, RuntimeBudgetAttemptWork) {
    let mut scene = Scene::new();
    let dispatch = issued(&mut scene).await;
    let owner = dispatch.budget_owner.unwrap();
    publish(&scene, BudgetLimits::default()).await;
    let work = owner
        .prepare_attempt(
            ModelTokenUsageKey::new("openai", "gpt-test"),
            BudgetAttemptPurpose::Execution,
        )
        .unwrap();
    (scene, owner, work)
}

fn grants(scene: &Scene) -> i64 {
    rusqlite::Connection::open(scene.store.path())
        .unwrap()
        .query_row("SELECT COUNT(*) FROM budget_admissions", [], |row| {
            row.get(0)
        })
        .unwrap()
}

#[tokio::test]
async fn runtime_budget_admission_cancelled_waiters_share_one_worker_and_one_permit() {
    let (scene, owner, work) = case().await;
    let admission = work.admission_owner();
    let original = admission.clone();
    let execute = work.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let waiter = tokio::spawn(async move {
        original
            .claim_with_for_test(move || {
                count.fetch_add(1, Ordering::SeqCst);
                let _ = started.send(());
                blocked
                    .recv()
                    .map_err(|_| MezError::invalid_state("fixture admission gate closed"))?;
                execute.authorize()
            })
            .await
    });
    ready.await.unwrap();
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert_eq!(grants(&scene), 0);
    {
        let observation = admission.reconcile();
        tokio::pin!(observation);
        assert!(futures_util::poll!(&mut observation).is_pending());
    }
    // The held worker does not own a global actor or database lock.
    let (_sibling, _sibling_owner, sibling) = case().await;
    assert!(matches!(
        sibling.admission_owner().claim().await.unwrap(),
        RuntimeBudgetAttemptAdmission::Authorized(_)
    ));
    release.send(()).unwrap();
    let clone = work.clone().admission_owner();
    let (a, b) = tokio::join!(admission.claim(), clone.claim());
    let RuntimeBudgetAttemptAdmission::Authorized(permit) = a.unwrap() else {
        panic!("original permit missing")
    };
    let RuntimeBudgetAttemptAdmission::PreviouslyAuthorized(evidence) = b.unwrap() else {
        panic!("second spend permit minted")
    };
    let issued = tokio::task::spawn_blocking(move || permit.consume())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(issued.outcome(), &evidence);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(grants(&scene), 1);
    assert_eq!(clone.reconcile().await.unwrap(), Some(evidence));
    assert!(clone.retry_held().await.is_err());
    assert!(clone.retry_after_absence().await.is_err());
    assert_eq!(
        scene
            .store
            .budget_account(&owner.member.root)
            .unwrap()
            .known,
        UsageTotals::default()
    );
}

#[tokio::test]
async fn runtime_budget_admission_hold_requires_explicit_current_policy_reconsideration() {
    let (scene, _owner, work) = case().await;
    publish(
        &scene,
        BudgetLimits {
            input: TokenLimit::Limited(0),
            output: TokenLimit::Unlimited,
        },
    )
    .await;
    let admission = work.admission_owner();
    assert!(matches!(
        admission.claim().await.unwrap(),
        RuntimeBudgetAttemptAdmission::Held(BudgetAdmissionHold::Exhausted { .. })
    ));
    assert_eq!(grants(&scene), 0);
    // Publishing a new policy does not itself replay an authorization waiter.
    publish(&scene, BudgetLimits::default()).await;
    assert!(matches!(
        admission.claim().await.unwrap(),
        RuntimeBudgetAttemptAdmission::Held(BudgetAdmissionHold::Exhausted { .. })
    ));
    assert!(matches!(
        admission.retry_held().await.unwrap(),
        RuntimeBudgetAttemptAdmission::Authorized(_)
    ));
    assert_eq!(grants(&scene), 1);
    assert!(matches!(
        admission.claim().await.unwrap(),
        RuntimeBudgetAttemptAdmission::PreviouslyAuthorized(_)
    ));
    assert!(admission.retry_held().await.is_err());
}

#[tokio::test]
async fn runtime_budget_admission_commit_before_panic_recovers_evidence_never_permission() {
    let (scene, _owner, work) = case().await;
    let admission = work.admission_owner();
    let execute = work.clone();
    let error = admission
        .claim_with_for_test(move || {
            drop(execute.authorize()?);
            panic!("PRIVATE_AUTHORIZATION_WRITER_FAULT");
        })
        .await
        .unwrap_err();
    assert!(!error.message().contains("PRIVATE_AUTHORIZATION"));
    assert_eq!(grants(&scene), 1);
    assert!(admission.claim().await.is_err());
    assert!(admission.retry_held().await.is_err());
    assert!(admission.retry_after_absence().await.is_err());
    let outcome = admission.reconcile().await.unwrap().unwrap();
    assert!(
        matches!(work.admission_owner().claim().await.unwrap(), RuntimeBudgetAttemptAdmission::PreviouslyAuthorized(evidence) if evidence == outcome)
    );
    assert!(admission.retry_after_absence().await.is_err());
    assert_eq!(grants(&scene), 1);
}

#[tokio::test]
async fn runtime_budget_admission_refusal_and_failed_query_cannot_grant_automatic_retry() {
    let (scene, _owner, work) = case().await;
    let admission = work.admission_owner();
    let connection = rusqlite::Connection::open(scene.store.path()).unwrap();
    connection.execute_batch("CREATE TRIGGER refuse_authorization BEFORE INSERT ON budget_admissions BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(admission.claim().await.is_err());
    assert_eq!(grants(&scene), 0);
    assert!(admission.retry_held().await.is_err());
    connection
        .execute_batch("DROP TRIGGER refuse_authorization;")
        .unwrap();
    assert!(admission.claim().await.is_err());
    assert!(admission.retry_after_absence().await.is_err());
    assert!(admission.reconcile().await.unwrap().is_none());
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    connection.pragma_update(None, "user_version", 999).unwrap();
    assert!(admission.reconcile().await.is_err());
    assert!(
        admission.retry_after_absence().await.is_err(),
        "failed query must revoke prior absence"
    );
    connection
        .pragma_update(None, "user_version", version)
        .unwrap();
    assert!(admission.reconcile().await.unwrap().is_none());
    assert!(matches!(
        admission.retry_after_absence().await.unwrap(),
        RuntimeBudgetAttemptAdmission::Authorized(_)
    ));
    assert!(admission.retry_after_absence().await.is_err());
    assert_eq!(grants(&scene), 1);
}

#[tokio::test]
async fn runtime_budget_admission_retained_grant_has_no_idle_source_custody_cycle() {
    let (mut scene, owner, work) = case().await;
    let member = owner.member.clone();
    let admission = work.admission_owner();
    let execute = work.clone();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let waiter = tokio::spawn(async move {
        admission
            .claim_with_for_test(move || {
                let _ = started.send(());
                blocked
                    .recv()
                    .map_err(|_| MezError::invalid_state("fixture admission gate closed"))?;
                execute.authorize()
            })
            .await
    });
    ready.await.unwrap();
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    release.send(()).unwrap();
    // Leave the completed native grant inside its UNOBSERVED JoinHandle.
    // A runtime-source Arc in that result would otherwise create an idle cycle.
    let retained = work.admission_owner();
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while !retained.worker_finished_for_test().await {
            tokio::task::yield_now().await;
        }
    })
    .await
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
    drop((work, owner));
    assert!(
        scene
            .store
            .publish_native_budget_membership(&scene.transcripts, &member, "successor", None)
            .is_err()
    );
    drop(retained);
    let successor = scene
        .store
        .publish_native_budget_membership(&scene.transcripts, &member, "successor", None)
        .unwrap();
    assert_eq!(
        grants(&scene),
        1,
        "dropping custody cannot erase issued exposure"
    );
    drop(successor);
}

#[tokio::test]
async fn runtime_budget_admission_cancelled_query_retains_exact_read_worker() {
    let (scene, _owner, work) = case().await;
    let admission = work.admission_owner();
    assert!(
        admission
            .claim_with_for_test(|| {
                Err(MezError::invalid_state("fixture authorization refusal"))
            })
            .await
            .is_err()
    );
    let original = admission.clone();
    let query_work = work.clone();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let waiter = tokio::spawn(async move {
        original
            .reconcile_with_for_test(move || {
                let _ = started.send(());
                blocked
                    .recv()
                    .map_err(|_| MezError::invalid_state("fixture query gate closed"))?;
                query_work.reconcile_admission()
            })
            .await
    });
    ready.await.unwrap();
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    {
        let retry = admission.retry_after_absence();
        tokio::pin!(retry);
        assert!(futures_util::poll!(&mut retry).is_pending());
    }
    assert_eq!(grants(&scene), 0);
    release.send(()).unwrap();
    assert!(
        admission
            .reconcile_with_for_test(|| { panic!("canceled read query must not be duplicated") })
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        admission.retry_after_absence().await.unwrap(),
        RuntimeBudgetAttemptAdmission::Authorized(_)
    ));
    assert_eq!(grants(&scene), 1);
}
