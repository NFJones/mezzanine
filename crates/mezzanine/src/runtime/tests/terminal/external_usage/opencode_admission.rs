//! Reachable legacy OpenCode admission and canonical accounting migration.
//!
//! Launch/registration and every migrated/new report use production admission,
//! worker execution and actor completion. Only two pre-upgrade expense rows are
//! seeded from exact server-authorized reports; no native mapping, owner setter
//! or synthetic native proof is supplied. Legacy capabilities remain legacy.

use super::*;

/// Normal usage RPC admission must migrate exact charged per-message receipts,
/// charge new observations into one bounded group, preserve a reset and reject
/// changed final metadata. Neither owner promotion nor vendor SDK qualification
/// follows from a capability-only fixture.
#[test]
fn runtime_external_opencode_normal_admission_migrates_without_native_mapping() {
    qualify(9);
}

/// Explicit normal-admission workload passes the old4096-message boundary with
/// two old checkpoints and one shared new checkpoint. Source aliases stay finite
/// and exactly replayable; this is not an ordinary vendor/native producer test.
#[test]
#[ignore = "explicit normal legacy admission 4097-message workload"]
fn runtime_external_opencode_normal_admission_exceeds_4096_messages() {
    qualify(4097);
}

/// Drives only authorized legacy launch work. The seeded two old ledger rows
/// represent pre-upgrade expense, not source authority or native provenance.
fn qualify(count: u64) {
    let (mut service, mut connection, params, store) = fixture_for_harness("opencode");
    let now = params["observed_at"].as_u64().unwrap();
    let make = |index| {
        let mut params = params.clone();
        params["epoch"] = serde_json::json!(format!("opencode-message:{index:064x}"));
        params["event_id"] = serde_json::json!(format!("completed:{}", now * 1000 + index % 1000));
        params
    };
    let mut original_report = None;
    // Capture original server-owned owner/project through actual authorization.
    // The old store entry point represents the previous release's worker layout.
    for index in 0..2 {
        let work = service
            .prepare_external_usage(&rpc(&make(index)), &connection)
            .unwrap();
        original_report.get_or_insert_with(|| work.report.clone());
        store.ingest_external(&work.report, now).unwrap();
        service.complete_external_usage(
            work,
            Err(crate::error::MezError::conflict("pre-upgrade fixture")),
        );
    }
    assert_eq!(external_ledger_input(&store), 20);
    service.set_external_usage_stream_limit_for_tests(1);
    for index in 0..count {
        if index % 64 == 0 {
            let renewal = serde_json::json!({"jsonrpc":"2.0","id":"renew","method":"agent/external/renew", "params":{"launch_token":params["launch_token"],"generation":params["generation"],"external_session_id":params["external_session_id"]}}).to_string();
            let reply: serde_json::Value = serde_json::from_str(
                &service.dispatch_runtime_control_body_for_connection(&renewal, &mut connection),
            )
            .unwrap();
            assert!(reply.get("error").is_none());
        }
        let reply = finish(&mut service, &connection, &make(index));
        assert_eq!(reply["result"]["durable"], true, "{reply}");
        assert_eq!(reply["result"]["applied"], index >= 2);
        if index == 1 {
            service.reset_agent_token_usage_for_pane("%1");
        }
    }
    assert_eq!(external_ledger_input(&store), (count * 10) as i64);
    assert_eq!(service.external_usage_projection_counts_for_tests(), (1, 3));
    assert_eq!(
        service
            .external_token_usage(Some("%1"))
            .values()
            .map(|usage| usage.input_tokens)
            .sum::<u64>(),
        (count - 2) * 10
    );
    for index in [0, 1, count - 1] {
        let reply = finish(&mut service, &connection, &make(index));
        assert_eq!(reply["result"]["applied"], false);
    }
    let mut changed = make(0);
    changed["counters"]["input_tokens"] = serde_json::json!(11);
    assert!(
        finish(&mut service, &connection, &changed)
            .get("error")
            .is_some()
    );
    let mut changed = make(count - 1);
    changed["event_id"] =
        serde_json::json!(format!("completed:{}", now * 1000 + (count - 1) % 1000 + 1));
    assert!(
        finish(&mut service, &connection, &changed)
            .get("error")
            .is_some()
    );
    assert_eq!(external_ledger_input(&store), (count * 10) as i64);
    let pair = crate::integrations::bootstrap::opencode::CompletedObservation::from_legacy_report(
        &original_report.unwrap(),
    )
    .unwrap()
    .unwrap();
    assert!(
        store.ingest_external(pair.observation(), now).is_err(),
        "legacy aliases must not grant native storage authority"
    );
    assert!(
        store
            .opencode_observation_group_snapshot(pair.observation())
            .is_err()
    );
    let mut wrong = make(count);
    wrong["launch_token"] = serde_json::json!("x".repeat(43));
    assert!(
        service
            .prepare_external_usage(&rpc(&wrong), &connection)
            .is_err()
    );
    let sql = rusqlite::Connection::open(store.path()).unwrap();
    assert_eq!(
        sql.query_row("SELECT count(*) FROM external_usage_sources", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        sql.query_row("SELECT count(*) FROM external_usage_streams", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        3
    );
    assert_eq!(
        sql.query_row(
            "SELECT count(*) FROM external_usage_observations",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        count as i64
    );
    assert_eq!(
        service.external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Normal protocol method, with no internal migration or native source selector.
fn rpc(params: &serde_json::Value) -> crate::control::JsonRpcRequest {
    crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"normal","method":"agent/external/usage","params":params}).to_string()).unwrap()
}

/// Executes the admitted real ledger worker and its actual actor settlement.
fn finish(
    service: &mut RuntimeSessionService,
    connection: &ControlConnectionState,
    params: &serde_json::Value,
) -> serde_json::Value {
    let work = service
        .prepare_external_usage(&rpc(params), connection)
        .unwrap();
    let result = work.execute();
    serde_json::from_str(&service.complete_external_usage(work, result)).unwrap()
}
