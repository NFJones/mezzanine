//! Actual actor dispatch boundaries for retained precommit accounting retry.
//!
//! All cases share an owned real native producer/root and one original marker.
//! Parent-authored component frames do not qualify installed vendor transport.
//! Genuine DB locks, explicit failed-worker/queue checkpoints and actual actor
//! completion observations establish counts and conservation. Deliberate2s
//! queue holds test the declared dispatch horizon, not guessed storage progress.

use super::*;
use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
use futures_util::FutureExt;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::Ordering;
use tokio::io::AsyncWriteExt;

/// Source-backed actor scenario; none supplies a synthetic storage result.
#[derive(Clone, Copy)]
enum Case {
    /// Keep the real writer through all three permitted executions.
    Exhausted,
    /// Hold a failed invocation beyond the fixed original dispatch horizon.
    Expired,
    /// Drop the original RPC waiter but retain admitted incurred expense.
    ClosedWaiter,
    /// Retire the registration before completing already-admitted expense.
    Retired,
    /// Another exact report commits before the original retry resumes.
    CompetingReplay,
    /// Queue delay beyond the horizon prevents even initial socket dispatch.
    QueuedSocket,
}

/// Frames a component request without printing or persisting private handles.
fn frame(id: &str, method: &str, params: &serde_json::Value) -> Vec<u8> {
    crate::control::encode_control_body(
        &serde_json::json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string(),
    )
}

/// Reads only the actual typed actor reply; neutral callback exit is not proof.
fn response(output: &[u8]) -> serde_json::Value {
    let (body, _) = crate::control::decode_control_frame(output, 8192).unwrap();
    serde_json::from_str(&body).unwrap()
}

/// Runs one bounded native actor scenario and always releases fixture-owned
/// writer/probe/queue/child resources before surfacing caught check failures.
/// Original-root cleanup follows the same source-reviewed baseline pattern.
async fn run_case(case: Case) {
    let Some(mut fixture) = pending_retry::pending_fixture().await else {
        return;
    };
    let path = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
    let enrolled = enroll_producer(&mut fixture).await;
    let owner = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap()
        .accounting_owner
        .clone();
    let probe = fixture.service.probe_external_usage_worker_for_tests();
    let queued = Arc::new(tokio::sync::Notify::new());
    let queue_release = Arc::new(tokio::sync::Notify::new());
    if matches!(case, Case::QueuedSocket) {
        fixture
            .service
            .gate_external_usage_worker_for_tests(queued.clone(), queue_release.clone());
    }
    let database = rusqlite::Connection::open(&path).unwrap();
    let params = serde_json::json!({"launch_token":enrolled["result"]["launch_token"],"generation":enrolled["result"]["generation"],"external_session_id":"session-a","request_id":"a".repeat(64)});
    let service = std::mem::replace(&mut fixture.service, RuntimeServiceFixture::new().build());
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let actor_task = tokio::spawn(actor.run());
    let mut end_task = None;
    let checked = AssertUnwindSafe(async {
        let ready = handle.handle_control_input_for_connection(frame("begin", "agent/external/usage-begin", &params), 8192, fixture.connection.clone()).await.unwrap();
        let ready = response(&ready.output);
        assert_eq!(ready["result"]["accepted"], true);
        let observed_at = crate::runtime::current_unix_seconds();
        let mut end = params.clone();
        end["event_id"] = serde_json::json!("b".repeat(64));
        end["observed_at"] = observed_at.into();
        end["provider"] = "fixture".into();
        end["model"] = "original-model".into();
        end["counters"] = serde_json::json!({"input_tokens":10,"output_tokens":3,"cached_input_tokens":0});
        let method = "agent/external/usage-end";
        let queued_case = matches!(case, Case::QueuedSocket);
        if !queued_case { database.execute_batch("BEGIN IMMEDIATE").unwrap(); }
        let sender = handle.clone();
        let connection = fixture.connection.clone();
        let original_frame = frame("original", method, &end);
        end_task = Some(tokio::spawn(async move { sender.handle_control_input_for_connection(original_frame, 8192, connection).await }));
        if queued_case {
            tokio::time::timeout(Duration::from_secs(5), queued.notified()).await.unwrap();
            assert_eq!(probe.attempts.load(Ordering::SeqCst), 0);
            // Actual queue delay deliberately exceeds the documented horizon;
            // it is not a sleep used to infer a storage failure or child exit.
            tokio::time::sleep(Duration::from_millis(2100)).await;
            queue_release.notify_one();
        } else {
            tokio::time::timeout(Duration::from_secs(5), probe.failed.notified()).await.unwrap();
            assert_eq!(probe.attempts.load(Ordering::SeqCst), 1);
            assert_eq!(probe.completions.load(Ordering::SeqCst), 0);
            assert_eq!(database.query_row("SELECT count(*) FROM token_usage_events", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
            assert!(database.query_row("SELECT observation_id FROM external_usage_requests", [], |row| row.get::<_, Option<String>>(0)).unwrap().is_none());
            assert_eq!(handle.lifecycle_state().await.unwrap(), crate::runtime::RuntimeLifecycleState::Running);
            match case {
                Case::Expired => {
                    tokio::time::sleep(Duration::from_millis(2100)).await;
                    database.execute_batch("ROLLBACK").unwrap();
                }
                Case::ClosedWaiter => {
                    let waiter = end_task.take().unwrap();
                    waiter.abort();
                    assert!(waiter.await.unwrap_err().is_cancelled());
                    database.execute_batch("ROLLBACK").unwrap();
                }
                Case::Retired => {
                    database.execute_batch("ROLLBACK").unwrap();
                    let retirement = serde_json::json!({"launch_token":params["launch_token"],"generation":params["generation"],"external_session_id":"session-a"});
                    let retired = handle.handle_control_input_for_connection(frame("retire", "agent/external/deregister", &retirement), 8192, fixture.connection.clone()).await.unwrap();
                    assert!(response(&retired.output).get("result").is_some());
                }
                Case::CompetingReplay => {
                    database.execute_batch("ROLLBACK").unwrap();
                    let competitor = handle.handle_control_input_for_connection(frame("competitor", method, &end), 8192, fixture.connection.clone()).await.unwrap();
                    let competitor = response(&competitor.output);
                    assert_eq!(competitor["result"]["durable"], true);
                    assert_eq!(competitor["result"]["applied"], true);
                    assert_eq!(probe.completions.load(Ordering::SeqCst), 1);
                }
                Case::Exhausted => {}
                Case::QueuedSocket => unreachable!("queued branch"),
            }
            probe.release.notify_one();
        }
        let reply = if let Some(waiter) = end_task.take() {
            let result = tokio::time::timeout(Duration::from_secs(5), waiter).await.unwrap().unwrap().unwrap();
            Some(response(&result.output))
        } else {
            tokio::time::timeout(Duration::from_secs(5), probe.completed.notified()).await.unwrap();
            None
        };
        if !database.is_autocommit() { database.execute_batch("ROLLBACK").unwrap(); }
        (reply, ready, observed_at)
    }).catch_unwind().await;
    let rollback = if database.is_autocommit() {
        Ok(())
    } else {
        database.execute_batch("ROLLBACK")
    };
    probe.release.notify_one();
    queue_release.notify_one();
    if let Some(waiter) = end_task {
        waiter.abort();
    }
    let shutdown = handle.shutdown().await;
    let mut exit = actor_task.await.unwrap();
    // Stop actor maintenance while the original native producer is still
    // alive. Its later intentional exit must not mask a retirement/revival bug
    // in the completed accounting state we inspect below.
    let child_release = fixture.socket.write_all(&[2]).await;
    let completed = pending_retry::child_status(&fixture.directory).await;
    let terminated = exit.service.terminate_all_pane_processes();
    fixture.service = exit.service;
    rollback.unwrap();
    child_release.unwrap();
    terminated.unwrap();
    shutdown.unwrap();
    assert_eq!(completed.unwrap(), "0\n");
    let (reply, ready, observed_at) = match checked {
        Ok(value) => value,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    let success = matches!(
        case,
        Case::ClosedWaiter | Case::Retired | Case::CompetingReplay
    );
    let (attempts, admissions) = match case {
        Case::Exhausted => (3, 1),
        Case::Expired => (1, 1),
        Case::ClosedWaiter | Case::Retired => (2, 1),
        Case::CompetingReplay => (3, 2),
        Case::QueuedSocket => (0, 1),
    };
    assert_eq!(probe.attempts.load(Ordering::SeqCst), attempts);
    assert_eq!(probe.completions.load(Ordering::SeqCst), admissions);
    assert_eq!(
        fixture.service.external_usage_admission_count_for_tests(),
        u64::try_from(admissions).unwrap()
    );
    assert_eq!(
        fixture
            .service
            .external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
    let binding = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap();
    assert_eq!(binding.accounting_owner, owner);
    assert_eq!(binding.retired, matches!(case, Case::Retired));
    if let Some(reply) = reply {
        if success {
            assert_eq!(reply["result"]["durable"], true);
            assert_eq!(
                reply["result"]["applied"],
                !matches!(case, Case::CompetingReplay)
            );
            assert_eq!(reply["result"]["coverage"]["reasoning_known"], false);
        } else {
            assert!(reply.get("error").is_some());
        }
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
            i64::from(success)
        );
    }
    let observation = success;
    assert_eq!(
        database
            .query_row(
                "SELECT count(*) FROM external_usage_observations",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        i64::from(observation)
    );
    assert_eq!(
        database
            .query_row(
                "SELECT admitted_at FROM external_usage_requests",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        i64::try_from(ready["result"]["admitted_at"].as_u64().unwrap()).unwrap()
    );
    assert_eq!(
        database
            .query_row(
                "SELECT observation_id FROM external_usage_requests",
                [],
                |row| row.get::<_, Option<String>>(0)
            )
            .unwrap(),
        observation.then(|| "b".repeat(64))
    );
    if success {
        let recorded = database.query_row("SELECT input_tokens,output_tokens,observed_at,model,cached_input_tokens,reasoning_known,project_id FROM token_usage_events", [], |row| Ok((row.get::<_, i64>(0)?,row.get::<_, i64>(1)?,row.get::<_, i64>(2)?,row.get::<_, String>(3)?,row.get::<_, Option<i64>>(4)?,row.get::<_, i64>(5)?,row.get::<_, Option<String>>(6)?))).unwrap();
        assert_eq!(
            recorded,
            (
                10,
                3,
                i64::try_from(observed_at).unwrap(),
                "original-model".into(),
                Some(0),
                0,
                None
            )
        );
        assert_eq!(
            fixture
                .service
                .external_token_usage(None)
                .values()
                .map(|usage| usage.input_tokens)
                .sum::<u64>(),
            10
        );
    }
}

/// Three real writer refusals exhaust without expense, eviction or a new admission.
#[tokio::test(flavor = "current_thread")]
async fn external_request_actor_three_refusals_exhaust_one_admission() {
    run_case(Case::Exhausted).await;
}

/// A failed worker held beyond2s cannot dispatch another attempt or reset its budget.
#[tokio::test(flavor = "current_thread")]
async fn external_request_actor_delayed_failure_expires_original_horizon() {
    run_case(Case::Expired).await;
}

/// Losing the reply waiter does not cancel already-admitted expense or completion.
#[tokio::test(flavor = "current_thread")]
async fn external_request_actor_closed_waiter_still_commits_once() {
    run_case(Case::ClosedWaiter).await;
}

/// Incurring expense before retirement does not revive retired observer presentation.
#[tokio::test(flavor = "current_thread")]
async fn external_request_actor_retired_source_settles_without_revival() {
    run_case(Case::Retired).await;
}

/// A competing exact commit yields actual applied=false replay and one conserved delta.
#[tokio::test(flavor = "current_thread")]
async fn external_request_actor_competing_commit_returns_durable_inert_replay() {
    run_case(Case::CompetingReplay).await;
}

/// Delayed socket dispatch is fenced before initial execution under the same horizon.
#[tokio::test(flavor = "current_thread")]
async fn external_request_actor_queue_delay_prevents_socket_dispatch() {
    run_case(Case::QueuedSocket).await;
}
