//! Runtime tests for remote pairing, proof resolution, and local administration.

use super::*;

use crate::control::AuthenticatedPeer;
use iroh::SecretKey;

/// Creates an initialized local-owner connection for remote administration.
fn local_owner_connection(service: &mut RuntimeSessionService) -> ControlConnectionState {
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut connection = ControlConnectionState::trusted_existing_client(primary);
    connection
        .bind_authenticated_peer(AuthenticatedPeer::unix_user(effective_uid()))
        .unwrap();
    let session_id = service.session().id.to_string();
    let endpoint_id = service
        .integration
        .ensure_remote_endpoint_identity(&session_id)
        .unwrap()
        .secret_key()
        .public();
    service.integration.set_remote_endpoint_addr(Some(
        iroh::EndpointAddr::new(endpoint_id).with_ip_addr("127.0.0.1:1".parse().unwrap()),
    ));
    connection
}

/// Sends one control request through the stateful runtime connection boundary.
fn request(
    service: &mut RuntimeSessionService,
    connection: &mut ControlConnectionState,
    body: &str,
) -> serde_json::Value {
    let input = encode_control_body(body);
    let (output, consumed) = service
        .handle_control_input_for_connection(&input, 1024 * 1024, connection)
        .unwrap();
    assert_eq!(consumed, input.len());
    let (body, frame_consumed) = decode_control_frame(&output, 1024 * 1024).unwrap();
    assert_eq!(frame_consumed, output.len());
    serde_json::from_str(&body).unwrap()
}

/// Verifies invitation creation requires a current concrete transport route
/// and does not persist trust state when Iroh is inactive or not yet dialable.
///
/// A client cannot use an endpoint-id-only invitation without address lookup,
/// so both absent and route-empty publication states must fail transactionally.
#[test]
fn runtime_remote_invite_rejects_undialable_addresses_without_persistence() {
    let root = temp_root("remote-undialable-invite-runtime");
    let mut service = test_runtime_service();
    service.set_config_root(root.clone());
    let mut local = local_owner_connection(&mut service);
    let session_id = service.session().id.to_string();
    let endpoint_id = service
        .integration
        .ensure_remote_endpoint_identity(&session_id)
        .unwrap()
        .secret_key()
        .public();
    let trust_path =
        crate::security::remote::RemoteTrustStore::under_config_root(&root, &session_id)
            .unwrap()
            .directory()
            .join("trust.json");

    service.integration.set_remote_endpoint_addr(None);
    let inactive = request(
        &mut service,
        &mut local,
        r#"{"jsonrpc":"2.0","id":"inactive","method":"remote/invite","params":{"role":"observer","expires_seconds":600,"idempotency_key":"inactive-invite"}}"#,
    );
    assert_eq!(
        inactive
            .pointer("/error/data/mezzanine_code")
            .and_then(serde_json::Value::as_str),
        Some("invalid_state")
    );
    assert!(!trust_path.exists());

    service
        .integration
        .set_remote_endpoint_addr(Some(iroh::EndpointAddr::new(endpoint_id)));
    let route_empty = request(
        &mut service,
        &mut local,
        r#"{"jsonrpc":"2.0","id":"route-empty","method":"remote/invite","params":{"role":"observer","expires_seconds":600,"idempotency_key":"route-empty-invite"}}"#,
    );
    assert_eq!(
        route_empty
            .pointer("/error/data/mezzanine_code")
            .and_then(serde_json::Value::as_str),
        Some("invalid_state")
    );
    assert!(!trust_path.exists());

    let _ = fs::remove_dir_all(root);
}

/// Verifies endpoint identity alone cannot initialize and an invitation becomes
/// a durable endpoint-bound device proof without leaking the token to storage.
#[test]
fn runtime_iroh_initialize_requires_pairing_then_accepts_device_proof() {
    let root = temp_root("remote-pairing-runtime");
    let mut service = test_runtime_service();
    service.set_config_root(root.clone());
    let mut local = local_owner_connection(&mut service);

    let invite = request(
        &mut service,
        &mut local,
        r#"{"jsonrpc":"2.0","id":"invite","method":"remote/invite","params":{"role":"observer","expires_seconds":600,"idempotency_key":"invite-1"}}"#,
    );
    let repeated_invite = request(
        &mut service,
        &mut local,
        r#"{"jsonrpc":"2.0","id":"invite","method":"remote/invite","params":{"role":"observer","expires_seconds":600,"idempotency_key":"invite-1"}}"#,
    );
    assert_eq!(invite, repeated_invite);
    let invitation = invite.get("result").unwrap();
    let token = invitation
        .get("token")
        .and_then(serde_json::Value::as_str)
        .unwrap();
    let client_endpoint_id = SecretKey::generate().public().to_string();

    let mut unpaired = ControlConnectionState::new(true, true);
    unpaired
        .bind_authenticated_peer(AuthenticatedPeer::iroh_endpoint(&client_endpoint_id))
        .unwrap();
    let denied = request(
        &mut service,
        &mut unpaired,
        r#"{"jsonrpc":"2.0","id":"unpaired","method":"control/initialize","params":{"requested_role":"observer","requested_version":1,"client_name":"remote-observer","client":{"name":"remote-observer","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}}}}"#,
    );
    assert_eq!(
        denied
            .pointer("/error/data/mezzanine_code")
            .and_then(serde_json::Value::as_str),
        Some("forbidden")
    );
    assert!(!unpaired.initialized());

    let mut paired = ControlConnectionState::new(true, true);
    paired
        .bind_authenticated_peer(AuthenticatedPeer::iroh_endpoint(&client_endpoint_id))
        .unwrap();
    let initialize = format!(
        r#"{{"jsonrpc":"2.0","id":"pair","method":"control/initialize","params":{{"requested_role":"observer","requested_version":1,"client_name":"remote-observer","client":{{"name":"remote-observer","interactive":true,"terminal":{{"columns":80,"rows":24,"term":"xterm-256color"}}}},"authentication":{{"mechanism":"extension:iroh_invitation","token":{}}}}}}}"#,
        serde_json::to_string(token).unwrap()
    );
    let paired_response = request(&mut service, &mut paired, &initialize);
    let device_credential = paired_response
        .pointer("/result/device_credential")
        .and_then(serde_json::Value::as_str)
        .unwrap()
        .to_string();
    assert_eq!(
        paired_response
            .pointer("/result/granted_role")
            .and_then(serde_json::Value::as_str),
        Some("pending_observer")
    );
    assert!(paired.remote_principal().is_some());
    let trust_text = fs::read_to_string(
        crate::security::remote::RemoteTrustStore::under_config_root(
            &root,
            service.session().id.as_str(),
        )
        .unwrap()
        .directory()
        .join("trust.json"),
    )
    .unwrap();
    assert!(!trust_text.contains(token));
    assert!(!trust_text.contains(&device_credential));

    let mut reconnect = ControlConnectionState::new(true, true);
    reconnect
        .bind_authenticated_peer(AuthenticatedPeer::iroh_endpoint(&client_endpoint_id))
        .unwrap();
    let reconnect_initialize = format!(
        r#"{{"jsonrpc":"2.0","id":"reconnect","method":"control/initialize","params":{{"requested_role":"observer","requested_version":1,"client_name":"remote-observer","client":{{"name":"remote-observer","interactive":true,"terminal":{{"columns":80,"rows":24,"term":"xterm-256color"}}}},"authentication":{{"mechanism":"extension:iroh_device","token":{}}}}}}}"#,
        serde_json::to_string(&device_credential).unwrap()
    );
    let reconnect_response = request(&mut service, &mut reconnect, &reconnect_initialize);
    assert_eq!(
        reconnect_response
            .pointer("/result/granted_role")
            .and_then(serde_json::Value::as_str),
        Some("pending_observer")
    );
    assert!(reconnect.remote_principal().is_some());

    let _ = fs::remove_dir_all(root);
}

/// Verifies even a paired Iroh primary cannot invoke trust administration,
/// which remains owned by the local Unix recovery path.
#[test]
fn runtime_remote_administration_rejects_iroh_transport() {
    let root = temp_root("remote-local-only-runtime");
    let mut service = test_runtime_service();
    service.set_config_root(root.clone());
    let mut local = local_owner_connection(&mut service);
    let invite = request(
        &mut service,
        &mut local,
        r#"{"jsonrpc":"2.0","id":"invite","method":"remote/invite","params":{"role":"primary","expires_seconds":600,"idempotency_key":"invite-primary"}}"#,
    );
    let token = invite
        .pointer("/result/token")
        .and_then(serde_json::Value::as_str)
        .unwrap();
    let primary = service.session().primary_client_id().cloned().unwrap();
    service
        .detach_primary(&primary, Size::new(80, 24).unwrap())
        .unwrap();

    let endpoint_id = SecretKey::generate().public().to_string();
    let mut remote = ControlConnectionState::new(true, true);
    remote
        .bind_authenticated_peer(AuthenticatedPeer::iroh_endpoint(endpoint_id))
        .unwrap();
    let initialize = format!(
        r#"{{"jsonrpc":"2.0","id":"pair","method":"control/initialize","params":{{"requested_role":"primary","requested_version":1,"client_name":"remote-primary","client":{{"name":"remote-primary","interactive":true,"terminal":{{"columns":80,"rows":24,"term":"xterm-256color"}}}},"authentication":{{"mechanism":"extension:iroh_invitation","token":{}}}}}}}"#,
        serde_json::to_string(token).unwrap()
    );
    let initialized = request(&mut service, &mut remote, &initialize);
    assert_eq!(
        initialized
            .pointer("/result/granted_role")
            .and_then(serde_json::Value::as_str),
        Some("primary")
    );

    let denied = request(
        &mut service,
        &mut remote,
        r#"{"jsonrpc":"2.0","id":"clients","method":"remote/client/list","params":{}}"#,
    );
    assert_eq!(
        denied
            .pointer("/error/data/mezzanine_code")
            .and_then(serde_json::Value::as_str),
        Some("forbidden")
    );

    let _ = fs::remove_dir_all(root);
}

/// Verifies generic initialization failure leaves invitation and live authority untouched.
#[test]
fn runtime_failed_remote_initialize_does_not_consume_invitation() {
    let root = temp_root("remote-failed-initialize-runtime");
    let mut service = test_runtime_service();
    service.set_config_root(root.clone());
    let mut local = local_owner_connection(&mut service);
    let invite = request(
        &mut service,
        &mut local,
        r#"{"jsonrpc":"2.0","id":"invite","method":"remote/invite","params":{"role":"observer","expires_seconds":600,"idempotency_key":"failed-init-invite"}}"#,
    );
    let token = invite
        .pointer("/result/token")
        .and_then(serde_json::Value::as_str)
        .unwrap();
    let endpoint_id = SecretKey::generate().public().to_string();
    let observers_before = service.session().observers().len();
    let clients_before = service.session().clients().len();

    let mut failed = ControlConnectionState::new(true, true);
    failed
        .bind_authenticated_peer(AuthenticatedPeer::iroh_endpoint(&endpoint_id))
        .unwrap();
    let invalid_initialize = format!(
        r#"{{"jsonrpc":"2.0","id":"bad-version","method":"control/initialize","params":{{"requested_role":"observer","requested_version":999,"client_name":"remote-observer","client":{{"name":"remote-observer","interactive":true,"terminal":{{"columns":80,"rows":24,"term":"xterm-256color"}}}},"authentication":{{"mechanism":"extension:iroh_invitation","token":{}}}}}}}"#,
        serde_json::to_string(token).unwrap()
    );
    let denied = request(&mut service, &mut failed, &invalid_initialize);
    assert_eq!(
        denied
            .pointer("/error/data/mezzanine_code")
            .and_then(serde_json::Value::as_str),
        Some("invalid_params")
    );
    assert!(!failed.initialized());
    assert!(failed.remote_principal().is_none());
    assert_eq!(service.session().observers().len(), observers_before);
    assert_eq!(service.session().clients().len(), clients_before);
    let store = crate::security::remote::RemoteTrustStore::under_config_root(
        &root,
        service.session().id.as_str(),
    )
    .unwrap();
    assert!(store.list_records().unwrap().is_empty());

    let mut retry = ControlConnectionState::new(true, true);
    retry
        .bind_authenticated_peer(AuthenticatedPeer::iroh_endpoint(endpoint_id))
        .unwrap();
    let valid_initialize = format!(
        r#"{{"jsonrpc":"2.0","id":"retry","method":"control/initialize","params":{{"requested_role":"observer","requested_version":1,"client_name":"remote-observer","client":{{"name":"remote-observer","interactive":true,"terminal":{{"columns":80,"rows":24,"term":"xterm-256color"}}}},"authentication":{{"mechanism":"extension:iroh_invitation","token":{}}}}}}}"#,
        serde_json::to_string(token).unwrap()
    );
    let accepted = request(&mut service, &mut retry, &valid_initialize);
    assert_eq!(
        accepted
            .pointer("/result/granted_role")
            .and_then(serde_json::Value::as_str),
        Some("pending_observer")
    );
    assert!(accepted.pointer("/result/device_credential").is_some());
    assert!(retry.remote_principal().is_some());
    assert_eq!(store.list_records().unwrap().len(), 1);

    let _ = fs::remove_dir_all(root);
}

/// Verifies invitation audit records retain safe identifiers but never bearer material.
#[test]
fn runtime_remote_invitation_audit_excludes_bearer_token() {
    let root = temp_root("remote-invitation-audit-runtime");
    let audit_path = root.join("audit.jsonl");
    let mut service = test_runtime_service();
    service.set_config_root(root.clone());
    service.set_audit_log(AuditLog::new(crate::security::audit::AuditConfig {
        enabled: true,
        path: audit_path.clone(),
        hash_chain: false,
        required: true,
    }));
    let mut local = local_owner_connection(&mut service);

    let invite = request(
        &mut service,
        &mut local,
        r#"{"jsonrpc":"2.0","id":"invite","method":"remote/invite","params":{"role":"observer","expires_seconds":600,"idempotency_key":"audit-invite"}}"#,
    );
    let invitation_id = invite
        .pointer("/result/invitation_id")
        .and_then(serde_json::Value::as_str)
        .unwrap();
    let token = invite
        .pointer("/result/token")
        .and_then(serde_json::Value::as_str)
        .unwrap();
    let audit = fs::read_to_string(&audit_path).unwrap();

    assert!(audit.contains(r#""event_type":"remote_trust""#), "{audit}");
    assert!(audit.contains(r#""action":"invite_created""#), "{audit}");
    assert!(audit.contains(invitation_id), "{audit}");
    assert!(!audit.contains(token), "{audit}");
    assert!(!audit.contains("device_credential"), "{audit}");
    assert!(!audit.contains("credential_verifier"), "{audit}");

    let _ = fs::remove_dir_all(root);
}

/// Verifies rejected and successful pairing audits retain no bearer credentials.
#[test]
fn runtime_remote_pairing_audit_records_rejection_and_redemption() {
    let root = temp_root("remote-pairing-audit-runtime");
    let audit_path = root.join("audit.jsonl");
    let mut service = test_runtime_service();
    service.set_config_root(root.clone());
    service.set_audit_log(AuditLog::new(crate::security::audit::AuditConfig {
        enabled: true,
        path: audit_path.clone(),
        hash_chain: false,
        required: true,
    }));
    let mut local = local_owner_connection(&mut service);
    let invite = request(
        &mut service,
        &mut local,
        r#"{"jsonrpc":"2.0","id":"invite","method":"remote/invite","params":{"role":"observer","expires_seconds":600,"idempotency_key":"pairing-audit-invite"}}"#,
    );
    let token = invite
        .pointer("/result/token")
        .and_then(serde_json::Value::as_str)
        .unwrap();
    let endpoint_id = SecretKey::generate().public().to_string();

    let mut rejected = ControlConnectionState::new(true, true);
    rejected
        .bind_authenticated_peer(AuthenticatedPeer::iroh_endpoint(&endpoint_id))
        .unwrap();
    let invalid_initialize = format!(
        r#"{{"jsonrpc":"2.0","id":"bad-version","method":"control/initialize","params":{{"requested_role":"observer","requested_version":999,"client_name":"remote-observer","client":{{"name":"remote-observer","interactive":true,"terminal":{{"columns":80,"rows":24,"term":"xterm-256color"}}}},"authentication":{{"mechanism":"extension:iroh_invitation","token":{}}}}}}}"#,
        serde_json::to_string(token).unwrap()
    );
    let denied = request(&mut service, &mut rejected, &invalid_initialize);
    assert!(denied.get("error").is_some(), "{denied}");

    let mut paired = ControlConnectionState::new(true, true);
    paired
        .bind_authenticated_peer(AuthenticatedPeer::iroh_endpoint(endpoint_id))
        .unwrap();
    let valid_initialize = format!(
        r#"{{"jsonrpc":"2.0","id":"pair","method":"control/initialize","params":{{"requested_role":"observer","requested_version":1,"client_name":"remote-observer","client":{{"name":"remote-observer","interactive":true,"terminal":{{"columns":80,"rows":24,"term":"xterm-256color"}}}},"authentication":{{"mechanism":"extension:iroh_invitation","token":{}}}}}}}"#,
        serde_json::to_string(token).unwrap()
    );
    let accepted = request(&mut service, &mut paired, &valid_initialize);
    let device_credential = accepted
        .pointer("/result/device_credential")
        .and_then(serde_json::Value::as_str)
        .unwrap();
    let audit = fs::read_to_string(&audit_path).unwrap();

    assert!(audit.contains(r#""action":"pairing_rejected""#), "{audit}");
    assert!(audit.contains(r#""reason":"initialize_failed""#), "{audit}");
    assert!(
        audit.contains(r#""action":"invitation_redeemed""#),
        "{audit}"
    );
    assert!(audit.contains(r#""mechanism":"invitation""#), "{audit}");
    assert!(audit.contains(r#""role":"observer""#), "{audit}");
    assert!(!audit.contains(token), "{audit}");
    assert!(!audit.contains(device_credential), "{audit}");
    assert!(!audit.contains("credential_verifier"), "{audit}");

    let _ = fs::remove_dir_all(root);
}

/// Verifies durable remote proof stays endpoint-bound and role-limited, while
/// local Unix administration remains available after revocation.
#[test]
fn runtime_remote_device_proof_rejects_escalation_revocation_and_unsupported_roles() {
    let root = temp_root("remote-device-adversarial-runtime");
    let mut service = test_runtime_service();
    service.set_config_root(root.clone());
    let mut local = local_owner_connection(&mut service);
    let invite = request(
        &mut service,
        &mut local,
        r#"{"jsonrpc":"2.0","id":"invite","method":"remote/invite","params":{"role":"observer","expires_seconds":600,"idempotency_key":"adversarial-invite"}}"#,
    );
    let token = invite
        .pointer("/result/token")
        .and_then(serde_json::Value::as_str)
        .unwrap();
    let endpoint_id = SecretKey::generate().public().to_string();
    let mut paired = ControlConnectionState::new(true, true);
    paired
        .bind_authenticated_peer(AuthenticatedPeer::iroh_endpoint(&endpoint_id))
        .unwrap();
    let initialize = format!(
        r#"{{"jsonrpc":"2.0","id":"pair","method":"control/initialize","params":{{"requested_role":"observer","requested_version":1,"client_name":"remote-observer","client":{{"name":"remote-observer","interactive":true,"terminal":{{"columns":80,"rows":24,"term":"xterm-256color"}}}},"authentication":{{"mechanism":"extension:iroh_invitation","token":{}}}}}}}"#,
        serde_json::to_string(token).unwrap()
    );
    let accepted = request(&mut service, &mut paired, &initialize);
    let credential = accepted
        .pointer("/result/device_credential")
        .and_then(serde_json::Value::as_str)
        .unwrap()
        .to_string();

    for (case, proof_endpoint, role, proof) in [
        (
            "bad-proof",
            endpoint_id.clone(),
            "observer",
            "not-the-device-proof".to_string(),
        ),
        (
            "wrong-endpoint",
            SecretKey::generate().public().to_string(),
            "observer",
            credential.clone(),
        ),
        (
            "role-escalation",
            endpoint_id.clone(),
            "primary",
            credential.clone(),
        ),
        (
            "agent-role",
            endpoint_id.clone(),
            "agent",
            credential.clone(),
        ),
        (
            "automation-role",
            endpoint_id.clone(),
            "automation",
            credential.clone(),
        ),
    ] {
        let mut connection = ControlConnectionState::new(true, true);
        connection
            .bind_authenticated_peer(AuthenticatedPeer::iroh_endpoint(proof_endpoint))
            .unwrap();
        let initialize = format!(
            r#"{{"jsonrpc":"2.0","id":{},"method":"control/initialize","params":{{"requested_role":{},"requested_version":1,"client_name":"remote-adversarial","client":{{"name":"remote-adversarial","interactive":true,"terminal":{{"columns":80,"rows":24,"term":"xterm-256color"}}}},"authentication":{{"mechanism":"extension:iroh_device","token":{}}}}}}}"#,
            serde_json::to_string(case).unwrap(),
            serde_json::to_string(role).unwrap(),
            serde_json::to_string(&proof).unwrap(),
        );
        let denied = request(&mut service, &mut connection, &initialize);
        assert_eq!(
            denied
                .pointer("/error/data/mezzanine_code")
                .and_then(serde_json::Value::as_str),
            Some("forbidden"),
            "{case}: {denied}"
        );
        assert!(!connection.initialized(), "{case}");
        assert!(connection.remote_principal().is_none(), "{case}");
    }

    let clients = request(
        &mut service,
        &mut local,
        r#"{"jsonrpc":"2.0","id":"clients","method":"remote/client/list","params":{}}"#,
    );
    let client_id = clients
        .pointer("/result/clients/0/id")
        .and_then(serde_json::Value::as_str)
        .unwrap();
    let revoke = format!(
        r#"{{"jsonrpc":"2.0","id":"revoke","method":"remote/client/revoke","params":{{"client_id":{},"reason":"device retired","idempotency_key":"revoke-adversarial"}}}}"#,
        serde_json::to_string(client_id).unwrap()
    );
    let revoked = request(&mut service, &mut local, &revoke);
    assert_eq!(
        revoked
            .pointer("/result/revocation_reason")
            .and_then(serde_json::Value::as_str),
        Some("device retired")
    );

    let mut after_revoke = ControlConnectionState::new(true, true);
    after_revoke
        .bind_authenticated_peer(AuthenticatedPeer::iroh_endpoint(endpoint_id))
        .unwrap();
    let reconnect = format!(
        r#"{{"jsonrpc":"2.0","id":"revoked","method":"control/initialize","params":{{"requested_role":"observer","requested_version":1,"client_name":"remote-observer","client":{{"name":"remote-observer","interactive":true,"terminal":{{"columns":80,"rows":24,"term":"xterm-256color"}}}},"authentication":{{"mechanism":"extension:iroh_device","token":{}}}}}}}"#,
        serde_json::to_string(&credential).unwrap()
    );
    let denied = request(&mut service, &mut after_revoke, &reconnect);
    assert_eq!(
        denied
            .pointer("/error/data/mezzanine_code")
            .and_then(serde_json::Value::as_str),
        Some("forbidden")
    );
    assert!(!after_revoke.initialized());

    let status = request(
        &mut service,
        &mut local,
        r#"{"jsonrpc":"2.0","id":"status","method":"remote/status","params":{}}"#,
    );
    let result = status.get("result").unwrap();
    assert_eq!(
        result.get("listener_active"),
        Some(&serde_json::json!(false))
    );
    assert_eq!(
        result.get("active_remote_connections"),
        Some(&serde_json::json!(0))
    );
    assert_eq!(
        result.get("connections_accepted"),
        Some(&serde_json::json!(0))
    );
    assert_eq!(
        result.get("connections_rejected"),
        Some(&serde_json::json!(0))
    );
    assert_eq!(result.get("setup_successes"), Some(&serde_json::json!(0)));
    assert_eq!(result.get("setup_failures"), Some(&serde_json::json!(0)));
    assert_eq!(result.get("shutdown_aborts"), Some(&serde_json::json!(0)));
    assert_eq!(
        result.get("last_connection_path"),
        Some(&serde_json::json!("unknown"))
    );
    let serialized = status.to_string();
    for forbidden in [
        "device_credential",
        "invitation_token",
        "private_key",
        "peer_address",
    ] {
        assert!(!serialized.contains(forbidden), "{serialized}");
    }

    let _ = fs::remove_dir_all(root);
}
