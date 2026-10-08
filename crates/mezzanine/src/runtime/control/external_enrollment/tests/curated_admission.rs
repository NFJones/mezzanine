//! Declared curated creator admission with genuine ordinary processes only.
//!
//! Profile/event/session labels declare the adapter contract, not executable
//! attestation. Socket-origin/current-writer and retained native creator/root
//! evidence—not metadata PIDs—authorize only public observational registration.

use super::helper_presentation::{child, release};
use super::*;
use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};

/// Fixed explicit no-shell creator contract; supplied process IDs/roles/caps
/// cannot enter this strict metadata namespace.
fn curated_request() -> JsonRpcRequest {
    crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"curated","method":"agent/external/curated-enroll","params":{
        "pane_id":"%1","harness":"claude","version":"fixture","external_session_id":"session-a","display_name":"curated fixture",
        "observer_kind":"curated-command","observer_instance":"module-a","source_contract":"claude-curated-command/1","session_boundary":"startup"}}).to_string()).unwrap()
}

/// Runs actual native observation off actor and consumes one reservation through
/// the same common settlement owner used by ordinary persistent enrollment.
async fn enroll(fixture: &mut Fixture, connection: &ControlConnectionState) -> serde_json::Value {
    let work = fixture
        .service
        .prepare_external_enrollment(&curated_request(), connection)
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

/// An ordinary declared creator is retained separately from its short-lived
/// helper. Public replies contain no bearer token, observer sockets or usage
/// authority; exact retry preserves agent/run/accounting/source identity without
/// renewing its lease. Helper exit cannot be misread as creator death.
#[tokio::test(flavor = "current_thread")]
async fn external_curated_admission_native_creator_public_reply_and_retry() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let (mut socket, connection) = child(&mut fixture).await;
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .is_empty()
    );
    let first = enroll(&mut fixture, &connection).await;
    assert!(
        first.get("error").is_none(),
        "curated native admission rejected: {first}"
    );
    assert_eq!(first["result"]["registered"], true);
    assert!(first["result"].get("launch_token").is_none());
    assert_eq!(first["result"]["controls"], serde_json::json!([]));
    assert_eq!(
        first["result"]["observer_transport"],
        "unavailable-curated-freshness"
    );
    let registry = fixture.service.control.external_agents();
    let binding = registry.bindings.values().next().unwrap();
    let source = binding.enrollment.as_ref().unwrap();
    assert!(source.producer.matches_parent(
        fixture.connection.unix_origin().unwrap().uid(),
        fixture.connection.unix_origin().unwrap().identity
    ));
    assert!(source.observers.is_empty());
    assert!(!source.has_live_observer());
    assert!(source.authorize_connection(&fixture.connection).is_err());
    let owner = binding.accounting_owner.clone();
    let expires = binding.expires;
    release(&mut socket, &connection).await;
    assert!(
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
            .provenance_is_live()
    );
    let (_socket, connection) = child(&mut fixture).await;
    let retry = enroll(&mut fixture, &connection).await;
    assert_eq!(first, retry);
    let registry = fixture.service.control.external_agents();
    assert_eq!(registry.bindings.len(), 1);
    let binding = registry.bindings.values().next().unwrap();
    assert_eq!(binding.accounting_owner, owner);
    assert_eq!(binding.expires, expires);
    assert!(registry.enrollments.pending.is_empty());
    assert!(!connection.initialized());
    let callback = crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"state","method":"agent/external/helper-observe","params":{"harness":"claude","generation":first["result"]["generation"],"observer_witness":first["result"]["observer_witness"],"external_session_id":"session-a","sequence":1,"state":"running"}}).to_string()).unwrap();
    let work = fixture
        .service
        .prepare_external_helper_presentation(&callback, &connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    let result: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, &connection),
    )
    .unwrap();
    assert_eq!(result["result"]["changed"], true);
    let binding = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap();
    assert_eq!(binding.expires, expires);
    assert!(binding.enrollment.as_ref().unwrap().observers.is_empty());
    assert_eq!(binding.accounting_owner, owner);
    fixture
        .service
        .control
        .external_agents_mut()
        .bindings
        .values_mut()
        .next()
        .unwrap()
        .expires = current_unix_seconds() - 1;
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
}

/// Actual source-as-root and unrelated creator topologies are rejected without
/// using executable names or payload PIDs. Late helper death/deadline evidence
/// cannot allocate after successful observation; admission pressure stays finite.
#[tokio::test(flavor = "current_thread")]
async fn external_curated_admission_native_root_unrelated_late_and_capacity_fences() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let work = fixture
        .service
        .prepare_external_enrollment(&curated_request(), &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(
        observed.is_err(),
        "the pane-root creator is not a separate producer"
    );
    fixture
        .service
        .complete_external_enrollment(work, observed, &fixture.connection);
    let Some(mut other) = super::fixture("hold").await else {
        return;
    };
    let (_socket, other_connection) = child(&mut other).await;
    let work = fixture
        .service
        .prepare_external_enrollment(&curated_request(), &other_connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(observed.is_err());
    fixture
        .service
        .complete_external_enrollment(work, observed, &other_connection);
    let (mut socket, connection) = child(&mut fixture).await;
    let mut work = fixture
        .service
        .prepare_external_enrollment(&curated_request(), &connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(observed.is_ok());
    work.deadline = Instant::now();
    let result: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, &connection),
    )
    .unwrap();
    assert!(result.get("error").is_some());
    let work = fixture
        .service
        .prepare_external_enrollment(&curated_request(), &connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    release(&mut socket, &connection).await;
    let result: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, &connection),
    )
    .unwrap();
    assert!(result.get("error").is_some());
    let (_socket, connection) = child(&mut fixture).await;
    let budget = fixture
        .service
        .control
        .external_agents()
        .enrollments
        .ancestry_budget
        .clone();
    let held = budget.reserve_for_tests(512).unwrap();
    let work = fixture
        .service
        .prepare_external_enrollment(&curated_request(), &connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert_eq!(
        observed.as_ref().unwrap_err().kind(),
        crate::error::MezErrorKind::RateLimited
    );
    fixture
        .service
        .complete_external_enrollment(work, observed, &connection);
    assert_eq!(budget.reserved(), 512);
    drop(held);
    let mut pending = Vec::new();
    for _ in 0..32 {
        pending.push(
            fixture
                .service
                .prepare_external_enrollment(&curated_request(), &connection)
                .unwrap(),
        );
    }
    assert!(
        fixture
            .service
            .prepare_external_enrollment(&curated_request(), &connection)
            .is_err()
    );
    for work in pending {
        fixture.service.complete_external_enrollment(
            work,
            Err(MezError::conflict("discarded fixture work")),
            &connection,
        );
    }
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
    assert_eq!(budget.reserved(), 0);
}

/// The async framed control route uses the production bounded worker/settlement
/// grammar and public response. Native evidence comes from a genuine child; a
/// frame can obtain no primary/observer client role or private socket capability.
#[tokio::test(flavor = "current_thread")]
async fn external_curated_admission_async_framed_ingress_is_public_only() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let (_socket, connection) = child(&mut fixture).await;
    let service = std::mem::replace(&mut fixture.service, RuntimeServiceFixture::new().build());
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let task = tokio::spawn(actor.run());
    let request = curated_request();
    let body = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":\"wire\",\"method\":\"agent/external/curated-enroll\",\"params\":{}}}",
        request.params.unwrap()
    );
    let reply = handle
        .handle_control_input_for_connection(
            crate::control::encode_control_body(&body),
            8192,
            connection.clone(),
        )
        .await
        .unwrap();
    let (body, _) = crate::control::decode_control_frame(&reply.output, 8192).unwrap();
    let result: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(result["result"]["registered"], true);
    assert!(result["result"].get("launch_token").is_none());
    assert!(!reply.connection.initialized());
    assert!(!connection.initialized());
    handle.shutdown().await.unwrap();
    let mut exit = task.await.unwrap();
    assert_eq!(exit.service.control.external_agents().bindings.len(), 1);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Saturation blocks new allocations, not native inspection of an exact accepted
/// creator/session/instance retry. Existing primary launches consume real slots
/// but supply no role/credential to the curated helper. Changed instances cannot
/// rotate implicitly or lookup-newest; original public identity remains unchanged.
#[tokio::test(flavor = "current_thread")]
async fn external_curated_admission_retry_at_capacity_does_not_rotate_instance() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let (_socket, connection) = child(&mut fixture).await;
    let first = enroll(&mut fixture, &connection).await;
    assert!(first.get("error").is_none());
    let primary = fixture
        .service
        .attach_primary(
            "fixture",
            true,
            mez_mux::layout::Size::new(80, 24).unwrap(),
            120,
        )
        .unwrap();
    for _ in 1..super::super::super::external_agents::MAX_BINDINGS {
        let reply = fixture.service.dispatch_runtime_control_body(r#"{"jsonrpc":"2.0","id":"fill","method":"agent/external/launch","params":{"pane_id":"%1","harness":"pi","version":"fixture"}}"#, &primary);
        assert!(
            serde_json::from_str::<serde_json::Value>(&reply)
                .unwrap()
                .get("error")
                .is_none()
        );
    }
    let retry = enroll(&mut fixture, &connection).await;
    assert_eq!(first, retry);
    let mut request = curated_request();
    let mut params: serde_json::Value =
        serde_json::from_str(request.params.as_deref().unwrap()).unwrap();
    params["observer_instance"] = "module-b".into();
    request.params = Some(params.to_string());
    let work = fixture
        .service
        .prepare_external_enrollment(&request, &connection)
        .unwrap();
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
    assert!(reply.get("error").is_some());
    assert_eq!(
        fixture.service.control.external_agents().bindings.len(),
        super::super::super::external_agents::MAX_BINDINGS
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
}

/// Expiry cannot silently re-admit the same live creator/session or a replacement
/// observer, even after the original capability tombstone is garbage-collected.
/// Each case uses real retained native source evidence; the fence is not a bearer
/// capability and must preserve fail-closed observer/run/accounting semantics.
#[tokio::test(flavor = "current_thread")]
async fn external_curated_admission_expired_namespace_rejects_retry_after_gc() {
    for garbage_collect in [false, true] {
        for changed_instance in [false, true] {
            let Some(mut fixture) = fixture("hold").await else {
                return;
            };
            let (_socket, connection) = child(&mut fixture).await;
            let first = enroll(&mut fixture, &connection).await;
            assert!(first.get("error").is_none());
            fixture
                .service
                .control
                .external_agents_mut()
                .bindings
                .values_mut()
                .next()
                .unwrap()
                .expires = current_unix_seconds() - 1;
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
            if garbage_collect {
                fixture
                    .service
                    .control
                    .external_agents_mut()
                    .bindings
                    .values_mut()
                    .next()
                    .unwrap()
                    .expires = current_unix_seconds() - 1;
                fixture.service.reconcile_external_agent_registrations();
                assert!(
                    fixture
                        .service
                        .control
                        .external_agents()
                        .bindings
                        .is_empty()
                );
            }
            assert!(fixture.connection.unix_origin().unwrap().is_live());
            let mut request = curated_request();
            if changed_instance {
                let mut params: serde_json::Value =
                    serde_json::from_str(request.params.as_deref().unwrap()).unwrap();
                params["observer_instance"] = "replacement".into();
                request.params = Some(params.to_string());
            }
            let generation = fixture.service.control.external_agents().next_generation;
            let work = fixture
                .service
                .prepare_external_enrollment(&request, &connection)
                .unwrap();
            let native = work.clone();
            let observed = tokio::task::spawn_blocking(move || native.observe())
                .await
                .unwrap();
            assert!(observed.is_ok());
            let reply: serde_json::Value = serde_json::from_str(
                &fixture
                    .service
                    .complete_external_enrollment(work, observed, &connection),
            )
            .unwrap();
            assert!(
                reply.get("error").is_some(),
                "expired creator namespace was implicitly replaced (gc={garbage_collect}, changed={changed_instance})"
            );
            assert_eq!(
                fixture.service.control.external_agents().next_generation,
                generation
            );
            assert!(
                !fixture
                    .service
                    .control
                    .external_agents()
                    .enrollments
                    .curated_namespaces
                    .is_empty()
            );
            assert!(fixture.service.external_agent_cleanup_needed());
            if garbage_collect {
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
                assert!(!fixture.service.external_agent_cleanup_needed());
            }
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
}

/// Expiring and garbage-collecting real accepted sources does not evade the
/// independent finite namespace bound. No live creator fence is evicted to allow
/// another session label, and rejected allocation leaves generations unchanged.
#[tokio::test(flavor = "current_thread")]
async fn external_curated_admission_namespace_capacity_survives_binding_gc() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let (_socket, connection) = child(&mut fixture).await;
    for index in 0..super::super::super::external_agents::MAX_BINDINGS {
        let mut request = curated_request();
        let mut params: serde_json::Value =
            serde_json::from_str(request.params.as_deref().unwrap()).unwrap();
        params["external_session_id"] = format!("session-{index}").into();
        request.params = Some(params.to_string());
        let work = fixture
            .service
            .prepare_external_enrollment(&request, &connection)
            .unwrap();
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
        assert!(reply.get("error").is_none());
        fixture
            .service
            .control
            .external_agents_mut()
            .bindings
            .values_mut()
            .next()
            .unwrap()
            .expires = current_unix_seconds() - 1;
        fixture.service.reconcile_external_agent_registrations();
        fixture
            .service
            .control
            .external_agents_mut()
            .bindings
            .values_mut()
            .next()
            .unwrap()
            .expires = current_unix_seconds() - 1;
        fixture.service.reconcile_external_agent_registrations();
        assert!(
            fixture
                .service
                .control
                .external_agents()
                .bindings
                .is_empty()
        );
    }
    let generation = fixture.service.control.external_agents().next_generation;
    let reply = enroll(&mut fixture, &connection).await;
    assert!(reply.get("error").is_some());
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
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .enrollments
            .pending
            .is_empty()
    );
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
}

/// Binding-only snapshot cleanup drops capabilities/identities, not the original
/// living creator/session retirement fence. Re-admission requires explicit policy
/// rather than implicitly allocating a new namespace under the same native source.
#[tokio::test(flavor = "current_thread")]
async fn external_curated_admission_snapshot_binding_cleanup_keeps_namespace_fence() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let (_socket, connection) = child(&mut fixture).await;
    assert!(
        enroll(&mut fixture, &connection)
            .await
            .get("error")
            .is_none()
    );
    fixture.service.retire_unbound_external_message_identities();
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .is_empty()
    );
    let generation = fixture.service.control.external_agents().next_generation;
    assert!(
        enroll(&mut fixture, &connection)
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
}

/// Generic helpers, unknown contracts, other harnesses and compact-only initial
/// boundaries cannot select parent ownership by declarations. Strict extra fields
/// and unqualified/no-source successful Results are rejected before allocation.
#[tokio::test(flavor = "current_thread")]
async fn external_curated_admission_rejects_metadata_or_missing_native_proof() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let (_socket, connection) = child(&mut fixture).await;
    for (field, value) in [
        ("harness", "pi"),
        ("observer_kind", "helper"),
        ("source_contract", "unknown"),
        ("session_boundary", "compact"),
        ("pid", "42"),
    ] {
        let mut request = curated_request();
        let mut params: serde_json::Value =
            serde_json::from_str(request.params.as_deref().unwrap()).unwrap();
        params[field] = value.into();
        request.params = Some(params.to_string());
        assert!(
            fixture
                .service
                .prepare_external_enrollment(&request, &connection)
                .is_err()
        );
    }
    let work = fixture
        .service
        .prepare_external_enrollment(&curated_request(), &connection)
        .unwrap();
    let response: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, Ok(()), &connection),
    )
    .unwrap();
    assert!(response.get("error").is_some());
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
