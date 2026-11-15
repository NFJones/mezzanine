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
    assert!(clone.retire_unused().await.unwrap().is_none());
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
    // An unstarted or held owner cannot invent unused grant custody.
    let (scene, _owner, work) = case().await;
    assert!(
        work.admission_owner()
            .retire_unused()
            .await
            .unwrap()
            .is_none()
    );
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
    assert!(admission.retire_unused().await.unwrap().is_none());
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

/// Wait observations are read-only snapshots, not fresh grant/retry authority.
/// Policy, expense and issued exposure remain independently observable.
#[tokio::test]
async fn runtime_budget_admission_wait_observes_changes_without_authorizing() {
    let (scene, owner, work) = case().await;
    let admission = work.admission_owner();
    assert!(admission.observe_held().await.is_err());
    publish(
        &scene,
        BudgetLimits {
            input: TokenLimit::Limited(0),
            output: TokenLimit::Unlimited,
        },
    )
    .await;
    assert!(matches!(
        admission.claim().await.unwrap(),
        RuntimeBudgetAttemptAdmission::Held(BudgetAdmissionHold::Exhausted { .. })
    ));
    let initial = admission.observe_held().await.unwrap();
    assert_eq!(initial.limits.unwrap().input, TokenLimit::Limited(0));
    assert_eq!(initial.known, UsageTotals::default());
    assert!(!initial.root_outstanding);
    assert_eq!(initial.outstanding_attempts, 0);
    assert_eq!(admission.observe_held().await.unwrap(), initial);
    publish(&scene, BudgetLimits::default()).await;
    let changed = admission.observe_held().await.unwrap();
    assert!(changed.revisions.projection > initial.revisions.projection);
    assert_eq!(changed.revisions.accounting, initial.revisions.accounting);
    assert_eq!(grants(&scene), 0);
    // The original hold remains sticky until a caller explicitly reconsiders.
    assert!(matches!(
        admission.claim().await.unwrap(),
        RuntimeBudgetAttemptAdmission::Held(_)
    ));
    let sibling = owner
        .prepare_attempt(
            ModelTokenUsageKey::new("openai", "gpt-test"),
            BudgetAttemptPurpose::Memory,
        )
        .unwrap();
    let RuntimeBudgetAttemptAdmission::Authorized(permit) =
        sibling.admission_owner().claim().await.unwrap()
    else {
        panic!("unlimited sibling grant expected");
    };
    let issued = tokio::task::spawn_blocking(move || permit.consume())
        .await
        .unwrap()
        .unwrap();
    let outstanding = admission.observe_held().await.unwrap();
    assert!(outstanding.root_outstanding);
    assert_eq!(outstanding.outstanding_attempts, 1);
    let receipt = issued
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
    settle(receipt).await.unwrap();
    let settled = admission.observe_held().await.unwrap();
    assert_eq!(
        settled.known,
        UsageTotals {
            input: 11,
            output: 3
        }
    );
    assert!(settled.revisions.accounting > outstanding.revisions.accounting);
    assert!(settled.root_outstanding, "expense is not local retirement");
    assert_eq!(settled.outstanding_attempts, 1);
    assert_eq!(grants(&scene), 1, "observations minted authorization");
    assert!(matches!(
        admission.retry_held().await.unwrap(),
        RuntimeBudgetAttemptAdmission::Authorized(_)
    ));
    assert!(
        admission.observe_held().await.is_err(),
        "issued work became a waiter"
    );
}

/// A publication fence is not unlimited policy; root overrides remain explicit,
/// and native/store failures cannot be converted into an empty account view.
#[tokio::test]
async fn runtime_budget_admission_wait_policy_and_original_binding_fences() {
    use crate::storage::token_usage::budget::operations::BudgetPolicyOperation;
    use mez_agent::token_budget::BudgetOverride;
    let (scene, owner, work) = case().await;
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
        RuntimeBudgetAttemptAdmission::Held(_)
    ));
    let before = admission.observe_held().await.unwrap();
    let policy = BudgetPolicyOperation {
        id: "wait-root-override".into(),
        namespace: owner.member.namespace.clone(),
        root: owner.member.root.clone(),
        input: Some(BudgetOverride::Explicit(TokenLimit::Unlimited)),
        output: None,
    };
    let store = scene.store.clone();
    tokio::task::spawn_blocking(move || store.commit_budget_policy(&policy))
        .await
        .unwrap()
        .unwrap();
    let overridden = admission.observe_held().await.unwrap();
    assert!(overridden.revisions.root_policy > before.revisions.root_policy);
    assert_eq!(overridden.revisions.projection, before.revisions.projection);
    assert_eq!(overridden.limits.unwrap().input, TokenLimit::Unlimited);
    let projection = scene.store.budget_policy_projection().unwrap();
    let operation = BudgetPolicyPublication {
        namespace: projection.namespace,
        id: BudgetReceiptId::new("wait-updating").unwrap(),
        expected_revision: projection.revision,
        previous_digest: projection.committed.unwrap().digest,
        proposed: BudgetConfigPolicy {
            source: "primary".into(),
            digest: "d".repeat(64),
            limits: BudgetLimits::default(),
        },
        supersedes: None,
    };
    let store = scene.store.clone();
    tokio::task::spawn_blocking(move || store.begin_budget_policy_publication(&operation))
        .await
        .unwrap()
        .unwrap();
    let updating = admission.observe_held().await.unwrap();
    assert!(
        updating.limits.is_none(),
        "pending publication became unlimited"
    );
    assert!(updating.revisions.projection > overridden.revisions.projection);
    assert_eq!(grants(&scene), 0);
    let original = scene
        .store
        .budget_binding_owner(&owner.member)
        .unwrap()
        .unwrap();
    let connection = rusqlite::Connection::open(scene.store.path()).unwrap();
    connection
        .execute(
            "UPDATE budget_bindings SET owner='foreign-wait-owner' WHERE conversation=?1",
            [&owner.member.conversation],
        )
        .unwrap();
    assert!(admission.observe_held().await.is_err());
    connection
        .execute(
            "UPDATE budget_bindings SET owner=?1 WHERE conversation=?2",
            [&original, &owner.member.conversation],
        )
        .unwrap();
    drop(connection);
    assert_eq!(admission.observe_held().await.unwrap(), updating);
    let moved = scene.store.path().with_extension("wait-moved");
    std::fs::rename(scene.store.path(), &moved).unwrap();
    let refused = admission.observe_held().await.is_err();
    let absent = !scene.store.path().exists();
    std::fs::rename(&moved, scene.store.path()).unwrap();
    assert!(
        refused && absent,
        "wait observation recreated a missing store"
    );
    assert_eq!(admission.observe_held().await.unwrap(), updating);
}

/// Canceling the borrowed reader wait retains the actual native reader. Both
/// its replacement waiter and original journal drain must join it, not detach
/// it or fabricate an empty dependency snapshot.
#[tokio::test]
async fn runtime_budget_admission_cancelled_wait_reader_keeps_original_custody() {
    for drain in [false, true] {
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
            RuntimeBudgetAttemptAdmission::Held(_)
        ));
        let original = admission.clone();
        let read = work.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, gate) = std::sync::mpsc::channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let waiter = tokio::spawn(async move {
            original
                .observe_held_with_for_test(move || {
                    count.fetch_add(1, Ordering::SeqCst);
                    let _ = started.send(());
                    gate.recv()
                        .map_err(|_| MezError::invalid_state("wait reader gate closed"))?;
                    read.observe_admission_wait()
                })
                .await
        });
        ready.await.unwrap();
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        if drain {
            let reconcile = admission.reconcile();
            tokio::pin!(reconcile);
            assert!(futures_util::poll!(&mut reconcile).is_pending());
            release.send(()).unwrap();
            assert!(reconcile.await.unwrap().is_none());
        } else {
            let replacement = admission
                .observe_held_with_for_test(|| panic!("canceled wait reader was replaced"));
            tokio::pin!(replacement);
            assert!(futures_util::poll!(&mut replacement).is_pending());
            release.send(()).unwrap();
            assert_eq!(replacement.await.unwrap().known, UsageTotals::default());
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(grants(&scene), 0);
    }
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
    assert!(admission.retire_unused().await.unwrap().is_none());
    assert!(
        matches!(work.admission_owner().claim().await.unwrap(), RuntimeBudgetAttemptAdmission::PreviouslyAuthorized(evidence) if evidence == outcome)
    );
    assert!(admission.retry_after_absence().await.is_err());
    assert_eq!(grants(&scene), 1);
}

#[tokio::test]
async fn runtime_budget_admission_unused_retirement_joins_and_withdraws_only_original_permit() {
    Box::pin(unused_retirement(false)).await;
    Box::pin(unused_retirement(true)).await;
}

async fn unused_retirement(selected: bool) {
    let (scene, _owner, work) = case().await;
    let admission = work.admission_owner();
    if selected {
        publish(
            &scene,
            BudgetLimits {
                input: TokenLimit::Limited(0),
                output: TokenLimit::Unlimited,
            },
        )
        .await;
        assert!(matches!(
            admission.claim().await.unwrap(),
            RuntimeBudgetAttemptAdmission::Held(_)
        ));
        publish(&scene, BudgetLimits::default()).await;
    }
    let window = Arc::new(());
    let weak_window = Arc::downgrade(&window);
    let original = admission.clone();
    let execute = work.clone();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let waiter = tokio::spawn(async move {
        let authorize = move || {
            let _ = started.send(());
            blocked
                .recv_timeout(std::time::Duration::from_secs(10))
                .map_err(|_| MezError::invalid_state("fixture gate closed"))?;
            execute.authorize()
        };
        if selected {
            original
                .retry_held_with_for_test(Some(window), authorize)
                .await
        } else {
            drop(window);
            original.claim_with_for_test(authorize).await
        }
    });
    ready.await.unwrap();
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    let kept_while_running = weak_window.upgrade().is_some();
    {
        let retirement = admission.retire_unused();
        tokio::pin!(retirement);
        assert!(futures_util::poll!(&mut retirement).is_pending());
    }
    assert_eq!(grants(&scene), 0);
    release.send(()).unwrap();
    let unused = admission.retire_unused().await.unwrap().unwrap();
    let released_after_join = weak_window.upgrade().is_none();
    assert_eq!(grants(&scene), 1);
    assert!(admission.retire_unused().await.unwrap().is_none());
    assert!(matches!(
        admission.claim().await.unwrap(),
        RuntimeBudgetAttemptAdmission::PreviouslyAuthorized(_)
    ));
    assert!(admission.retry_held().await.is_err());
    assert!(admission.retry_after_absence().await.is_err());
    // Withdrawing send authority alone is not the durable release operation.
    let outstanding: i64 = rusqlite::Connection::open(scene.store.path())
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM budget_admissions WHERE retired_receipt IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(outstanding, 1);
    drop(unused);
    assert!(admission.retire_unused().await.unwrap().is_none());
    drop((admission, work, _owner, scene));
    assert_eq!(kept_while_running, selected);
    assert!(released_after_join);
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
