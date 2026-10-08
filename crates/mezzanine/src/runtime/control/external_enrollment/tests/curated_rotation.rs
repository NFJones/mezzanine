//! Explicit same-creator curated observer handoff using actual native children.
//! Public predecessor/instance selectors never substitute for retained source,
//! current helper writer, exact root or bounded off-actor ancestry evidence.

use super::curated_admission::{curated_request, enroll};
use super::helper_presentation::{child, release, settle};
use super::*;

/// Builds only the explicit successor selector on the known curated contract.
fn successor(instance: &str, predecessor: u64) -> JsonRpcRequest {
    let mut request = curated_request();
    let mut params: serde_json::Value =
        serde_json::from_str(request.params.as_deref().unwrap()).unwrap();
    params["observer_instance"] = instance.into();
    params["predecessor_generation"] = predecessor.into();
    request.params = Some(params.to_string());
    request
}

/// Executes the real bounded observation and settlement path. No supplied PID,
/// source descriptor or test-only successful native result can authorize work.
async fn submit(
    fixture: &mut Fixture,
    connection: &ControlConnectionState,
    request: &JsonRpcRequest,
) -> serde_json::Value {
    let work = fixture
        .service
        .prepare_external_enrollment(request, connection)
        .unwrap();
    settle(fixture, work, connection).await
}

/// Public epoch selectors still require exact current native helper evidence.
fn heartbeat(receipt: &serde_json::Value, sequence: u64) -> JsonRpcRequest {
    crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"proof","method":"agent/external/curated-heartbeat","params":{
        "harness":"claude","external_session_id":"session-a","generation":receipt["result"]["generation"],
        "observer_witness":receipt["result"]["observer_witness"],"sequence":sequence}}).to_string()).unwrap()
}

/// A changed module must explicitly name its current predecessor, obtain fresh
/// public observer selectors and keep the same native creator/run/accounting
/// owner. Identical transition retry is inert; original callbacks lose authority.
#[tokio::test(flavor = "current_thread")]
async fn external_curated_observer_successor_preserves_native_run_and_fences_original() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let (_socket, connection) = child(&mut fixture).await;
    let first = enroll(&mut fixture, &connection).await;
    let proof = fixture
        .service
        .prepare_external_helper_presentation(&heartbeat(&first, 7), &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, proof, &connection).await["result"]["changed"],
        true
    );
    let stale = fixture
        .service
        .prepare_external_helper_presentation(&heartbeat(&first, 8), &connection)
        .unwrap();
    let native = stale.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(observed.is_ok());
    let binding = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap();
    let owner = binding.accounting_owner.clone();
    let origin = binding.accounting_origin.clone();
    let source = binding.enrollment.as_ref().unwrap().producer.clone();
    let request = successor("module-b", first["result"]["generation"].as_u64().unwrap());
    let second = submit(&mut fixture, &connection, &request).await;
    assert!(
        second.get("error").is_none(),
        "explicit native successor rejected: {second}"
    );
    assert_ne!(
        first["result"]["generation"],
        second["result"]["generation"]
    );
    assert_ne!(
        first["result"]["observer_witness"],
        second["result"]["observer_witness"]
    );
    assert_eq!(first["result"]["run_id"], second["result"]["run_id"]);
    assert_eq!(first["result"]["agent_id"], second["result"]["agent_id"]);
    assert_eq!(second["result"]["observer_epoch"], 2);
    assert!(second["result"].get("launch_token").is_none());
    assert_eq!(second["result"]["controls"], serde_json::json!([]));
    let registry = fixture.service.control.external_agents();
    assert_eq!(registry.bindings.len(), 1);
    let binding = registry.bindings.values().next().unwrap();
    assert_eq!(binding.accounting_owner, owner);
    assert_eq!(binding.accounting_origin, origin);
    let enrollment = binding.enrollment.as_ref().unwrap();
    assert!(enrollment.producer.same_owner(&source));
    assert!(enrollment.observers.is_empty());
    assert!(!enrollment.has_live_observer());
    assert_eq!(enrollment.curated_observer.as_ref().unwrap().sequence, 0);
    assert!(
        enrollment
            .curated_observer
            .as_ref()
            .unwrap()
            .observed_at
            .is_none()
    );
    assert!(
        enrollment
            .authorize_connection(&fixture.connection)
            .is_err()
    );
    let expiry = binding.expires;
    let retry = submit(&mut fixture, &connection, &request).await;
    assert_eq!(retry, second);
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
    let late: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(stale, observed, &connection),
    )
    .unwrap();
    assert!(late.get("error").is_some());
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&heartbeat(&first, 100), &connection)
            .is_err()
    );
    let proof = fixture
        .service
        .prepare_external_helper_presentation(&heartbeat(&second, 1), &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, proof, &connection).await["result"]["changed"],
        true
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
            .expires,
        expiry
    );
    assert!(
        enroll(&mut fixture, &connection)
            .await
            .get("error")
            .is_some()
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

/// Late deadline/helper death, direct-producer impersonation and public integer
/// exhaustion must leave the original namespace and observer identity unchanged.
/// Successful worker evidence is never sufficient after its native owner dies.
#[tokio::test(flavor = "current_thread")]
async fn external_curated_observer_late_native_evidence_and_overflow_have_no_effect() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let (mut socket, connection) = child(&mut fixture).await;
    let first = enroll(&mut fixture, &connection).await;
    let generation = first["result"]["generation"].as_u64().unwrap();
    let request = successor("module-b", generation);
    let direct = fixture
        .service
        .prepare_external_enrollment(&request, &fixture.connection)
        .unwrap();
    let native = direct.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(observed.is_err());
    fixture
        .service
        .complete_external_enrollment(direct, observed, &fixture.connection);
    let mut late = fixture
        .service
        .prepare_external_enrollment(&request, &connection)
        .unwrap();
    let native = late.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(observed.is_ok());
    late.deadline = Instant::now();
    let reply: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(late, observed, &connection),
    )
    .unwrap();
    assert!(reply.get("error").is_some());
    let dead = fixture
        .service
        .prepare_external_enrollment(&request, &connection)
        .unwrap();
    let native = dead.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(observed.is_ok());
    release(&mut socket, &connection).await;
    let reply: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(dead, observed, &connection),
    )
    .unwrap();
    assert!(reply.get("error").is_some());
    let (_socket, connection) = child(&mut fixture).await;
    fixture
        .service
        .control
        .external_agents_mut()
        .next_generation = 9_007_199_254_740_991;
    assert!(
        submit(&mut fixture, &connection, &request)
            .await
            .get("error")
            .is_some()
    );
    let mut initial_overflow = curated_request();
    let mut params: serde_json::Value =
        serde_json::from_str(initial_overflow.params.as_deref().unwrap()).unwrap();
    params["external_session_id"] = "session-b".into();
    initial_overflow.params = Some(params.to_string());
    assert!(
        submit(&mut fixture, &connection, &initial_overflow)
            .await
            .get("error")
            .is_some()
    );
    assert_eq!(enroll(&mut fixture, &connection).await, first);
    let registry = fixture.service.control.external_agents();
    assert_eq!(registry.next_generation, 9_007_199_254_740_991);
    assert_eq!(registry.bindings.len(), 1);
    assert_eq!(
        registry.bindings.values().next().unwrap().generation,
        generation
    );
    assert_eq!(
        registry
            .bindings
            .values()
            .next()
            .unwrap()
            .enrollment
            .as_ref()
            .unwrap()
            .instances
            .len(),
        1
    );
    assert!(!registry.bindings.values().next().unwrap().retired);
    assert!(registry.enrollments.pending.is_empty());
}

/// Rotation must update the original namespace selector without losing its
/// lifetime fence during either production tombstone GC or snapshot cleanup.
/// Even an implicit new instance cannot allocate after binding removal while
/// that original native creator lives; actual creator death releases the fence.
#[tokio::test(flavor = "current_thread")]
async fn external_curated_rotated_namespace_survives_gc_and_snapshot_cleanup() {
    for snapshot in [false, true] {
        let Some(mut fixture) = fixture("hold").await else {
            return;
        };
        let (_socket, connection) = child(&mut fixture).await;
        let first = enroll(&mut fixture, &connection).await;
        let old = first["result"]["generation"].as_u64().unwrap();
        let second = submit(&mut fixture, &connection, &successor("module-b", old)).await;
        assert!(second.get("error").is_none());
        let generation = fixture.service.control.external_agents().next_generation;
        if snapshot {
            fixture.service.retire_unbound_external_message_identities();
        } else {
            let binding = fixture
                .service
                .control
                .external_agents_mut()
                .bindings
                .values_mut()
                .next()
                .unwrap();
            binding.expires = current_unix_seconds() - 1;
            fixture.service.reconcile_external_agent_registrations();
            let binding = fixture
                .service
                .control
                .external_agents_mut()
                .bindings
                .values_mut()
                .next()
                .unwrap();
            assert!(binding.retired);
            binding.expires = current_unix_seconds() - 1;
            fixture.service.reconcile_external_agent_registrations();
        }
        assert!(
            fixture
                .service
                .control
                .external_agents()
                .bindings
                .is_empty()
        );
        assert!(fixture.connection.unix_origin().unwrap().is_live());
        assert!(
            submit(&mut fixture, &connection, &successor("module-b", old))
                .await
                .get("error")
                .is_some()
        );
        let mut implicit = curated_request();
        let mut params: serde_json::Value =
            serde_json::from_str(implicit.params.as_deref().unwrap()).unwrap();
        params["observer_instance"] = "module-c".into();
        implicit.params = Some(params.to_string());
        assert!(
            submit(&mut fixture, &connection, &implicit)
                .await
                .get("error")
                .is_some()
        );
        assert_eq!(
            fixture.service.control.external_agents().next_generation,
            generation
        );
        assert!(
            fixture
                .service
                .control
                .external_agents()
                .bindings
                .is_empty()
        );
        assert!(fixture.service.external_agent_cleanup_needed());
        release(&mut fixture.socket, &fixture.connection).await;
        fixture.service.reconcile_external_agent_registrations();
        assert!(
            fixture
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
                .enrollments
                .pending
                .is_empty()
        );
    }
}

/// Two physically qualified contenders on one predecessor cannot both publish.
/// Retired instances, missing/wrong retry witnesses, metadata changes and the
/// finite instance budget fail without rotating the winner or its namespace.
/// Expired namespace ownership stays fenced for the actual live creator.
#[tokio::test(flavor = "current_thread")]
async fn external_curated_observer_contenders_capacity_and_retirement_are_fenced() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let (_socket, connection) = child(&mut fixture).await;
    let first = enroll(&mut fixture, &connection).await;
    let old = first["result"]["generation"].as_u64().unwrap();
    let contender = fixture
        .service
        .prepare_external_enrollment(&successor("module-c", old), &connection)
        .unwrap();
    let second = submit(&mut fixture, &connection, &successor("module-b", old)).await;
    assert!(second.get("error").is_none());
    assert!(
        settle(&mut fixture, contender, &connection)
            .await
            .get("error")
            .is_some()
    );
    let current = second["result"]["generation"].as_u64().unwrap();
    for request in [
        successor("module-a", current),
        successor("module-c", old),
        successor("module-b", current),
    ] {
        assert!(
            submit(&mut fixture, &connection, &request)
                .await
                .get("error")
                .is_some()
        );
    }
    let mut no_predecessor = curated_request();
    let mut params: serde_json::Value =
        serde_json::from_str(no_predecessor.params.as_deref().unwrap()).unwrap();
    params["observer_instance"] = "module-c".into();
    no_predecessor.params = Some(params.to_string());
    assert!(
        submit(&mut fixture, &connection, &no_predecessor)
            .await
            .get("error")
            .is_some()
    );
    let mut wrong_metadata = successor("module-c", current);
    let mut params: serde_json::Value =
        serde_json::from_str(wrong_metadata.params.as_deref().unwrap()).unwrap();
    params["version"] = "changed".into();
    wrong_metadata.params = Some(params.to_string());
    assert!(
        submit(&mut fixture, &connection, &wrong_metadata)
            .await
            .get("error")
            .is_some()
    );
    for predecessor in [0, 9_007_199_254_740_992] {
        assert!(
            fixture
                .service
                .prepare_external_enrollment(&successor("module-c", predecessor), &connection)
                .is_err()
        );
    }
    {
        let binding = fixture
            .service
            .control
            .external_agents_mut()
            .bindings
            .values_mut()
            .next()
            .unwrap();
        let source = binding.enrollment.as_mut().unwrap();
        for n in source.instances.len()..MAX_OBSERVER_INSTANCES {
            source.instances.insert(format!("reserved-{n}"));
        }
    }
    assert!(
        submit(&mut fixture, &connection, &successor("module-c", current))
            .await
            .get("error")
            .is_some()
    );
    assert_eq!(
        submit(&mut fixture, &connection, &successor("module-b", old)).await,
        second
    );
    let binding = fixture
        .service
        .control
        .external_agents_mut()
        .bindings
        .values_mut()
        .next()
        .unwrap();
    binding.expires = current_unix_seconds() - 1;
    fixture.service.reconcile_external_agent_registrations();
    assert!(fixture.connection.unix_origin().unwrap().is_live());
    assert!(
        submit(&mut fixture, &connection, &successor("module-c", current))
            .await
            .get("error")
            .is_some()
    );
    let registry = fixture.service.control.external_agents();
    assert_eq!(registry.bindings.len(), 1);
    assert_eq!(
        registry.bindings.values().next().unwrap().generation,
        current
    );
    assert!(registry.bindings.values().next().unwrap().retired);
    assert!(!registry.enrollments.curated_namespaces.is_empty());
    assert!(registry.enrollments.pending.is_empty());
}
