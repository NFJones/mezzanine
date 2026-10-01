//! Trusted membership and atomic sparse-identity reconciliation coverage.
//!
//! Registration and presence must change together only after all immutable
//! fields agree. Conflicts must not publish partially filled metadata.

use super::*;

/// Project membership derives deterministically from canonical bytes and is
/// immutable once two trusted registrations disagree on the same agent.
#[test]
fn project_scope_is_stable_distinct_and_immutable_per_agent() {
    let first = ProjectScopeId::from_canonical_root_bytes(b"/workspace/alpha");
    assert_eq!(
        first,
        ProjectScopeId::from_canonical_root_bytes(b"/workspace/alpha")
    );
    assert_ne!(
        first,
        ProjectScopeId::from_canonical_root_bytes(b"/workspace/beta")
    );
    let mut service = MessageService::default();
    let identity = SenderIdentity {
        agent_id: AgentId::opaque("agent-%1").unwrap(),
        project_scope: Some(first),
        pane_id: None,
        window_id: None,
        role: Some("agent".to_string()),
        capabilities: Vec::new(),
        objective: None,
    };
    service.ensure_agent_identity(identity.clone(), 0).unwrap();
    let mut mismatched = identity;
    mismatched.project_scope = Some(ProjectScopeId::from_canonical_root_bytes(
        b"/workspace/beta",
    ));
    assert_eq!(
        service
            .ensure_agent_identity(mismatched, 1)
            .unwrap_err()
            .message(),
        "MMP sender project scope cannot change after registration"
    );
    let unscoped = SenderIdentity {
        agent_id: AgentId::opaque("agent-%2").unwrap(),
        project_scope: None,
        pane_id: None,
        window_id: None,
        role: Some("agent".to_string()),
        capabilities: Vec::new(),
        objective: None,
    };
    service.ensure_agent_identity(unscoped.clone(), 0).unwrap();
    let mut upgraded = unscoped;
    let trusted_scope = ProjectScopeId::from_canonical_root_bytes(b"/workspace/trusted");
    upgraded.project_scope = Some(trusted_scope.clone());
    assert_eq!(
        service
            .ensure_agent_identity(upgraded, 1)
            .unwrap()
            .project_scope,
        Some(trusted_scope)
    );
}

/// A sparse placeholder identity accepts trusted runtime metadata once,
/// mirrors that complete identity into presence, and preserves its
/// objective and publication timestamp.
#[test]
fn ensure_agent_identity_fills_sparse_identity_and_presence_atomically() {
    let mut service = MessageService::default();
    let agent_id = AgentId::opaque("agent-%1").unwrap();
    service
        .ensure_agent_identity(
            SenderIdentity {
                agent_id: agent_id.clone(),
                project_scope: None,
                pane_id: None,
                window_id: None,
                role: None,
                capabilities: Vec::new(),
                objective: Some("Keep objective".to_string()),
            },
            42,
        )
        .unwrap();
    let complete = SenderIdentity {
        agent_id: agent_id.clone(),
        project_scope: Some(ProjectScopeId::from_canonical_root_bytes(
            b"/workspace/alpha",
        )),
        pane_id: Some(PaneId::parse('%', "%1").unwrap()),
        window_id: Some(WindowId::parse('@', "@1").unwrap()),
        role: Some("agent".to_string()),
        capabilities: vec!["agent-harness".to_string()],
        objective: None,
    };
    let reconciled = service.ensure_agent_identity(complete, 99).unwrap();
    assert_eq!(reconciled.pane_id.as_ref().map(PaneId::as_str), Some("%1"));
    assert_eq!(
        reconciled.window_id.as_ref().map(WindowId::as_str),
        Some("@1")
    );
    assert_eq!(reconciled.role.as_deref(), Some("agent"));
    assert_eq!(reconciled.capabilities, vec!["agent-harness"]);
    assert_eq!(reconciled.objective.as_deref(), Some("Keep objective"));
    assert_eq!(service.presence()[0].identity, reconciled);
    assert_eq!(service.presence()[0].updated_at_ms, 42);
}

/// Every populated immutable identity field conflicts rather than replacing
/// registered metadata, and a mixed fill plus scope conflict leaves both
/// identity and presence exactly unchanged.
#[test]
fn ensure_agent_identity_rejects_conflicts_without_partial_mutation() {
    let mut service = MessageService::default();
    let agent_id = AgentId::opaque("agent-%2").unwrap();
    let original = SenderIdentity {
        agent_id: agent_id.clone(),
        project_scope: Some(ProjectScopeId::from_canonical_root_bytes(
            b"/workspace/alpha",
        )),
        pane_id: Some(PaneId::parse('%', "%2").unwrap()),
        window_id: Some(WindowId::parse('@', "@2").unwrap()),
        role: Some("agent".to_string()),
        capabilities: vec!["agent-harness".to_string()],
        objective: Some("Preserve objective".to_string()),
    };
    service.ensure_agent_identity(original.clone(), 42).unwrap();
    for conflicting in [
        SenderIdentity {
            pane_id: Some(PaneId::parse('%', "%3").unwrap()),
            ..original.clone()
        },
        SenderIdentity {
            window_id: Some(WindowId::parse('@', "@3").unwrap()),
            ..original.clone()
        },
        SenderIdentity {
            role: Some("worker".to_string()),
            ..original.clone()
        },
        SenderIdentity {
            capabilities: vec!["worker".to_string()],
            ..original.clone()
        },
    ] {
        assert!(service.ensure_agent_identity(conflicting, 99).is_err());
        assert_eq!(service.registered_identity(&agent_id), Some(&original));
        assert_eq!(service.presence()[0].identity, original);
        assert_eq!(service.presence()[0].updated_at_ms, 42);
    }
    let sparse_id = AgentId::opaque("agent-%3").unwrap();
    let sparse = SenderIdentity {
        agent_id: sparse_id.clone(),
        project_scope: Some(ProjectScopeId::from_canonical_root_bytes(
            b"/workspace/alpha",
        )),
        pane_id: None,
        window_id: None,
        role: None,
        capabilities: Vec::new(),
        objective: None,
    };
    service.ensure_agent_identity(sparse.clone(), 7).unwrap();
    let mixed_conflict = SenderIdentity {
        agent_id: sparse_id.clone(),
        project_scope: Some(ProjectScopeId::from_canonical_root_bytes(
            b"/workspace/beta",
        )),
        pane_id: Some(PaneId::parse('%', "%3").unwrap()),
        window_id: Some(WindowId::parse('@', "@3").unwrap()),
        role: Some("agent".to_string()),
        capabilities: vec!["agent-harness".to_string()],
        objective: None,
    };
    assert!(service.ensure_agent_identity(mixed_conflict, 99).is_err());
    assert_eq!(service.registered_identity(&sparse_id), Some(&sparse));
    assert_eq!(
        service
            .presence()
            .into_iter()
            .find(|presence| presence.identity.agent_id == sparse_id)
            .map(|presence| presence.identity),
        Some(sparse)
    );
}
