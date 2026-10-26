//! Completed native request markers retain independent evidence of observation
//! membership. Losing the alias is corruption, not an honestly empty source.

use super::*;

/// Commit an actual request/report, then corrupt ONLY its alias (delete or move
/// owner) while request marker/checkpoint/event survive. Fresh native proof and
/// an actual cold claim cannot turn that detectable gap into private readiness
/// or a partial projection. No caller-supplied successful native result is used.
#[tokio::test(flavor = "current_thread")]
async fn external_cold_source_missing_completed_request_alias_refuses_publication() {
    use super::source_recovery_usage::{begin, finish, ready, report};
    for (misown, partial) in [(false, false), (true, false), (false, true), (true, true)] {
        let Some((mut fixture, store, publication, first)) =
            super::source_activation_fixture::accepted_source().await
        else {
            return;
        };
        let first = serde_json::json!({"result":first});
        assert_eq!(
            ready(&mut fixture, &begin(&first, &"a".repeat(64))).await["result"]["accepted"],
            true
        );
        assert_eq!(
            finish(
                &mut fixture,
                &report(
                    &first,
                    &"a".repeat(64),
                    &"b".repeat(64),
                    "original-model",
                    10
                )
            )
            .await["result"]["applied"],
            true
        );
        let database = rusqlite::Connection::open(store.path()).unwrap();
        if partial {
            assert_eq!(
                ready(&mut fixture, &begin(&first, &"d".repeat(64))).await["result"]["accepted"],
                true
            );
            assert_eq!(
                finish(
                    &mut fixture,
                    &report(
                        &first,
                        &"d".repeat(64),
                        &"e".repeat(64),
                        "retained-model",
                        5
                    )
                )
                .await["result"]["applied"],
                true
            );
        }
        if misown {
            database.execute("UPDATE external_usage_observations SET owner='00000000-0000-4000-8000-000000000001' WHERE event_id=?1",["b".repeat(64)]).unwrap();
        } else {
            database
                .execute(
                    "DELETE FROM external_usage_observations WHERE event_id=?1",
                    ["b".repeat(64)],
                )
                .unwrap();
        }
        assert_eq!(
            database
                .query_row(
                    "SELECT count(*) FROM external_usage_requests WHERE observation_id IS NOT NULL",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            if partial { 2 } else { 1 }
        );
        assert_eq!(
            database
                .query_row("SELECT count(*) FROM external_usage_streams", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            if partial { 2 } else { 1 }
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
        params["observer_instance"] = serde_json::json!("cold-missing-alias");
        params["durable_source_resume"] = serde_json::json!({"receipt":first["result"]["source_receipt"],"operation_id":"c".repeat(64)});
        recovery.params = Some(params.to_string());
        let response = {
            let _observer = crate::runtime::UnixOriginStream::new(
                &mut fixture.socket,
                Some(publication.origin.clone()),
            );
            super::cold_source_recovery::enroll(
                &mut fixture.service,
                &fixture.connection,
                &recovery,
            )
            .await
        };
        assert!(
            response.get("error").is_some(),
            "completed request alias loss published a valid empty cold snapshot"
        );
        assert!(fixture.service.external_token_usage(None).is_empty());
        assert_eq!(
            fixture
                .service
                .external_usage_reservation_counts_for_tests(),
            (0, 0)
        );
        assert_eq!(
            database
                .query_row(
                    "SELECT count(*),sum(input_tokens) FROM token_usage_events",
                    [],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
                )
                .unwrap(),
            if partial { (2, 15) } else { (1, 10) }
        );
    }
}
