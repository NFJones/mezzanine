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

/// Documented classic Copilot exec hooks carry a session identifier without SDK
/// invocation context. A genuine same-user direct creator should enroll under
/// its own contract; a Codex or Claude declaration is not a substitute.
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
        ("permission_prompt", 2, "waiting", 2),
        ("postToolUse", 3, "running", 3),
        ("elicitation_dialog", 4, "waiting", 4),
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
