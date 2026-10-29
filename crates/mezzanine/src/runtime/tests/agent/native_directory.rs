//! Agent workspace selection remains independent of observed pane directories.
//!
//! These fixtures adopt already-created canonical paths through the internal
//! state recorder; they qualify workspace consumers, not a registered `/cd`
//! command or asynchronous validation authority. No pane child is needed.

use super::*;

/// Ordinary metadata refresh, hidden-agent reentry and unrelated pane state
/// cannot overwrite exact native intent. Pane mode still uses its actual pane
/// directory, while the native launch owner retains separate root evidence.
#[test]
fn native_agent_directory_survives_pane_observation_and_hidden_reentry() {
    let root = temp_root("native-directory-owner-consumers");
    let pane = root.join("pane");
    let target = root.join("target");
    let observed = root.join("observed");
    let other = root.join("other");
    for path in [&pane, &target, &observed, &other] {
        fs::create_dir_all(path).unwrap();
    }
    let pane = fs::canonicalize(pane).unwrap();
    let target = fs::canonicalize(target).unwrap();
    let observed = fs::canonicalize(observed).unwrap();
    let other = fs::canonicalize(other).unwrap();
    let mut service = test_runtime_service();
    for id in ["%1", "%2"] {
        service.agent_shell_store_mut().enter_or_resume(id).unwrap();
        service.set_agent_native_shell_mode_for_tests(id);
    }
    service.set_pane_current_working_directory("%1", pane.clone());
    service.set_pane_current_working_directory("%2", other.clone());
    service
        .agent_shell_store_mut()
        .ensure_session("%1")
        .unwrap()
        .record_native_working_directory(target.clone(), 0)
        .unwrap();
    assert_eq!(
        service.agent_effective_working_directory("%1"),
        Some(target.clone())
    );
    assert_eq!(service.pane_current_working_directory("%1"), Some(pane));
    assert_eq!(
        service.agent_effective_working_directory("%2"),
        Some(other.clone())
    );
    service.set_pane_current_working_directory("%1", observed.clone());
    service.agent_shell_store_mut().request_exit("%1").unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    assert_eq!(
        service.agent_effective_working_directory("%1"),
        Some(target)
    );
    assert_eq!(
        service.pane_current_working_directory("%1"),
        Some(observed.clone())
    );
    assert_eq!(service.agent_effective_working_directory("%2"), Some(other));
    service.set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Pane));
    assert_eq!(
        service.agent_effective_working_directory("%1"),
        Some(observed)
    );
    let _ = fs::remove_dir_all(root);
}

/// Overlay and trust refresh must select the accepted native workspace rather
/// than stale pane observations. Entering an empty workspace clears the old
/// overlay, and an untrusted workspace does not gain trust or apply its config.
/// Navigation does not replace the conversation's immutable MMP project scope.
#[test]
fn native_agent_directory_drives_project_overlay_and_trust_refresh() {
    let root = temp_root("native-directory-project-consumers");
    let pane = root.join("pane");
    let target = root.join("target");
    let empty = root.join("empty");
    let untrusted = root.join("untrusted");
    for path in [&pane, &target, &empty, &untrusted] {
        fs::create_dir_all(path.join(".git")).unwrap();
    }
    for (path, lines) in [(&pane, 11), (&target, 17), (&untrusted, 29)] {
        fs::create_dir_all(path.join(".mezzanine")).unwrap();
        fs::write(
            path.join(".mezzanine/config.toml"),
            format!(
                "version = {}\n[history]\nlines = {lines}\n",
                crate::config::CURRENT_CONFIG_SCHEMA_VERSION
            ),
        )
        .unwrap();
    }
    let pane = fs::canonicalize(pane).unwrap();
    let target = fs::canonicalize(target).unwrap();
    let empty = fs::canonicalize(empty).unwrap();
    let untrusted = fs::canonicalize(untrusted).unwrap();
    let mut service = test_runtime_service();
    service.set_config_root(root.join("config"));
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[history]\nlines = 3\n".to_string(),
        }])
        .unwrap();
    service.set_agent_native_shell_mode_for_tests("%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let membership = mez_agent::messaging::ProjectMembership::from_canonical_root(
        fs::canonicalize(&pane).unwrap(),
    );
    service
        .agent_shell_store_mut()
        .capture_project_scope("%1", membership.clone())
        .unwrap();
    let mut trust = ProjectTrustStore::default();
    for path in [&pane, &target] {
        trust
            .decide_at(path.clone(), TrustDecision::Trusted, None, 1)
            .unwrap();
    }
    service.set_project_trust_store(trust, None);
    service.set_pane_current_working_directory("%1", pane.clone());
    service
        .refresh_project_config_layers_for_pane("%1")
        .unwrap();
    assert_eq!(service.terminal_history_limit(), 11);
    for (index, path, lines) in [(0, &target, 17), (1, &empty, 3), (2, &untrusted, 3)] {
        service
            .agent_shell_store_mut()
            .ensure_session("%1")
            .unwrap()
            .record_native_working_directory(fs::canonicalize(path).unwrap(), index)
            .unwrap();
        service
            .refresh_project_config_layers_for_pane("%1")
            .unwrap();
        assert_eq!(
            service.terminal_history_limit(),
            lines,
            "workspace={path:?}"
        );
        assert_eq!(
            service.pane_current_working_directory("%1"),
            Some(pane.clone())
        );
        assert_eq!(
            service
                .agent_shell_store()
                .get("%1")
                .unwrap()
                .project_scope
                .as_ref(),
            Some(&membership)
        );
    }
    assert!(service.trusted_agent_project_root_for_pane("%1").is_none());
    assert!(
        service
            .config_layers()
            .iter()
            .filter(|layer| layer.scope == ConfigScope::ProjectOverlay)
            .all(|layer| !layer.trusted)
    );
    assert_eq!(service.trusted_project_root_for_pane("%1"), Some(pane));
    let _ = fs::remove_dir_all(root);
}

/// Internal native requests follow accepted agent cwd while external CLI
/// enrollment continues to freeze the observed pane-process origin. Both use
/// actual worker-qualified mapping rows and stored trust, never path labels as
/// permission. Choosing an untrusted native workspace remains unattributed.
#[test]
fn native_agent_directory_keeps_external_accounting_on_pane_origin() {
    use crate::storage::token_usage::{AccountingOrigin, TokenUsageStore};
    let root = temp_root("native-directory-accounting-roles");
    let pane = root.join("pane");
    let target = root.join("target");
    let untrusted = root.join("untrusted");
    for path in [&pane, &target, &untrusted] {
        fs::create_dir_all(path).unwrap();
    }
    let pane = fs::canonicalize(pane).unwrap();
    let target = fs::canonicalize(target).unwrap();
    let untrusted = fs::canonicalize(untrusted).unwrap();
    let mut trust = ProjectTrustStore::default();
    for path in [&pane, &target] {
        trust
            .decide_at(path.clone(), TrustDecision::Trusted, None, 1)
            .unwrap();
    }
    let store = TokenUsageStore::new(root.join("usage.sqlite"));
    let records = trust.records().cloned().collect::<Vec<_>>();
    let rows = store.prepare_accounting_projects(&records).unwrap();
    let mut service = test_runtime_service();
    service.set_token_usage_store(store.clone());
    service
        .persistence
        .install_accounting_projects(&store, Some(rows));
    service.set_project_trust_store(trust, None);
    service.set_pane_current_working_directory("%1", pane.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_agent_native_shell_mode_for_tests("%1");
    let original = service.capture_pane_process_accounting_origin("%1");
    assert!(matches!(original, AccountingOrigin::Project(_)));
    assert_eq!(
        service.capture_agent_accounting_origin_for_pane("%1"),
        original
    );
    service
        .agent_shell_store_mut()
        .ensure_session("%1")
        .unwrap()
        .record_native_working_directory(target, 0)
        .unwrap();
    let moved = service.capture_agent_accounting_origin_for_pane("%1");
    assert!(matches!(moved, AccountingOrigin::Project(_)));
    assert_ne!(moved, original);
    let scope = service
        .runtime_message_project_scope(&mez_core::ids::PaneId::opaque("%1").unwrap())
        .unwrap();
    assert_eq!(
        scope,
        mez_agent::messaging::ProjectMembership::from_canonical_root(pane).scope_id()
    );
    assert_eq!(
        service.capture_pane_process_accounting_origin("%1"),
        original
    );
    service
        .agent_shell_store_mut()
        .ensure_session("%1")
        .unwrap()
        .record_native_working_directory(untrusted, 1)
        .unwrap();
    assert_eq!(
        service.capture_agent_accounting_origin_for_pane("%1"),
        AccountingOrigin::Unattributed
    );
    assert_eq!(
        service.capture_pane_process_accounting_origin("%1"),
        original
    );
    let _ = fs::remove_dir_all(root);
}

/// Fresh loop conversations change model-history ownership but must preserve
/// the native workspace captured by their actual controller. Both fork and new
/// modes use a runtime-owned binding; neither transcript-source labels nor a
/// copied parent intent authorize an independently rebound conversation.
#[test]
fn native_agent_directory_follows_exact_loop_work_binding() {
    for mode in ["--fork", "--new", ""] {
        let root = temp_root("native-directory-loop");
        fs::create_dir_all(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let pane = root.join("pane");
        let target = root.join("target");
        for path in [&pane, &target] {
            fs::create_dir(path).unwrap();
        }
        let mut service = test_runtime_service();
        service.set_agent_native_shell_mode_for_tests("%1");
        service.set_pane_current_working_directory("%1", pane.clone());
        service.set_pane_screen(
            "%1".to_string(),
            TerminalScreen::new(Size::new(80, 24).unwrap(), 100).unwrap(),
        );
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        let session = service
            .agent_shell_store_mut()
            .ensure_session("%1")
            .unwrap();
        session
            .record_native_working_directory(target.clone(), 0)
            .unwrap();
        let parent = session.session_id.clone();
        service
            .execute_agent_shell_loop_command(
                "%1",
                &format!("/loop {mode} --limit 1 inspect this workspace"),
            )
            .unwrap();
        assert_eq!(
            service.agent_effective_working_directory("%1"),
            Some(target.clone()),
            "{mode}"
        );
        assert_eq!(
            service.pane_current_working_directory("%1"),
            Some(pane.clone())
        );
        service.set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Pane));
        assert_eq!(
            service.agent_effective_working_directory("%1"),
            Some(pane.clone())
        );
        service.set_agent_native_shell_mode_for_tests("%1");
        assert_eq!(
            service.agent_effective_working_directory("%1"),
            Some(target.clone())
        );
        if !mode.is_empty() {
            service
                .agent_loop_state_mut("%1")
                .unwrap()
                .execution_pane_id = "%2".to_string();
            assert_eq!(service.native_agent_working_directory("%1"), None);
            service
                .agent_loop_state_mut("%1")
                .unwrap()
                .execution_pane_id = "%1".to_string();
        }
        service
            .agent_shell_store_mut()
            .ensure_session("%1")
            .unwrap()
            .pane_id = "%2".to_string();
        assert_eq!(service.native_agent_working_directory("%1"), None);
        service
            .agent_shell_store_mut()
            .ensure_session("%1")
            .unwrap()
            .pane_id = "%1".to_string();
        let session = service.agent_shell_store().get("%1").unwrap();
        let work = session.session_id.clone();
        let running = session.running_turn_id.clone().unwrap();
        if !mode.is_empty() {
            assert_ne!(work, parent);
            assert!(session.ephemeral);
            assert_eq!(session.native_working_directory(), None);
        }
        service
            .agent_shell_store_mut()
            .finish_turn("%1", &running)
            .unwrap();
        let state = service.agent_loop_state("%1").unwrap().clone();
        service
            .restore_agent_loop_parent_conversation("%1", &state, None)
            .unwrap();
        assert_eq!(
            service.agent_shell_store().get("%1").unwrap().session_id,
            parent
        );
        assert_eq!(
            service.agent_effective_working_directory("%1"),
            Some(target)
        );
        assert_eq!(
            service.agent_loop_state("%1").unwrap().work_conversation_id,
            None
        );
        if !mode.is_empty() {
            // Even the previous exact work CID is retired on parent return.
            service
                .agent_shell_store_mut()
                .bind_ephemeral_conversation_with_lineage_and_transcript_source(
                    "%1",
                    &work,
                    0,
                    None,
                    Some(parent.clone()),
                    0,
                )
                .unwrap();
            assert_eq!(service.native_agent_working_directory("%1"), None);
        }
        // Matching source labels alone do not install work in the controller.
        service
            .agent_shell_store_mut()
            .bind_ephemeral_conversation_with_lineage_and_transcript_source(
                "%1",
                "unrelated-attempt",
                0,
                None,
                Some(parent),
                0,
            )
            .unwrap();
        assert_eq!(service.agent_effective_working_directory("%1"), Some(pane));
        assert_eq!(service.native_agent_working_directory("%1"), None);
        let _ = fs::remove_dir_all(root);
    }
}
