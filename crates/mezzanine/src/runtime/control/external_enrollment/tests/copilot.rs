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
    for work in [stale, stale_end, stale_attachment] {
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
