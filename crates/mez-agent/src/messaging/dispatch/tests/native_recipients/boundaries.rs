//! Raw direct-ID boundary controls beside the discovery interoperability test.
//!
//! Opaque address syntax never creates an identity, widens audience, changes
//! canonical sender metadata, resets cursors, or allocates generated IDs.
//! These fixtures exercise raw dispatch rather than constructing acceptance
//! directly; trusted setup and snapshot adoption remain explicit local APIs.

use super::*;

/// Constructs one complete raw envelope under the actual generated binding.
fn raw_envelope(
    connection: &MessageConnection,
    id: &str,
    recipient: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "protocol": "mmp/1", "id": id, "type": "send", "time": "fixture:30",
        "sender": {"agent_id": connection.agent_id.as_ref().unwrap().as_str()},
        "recipient": recipient, "correlation_id": null, "ttl_ms": null,
        "content_type": "text/plain; charset=utf-8", "payload": "boundary evidence",
    })
}

/// Dispatches a complete JSON body without bypassing the transport parser.
fn dispatch_value(
    service: &mut MessageService,
    connection: &mut MessageConnection,
    body: &serde_json::Value,
) -> serde_json::Value {
    serde_json::from_str(&dispatch_mmp_body(
        &body.to_string(),
        service,
        connection,
        30,
    ))
    .unwrap()
}

/// Registers one additional exact opaque identity using the same trusted
/// project membership, not an alias or a change to another identity's metadata.
fn same_project_identity(
    service: &mut MessageService,
    native: &SenderIdentity,
    id: &str,
) -> SenderIdentity {
    let mut identity = native.clone();
    identity.agent_id = AgentId::opaque(id).unwrap();
    identity.pane_id = None;
    identity.window_id = None;
    identity.role = Some("address-fixture".into());
    let identity = service.ensure_agent_identity(identity, 10).unwrap();
    service
        .subscribe_from_retained_start(&identity.agent_id)
        .unwrap();
    identity
}

/// Generated, generic opaque and native registrations all use the exact IDs
/// returned by raw discovery. Native address parsing neither consumes generated
/// allocation high-water nor treats a native suffix as a pane selector.
#[test]
fn raw_direct_ids_preserve_generated_generic_and_native_addresses() {
    let (mut service, mut connection, native) = native_raw_fixture();
    let generated = service.register_agent(None, None, "generated-fixture", Vec::new());
    assert_eq!(generated.agent_id.as_str(), "a2");
    service
        .rebind_agent_project_scope(&generated.agent_id, native.project_scope.clone())
        .unwrap();
    service
        .subscribe_from_retained_start(&generated.agent_id)
        .unwrap();
    let generic = same_project_identity(&mut service, &native, "agent-alpha");
    let unicode = same_project_identity(&mut service, &native, "agent-Δ/exact");
    for (index, target) in [&native, &generated, &generic, &unicode]
        .into_iter()
        .enumerate()
    {
        let discovery = dispatch_value(
            &mut service,
            &mut connection,
            &serde_json::json!({
                "protocol": "mmp/1", "type": "discover", "agent_id": target.agent_id.as_str(),
            }),
        );
        assert_eq!(discovery["scope"], "project");
        let agents = discovery["agents"].as_array().unwrap();
        assert_eq!(agents.len(), 1);
        let discovered = agents[0]["agent_id"].as_str().unwrap();
        assert_eq!(discovered, target.agent_id.as_str());
        let body = raw_envelope(
            &connection,
            &format!("address-{index}"),
            serde_json::json!({"agent_id": discovered}),
        );
        let response = dispatch_value(&mut service, &mut connection, &body);
        assert_eq!(response["type"], "ack", "{response}");
        assert_eq!(response["status"], "accepted");
        assert_eq!(response["queued_recipients"], 1);
        let batch = service
            .receive_subscribed(&target.agent_id, 31, 10)
            .unwrap();
        assert_eq!(batch.messages.len(), 1);
        assert_eq!(
            batch.messages[0].envelope.recipient,
            Recipient::Agent(target.agent_id.clone())
        );
    }
    let next = service.register_agent(None, None, "allocation-control", Vec::new());
    assert_eq!(next.agent_id.as_str(), "a3");
}

/// Missing, retired, foreign-project, case/whitespace variants, role labels and
/// long opaque addresses share the public unavailable result and create no
/// registration, receipt, sequence or cursor. A sibling sharing native pane and
/// role metadata cannot receive an exact direct-agent message by resemblance.
#[test]
fn raw_opaque_lookup_is_exact_unavailable_and_side_effect_free() {
    let (mut service, mut connection, native) = native_raw_fixture();
    let retired = same_project_identity(&mut service, &native, "agent-retired");
    assert!(service.retire_agent_identity(&retired.agent_id));
    let mut foreign = native.clone();
    foreign.agent_id = AgentId::opaque("agent-foreign").unwrap();
    foreign.project_scope = Some(ProjectScopeId::from_canonical_root_bytes(
        b"/fixture/raw-native-beta",
    ));
    service.ensure_agent_identity(foreign, 10).unwrap();
    let mut sibling = native.clone();
    sibling.agent_id = AgentId::opaque("agent-sibling").unwrap();
    let sibling = service.ensure_agent_identity(sibling, 10).unwrap();
    service
        .subscribe_from_retained_start(&sibling.agent_id)
        .unwrap();
    let before = service.snapshot_state();
    let long = format!("agent-{}", "x".repeat(1024));
    let mut public = None;
    for (index, id) in [
        "agent-missing",
        "agent-retired",
        "agent-foreign",
        "Agent-%1",
        "agent-%1 ",
        " agent-%1",
        "agent-%01",
        "native-fixture",
        "a18446744073709551616",
        long.as_str(),
    ]
    .into_iter()
    .enumerate()
    {
        let body = raw_envelope(
            &connection,
            &format!("unavailable-{index}"),
            serde_json::json!({"agent_id": id}),
        );
        let response = dispatch_value(&mut service, &mut connection, &body);
        assert_eq!(response["type"], "error");
        assert_eq!(response["error"]["code"], "undeliverable");
        if let Some(expected) = &public {
            assert_eq!(&response, expected);
        } else {
            public = Some(response);
        }
        assert_eq!(service.snapshot_state(), before);
    }
    let body = raw_envelope(
        &connection,
        "exact-not-sibling",
        serde_json::json!({"agent_id": native.agent_id.as_str()}),
    );
    let response = dispatch_value(&mut service, &mut connection, &body);
    assert_eq!(response["queued_recipients"], 1);
    assert_eq!(
        service
            .receive_subscribed(&native.agent_id, 31, 10)
            .unwrap()
            .messages
            .len(),
        1
    );
    assert!(
        service
            .receive_subscribed(&sibling.agent_id, 31, 10)
            .unwrap()
            .messages
            .is_empty()
    );
    let mut changed = native.clone();
    changed.role = Some("forged-role".into());
    let snapshot = service.snapshot_state();
    assert!(service.ensure_agent_identity(changed, 32).is_err());
    let mut changed = native.clone();
    changed.capabilities.push("forged-capability".into());
    assert!(service.ensure_agent_identity(changed, 32).is_err());
    assert_eq!(service.snapshot_state(), snapshot);
}

/// Raw fields retain their existing type, nonblank, control-character and
/// independent-selector checks. Opaque agent support must not widen numeric
/// pane/window syntax or let a false session selector widen project audience.
#[test]
fn raw_native_ids_do_not_weaken_selector_validation() {
    let (mut service, mut connection, native) = native_raw_fixture();
    let before = service.snapshot_state();
    let invalid = vec![
        serde_json::json!(null),
        serde_json::json!(42),
        serde_json::json!("agent-%1"),
        serde_json::json!({}),
        serde_json::json!({"agent_id": null}),
        serde_json::json!({"agent_id": false}),
        serde_json::json!({"agent_id": []}),
        serde_json::json!({"agent_id": ""}),
        serde_json::json!({"agent_id": "  "}),
        serde_json::json!({"agent_id": "agent-\n%1"}),
        serde_json::json!({"agent_id": "agent-\u{0}%1"}),
        serde_json::json!({"agent_id": "agent-%1", "pane_id": "%1"}),
        serde_json::json!({"agent_id": "agent-%1", "role": "native-fixture"}),
        serde_json::json!({"agent_id": "agent-%1", "session": true}),
        serde_json::json!({"agent_id": "agent-%1", "session": "false"}),
        serde_json::json!({"pane_id": "agent-%1"}),
        serde_json::json!({"window_id": "opaque-window"}),
    ];
    for (index, recipient) in invalid.into_iter().enumerate() {
        let body = raw_envelope(&connection, &format!("invalid-{index}"), recipient);
        let response = dispatch_value(&mut service, &mut connection, &body);
        assert_eq!(response["type"], "error");
        assert_eq!(response["error"]["code"], "invalid_envelope", "{response}");
        assert_eq!(service.snapshot_state(), before);
    }
    let body = raw_envelope(
        &connection,
        "false-session",
        serde_json::json!({"agent_id": native.agent_id.as_str(), "session": false}),
    );
    let response = dispatch_value(&mut service, &mut connection, &body);
    assert_eq!(response["status"], "accepted");
    assert_eq!(response["queued_recipients"], 1);
}

/// Sender identity and project authority still come from the registered
/// connection. Forged canonical fields fail, an unknown sender field cannot
/// manufacture membership, and session widening is explicit and separate.
#[test]
fn raw_native_ids_preserve_sender_and_project_authority() {
    let (mut service, mut connection, native) = native_raw_fixture();
    let before = service.snapshot_state();
    for (field, value) in [
        ("agent_id", serde_json::json!(native.agent_id.as_str())),
        ("pane_id", serde_json::json!("%1")),
        ("role", serde_json::json!("native-fixture")),
        ("capabilities", serde_json::json!(["agent-harness"])),
    ] {
        let mut body = raw_envelope(
            &connection,
            &format!("forged-{field}"),
            serde_json::json!({"agent_id": native.agent_id.as_str()}),
        );
        body["sender"][field] = value;
        let response = dispatch_value(&mut service, &mut connection, &body);
        assert_eq!(response["type"], "error");
        assert_eq!(
            response["error"]["message"],
            "message sender does not match authenticated agent connection"
        );
        assert_eq!(service.snapshot_state(), before);
    }
    let body = raw_envelope(
        &connection,
        "no-hello",
        serde_json::json!({"agent_id": native.agent_id.as_str()}),
    );
    let mut unregistered = MessageConnection::default();
    assert_eq!(
        dispatch_value(&mut service, &mut unregistered, &body)["type"],
        "error"
    );
    assert_eq!(service.snapshot_state(), before);
    service
        .rebind_agent_project_scope(connection.agent_id.as_ref().unwrap(), None)
        .unwrap();
    let discovery = dispatch_value(
        &mut service,
        &mut connection,
        &serde_json::json!({"protocol": "mmp/1", "type": "discover", "role": "native-fixture"}),
    );
    assert!(discovery["agents"].as_array().unwrap().is_empty());
    let mut body = raw_envelope(
        &connection,
        "unbound-project",
        serde_json::json!({"agent_id": native.agent_id.as_str()}),
    );
    // Private routing digests are not transport identity fields. An arbitrary
    // caller claim must not grant membership, and this test must not expose a
    // trusted snapshot-only scope value in sender JSON to model that claim.
    body["sender"]["project_scope"] = serde_json::json!("untrusted-project-claim");
    let unbound = service.snapshot_state();
    assert_eq!(
        dispatch_value(&mut service, &mut connection, &body)["error"]["code"],
        "undeliverable"
    );
    assert_eq!(service.snapshot_state(), unbound);
    let discovery = dispatch_value(
        &mut service,
        &mut connection,
        &serde_json::json!({"protocol": "mmp/1", "type": "discover", "scope": "session", "agent_id": native.agent_id.as_str()}),
    );
    assert_eq!(discovery["agents"][0]["agent_id"], native.agent_id.as_str());
    body["id"] = serde_json::json!("explicit-session");
    body["scope"] = serde_json::json!("session");
    let accepted = dispatch_value(&mut service, &mut connection, &body);
    assert_eq!(accepted["status"], "accepted");
    assert_eq!(accepted["queued_recipients"], 1);
    let batch = service
        .receive_subscribed(&native.agent_id, 31, 10)
        .unwrap();
    assert_eq!(batch.messages.len(), 1);
    assert!(batch.messages[0].envelope.sender.project_scope.is_none());
}

/// Snapshot adoption retains opaque registrations, exact original mail and
/// receipt idempotence. It does not create reconnect authority: the test keeps
/// the same fixture-owned generated binding across local service reconstruction.
#[test]
fn raw_native_ids_preserve_restored_receipts_and_cursors() {
    let (mut service, mut connection, native) = native_raw_fixture();
    let body = raw_envelope(
        &connection,
        "native-before-restore",
        serde_json::json!({"agent_id": native.agent_id.as_str()}),
    );
    let accepted = dispatch_value(&mut service, &mut connection, &body);
    assert_eq!(accepted["status"], "accepted");
    let before = service.snapshot_state();
    let mut restored = MessageService::from_snapshot_state(&before).unwrap();
    assert_eq!(
        restored.registered_identity(&native.agent_id),
        Some(&native)
    );
    let discovery = dispatch_value(
        &mut restored,
        &mut connection,
        &serde_json::json!({"protocol": "mmp/1", "type": "discover", "agent_id": native.agent_id.as_str()}),
    );
    assert_eq!(discovery["agents"][0]["agent_id"], native.agent_id.as_str());
    assert_eq!(
        dispatch_value(&mut restored, &mut connection, &body),
        accepted
    );
    assert_eq!(
        restored.snapshot_state().next_sequence,
        before.next_sequence
    );
    let batch = restored
        .receive_subscribed(&native.agent_id, 31, 10)
        .unwrap();
    assert_eq!(batch.messages.len(), 1);
    assert_eq!(
        restored
            .subscription(&native.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
    restored
        .advance_subscription(&native.agent_id, batch.messages[0].sequence)
        .unwrap();
    let second = raw_envelope(
        &connection,
        "native-after-restore",
        serde_json::json!({"agent_id": native.agent_id.as_str()}),
    );
    assert_eq!(
        dispatch_value(&mut restored, &mut connection, &second)["status"],
        "accepted"
    );
    let remaining = restored
        .receive_subscribed(&native.agent_id, 32, 10)
        .unwrap();
    assert_eq!(remaining.messages.len(), 1);
    assert_eq!(remaining.messages[0].envelope.id, "native-after-restore");
}

/// Retained direct generated recipients remain reserved after retirement and
/// restore. Supporting opaque addresses must not make a fresh generated owner
/// inherit an old target's canonical mail, presence, or unavailable identity.
/// Use fresh observational retirement: reusable pane retirement intentionally
/// deletes addressed mail and therefore cannot establish this reservation case.
#[test]
fn raw_native_id_support_does_not_reuse_retired_generated_targets() {
    let (mut service, mut connection, native) = native_raw_fixture();
    let old = service.register_agent(None, None, "old-generated", Vec::new());
    service
        .rebind_agent_project_scope(&old.agent_id, native.project_scope.clone())
        .unwrap();
    service
        .subscribe_from_retained_start(&old.agent_id)
        .unwrap();
    let body = raw_envelope(
        &connection,
        "retired-generated-mail",
        serde_json::json!({"agent_id": old.agent_id.as_str()}),
    );
    assert_eq!(
        dispatch_value(&mut service, &mut connection, &body)["status"],
        "accepted"
    );
    let before = service.snapshot_state();
    assert!(
        service
            .retire_fresh_observational_identity(&old.agent_id)
            .unwrap()
    );
    let retired = service.snapshot_state();
    assert_eq!(retired.retained_messages, before.retained_messages);
    assert_eq!(retired.accepted_messages, before.accepted_messages);
    assert!(service.registered_identity(&old.agent_id).is_none());
    assert!(service.subscription(&old.agent_id).is_none());
    assert!(
        retired
            .retired_delivery_floors
            .iter()
            .all(|floor| floor.identity.agent_id != old.agent_id.as_str())
    );
    let mut restored = MessageService::from_snapshot_state(&retired).unwrap();
    let fresh = restored.register_agent(None, None, "fresh-generated", Vec::new());
    assert_ne!(fresh.agent_id, old.agent_id);
    restored
        .rebind_agent_project_scope(&fresh.agent_id, native.project_scope.clone())
        .unwrap();
    restored
        .subscribe_from_retained_start(&fresh.agent_id)
        .unwrap();
    assert!(
        restored
            .receive_subscribed(&fresh.agent_id, 31, 10)
            .unwrap()
            .messages
            .is_empty()
    );
    let unavailable = raw_envelope(
        &connection,
        "retired-lookup",
        serde_json::json!({"agent_id": old.agent_id.as_str()}),
    );
    assert_eq!(
        dispatch_value(&mut restored, &mut connection, &unavailable)["error"]["code"],
        "undeliverable"
    );
}

/// Existing payload and retention bounds apply identically to native direct
/// addresses. Rejection consumes no sequence or receipt, and retained original
/// envelopes keep bounded FIFO order without cursor acknowledgement by parsing.
#[test]
fn raw_native_ids_preserve_capacity_and_retention_bounds() {
    let (mut service, mut connection, native) =
        native_raw_fixture_with_service(MessageService::with_limits(2, 64));
    let mut body = raw_envelope(
        &connection,
        "too-large",
        serde_json::json!({"agent_id": native.agent_id.as_str()}),
    );
    body["payload"] = serde_json::json!("x".repeat(65));
    let before = service.snapshot_state();
    assert_eq!(
        dispatch_value(&mut service, &mut connection, &body)["error"]["code"],
        "payload_too_large"
    );
    assert_eq!(service.snapshot_state(), before);
    for index in 0..3 {
        let body = raw_envelope(
            &connection,
            &format!("bounded-{index}"),
            serde_json::json!({"agent_id": native.agent_id.as_str()}),
        );
        assert_eq!(
            dispatch_value(&mut service, &mut connection, &body)["queued_recipients"],
            1
        );
    }
    let retained = service.snapshot_state();
    assert_eq!(retained.retained_messages.len(), 2);
    let batch = service
        .receive_subscribed(&native.agent_id, 31, 10)
        .unwrap();
    assert!(
        batch
            .messages
            .iter()
            .map(|message| message.envelope.payload.len())
            .sum::<usize>()
            <= 64
    );
    assert_eq!(
        batch
            .messages
            .iter()
            .map(|message| message.envelope.id.as_str())
            .collect::<Vec<_>>(),
        ["bounded-1", "bounded-2"]
    );
    assert_eq!(
        service
            .subscription(&native.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
}
