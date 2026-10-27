//! Native request accounting across cache pressure and full control replacement.
//!
//! Genuine ordinary-process enrollment and publication own the source. Every
//! marker and observation uses the production worker and actor settlement; no
//! SQL source injection or synthetic successful proof enables accounting. The
//! short case runs normally, while the explicit long case crosses 4,096 real
//! requests. Neither case claims vendor SDK or never-received-report recovery.

use super::source_recovery_usage::{begin, finish, ready, report};
use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A small two-model source verifies the same production path as the long
/// qualification: durable reset, actual cold replacement, original replay and
/// conflicting/new observations conserve expense without per-message streams.
#[tokio::test(flavor = "current_thread")]
async fn external_native_source_capacity_and_cold_replay_conserve_expense() {
    qualify_native_capacity(9).await;
}

/// More than4,096 genuinely admitted requests must use bounded model streams
/// and reconstructible readiness, rather than fail at a per-message projection
/// limit. The first marker really leaves the cache before cold recovery/replay.
/// This explicitly requested offline workload uses only fixture-owned expense.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit 4097-request native enrollment/ledger/cold recovery workload"]
async fn external_native_source_exceeds_4096_requests_without_historical_recharge() {
    qualify_native_capacity(4097).await;
}

/// Exercises both budgets with actual source lifetime maintenance. A missing
/// ordinary fixture fails qualification instead of silently reporting success.
async fn qualify_native_capacity(count: u64) {
    let (mut fixture, store, publication, enrollment) =
        super::source_activation_fixture::accepted_source()
            .await
            .expect("native accounting qualification requires its ordinary fixture");
    fixture.service.set_external_usage_stream_limit_for_tests(2);
    let first = serde_json::json!({"result":enrollment});
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
    let original_marker = format!("{:064x}", 0);
    let mut original_ready = None;
    let mut original_report = None;
    let mut checkpoint = first["result"]["source_receipt"].clone();
    for index in 0..count {
        if index % 64 == 0 {
            checkpoint = maintain_source(&mut fixture, &publication, &first).await;
        }
        let marker = format!("{index:064x}");
        let readiness = ready(&mut fixture, &begin(&first, &marker)).await;
        assert_eq!(readiness["result"]["accepted"], true);
        let request = report(
            &first,
            &marker,
            &format!("{:064x}", index + 10_000),
            if index % 2 == 0 { "model-a" } else { "model-b" },
            1,
        );
        let acknowledgment = finish(&mut fixture, &request).await;
        assert_eq!(acknowledgment["result"]["durable"], true);
        assert_eq!(acknowledgment["result"]["applied"], true);
        if index == 0 {
            // The test deliberately loses this already-committed ACK, retaining
            // the exact original normalized report for a later real retry.
            original_ready = Some(readiness);
            original_report = Some(request);
        }
        assert_eq!(
            fixture.service.external_usage_projection_counts_for_tests(),
            (if index == 0 { 1 } else { 2 }, 0)
        );
        assert!(
            fixture
                .service
                .control
                .external_usage_begins()
                .origins
                .len()
                <= 4096
        );
        assert_eq!(
            fixture
                .service
                .external_usage_reservation_counts_for_tests(),
            (0, 0)
        );
    }
    assert_eq!(input(&fixture.service, None), count);
    assert_eq!(ledger(&store), (count, count));
    if count > 4096 {
        // Completed reports consume readiness. Reconstruct the original from
        // durable metadata, then admit genuinely unreported markers until the
        // bounded cache evicts it; none of these begins creates expense.
        assert_eq!(
            ready(&mut fixture, &begin(&first, &original_marker)).await["result"],
            original_ready.as_ref().unwrap()["result"]
        );
        for index in 0..4096 {
            if index % 64 == 0 {
                checkpoint = maintain_source(&mut fixture, &publication, &first).await;
            }
            let marker = format!("a{index:063x}");
            assert_eq!(
                ready(&mut fixture, &begin(&first, &marker)).await["result"]["accepted"],
                true
            );
        }
        assert_eq!(
            fixture
                .service
                .control
                .external_usage_begins()
                .origins
                .len(),
            4096
        );
        assert!(
            !fixture
                .service
                .control
                .external_usage_begins()
                .origins
                .contains_key(&(owner.clone(), original_marker.clone()))
        );
        let restored = ready(&mut fixture, &begin(&first, &original_marker)).await;
        assert_eq!(
            restored["result"],
            original_ready.as_ref().unwrap()["result"]
        );
        assert_eq!(ledger(&store), (count, count));
    }
    let reset = fixture.service.prepare_external_usage_reset("%1").unwrap();
    let worker = reset.clone();
    tokio::task::spawn_blocking(move || worker.execute())
        .await
        .unwrap()
        .unwrap();
    fixture
        .service
        .complete_external_usage_reset(&reset)
        .unwrap();
    assert_eq!(input(&fixture.service, Some("%1")), 0);
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
    fixture.service.set_external_usage_stream_limit_for_tests(2);
    let mut recovery = super::request();
    let mut params: serde_json::Value =
        serde_json::from_str(recovery.params.as_deref().unwrap()).unwrap();
    params["observer_instance"] = "native-capacity-cold-instance".into();
    params["durable_source_resume"] = serde_json::json!({
        "receipt":checkpoint,"operation_id":"e".repeat(64),
    });
    recovery.params = Some(params.to_string());
    let recovered = {
        let _observer = crate::runtime::UnixOriginStream::new(
            &mut fixture.socket,
            Some(publication.origin.clone()),
        );
        super::cold_source_recovery::enroll(&mut fixture.service, &fixture.connection, &recovery)
            .await
    };
    assert_eq!(recovered["result"]["registered"], true);
    let current_owner = &fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap()
        .accounting_owner;
    assert!(
        current_owner == &owner,
        "cold recovery changed the original namespace"
    );
    assert_eq!(input(&fixture.service, None), count);
    assert_eq!(input(&fixture.service, Some("%1")), 0);
    assert_eq!(
        fixture.service.external_usage_projection_counts_for_tests(),
        (2, 0)
    );
    let restored = ready(&mut fixture, &begin(&recovered, &original_marker)).await;
    assert_eq!(restored["result"], original_ready.unwrap()["result"]);
    let mut old = original_report.unwrap();
    let mut params: serde_json::Value =
        serde_json::from_str(old.params.as_deref().unwrap()).unwrap();
    params["launch_token"] = recovered["result"]["launch_token"].clone();
    params["generation"] = recovered["result"]["generation"].clone();
    old.params = Some(params.to_string());
    assert_eq!(finish(&mut fixture, &old).await["result"]["applied"], false);
    assert_eq!(ledger(&store), (count, count));
    assert_eq!(
        ready(&mut fixture, &begin(&recovered, &original_marker)).await["result"]["accepted"],
        true
    );
    params["counters"]["input_tokens"] = 2.into();
    let mut conflict = old.clone();
    conflict.params = Some(params.to_string());
    assert!(finish(&mut fixture, &conflict).await.get("error").is_some());
    assert_eq!(ledger(&store), (count, count));
    let marker = format!("{count:064x}");
    assert_eq!(
        ready(&mut fixture, &begin(&recovered, &marker)).await["result"]["accepted"],
        true
    );
    let new = report(
        &recovered,
        &marker,
        &format!("{:064x}", count + 10_000),
        "model-a",
        1,
    );
    assert_eq!(finish(&mut fixture, &new).await["result"]["applied"], true);
    assert_eq!(
        ready(&mut fixture, &begin(&recovered, &marker)).await["result"]["accepted"],
        true
    );
    assert_eq!(finish(&mut fixture, &new).await["result"]["applied"], false);
    assert_eq!(input(&fixture.service, None), count + 1);
    assert_eq!(input(&fixture.service, Some("%1")), 1);
    assert_eq!(ledger(&store), (count + 1, count + 1));
    assert_eq!(
        fixture.service.external_usage_projection_counts_for_tests(),
        (2, 0)
    );
}

/// Keeps the real child socket active and drives only genuinely due native
/// lifetime persistence. It never changes deadlines or supplies proof results.
async fn maintain_source(
    fixture: &mut Fixture,
    publication: &ExternalEnrollmentWork,
    receipt: &serde_json::Value,
) -> serde_json::Value {
    let mut observer = crate::runtime::UnixOriginStream::new(
        &mut fixture.socket,
        Some(publication.origin.clone()),
    );
    observer.write_all(&[3]).await.unwrap();
    let mut response = [0];
    observer.read_exact(&mut response).await.unwrap();
    assert_eq!(response, [1]);
    let renewal = crate::control::parse_json_rpc_request(
        &serde_json::json!({
            "jsonrpc":"2.0","id":"capacity-renew","method":"agent/external/renew","params":{
                "launch_token":receipt["result"]["launch_token"],
                "generation":receipt["result"]["generation"],"external_session_id":"session-a",
            },
        })
        .to_string(),
    )
    .unwrap();
    fixture
        .service
        .dispatch_external_agent_request(&renewal, &fixture.connection)
        .unwrap();
    fixture.service.renew_connected_external_observers();
    for effect in fixture.service.persistence.take_token_usage_effects() {
        let crate::runtime::RuntimeSideEffect::PersistExternalSourceRenewal { work } = effect
        else {
            panic!("unexpected accounting persistence family");
        };
        assert!(work.execute_persistence().await);
        assert_eq!(
            fixture.service.complete_external_source_renewal(&work),
            Some(true)
        );
    }
    let result: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .dispatch_external_agent_request(&renewal, &fixture.connection)
            .unwrap(),
    )
    .unwrap();
    assert!(result["source_receipt"].is_object());
    result["source_receipt"].clone()
}

/// Reads only normalized fixture ledger expense and event cardinality.
fn ledger(store: &crate::storage::token_usage::TokenUsageStore) -> (u64, u64) {
    let counters: (i64, i64) = rusqlite::Connection::open(store.path())
        .unwrap()
        .query_row(
            "SELECT coalesce(sum(input_tokens),0),count(*) FROM token_usage_events",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    (
        counters.0.try_into().unwrap(),
        counters.1.try_into().unwrap(),
    )
}

/// Sums the production absolute projection, including its durable pane baseline.
fn input(service: &RuntimeSessionService, pane: Option<&str>) -> u64 {
    service
        .external_token_usage(pane)
        .values()
        .map(|usage| usage.input_tokens)
        .sum()
}
