//! Sole-anchor cleanup after an exact captured ancestry lifetime ends.
//!
//! Ordinary Unix origins and pane roots are real fixture processes. Registry
//! loss removes only reconstructible registrations; source authority and ledger
//! ownership remain independently fenced. A broken chain must release ancestry
//! quota without evicting the surviving creator's retired-instance/source guard.
//! No vendor, provider work, caller PID hint, new schema or expiry policy is used.

use super::*;

/// A stale unreconciled binding can retain a shared dead ancestry Arc, but its
/// idle/diagnostic refresh must not undo anchor retirement or reattach the proof.
/// The original already-admitted report still commits/projects once independently;
/// only central binding retirement releases the final shared ancestry quota.
#[tokio::test(flavor = "current_thread")]
async fn external_source_anchor_retirement_is_absorbing_while_late_expense_settles() {
    let Some(mut fixture) = fixture("tree").await else {
        return;
    };
    let path = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
    let initial = enroll(&mut fixture, &request()).await;
    let mut successor = resume(&initial);
    let mut params: serde_json::Value =
        serde_json::from_str(successor.params.as_deref().unwrap()).unwrap();
    params["observer_instance"] = "fixture-instance-b".into();
    params["predecessor_generation"] = initial["result"]["generation"].clone();
    successor.params = Some(params.to_string());
    let first = enroll(&mut fixture, &successor).await;
    assert_eq!(first["result"]["registered"], true);
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
    let work = admit_usage(&mut fixture, &first).await;
    let native = work.clone();
    let result = tokio::task::spawn_blocking(move || native.execute())
        .await
        .unwrap();
    let before = ledger_snapshot(&path);
    let budget = fixture
        .service
        .control
        .external_agents()
        .enrollments
        .ancestry_budget
        .clone();
    let active = fixture
        .service
        .external_source_anchor_fence_for_tests(&owner)
        .unwrap();
    assert!(!active.0 && active.2 && active.3);
    super::ancestry::orphan_middle(&fixture).await;
    fixture.service.prune_external_source_anchors();
    let retired = fixture
        .service
        .external_source_anchor_fence_for_tests(&owner)
        .unwrap();
    assert!(retired.0 && !retired.2 && !retired.3);
    assert_eq!(retired.1, active.1 + 1);
    assert_eq!(
        budget.reserved(),
        3,
        "unreconciled binding still legitimately owns the proof"
    );
    fixture.service.renew_connected_external_observers();
    fixture
        .service
        .refresh_external_source_diagnostics_for_owner(&owner);
    assert_eq!(
        fixture
            .service
            .external_source_anchor_fence_for_tests(&owner),
        Some(retired)
    );
    let committed: serde_json::Value =
        serde_json::from_str(&fixture.service.complete_external_usage(work, result)).unwrap();
    assert_eq!(committed["result"]["durable"], true);
    assert_eq!(committed["result"]["applied"], true);
    assert_eq!(
        fixture
            .service
            .external_source_anchor_fence_for_tests(&owner),
        Some(retired)
    );
    assert_eq!(ledger_snapshot(&path), before);
    assert_eq!(
        fixture
            .service
            .external_token_usage(None)
            .values()
            .map(|usage| usage.input_tokens)
            .sum::<u64>(),
        11
    );
    assert_eq!(
        fixture
            .service
            .external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
    fixture.service.reconcile_external_agent_registrations();
    assert_eq!(budget.reserved(), 0);
    fixture.service.reconcile_external_agent_registrations();
    assert_eq!(
        fixture
            .service
            .external_source_anchor_fence_for_tests(&owner),
        Some(retired)
    );
    assert_eq!(ledger_snapshot(&path), before);
    assert!(
        enroll(&mut fixture, &resume(&first))
            .await
            .get("error")
            .is_some()
    );
}

/// Qualifies native work through the existing worker and actor settlement.
/// Negative metadata/admission results remain visible without fabricated proof.
async fn enroll(fixture: &mut Fixture, request: &JsonRpcRequest) -> serde_json::Value {
    let work = match fixture
        .service
        .prepare_external_enrollment(request, &fixture.connection)
    {
        Ok(work) => work,
        Err(error) => return serde_json::json!({"error":error.message()}),
    };
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    serde_json::from_str(&fixture.service.complete_external_enrollment(
        work,
        observed,
        &fixture.connection,
    ))
    .unwrap()
}

/// Builds only an explicit public receipt selector. Native ancestry/writer/root
/// and the server-owned namespace remain mandatory; no PID or owner is supplied.
fn resume(receipt: &serde_json::Value) -> JsonRpcRequest {
    let mut request = request();
    let mut params: serde_json::Value =
        serde_json::from_str(request.params.as_deref().unwrap()).unwrap();
    params["source_resume"] = serde_json::json!({"observer_witness":receipt["result"]["observer_witness"],"generation":receipt["result"]["generation"]});
    request.params = Some(params.to_string());
    request
}

/// Commits synthetic normalized expense through genuine native begin/end work,
/// not a direct storage seed or private namespace override. Its original UTC
/// clock/request marker is captured once and never rebuilt for replay.
async fn commit_usage(fixture: &mut Fixture, receipt: &serde_json::Value) {
    let work = admit_usage(fixture, receipt).await;
    let native = work.clone();
    let result = tokio::task::spawn_blocking(move || native.execute())
        .await
        .unwrap();
    let committed: serde_json::Value =
        serde_json::from_str(&fixture.service.complete_external_usage(work, result)).unwrap();
    assert_eq!(committed["result"]["durable"], true);
    assert_eq!(committed["result"]["applied"], true);
}

/// Reserves one actual native request before a later source-retirement boundary.
/// Readiness and report identity remain immutable for off-actor settlement.
async fn admit_usage(
    fixture: &mut Fixture,
    receipt: &serde_json::Value,
) -> crate::runtime::ExternalUsageWork {
    let params = serde_json::json!({"launch_token":receipt["result"]["launch_token"],"generation":receipt["result"]["generation"],
        "external_session_id":receipt["result"]["external_session_id"],"request_id":"a".repeat(64)});
    let begin = crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"begin","method":"agent/external/usage-begin","params":params}).to_string()).unwrap();
    let work = fixture
        .service
        .prepare_external_usage_begin(&begin, &fixture.connection)
        .unwrap();
    let native = work.clone();
    let result = tokio::task::spawn_blocking(move || native.execute())
        .await
        .unwrap();
    let ready: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_usage_begin(work, result, &fixture.connection),
    )
    .unwrap();
    assert_eq!(ready["result"]["accepted"], true);
    let mut params = params;
    params["event_id"] = "b".repeat(64).into();
    params["observed_at"] = crate::runtime::current_unix_seconds().into();
    params["provider"] = "fixture".into();
    params["model"] = "actual-model".into();
    params["counters"] = serde_json::json!({"input_tokens":11,"output_tokens":4});
    let end = crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"end","method":"agent/external/usage-end","params":params}).to_string()).unwrap();
    fixture
        .service
        .prepare_external_usage_end(&end, &fixture.connection)
        .unwrap()
}

/// Freezes logical source, request provenance, observation/receipt/checkpoint and
/// expense rows from the unique fixture ledger. These are metadata/counters only;
/// a source mapping or snapshot never substitutes for native admission authority.
fn ledger_snapshot(path: &std::path::Path) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
    let connection = rusqlite::Connection::open(path).unwrap();
    [
        "external_usage_sources",
        "external_usage_requests",
        "external_usage_observations",
        "external_usage_receipts",
        "external_usage_streams",
        "token_usage_events",
    ]
    .into_iter()
    .map(|table| {
        // Table names are fixed test-owned schema names, not caller input.
        let mut statement = connection
            .prepare(&format!("SELECT * FROM {table} ORDER BY 1,2"))
            .unwrap();
        let columns = statement.column_count();
        statement
            .query_map([], |row| {
                (0..columns)
                    .map(|column| row.get::<_, rusqlite::types::Value>(column))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    })
    .collect()
}

/// After registration loss, only the retained anchor owns the original three
/// ancestry descriptors. Exact upper-ancestor exit leaves producer/root alive
/// but invalidates the chain: resume already denies authority, then maintenance
/// must release quota while retaining an inert source/instance tombstone. Old
/// and never-seen instance labels cannot revive the source; committed usage,
/// original request provenance and mapping rows are byte-logically unchanged.
#[tokio::test(flavor = "current_thread")]
async fn external_source_anchor_broken_chain_releases_quota_without_reactivating_source() {
    let Some(mut fixture) = fixture("tree").await else {
        return;
    };
    let path = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
    let first = enroll(&mut fixture, &request()).await;
    assert_eq!(first["result"]["registered"], true);
    commit_usage(&mut fixture, &first).await;
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
    let budget = fixture
        .service
        .control
        .external_agents()
        .enrollments
        .ancestry_budget
        .clone();
    let root = fixture.service.pane_process_identity("%1").unwrap();
    let producer = fixture.connection.unix_origin().unwrap().identity;
    let before = ledger_snapshot(&path);
    assert_eq!(before[0].len(), 1);
    assert_eq!(before[5].len(), 1);
    assert_eq!(budget.reserved(), 3);
    *fixture.service.control.external_agents_mut() = Default::default();
    assert_eq!(budget.reserved(), 3);
    super::ancestry::orphan_middle(&fixture).await;
    assert_eq!(
        fixture
            .connection
            .unix_origin()
            .unwrap()
            .reobserve()
            .unwrap(),
        producer
    );
    assert!(
        fixture
            .service
            .pane_process_identity_is_current("%1", &root)
    );
    assert_eq!(budget.reserved(), 3);
    // Enrollment preparation invokes the same maintenance owner before it
    // rejects resume; the subsequent idle tick must remain quota-idempotent.
    assert!(
        enroll(&mut fixture, &resume(&first))
            .await
            .get("error")
            .is_some()
    );
    assert_eq!(budget.reserved(), 0);
    fixture.service.reconcile_external_agent_registrations();
    assert_eq!(
        budget.reserved(),
        0,
        "sole-anchor cleanup retained a broken chain's quota"
    );
    assert!(
        fixture.service.external_agent_cleanup_needed(),
        "surviving creator must retain its inert source fence"
    );
    assert!(
        enroll(&mut fixture, &resume(&first))
            .await
            .get("error")
            .is_some()
    );
    assert!(
        enroll(&mut fixture, &request())
            .await
            .to_string()
            .contains("external native source has retired")
    );
    let mut alias = request();
    let mut params: serde_json::Value =
        serde_json::from_str(alias.params.as_deref().unwrap()).unwrap();
    params["observer_instance"] = "never-seen-instance".into();
    alias.params = Some(params.to_string());
    assert!(
        enroll(&mut fixture, &alias)
            .await
            .to_string()
            .contains("external native source has retired")
    );
    fixture.service.reconcile_external_agent_registrations();
    assert_eq!(budget.reserved(), 0);
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .is_empty()
    );
    assert_eq!(ledger_snapshot(&path), before);
    let database = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        database
            .query_row("SELECT owner FROM external_usage_sources", [], |row| row
                .get::<_, String>(
                0
            ))
            .unwrap(),
        owner
    );
    assert_eq!(
        database
            .query_row(
                "SELECT sum(input_tokens) FROM token_usage_events",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        11
    );
    drop(database);
    super::helper_presentation::release(&mut fixture.socket, &fixture.connection).await;
    fixture.service.reconcile_external_agent_registrations();
    assert!(!fixture.service.external_agent_cleanup_needed());
    assert_eq!(ledger_snapshot(&path), before);
}

/// An intact retained chain is not cleanup garbage. Registry loss retains its
/// three-descriptor quota; real explicit recovery conserves the accounting owner
/// and keeps that same original ancestry. Actual pane-root end finally releases
/// the source fence without a new source/mapping or fabricated usage delta.
#[tokio::test(flavor = "current_thread")]
async fn external_source_anchor_intact_chain_recovers_and_root_end_releases_quota() {
    let Some(mut fixture) = fixture("tree").await else {
        return;
    };
    let path = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
    let first = enroll(&mut fixture, &request()).await;
    assert_eq!(first["result"]["registered"], true);
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
    let budget = fixture
        .service
        .control
        .external_agents()
        .enrollments
        .ancestry_budget
        .clone();
    let before = ledger_snapshot(&path);
    *fixture.service.control.external_agents_mut() = Default::default();
    fixture.service.reconcile_external_agent_registrations();
    assert_eq!(budget.reserved(), 3);
    let recovered = enroll(&mut fixture, &resume(&first)).await;
    assert_eq!(recovered["result"]["registered"], true);
    assert_eq!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .next()
            .unwrap()
            .accounting_owner,
        owner
    );
    assert_eq!(budget.reserved(), 3);
    assert_eq!(ledger_snapshot(&path), before);
    fixture.service.terminate_all_pane_processes().unwrap();
    fixture.service.reconcile_external_agent_registrations();
    assert_eq!(budget.reserved(), 0);
    assert_eq!(ledger_snapshot(&path), before);
}
