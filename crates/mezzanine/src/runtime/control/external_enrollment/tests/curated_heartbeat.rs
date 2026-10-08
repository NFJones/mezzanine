//! Native original-epoch observer proof, separate from source lifetime and lease.
use super::curated_admission::enroll;
use super::helper_presentation::child;
use super::helper_presentation::release;
use super::*;

/// Strict public selectors must still pass native exact creator/writer/root proof.
fn heartbeat(first: &serde_json::Value, sequence: u64) -> JsonRpcRequest {
    crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"heartbeat","method":"agent/external/curated-heartbeat","params":{"harness":"claude","generation":first["result"]["generation"],"observer_witness":first["result"]["observer_witness"],"external_session_id":"session-a","sequence":sequence}}).to_string()).unwrap()
}

/// Shared actual native observation and actor settlement; no test bypass grants
/// freshness, and all public selectors still pass the original creator fences.
async fn prove(
    fixture: &mut Fixture,
    connection: &ControlConnectionState,
    first: &serde_json::Value,
    sequence: u64,
) -> serde_json::Value {
    let work = fixture
        .service
        .prepare_external_helper_presentation(&heartbeat(first, sequence), connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, connection),
    )
    .unwrap()
}

/// Current original sender/root/source evidence must accompany fresh epoch proof.
/// Wrong public generation, arbitrary fields, a direct producer impersonating
/// its own helper, expired work and actual helper/source death leave freshness
/// unchanged. A still-recent timestamp cannot conceal native creator death.
#[tokio::test(flavor = "current_thread")]
async fn external_curated_heartbeat_stale_and_native_death_leave_no_authority() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let (mut socket, connection) = child(&mut fixture).await;
    let first = enroll(&mut fixture, &connection).await;
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&heartbeat(&first, 0), &connection)
            .is_err()
    );
    let mut altered = first.clone();
    altered["result"]["generation"] = serde_json::json!(999);
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&heartbeat(&altered, 1), &connection)
            .is_err()
    );
    let direct = fixture
        .service
        .prepare_external_helper_presentation(&heartbeat(&first, 1), &fixture.connection)
        .unwrap();
    let native = direct.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(observed.is_err());
    fixture
        .service
        .complete_external_enrollment(direct, observed, &fixture.connection);
    let mut work = fixture
        .service
        .prepare_external_helper_presentation(&heartbeat(&first, 1), &connection)
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
    let work = fixture
        .service
        .prepare_external_helper_presentation(&heartbeat(&first, 1), &connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    release(&mut socket, &connection).await;
    let reply: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, &connection),
    )
    .unwrap();
    assert!(reply.get("error").is_some());
    let (_socket, connection) = child(&mut fixture).await;
    assert_eq!(
        prove(&mut fixture, &connection, &first, 1).await["result"]["changed"],
        true
    );
    release(&mut fixture.socket, &fixture.connection).await;
    fixture.service.renew_connected_external_observers();
    fixture.service.reconcile_external_agent_registrations();
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .all(|binding| binding.retired)
    );
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&heartbeat(&first, 2), &connection)
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
}

/// A real creator child can report original-epoch freshness without acquiring
/// a socket/client role. The heartbeat itself must not renew a lease, and its
/// public receipt is neither a process-lifetime nor durable accounting receipt.
#[tokio::test(flavor = "current_thread")]
async fn external_curated_heartbeat_native_proof_is_separate_from_lease() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let (_socket, connection) = child(&mut fixture).await;
    let first = enroll(&mut fixture, &connection).await;
    assert!(first.get("error").is_none());
    let expiry = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap()
        .expires;
    let work = fixture
        .service
        .prepare_external_helper_presentation(&heartbeat(&first, 1), &connection)
        .expect("native curated observer proof unavailable");
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    let reply: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, &connection),
    )
    .unwrap();
    assert_eq!(reply["result"]["observed"], true);
    assert_eq!(reply["result"]["changed"], true);
    let binding = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap();
    assert_eq!(binding.expires, expiry);
    assert!(binding.enrollment.as_ref().unwrap().has_live_observer());
    assert!(binding.enrollment.as_ref().unwrap().observers.is_empty());
    assert!(!connection.initialized());
    let observed_at = binding
        .enrollment
        .as_ref()
        .unwrap()
        .curated_observer
        .as_ref()
        .unwrap()
        .observed_at;
    let replay = prove(&mut fixture, &connection, &first, 1).await;
    assert_eq!(replay["result"]["changed"], false);
    assert_eq!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .next()
            .unwrap()
            .enrollment
            .as_ref()
            .unwrap()
            .curated_observer
            .as_ref()
            .unwrap()
            .observed_at,
        observed_at
    );
    assert_eq!(
        prove(&mut fixture, &connection, &first, 3).await["result"]["changed"],
        true
    );
    assert!(
        prove(&mut fixture, &connection, &first, 2)
            .await
            .get("error")
            .is_some()
    );
    let mut foreign = first.clone();
    foreign["result"]["observer_witness"] = "b".repeat(64).into();
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&heartbeat(&foreign, 4), &connection)
            .is_err()
    );
    fixture
        .service
        .control
        .external_agents_mut()
        .bindings
        .values_mut()
        .next()
        .unwrap()
        .expires = current_unix_seconds() - 1;
    fixture.service.renew_connected_external_observers();
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .next()
            .unwrap()
            .expires
            > current_unix_seconds()
    );
    let binding = fixture
        .service
        .control
        .external_agents_mut()
        .bindings
        .values_mut()
        .next()
        .unwrap();
    binding
        .enrollment
        .as_mut()
        .unwrap()
        .curated_observer
        .as_mut()
        .unwrap()
        .observed_at = Some(Instant::now() - Duration::from_secs(31));
    let aged = binding
        .enrollment
        .as_ref()
        .unwrap()
        .curated_observer
        .as_ref()
        .unwrap()
        .observed_at;
    assert!(!binding.enrollment.as_ref().unwrap().has_live_observer());
    assert_eq!(
        prove(&mut fixture, &connection, &first, 3).await["result"]["changed"],
        false
    );
    let binding = fixture
        .service
        .control
        .external_agents_mut()
        .bindings
        .values_mut()
        .next()
        .unwrap();
    assert_eq!(
        binding
            .enrollment
            .as_ref()
            .unwrap()
            .curated_observer
            .as_ref()
            .unwrap()
            .observed_at,
        aged
    );
    binding.expires = current_unix_seconds() - 1;
    fixture.service.renew_connected_external_observers();
    fixture.service.reconcile_external_agent_registrations();
    assert!(fixture.connection.unix_origin().unwrap().is_live());
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .all(|binding| binding.retired)
    );
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&heartbeat(&first, 4), &connection)
            .is_err()
    );
}
