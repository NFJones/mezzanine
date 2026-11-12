//! Real original receipt transactions through cancellation-safe writer custody.
//! Fault closures only control the existing blocking boundary, never replace
//! receipt identity, SQLite reconciliation or the durable accounting oracle.

use super::*;
use crate::error::MezErrorKind;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

async fn receipt(scene: &mut Scene) -> RuntimeBudgetReceiptWork {
    let dispatch = issued(scene).await;
    let attempt = dispatch
        .budget_owner
        .as_ref()
        .unwrap()
        .prepare_attempt(
            ModelTokenUsageKey::new("openai", "gpt-test"),
            BudgetAttemptPurpose::Execution,
        )
        .unwrap();
    let registered = tokio::task::spawn_blocking(move || attempt.execute())
        .await
        .unwrap()
        .unwrap();
    registered
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
        .unwrap()
}

fn receipt_count(scene: &Scene) -> i64 {
    rusqlite::Connection::open(scene.store.path())
        .unwrap()
        .query_row("SELECT COUNT(*) FROM budget_receipts", [], |row| row.get(0))
        .unwrap()
}

#[tokio::test]
async fn runtime_budget_receipt_commit_clones_share_one_actual_writer_and_original_ack() {
    let mut scene = Scene::new();
    let work = receipt(&mut scene).await;
    let first = work.commit_owner();
    let second = work.clone().commit_owner();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let execute = work.clone();
    let (a, b) = tokio::join!(
        first.commit_with_for_test(move || {
            count.fetch_add(1, Ordering::SeqCst);
            execute.execute()
        }),
        second.commit_with_for_test(|| panic!("duplicate writer must not run")),
    );
    let acknowledged = a.unwrap();
    assert_eq!(b.unwrap(), acknowledged);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(receipt_count(&scene), 1);
    assert_eq!(first.commit().await.unwrap(), acknowledged);
    assert_eq!(
        second.reconcile().await.unwrap(),
        Some(acknowledged.clone())
    );
    assert_eq!(second.retry_after_absence().await.unwrap(), acknowledged);
    assert_eq!(receipt_count(&scene), 1);
}

#[tokio::test]
async fn runtime_budget_receipt_cancelled_waiters_retain_actual_writer_until_join() {
    let mut scene = Scene::new();
    let work = receipt(&mut scene).await;
    let owner = work.commit_owner();
    let original = owner.clone();
    let execute = work.clone();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let waiter = tokio::spawn(async move {
        original
            .commit_with_for_test(move || {
                let _ = started.send(());
                blocked
                    .recv()
                    .map_err(|_| MezError::invalid_state("fixture writer released by cleanup"))?;
                execute.execute()
            })
            .await
    });
    ready.await.unwrap();
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert_eq!(receipt_count(&scene), 0);
    assert!(owner.retry_after_absence().await.is_err());
    // A query cannot certify absence while the original producer is live.
    {
        let query = owner.reconcile();
        tokio::pin!(query);
        assert!(futures_util::poll!(&mut query).is_pending());
    }
    // A different root still does useful work while the writer is held.
    let mut sibling = Scene::new();
    let other = receipt(&mut sibling).await;
    assert_eq!(other.commit_owner().commit().await.unwrap().known.input, 11);
    release.send(()).unwrap();
    let acknowledged = owner.commit().await.unwrap();
    assert_eq!(work.commit_owner().commit().await.unwrap(), acknowledged);
    assert_eq!(receipt_count(&scene), 1);
    assert_eq!(
        acknowledged.known,
        UsageTotals {
            input: 11,
            output: 3
        }
    );
}

#[tokio::test]
async fn runtime_budget_receipt_commit_loss_recovers_exact_ack_without_reissuing_write() {
    let mut scene = Scene::new();
    let work = receipt(&mut scene).await;
    let owner = work.commit_owner();
    let execute = work.clone();
    let error = owner
        .commit_with_for_test(move || {
            execute.execute()?;
            panic!("PRIVATE_RECEIPT_WRITER_FAULT");
        })
        .await
        .unwrap_err();
    assert_eq!(error.kind(), MezErrorKind::InvalidState);
    assert!(!error.message().contains("PRIVATE_RECEIPT"));
    assert_eq!(receipt_count(&scene), 1);
    assert!(owner.commit().await.is_err());
    assert!(owner.retry_after_absence().await.is_err());
    let recovered = owner.reconcile().await.unwrap().unwrap();
    assert_eq!(recovered, work.reconcile().unwrap().unwrap());
    assert_eq!(owner.commit().await.unwrap(), recovered);
    assert_eq!(owner.retry_after_absence().await.unwrap(), recovered);
    assert_eq!(receipt_count(&scene), 1);
}

#[tokio::test]
async fn runtime_budget_receipt_refusal_requires_new_absence_before_each_explicit_retry() {
    let mut scene = Scene::new();
    let work = receipt(&mut scene).await;
    let owner = work.commit_owner();
    assert!(owner.reconcile().await.unwrap().is_none());
    assert!(owner.retry_after_absence().await.is_err());
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let failure = owner
        .commit_with_for_test(move || {
            count.fetch_add(1, Ordering::SeqCst);
            Err(MezError::forbidden("fixture original refusal"))
        })
        .await
        .unwrap_err();
    assert_eq!(failure.kind(), MezErrorKind::Forbidden);
    assert_eq!(
        owner.commit().await.unwrap_err().message(),
        failure.message()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(owner.retry_after_absence().await.is_err());
    assert_eq!(receipt_count(&scene), 0);
    // A real transaction refusal consumes the first qualified retry. Removing
    // it alone cannot authorize another write without another exact query.
    let connection = rusqlite::Connection::open(scene.store.path()).unwrap();
    connection
        .execute_batch("CREATE TRIGGER refuse_receipt BEFORE INSERT ON budget_receipts BEGIN SELECT RAISE(ABORT,'fixture'); END;")
        .unwrap();
    assert!(owner.reconcile().await.unwrap().is_none());
    assert!(owner.retry_after_absence().await.is_err());
    assert_eq!(receipt_count(&scene), 0);
    connection
        .execute_batch("DROP TRIGGER refuse_receipt;")
        .unwrap();
    assert!(owner.retry_after_absence().await.is_err());
    assert!(owner.reconcile().await.unwrap().is_none());
    let acknowledged = owner.retry_after_absence().await.unwrap();
    assert_eq!(
        acknowledged.known,
        UsageTotals {
            input: 11,
            output: 3
        }
    );
    assert_eq!(receipt_count(&scene), 1);
}

#[tokio::test]
async fn runtime_budget_receipt_failed_query_is_not_absence_or_a_retry_grant() {
    let mut scene = Scene::new();
    let work = receipt(&mut scene).await;
    let owner = work.commit_owner();
    assert!(
        owner
            .commit_with_for_test(|| Err(MezError::invalid_state("fixture refusal")))
            .await
            .is_err()
    );
    let connection = rusqlite::Connection::open(scene.store.path()).unwrap();
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    connection.pragma_update(None, "user_version", 999).unwrap();
    assert!(owner.reconcile().await.is_err());
    assert!(owner.retry_after_absence().await.is_err());
    assert_eq!(receipt_count(&scene), 0);
    connection
        .pragma_update(None, "user_version", version)
        .unwrap();
    assert!(owner.reconcile().await.unwrap().is_none());
    assert!(owner.retry_after_absence().await.is_ok());
    assert_eq!(receipt_count(&scene), 1);
}

#[tokio::test]
async fn runtime_budget_receipt_cancelled_query_keeps_its_actual_handle_and_source() {
    let mut scene = Scene::new();
    let work = receipt(&mut scene).await;
    let owner = work.commit_owner();
    assert!(
        owner
            .commit_with_for_test(|| Err(MezError::invalid_state("fixture refusal")))
            .await
            .is_err()
    );
    let original = owner.clone();
    let query_work = work.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let waiter = tokio::spawn(async move {
        original
            .reconcile_with_for_test(move || {
                count.fetch_add(1, Ordering::SeqCst);
                let _ = started.send(());
                blocked
                    .recv()
                    .map_err(|_| MezError::invalid_state("fixture query released by cleanup"))?;
                query_work.reconcile()
            })
            .await
    });
    ready.await.unwrap();
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    release.send(()).unwrap();
    assert!(owner.reconcile().await.unwrap().is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(receipt_count(&scene), 0);
    assert!(owner.retry_after_absence().await.is_ok());
    assert_eq!(receipt_count(&scene), 1);
}
