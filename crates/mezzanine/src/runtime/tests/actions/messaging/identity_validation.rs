//! Peer identity sanitation and atomic runtime registration reconciliation.

use super::*;

/// Verifies the injected peer block and its provider label bound and sanitize
/// peer-supplied sender fields, so an oversized or evil identity cannot shape
/// framing text or the block label.
#[test]
fn runtime_peer_message_context_bounds_evil_sender_identity() {
    let oversized = "x".repeat(mez_agent::AGENT_LIST_MAX_STRING_BYTES * 2);
    let evil_capability = format!(
        "caps {}",
        "y".repeat(mez_agent::AGENT_LIST_MAX_STRING_BYTES)
    );
    let envelope = Envelope {
        protocol: "mmp/1",
        id: oversized.clone(),
        message_type: "send".to_string(),
        time: "runtime:1".to_string(),
        sender: mez_agent::messaging::SenderIdentity {
            agent_id: AgentId::opaque(oversized.clone()).unwrap(),
            project_scope: None,
            pane_id: None,
            window_id: None,
            role: Some(oversized.clone()),
            capabilities: vec![evil_capability.clone(); mez_agent::AGENT_LIST_MAX_CAPABILITIES + 2],
            objective: Some(oversized.clone()),
        },
        recipient: mez_agent::messaging::Recipient::Session,
        correlation_id: Some(oversized.clone()),
        ttl_ms: None,
        content_type: "text/plain; charset=utf-8".to_string(),
        payload: "bounded payload".to_string(),
        extension_fields: Vec::new(),
    };
    let content = crate::runtime::control::runtime_peer_message_context_content(&envelope);
    assert!(
        !content.contains(&oversized),
        "peer-supplied identity text must be bounded at render time"
    );
    assert!(!content.contains(&evil_capability));
    let bounded_capability = mez_agent::agent_list_bounded_text(&evil_capability);
    assert_eq!(
        content.matches(&bounded_capability).count(),
        mez_agent::AGENT_LIST_MAX_CAPABILITIES,
        "the injected block must carry at most the documented capability bound"
    );
    assert!(
        content.len()
            <= mez_agent::AGENT_LIST_MAX_STRING_BYTES
                * (mez_agent::AGENT_LIST_MAX_CAPABILITIES + 8)
    );
    let label = crate::runtime::control::runtime_peer_message_block_label(7, &oversized);
    assert!(label.starts_with("peer message sequence 7 id "));
    assert!(!label.contains(&oversized));
    assert!(label.len() <= mez_agent::AGENT_LIST_MAX_STRING_BYTES + 32);
}

/// A sparse early runtime identity must reconcile to pane-backed authoritative
/// metadata, while a later conflicting refresh cannot partially mutate either
/// the registered identity or its matching presence projection.
#[test]
fn runtime_identity_reconciliation_fills_placeholder_and_rejects_conflicting_repeat() {
    let mut service = test_runtime_service();
    let _primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let agent_id = AgentId::opaque("agent-%1").unwrap();
    service
        .message_service_mut()
        .ensure_agent_identity(
            mez_agent::messaging::SenderIdentity {
                agent_id: agent_id.clone(),
                project_scope: None,
                pane_id: None,
                window_id: None,
                role: None,
                capabilities: Vec::new(),
                objective: None,
            },
            7,
        )
        .unwrap();
    let authoritative = service
        .ensure_runtime_message_identity(
            "agent-%1",
            PaneId::opaque("%1".to_string()),
            "agent",
            &["agent-harness"],
            99,
        )
        .unwrap();
    assert_eq!(
        authoritative.pane_id.as_ref().map(PaneId::as_str),
        Some("%1")
    );
    assert!(authoritative.window_id.is_some());
    assert_eq!(authoritative.role.as_deref(), Some("agent"));
    assert_eq!(authoritative.capabilities, vec!["agent-harness"]);
    let presence_before = service
        .message_service()
        .presence()
        .into_iter()
        .find(|presence| presence.identity.agent_id == agent_id)
        .unwrap();
    let mut conflicting = authoritative.clone();
    conflicting.role = Some("worker".to_string());
    assert!(
        service
            .message_service_mut()
            .ensure_agent_identity(conflicting, 100)
            .is_err()
    );
    assert_eq!(
        service.message_service().registered_identity(&agent_id),
        Some(&authoritative)
    );
    assert_eq!(
        service
            .message_service()
            .presence()
            .into_iter()
            .find(|presence| presence.identity.agent_id == agent_id),
        Some(presence_before)
    );
    service.terminate_all_pane_processes().unwrap();
}
