//! Exact typed source owners versus socket-writer/observer authority.
//!
//! Parent facts are obtained from a genuine native helper, not a payload PID.
//! Replacing only a test-owned binding's source kind demonstrates why matching
//! UID/birth metadata cannot upgrade the original producer connection or private
//! handle. No new parent admission method or source policy is exercised here.

use super::helper_presentation::{child, enroll_producer, release};
use super::*;

/// A verified parent's fields equal the live socket producer's fields, yet its
/// type has no producer-socket authority or observer freshness. A retained native
/// parent survives helper exit; source death later invalidates it. Exact typed
/// owner clones preserve pointer identity, but matching different-kind facts do not.
#[tokio::test(flavor = "current_thread")]
async fn external_producer_verified_parent_facts_do_not_authorize_socket_or_observer() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let enrolled = enroll_producer(&mut fixture).await;
    let (mut socket, connection) = child(&mut fixture).await;
    let origin = connection.unix_origin().unwrap().clone();
    let parent = tokio::task::spawn_blocking(move || origin.capture_parent())
        .await
        .unwrap()
        .unwrap();
    let parent = ProducerEvidence::VerifiedParent(Arc::new(parent));
    let origin = connection.unix_origin().unwrap().clone();
    let other_parent = ProducerEvidence::VerifiedParent(Arc::new(
        tokio::task::spawn_blocking(move || origin.capture_parent())
            .await
            .unwrap()
            .unwrap(),
    ));
    assert_eq!(parent.identity(), other_parent.identity());
    assert!(
        !parent.same_owner(&other_parent),
        "same-kind native metadata is not an exact owner clone"
    );
    let socket_source = ProducerEvidence::Socket(fixture.connection.unix_origin().unwrap().clone());
    assert_eq!(parent.uid(), socket_source.uid());
    assert_eq!(parent.identity(), socket_source.identity());
    assert!(parent.is_live());
    assert_eq!(parent.reobserve().unwrap(), parent.identity());
    assert!(!parent.same_owner(&socket_source));
    assert!(parent.same_owner(&parent.clone()));
    assert!(socket_source.same_owner(&socket_source.clone()));
    assert!(!parent.matches_socket(fixture.connection.unix_origin().unwrap()));
    assert!(
        parent
            .authorize_socket(fixture.connection.unix_origin().unwrap())
            .is_err()
    );
    assert!(
        socket_source
            .authorize_socket(fixture.connection.unix_origin().unwrap())
            .is_ok()
    );
    release(&mut socket, &connection).await;
    assert!(parent.is_live(), "helper exit is not captured-parent death");

    let registry = fixture.service.control.external_agents_mut();
    let binding = registry.bindings.values_mut().next().unwrap();
    let enrollment = binding.enrollment.as_mut().unwrap();
    enrollment.producer = parent.clone();
    // The original retained ancestry remains valid, but its socket observer is
    // not a transport of the new source kind. No lease extension can be inferred.
    assert!(enrollment.provenance_is_live());
    assert!(
        enrollment
            .authorize_connection(&fixture.connection)
            .is_err()
    );
    assert!(!enrollment.has_live_observer());
    assert!(!enrollment.idle_renewable());
    assert!(enrollment.observers.is_empty());
    enrollment.observe_connection(fixture.connection.unix_origin().unwrap());
    assert!(enrollment.observers.is_empty());
    let credential = enrolled["result"]["launch_token"].as_str().unwrap();
    let body = serde_json::json!({"jsonrpc":"2.0","id":"source-kind","method":"agent/external/presentation","params":{"launch_token":credential,"generation":enrolled["result"]["generation"],"external_session_id":"session-a","sequence":1,"state":"running"}}).to_string();
    let input = crate::control::encode_control_body(&body);
    let (reply, consumed) = fixture
        .service
        .handle_control_input_for_connection(&input, 8192, &mut fixture.connection)
        .unwrap();
    assert_eq!(consumed, input.len());
    let (body, _) = crate::control::decode_control_frame(&reply, 8192).unwrap();
    let response: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        response.get("error").is_some(),
        "matching source metadata must not authorize a socket capability"
    );
    release(&mut fixture.socket, &fixture.connection).await;
    assert!(!parent.is_live());
    assert!(parent.reobserve().is_err());
}

/// Two socket evidence objects may name the exact same native process yet own
/// distinct retained origin objects. A source-owner swap after successful worker
/// observation cannot authorize helper settlement merely through metadata equality;
/// all other root/writer/native facts remain real and the prior state is unchanged.
#[tokio::test(flavor = "current_thread")]
async fn external_producer_same_kind_owner_swap_rejects_pending_helper() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let enrolled = enroll_producer(&mut fixture).await;
    let (_socket, connection) = child(&mut fixture).await;
    let request = crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"owner-swap","method":"agent/external/helper-observe","params":{"harness":"pi","generation":enrolled["result"]["generation"],"observer_witness":enrolled["result"]["observer_witness"],"external_session_id":"session-a","sequence":1,"state":"running"}}).to_string()).unwrap();
    let work = fixture
        .service
        .prepare_external_helper_presentation(&request, &connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(observed.is_ok());
    // Capture from the actual test-owned accepted producer socket, not a supplied
    // process ID. Re-obtaining evidence does not create the original exact owner.
    let replacement = ProducerEvidence::Socket(Arc::new(
        crate::runtime::capture_unix_origin(
            fixture.socket.as_raw_fd(),
            crate::runtime::current_effective_uid(),
        )
        .unwrap(),
    ));
    let binding = fixture
        .service
        .control
        .external_agents_mut()
        .bindings
        .values_mut()
        .next()
        .unwrap();
    let source = &mut binding.enrollment.as_mut().unwrap().producer;
    assert_eq!(source.identity(), replacement.identity());
    assert_eq!(source.uid(), replacement.uid());
    assert!(!source.same_owner(&replacement));
    *source = replacement;
    let response: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, &connection),
    )
    .unwrap();
    assert!(response.get("error").is_some());
    let registry = fixture.service.control.external_agents();
    assert!(registry.enrollments.pending.is_empty());
    assert_eq!(registry.enrollments.ancestry_budget.reserved(), 1);
    assert!(
        registry
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
