//! Native same-pane creators with colliding self-reported session selectors.
//!
//! Real helper writers and retained creator/root lifetimes remain authority.
//! Public receipts identify an original candidate only; they cannot transfer
//! origin, request readiness or committed expense to another creator.

use super::curated_admission::enroll;
use super::curated_heartbeat::heartbeat;
use super::curated_usage::{begin, end};
use super::helper_presentation::{child, child_of, settle};
use super::*;

/// Two distinct live creators under one actual pane root may report the same
/// session label without merging accounting or blocking exact original receipts.
/// Foreign receipt reuse fails native observation; history without a new begin
/// is excluded, while a genuinely newly admitted request commits exactly once.
/// This fixture qualifies native isolation, not real vendor resume/fork APIs.
#[tokio::test(flavor = "current_thread")]
async fn external_curated_request_same_pane_creators_keep_origins_and_expense_disjoint() {
    let Some(mut fixture) = fixture("hold").await else {
        panic!("creator isolation requires native peer evidence");
    };
    let path = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
    let (mut first_socket, first_connection) = child(&mut fixture).await;
    let first = enroll(&mut fixture, &first_connection).await;
    assert_eq!(first["result"]["registered"], true);
    let work = fixture
        .service
        .prepare_external_helper_presentation(&begin(&first), &first_connection)
        .unwrap();
    let ready = settle(&mut fixture, work, &first_connection).await;
    assert_eq!(ready["result"]["accepted"], true, "{ready}");
    let original_end = end(&first, ready["result"]["admitted_at"].as_u64().unwrap());
    let work = fixture
        .service
        .prepare_external_helper_presentation(&original_end, &first_connection)
        .unwrap();
    let committed = settle(&mut fixture, work, &first_connection).await;
    assert_eq!(committed["result"]["durable"], true, "{committed}");
    assert_eq!(committed["result"]["applied"], true);

    // The first helper also acts as a second independent declared creator.
    // Its new child proves that creator's own writer/parent chain natively.
    let (_second_socket, second_connection) = child_of(&mut first_socket, &fixture.listener).await;
    let second = enroll(&mut fixture, &second_connection).await;
    assert_eq!(second["result"]["registered"], true, "{second}");
    assert_ne!(
        first["result"]["observer_witness"],
        second["result"]["observer_witness"]
    );
    let registry = fixture.service.control.external_agents();
    assert_eq!(registry.bindings.len(), 2);
    let owners: BTreeSet<_> = registry
        .bindings
        .values()
        .map(|binding| binding.accounting_owner.clone())
        .collect();
    assert_eq!(owners.len(), 2);
    assert!(
        registry
            .bindings
            .values()
            .all(|binding| binding.pane_id == "%1")
    );
    assert!(
        registry
            .enrollments
            .helper_targets
            .select(
                second_connection.unix_origin().unwrap().uid(),
                "claude",
                "session-a",
            )
            .is_err(),
        "tuple-only discovery must remain ambiguous"
    );

    let work = fixture
        .service
        .prepare_external_helper_presentation(&begin(&first), &first_connection)
        .unwrap();
    let restored = settle(&mut fixture, work, &first_connection).await;
    assert_eq!(
        restored["result"], ready["result"],
        "original readiness must retain first origin"
    );
    for foreign in [begin(&first), original_end.clone(), heartbeat(&first, 1)] {
        let work = fixture
            .service
            .prepare_external_helper_presentation(&foreign, &second_connection)
            .unwrap();
        let rejected = settle(&mut fixture, work, &second_connection).await;
        assert!(
            rejected.get("error").is_some(),
            "foreign receipt accepted: {rejected}"
        );
    }
    for (receipt, connection) in [(&first, &first_connection), (&second, &second_connection)] {
        let work = fixture
            .service
            .prepare_external_helper_presentation(&heartbeat(receipt, 1), connection)
            .unwrap();
        let proof = settle(&mut fixture, work, connection).await;
        assert_eq!(proof["result"]["observed"], true, "{proof}");
        assert_eq!(proof["result"]["changed"], true, "{proof}");
    }
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(
                &end(&second, ready["result"]["admitted_at"].as_u64().unwrap()),
                &second_connection,
            )
            .is_err(),
        "a new creator cannot import completed history without request readiness"
    );

    let work = fixture
        .service
        .prepare_external_helper_presentation(&begin(&second), &second_connection)
        .unwrap();
    let second_ready = settle(&mut fixture, work, &second_connection).await;
    assert_eq!(second_ready["result"]["accepted"], true, "{second_ready}");
    let mut fresh_end = end(
        &second,
        second_ready["result"]["admitted_at"].as_u64().unwrap(),
    );
    let mut params: serde_json::Value =
        serde_json::from_str(fresh_end.params.as_deref().unwrap()).unwrap();
    params["event_id"] = serde_json::json!("c".repeat(64));
    fresh_end.params = Some(params.to_string());
    for (request, receipt, connection, applied) in [
        (&fresh_end, &second, &second_connection, true),
        (&fresh_end, &second, &second_connection, false),
        (&original_end, &first, &first_connection, false),
    ] {
        let work = fixture
            .service
            .prepare_external_helper_presentation(&begin(receipt), connection)
            .unwrap();
        let ready = settle(&mut fixture, work, connection).await;
        assert_eq!(ready["result"]["accepted"], true, "{ready}");
        let work = fixture
            .service
            .prepare_external_helper_presentation(request, connection)
            .unwrap();
        let reply = settle(&mut fixture, work, connection).await;
        assert_eq!(reply["result"]["durable"], true, "{reply}");
        assert_eq!(reply["result"]["applied"], applied, "{reply}");
        assert!(!connection.initialized());
        assert!(connection.caller_client_id().is_none());
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
    let database = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        database
            .query_row(
                "SELECT sum(input_tokens) FROM token_usage_events",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        20
    );
    assert_eq!(
        database
            .query_row(
                "SELECT count(DISTINCT owner) FROM external_usage_requests",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        2
    );
    assert_eq!(
        database
            .query_row(
                "SELECT count(*) FROM external_usage_observations",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        2
    );
}
