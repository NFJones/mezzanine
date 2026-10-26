//! Cold accounting projection conservation uses real native evidence and actual
//! committed reports, never replayed payloads as a substitute for stored totals.

use super::*;

/// Full control loss discards the old projection owner, not incurred expense.
/// Two response-model partitions must be visible immediately after qualified
/// cold publication, including the model whose report is NEVER replayed.
#[tokio::test(flavor = "current_thread")]
async fn external_cold_source_restores_unreplayed_model_partitions_before_reporting() {
    use super::source_recovery_usage::{begin, finish, ready, report};
    let Some((mut fixture, store, publication, first)) =
        super::source_activation_fixture::accepted_source().await
    else {
        return;
    };
    let first = serde_json::json!({"result":first});
    for (request, event, model, input) in [
        ("a".repeat(64), "b".repeat(64), "model-a", 10),
        ("c".repeat(64), "d".repeat(64), "unreplayed-model", 5),
    ] {
        assert_eq!(
            ready(&mut fixture, &begin(&first, &request)).await["result"]["accepted"],
            true
        );
        assert_eq!(
            finish(
                &mut fixture,
                &report(&first, &request, &event, model, input)
            )
            .await["result"]["applied"],
            true
        );
    }
    let before = fixture.service.external_token_usage(None);
    assert_eq!(
        before.values().map(|usage| usage.input_tokens).sum::<u64>(),
        15
    );
    fixture.service.retire_all_external_source_anchors();
    fixture.service.control = crate::runtime::control::RuntimeControlComponent::new(
        crate::control::ControlIdempotencyCache::default(),
        mez_agent::messaging::MessageService::default(),
        None,
    );
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(
            store.path(),
        ));
    let mut recovery = request();
    let mut params: serde_json::Value =
        serde_json::from_str(recovery.params.as_deref().unwrap()).unwrap();
    params["observer_instance"] = serde_json::json!("cold-projection-successor");
    params["durable_source_resume"] = serde_json::json!({"receipt":first["result"]["source_receipt"],"operation_id":"e".repeat(64)});
    recovery.params = Some(params.to_string());
    fixture.service.set_external_usage_stream_limit_for_tests(1);
    let refused = {
        let _observer = crate::runtime::UnixOriginStream::new(
            &mut fixture.socket,
            Some(publication.origin.clone()),
        );
        super::cold_source_recovery::enroll(&mut fixture.service, &fixture.connection, &recovery)
            .await
    };
    assert_eq!(
        refused["error"]["message"],
        "external source checkpoint projection capacity unavailable"
    );
    assert!(
        fixture.service.external_token_usage(None).is_empty(),
        "capacity failure installed a partial source projection"
    );
    assert_eq!(
        fixture
            .service
            .external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
    // The SQL claim may already be real. Retry ONLY the identical public intent;
    // capacity returning is not permission for another observer claim or charge.
    fixture.service.set_external_usage_stream_limit_for_tests(2);
    let recovered = {
        let _observer = crate::runtime::UnixOriginStream::new(
            &mut fixture.socket,
            Some(publication.origin.clone()),
        );
        super::cold_source_recovery::enroll(&mut fixture.service, &fixture.connection, &recovery)
            .await
    };
    assert_eq!(recovered["result"]["registered"], true);
    assert_eq!(
        fixture.service.external_token_usage(None),
        before,
        "cold publication dropped an unreplayed committed partition"
    );
    assert_eq!(
        fixture
            .service
            .external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
    let database = rusqlite::Connection::open(store.path()).unwrap();
    assert_eq!(
        database
            .query_row(
                "SELECT sum(input_tokens),count(*) FROM token_usage_events",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            )
            .unwrap(),
        (15, 2)
    );
}
