//! Native original-work refusal/publication controls beside actor acceptance.
//!
//! The shared owned child supplies actual source/root/writer proof and successful
//! exit evidence. These controls invoke original admitted work directly to check
//! typed transaction boundaries and authority conservation; they do not replace
//! the one-frame normal actor RED/GREEN or qualify installed vendor callbacks.

use super::*;
use crate::storage::token_usage::{ExternalPrecommitPhase, ExternalUsageAttempt};
use tokio::io::AsyncWriteExt;

/// Qualifies one original native request and returns its still-reserved work.
/// No fake readiness or supplied PID/source/project selects admission.
async fn native_work() -> Option<(Fixture, crate::runtime::ExternalUsageWork)> {
    let mut fixture = pending_retry::pending_fixture().await?;
    let path = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
    let enrolled = enroll_producer(&mut fixture).await;
    let begin = begin(&enrolled, &"a".repeat(64));
    let ready = fixture
        .service
        .prepare_external_usage_begin(&begin, &fixture.connection)
        .unwrap();
    let worker = ready.clone();
    let result = tokio::task::spawn_blocking(move || worker.execute())
        .await
        .unwrap();
    let response: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_usage_begin(ready, result, &fixture.connection),
    )
    .unwrap();
    assert_eq!(response["result"]["accepted"], true);
    let mut params: serde_json::Value =
        serde_json::from_str(begin.params.as_deref().unwrap()).unwrap();
    params["event_id"] = serde_json::json!("b".repeat(64));
    params["observed_at"] = serde_json::json!(crate::runtime::current_unix_seconds());
    params["provider"] = serde_json::json!("fixture");
    params["model"] = serde_json::json!("original-model");
    params["counters"] =
        serde_json::json!({"input_tokens":10,"output_tokens":3,"cached_input_tokens":0});
    let request = crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"original-end","method":"agent/external/usage-end","params":params}).to_string()).unwrap();
    let work = fixture
        .service
        .prepare_external_usage_end(&request, &fixture.connection)
        .unwrap();
    assert!(work.permits_precommit_retry());
    assert_eq!(
        fixture
            .service
            .external_usage_reservation_counts_for_tests(),
        (1, 1)
    );
    Some((fixture, work))
}

/// Finishes the exact native child, checks its atomic actual exit record, and
/// terminates only the generated fixture root before surfacing cleanup failures.
async fn complete_child(mut fixture: Fixture) {
    let release = fixture.socket.write_all(&[2]).await;
    let status = pending_retry::child_status(&fixture.directory).await;
    let terminated = fixture.service.terminate_all_pane_processes();
    release.unwrap();
    terminated.unwrap();
    assert_eq!(status.unwrap(), "0\n");
}

/// Real ledger BEGIN BUSY qualifies the exact native work, preserving original
/// model/time/counters/origin and one reservation. Re-executing already published
/// exact work returns its ACTUAL original evidence, never a second OnceLock set.
/// Mutated work cannot borrow the cached result; ledger deltas remain once.
#[tokio::test(flavor = "current_thread")]
async fn external_request_attempt_preserves_native_report_and_real_publication() {
    let Some((mut fixture, work)) = native_work().await else {
        return;
    };
    let original = work.report.clone();
    let database = rusqlite::Connection::open(work.store.path()).unwrap();
    database.execute_batch("BEGIN IMMEDIATE").unwrap();
    let worker = work.clone();
    let attempt = tokio::task::spawn_blocking(move || worker.execute_attempt())
        .await
        .unwrap();
    let ExternalUsageAttempt::PrecommitRefused { refusal, .. } = attempt else {
        panic!("actual native writer refusal not qualified");
    };
    assert_eq!(refusal.phase, ExternalPrecommitPhase::LedgerBeginImmediate);
    assert_eq!(refusal.extended_code, rusqlite::ffi::SQLITE_BUSY);
    assert_eq!(
        fixture
            .service
            .external_usage_reservation_counts_for_tests(),
        (1, 1)
    );
    assert_eq!(work.report, original);
    database.execute_batch("ROLLBACK").unwrap();
    let worker = work.clone();
    let commit = tokio::task::spawn_blocking(move || worker.execute_attempt())
        .await
        .unwrap()
        .into_result()
        .unwrap();
    assert!(commit.applied);
    assert_eq!(work.execute_attempt().into_result().unwrap(), commit);
    let mut changed = work.clone();
    changed.report.model.model = "changed-model".into();
    assert!(matches!(
        changed.execute_attempt(),
        ExternalUsageAttempt::Completed(Err(_))
    ));
    let response: serde_json::Value =
        serde_json::from_str(&fixture.service.complete_external_usage(work, Ok(commit))).unwrap();
    assert_eq!(response["result"]["durable"], true);
    assert_eq!(
        fixture
            .service
            .external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
    assert_eq!(
        database
            .query_row(
                "SELECT sum(input_tokens) FROM token_usage_events",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        10
    );
    assert_eq!(
        database
            .query_row("SELECT model FROM token_usage_events", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        original.model.model
    );
    assert_eq!(
        database
            .query_row("SELECT observed_at FROM token_usage_events", [], |row| row
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        i64::try_from(original.observed_at).unwrap()
    );
    for table in [
        "external_usage_observations",
        "external_usage_receipts",
        "external_usage_streams",
        "token_usage_events",
    ] {
        assert_eq!(
            database
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    drop(database);
    complete_child(fixture).await;
}

/// The shared binding builder used before curated helper proof never opts into
/// socket retry. Even real native readiness and a contended same-owner ledger
/// remain ordinary Completed(Err), not a private scheduling witness. No helper
/// or legacy work is promoted by merely carrying the same normalized report.
#[tokio::test(flavor = "current_thread")]
async fn external_request_shared_helper_builder_remains_one_shot() {
    let Some((mut fixture, original)) = native_work().await else {
        return;
    };
    fixture.service.release_unexecuted_external_usage(&original);
    let binding = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap();
    let params = serde_json::json!({"request_id":"a".repeat(64),"event_id":"b".repeat(64),"observed_at":original.report.observed_at,"provider":"fixture","model":"original-model","counters":{"input_tokens":10,"output_tokens":3,"cached_input_tokens":0}});
    let work = fixture
        .service
        .external_request_work_for_binding("shared-work".into(), binding, &params)
        .unwrap();
    let work = fixture.service.reserve_external_usage_work(work).unwrap();
    assert!(!work.permits_precommit_retry());
    let database = rusqlite::Connection::open(work.store.path()).unwrap();
    database.execute_batch("BEGIN IMMEDIATE").unwrap();
    let worker = work.clone();
    let attempt = tokio::task::spawn_blocking(move || worker.execute_attempt())
        .await
        .unwrap();
    assert!(matches!(attempt, ExternalUsageAttempt::Completed(Err(_))));
    database.execute_batch("ROLLBACK").unwrap();
    fixture.service.release_unexecuted_external_usage(&work);
    assert_eq!(
        fixture
            .service
            .external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM token_usage_events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(database);
    complete_child(fixture).await;
}

/// An original admitted origin cannot be changed between attempts. Tampering a
/// generated marker's time remains terminal with no expense, even when coarse
/// report/source/model fields are unchanged. Storage possession is not authority
/// to reinterpret the original readiness or borrow a new admission boundary.
#[tokio::test(flavor = "current_thread")]
async fn external_request_attempt_rejects_changed_original_origin() {
    let Some((mut fixture, work)) = native_work().await else {
        return;
    };
    let database = rusqlite::Connection::open(work.store.path()).unwrap();
    database
        .execute(
            "UPDATE external_usage_requests SET admitted_at=admitted_at-1",
            [],
        )
        .unwrap();
    let worker = work.clone();
    let attempt = tokio::task::spawn_blocking(move || worker.execute_attempt())
        .await
        .unwrap();
    assert!(matches!(attempt, ExternalUsageAttempt::Completed(Err(_))));
    fixture.service.release_unexecuted_external_usage(&work);
    assert_eq!(
        fixture
            .service
            .external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM token_usage_events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(database);
    complete_child(fixture).await;
}
