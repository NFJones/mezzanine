//! Curated request metadata through genuine native helper/creator observation.
//! Public selectors are not authority, helper identity never owns billing, and
//! actual sealed origin publication is required before readiness. No provider,
//! vendor callback content, credentials, or user settings are involved.
use super::curated_admission::enroll;
use super::helper_presentation::child;
use super::*;

/// A normally invoked creator spawns the freshly built fixed binary, which
/// discovers standard MEZ routing and sends public capsules on real Unix ingress.
/// Admission, origin and expense all traverse production parsing and actor work;
/// replay preserves first origin and charges once. No vendor callback is claimed.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires freshly built sibling mez binary; real fixed-helper qualification"]
async fn external_curated_request_fixed_binary_unix_roundtrip_conserves() {
    use crate::host::async_runtime::{
        AsyncRuntimeActorConfig, AsyncRuntimeControlConnectionConfig, AsyncRuntimeSessionActor,
        serve_async_runtime_control_connection_loop,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let Some(mut fixture) = fixture("hold").await else {
        panic!("fixed-helper qualification requires native peer evidence");
    };
    let path = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
    let service = std::mem::replace(&mut fixture.service, RuntimeServiceFixture::new().build());
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let task = tokio::spawn(actor.run());
    let mut original = serde_json::Value::Null;
    let mut observed_at = None;
    for operation in ["enroll", "begin", "end", "begin", "end"] {
        let capsule = if operation == "enroll" {
            serde_json::json!({"external_session_id":"session-a","observer_instance":"module-a","session_boundary":"startup"})
        } else {
            let mut capsule = serde_json::json!({
                "operation":format!("curated-usage-{operation}"),
                "external_session_id":"session-a","generation":original["generation"],
                "observer_witness":original["observer_witness"],"request_id":"a".repeat(64)
            });
            if operation == "end" {
                capsule["event_id"] = serde_json::json!("b".repeat(64));
                capsule["observed_at"] = serde_json::json!(observed_at.unwrap());
                capsule["provider"] = serde_json::json!("fixture");
                capsule["model"] = serde_json::json!("model");
                capsule["counters"] = serde_json::json!({"input_tokens":10,"output_tokens":3});
            }
            capsule
        };
        let bytes = capsule.to_string();
        fixture.socket.write_all(&[6]).await.unwrap();
        fixture
            .socket
            .write_all(&u32::try_from(bytes.len()).unwrap().to_be_bytes())
            .await
            .unwrap();
        fixture.socket.write_all(bytes.as_bytes()).await.unwrap();
        let (mut socket, _) =
            tokio::time::timeout(Duration::from_secs(10), fixture.listener.accept())
                .await
                .unwrap()
                .unwrap();
        let mut connection = ControlConnectionState::new(true, false);
        serve_async_runtime_control_connection_loop(
            &mut socket,
            &handle,
            &mut connection,
            AsyncRuntimeControlConnectionConfig::new(8192, crate::runtime::current_effective_uid())
                .unwrap(),
            |_, _| false,
        )
        .await
        .unwrap();
        assert!(!connection.initialized());
        assert!(connection.caller_client_id().is_none());
        let mut length = [0; 4];
        tokio::time::timeout(
            Duration::from_secs(10),
            fixture.socket.read_exact(&mut length),
        )
        .await
        .unwrap()
        .unwrap();
        let length = u32::from_be_bytes(length) as usize;
        assert!(length <= 4096);
        let mut bytes = vec![0; length];
        fixture.socket.read_exact(&mut bytes).await.unwrap();
        let reply: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(reply.get("launch_token").is_none());
        match operation {
            "enroll" => {
                assert_eq!(reply["registered"], true, "{reply}");
                original = reply;
            }
            "begin" => {
                assert_eq!(reply["accepted"], true, "{reply}");
                let admitted_at = reply["admitted_at"].as_u64().unwrap();
                assert_eq!(*observed_at.get_or_insert(admitted_at), admitted_at);
            }
            "end" => assert_eq!(reply["durable"], true, "{reply}"),
            _ => unreachable!(),
        }
    }
    handle.shutdown().await.unwrap();
    fixture.service = task.await.unwrap().service;
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
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        10
    );
    assert_eq!(
        database
            .query_row(
                "SELECT count(*) FROM external_usage_observations",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
}

/// Canonical framed actor dispatch receives public helper selectors on a real
/// native child connection, performs off-actor parent/origin/ledger work, and
/// acknowledges durable replay without initialized client roles or double cost.
#[tokio::test(flavor = "current_thread")]
async fn external_curated_request_framed_actor_begin_end_retains_shared_ledger() {
    use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let path = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
    let (_socket, connection) = child(&mut fixture).await;
    let first = enroll(&mut fixture, &connection).await;
    let service = std::mem::replace(&mut fixture.service, RuntimeServiceFixture::new().build());
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let task = tokio::spawn(actor.run());
    let mut observed_at = None;
    for iteration in 0..2 {
        for is_end in [false, true] {
            let request = if is_end {
                end(&first, observed_at.unwrap())
            } else {
                begin(&first)
            };
            let body=serde_json::json!({"jsonrpc":"2.0","id":"wire-curated","method":request.method,"params":serde_json::from_str::<serde_json::Value>(request.params.as_deref().unwrap()).unwrap()}).to_string();
            let reply = handle
                .handle_control_input_for_connection(
                    crate::control::encode_control_body(&body),
                    8192,
                    connection.clone(),
                )
                .await
                .unwrap();
            let (body, _) = crate::control::decode_control_frame(&reply.output, 8192).unwrap();
            let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(parsed["result"]["accepted"], true, "{parsed}");
            if is_end {
                assert_eq!(parsed["result"]["durable"], true);
                assert_eq!(parsed["result"]["applied"], iteration == 0);
            } else {
                let admitted_at = parsed["result"]["admitted_at"].as_u64().unwrap();
                assert_eq!(*observed_at.get_or_insert(admitted_at), admitted_at);
            }
            assert!(!reply.connection.initialized());
            assert!(reply.connection.caller_client_id().is_none());
        }
    }
    handle.shutdown().await.unwrap();
    let mut exit = task.await.unwrap();
    assert_eq!(
        exit.service.external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
    assert!(
        exit.service
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
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        10
    );
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Rejected ledger capacity releases native admission; unpublished successful
/// results cannot acknowledge expense. A real committed worker still settles
/// expense once after actor deadline expiry without granting stale-helper ACK.
#[tokio::test(flavor = "current_thread")]
async fn external_curated_request_end_capacity_unpublished_and_late_commit_fence() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let path = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
    let (_socket, connection) = child(&mut fixture).await;
    let first = enroll(&mut fixture, &connection).await;
    let work = fixture
        .service
        .prepare_external_helper_presentation(&begin(&first), &connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    fixture
        .service
        .complete_external_enrollment(work, observed, &connection);
    let request = end(&first, crate::runtime::current_unix_seconds());
    fixture.service.set_external_usage_stream_limit_for_tests(0);
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&request, &connection)
            .is_err()
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
    assert_eq!(
        fixture
            .service
            .external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
    fixture
        .service
        .set_external_usage_stream_limit_for_tests(4096);
    let work = fixture
        .service
        .prepare_external_helper_presentation(&request, &connection)
        .unwrap();
    let reply: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, Ok(()), &connection),
    )
    .unwrap();
    assert!(reply.get("error").is_some());
    assert_eq!(
        fixture
            .service
            .external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
    let mut work = fixture
        .service
        .prepare_external_helper_presentation(&request, &connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(observed.is_ok());
    work.deadline = Instant::now();
    let reply: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, &connection),
    )
    .unwrap();
    assert!(reply.get("error").is_some());
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
        10
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
}

/// Normalized finalized counters remain tied to the existing original marker;
/// no source owner, PID, project, epoch or private credential is supplied.
pub(super) fn end(first: &serde_json::Value, observed_at: u64) -> JsonRpcRequest {
    crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"curated-end","method":"agent/external/curated-usage-end","params":{
        "harness":"claude","generation":first["result"]["generation"],"observer_witness":first["result"]["observer_witness"],"external_session_id":"session-a","request_id":"a".repeat(64),
        "event_id":"b".repeat(64),"observed_at":observed_at,"provider":"fixture","model":"model","counters":{"input_tokens":10,"output_tokens":3}}}).to_string()).unwrap()
}

/// A genuine child helper obtains request origin, rejects a same-user wrong
/// parent without expense or lost readiness, then commits once and safely
/// retries the same report despite a different captured clock second. Original
/// readiness and the entire first end request stay frozen, while independently
/// changed timestamps or counters still conflict without another charge. Both
/// native and ledger reservations are consumed, including rejected reports.
#[tokio::test(flavor = "current_thread")]
async fn external_curated_request_end_native_commit_replay_and_wrong_parent_conserve() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let path = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
    let (_socket, connection) = child(&mut fixture).await;
    let first = enroll(&mut fixture, &connection).await;
    let original_capture = crate::runtime::current_unix_seconds().saturating_sub(500);
    let mut original_request = None;
    for iteration in 0..2 {
        let capture_at = original_capture + iteration * 100;
        let mut work = fixture
            .service
            .prepare_external_helper_presentation(&begin(&first), &connection)
            .unwrap();
        assert!(work.fixture_usage_capture_at(capture_at));
        let native = work.clone();
        let observed = tokio::task::spawn_blocking(move || native.observe())
            .await
            .unwrap();
        let ready: serde_json::Value = serde_json::from_str(
            &fixture
                .service
                .complete_external_enrollment(work, observed, &connection),
        )
        .unwrap();
        assert_eq!(ready["result"]["accepted"], true);
        assert_eq!(ready["result"]["admitted_at"], original_capture);
        // Capture the complete report once, after actual native readiness and
        // before even the wrong-parent attempt. A retry is not a new report.
        let request = original_request
            .get_or_insert_with(|| end(&first, ready["result"]["admitted_at"].as_u64().unwrap()))
            .clone();
        if iteration == 0 {
            let work = fixture
                .service
                .prepare_external_helper_presentation(&request, &fixture.connection)
                .unwrap();
            let native = work.clone();
            let observed = tokio::task::spawn_blocking(move || native.observe())
                .await
                .unwrap();
            assert!(observed.is_err());
            let reply: serde_json::Value = serde_json::from_str(
                &fixture
                    .service
                    .complete_external_enrollment(work, observed, &fixture.connection),
            )
            .unwrap();
            assert!(reply.get("error").is_some());
            assert_eq!(
                fixture
                    .service
                    .external_usage_reservation_counts_for_tests(),
                (0, 0)
            );
            assert_eq!(
                fixture
                    .service
                    .control
                    .external_usage_begins()
                    .origins
                    .len(),
                1
            );
            let database = rusqlite::Connection::open(&path).unwrap();
            assert_eq!(
                database
                    .query_row("SELECT count(*) FROM token_usage_events", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .unwrap(),
                0
            );
        } else {
            // Both clocks are safely in the past and at/after the original
            // readiness. A rebuilt report would conflict, not be a replay.
            assert!(capture_at > original_capture);
            assert_ne!(request.params, end(&first, capture_at).params);
            for field in ["observed_at", "counters"] {
                let mut changed = request.clone();
                let mut params: serde_json::Value =
                    serde_json::from_str(changed.params.as_deref().unwrap()).unwrap();
                if field == "observed_at" {
                    params[field] = capture_at.into();
                } else {
                    params[field]["input_tokens"] = 11.into();
                }
                changed.params = Some(params.to_string());
                let work = fixture
                    .service
                    .prepare_external_helper_presentation(&changed, &connection)
                    .unwrap();
                let native = work.clone();
                let observed = tokio::task::spawn_blocking(move || native.observe())
                    .await
                    .unwrap();
                let error = observed.as_ref().unwrap_err();
                assert_eq!(error.kind(), crate::error::MezErrorKind::Conflict);
                assert_eq!(
                    error.to_string(),
                    "Conflict: external observation id has conflicting immutable payload"
                );
                let reply: serde_json::Value = serde_json::from_str(
                    &fixture
                        .service
                        .complete_external_enrollment(work, observed, &connection),
                )
                .unwrap();
                assert!(reply.get("error").is_some(), "{reply}");
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
            }
        }
        let work = fixture
            .service
            .prepare_external_helper_presentation(&request, &connection)
            .unwrap();
        let native = work.clone();
        let observed = tokio::task::spawn_blocking(move || native.observe())
            .await
            .unwrap();
        assert!(observed.is_ok(), "{observed:?}");
        let reply: serde_json::Value = serde_json::from_str(
            &fixture
                .service
                .complete_external_enrollment(work, observed, &connection),
        )
        .unwrap();
        assert_eq!(reply["result"]["durable"], true, "{reply}");
        assert_eq!(reply["result"]["applied"], iteration == 0);
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
    }
    let database = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        database
            .query_row(
                "SELECT sum(input_tokens) FROM token_usage_events",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        10
    );
    assert_eq!(
        database
            .query_row(
                "SELECT count(*) FROM external_usage_observations",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert!(!connection.initialized());
    assert!(connection.caller_client_id().is_none());
}

/// Ephemeral registry reconstruction can reuse numeric generations but never
/// old observer proof. The same still-live native root/creator must recover its
/// durable accounting owner and original request origin, reject old selectors,
/// replay an already committed report without recharge, and accept a distinct
/// incurred report once. This is a component reset, not a real daemon/snapshot
/// restart or cross-process continuation fixture; native observation is real.
#[tokio::test(flavor = "current_thread")]
async fn external_curated_registry_reset_reuses_durable_owner_not_observer_witness() {
    use super::helper_presentation::settle;
    let Some(mut fixture) = fixture("hold").await else {
        panic!("registry reset qualification requires native peer evidence");
    };
    let path = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
    let (_socket, connection) = child(&mut fixture).await;
    let first = enroll(&mut fixture, &connection).await;
    assert_eq!(first["result"]["registered"], true);
    let (digest, owner, origin) = fixture
        .service
        .control
        .external_agents()
        .bindings
        .iter()
        .map(|(digest, binding)| {
            (
                *digest,
                binding.accounting_owner.clone(),
                binding.accounting_origin.clone(),
            )
        })
        .next()
        .unwrap();
    let mut work = fixture
        .service
        .prepare_external_helper_presentation(&begin(&first), &connection)
        .unwrap();
    let original_capture = crate::runtime::current_unix_seconds().saturating_sub(500);
    assert!(work.fixture_usage_capture_at(original_capture));
    let ready = settle(&mut fixture, work, &connection).await;
    assert_eq!(ready["result"]["accepted"], true, "{ready}");
    let at = ready["result"]["admitted_at"].as_u64().unwrap();
    assert_eq!(at, original_capture);
    let original = end(&first, at);
    let work = fixture
        .service
        .prepare_external_helper_presentation(&original, &connection)
        .unwrap();
    let committed = settle(&mut fixture, work, &connection).await;
    assert_eq!(committed["result"]["durable"], true, "{committed}");
    assert_eq!(committed["result"]["applied"], true);
    fixture.service.retire_external_agent_binding(digest);
    *fixture.service.control.external_agents_mut() = Default::default();
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
    let fresh = enroll(&mut fixture, &connection).await;
    assert_eq!(fresh["result"]["registered"], true, "{fresh}");
    assert_ne!(fresh["result"]["generation"], first["result"]["generation"]);
    assert_ne!(
        fresh["result"]["observer_witness"],
        first["result"]["observer_witness"]
    );
    let binding = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap();
    assert_eq!(binding.accounting_owner, owner);
    assert_eq!(binding.accounting_origin, origin);
    assert!(binding.enrollment.as_ref().unwrap().usage_owner_is_durable);
    let heartbeat = |receipt: &serde_json::Value| {
        crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"reset-proof","method":"agent/external/curated-heartbeat","params":{
            "harness":"claude","generation":receipt["result"]["generation"],"observer_witness":receipt["result"]["observer_witness"],
            "external_session_id":"session-a","sequence":1}}).to_string()).unwrap()
    };
    for mut stale in [begin(&first), original.clone(), heartbeat(&first)] {
        // The allocator no longer reuses a generation inside this service, but
        // supplying the current number must not make the original witness valid.
        let mut params: serde_json::Value =
            serde_json::from_str(stale.params.as_deref().unwrap()).unwrap();
        params["generation"] = fresh["result"]["generation"].clone();
        stale.params = Some(params.to_string());
        let refused = match fixture
            .service
            .prepare_external_helper_presentation(&stale, &connection)
        {
            Ok(work) => settle(&mut fixture, work, &connection)
                .await
                .get("error")
                .is_some(),
            Err(_) => true,
        };
        assert!(
            refused,
            "old witness was accepted with the current numeric generation"
        );
    }
    let source = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap()
        .enrollment
        .as_ref()
        .unwrap();
    assert_eq!(source.curated_observer.as_ref().unwrap().sequence, 0);
    assert!(
        source
            .curated_observer
            .as_ref()
            .unwrap()
            .observed_at
            .is_none()
    );
    let work = fixture
        .service
        .prepare_external_helper_presentation(&heartbeat(&fresh), &connection)
        .unwrap();
    let proof = settle(&mut fixture, work, &connection).await;
    assert_eq!(proof["result"]["observed"], true, "{proof}");
    assert_eq!(proof["result"]["changed"], true);
    let mut work = fixture
        .service
        .prepare_external_helper_presentation(&begin(&fresh), &connection)
        .unwrap();
    assert!(work.fixture_usage_capture_at(at + 100));
    let replay_ready = settle(&mut fixture, work, &connection).await;
    assert_eq!(replay_ready["result"]["accepted"], true, "{replay_ready}");
    assert_eq!(
        replay_ready["result"]["admitted_at"],
        ready["result"]["admitted_at"]
    );
    let work = fixture
        .service
        .prepare_external_helper_presentation(&end(&fresh, at), &connection)
        .unwrap();
    let replay = settle(&mut fixture, work, &connection).await;
    assert_eq!(replay["result"]["durable"], true, "{replay}");
    assert_eq!(replay["result"]["applied"], false);
    let mut new_begin = begin(&fresh);
    let mut params: serde_json::Value =
        serde_json::from_str(new_begin.params.as_deref().unwrap()).unwrap();
    params["request_id"] = "c".repeat(64).into();
    new_begin.params = Some(params.to_string());
    let mut work = fixture
        .service
        .prepare_external_helper_presentation(&new_begin, &connection)
        .unwrap();
    assert!(work.fixture_usage_capture_at(at + 200));
    let new_ready = settle(&mut fixture, work, &connection).await;
    assert_eq!(new_ready["result"]["accepted"], true, "{new_ready}");
    assert_eq!(new_ready["result"]["admitted_at"], at + 200);
    let mut new_end = end(&fresh, new_ready["result"]["admitted_at"].as_u64().unwrap());
    let mut params: serde_json::Value =
        serde_json::from_str(new_end.params.as_deref().unwrap()).unwrap();
    params["request_id"] = "c".repeat(64).into();
    params["event_id"] = "d".repeat(64).into();
    new_end.params = Some(params.to_string());
    let work = fixture
        .service
        .prepare_external_helper_presentation(&new_end, &connection)
        .unwrap();
    let new_commit = settle(&mut fixture, work, &connection).await;
    assert_eq!(new_commit["result"]["durable"], true, "{new_commit}");
    assert_eq!(new_commit["result"]["applied"], true);
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
    assert!(!connection.initialized());
    assert!(connection.caller_client_id().is_none());
    let database = rusqlite::Connection::open(&path).unwrap();
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
        1
    );
    assert_eq!(
        database
            .query_row(
                "SELECT count(DISTINCT owner) FROM external_usage_observations",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
}

/// The actual snapshot-message restoration owner must retire runtime-only
/// curated telemetry even while its native root/creator remain alive. Restored
/// metadata, old public selectors and pre-observed queued proof cannot revive
/// that living namespace, while already committed expense survives unchanged.
/// This exercises message restoration, not a complete daemon/pane-root restart.
#[tokio::test(flavor = "current_thread")]
async fn external_curated_snapshot_message_restore_retires_proof_not_expense() {
    use super::helper_presentation::settle;
    let Some(mut fixture) = fixture("hold").await else {
        panic!("snapshot curated qualification requires native peer evidence");
    };
    let path = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
    let (_socket, connection) = child(&mut fixture).await;
    let first = enroll(&mut fixture, &connection).await;
    assert_eq!(first["result"]["registered"], true);
    let work = fixture
        .service
        .prepare_external_helper_presentation(&begin(&first), &connection)
        .unwrap();
    let ready = settle(&mut fixture, work, &connection).await;
    assert_eq!(ready["result"]["accepted"], true, "{ready}");
    let report = end(&first, ready["result"]["admitted_at"].as_u64().unwrap());
    let work = fixture
        .service
        .prepare_external_helper_presentation(&report, &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["durable"],
        true
    );
    let presentation = crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"snapshot-status","method":"agent/external/helper-observe","params":{
        "harness":"claude","generation":first["result"]["generation"],"observer_witness":first["result"]["observer_witness"],
        "external_session_id":"session-a","sequence":1,"state":"running","title":"snapshot-native-status"}}).to_string()).unwrap();
    let work = fixture
        .service
        .prepare_external_helper_presentation(&presentation, &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["changed"],
        true
    );
    let before = fixture
        .service
        .runtime_agent_status_display_with_options("%1", false)
        .unwrap();
    assert!(before.contains("running"), "{before}");
    let heartbeat = crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"snapshot-proof","method":"agent/external/curated-heartbeat","params":{
        "harness":"claude","generation":first["result"]["generation"],"observer_witness":first["result"]["observer_witness"],
        "external_session_id":"session-a","sequence":1}}).to_string()).unwrap();
    let queued = fixture
        .service
        .prepare_external_helper_presentation(&heartbeat, &connection)
        .unwrap();
    let native = queued.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(observed.is_ok());
    assert_eq!(fixture.service.external_agent_rows().len(), 1);
    let mut payload =
        crate::storage::snapshot::SessionSnapshotPayload::from_session(fixture.service.session());
    payload.message_state = Some(fixture.service.message_service().snapshot_state());
    fixture
        .service
        .restore_message_state_for_restored_snapshot(&payload)
        .unwrap();
    assert!(fixture.service.external_agent_rows().is_empty());
    let status = fixture
        .service
        .runtime_agent_status_display_with_options("%1", false)
        .unwrap();
    assert!(
        status.contains("No current external telemetry is observed"),
        "{status}"
    );
    assert!(!status.contains("snapshot-native-status"), "{status}");
    assert!(
        fixture
            .service
            .message_service()
            .discover_agents_filtered_session_wide(
                None,
                None,
                None,
                Some("external-harness"),
                None,
                &[]
            )
            .is_empty()
    );
    let refused: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(queued, observed, &connection),
    )
    .unwrap();
    assert!(
        refused.get("error").is_some(),
        "queued pre-restore proof revived a binding: {refused}"
    );
    for old in [heartbeat, begin(&first), report] {
        assert!(
            fixture
                .service
                .prepare_external_helper_presentation(&old, &connection)
                .is_err()
        );
    }
    let repeated = enroll(&mut fixture, &connection).await;
    assert!(
        repeated.get("error").is_some(),
        "restored metadata revived living namespace: {repeated}"
    );
    assert!(fixture.connection.unix_origin().unwrap().is_live());
    assert!(
        !fixture
            .service
            .control
            .external_agents()
            .enrollments
            .curated_namespaces
            .is_empty()
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
    assert_eq!(
        fixture
            .service
            .external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
    assert!(!connection.initialized());
    assert!(connection.caller_client_id().is_none());
    let database = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        database
            .query_row(
                "SELECT sum(input_tokens) FROM token_usage_events",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        10
    );
    assert_eq!(
        database
            .query_row(
                "SELECT count(*) FROM external_usage_observations",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM external_usage_sources", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

/// Builds only a public current observer selector plus one opaque request ID.
pub(super) fn begin(first: &serde_json::Value) -> JsonRpcRequest {
    crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"curated-request","method":"agent/external/curated-usage-begin","params":{
        "harness":"claude","generation":first["result"]["generation"],"observer_witness":first["result"]["observer_witness"],"external_session_id":"session-a","request_id":"a".repeat(64)}}).to_string()).unwrap()
}

/// A real same-user direct helper reaches its previously enrolled native creator
/// namespace, not a helper namespace. Exact retry preserves first origin/time;
/// no presentation/freshness/expense/role is acquired by this metadata operation.
#[tokio::test(flavor = "current_thread")]
async fn external_curated_request_begin_native_owner_and_retry_preserve_metadata() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let path = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
    let (_socket, connection) = child(&mut fixture).await;
    let first = enroll(&mut fixture, &connection).await;
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
    let expiry = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap()
        .expires;
    let mut replies = Vec::new();
    for _ in 0..2 {
        let work = fixture
            .service
            .prepare_external_helper_presentation(&begin(&first), &connection)
            .unwrap();
        let native = work.clone();
        let observed = tokio::task::spawn_blocking(move || native.observe())
            .await
            .unwrap();
        assert!(observed.is_ok(), "{observed:?}");
        let reply: serde_json::Value = serde_json::from_str(
            &fixture
                .service
                .complete_external_enrollment(work, observed, &connection),
        )
        .unwrap();
        assert_eq!(reply["result"]["accepted"], true, "{reply}");
        replies.push(reply);
    }
    assert_eq!(replies[0]["result"], replies[1]["result"]);
    assert!(replies[0]["result"].get("launch_token").is_none());
    assert_eq!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .next()
            .unwrap()
            .expires,
        expiry
    );
    assert!(!connection.initialized());
    assert!(connection.caller_client_id().is_none());
    let database = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        database
            .query_row("SELECT owner FROM external_usage_requests", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
        owner
    );
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM external_usage_requests", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        1
    );
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM token_usage_events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

/// Public metadata cannot skip writer/parent/root proof. A direct producer is
/// not its own helper, and a synthetic successful result cannot publish origin.
#[tokio::test(flavor = "current_thread")]
async fn external_curated_request_begin_wrong_parent_and_unpublished_result_reject() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let path = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
    let (_socket, connection) = child(&mut fixture).await;
    let first = enroll(&mut fixture, &connection).await;
    let work = fixture
        .service
        .prepare_external_helper_presentation(&begin(&first), &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(observed.is_err());
    fixture
        .service
        .complete_external_enrollment(work, observed, &fixture.connection);
    let work = fixture
        .service
        .prepare_external_helper_presentation(&begin(&first), &connection)
        .unwrap();
    let reply: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, Ok(()), &connection),
    )
    .unwrap();
    assert!(reply.get("error").is_some());
    let mut forged = first.clone();
    forged["result"]["generation"] = serde_json::json!(999);
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&begin(&forged), &connection)
            .is_err()
    );
    forged = first.clone();
    forged["result"]["observer_witness"] = serde_json::json!("f".repeat(64));
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&begin(&forged), &connection)
            .is_err()
    );
    let database = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM external_usage_requests", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
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
