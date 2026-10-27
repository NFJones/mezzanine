//! Common request accounting for native direct-child helpers across adapters.
//!
//! Real enrollment, mandatory durable source acceptance and real kernel child
//! evidence authorize work. Harness selectors neither manufacture native source
//! authority nor certify a vendor callback schema or an installed vendor SDK.

use super::*;

/// Each canonical adapter can use the same public begin/end contract after real
/// ordinary-process admission. A producer impersonating its own child fails
/// native observation without expense; the genuine helper charges once and
/// replays the original outcome without changing source identity or totals.
#[tokio::test(flavor = "current_thread")]
async fn external_native_helper_accounting_supports_six_adapters_with_exact_parent() {
    for harness in ["pi", "opencode", "claude", "codex", "copilot", "cursor"] {
        let (mut fixture, store, _publication, first) =
            super::source_activation_fixture::accepted_source_for_harness(harness)
                .await
                .expect("native helper qualification requires real source evidence");
        let (_socket, connection) = super::helper_presentation::child(&mut fixture).await;
        let first = serde_json::json!({"result":first});
        let mut admitted_at = None;
        for iteration in 0..2 {
            let begin = selected(super::curated_usage::begin(&first), harness);
            let ready = settle(&mut fixture, &connection, &begin).await;
            assert_eq!(ready["result"]["accepted"], true, "{harness}: {ready}");
            let clock = ready["result"]["admitted_at"].as_u64().unwrap();
            assert_eq!(*admitted_at.get_or_insert(clock), clock);
            let end = selected(super::curated_usage::end(&first, clock), harness);
            if iteration == 0 {
                let own_connection = fixture.connection.clone();
                let rejected = settle(&mut fixture, &own_connection, &end).await;
                assert!(
                    rejected.get("error").is_some(),
                    "producer is not its own helper"
                );
                assert_eq!(ledger_input(&store), 0);
            }
            let reply = settle(&mut fixture, &connection, &end).await;
            assert_eq!(reply["result"]["durable"], true, "{harness}: {reply}");
            assert_eq!(reply["result"]["applied"], iteration == 0);
            assert_eq!(ledger_input(&store), 10);
            assert_eq!(
                fixture
                    .service
                    .external_usage_reservation_counts_for_tests(),
                (0, 0)
            );
        }
        let wrong = selected(
            super::curated_usage::begin(&first),
            if harness == "pi" { "codex" } else { "pi" },
        );
        assert!(
            fixture
                .service
                .prepare_external_helper_presentation(&wrong, &connection)
                .is_err()
        );
        assert!(!connection.initialized());
        assert!(connection.caller_client_id().is_none());
    }
}

/// Changes only the public selector before normal parsing/authorization. Source
/// mapping and native identities remain produced by real enrollment and workers.
fn selected(mut request: JsonRpcRequest, harness: &str) -> JsonRpcRequest {
    let mut params: serde_json::Value =
        serde_json::from_str(request.params.as_deref().unwrap()).unwrap();
    params["harness"] = serde_json::json!(harness);
    request.params = Some(params.to_string());
    request
}

/// Drives the actual native worker and actor completion, never injects success.
async fn settle(
    fixture: &mut Fixture,
    connection: &ControlConnectionState,
    request: &JsonRpcRequest,
) -> serde_json::Value {
    let work = fixture
        .service
        .prepare_external_helper_presentation(request, connection)
        .unwrap();
    let worker = work.clone();
    let observed = tokio::task::spawn_blocking(move || worker.observe())
        .await
        .unwrap();
    serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, connection),
    )
    .unwrap()
}

/// Reads existing incurred ledger expense only; no fixture SQL creates authority.
fn ledger_input(store: &crate::storage::token_usage::TokenUsageStore) -> i64 {
    rusqlite::Connection::open(store.path())
        .unwrap()
        .query_row(
            "SELECT coalesce(sum(input_tokens),0) FROM token_usage_events",
            [],
            |row| row.get(0),
        )
        .unwrap()
}
