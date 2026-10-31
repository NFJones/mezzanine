//! Shell-independent native authority composition regressions.
//!
//! These exercise the production actor authority owner without shell inference,
//! backend probes or process creation. Descriptor publication is tested in the
//! security filesystem owner; assembled patch dispatch remains a later task.

use super::*;
use crate::MezErrorKind;

/// Actual pane-root fixture with separate owned native navigation available.
struct FilesystemContextFixture {
    /// Actor-owned session with a real root process; no patch worker launched.
    service: RuntimeSessionService,
    /// Unique canonical directory containing process and operation fixtures.
    root: PathBuf,
}

impl FilesystemContextFixture {
    /// Starts the ordinary root fixture once; source capture itself must use
    /// process metadata rather than shell/environment discovery or PTY input.
    fn new() -> Self {
        let root = temp_root("native-filesystem-context");
        fs::create_dir_all(root.join("owned")).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let mut service = test_runtime_service();
        service
            .start_initial_pane_process_with_launch_context(Some("cat"), Some(&root), None)
            .unwrap();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        service.set_agent_native_shell_mode_for_tests("%1");
        let permissions = crate::runtime::config::runtime_configured_permissions_from_config(
            &serde_json::json!({"permissions":{"read_scopes":[root],"write_scopes":[root]}}),
        )
        .unwrap();
        service
            .integration
            .replace_configured_permissions(permissions);
        Self { service, root }
    }
}

impl Drop for FilesystemContextFixture {
    /// Stops the exact fixture process before removing only its owned directory.
    fn drop(&mut self) {
        let _ = self.service.terminate_all_pane_processes();
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Owned `/cd` intent selects the filesystem operation directory while the
/// root process retains its independent cwd. Source-bound grants use that
/// operation directory without shell/environment inference or daemon chdir.
#[test]
fn native_filesystem_source_preserves_owned_and_process_directory_separation() {
    let mut fixture = FilesystemContextFixture::new();
    let owned = fixture.root.join("owned");
    fixture
        .service
        .agent_shell_store_mut()
        .ensure_session("%1")
        .unwrap()
        .record_native_working_directory(owned.clone(), 0)
        .unwrap();
    let source = fixture
        .service
        .capture_native_filesystem_context(&turn())
        .unwrap();
    assert!(
        fixture
            .service
            .native_filesystem_context_is_current(&turn(), &source)
            .unwrap()
    );
    let scopes = fixture
        .service
        .native_filesystem_scopes_for_context(&turn(), &source)
        .unwrap()
        .unwrap();
    assert_eq!(PathBuf::from(&scopes.current_directory), owned);
    #[cfg(target_os = "linux")]
    {
        let root = source.capture_root().unwrap();
        assert_eq!(root.cwd(), owned);
        assert_eq!(root.process_cwd(), fixture.root);
        root.revalidate().unwrap();
    }
    #[cfg(target_os = "macos")]
    assert!(
        source.capture_root().is_err(),
        "foreign process group evidence must remain fail-closed"
    );
    assert_eq!(
        fixture.service.native_agent_working_directory("%1"),
        Some(owned.as_path())
    );
}

/// Returning a pane to Native after Pane mode cannot revive an old source
/// token, even with the same directory and configuration text. Allocation-bound
/// mode ownership must reject this ABA before resolving any write grants.
#[test]
fn native_filesystem_source_rejects_mode_aba() {
    let mut fixture = FilesystemContextFixture::new();
    let source = fixture
        .service
        .capture_native_filesystem_context(&turn())
        .unwrap();
    fixture
        .service
        .set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Pane));
    fixture
        .service
        .set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Native));
    assert!(
        !fixture
            .service
            .native_filesystem_context_is_current(&turn(), &source)
            .unwrap()
    );
    assert!(
        fixture
            .service
            .native_filesystem_scopes_for_context(&turn(), &source)
            .is_err()
    );
}

/// Re-adopting the same literal directory still advances navigation ownership.
/// A queued context must not rebase merely because its final path spelling is
/// equal after the intervening adoption.
#[test]
fn native_filesystem_source_rejects_same_path_navigation_generation_change() {
    let mut fixture = FilesystemContextFixture::new();
    let source = fixture
        .service
        .capture_native_filesystem_context(&turn())
        .unwrap();
    fixture
        .service
        .agent_shell_store_mut()
        .ensure_session("%1")
        .unwrap()
        .record_native_working_directory(fixture.root.clone(), 0)
        .unwrap();
    assert!(
        !fixture
            .service
            .native_filesystem_context_is_current(&turn(), &source)
            .unwrap()
    );
}

/// Changed configured resource provenance invalidates old source data before
/// current scopes can be confused with a newly widened admission. A fresh
/// context may be captured separately; this helper never mutates the old one.
#[test]
fn native_filesystem_source_rejects_resource_replacement() {
    let mut fixture = FilesystemContextFixture::new();
    let source = fixture
        .service
        .capture_native_filesystem_context(&turn())
        .unwrap();
    let permissions = crate::runtime::config::runtime_configured_permissions_from_config(
        &serde_json::json!({"permissions":{"read_scopes":[fixture.root],"write_scopes":[]}}),
    )
    .unwrap();
    fixture
        .service
        .integration
        .replace_configured_permissions(permissions);
    assert!(
        !fixture
            .service
            .native_filesystem_context_is_current(&turn(), &source)
            .unwrap()
    );
    assert!(
        fixture
            .service
            .native_filesystem_scopes_for_context(&turn(), &source)
            .is_err()
    );
}

/// Root process retirement cannot leave a valid source token behind. Missing
/// live-root evidence is rejection, independently of a still-matching pane,
/// conversation, operation directory or policy snapshot.
#[test]
fn native_filesystem_source_rejects_root_retirement() {
    let mut fixture = FilesystemContextFixture::new();
    let source = fixture
        .service
        .capture_native_filesystem_context(&turn())
        .unwrap();
    fixture.service.terminate_all_pane_processes().unwrap();
    assert!(
        fixture
            .service
            .native_filesystem_context_is_current(&turn(), &source)
            .is_err()
    );
    assert!(source.capture_root().is_err());
}

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

/// Managed catalog access is an additive controller grant, not an explicit
/// user scope declaration that disables deepest-project-trust fallback. A real
/// config application with a config root must retain both catalog management
/// and trusted project authority, while an empty child declaration still denies
/// inheritance and a deeper rejection still withholds project authority.
#[test]
fn managed_catalog_scopes_do_not_mask_implicit_project_trust() {
    let root = temp_root("catalog-scope-project-trust");
    let project = root.join("project");
    let nested = project.join("denied");
    let config = root.join("config");
    for path in [
        &project,
        &nested,
        &config.join("skills"),
        &config.join("macros"),
    ] {
        fs::create_dir_all(path).unwrap();
    }
    let project = fs::canonicalize(project).unwrap();
    let nested = fs::canonicalize(nested).unwrap();
    let config = fs::canonicalize(config).unwrap();
    let mut service = test_runtime_service();
    service.set_config_root(config.clone());
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "catalog-scope-defaults".into(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[history]\nlines = 10\n".into(),
        }])
        .unwrap();
    let mut trust = ProjectTrustStore::default();
    trust
        .decide_at(project.clone(), TrustDecision::Trusted, None, 1)
        .unwrap();
    trust
        .decide_at(nested.clone(), TrustDecision::Rejected, None, 2)
        .unwrap();
    service.set_project_trust_store(trust, None);
    service.set_pane_current_working_directory("%1", project.clone());
    let scopes = service
        .native_filesystem_scopes_for_turn(&turn(), &project)
        .unwrap()
        .unwrap();
    assert!(
        scopes
            .read_scopes
            .contains(&project.to_str().unwrap().to_string()),
        "{scopes:?}"
    );
    assert!(
        scopes
            .write_scopes
            .contains(&project.to_str().unwrap().to_string())
    );
    for name in ["skills", "macros"] {
        let path = config.join(name).to_str().unwrap().to_string();
        assert!(scopes.read_scopes.contains(&path));
        assert!(scopes.write_scopes.contains(&path));
    }
    assert_eq!(
        service.primary_path_scope_status("%1").provenance,
        "trusted-project"
    );
    for decision in [
        TrustDecision::Rejected,
        TrustDecision::Revoked,
        TrustDecision::Pending,
    ] {
        let mut trust = ProjectTrustStore::default();
        trust
            .decide_at(project.clone(), TrustDecision::Trusted, None, 1)
            .unwrap();
        if decision == TrustDecision::Pending {
            trust
                .decide_at(nested.clone(), TrustDecision::Trusted, None, 2)
                .unwrap();
            let database = root.join("pending-trust.sqlite");
            trust.save_to_file(&database).unwrap();
            let connection = rusqlite::Connection::open(&database).unwrap();
            connection
                .execute(
                    "UPDATE project_trust SET state = 'pending' WHERE project_root = ?1",
                    [nested.to_str().unwrap()],
                )
                .unwrap();
            drop(connection);
            trust = ProjectTrustStore::load_from_file(&database).unwrap();
        } else {
            trust.decide_at(nested.clone(), decision, None, 2).unwrap();
        }
        service.set_project_trust_store(trust, None);
        service.set_pane_current_working_directory("%1", nested.clone());
        let denied = service
            .native_filesystem_scopes_for_turn(&turn(), &nested)
            .unwrap()
            .unwrap();
        assert!(
            !denied
                .read_scopes
                .iter()
                .any(|scope| Path::new(scope).starts_with(&project))
        );
        let error = service
            .ensure_implicit_project_authority_for_action(&turn())
            .unwrap_err();
        assert_eq!(error.kind(), MezErrorKind::Forbidden);
        assert!(
            error
                .message()
                .contains(if decision == TrustDecision::Pending {
                    "pending"
                } else if decision == TrustDecision::Revoked {
                    "revoked"
                } else {
                    "rejected"
                })
        );
    }
    service.set_pane_current_working_directory("%1", project.clone());
    service.set_subagent_scope_declaration(
        "agent-%1",
        mez_agent::SubagentScopeDeclaration {
            cooperation_mode: CooperationMode::ExploreOnly,
            approval_provenance: mez_agent::SubagentApprovalProvenance::Requested,
            current_directory: project.to_str().unwrap().into(),
            read_scopes: Vec::new(),
            write_scopes: Vec::new(),
            permission_preset: None,
        },
    );
    let child = service
        .native_filesystem_scopes_for_turn(&turn(), &project)
        .unwrap()
        .unwrap();
    assert!(child.read_scopes.is_empty());
    assert!(child.write_scopes.is_empty());
    fs::remove_dir_all(root).unwrap();
}

/// Explicit user scope lists retain precedence even when the named scope is
/// exactly a managed catalog root. Reapplying configuration under another config
/// root rebuilds provenance rather than retaining obsolete automatic grants.
#[test]
fn managed_catalog_scope_provenance_preserves_explicit_grants_and_reload() {
    let root = temp_root("catalog-scope-explicit-reload");
    let project = root.join("project");
    let first = root.join("first-config");
    let second = root.join("second-config");
    for path in [
        &project,
        &first.join("skills"),
        &first.join("macros"),
        &second.join("skills"),
        &second.join("macros"),
    ] {
        fs::create_dir_all(path).unwrap();
    }
    let project = fs::canonicalize(project).unwrap();
    let first = fs::canonicalize(first).unwrap();
    let second = fs::canonicalize(second).unwrap();
    let mut service = test_runtime_service();
    service.set_config_root(first.clone());
    let mut trust = ProjectTrustStore::default();
    trust
        .decide_at(project.clone(), TrustDecision::Trusted, None, 1)
        .unwrap();
    service.set_project_trust_store(trust, None);
    service.set_pane_current_working_directory("%1", project.clone());
    let catalog = first.join("skills");
    let explicit = format!(
        "[permissions]\nread_scopes = [{}]\n",
        serde_json::to_string(catalog.to_str().unwrap()).unwrap()
    );
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "explicit-catalog".into(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: explicit,
        }])
        .unwrap();
    let status = service.primary_path_scope_status("%1");
    assert_eq!(status.provenance, "explicit");
    assert!(
        !status
            .read_scopes
            .contains(&project.to_str().unwrap().to_string())
    );
    let scopes = service
        .native_filesystem_scopes_for_turn(&turn(), &project)
        .unwrap()
        .unwrap();
    assert!(
        scopes
            .read_scopes
            .contains(&catalog.to_str().unwrap().to_string())
    );
    assert!(
        !scopes
            .read_scopes
            .contains(&project.to_str().unwrap().to_string())
    );
    service.set_config_root(second.clone());
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "new-catalog-defaults".into(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[history]\nlines = 11\n".into(),
        }])
        .unwrap();
    let status = service.primary_path_scope_status("%1");
    assert_eq!(status.provenance, "trusted-project");
    assert!(
        status
            .read_scopes
            .contains(&project.to_str().unwrap().to_string())
    );
    assert!(
        status
            .read_scopes
            .iter()
            .any(|path| Path::new(path).starts_with(&second))
    );
    assert!(
        !status
            .read_scopes
            .iter()
            .any(|path| Path::new(path).starts_with(&first))
    );
    fs::remove_dir_all(root).unwrap();
}

/// The shared selector must honor each caller's cwd source. Native agent
/// workspace intent selects its project, while a pane-status provider using the
/// observed root context retains that root's project and never borrows intent.
#[test]
fn managed_catalog_scope_selection_preserves_agent_and_pane_context_roles() {
    let root = temp_root("catalog-scope-context-roles");
    let pane = root.join("pane");
    let agent = root.join("agent");
    let config = root.join("config");
    for path in [
        &pane,
        &agent,
        &config.join("skills"),
        &config.join("macros"),
    ] {
        fs::create_dir_all(path).unwrap();
    }
    let pane = fs::canonicalize(pane).unwrap();
    let agent = fs::canonicalize(agent).unwrap();
    let config = fs::canonicalize(config).unwrap();
    let mut service = test_runtime_service();
    service.set_config_root(config);
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "role-defaults".into(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[history]\nlines = 10\n".into(),
        }])
        .unwrap();
    let mut trust = ProjectTrustStore::default();
    for path in [&pane, &agent] {
        trust
            .decide_at(path.clone(), TrustDecision::Trusted, None, 1)
            .unwrap();
    }
    service.set_project_trust_store(trust, None);
    service.set_pane_current_working_directory("%1", pane.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_agent_native_shell_mode_for_tests("%1");
    service
        .agent_shell_store_mut()
        .ensure_session("%1")
        .unwrap()
        .record_native_working_directory(agent.clone(), 0)
        .unwrap();
    let status = service.primary_path_scope_status("%1");
    assert!(
        status
            .read_scopes
            .contains(&agent.to_str().unwrap().to_string())
    );
    assert!(
        !status
            .read_scopes
            .contains(&pane.to_str().unwrap().to_string())
    );
    let context = crate::runtime::processes::NativeShellContext::for_test(
        "/bin/sh".into(),
        Vec::new(),
        pane.clone(),
    );
    let scopes = service
        .native_path_scopes_for_pane_status_provider("%1", &context)
        .unwrap()
        .unwrap();
    assert!(
        scopes
            .read_scopes
            .contains(&pane.to_str().unwrap().to_string())
    );
    assert!(
        !scopes
            .read_scopes
            .contains(&agent.to_str().unwrap().to_string())
    );
    assert_eq!(service.pane_current_working_directory("%1"), Some(pane));
    fs::remove_dir_all(root).unwrap();
}
