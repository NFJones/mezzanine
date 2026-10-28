//! Native model-request delivery under actual storage refusal and finite custody.
//!
//! Real direct helpers and the production worker/actor/ledger own every proof.
//! SQL failure does not create an acknowledgment, and equal public turn labels
//! cannot replace the original readiness or evict an outstanding request.

use super::codex_observation::{callback, declaration};
use super::helper_presentation::{child, settle};
use super::*;

/// Constructs only normalized checked counters. This supplies no request marker,
/// source witness, original origin, provider response model or process identity.
fn post(turn: &str, input: u64) -> JsonRpcRequest {
    crate::control::parse_json_rpc_request(
        &serde_json::json!({
            "jsonrpc":"2.0","id":"post","method":"agent/external/codex-observe",
            "params":{"pane_id":"%1","external_session_id":"session-a","event":"PostModelRequest",
                "turn_id":turn,"counters":{"input_tokens":input,"output_tokens":3}}
        })
        .to_string(),
    )
    .unwrap()
}

/// An actual SQL abort leaves the immutable report under its original admission.
/// A later native pre retries that report while admitting its own distinct next
/// request. Dropping the successful reply and replaying the original post still
/// charges once, and the next request's differing counters cannot substitute for
/// the retained outcome. The original ledger timestamp comes from its pre.
#[tokio::test(flavor = "current_thread")]
async fn external_codex_model_storage_refusal_retains_original_report_through_next_pre() {
    let Some(mut fixture) = fixture("hold").await else {
        panic!("Codex delivery qualification requires native peer evidence");
    };
    let path = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
    let (_source_socket, source) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_enrollment(&declaration(), &source)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &source).await["result"]["registered"],
        true
    );
    let work = fixture
        .service
        .prepare_external_helper_presentation(&callback("UserPromptSubmit", "turn-a"), &source)
        .unwrap();
    assert!(
        settle(&mut fixture, work, &source)
            .await
            .get("error")
            .is_none()
    );
    let (_pre_socket, pre) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_helper_presentation(&callback("PreModelRequest", "turn-a"), &pre)
        .unwrap();
    #[cfg(target_os = "linux")]
    let work = {
        let mut work = work;
        work.helper
            .as_mut()
            .unwrap()
            .usage_begin
            .as_mut()
            .unwrap()
            .fixture_capture_at(current_unix_seconds().saturating_sub(30));
        work
    };
    assert_eq!(
        settle(&mut fixture, work, &pre).await["result"]["observed"],
        true
    );
    let database = rusqlite::Connection::open(&path).unwrap();
    let (original, admitted): (String, i64) = database
        .query_row(
            "SELECT request_id,admitted_at FROM external_usage_requests",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    database.execute_batch("CREATE TRIGGER refuse_codex_usage BEFORE INSERT ON token_usage_events BEGIN SELECT RAISE(ABORT,'fixture storage refusal'); END;").unwrap();
    let (_post_socket, connection) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_helper_presentation(&post("turn-a", 10), &connection)
        .unwrap();
    let failed = settle(&mut fixture, work, &connection).await;
    assert_ne!(failed["result"]["durable"], true, "{failed}");
    let source_state = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap()
        .enrollment
        .as_ref()
        .unwrap();
    assert_eq!(
        source_state.codex_request_gap_coverage(),
        Some("unavailable-codex-usage-durable-acknowledgment")
    );
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM token_usage_events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    database
        .execute_batch("DROP TRIGGER refuse_codex_usage;")
        .unwrap();
    let (_next_pre_socket, next_pre) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_helper_presentation(&callback("PreModelRequest", "turn-a"), &next_pre)
        .unwrap();
    // Deliberately discard the actual durable reply, modelling a lost waiter.
    assert_eq!(
        settle(&mut fixture, work, &next_pre).await["result"]["durable"],
        true
    );
    let recovered_source = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap()
        .enrollment
        .as_ref()
        .unwrap();
    assert_eq!(recovered_source.codex_request_gap_coverage(), None);
    let work = fixture
        .service
        .prepare_external_helper_presentation(&post("turn-a", 10), &connection)
        .unwrap();
    let replay = settle(&mut fixture, work, &connection).await;
    assert_eq!(replay["result"]["durable"], true, "{replay}");
    assert_eq!(replay["result"]["applied"], false, "{replay}");
    let (_next_post_socket, next_post) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_helper_presentation(&post("turn-a", 4), &next_post)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &next_post).await["result"]["durable"],
        true
    );
    let totals: (i64, i64) = database
        .query_row(
            "SELECT count(*),sum(input_tokens) FROM token_usage_events",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(totals, (2, 14));
    let original_time: i64 = database.query_row("SELECT receipt.observed_at FROM external_usage_observations o JOIN external_usage_requests r ON r.owner=o.owner AND r.observation_id=o.event_id JOIN external_usage_receipts receipt ON receipt.stream_id=o.stream_id AND receipt.event_id=o.event_id WHERE r.request_id=?1", [&original], |row| row.get(0)).unwrap();
    assert_eq!(original_time, admitted);
    let (_missing_pre_socket, missing_pre) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_helper_presentation(&callback("PreModelRequest", "turn-a"), &missing_pre)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &missing_pre).await["result"]["observed"],
        true
    );
    let mut missing = post("turn-a", 1);
    let mut params: serde_json::Value =
        serde_json::from_str(missing.params.as_deref().unwrap()).unwrap();
    params["counters"] = serde_json::Value::Null;
    missing.params = Some(params.to_string());
    let (_missing_socket, missing_connection) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_helper_presentation(&missing, &missing_connection)
        .unwrap();
    assert_ne!(
        settle(&mut fixture, work, &missing_connection).await["result"]["durable"],
        true
    );
    let (_last_pre_socket, last_pre) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_helper_presentation(&callback("PreModelRequest", "turn-a"), &last_pre)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &last_pre).await["result"]["observed"],
        true
    );
    database.execute_batch("CREATE TRIGGER refuse_codex_usage BEFORE INSERT ON token_usage_events BEGIN SELECT RAISE(ABORT,'fixture storage refusal'); END;").unwrap();
    let (_last_post_socket, last_post) = child(&mut fixture).await;
    for durable in [false, true] {
        if durable {
            database
                .execute_batch("DROP TRIGGER refuse_codex_usage;")
                .unwrap();
        }
        let work = fixture
            .service
            .prepare_external_helper_presentation(&post("turn-a", 2), &last_post)
            .unwrap();
        let reply = settle(&mut fixture, work, &last_post).await;
        assert_eq!(reply["result"]["durable"] == true, durable, "{reply}");
        let source_state = fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .next()
            .unwrap()
            .enrollment
            .as_ref()
            .unwrap();
        assert_eq!(
            source_state.codex_request_gap_coverage(),
            Some("unavailable-codex-usage-finalized-counters")
        );
    }
    assert_eq!(
        fixture
            .service
            .external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
}

/// Sixteen outstanding genuine readiness records exhaust finite custody without
/// evicting the oldest. The seventeenth pre receives no original admission; a
/// late post may finish the oldest request under its original turn, freeing one
/// slot so the same native seventeenth writer can retry without a new identity.
#[tokio::test(flavor = "current_thread")]
async fn external_codex_model_capacity_preserves_oldest_origin_and_recovers_slot() {
    let Some(mut fixture) = fixture("hold").await else {
        panic!("Codex capacity qualification requires native peer evidence");
    };
    let path = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
    let (_source_socket, source) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_enrollment(&declaration(), &source)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &source).await["result"]["registered"],
        true
    );
    let mut helpers = Vec::new();
    for index in 0..17 {
        let turn = format!("turn-{index}");
        let work = fixture
            .service
            .prepare_external_helper_presentation(&callback("UserPromptSubmit", &turn), &source)
            .unwrap();
        assert!(
            settle(&mut fixture, work, &source)
                .await
                .get("error")
                .is_none()
        );
        let (socket, connection) = child(&mut fixture).await;
        let work = fixture
            .service
            .prepare_external_helper_presentation(&callback("PreModelRequest", &turn), &connection)
            .unwrap();
        assert_eq!(
            settle(&mut fixture, work, &connection).await["result"]["observed"],
            true
        );
        helpers.push((socket, connection));
    }
    let database = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM external_usage_requests", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        16
    );
    let source_state = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap()
        .enrollment
        .as_ref()
        .unwrap();
    assert_eq!(
        source_state.codex_request_gap_coverage(),
        Some("unavailable-codex-usage-capacity")
    );
    let (_post_socket, connection) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_helper_presentation(&post("turn-0", 10), &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["durable"],
        true
    );
    let connection = &helpers.last().unwrap().1;
    let work = fixture
        .service
        .prepare_external_helper_presentation(&callback("PreModelRequest", "turn-16"), connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, connection).await["result"]["observed"],
        true
    );
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM external_usage_requests", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        17
    );
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM token_usage_events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}
