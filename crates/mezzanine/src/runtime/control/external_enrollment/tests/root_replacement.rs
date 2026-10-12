//! Actual pane-root replacement with new ordinary native creator/helper evidence.
//!
//! The fixture terminates a real pane root and starts another through production
//! process ownership, then invokes a fresh producer normally. It does not borrow
//! old PID/token/observer authority, simulate snapshot metadata, or claim that a
//! new physical source can attach unobserved historical vendor reports.

use super::curated_admission::enroll;
use super::curated_usage::{begin, end};
use super::helper_presentation::{child, settle};
use super::*;

/// Replacing the actual root/creator must invalidate queued and public old
/// observations, keep already committed expense, and admit a newly invoked
/// creator only under its own fresh native namespace. A distinct report charges
/// once under that new owner; it cannot borrow an old request origin. This is
/// actual pane-process replacement, not a full daemon/snapshot resume workflow.
#[tokio::test(flavor = "current_thread")]
async fn external_curated_actual_root_replacement_has_fresh_native_owner() {
    let Some(mut fixture) = fixture("hold").await else {
        panic!("root replacement qualification requires native peer evidence");
    };
    let path = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
    let original_root = fixture.service.pane_process_identity("%1").unwrap();
    let original_creator = fixture.connection.unix_origin().unwrap().identity;
    let (_old_socket, old_connection) = child(&mut fixture).await;
    let first = enroll(&mut fixture, &old_connection).await;
    assert_eq!(first["result"]["registered"], true);
    let old_owner = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap()
        .accounting_owner
        .clone();
    let work = fixture
        .service
        .prepare_external_helper_presentation(&begin(&first), &old_connection)
        .unwrap();
    let ready = settle(&mut fixture, work, &old_connection).await;
    assert_eq!(ready["result"]["accepted"], true, "{ready}");
    let old_report = end(&first, ready["result"]["admitted_at"].as_u64().unwrap());
    let work = fixture
        .service
        .prepare_external_helper_presentation(&old_report, &old_connection)
        .unwrap();
    let committed = settle(&mut fixture, work, &old_connection).await;
    assert_eq!(committed["result"]["durable"], true, "{committed}");
    let heartbeat = crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"old-root-proof","method":"agent/external/curated-heartbeat","params":{
        "harness":"claude","generation":first["result"]["generation"],"observer_witness":first["result"]["observer_witness"],"external_session_id":"session-a","sequence":1}}).to_string()).unwrap();
    let queued = fixture
        .service
        .prepare_external_helper_presentation(&heartbeat, &old_connection)
        .unwrap();
    let native = queued.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(observed.is_ok());
    fixture.service.terminate_all_pane_processes().unwrap();
    fixture.service.start_initial_pane_process(None).unwrap();
    let fresh_root = fixture.service.pane_process_identity("%1").unwrap();
    assert!(!fresh_root.same_incarnation(&original_root));
    assert_ne!(
        (fresh_root.process_id, fresh_root.start_token),
        (original_root.process_id, original_root.start_token)
    );
    let refused: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(queued, observed, &old_connection),
    )
    .unwrap();
    assert!(
        refused.get("error").is_some(),
        "queued old root work was accepted: {refused}"
    );
    fixture.service.reconcile_external_agent_registrations();
    assert!(fixture.service.external_agent_rows().is_empty());
    let (socket, connection) =
        launch_ordinary_fixture(&mut fixture.service, &fixture.listener, "hold")
            .await
            .unwrap();
    fixture.socket = socket;
    fixture.connection = connection;
    let fresh_creator = fixture.connection.unix_origin().unwrap().identity;
    assert_ne!(
        (fresh_creator.process_id, fresh_creator.start_token),
        (original_creator.process_id, original_creator.start_token)
    );
    let (_new_socket, new_connection) = child(&mut fixture).await;
    let fresh = enroll(&mut fixture, &new_connection).await;
    assert_eq!(fresh["result"]["registered"], true, "{fresh}");
    assert_ne!(
        fresh["result"]["observer_witness"],
        first["result"]["observer_witness"]
    );
    let new_owner = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .find(|binding| !binding.retired)
        .unwrap()
        .accounting_owner
        .clone();
    assert_ne!(new_owner, old_owner);
    assert_eq!(fixture.service.external_agent_rows().len(), 1);
    for old in [heartbeat, begin(&first), old_report] {
        assert!(
            fixture
                .service
                .prepare_external_helper_presentation(&old, &new_connection)
                .is_err()
        );
    }
    let mut new_begin = begin(&fresh);
    let mut params: serde_json::Value =
        serde_json::from_str(new_begin.params.as_deref().unwrap()).unwrap();
    params["request_id"] = "c".repeat(64).into();
    new_begin.params = Some(params.to_string());
    let mut work = fixture
        .service
        .prepare_external_helper_presentation(&new_begin, &new_connection)
        .unwrap();
    let capture = crate::runtime::current_unix_seconds().saturating_sub(500);
    assert!(work.fixture_usage_capture_at(capture));
    let new_ready = settle(&mut fixture, work, &new_connection).await;
    assert_eq!(new_ready["result"]["accepted"], true, "{new_ready}");
    assert_eq!(new_ready["result"]["admitted_at"], capture);
    let mut new_end = end(&fresh, new_ready["result"]["admitted_at"].as_u64().unwrap());
    let mut params: serde_json::Value =
        serde_json::from_str(new_end.params.as_deref().unwrap()).unwrap();
    params["request_id"] = "c".repeat(64).into();
    params["event_id"] = "d".repeat(64).into();
    new_end.params = Some(params.to_string());
    for (applied, elapsed) in [(true, 100), (false, 200)] {
        // Report recovery repeats only the original metadata begin, never
        // vendor work, and must recover its already frozen request boundary.
        let mut work = fixture
            .service
            .prepare_external_helper_presentation(&new_begin, &new_connection)
            .unwrap();
        assert!(work.fixture_usage_capture_at(capture + elapsed));
        let ready_again = settle(&mut fixture, work, &new_connection).await;
        assert_eq!(ready_again["result"]["accepted"], true, "{ready_again}");
        assert_eq!(
            ready_again["result"]["admitted_at"],
            new_ready["result"]["admitted_at"]
        );
        let work = fixture
            .service
            .prepare_external_helper_presentation(&new_end, &new_connection)
            .unwrap();
        let receipt = settle(&mut fixture, work, &new_connection).await;
        assert_eq!(receipt["result"]["durable"], true, "{receipt}");
        assert_eq!(receipt["result"]["applied"], applied);
    }
    assert_eq!(
        fixture
            .service
            .external_usage_reservation_counts_for_tests(),
        (0, 0)
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
    assert!(!new_connection.initialized());
    assert!(new_connection.caller_client_id().is_none());
    let database = rusqlite::Connection::open(path).unwrap();
    for owner in [&old_owner, &new_owner] {
        assert_eq!(
            database
                .query_row(
                    "SELECT count(*) FROM external_usage_observations WHERE owner=?1",
                    [owner],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
    }
    assert_eq!(
        database
            .query_row(
                "SELECT sum(input_tokens) FROM token_usage_events",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        20
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
                "SELECT count(DISTINCT owner) FROM external_usage_sources",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        2
    );
    assert_eq!(
        database
            .query_row(
                "SELECT count(DISTINCT owner) FROM external_usage_observations",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        2
    );
}
