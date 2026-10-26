//! Fresh native proof after full control replacement, never namespace-only
//! activation or a stored/mock producer standing in for an actual live source.

use super::*;

/// Drives the actual native and required publication phases, including exact
/// already-published retry replies. This never supplies a fake successful proof.
pub(super) async fn enroll(
    service: &mut RuntimeSessionService,
    connection: &ControlConnectionState,
    request: &JsonRpcRequest,
) -> serde_json::Value {
    let work = service
        .prepare_external_enrollment(request, connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.execute_enrollment())
        .await
        .unwrap();
    let body = service.complete_external_enrollment(work.clone(), observed, connection);
    let body = match service.begin_external_source_publication(work, body) {
        Ok(publication) => {
            let native = publication.clone();
            let claimed = tokio::task::spawn_blocking(move || native.execute_enrollment())
                .await
                .unwrap();
            service.complete_external_source_publication(publication, claimed, connection)
        }
        Err(body) => body,
    };
    serde_json::from_str(&body).unwrap()
}

/// A committed original request is replayed byte-for-byte except transport
/// credentials after all control authority/store handles are replaced. The
/// original request boundary/owner/model/counters remain unchanged; reset is
/// not a recharge, and a distinct legitimate later request adds exactly once.
#[tokio::test(flavor = "current_thread")]
async fn external_cold_source_full_control_replacement_replays_original_report_once() {
    use super::source_recovery_usage::{begin, finish, ready as begin_ready, report};
    let Some((mut fixture, store, publication, first)) =
        super::source_activation_fixture::accepted_source().await
    else {
        return;
    };
    let first = serde_json::json!({"result":first});
    let original_ready = begin_ready(&mut fixture, &begin(&first, &"a".repeat(64))).await;
    assert_eq!(original_ready["result"]["accepted"], true);
    assert!(
        original_ready["result"]["admitted_at"]
            .as_u64()
            .is_some_and(|time| time > 0)
    );
    assert_eq!(
        original_ready["result"]["source_witness"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    assert!(original_ready["result"].get("origin").is_some());
    let frozen = report(
        &first,
        &"a".repeat(64),
        &"c".repeat(64),
        "original-model",
        10,
    );
    let first_ack = finish(&mut fixture, &frozen).await;
    assert_eq!(first_ack["result"]["durable"], true);
    assert_eq!(first_ack["result"]["applied"], true);
    let old_ready_work = fixture
        .service
        .prepare_external_usage_begin(&begin(&first, &"d".repeat(64)), &fixture.connection)
        .unwrap();
    let native = old_ready_work.clone();
    let old_ready_result = tokio::task::spawn_blocking(move || native.execute())
        .await
        .unwrap();
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
    params["observer_instance"] = serde_json::json!("fresh-cold-instance");
    params["durable_source_resume"] = serde_json::json!({"receipt":first["result"]["source_receipt"],"operation_id":"b".repeat(64)});
    recovery.params = Some(params.to_string());
    let recovered = {
        let _observer = crate::runtime::UnixOriginStream::new(
            &mut fixture.socket,
            Some(publication.origin.clone()),
        );
        let recovered = enroll(&mut fixture.service, &fixture.connection, &recovery).await;
        assert_eq!(recovered["result"]["registered"], true);
        let retry = enroll(&mut fixture.service, &fixture.connection, &recovery).await;
        assert_eq!(
            retry["result"]["launch_token"], recovered["result"]["launch_token"],
            "exact cold reply retry replaced its handle"
        );
        assert_eq!(
            retry["result"]["source_receipt"],
            recovered["result"]["source_receipt"]
        );
        let mut stale = request();
        let mut stale_params: serde_json::Value =
            serde_json::from_str(stale.params.as_deref().unwrap()).unwrap();
        stale_params["predecessor_generation"] = recovered["result"]["generation"].clone();
        stale.params = Some(stale_params.to_string());
        assert!(
            enroll(&mut fixture.service, &fixture.connection, &stale)
                .await
                .get("error")
                .is_some(),
            "old instance exploited reused private generation"
        );
        let mut bare = request();
        let mut bare_params: serde_json::Value =
            serde_json::from_str(bare.params.as_deref().unwrap()).unwrap();
        bare_params["observer_instance"] = serde_json::json!("fresh-cold-instance");
        bare.params = Some(bare_params.to_string());
        assert!(
            enroll(&mut fixture.service, &fixture.connection, &bare)
                .await
                .get("error")
                .is_some(),
            "bare initial request recovered a cold handle"
        );
        recovered
    };
    let current_ready_work = fixture
        .service
        .prepare_external_usage_begin(&begin(&recovered, &"a".repeat(64)), &fixture.connection)
        .unwrap();
    let old_reply: serde_json::Value =
        serde_json::from_str(&fixture.service.complete_external_usage_begin(
            old_ready_work,
            old_ready_result,
            &fixture.connection,
        ))
        .unwrap();
    assert!(old_reply.get("error").is_some());
    assert_eq!(
        fixture.service.external_usage_begin_pending_for_tests(),
        1,
        "old completion consumed replacement reservation"
    );
    let native = current_ready_work.clone();
    let current_result = tokio::task::spawn_blocking(move || native.execute())
        .await
        .unwrap();
    let current_ready: serde_json::Value =
        serde_json::from_str(&fixture.service.complete_external_usage_begin(
            current_ready_work,
            current_result,
            &fixture.connection,
        ))
        .unwrap();
    assert_eq!(
        current_ready["result"]["admitted_at"],
        original_ready["result"]["admitted_at"]
    );
    let mut replay = frozen.clone();
    for field in ["source_witness", "origin", "request_id"] {
        assert_eq!(
            current_ready["result"][field], original_ready["result"][field],
            "cold begin changed original request provenance"
        );
    }
    let mut replay_params: serde_json::Value =
        serde_json::from_str(replay.params.as_deref().unwrap()).unwrap();
    let original_params = replay_params.clone();
    replay_params["launch_token"] = recovered["result"]["launch_token"].clone();
    replay_params["generation"] = recovered["result"]["generation"].clone();
    for field in [
        "request_id",
        "event_id",
        "provider",
        "model",
        "observed_at",
        "counters",
        "external_session_id",
    ] {
        assert_eq!(replay_params[field], original_params[field]);
    }
    replay.params = Some(replay_params.to_string());
    let duplicate = finish(&mut fixture, &replay).await;
    assert_eq!(duplicate["result"]["durable"], true);
    assert_eq!(duplicate["result"]["applied"], false);
    // Pane-view reset persistence across FULL control replacement is a separate
    // unfinished contract: current baselines live in the replaced component.
    // Here establish a reset in the recovered projection, then require neither
    // exact replay nor subsequent distinct reports to resurrect its old spend.
    assert!(fixture.service.reset_external_token_usage("%1"));
    begin_ready(&mut fixture, &begin(&recovered, &"a".repeat(64))).await;
    assert_eq!(
        finish(&mut fixture, &replay).await["result"]["applied"],
        false
    );
    assert_eq!(
        fixture
            .service
            .external_token_usage(None)
            .values()
            .map(|usage| usage.input_tokens)
            .sum::<u64>(),
        10
    );
    assert_eq!(
        fixture
            .service
            .external_token_usage(Some("%1"))
            .values()
            .map(|usage| usage.input_tokens)
            .sum::<u64>(),
        0,
        "replay revived spend after the recovered projection reset"
    );
    begin_ready(&mut fixture, &begin(&recovered, &"e".repeat(64))).await;
    let fresh = report(
        &recovered,
        &"e".repeat(64),
        &"f".repeat(64),
        "original-model",
        7,
    );
    assert_eq!(
        finish(&mut fixture, &fresh).await["result"]["applied"],
        true
    );
    begin_ready(&mut fixture, &begin(&recovered, &"e".repeat(64))).await;
    assert_eq!(
        finish(&mut fixture, &fresh).await["result"]["applied"],
        false
    );
    let database = rusqlite::Connection::open(store.path()).unwrap();
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
                "SELECT count(*),sum(input_tokens) FROM token_usage_events",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            )
            .unwrap(),
        (2, 17)
    );
    assert_eq!(
        fixture
            .service
            .external_token_usage(None)
            .values()
            .map(|usage| usage.input_tokens)
            .sum::<u64>(),
        17
    );
    assert_eq!(
        fixture
            .service
            .external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
}

/// The same genuinely live producer/root/boot/session can explicitly recover
/// its original accounting owner after all control authority is replaced. Only
/// public historical selectors cross replacement; a separately actor-qualified
/// worker must win the exact durable observer claim before exposing the handle.
#[tokio::test(flavor = "current_thread")]
async fn external_cold_source_full_control_replacement_preserves_original_owner() {
    let Some((mut fixture, store, publication, ready)) =
        super::source_activation_fixture::accepted_source().await
    else {
        return;
    };
    let _observer = crate::runtime::UnixOriginStream::new(
        &mut fixture.socket,
        Some(publication.origin.clone()),
    );
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
    let (key, activation, _) = publication
        .source_preparation
        .get()
        .unwrap()
        .observer_scope_for_tests();
    // Fence the old control owner's pending metadata before replacing it, as
    // real teardown does. This is NOT a native source end/tombstone.
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
    params["observer_instance"] = serde_json::json!("fresh-cold-instance");
    params["durable_source_resume"] =
        serde_json::json!({"receipt":ready["source_receipt"],"operation_id":"b".repeat(64)});
    recovery.params = Some(params.to_string());
    let work = fixture
        .service
        .prepare_external_enrollment(&recovery, &fixture.connection)
        .expect("typed native cold source admission missing");
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.execute_enrollment())
        .await
        .unwrap();
    assert!(observed.is_ok());
    let body =
        fixture
            .service
            .complete_external_enrollment(work.clone(), observed, &fixture.connection);
    let pending: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        pending["result"]["registered"], true,
        "actual cold actor grant missing"
    );
    let publication = fixture
        .service
        .begin_external_source_publication(work, body)
        .unwrap_or_else(|_| panic!("required cold claim was not retained"));
    assert_eq!(
        store
            .external_source_activation(&key)
            .unwrap()
            .unwrap()
            .observer
            .unwrap()
            .generation(),
        1,
        "actor mutated metadata"
    );
    let native = publication.clone();
    let claimed = tokio::task::spawn_blocking(move || native.execute_enrollment())
        .await
        .unwrap();
    assert!(claimed.is_ok());
    let recovered: serde_json::Value =
        serde_json::from_str(&fixture.service.complete_external_source_publication(
            publication,
            claimed,
            &fixture.connection,
        ))
        .unwrap();
    assert_eq!(recovered["result"]["registered"], true);
    assert_eq!(
        recovered["result"]["recovery"]["kind"],
        "durable-native-source"
    );
    assert_eq!(
        recovered["result"]["source_receipt"]["observer_generation"],
        "2"
    );
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
        old_owner
    );
    let current = store.external_source_activation(&key).unwrap().unwrap();
    assert_eq!(current.activation, activation);
    assert_eq!(current.revision, 2);
    assert!(current.renewal.is_none());
}
