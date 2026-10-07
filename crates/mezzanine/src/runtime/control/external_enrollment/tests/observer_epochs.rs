//! Observer replacement fences using the shared real ordinary-process fixture.

use super::*;

/// Executes one native observation and its actor-owned settlement with a real
/// fixture origin, never replacing ancestry with a test-only authority bypass.
async fn enroll(fixture: &mut Fixture, request: &JsonRpcRequest) -> serde_json::Value {
    let work = fixture
        .service
        .prepare_external_enrollment(request, &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    serde_json::from_str(&fixture.service.complete_external_enrollment(
        work,
        observed,
        &fixture.connection,
    ))
    .unwrap()
}

/// Builds a bounded observer transition selected by the current server epoch;
/// the instance/witness pair narrows replacement, never supplies authority.
fn instance_request(instance: &str, predecessor: Option<u64>) -> JsonRpcRequest {
    let mut request = request();
    let mut params: serde_json::Value =
        serde_json::from_str(request.params.as_deref().unwrap()).unwrap();
    params["observer_instance"] = serde_json::json!(instance);
    if let Some(predecessor) = predecessor {
        params["predecessor_generation"] = serde_json::json!(predecessor);
    }
    request.params = Some(params.to_string());
    request
}

/// Sends lifecycle/presentation only under the returned private observer handle.
/// Diagnostics never print the handle; all producer evidence stays native.
fn callback(
    fixture: &mut Fixture,
    enrollment: &serde_json::Value,
    method: &str,
    extra: serde_json::Value,
) -> Result<String> {
    let mut params = serde_json::json!({"launch_token":enrollment["result"]["launch_token"],
        "generation":enrollment["result"]["generation"],"external_session_id":"session-a"});
    params
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    let request = crate::control::parse_json_rpc_request(
        &serde_json::json!({"jsonrpc":"2.0","id":"event","method":method,"params":params})
            .to_string(),
    )
    .unwrap();
    fixture
        .service
        .dispatch_external_agent_request(&request, &fixture.connection)
}

/// Replacement creates a fresh private generation and sequence owner while
/// preserving the immutable run, agent and accounting provenance. Old end,
/// renew and presentation events cannot overwrite/retire the new observer.
#[tokio::test(flavor = "current_thread")]
async fn external_enrollment_observer_epoch_replacement_fences_old_callbacks() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let initial = enroll(&mut fixture, &request()).await;
    assert!(initial.get("error").is_none());
    let original = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap();
    let accounting_owner = original.accounting_owner.clone();
    let accounting_origin = original.accounting_origin.clone();
    callback(
        &mut fixture,
        &initial,
        "agent/external/presentation",
        serde_json::json!({"sequence":99,"state":"running","title":"old observer"}),
    )
    .unwrap();
    let predecessor = initial["result"]["generation"].as_u64().unwrap();
    let replacement_request = instance_request("fixture-instance-b", Some(predecessor));
    let replacement = enroll(&mut fixture, &replacement_request).await;
    assert!(replacement.get("error").is_none());
    assert!(
        initial["result"]["launch_token"] != replacement["result"]["launch_token"],
        "replacement reused a private handle"
    );
    assert_ne!(
        initial["result"]["generation"],
        replacement["result"]["generation"]
    );
    assert_eq!(initial["result"]["run_id"], replacement["result"]["run_id"]);
    assert_eq!(
        initial["result"]["agent_id"],
        replacement["result"]["agent_id"]
    );
    assert_eq!(replacement["result"]["observer_epoch"], 2);
    let binding = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap();
    assert_eq!(binding.accounting_owner, accounting_owner);
    assert_eq!(binding.accounting_origin, accounting_origin);
    assert!(
        binding
            .registration
            .as_ref()
            .unwrap()
            .presentation
            .is_none()
    );
    assert!(fixture.service.live_pane_harness_status("%1").is_none());
    callback(
        &mut fixture,
        &replacement,
        "agent/external/presentation",
        serde_json::json!({"sequence":1,"state":"ready","title":"new observer"}),
    )
    .unwrap();
    for method in [
        "agent/external/deregister",
        "agent/external/renew",
        "agent/external/presentation",
    ] {
        let extra = if method.ends_with("presentation") {
            serde_json::json!({"sequence":100,"state":"failed"})
        } else {
            serde_json::json!({})
        };
        assert!(callback(&mut fixture, &initial, method, extra).is_err());
    }
    let retry = enroll(&mut fixture, &replacement_request).await;
    assert!(
        retry["result"]["launch_token"] == replacement["result"]["launch_token"],
        "same-instance retry rotated private handle"
    );
    assert_eq!(retry["result"]["observer_epoch"], 2);
    let binding = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap();
    assert!(!binding.retired);
    assert_eq!(
        binding
            .registration
            .as_ref()
            .unwrap()
            .presentation
            .as_ref()
            .unwrap()
            .sequence,
        1
    );
    assert!(
        enroll(&mut fixture, &request())
            .await
            .get("error")
            .is_some()
    );
    let newest = replacement["result"]["generation"].as_u64().unwrap();
    let stale_instance = instance_request("fixture-instance-a", Some(newest));
    assert!(
        enroll(&mut fixture, &stale_instance)
            .await
            .get("error")
            .is_some()
    );
    assert_eq!(fixture.service.control.external_agents().bindings.len(), 1);
}

/// Two replacements captured against one generation may finish out of order.
/// Only one can publish; stale predecessor replay cannot rotate the winner even
/// with fresh native evidence and a new admission reservation.
#[tokio::test(flavor = "current_thread")]
async fn external_enrollment_observer_replacements_are_predecessor_fenced() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let initial = enroll(&mut fixture, &request()).await;
    let predecessor = initial["result"]["generation"].as_u64().unwrap();
    let first_request = instance_request("instance-first", Some(predecessor));
    let second_request = instance_request("instance-second", Some(predecessor));
    let first = fixture
        .service
        .prepare_external_enrollment(&first_request, &fixture.connection)
        .unwrap();
    let second = fixture
        .service
        .prepare_external_enrollment(&second_request, &fixture.connection)
        .unwrap();
    let native = second.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    let winner: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(second, observed, &fixture.connection),
    )
    .unwrap();
    assert!(winner.get("error").is_none());
    let native = first.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(
        fixture
            .service
            .complete_external_enrollment(first, observed, &fixture.connection)
            .contains("predecessor changed")
    );
    assert!(
        enroll(&mut fixture, &first_request)
            .await
            .get("error")
            .is_some()
    );
    let retry = enroll(&mut fixture, &second_request).await;
    assert!(
        retry["result"]["launch_token"] == winner["result"]["launch_token"],
        "out-of-order completion displaced winner"
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

/// Exhausting bounded live-run instance fences rejects replacement rather than
/// evicting old IDs and accepting stale takeover. The current observer still has
/// stable retry/renewal rights after the exact 128-instance boundary.
#[tokio::test(flavor = "current_thread")]
async fn external_enrollment_observer_instance_fences_never_evict() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let mut current = enroll(&mut fixture, &request()).await;
    let mut last_request = request();
    for index in 1..MAX_OBSERVER_INSTANCES {
        last_request = instance_request(
            &format!("instance-{index}"),
            current["result"]["generation"].as_u64(),
        );
        current = enroll(&mut fixture, &last_request).await;
        assert!(
            current.get("error").is_none(),
            "bounded observer replacement rejected"
        );
    }
    assert_eq!(
        current["result"]["observer_epoch"],
        MAX_OBSERVER_INSTANCES as u64
    );
    let overflow = instance_request(
        "instance-overflow",
        current["result"]["generation"].as_u64(),
    );
    assert!(enroll(&mut fixture, &overflow).await.get("error").is_some());
    let retry = enroll(&mut fixture, &last_request).await;
    assert!(
        retry["result"]["launch_token"] == current["result"]["launch_token"],
        "exhaustion changed current private handle"
    );
    assert!(
        enroll(&mut fixture, &request())
            .await
            .get("error")
            .is_some()
    );
    assert_eq!(fixture.service.control.external_agents().bindings.len(), 1);
}
