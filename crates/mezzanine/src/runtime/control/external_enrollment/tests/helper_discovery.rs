//! Token-free callbacks to already-enrolled producers, never parent enrollment.
//!
//! Selectors find one indexed candidate; genuine native child-parent and pane
//! ancestry evidence remains authority. Unknown or ambiguous selectors must fail
//! closed without allocating producer, client, observer, accounting or lease.

use super::helper_presentation::{child, enroll_producer, settle};
use super::*;

/// Emits only nonsecret candidate selectors and inert generic presentation.
/// No private handle, PID, pane claim or client role is supplied. The captured
/// public source generation only narrows the current indexed candidate.
fn observation(enrolled: &serde_json::Value, sequence: u64) -> JsonRpcRequest {
    crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"observe",
        "method":"agent/external/helper-observe","params":{"harness":"pi","generation":enrolled["result"]["generation"],
        "observer_witness":enrolled["result"]["observer_witness"],"external_session_id":"session-a",
        "sequence":sequence,"state":"running"}}).to_string()).unwrap()
}

/// Numeric generations can repeat in separate daemon instances. The original
/// observer witness must reject missing/foreign instance data even when every
/// other selector matches and a real child has valid native parent evidence.
#[tokio::test(flavor = "current_thread")]
async fn external_helper_discovery_observer_witness_fences_reused_generations() {
    let Some(mut first) = fixture("hold").await else {
        return;
    };
    let old = enroll_producer(&mut first).await;
    let Some(mut second) = fixture("hold").await else {
        return;
    };
    let current = enroll_producer(&mut second).await;
    assert_eq!(generation(&old), generation(&current));
    assert!(old["result"]["observer_witness"] != current["result"]["observer_witness"]);
    let (_socket, connection) = child(&mut second).await;
    let event = observation(&current, 1);
    for witness in [
        None,
        Some(old["result"]["observer_witness"].clone()),
        Some(serde_json::json!("x".repeat(64))),
    ] {
        let mut stale = event.clone();
        let mut params: serde_json::Value =
            serde_json::from_str(stale.params.as_deref().unwrap()).unwrap();
        if let Some(witness) = witness {
            params["observer_witness"] = witness;
        } else {
            params.as_object_mut().unwrap().remove("observer_witness");
        }
        stale.params = Some(params.to_string());
        assert!(
            second
                .service
                .prepare_external_helper_presentation(&stale, &connection)
                .is_err()
        );
    }
    let work = second
        .service
        .prepare_external_helper_presentation(&event, &connection)
        .unwrap();
    assert_eq!(
        settle(&mut second, work, &connection).await["result"]["changed"],
        true
    );
    assert!(
        second
            .service
            .control
            .external_agents()
            .enrollments
            .pending
            .is_empty()
    );
    let witness = current["result"]["observer_witness"].as_str().unwrap();
    assert_eq!(witness.len(), 64);
    // A public witness is not shaped as the private 43-byte capability and
    // cannot authorize producer-only lifecycle operations on its own.
    assert!(
        super::super::super::external_agents::credential(
            &serde_json::json!({"launch_token":witness})
        )
        .is_err()
    );
}

/// Captures only the nonsecret server generation from the producer's reply.
fn generation(enrolled: &serde_json::Value) -> u64 {
    enrolled["result"]["generation"].as_u64().unwrap()
}

/// Every eligible canonical persistent source can use the token-free callback
/// path with an actual native direct child. Labels remain metadata and cannot
/// replace parent proof or create a missing vendor integration/usage producer.
#[tokio::test(flavor = "current_thread")]
async fn external_helper_discovery_six_labels_retain_native_parent_authority() {
    for harness in ["claude", "codex", "copilot", "opencode", "cursor", "pi"] {
        let Some(mut fixture) = fixture("hold").await else {
            return;
        };
        let mut request = request();
        let mut params: serde_json::Value =
            serde_json::from_str(request.params.as_deref().unwrap()).unwrap();
        params["harness"] = serde_json::json!(harness);
        request.params = Some(params.to_string());
        let work = fixture
            .service
            .prepare_external_enrollment(&request, &fixture.connection)
            .unwrap();
        let own_connection = fixture.connection.clone();
        let enrolled = settle(&mut fixture, work, &own_connection).await;
        assert!(enrolled.get("error").is_none());
        let (_socket, connection) = child(&mut fixture).await;
        let mut event = observation(&enrolled, 1);
        let mut params: serde_json::Value =
            serde_json::from_str(event.params.as_deref().unwrap()).unwrap();
        params["harness"] = serde_json::json!(harness);
        event.params = Some(params.to_string());
        let work = fixture
            .service
            .prepare_external_helper_presentation(&event, &connection)
            .unwrap();
        assert_eq!(
            settle(&mut fixture, work, &connection).await["result"]["changed"],
            true
        );
        assert!(!connection.initialized());
        assert_eq!(fixture.service.control.external_agents().bindings.len(), 1);
    }
}

/// Helper cleanup cannot sweep unrelated registrations. An expired legacy slot
/// remains untouched while a healthy indexed callback settles; expiring the
/// selected ordinary run removes its index. Pi-owned generic sequence updates
/// reject independently of native qualification or credential possession.
#[tokio::test(flavor = "current_thread")]
async fn external_helper_discovery_cleanup_is_targeted_and_ownership_is_preserved() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let enrolled = enroll_producer(&mut fixture).await;
    let (_socket, connection) = child(&mut fixture).await;
    let primary = fixture
        .service
        .attach_primary(
            "legacy negative",
            true,
            mez_mux::layout::Size::new(80, 24).unwrap(),
            120,
        )
        .unwrap();
    let legacy: serde_json::Value = serde_json::from_str(&fixture.service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"legacy","method":"agent/external/launch","params":{"pane_id":"%1","harness":"opencode","version":"fixture"}}"#, &primary)).unwrap();
    assert!(legacy.get("error").is_none());
    let legacy_digest = super::super::super::external_agents::credential(
        &serde_json::json!({"launch_token":legacy["result"]["launch_token"]}),
    )
    .unwrap();
    fixture
        .service
        .control
        .external_agents_mut()
        .bindings
        .get_mut(&legacy_digest)
        .unwrap()
        .expires = current_unix_seconds() - 1;
    let event = observation(&enrolled, 1);
    let work = fixture
        .service
        .prepare_external_helper_presentation(&event, &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["changed"],
        true
    );
    assert!(!fixture.service.control.external_agents().bindings[&legacy_digest].retired);
    let mut legacy_event = observation(&legacy, 2);
    let mut params: serde_json::Value =
        serde_json::from_str(legacy_event.params.as_deref().unwrap()).unwrap();
    params["harness"] = serde_json::json!("opencode");
    legacy_event.params = Some(params.to_string());
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&legacy_event, &connection)
            .is_err()
    );
    let pi = crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"pi","method":"agent/external/pi-observation","params":{
        "launch_token":enrolled["result"]["launch_token"],"generation":enrolled["result"]["generation"],"external_session_id":"session-a","sequence":1,
        "event":{"type":"session_start","reason":"startup"}}}).to_string()).unwrap();
    // Generic presentation already owns this epoch, so first rotate to a fresh
    // observer before establishing the production Pi sequence owner.
    let mut replacement = request();
    let mut params: serde_json::Value =
        serde_json::from_str(replacement.params.as_deref().unwrap()).unwrap();
    params["observer_instance"] = serde_json::json!("pi-owner");
    params["predecessor_generation"] = serde_json::json!(generation(&enrolled));
    replacement.params = Some(params.to_string());
    let work = fixture
        .service
        .prepare_external_enrollment(&replacement, &fixture.connection)
        .unwrap();
    let own_connection = fixture.connection.clone();
    let updated = settle(&mut fixture, work, &own_connection).await;
    assert!(updated.get("error").is_none());
    let mut pi = pi;
    let mut params: serde_json::Value =
        serde_json::from_str(pi.params.as_deref().unwrap()).unwrap();
    params["launch_token"] = updated["result"]["launch_token"].clone();
    params["generation"] = updated["result"]["generation"].clone();
    pi.params = Some(params.to_string());
    fixture
        .service
        .dispatch_external_agent_request(&pi, &fixture.connection)
        .unwrap();
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&observation(&updated, 2), &connection)
            .unwrap_err()
            .message()
            .contains("Pi lifecycle")
    );
    let uid = connection.unix_origin().unwrap().uid();
    let digest = fixture
        .service
        .control
        .external_agents()
        .enrollments
        .helper_targets
        .select(uid, "pi", "session-a")
        .unwrap();
    fixture
        .service
        .control
        .external_agents_mut()
        .bindings
        .get_mut(&digest)
        .unwrap()
        .expires = current_unix_seconds() - 1;
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&observation(&updated, 2), &connection)
            .is_err()
    );
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .enrollments
            .helper_targets
            .candidates(uid, "pi", "session-a")
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

/// Snapshot replacement clears targets with registrations. In-flight helper
/// work releases its reservation but cannot publish after clearing; a fresh
/// enrollment remains uniquely indexed and rejects old generation callbacks.
#[tokio::test(flavor = "current_thread")]
async fn external_helper_discovery_bulk_retirement_clears_index_and_old_work() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let enrolled = enroll_producer(&mut fixture).await;
    let (_socket, connection) = child(&mut fixture).await;
    let event = observation(&enrolled, 1);
    let work = fixture
        .service
        .prepare_external_helper_presentation(&event, &connection)
        .unwrap();
    let uid = connection.unix_origin().unwrap().uid();
    fixture.service.retire_unbound_external_message_identities();
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
            .helper_targets
            .candidates(uid, "pi", "session-a")
            .is_empty()
    );
    assert!(
        settle(&mut fixture, work, &connection)
            .await
            .get("error")
            .is_some()
    );
    let fresh = enroll_producer(&mut fixture).await;
    assert!(generation(&fresh) > generation(&enrolled));
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&event, &connection)
            .is_err()
    );
    let work = fixture
        .service
        .prepare_external_helper_presentation(&observation(&fresh, 1), &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["changed"],
        true
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

/// Missing producers cannot be enrolled by a helper. A known selector still
/// cannot authorize the producer itself or another parent's genuine child;
/// mixed credentials/PID/pane/unknown selectors and invalid generations reject.
#[tokio::test(flavor = "current_thread")]
async fn external_helper_discovery_selectors_never_substitute_for_native_parent() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let (_socket, connection) = child(&mut fixture).await;
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(
                &observation(&serde_json::json!({"result":{"generation":1}}), 1),
                &connection
            )
            .is_err()
    );
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .is_empty()
    );
    let enrolled = enroll_producer(&mut fixture).await;
    let event = observation(&enrolled, 1);
    let own_connection = fixture.connection.clone();
    let work = fixture
        .service
        .prepare_external_helper_presentation(&event, &own_connection)
        .unwrap();
    assert!(
        settle(&mut fixture, work, &own_connection)
            .await
            .get("error")
            .is_some()
    );
    let Some(mut other) = super::fixture("hold").await else {
        return;
    };
    let (_socket, unrelated) = child(&mut other).await;
    let work = fixture
        .service
        .prepare_external_helper_presentation(&event, &unrelated)
        .unwrap();
    assert!(
        settle(&mut fixture, work, &unrelated)
            .await
            .get("error")
            .is_some()
    );
    for (field, value) in [
        ("launch_token", serde_json::json!("x".repeat(43))),
        ("pane_id", serde_json::json!("%1")),
        ("pid", serde_json::json!(1)),
        ("generation", serde_json::json!(0)),
        ("generation", serde_json::Value::Null),
        ("harness", serde_json::json!("gemini")),
        ("harness", serde_json::json!("unknown")),
        ("external_session_id", serde_json::json!("other")),
    ] {
        let mut changed = event.clone();
        let mut params: serde_json::Value =
            serde_json::from_str(changed.params.as_deref().unwrap()).unwrap();
        params[field] = value;
        changed.params = Some(params.to_string());
        assert!(
            fixture
                .service
                .prepare_external_helper_presentation(&changed, &connection)
                .is_err()
        );
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
    assert_eq!(fixture.service.control.external_agents().bindings.len(), 1);
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .next()
            .unwrap()
            .registration
            .as_ref()
            .unwrap()
            .presentation
            .is_none()
    );
}

/// Observer replacement moves the indexed target exactly once. Work already
/// captured against the old generation and callbacks delayed until after rotation
/// both reject; fresh witnessed callbacks work, and retirement removes the key.
#[tokio::test(flavor = "current_thread")]
async fn external_helper_discovery_rotation_and_retirement_fence_old_sources() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let enrolled = enroll_producer(&mut fixture).await;
    let (_socket, connection) = child(&mut fixture).await;
    let old_event = observation(&enrolled, 1);
    let old_work = fixture
        .service
        .prepare_external_helper_presentation(&old_event, &connection)
        .unwrap();
    let uid = connection.unix_origin().unwrap().uid();
    let original_digest = fixture
        .service
        .control
        .external_agents()
        .enrollments
        .helper_targets
        .select(uid, "pi", "session-a")
        .unwrap();
    let mut replacement = request();
    let mut params: serde_json::Value =
        serde_json::from_str(replacement.params.as_deref().unwrap()).unwrap();
    params["observer_instance"] = serde_json::json!("replacement");
    params["predecessor_generation"] = serde_json::json!(generation(&enrolled));
    replacement.params = Some(params.to_string());
    let work = fixture
        .service
        .prepare_external_enrollment(&replacement, &fixture.connection)
        .unwrap();
    let own_connection = fixture.connection.clone();
    let updated = settle(&mut fixture, work, &own_connection).await;
    assert!(updated.get("error").is_none());
    assert!(
        settle(&mut fixture, old_work, &connection)
            .await
            .get("error")
            .is_some()
    );
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&old_event, &connection)
            .is_err()
    );
    let digest = fixture
        .service
        .control
        .external_agents()
        .enrollments
        .helper_targets
        .select(uid, "pi", "session-a")
        .unwrap();
    assert_ne!(digest, original_digest);
    let fresh = observation(&updated, 1);
    let work = fixture
        .service
        .prepare_external_helper_presentation(&fresh, &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["changed"],
        true
    );
    fixture.service.retire_external_agent_binding(digest);
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .enrollments
            .helper_targets
            .select(uid, "pi", "session-a")
            .is_err()
    );
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&fresh, &connection)
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

/// Two actual ordinary producers with the same selectors remain ambiguous even
/// when the callback is a genuine child of one. No focus/first-match fallback is
/// allowed. Retiring the other exact owner restores only the original target.
#[tokio::test(flavor = "current_thread")]
async fn external_helper_discovery_ambiguous_producers_fail_closed_without_scans() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let enrolled = enroll_producer(&mut fixture).await;
    let (_socket, connection) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &connection)
        .unwrap();
    let second = settle(&mut fixture, work, &connection).await;
    assert!(second.get("error").is_none());
    let uid = connection.unix_origin().unwrap().uid();
    assert_eq!(
        fixture
            .service
            .control
            .external_agents()
            .enrollments
            .helper_targets
            .select(uid, "pi", "session-a")
            .unwrap_err()
            .kind(),
        crate::error::MezErrorKind::Conflict
    );
    let event = observation(&enrolled, 1);
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&event, &connection)
            .is_err()
    );
    let digest = super::super::super::external_agents::credential(
        &serde_json::json!({"launch_token":second["result"]["launch_token"]}),
    )
    .unwrap();
    fixture.service.retire_external_agent_binding(digest);
    let work = fixture
        .service
        .prepare_external_helper_presentation(&event, &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["changed"],
        true
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

/// A genuine child discovers its existing producer without receiving a private
/// token. It updates only generic presentation; retry remains inert, no other
/// run/accounting/observer/client/lease ownership is created or extended.
#[tokio::test(flavor = "current_thread")]
async fn external_helper_discovery_native_child_uses_daemon_owned_target_only() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let enrolled = enroll_producer(&mut fixture).await;
    let (_socket, connection) = child(&mut fixture).await;
    let binding = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap();
    let before = (
        binding.generation,
        binding.accounting_owner.clone(),
        binding.expires,
        binding.enrollment.as_ref().unwrap().observers.len(),
    );
    for changed in [true, false] {
        let work = fixture
            .service
            .prepare_external_helper_presentation(&observation(&enrolled, 1), &connection)
            .unwrap();
        let result = settle(&mut fixture, work, &connection).await;
        assert_eq!(result["result"]["changed"], changed);
    }
    assert_eq!(fixture.service.control.external_agents().bindings.len(), 1);
    let binding = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap();
    assert_eq!(
        (
            binding.generation,
            binding.accounting_owner.clone(),
            binding.expires,
            binding.enrollment.as_ref().unwrap().observers.len()
        ),
        before
    );
    assert!(!connection.initialized());
    assert!(connection.caller_client_id().is_none());
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
