//! Shell-independent native authority composition regressions.
//!
//! These exercise the production actor authority owner without shell inference,
//! backend probes or process creation. Descriptor publication is tested in the
//! security filesystem owner; assembled patch dispatch remains a later task.

use super::*;

/// Minimal running turn for direct authority inspection.
fn turn() -> AgentTurnRecord {
    AgentTurnRecord {
        turn_id: "native-authority".into(),
        conversation_id: "conversation-1".into(),
        agent_id: "agent-%1".into(),
        pane_id: "%1".into(),
        trigger: mez_agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 1,
        deadline_at_unix_millis: 0,
        policy_profile: "default".into(),
        model_profile: "default".into(),
        parent_turn_id: None,
        state: AgentTurnState::Running,
        cooperation_mode: None,
        initial_capability: None,
    }
}

/// Deepest rejected, revoked and pending decisions withhold implicit grants;
/// a trusted ancestor cannot restore authority through a denied nested project.
#[test]
fn native_filesystem_authority_preserves_deepest_trust_and_planning() {
    let root = temp_root("native-filesystem-trust");
    fs::create_dir_all(root.join("nested")).unwrap();
    let root = fs::canonicalize(root).unwrap();
    let nested = root.join("nested");
    let mut service = test_runtime_service();
    for decision in [
        TrustDecision::Rejected,
        TrustDecision::Revoked,
        TrustDecision::Pending,
        TrustDecision::Trusted,
    ] {
        let mut store = ProjectTrustStore::default();
        store
            .decide_at(root.clone(), TrustDecision::Trusted, None, 1)
            .unwrap();
        if decision == TrustDecision::Pending {
            store
                .decide_at(nested.clone(), TrustDecision::Trusted, None, 2)
                .unwrap();
            let database = root.join("pending-trust.sqlite");
            store.save_to_file(&database).unwrap();
            let connection = rusqlite::Connection::open(&database).unwrap();
            connection
                .execute(
                    "UPDATE project_trust SET state = 'pending' WHERE project_root = ?1",
                    [nested.to_str().unwrap()],
                )
                .unwrap();
            drop(connection);
            store = ProjectTrustStore::load_from_file(&database).unwrap();
        } else {
            store.decide_at(nested.clone(), decision, None, 2).unwrap();
        }
        service.set_project_trust_store(store, None);
        let scopes = service
            .native_filesystem_scopes_for_turn(&turn(), &nested)
            .unwrap();
        if decision == TrustDecision::Trusted {
            let scopes = scopes.unwrap();
            assert_eq!(scopes.write_scopes, vec![nested.to_str().unwrap()]);
            service.set_agent_planning_enabled("%1", true);
            let planning = service
                .native_filesystem_scopes_for_turn(&turn(), &nested)
                .unwrap()
                .unwrap();
            assert!(planning.write_scopes.is_empty());
            assert_eq!(planning.read_scopes, vec![nested.to_str().unwrap()]);
            service.set_agent_planning_enabled("%1", false);
        } else {
            assert!(scopes.is_none());
        }
    }
    fs::remove_dir_all(root).unwrap();
}

/// Explicit configured grants retain their existing precedence while child
/// scopes intersect them, including explicit empty/read-only declarations.
#[test]
fn native_filesystem_authority_intersects_child_scopes_without_shell_context() {
    let root = temp_root("native-filesystem-child-scopes");
    fs::create_dir_all(root.join("child")).unwrap();
    let root = fs::canonicalize(root).unwrap();
    let child = root.join("child");
    let mut service = test_runtime_service();
    let configured =
        crate::runtime::config::runtime_configured_permissions_from_config(&serde_json::json!({
            "permissions": { "read_scopes": [root], "write_scopes": [root] }
        }))
        .unwrap();
    service
        .integration
        .replace_configured_permissions(configured);
    service.set_subagent_scope_declaration(
        "agent-%1",
        mez_agent::SubagentScopeDeclaration {
            cooperation_mode: CooperationMode::ExploreOnly,
            approval_provenance: mez_agent::SubagentApprovalProvenance::Requested,
            current_directory: root.to_str().unwrap().into(),
            read_scopes: vec![child.to_str().unwrap().into()],
            write_scopes: Vec::new(),
            permission_preset: Some(mez_agent::PermissionPreset::ReadOnly),
        },
    );
    let scopes = service
        .native_filesystem_scopes_for_turn(&turn(), &root)
        .unwrap()
        .unwrap();
    assert_eq!(scopes.read_scopes, vec![child.to_str().unwrap()]);
    assert!(scopes.write_scopes.is_empty());
    service.set_subagent_scope_declaration(
        "agent-%1",
        mez_agent::SubagentScopeDeclaration {
            cooperation_mode: CooperationMode::ExploreOnly,
            approval_provenance: mez_agent::SubagentApprovalProvenance::Requested,
            current_directory: root.to_str().unwrap().into(),
            read_scopes: Vec::new(),
            write_scopes: Vec::new(),
            permission_preset: None,
        },
    );
    let scopes = service
        .native_filesystem_scopes_for_turn(&turn(), &root)
        .unwrap()
        .unwrap();
    assert!(scopes.read_scopes.is_empty());
    assert!(scopes.write_scopes.is_empty());
    fs::remove_dir_all(root).unwrap();
}

/// Active Seatbelt adds installed toolchain reads to native authority without
/// replacing trusted-project writes or escaping a narrowed child scope.
#[cfg(target_os = "macos")]
#[test]
fn native_seatbelt_toolchain_reads_preserve_trust_and_child_intersection() {
    let root = std::env::current_dir().unwrap().join(format!(
        "target/mez-native-toolchain-trust-{}",
        rand::random::<u64>()
    ));
    fs::create_dir_all(&root).unwrap();
    let root = fs::canonicalize(root).unwrap();
    let mut service = crate::test_support::runtime::RuntimeServiceFixture::new()
        .control_socket(root.join("control.sock"))
        .build();
    let configured = crate::runtime::config::runtime_configured_permissions_from_config(
        &serde_json::json!({"permissions": {"sandbox": "seatbelt"}}),
    )
    .unwrap();
    service
        .integration
        .replace_configured_permissions(configured);
    let mut trust = ProjectTrustStore::default();
    trust
        .decide_at(root.clone(), TrustDecision::Trusted, None, 1)
        .unwrap();
    service.set_project_trust_store(trust, None);

    let scopes = service
        .native_filesystem_scopes_for_turn(&turn(), &root)
        .unwrap()
        .unwrap();
    assert_eq!(scopes.write_scopes, vec![root.to_str().unwrap()]);
    for path in crate::security::sandbox::seatbelt::macos_toolchain_read_subpaths() {
        let traversable =
            crate::security::filesystem::resolve_host_path(&root, path).is_ok_and(|evidence| {
                evidence.kind == mez_agent::permissions::ResolvedPathKind::Existing
                    && evidence.object_kind
                        == mez_agent::permissions::ResolvedPathObjectKind::Directory
                    && evidence.canonical_path == path
            });
        assert_eq!(
            scopes.read_scopes.iter().any(|scope| scope == path),
            traversable,
            "optional native scope must match accessible physical root: {path}"
        );
        assert!(!scopes.write_scopes.iter().any(|scope| scope == path));
    }

    service.set_subagent_scope_declaration(
        "agent-%1",
        mez_agent::SubagentScopeDeclaration {
            cooperation_mode: CooperationMode::ExploreOnly,
            approval_provenance: mez_agent::SubagentApprovalProvenance::Requested,
            current_directory: root.to_str().unwrap().into(),
            read_scopes: vec![root.to_str().unwrap().into()],
            write_scopes: Vec::new(),
            permission_preset: Some(mez_agent::PermissionPreset::ReadOnly),
        },
    );
    let narrowed = service
        .native_filesystem_scopes_for_turn(&turn(), &root)
        .unwrap()
        .unwrap();
    assert_eq!(narrowed.read_scopes, vec![root.to_str().unwrap()]);
    assert!(narrowed.write_scopes.is_empty());
    fs::remove_dir_all(root).unwrap();
}
