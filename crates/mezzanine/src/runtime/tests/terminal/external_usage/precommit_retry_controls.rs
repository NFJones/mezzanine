//! Existing primary-launch legacy accounting is outside socket retry policy.
//!
//! The owning legacy fixture performs actual primary-authorized launch and
//! capability-only registration. No ordinary enrollment is nulled or bypassed.
//! Real writer contention and an explicit delayed queue prove unchanged one-shot
//! execution, frozen counters and actor responsiveness, without claiming an
//! installed producer/native same-live-source or vendor qualification.

use super::*;
use futures_util::FutureExt;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::Ordering;
use std::time::Duration;

/// Exercises the authentic legacy owner with either real writer contention or
/// queue delay beyond the SOCKET-only horizon. All fixture-owned gates/writers
/// are released and original roots terminated before check failures surface.
async fn legacy_case(delayed: bool) {
    let (mut service, connection, params, store) = fixture();
    let probe = service.probe_external_usage_worker_for_tests();
    let queued = std::sync::Arc::new(tokio::sync::Notify::new());
    let queue_release = std::sync::Arc::new(tokio::sync::Notify::new());
    if delayed {
        service.gate_external_usage_worker_for_tests(queued.clone(), queue_release.clone());
    }
    let database = rusqlite::Connection::open(store.path()).unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let actor_task = tokio::spawn(actor.run());
    let mut request_task = None;
    let checked = AssertUnwindSafe(async {
        if !delayed { database.execute_batch("BEGIN IMMEDIATE").unwrap(); }
        let frame = crate::control::encode_control_body(&serde_json::json!({"jsonrpc":"2.0","id":"legacy","method":"agent/external/usage","params":params}).to_string());
        let sender = handle.clone();
        request_task = Some(tokio::spawn(async move { sender.handle_control_input_for_connection(frame, 8192, connection).await }));
        if delayed {
            tokio::time::timeout(Duration::from_secs(5), queued.notified()).await.unwrap();
            assert_eq!(probe.attempts.load(Ordering::SeqCst), 0);
            // This actual queue hold tests policy, not guessed DB progress.
            tokio::time::sleep(Duration::from_millis(2100)).await;
            queue_release.notify_one();
        } else {
            tokio::time::timeout(Duration::from_secs(5), probe.failed.notified()).await.unwrap();
            assert_eq!(probe.attempts.load(Ordering::SeqCst), 1);
            assert_eq!(probe.retry_enabled.get(), Some(&false));
            assert_eq!(probe.completions.load(Ordering::SeqCst), 0);
            assert_eq!(handle.lifecycle_state().await.unwrap(), RuntimeLifecycleState::Running);
            assert_eq!(database.query_row("SELECT count(*) FROM token_usage_events", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
            database.execute_batch("ROLLBACK").unwrap();
            probe.release.notify_one();
        }
        let response = tokio::time::timeout(Duration::from_secs(5), request_task.take().unwrap()).await.unwrap().unwrap().unwrap();
        assert!(!response.connection.initialized());
        let (body, _) = crate::control::decode_control_frame(&response.output, 8192).unwrap();
        serde_json::from_str::<serde_json::Value>(&body).unwrap()
    }).catch_unwind().await;
    let rollback = if database.is_autocommit() {
        Ok(())
    } else {
        database.execute_batch("ROLLBACK")
    };
    probe.release.notify_one();
    queue_release.notify_one();
    if let Some(waiter) = request_task {
        waiter.abort();
    }
    let shutdown = handle.shutdown().await;
    let mut exit = actor_task.await.unwrap();
    let terminated = exit.service.terminate_all_pane_processes();
    rollback.unwrap();
    terminated.unwrap();
    shutdown.unwrap();
    let result = match checked {
        Ok(value) => value,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    assert_eq!(probe.retry_enabled.get(), Some(&false));
    assert_eq!(probe.attempts.load(Ordering::SeqCst), 1);
    assert_eq!(probe.completions.load(Ordering::SeqCst), 1);
    assert_eq!(exit.service.external_usage_admission_count_for_tests(), 1);
    assert_eq!(
        exit.service.external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
    assert_eq!(external_ledger_input(&store), if delayed { 10 } else { 0 });
    if delayed {
        assert_eq!(result["result"]["durable"], true);
        assert_eq!(result["result"]["applied"], true);
        assert_eq!(result["result"]["coverage"]["reasoning_known"], true);
        let event = database.query_row("SELECT input_tokens,output_tokens,reasoning_tokens,cached_input_tokens,cache_write_input_tokens,provider,model,observed_at FROM token_usage_events", [], |row| Ok((row.get::<_, i64>(0)?,row.get::<_, i64>(1)?,row.get::<_, i64>(2)?,row.get::<_, Option<i64>>(3)?,row.get::<_, Option<i64>>(4)?,row.get::<_, String>(5)?,row.get::<_, String>(6)?,row.get::<_, i64>(7)?))).unwrap();
        assert_eq!(
            event,
            (
                10,
                4,
                2,
                Some(3),
                None,
                "provider".into(),
                "model".into(),
                i64::try_from(params["observed_at"].as_u64().unwrap()).unwrap()
            )
        );
    } else {
        assert!(result.get("error").is_some());
    }
    for table in [
        "external_usage_receipts",
        "external_usage_streams",
        "token_usage_events",
    ] {
        assert_eq!(
            database
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            i64::from(delayed)
        );
    }
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM external_usage_requests", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
    assert_eq!(
        database
            .query_row(
                "SELECT count(*) FROM external_usage_observations",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    drop(database);
    std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
}

/// The genuine legacy primary-launch worker remains one-shot after actual BUSY.
/// Releasing that lock does not turn its failed original result into a retry.
#[tokio::test(flavor = "current_thread")]
async fn external_request_actor_legacy_usage_stays_one_shot() {
    legacy_case(false).await;
}

/// A0's socket horizon never suppresses an authorized queued legacy invocation;
/// it executes exactly once and acknowledges the unchanged original expense.
#[tokio::test(flavor = "current_thread")]
async fn external_request_actor_queue_delay_does_not_change_legacy_dispatch() {
    legacy_case(true).await;
}
