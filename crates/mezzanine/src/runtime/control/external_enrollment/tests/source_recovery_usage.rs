//! Native request conservation across same-service source recovery.
//!
//! Real socket/root evidence owns every begin/end. SQLite reopen and disposable
//! registry loss must conserve original request project/time/source witness and
//! immutable report clocks. This is a source-native fixture, not installed Node
//! qualification, a paid request, or a never-received-report guarantee.

use super::*;

/// Full control-owner replacement is NOT source recovery. Already committed
/// expense stays in SQLite, but old begin/end completion and cancellation cannot
/// consume a new service's coincident numeric reservation. A genuinely different
/// native session namespace independently admits a new request; no old marker,
/// capability or projection is reconstructed from storage.
#[tokio::test(flavor = "current_thread")]
async fn external_source_resume_full_control_replacement_fences_accounting_aba() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let path = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
    let first = enroll(&mut fixture, &request()).await;
    ready(&mut fixture, &begin(&first, &"a".repeat(64))).await;
    let old_begin = fixture
        .service
        .prepare_external_usage_begin(&begin(&first, &"e".repeat(64)), &fixture.connection)
        .unwrap();
    let native = old_begin.clone();
    let old_ready = tokio::task::spawn_blocking(move || native.execute())
        .await
        .unwrap();
    let old_report = report(&first, &"a".repeat(64), &"b".repeat(64), "old-model", 10);
    let old_end = fixture
        .service
        .prepare_external_usage_end(&old_report, &fixture.connection)
        .unwrap();
    let native = old_end.clone();
    let old_commit = tokio::task::spawn_blocking(move || native.execute())
        .await
        .unwrap();
    fixture.service.control = crate::runtime::control::RuntimeControlComponent::new(
        crate::control::ControlIdempotencyCache::default(),
        mez_agent::messaging::MessageService::default(),
        None,
    );
    let mut new_source = request();
    let mut params: serde_json::Value =
        serde_json::from_str(new_source.params.as_deref().unwrap()).unwrap();
    params["external_session_id"] = "distinct-session-b".into();
    params["observer_instance"] = "distinct-instance-b".into();
    new_source.params = Some(params.to_string());
    let second = enroll(&mut fixture, &new_source).await;
    assert_eq!(second["result"]["registered"], true, "{second}");
    let new_begin = fixture
        .service
        .prepare_external_usage_begin(&begin(&second, &"c".repeat(64)), &fixture.connection)
        .unwrap();
    let old_reply: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_usage_begin(old_begin, old_ready, &fixture.connection),
    )
    .unwrap();
    assert!(
        old_reply
            .to_string()
            .contains("external readiness reservation owner changed")
    );
    assert_eq!(fixture.service.external_usage_begin_pending_for_tests(), 1);
    let native = new_begin.clone();
    let result = tokio::task::spawn_blocking(move || native.execute())
        .await
        .unwrap();
    let new_ready: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_usage_begin(new_begin, result, &fixture.connection),
    )
    .unwrap();
    assert_eq!(new_ready["result"]["accepted"], true, "{new_ready}");
    let new_report = report(&second, &"c".repeat(64), &"d".repeat(64), "new-model", 7);
    let new_end = fixture
        .service
        .prepare_external_usage_end(&new_report, &fixture.connection)
        .unwrap();
    fixture.service.release_unexecuted_external_usage(&old_end);
    assert_eq!(
        fixture
            .service
            .external_usage_reservation_counts_for_tests(),
        (1, 1)
    );
    let old_reply: serde_json::Value =
        serde_json::from_str(&fixture.service.complete_external_usage(old_end, old_commit))
            .unwrap();
    assert!(
        old_reply
            .to_string()
            .contains("external accounting reservation owner changed")
    );
    assert_eq!(
        fixture
            .service
            .external_usage_reservation_counts_for_tests(),
        (1, 1)
    );
    let native = new_end.clone();
    let result = tokio::task::spawn_blocking(move || native.execute())
        .await
        .unwrap();
    let new_reply: serde_json::Value =
        serde_json::from_str(&fixture.service.complete_external_usage(new_end, result)).unwrap();
    assert_eq!(new_reply["result"]["durable"], true, "{new_reply}");
    assert_eq!(
        fixture
            .service
            .external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
    let database = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        database
            .query_row(
                "SELECT sum(input_tokens) FROM token_usage_events",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        17
    );
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM external_usage_sources", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
}

/// Registration loss is not a usage reset. Both existing model streams and
/// session totals survive even when only one report is replayed. The pane reset
/// baseline remains zero on replay, and only genuinely new expense becomes
/// visible; this is one retained accounting object, not a reconstructed copy.
#[tokio::test(flavor = "current_thread")]
async fn external_source_resume_unreplayed_stream_and_reset_baselines_are_retained() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(
            fixture.directory.join("usage.sqlite"),
        ));
    let first = enroll(&mut fixture, &request()).await;
    ready(&mut fixture, &begin(&first, &"a".repeat(64))).await;
    let old = report(&first, &"a".repeat(64), &"b".repeat(64), "model-a", 10);
    assert_eq!(finish(&mut fixture, &old).await["result"]["applied"], true);
    ready(&mut fixture, &begin(&first, &"c".repeat(64))).await;
    let other = report(
        &first,
        &"c".repeat(64),
        &"d".repeat(64),
        "unreplayed-model",
        5,
    );
    assert_eq!(
        finish(&mut fixture, &other).await["result"]["applied"],
        true
    );
    let before = fixture.service.external_token_usage(None);
    assert_eq!(
        before.values().map(|usage| usage.input_tokens).sum::<u64>(),
        15
    );
    assert!(fixture.service.reset_external_token_usage("%1"));
    *fixture.service.control.external_agents_mut() = Default::default();
    assert_eq!(fixture.service.external_token_usage(None), before);
    let mut recovery = request();
    let mut params: serde_json::Value =
        serde_json::from_str(recovery.params.as_deref().unwrap()).unwrap();
    params["source_resume"] = serde_json::json!({"observer_witness":first["result"]["observer_witness"],"generation":first["result"]["generation"]});
    recovery.params = Some(params.to_string());
    let second = enroll(&mut fixture, &recovery).await;
    ready(&mut fixture, &begin(&second, &"a".repeat(64))).await;
    let mut replay = old;
    let mut params: serde_json::Value =
        serde_json::from_str(replay.params.as_deref().unwrap()).unwrap();
    params["launch_token"] = second["result"]["launch_token"].clone();
    params["generation"] = second["result"]["generation"].clone();
    replay.params = Some(params.to_string());
    assert_eq!(
        finish(&mut fixture, &replay).await["result"]["applied"],
        false
    );
    assert_eq!(fixture.service.external_token_usage(None), before);
    assert_eq!(
        fixture
            .service
            .external_token_usage(Some("%1"))
            .values()
            .map(|usage| usage.input_tokens)
            .sum::<u64>(),
        0
    );
    ready(&mut fixture, &begin(&second, &"e".repeat(64))).await;
    let new = report(&second, &"e".repeat(64), &"f".repeat(64), "model-a", 7);
    assert_eq!(finish(&mut fixture, &new).await["result"]["applied"], true);
    assert_eq!(
        fixture
            .service
            .external_token_usage(None)
            .values()
            .map(|usage| usage.input_tokens)
            .sum::<u64>(),
        22
    );
    assert_eq!(
        fixture
            .service
            .external_token_usage(Some("%1"))
            .values()
            .map(|usage| usage.input_tokens)
            .sum::<u64>(),
        7
    );
}

/// Constructs a finalized fixture report with a frozen original clock. Retrying
/// it never rebuilds IDs, model/counters or source-origin metadata.
pub(super) fn report(
    receipt: &serde_json::Value,
    marker: &str,
    event: &str,
    model: &str,
    input: u64,
) -> JsonRpcRequest {
    let mut request = begin(receipt, marker);
    request.method = "agent/external/usage-end".into();
    let mut params: serde_json::Value =
        serde_json::from_str(request.params.as_deref().unwrap()).unwrap();
    params["event_id"] = event.into();
    params["provider"] = "fixture".into();
    params["model"] = model.into();
    params["observed_at"] = crate::runtime::current_unix_seconds().into();
    params["counters"] = serde_json::json!({"input_tokens":input,"output_tokens":0});
    request.params = Some(params.to_string());
    request
}

/// A readiness worker captured before registry loss cannot consume a later
/// reservation. Its persisted original origin remains available, but stale
/// credential settlement rejects while the new worker independently succeeds.
#[tokio::test(flavor = "current_thread")]
async fn external_source_resume_pending_begin_keeps_original_reservation_owner() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(
            fixture.directory.join("usage.sqlite"),
        ));
    let first = enroll(&mut fixture, &request()).await;
    let old_request = begin(&first, &"a".repeat(64));
    let old = fixture
        .service
        .prepare_external_usage_begin(&old_request, &fixture.connection)
        .unwrap();
    let native = old.clone();
    let old_result = tokio::task::spawn_blocking(move || native.execute())
        .await
        .unwrap();
    *fixture.service.control.external_agents_mut() = Default::default();
    let mut recovery = request();
    let mut params: serde_json::Value =
        serde_json::from_str(recovery.params.as_deref().unwrap()).unwrap();
    params["source_resume"] = serde_json::json!({"observer_witness":first["result"]["observer_witness"],"generation":first["result"]["generation"]});
    recovery.params = Some(params.to_string());
    let second = enroll(&mut fixture, &recovery).await;
    let new_request = begin(&second, &"a".repeat(64));
    let new = fixture
        .service
        .prepare_external_usage_begin(&new_request, &fixture.connection)
        .unwrap();
    assert_eq!(fixture.service.external_usage_begin_pending_for_tests(), 2);
    let rejected: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_usage_begin(old, old_result, &fixture.connection),
    )
    .unwrap();
    assert!(rejected.get("error").is_some());
    assert_eq!(fixture.service.external_usage_begin_pending_for_tests(), 1);
    let native = new.clone();
    let result = tokio::task::spawn_blocking(move || native.execute())
        .await
        .unwrap();
    let ready: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_usage_begin(new, result, &fixture.connection),
    )
    .unwrap();
    assert_eq!(ready["result"]["accepted"], true, "{ready}");
    assert_eq!(fixture.service.external_usage_begin_pending_for_tests(), 0);
}

/// Already-admitted expense must settle even when registration is recreated.
/// Delayed old completion or cancellation consumes only its original reservation,
/// never a new same-stream/different-stream slot. The new report also gets a real
/// durable ACK, projections conserve 17 (or 7 on cancellation), and no orphaned
/// admission remains. Cancellation does not perform provider/storage work.
#[tokio::test(flavor = "current_thread")]
async fn external_source_resume_pending_end_and_release_preserve_new_reservations() {
    for same_stream in [false, true] {
        for cancel in [false, true] {
            let Some(mut fixture) = fixture("hold").await else {
                return;
            };
            fixture.service.set_token_usage_store(
                crate::storage::token_usage::TokenUsageStore::new(
                    fixture.directory.join("usage.sqlite"),
                ),
            );
            let first = enroll(&mut fixture, &request()).await;
            ready(&mut fixture, &begin(&first, &"a".repeat(64))).await;
            let old_report = report(&first, &"a".repeat(64), &"b".repeat(64), "model-a", 10);
            let old = fixture
                .service
                .prepare_external_usage_end(&old_report, &fixture.connection)
                .unwrap();
            let old_result = if cancel {
                None
            } else {
                let native = old.clone();
                Some(
                    tokio::task::spawn_blocking(move || native.execute())
                        .await
                        .unwrap(),
                )
            };
            *fixture.service.control.external_agents_mut() = Default::default();
            let mut recovery = request();
            let mut params: serde_json::Value =
                serde_json::from_str(recovery.params.as_deref().unwrap()).unwrap();
            params["source_resume"] = serde_json::json!({"observer_witness":first["result"]["observer_witness"],"generation":first["result"]["generation"]});
            recovery.params = Some(params.to_string());
            let second = enroll(&mut fixture, &recovery).await;
            ready(&mut fixture, &begin(&second, &"c".repeat(64))).await;
            let new_report = report(
                &second,
                &"c".repeat(64),
                &"d".repeat(64),
                if same_stream { "model-a" } else { "model-b" },
                7,
            );
            let new = fixture
                .service
                .prepare_external_usage_end(&new_report, &fixture.connection)
                .unwrap();
            assert_eq!(
                fixture
                    .service
                    .external_usage_reservation_counts_for_tests()
                    .0,
                2
            );
            if let Some(result) = old_result {
                let committed: serde_json::Value =
                    serde_json::from_str(&fixture.service.complete_external_usage(old, result))
                        .unwrap();
                assert_eq!(committed["result"]["durable"], true, "{committed}");
            } else {
                fixture.service.release_unexecuted_external_usage(&old);
            }
            assert_eq!(
                fixture
                    .service
                    .external_usage_reservation_counts_for_tests()
                    .0,
                1
            );
            let native = new.clone();
            let result = tokio::task::spawn_blocking(move || native.execute())
                .await
                .unwrap();
            let committed: serde_json::Value =
                serde_json::from_str(&fixture.service.complete_external_usage(new, result))
                    .unwrap();
            assert_eq!(committed["result"]["durable"], true, "{committed}");
            assert_eq!(
                fixture
                    .service
                    .external_usage_reservation_counts_for_tests(),
                (0, 0)
            );
            assert_eq!(
                fixture
                    .service
                    .external_token_usage(None)
                    .values()
                    .map(|usage| usage.input_tokens)
                    .sum::<u64>(),
                if cancel { 7 } else { 17 }
            );
        }
    }
}

/// Runs one source-owned enrollment through actual off-actor native observation.
/// Rejects unsuccessful admission here so an error reply cannot become a null
/// readiness credential. Diagnostics never print the private enrollment reply.
async fn enroll(fixture: &mut Fixture, request: &JsonRpcRequest) -> serde_json::Value {
    let work = fixture
        .service
        .prepare_external_enrollment(request, &fixture.connection)
        .unwrap();
    let native = work.clone();
    let result = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    let response: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, result, &fixture.connection),
    )
    .unwrap();
    assert!(
        response.get("error").is_none() && response["result"]["registered"] == true,
        "source enrollment failed before accounting readiness"
    );
    response
}

/// Supplies only the native handle and opaque original marker; origin and source
/// accounting namespace are daemon-owned facts, never supplied request fields.
pub(super) fn begin(receipt: &serde_json::Value, marker: &str) -> JsonRpcRequest {
    crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"begin","method":"agent/external/usage-begin","params":{
        "launch_token":receipt["result"]["launch_token"],"generation":receipt["result"]["generation"],
        "external_session_id":receipt["result"]["external_session_id"],"request_id":marker,
    }}).to_string()).unwrap()
}

/// Executes the production durable readiness worker and exact actor settlement.
pub(super) async fn ready(fixture: &mut Fixture, request: &JsonRpcRequest) -> serde_json::Value {
    let work = fixture
        .service
        .prepare_external_usage_begin(request, &fixture.connection)
        .unwrap();
    let native = work.clone();
    let result = tokio::task::spawn_blocking(move || native.execute())
        .await
        .unwrap();
    serde_json::from_str(&fixture.service.complete_external_usage_begin(
        work,
        result,
        &fixture.connection,
    ))
    .unwrap()
}

/// Executes one immutable normalized observation, including storage conflicts.
pub(super) async fn finish(fixture: &mut Fixture, request: &JsonRpcRequest) -> serde_json::Value {
    let work = fixture
        .service
        .prepare_external_usage_end(request, &fixture.connection)
        .unwrap();
    let native = work.clone();
    let result = tokio::task::spawn_blocking(move || native.execute())
        .await
        .unwrap();
    serde_json::from_str(&fixture.service.complete_external_usage(work, result)).unwrap()
}

/// A committed report whose ACK is lost remains inert through explicit same-live
/// recovery. Original trusted source project/time/witness and report clock are
/// conserved despite changed pane cwd/trust focus; changed payload conflicts.
/// A distinct new report commits once. Resetting the restored pane view does not
/// erase historical ledger expense or let that old report be charged again.
#[tokio::test(flavor = "current_thread")]
async fn external_source_resume_request_origin_replay_and_distinct_report_conserve_expense() {
    use crate::security::project::{ProjectTrustStore, TrustDecision};
    let directory = std::env::temp_dir().join(format!(
        "mez-source-origin-{}",
        crate::storage::token_usage::new_token_usage_event_id()
    ));
    let a = directory.join("original");
    let b = directory.join("later");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let Some(mut fixture) = fixture_with_start_directory("hold", Some(&a)).await else {
        std::fs::remove_dir_all(directory).unwrap();
        return;
    };
    let path = fixture.directory.join("usage.sqlite");
    let store = crate::storage::token_usage::TokenUsageStore::new(&path);
    let mut trust = ProjectTrustStore::default();
    trust
        .decide(a.clone(), TrustDecision::Trusted, None)
        .unwrap();
    trust
        .decide(b.clone(), TrustDecision::Trusted, None)
        .unwrap();
    let inventory = store
        .prepare_accounting_projects(&trust.records().cloned().collect::<Vec<_>>())
        .unwrap();
    let original_project = inventory
        .iter()
        .find(|row| row.root == a.canonicalize().unwrap())
        .unwrap()
        .id
        .clone()
        .unwrap();
    fixture.service.set_project_trust_store(trust, None);
    fixture.service.set_token_usage_store(store.clone());
    fixture
        .service
        .persistence
        .install_accounting_projects(&store, Some(inventory));
    let first = enroll(&mut fixture, &request()).await;
    let marker = "a".repeat(64);
    let original = ready(&mut fixture, &begin(&first, &marker)).await;
    let observed_at = crate::runtime::current_unix_seconds();
    let mut params: serde_json::Value =
        serde_json::from_str(begin(&first, &marker).params.as_deref().unwrap()).unwrap();
    params["event_id"] = "b".repeat(64).into();
    params["observed_at"] = observed_at.into();
    params["provider"] = "fixture".into();
    params["model"] = "actual-model".into();
    params["counters"] =
        serde_json::json!({"input_tokens":10,"output_tokens":3,"cached_input_tokens":0});
    let mut end = crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"end","method":"agent/external/usage-end","params":params}).to_string()).unwrap();
    let committed = finish(&mut fixture, &end).await;
    assert_eq!(committed["result"]["durable"], true, "{committed}");
    assert_eq!(committed["result"]["applied"], true);
    assert!(fixture.service.reset_external_token_usage("%1"));
    let session_before = fixture.service.external_token_usage(None);
    assert_eq!(
        fixture
            .service
            .external_token_usage(Some("%1"))
            .values()
            .map(|usage| usage.input_tokens)
            .sum::<u64>(),
        0
    );
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
    *fixture.service.control.external_agents_mut() = Default::default();
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
    fixture.service.set_pane_current_working_directory("%1", b);
    let mut recovery = request();
    let mut enrollment: serde_json::Value =
        serde_json::from_str(recovery.params.as_deref().unwrap()).unwrap();
    enrollment["source_resume"] = serde_json::json!({"observer_witness":first["result"]["observer_witness"],"generation":first["result"]["generation"]});
    recovery.params = Some(enrollment.to_string());
    let second = enroll(&mut fixture, &recovery).await;
    assert_eq!(second["result"]["registered"], true, "{second}");
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
    let restored = ready(&mut fixture, &begin(&second, &marker)).await;
    for field in ["source_witness", "admitted_at", "origin", "request_id"] {
        assert_eq!(
            restored["result"][field], original["result"][field],
            "{field}"
        );
    }
    let mut params: serde_json::Value =
        serde_json::from_str(end.params.as_deref().unwrap()).unwrap();
    params["launch_token"] = second["result"]["launch_token"].clone();
    params["generation"] = second["result"]["generation"].clone();
    end.params = Some(params.to_string());
    let replay = finish(&mut fixture, &end).await;
    assert_eq!(replay["result"]["durable"], true, "{replay}");
    assert_eq!(replay["result"]["applied"], false);
    assert_eq!(fixture.service.external_token_usage(None), session_before);
    assert_eq!(
        fixture
            .service
            .external_token_usage(Some("%1"))
            .values()
            .map(|usage| usage.input_tokens)
            .sum::<u64>(),
        0
    );
    ready(&mut fixture, &begin(&second, &marker)).await;
    let mut changed = end.clone();
    params["counters"]["input_tokens"] = 11.into();
    changed.params = Some(params.to_string());
    assert!(finish(&mut fixture, &changed).await.get("error").is_some());
    assert!(!fixture.service.reset_external_token_usage("%1"));
    let fresh = "c".repeat(64);
    ready(&mut fixture, &begin(&second, &fresh)).await;
    params["request_id"] = fresh.clone().into();
    params["event_id"] = "d".repeat(64).into();
    params["observed_at"] = crate::runtime::current_unix_seconds().into();
    params["counters"]["input_tokens"] = 7.into();
    let mut fresh_end = end.clone();
    fresh_end.params = Some(params.to_string());
    assert_eq!(
        finish(&mut fixture, &fresh_end).await["result"]["applied"],
        true
    );
    ready(&mut fixture, &begin(&second, &fresh)).await;
    assert_eq!(
        finish(&mut fixture, &fresh_end).await["result"]["applied"],
        false
    );
    assert_eq!(
        fixture
            .service
            .external_token_usage(Some("%1"))
            .values()
            .map(|usage| usage.input_tokens)
            .sum::<u64>(),
        7
    );
    let database = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM external_usage_sources", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        database
            .query_row(
                "SELECT count(*) FROM external_usage_observations",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        2
    );
    assert_eq!(
        database
            .query_row(
                "SELECT sum(input_tokens) FROM token_usage_events",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        17
    );
    assert_eq!(
        database
            .query_row(
                "SELECT project_id FROM external_usage_requests WHERE owner=?1 AND request_id=?2",
                rusqlite::params![owner, marker],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        original_project.as_str()
    );
    assert_eq!(
        database
            .query_row(
                "SELECT min(observed_at) FROM token_usage_events",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        i64::try_from(observed_at).unwrap()
    );
    drop(database);
    drop(fixture);
    std::fs::remove_dir_all(directory).unwrap();
}

/// An actual native capture refusal must fail at enrollment, before an error
/// reply can supply null credential fields to the readiness request builder.
/// Forced expiry is instance-local and does not relax or race a production
/// deadline. The static panic excludes private receipt/capability diagnostics;
/// failed admission must also release its reservation without a live binding.
#[tokio::test(flavor = "current_thread")]
async fn external_source_recovery_enrollment_refusal_precedes_readiness() {
    use futures_util::FutureExt;

    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    fixture.service.sync_external_ancestry_budget();
    let budget = fixture
        .service
        .control
        .external_agents()
        .enrollments
        .ancestry_budget
        .clone();
    budget.force_capture_expiry_at_check_for_tests(1);
    let result = std::panic::AssertUnwindSafe(enroll(&mut fixture, &request()))
        .catch_unwind()
        .await;
    assert_eq!(budget.capture_expiry_observation_for_tests(), (1, 0));
    assert_eq!(budget.reserved(), 0);
    let Err(failure) = result else {
        panic!("failed native enrollment reached the readiness caller");
    };
    let message = failure
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| failure.downcast_ref::<String>().map(String::as_str));
    assert_eq!(
        message,
        Some("source enrollment failed before accounting readiness")
    );
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .is_empty()
    );
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .enrollments
            .pending
            .is_empty()
    );
}
