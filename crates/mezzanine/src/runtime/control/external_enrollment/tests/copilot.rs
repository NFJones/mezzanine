//! Classic command-hook native source and original-session Copilot qualification.
//!
//! Genuine fixture processes provide all native facts; metadata supplies no PID,
//! credential or client role. Installed acceptance invokes production hook bytes.

use super::curated_admission::curated_request;
use super::helper_presentation::{child, settle};
use super::*;

/// Distinct classic declaration; vendor labels are inert native-work selectors.
fn declaration(session: &str) -> JsonRpcRequest {
    let mut request = curated_request();
    let mut params: serde_json::Value =
        serde_json::from_str(request.params.as_deref().unwrap()).unwrap();
    params["harness"] = "copilot".into();
    params["observer_kind"] = "command-hook".into();
    params["source_contract"] = "copilot-local-command/1".into();
    params["observer_instance"] = "copilot-local-hooks-1".into();
    params["external_session_id"] = session.into();
    request.params = Some(params.to_string());
    request
}

/// Fixed local observation; no payload state/title/credentials can cross ingress.
fn callback(session: &str, event: &str, timestamp: u64) -> JsonRpcRequest {
    crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"copilot-test","method":"agent/external/copilot-observe","params":{"pane_id":"%1","external_session_id":session,"event":event,"timestamp":timestamp}}).to_string()).unwrap()
}

/// Attachment has no event, claimed boundary, generation, credential or PID.
/// The indexed candidate still needs the worker's original parent/root proof.
fn attachment(session: &str) -> JsonRpcRequest {
    crate::control::parse_json_rpc_request(
        &serde_json::json!({"jsonrpc":"2.0","id":"copilot-attach-test",
            "method":"agent/external/copilot-attach",
            "params":{"pane_id":"%1","external_session_id":session}})
        .to_string(),
    )
    .unwrap()
}

/// Original public handles and an inert instance/counter, never a supplied PID.
fn pulse(enrolled: &serde_json::Value, nonce: &str, sequence: u64) -> JsonRpcRequest {
    crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"copilot-pulse-test",
        "method":"agent/external/copilot-pulse","params":{"pane_id":"%1","external_session_id":"session-a",
        "generation":enrolled["result"]["generation"],"observer_witness":enrolled["result"]["observer_witness"],
        "observer_id":nonce,"sequence":sequence}}).to_string()).unwrap()
}

/// A real extension child can supply finite proof independently of classic
/// event/presentation counters. Identical/stale pulses cannot refresh its clock,
/// another child or nonce cannot borrow its lifetime, and only daemon maintenance
/// renews the lease. Actual writer death invalidates proof while the CLI survives;
/// a newly qualified child may replace only that dead observer, not the source.
#[tokio::test(flavor = "current_thread")]
async fn external_copilot_idle_pulses_have_exact_writer_and_independent_replay() {
    use super::helper_presentation::release;
    let mut fixture = fixture("hold").await.expect("native Copilot fixture");
    let (mut socket, connection) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_enrollment(&declaration("session-a"), &connection)
        .unwrap();
    let first = settle(&mut fixture, work, &connection).await;
    let a = "a".repeat(64);
    let b = "b".repeat(64);
    for sequence in [0, 2] {
        let request = pulse(&first, &a, sequence);
        match fixture
            .service
            .prepare_external_helper_presentation(&request, &connection)
        {
            Ok(work) => assert!(
                settle(&mut fixture, work, &connection)
                    .await
                    .get("error")
                    .is_some()
            ),
            Err(_) => assert_eq!(sequence, 0),
        }
    }
    let work = fixture
        .service
        .prepare_external_helper_presentation(&pulse(&first, &a, 1), &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["changed"],
        true
    );
    for (event, timestamp) in [("userPromptSubmitted", 1), ("agentStop", 2)] {
        let work = fixture
            .service
            .prepare_external_helper_presentation(
                &callback("session-a", event, timestamp),
                &connection,
            )
            .unwrap();
        assert!(
            settle(&mut fixture, work, &connection)
                .await
                .get("error")
                .is_none()
        );
    }
    let registry = fixture.service.control.external_agents_mut();
    let binding = registry.bindings.values_mut().next().unwrap();
    let original = (
        binding.generation,
        binding.accounting_owner.clone(),
        binding.expires,
    );
    let source = binding.enrollment.as_mut().unwrap();
    source.curated_observer.as_mut().unwrap().observed_at =
        Some(Instant::now() - Duration::from_secs(120));
    source.copilot_events.age_extension_for_test();
    let aged = source.copilot_events.extension_snapshot_for_test();
    assert!(!source.has_live_observer());
    let work = fixture
        .service
        .prepare_external_helper_presentation(&pulse(&first, &a, 1), &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["changed"],
        false
    );
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
    assert_eq!(source.copilot_events.extension_snapshot_for_test(), aged);
    assert!(!source.has_live_observer());
    let work = fixture
        .service
        .prepare_external_helper_presentation(&pulse(&first, &a, 2), &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["changed"],
        true
    );
    let (mut other_socket, other) = child(&mut fixture).await;
    for (who, nonce, sequence) in [(&other, &a, 3), (&connection, &b, 1), (&connection, &a, 1)] {
        let work = fixture
            .service
            .prepare_external_helper_presentation(&pulse(&first, nonce, sequence), who)
            .unwrap();
        assert!(settle(&mut fixture, work, who).await.get("error").is_some());
    }
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
            binding.expires
        ),
        original
    );
    let source = binding.enrollment.as_ref().unwrap();
    assert!(source.has_live_observer());
    assert_eq!(source.curated_observer.as_ref().unwrap().sequence, 2);
    assert_eq!(
        binding
            .registration
            .as_ref()
            .unwrap()
            .presentation
            .as_ref()
            .unwrap()
            .sequence,
        2
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
    let late = fixture
        .service
        .prepare_external_helper_presentation(&pulse(&first, &a, 3), &connection)
        .unwrap();
    release(&mut socket, &connection).await;
    assert!(
        !fixture
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
            .has_live_observer()
    );
    assert!(
        settle(&mut fixture, late, &connection)
            .await
            .get("error")
            .is_some()
    );
    let work = fixture
        .service
        .prepare_external_helper_presentation(&pulse(&first, &b, 1), &other)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &other).await["result"]["changed"],
        true
    );
    release(&mut other_socket, &other).await;
    assert!(
        !fixture
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
            .has_live_observer()
    );
    assert!(fixture.connection.unix_origin().unwrap().is_live());
}

/// A real direct child may read the existing original public observer selectors
/// after native verification. Repeating attachment cannot allocate another run,
/// repaint status, refresh freshness, renew a lease or create expense. The pane
/// root itself and a different session cannot borrow that child's relationship.
#[tokio::test(flavor = "current_thread")]
async fn external_copilot_attachment_is_native_and_time_inert() {
    let mut fixture = fixture("hold").await.expect("native Copilot fixture");
    let (_socket, connection) = child(&mut fixture).await;
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&attachment("session-a"), &connection,)
            .is_err()
    );
    let work = fixture
        .service
        .prepare_external_enrollment(&declaration("session-a"), &connection)
        .unwrap();
    let enrolled = settle(&mut fixture, work, &connection).await;
    let work = fixture
        .service
        .prepare_external_helper_presentation(
            &callback("session-a", "userPromptSubmitted", 1),
            &connection,
        )
        .unwrap();
    settle(&mut fixture, work, &connection).await;
    let before = {
        let binding = fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .next()
            .unwrap();
        let observer = binding
            .enrollment
            .as_ref()
            .unwrap()
            .curated_observer
            .as_ref()
            .unwrap();
        (
            binding.generation,
            binding.accounting_owner.clone(),
            binding.expires,
            observer.sequence,
            observer.observed_at,
        )
    };
    for _ in 0..2 {
        let work = fixture
            .service
            .prepare_external_helper_presentation(&attachment("session-a"), &connection)
            .unwrap();
        let reply = settle(&mut fixture, work, &connection).await;
        assert_eq!(reply["id"], "copilot-attach-test");
        assert_eq!(reply["result"], enrolled["result"]);
        assert!(reply["result"].get("launch_token").is_none());
    }
    let root = fixture.connection.clone();
    let work = fixture
        .service
        .prepare_external_helper_presentation(&attachment("session-a"), &root)
        .unwrap();
    assert!(
        settle(&mut fixture, work, &root)
            .await
            .get("error")
            .is_some()
    );
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&attachment("other-session"), &connection,)
            .is_err()
    );
    let registry = fixture.service.control.external_agents();
    assert_eq!(registry.bindings.len(), 1);
    let binding = registry.bindings.values().next().unwrap();
    let observer = binding
        .enrollment
        .as_ref()
        .unwrap()
        .curated_observer
        .as_ref()
        .unwrap();
    assert_eq!(
        (
            binding.generation,
            binding.accounting_owner.clone(),
            binding.expires,
            observer.sequence,
            observer.observed_at
        ),
        before
    );
    let presentation = binding
        .registration
        .as_ref()
        .unwrap()
        .presentation
        .as_ref()
        .unwrap();
    assert_eq!(presentation.sequence, 1);
    assert_eq!(presentation.state, "running");
    assert!(registry.enrollments.pending.is_empty());
    assert!(!connection.initialized());
    assert!(connection.caller_client_id().is_none());
}

/// Rotation while native lifecycle/end work is pending fences the original
/// generation. Stateless callbacks cannot adopt the successor even with the same
/// creator/session metadata; rejection must not retire the live replacement.
#[tokio::test(flavor = "current_thread")]
async fn external_copilot_rotation_rejects_pending_and_new_original_callbacks() {
    let Some(mut fixture) = fixture("hold").await else {
        panic!("Copilot rotation requires native evidence");
    };
    let (_socket, connection) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_enrollment(&declaration("session-a"), &connection)
        .unwrap();
    let first = settle(&mut fixture, work, &connection).await;
    assert_eq!(first["result"]["registered"], true);
    let work = fixture
        .service
        .prepare_external_helper_presentation(&pulse(&first, &"a".repeat(64), 1), &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["changed"],
        true
    );
    let stale = fixture
        .service
        .prepare_external_helper_presentation(
            &callback("session-a", "userPromptSubmitted", 1),
            &connection,
        )
        .unwrap();
    let stale_end = fixture
        .service
        .prepare_external_helper_presentation(&callback("session-a", "sessionEnd", 2), &connection)
        .unwrap();
    let stale_attachment = fixture
        .service
        .prepare_external_helper_presentation(&attachment("session-a"), &connection)
        .unwrap();
    let stale_pulse = fixture
        .service
        .prepare_external_helper_presentation(&pulse(&first, &"a".repeat(64), 2), &connection)
        .unwrap();
    let mut rotation = declaration("session-a");
    let mut params: serde_json::Value =
        serde_json::from_str(rotation.params.as_deref().unwrap()).unwrap();
    params["observer_instance"] = "successor".into();
    params["predecessor_generation"] = first["result"]["generation"].clone();
    rotation.params = Some(params.to_string());
    let work = fixture
        .service
        .prepare_external_enrollment(&rotation, &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["registered"],
        true
    );
    for work in [stale, stale_end, stale_attachment, stale_pulse] {
        assert!(
            settle(&mut fixture, work, &connection)
                .await
                .get("error")
                .is_some()
        );
    }
    for event in ["agentStop", "sessionEnd"] {
        assert!(
            fixture
                .service
                .prepare_external_helper_presentation(&callback("session-a", event, 3), &connection)
                .is_err()
        );
    }
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&attachment("session-a"), &connection,)
            .is_err()
    );
    let registry = fixture.service.control.external_agents();
    assert_eq!(registry.bindings.len(), 1);
    assert!(registry.bindings.values().all(|b| !b.retired));
    assert!(
        registry
            .bindings
            .values()
            .all(|b| !b.enrollment.as_ref().unwrap().has_live_observer())
    );
    assert!(registry.enrollments.pending.is_empty());
    assert!(!connection.initialized());
    assert!(connection.caller_client_id().is_none());
}

/// Documented classic Copilot exec hooks carry a session identifier without SDK
/// invocation context. A genuine same-user direct creator should enroll under
/// its own contract; a Codex or Claude declaration is not a substitute.
///
/// A successful declaration is only observational registration, not accounting
/// or persistent idle observer health.
#[tokio::test(flavor = "current_thread")]
async fn external_copilot_classic_command_creator_is_supported() {
    let Some(mut fixture) = fixture("hold").await else {
        panic!("Copilot creator requires native peer evidence");
    };
    let (_socket, connection) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_enrollment(&declaration("session-a"), &connection)
        .unwrap();
    let reply = settle(&mut fixture, work, &connection).await;
    assert_eq!(reply["result"]["registered"], true, "{reply}");
}

/// Original native callbacks conserve replay sequences, reject root/foreign
/// writers, invalid semantics and stale/tied facts, and never grant a client role
/// or renew leases directly. Ending an old /clear session preserves replacement.
///
/// Observer replacement itself is covered separately to distinguish a
/// different session from a changed epoch of this exact original session.
#[tokio::test(flavor = "current_thread")]
async fn external_copilot_native_lifecycle_replay_and_exact_end() {
    let Some(mut fixture) = fixture("hold").await else {
        panic!("Copilot lifecycle requires native evidence");
    };
    let (_socket, connection) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_enrollment(&declaration("session-a"), &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["registered"],
        true
    );
    let expires = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap()
        .expires;
    for (event, time, state, seq) in [
        ("userPromptSubmitted", 1, "running", 1),
        ("permission_prompt", 2, "approval-wait", 2),
        ("postToolUse", 3, "running", 3),
        ("elicitation_dialog", 4, "input-wait", 4),
        ("preToolUse", 5, "running", 5),
        ("agentStop", 6, "ready", 6),
        ("errorOccurred", 7, "interrupted", 7),
    ] {
        for changed in [true, false] {
            let work = fixture
                .service
                .prepare_external_helper_presentation(
                    &callback("session-a", event, time),
                    &connection,
                )
                .unwrap();
            let reply = settle(&mut fixture, work, &connection).await;
            assert_eq!(reply["id"], "copilot-test");
            assert_eq!(reply["result"]["sequence"], seq, "{reply}");
            assert_eq!(reply["result"]["changed"], changed, "{reply}");
            assert_eq!(
                fixture
                    .service
                    .live_pane_harness_status("%1")
                    .unwrap()
                    .text
                    .as_deref(),
                Some(state)
            );
        }
    }
    for time in [6, 7] {
        let work = fixture
            .service
            .prepare_external_helper_presentation(
                &callback("session-a", "agentStop", time),
                &connection,
            )
            .unwrap();
        assert!(
            settle(&mut fixture, work, &connection)
                .await
                .get("error")
                .is_some()
        );
    }
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(
                &callback("session-a", "permissionRequest", 8),
                &connection
            )
            .is_err()
    );
    let root = fixture.connection.clone();
    let work = fixture
        .service
        .prepare_external_helper_presentation(&callback("session-a", "agentStop", 8), &root)
        .unwrap();
    assert!(
        settle(&mut fixture, work, &root)
            .await
            .get("error")
            .is_some()
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
        expires
    );
    assert!(!connection.initialized());
    assert!(connection.caller_client_id().is_none());
    let work = fixture
        .service
        .prepare_external_enrollment(&declaration("session-b"), &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["registered"],
        true
    );
    let work = fixture
        .service
        .prepare_external_helper_presentation(
            &callback("session-b", "userPromptSubmitted", 10),
            &connection,
        )
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["changed"],
        true
    );
    let work = fixture
        .service
        .prepare_external_helper_presentation(&callback("session-a", "sessionEnd", 11), &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["retired"],
        true
    );
    let bindings = &fixture.service.control.external_agents().bindings;
    assert!(
        bindings
            .values()
            .find(|b| b.registration.as_ref().unwrap().external_session_id == "session-a")
            .unwrap()
            .retired
    );
    assert!(
        !bindings
            .values()
            .find(|b| b.registration.as_ref().unwrap().external_session_id == "session-b")
            .unwrap()
            .retired
    );
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(
                &callback("session-a", "agentStop", 12),
                &connection
            )
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
