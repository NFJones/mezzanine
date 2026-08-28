//! Runtime tests for agent prompt lifecycle behavior.

use super::*;

/// Verifies shorthand prompt words still resolve to the read-only subagent mode.
///
/// Provider prompts describe cooperation mode as a safety/scope concept, and
/// some models may echo those words back even when the compact spawn schema
/// omits the explicit field. Accepting these shorthands keeps runtime subagent
/// spawns compatible with that model behavior instead of failing validation.
#[test]
fn runtime_cooperation_mode_accepts_prompt_shorthand_scope_words() {
    assert_eq!(
        runtime_cooperation_mode("safety").unwrap(),
        CooperationMode::ExploreOnly
    );
    assert_eq!(
        runtime_cooperation_mode("scope").unwrap(),
        CooperationMode::ExploreOnly
    );
    assert_eq!(
        runtime_cooperation_mode("scoped").unwrap(),
        CooperationMode::ExploreOnly
    );
}

/// Verifies native runtime-owned subagent startup validates only root-process
/// context and permits the child turn to start without pane bootstrap state.
///
/// This protects native mode from accidentally inheriting the foreign-shell
/// prompt gate introduced for pane-mode compatibility startup.
#[test]
fn runtime_native_subagent_startup_bypasses_pane_bootstrap() {
    let mut service = test_runtime_service();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let spawned = service
        .spawn_runtime_subagent(
            &primary,
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "explorer".to_string(),
                placement: "new-pane".to_string(),
                cooperation_mode: CooperationMode::ExploreOnly,
                cooperation_mode_defaulted: false,
                read_scopes: Vec::new(),
                read_scopes_defaulted: false,
                write_scopes: Vec::new(),
                write_scopes_defaulted: false,
                task_prompt: "inspect native startup".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: false,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    let spawned = serde_json::from_str::<serde_json::Value>(&spawned).unwrap();
    let pane_id = spawned["pane"]["pane_id"].as_str().unwrap();
    let turn_id = spawned["turn"]["id"].as_str().unwrap();

    assert_eq!(
        service.runtime_agent_surface_startup_phase_for_tests(pane_id),
        Some("ready")
    );
    assert!(!service.pane_bootstrap_is_pending_for_tests(pane_id));
    assert!(!service.pane_has_uncertified_foreign_shell_boundary(pane_id));
    assert!(service.agent_provider_task_is_pending(turn_id));
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Running)
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a POSIX pane-mode subagent remains queued until authenticated
/// prompt admission and environment bootstrap settle, without creating a
/// foreign-shell boundary or claiming provider capacity early.
#[test]
fn runtime_posix_subagent_startup_releases_queued_turn_after_bootstrap() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let spawned = service
        .spawn_runtime_subagent(
            &primary,
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "explorer".to_string(),
                placement: "new-pane".to_string(),
                cooperation_mode: CooperationMode::ExploreOnly,
                cooperation_mode_defaulted: false,
                read_scopes: Vec::new(),
                read_scopes_defaulted: false,
                write_scopes: Vec::new(),
                write_scopes_defaulted: false,
                task_prompt: "inspect managed startup".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: false,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    let spawned = serde_json::from_str::<serde_json::Value>(&spawned).unwrap();
    let pane_id = spawned["pane"]["pane_id"].as_str().unwrap().to_string();
    let turn_id = spawned["turn"]["id"].as_str().unwrap().to_string();

    assert_eq!(
        service.runtime_agent_surface_startup_phase_for_tests(&pane_id),
        Some("managed-admitting")
    );
    assert_eq!(service.agent_scheduler().snapshot().queued, 1);
    assert_eq!(service.agent_scheduler().snapshot().running, 0);
    assert!(!service.agent_provider_task_is_pending(&turn_id));
    assert!(!service.pane_has_uncertified_foreign_shell_boundary(&pane_id));
    assert!(
        service
            .running_shell_transactions_for_tests()
            .values()
            .all(|transaction| transaction.pane_id != pane_id)
    );

    let token = service
        .posix_startup_token_for_tests(&pane_id)
        .unwrap()
        .to_string();
    assert_eq!(
        service
            .observe_managed_shell_protocol_event(
                &pane_id,
                mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION,
                mez_terminal::ManagedShellAdapter::Posix,
                &token,
                &mez_terminal::ManagedShellProtocolEvent::AdapterAvailable { trigger: None },
            )
            .unwrap(),
        1
    );
    assert_eq!(
        service.runtime_agent_surface_startup_phase_for_tests(&pane_id),
        Some("managed-bootstrapping")
    );
    assert_eq!(service.agent_scheduler().snapshot().queued, 1);

    let (marker, bootstrap_turn_id) = service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| {
            (transaction.pane_id == pane_id
                && transaction.kind == RunningShellTransactionKind::Bootstrap)
                .then(|| (marker.clone(), transaction.turn_id.clone()))
        })
        .unwrap();
    service
        .observe_agent_shell_transaction_start(
            &pane_id,
            &marker,
            &bootstrap_turn_id,
            &format!("agent-{pane_id}"),
            &pane_id,
        )
        .unwrap();
    let output = "env\tos\tLinux\n\
env\tarch\tx86_64\n\
env\thost\ttest-host\n\
env\tuser\ttest-user\n\
env\tshell_path\t/bin/sh\n\
env\tshell_class\tposix-sh\n\
env\tpath\t/usr/bin:/bin\n\
env\tcwd\t/tmp\n\
env\tgit_repo\t0\n\
bootstrap\tcomplete\t1714500000\n";
    let transaction = service
        .running_shell_transactions_mut_for_tests()
        .get_mut(&marker)
        .unwrap();
    transaction.observed_output_bytes = output.len();
    transaction.observed_output_preview = output.to_string();
    service
        .observe_agent_shell_transaction_end(
            &pane_id,
            &marker,
            &bootstrap_turn_id,
            &format!("agent-{pane_id}"),
            &pane_id,
            0,
        )
        .unwrap();

    assert_eq!(
        service.runtime_agent_surface_startup_phase_for_tests(&pane_id),
        Some("ready")
    );
    assert_eq!(service.agent_scheduler().snapshot().queued, 0);
    assert_eq!(service.agent_scheduler().snapshot().running, 1);
    assert!(service.agent_provider_task_is_pending(&turn_id));
    assert!(!service.pane_bootstrap_is_pending_for_tests(&pane_id));
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a managed pane startup admission timeout fails the queued child
/// turn and releases scheduler ownership instead of leaving it bootstrapping.
#[test]
fn runtime_subagent_startup_timeout_settles_queued_turn() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let spawned = service
        .spawn_runtime_subagent(
            &primary,
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "explorer".to_string(),
                placement: "new-pane".to_string(),
                cooperation_mode: CooperationMode::ExploreOnly,
                cooperation_mode_defaulted: false,
                read_scopes: Vec::new(),
                read_scopes_defaulted: false,
                write_scopes: Vec::new(),
                write_scopes_defaulted: false,
                task_prompt: "inspect startup timeout".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: false,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    let spawned = serde_json::from_str::<serde_json::Value>(&spawned).unwrap();
    let pane_id = spawned["pane"]["pane_id"].as_str().unwrap();
    let turn_id = spawned["turn"]["id"].as_str().unwrap();

    assert_eq!(
        service
            .recover_expired_runtime_agent_surface_startups(u64::MAX)
            .unwrap(),
        1
    );
    assert_eq!(
        service.runtime_agent_surface_startup_phase_for_tests(pane_id),
        Some("failed")
    );
    assert_eq!(service.agent_scheduler().snapshot().queued, 0);
    assert_eq!(service.agent_scheduler().snapshot().running, 0);
    assert!(!service.agent_provider_task_is_pending(turn_id));
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Failed)
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a spawned subagent pane records the exact parent prompt before the
/// child turn starts.
///
/// Parent-authored task text is the child agent's effective user instruction.
/// Showing it as a `parent>` log entry lets users inspect the child pane
/// without reconstructing the prompt from parent-pane status messages.
#[test]
fn runtime_subagent_spawn_logs_parent_prompt_in_child_pane() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let spawn = SubagentSpawnRequest {
        parent_agent_id: "agent-%1".to_string(),
        requested_role: "explorer".to_string(),
        placement: "new-pane".to_string(),
        cooperation_mode: CooperationMode::ExploreOnly,
        cooperation_mode_defaulted: false,
        read_scopes: Vec::new(),
        read_scopes_defaulted: false,
        write_scopes: Vec::new(),
        write_scopes_defaulted: false,
        task_prompt: "inspect the renderer issue".to_string(),
        explicit_user_approval: false,
        skip_initial_turn: false,
    };

    let spawned = service
        .spawn_runtime_subagent(
            &primary,
            spawn,
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    assert!(spawned.contains(r#""id":"turn-1""#), "{spawned}");
    let child_pane_id = serde_json::from_str::<serde_json::Value>(&spawned)
        .unwrap()
        .get("pane")
        .and_then(|pane| pane.get("pane_id"))
        .and_then(serde_json::Value::as_str)
        .expect("spawned pane id")
        .to_string();
    let child_text = service
        .pane_screen(&child_pane_id)
        .unwrap()
        .normal_content_lines()
        .join("\n");

    assert!(
        child_text.contains("parent> inspect the renderer issue"),
        "{child_text}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies spawned subagents inherit the parent pane's plan-only mode and
/// pane-local latency override before their first turn is created.
///
/// Both settings are session preferences rather than child-role inputs. Losing
/// either at spawn time lets a child issue writes while the parent is planning
/// or silently changes the provider-visible latency selected by the user.
#[test]
fn runtime_subagent_inherits_parent_plan_and_latency_preferences() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\"]\ndefault_model = \"gpt-5.5\"\n\n[model_profiles.default]\nprovider = \"openai\"\nmodel = \"gpt-5.5\"\nreasoning_profile = \"high\"\nlatency_preference = \"default\"\n\n[model_profiles.default.provider_options]\nreasoning_effort = \"high\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.cache_provider_model_catalog_for_tests(
        "openai",
        vec![mez_agent::ProviderModelInfo {
            id: "gpt-5.5".to_string(),
            display_name: None,
            reasoning_levels: vec!["high".to_string()],
            context_window_tokens: Some(1_050_000),
            max_input_tokens: None,
            capabilities: Vec::new(),
        }],
        vec!["high".to_string()],
    );
    service
        .execute_agent_shell_plan_command("%1", "/plan on")
        .unwrap();
    service
        .execute_agent_shell_latency_command("%1", "/latency slow")
        .unwrap();

    let spawned = service
        .spawn_runtime_subagent(
            &primary,
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "explorer".to_string(),
                placement: "new-pane".to_string(),
                cooperation_mode: CooperationMode::ExploreOnly,
                cooperation_mode_defaulted: false,
                read_scopes: Vec::new(),
                read_scopes_defaulted: false,
                write_scopes: Vec::new(),
                write_scopes_defaulted: false,
                task_prompt: "inspect the inherited preferences".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: true,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    let child_pane_id =
        serde_json::from_str::<serde_json::Value>(&spawned).unwrap()["pane"]["pane_id"]
            .as_str()
            .expect("spawned pane id")
            .to_string();
    let child_agent_id = format!("agent-{child_pane_id}");

    assert!(service.agent_planning_enabled(&child_pane_id));
    let (_profile_name, profile) = service
        .active_model_profile_for_pane(&child_pane_id, &child_agent_id, None)
        .unwrap();
    assert_eq!(profile.latency_preference.as_deref(), Some("slow"));
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies spawning from default parent preferences leaves the child in the
/// default plan mode and does not create an unnecessary model-profile override.
///
/// Inheritance snapshots explicit pane-local choices. Synthesizing a child
/// override for defaults would detach it from later session configuration
/// changes without preserving any user-selected preference.
#[test]
fn runtime_subagent_default_preferences_do_not_create_overrides() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let spawned = service
        .spawn_runtime_subagent(
            &primary,
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "explorer".to_string(),
                placement: "new-pane".to_string(),
                cooperation_mode: CooperationMode::ExploreOnly,
                cooperation_mode_defaulted: false,
                read_scopes: Vec::new(),
                read_scopes_defaulted: false,
                write_scopes: Vec::new(),
                write_scopes_defaulted: false,
                task_prompt: "inspect the inherited defaults".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: true,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    let child_pane_id =
        serde_json::from_str::<serde_json::Value>(&spawned).unwrap()["pane"]["pane_id"]
            .as_str()
            .expect("spawned pane id")
            .to_string();
    let child_agent_id = format!("agent-{child_pane_id}");

    assert!(!service.agent_planning_enabled(&child_pane_id));
    assert!(
        !service
            .integration
            .model_profile_overrides()
            .agent_profiles
            .contains_key(&child_agent_id)
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies spawned subagent conversations are durable, classified separately,
/// and excluded from default `/resume` saved-session results.
///
/// Subagents retain delegated work for direct UUID recovery, but default resume
/// discovery must not offer child sessions that lack the parent interaction
/// context.
#[test]
fn runtime_subagent_sessions_are_durable_but_hidden_from_resume() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("subagent-not-resumable"));
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let spawn = SubagentSpawnRequest {
        parent_agent_id: "agent-%1".to_string(),
        requested_role: "explorer".to_string(),
        placement: "new-pane".to_string(),
        cooperation_mode: CooperationMode::ExploreOnly,
        cooperation_mode_defaulted: false,
        read_scopes: Vec::new(),
        read_scopes_defaulted: false,
        write_scopes: Vec::new(),
        write_scopes_defaulted: false,
        task_prompt: "inspect the renderer issue".to_string(),
        explicit_user_approval: false,
        skip_initial_turn: false,
    };

    let spawned = service
        .spawn_runtime_subagent(
            &primary,
            spawn,
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    let child_pane_id = serde_json::from_str::<serde_json::Value>(&spawned)
        .unwrap()
        .get("pane")
        .and_then(|pane| pane.get("pane_id"))
        .and_then(serde_json::Value::as_str)
        .expect("spawned pane id")
        .to_string();
    let child_session = service.agent_shell_store().get(&child_pane_id).unwrap();

    assert!(!child_session.ephemeral);
    assert_eq!(
        child_session.conversation_kind,
        mez_agent::AgentConversationKind::Subagent
    );
    assert_eq!(
        transcript_store
            .conversation_kind(&child_session.session_id)
            .unwrap(),
        mez_agent::AgentConversationKind::Subagent
    );
    let child_conversation_id = child_session.session_id.clone();
    let saved_child = transcript_store
        .saved_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.summary.conversation_id == child_conversation_id)
        .expect("durable subagent session should be retained in storage");
    assert_eq!(
        saved_child.conversation_kind,
        mez_agent::AgentConversationKind::Subagent
    );
    assert!(
        service
            .saved_sessions_record_browser()
            .unwrap()
            .records()
            .iter()
            .all(|record| record.id != child_conversation_id)
    );
    service.checkpoint_agent_session_metadata().unwrap();
    assert!(
        transcript_store
            .load_agent_session_metadata(service.session().id.as_str())
            .unwrap()
            .iter()
            .all(|metadata| metadata.conversation_id != child_conversation_id)
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Builds a Bubblewrap runtime whose root pane is inside one trusted project.
///
/// The helper intentionally leaves configured scope arrays empty so subagent
/// inheritance exercises the trusted-project default rather than explicit
/// permission configuration.
fn trusted_project_subagent_scope_service(
    test_name: &str,
) -> (
    RuntimeSessionService,
    mez_core::ids::ClientId,
    PathBuf,
    PathBuf,
) {
    let root = temp_root(test_name);
    let project_root = root.join("project");
    let working_directory = project_root.join("src");
    fs::create_dir_all(project_root.join(".git")).unwrap();
    fs::create_dir_all(&working_directory).unwrap();
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service.set_pane_current_working_directory("%1".to_string(), working_directory.clone());
    let configured =
        crate::runtime::config::runtime_configured_permissions_from_config(&serde_json::json!({
            "permissions": {"sandbox": "bubblewrap"}
        }))
        .unwrap();
    service
        .integration
        .replace_configured_permissions(configured);
    let mut trust_store = ProjectTrustStore::default();
    trust_store
        .decide_at(
            project_root.clone(),
            TrustDecision::Trusted,
            Some(project_root.join(".git")),
            1,
        )
        .unwrap();
    service.set_project_trust_store(trust_store, None);
    (service, primary, root, project_root)
}

/// Spawns one idle child and returns its retained effective scope declaration.
fn spawn_idle_subagent_scope(
    service: &mut RuntimeSessionService,
    primary: &mez_core::ids::ClientId,
    spawn: SubagentSpawnRequest,
) -> (String, mez_agent::SubagentScopeDeclaration) {
    let spawned = service
        .spawn_runtime_subagent(
            primary,
            spawn,
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    let child_agent_id =
        serde_json::from_str::<serde_json::Value>(&spawned).unwrap()["agent"]["id"]
            .as_str()
            .unwrap()
            .to_string();
    let scope = service
        .subagent_scope_declaration(&child_agent_id)
        .expect("Bubblewrap child must retain effective parent authority");
    (child_agent_id, scope)
}

/// Verifies omitted child scopes inherit the root parent's trusted-project
/// Bubblewrap authority instead of independently deriving authority later.
#[test]
fn runtime_subagent_omitted_scopes_inherit_parent_bubblewrap_authority() {
    let (mut service, primary, root, project_root) =
        trusted_project_subagent_scope_service("runtime-subagent-inherit-bubblewrap");
    let spawn = SubagentSpawnRequest {
        parent_agent_id: "agent-%1".to_string(),
        requested_role: "worker".to_string(),
        placement: "new-pane".to_string(),
        cooperation_mode: CooperationMode::OwnedWrite,
        cooperation_mode_defaulted: false,
        read_scopes: Vec::new(),
        read_scopes_defaulted: true,
        write_scopes: Vec::new(),
        write_scopes_defaulted: true,
        task_prompt: "implement the bounded change".to_string(),
        explicit_user_approval: false,
        skip_initial_turn: true,
    };

    let (_, scope) = spawn_idle_subagent_scope(&mut service, &primary, spawn);
    let expected = project_root.to_string_lossy().into_owned();

    assert_eq!(scope.read_scopes, vec![expected.clone()]);
    assert_eq!(scope.write_scopes, vec![expected]);
    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// Verifies an explorer with omitted scopes inherits confined read authority
/// without inheriting the writable parent's write authority.
///
/// Compact explorer actions omit scope arrays, so runtime normalization must
/// clear inherited writes before validating the explore-only cooperation mode.
#[test]
fn runtime_explorer_omitted_scopes_clear_inherited_write_authority() {
    let (mut service, primary, root, project_root) =
        trusted_project_subagent_scope_service("runtime-explorer-inherit-bubblewrap");
    let spawn = SubagentSpawnRequest {
        parent_agent_id: "agent-%1".to_string(),
        requested_role: "explorer".to_string(),
        placement: "new-pane".to_string(),
        cooperation_mode: CooperationMode::ExploreOnly,
        cooperation_mode_defaulted: true,
        read_scopes: Vec::new(),
        read_scopes_defaulted: true,
        write_scopes: Vec::new(),
        write_scopes_defaulted: true,
        task_prompt: "inspect the bounded change".to_string(),
        explicit_user_approval: false,
        skip_initial_turn: true,
    };

    let (_, scope) = spawn_idle_subagent_scope(&mut service, &primary, spawn);
    let expected = project_root.to_string_lossy().into_owned();

    assert_eq!(scope.read_scopes, vec![expected]);
    assert!(scope.write_scopes.is_empty());
    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// Verifies host access retains child coordination metadata without
/// manufacturing inherited Bubblewrap authority.
#[test]
fn runtime_host_access_subagent_retains_coordination_scope() {
    let (mut service, primary, root, _) =
        trusted_project_subagent_scope_service("runtime-subagent-host-access");
    service.set_pane_approval_policy_override("%1", Some(ApprovalPolicy::HostAccess));
    let spawn = SubagentSpawnRequest {
        parent_agent_id: "agent-%1".to_string(),
        requested_role: "worker".to_string(),
        placement: "new-pane".to_string(),
        cooperation_mode: CooperationMode::OwnedWrite,
        cooperation_mode_defaulted: false,
        read_scopes: Vec::new(),
        read_scopes_defaulted: true,
        write_scopes: Vec::new(),
        write_scopes_defaulted: true,
        task_prompt: "work with inherited host access".to_string(),
        explicit_user_approval: false,
        skip_initial_turn: true,
    };
    let spawned = service
        .spawn_runtime_subagent(
            &primary,
            spawn,
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    let child_agent_id =
        serde_json::from_str::<serde_json::Value>(&spawned).unwrap()["agent"]["id"]
            .as_str()
            .unwrap()
            .to_string();

    let scope = service
        .subagent_scope_declaration(&child_agent_id)
        .expect("host-access child must retain coordination metadata");
    assert_eq!(scope.cooperation_mode, CooperationMode::OwnedWrite);
    assert!(scope.read_scopes.is_empty());
    assert!(scope.write_scopes.is_empty());
    assert!(matches!(
        service.configured_permissions().sandbox,
        crate::runtime::SandboxConfig::Bubblewrap(_)
    ));
    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// Verifies explicit empty child scope arrays remain empty and therefore deny
/// Bubblewrap filesystem authority rather than behaving like omitted fields.
#[test]
fn runtime_subagent_explicit_empty_scopes_do_not_inherit_parent_authority() {
    let (mut service, primary, root, _) =
        trusted_project_subagent_scope_service("runtime-subagent-explicit-empty-bubblewrap");
    let spawn = SubagentSpawnRequest {
        parent_agent_id: "agent-%1".to_string(),
        requested_role: "worker".to_string(),
        placement: "new-pane".to_string(),
        cooperation_mode: CooperationMode::OwnedWrite,
        cooperation_mode_defaulted: false,
        read_scopes: Vec::new(),
        read_scopes_defaulted: false,
        write_scopes: Vec::new(),
        write_scopes_defaulted: false,
        task_prompt: "perform no filesystem work".to_string(),
        explicit_user_approval: false,
        skip_initial_turn: true,
    };

    let (_, scope) = spawn_idle_subagent_scope(&mut service, &primary, spawn);

    assert!(scope.read_scopes.is_empty());
    assert!(scope.write_scopes.is_empty());
    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// Verifies spawned children receive a parent pane's explicit sandbox backend
/// as a child-local snapshot before their first agent turn is started.
#[test]
fn runtime_subagent_inherits_explicit_parent_sandbox_override() {
    let (mut service, primary, root, _) =
        trusted_project_subagent_scope_service("runtime-subagent-inherit-sandbox-override");
    service
        .integration
        .set_pane_sandbox_override("%1", Some(crate::runtime::SandboxConfig::PolicyOnly));
    let spawn = SubagentSpawnRequest {
        parent_agent_id: "agent-%1".to_string(),
        requested_role: "worker".to_string(),
        placement: "new-pane".to_string(),
        cooperation_mode: CooperationMode::OwnedWrite,
        cooperation_mode_defaulted: false,
        read_scopes: Vec::new(),
        read_scopes_defaulted: true,
        write_scopes: Vec::new(),
        write_scopes_defaulted: true,
        task_prompt: "inherit sandbox state".to_string(),
        explicit_user_approval: false,
        skip_initial_turn: true,
    };

    let (child_agent_id, _) = spawn_idle_subagent_scope(&mut service, &primary, spawn);
    let child_pane_id = child_agent_id
        .strip_prefix("agent-")
        .expect("runtime subagent ids contain their pane id");

    assert!(service.pane_has_sandbox_override(child_pane_id));
    assert!(matches!(
        service.sandbox_config_for_pane(child_pane_id),
        crate::runtime::SandboxConfig::PolicyOnly
    ));
    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// Verifies child-requested scopes may narrow inherited Bubblewrap authority
/// but cannot add a sibling path outside the trusted parent scope.
#[test]
fn runtime_subagent_requested_scopes_only_narrow_parent_authority() {
    let (mut service, primary, root, project_root) =
        trusted_project_subagent_scope_service("runtime-subagent-narrow-bubblewrap");
    let spawn = SubagentSpawnRequest {
        parent_agent_id: "agent-%1".to_string(),
        requested_role: "worker".to_string(),
        placement: "new-pane".to_string(),
        cooperation_mode: CooperationMode::OwnedWrite,
        cooperation_mode_defaulted: false,
        read_scopes: vec!["generated".to_string(), "../../outside".to_string()],
        read_scopes_defaulted: false,
        write_scopes: vec!["generated".to_string(), "../../outside".to_string()],
        write_scopes_defaulted: false,
        task_prompt: "update generated files".to_string(),
        explicit_user_approval: false,
        skip_initial_turn: true,
    };

    let (_, scope) = spawn_idle_subagent_scope(&mut service, &primary, spawn);

    assert_eq!(scope.read_scopes, vec!["generated"]);
    assert_eq!(scope.write_scopes, vec!["generated"]);
    assert!(
        !scope
            .read_scopes
            .iter()
            .any(|scope| scope.contains("outside"))
    );
    assert!(project_root.join("src").starts_with(&project_root));
    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// Verifies a nested child inherits its scoped parent's already narrowed
/// authority even when its own pane remains under a broader trusted project.
#[test]
fn runtime_nested_subagent_cannot_rediscover_broader_trusted_authority() {
    let (mut service, primary, root, _) =
        trusted_project_subagent_scope_service("runtime-nested-subagent-bubblewrap");
    let first_spawn = SubagentSpawnRequest {
        parent_agent_id: "agent-%1".to_string(),
        requested_role: "worker".to_string(),
        placement: "new-pane".to_string(),
        cooperation_mode: CooperationMode::OwnedWrite,
        cooperation_mode_defaulted: false,
        read_scopes: vec!["generated".to_string()],
        read_scopes_defaulted: false,
        write_scopes: vec!["generated".to_string()],
        write_scopes_defaulted: false,
        task_prompt: "own generated files".to_string(),
        explicit_user_approval: false,
        skip_initial_turn: true,
    };
    let (parent_agent_id, parent_scope) =
        spawn_idle_subagent_scope(&mut service, &primary, first_spawn);
    let nested_spawn = SubagentSpawnRequest {
        parent_agent_id,
        requested_role: "worker".to_string(),
        placement: "new-pane".to_string(),
        cooperation_mode: CooperationMode::OwnedWrite,
        cooperation_mode_defaulted: false,
        read_scopes: Vec::new(),
        read_scopes_defaulted: true,
        write_scopes: Vec::new(),
        write_scopes_defaulted: true,
        task_prompt: "continue generated work".to_string(),
        explicit_user_approval: false,
        skip_initial_turn: true,
    };

    let (_, nested_scope) = spawn_idle_subagent_scope(&mut service, &primary, nested_spawn);

    assert_eq!(nested_scope.read_scopes, parent_scope.read_scopes);
    assert_eq!(nested_scope.write_scopes, parent_scope.write_scopes);
    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// Verifies runtime agent shell prompt starts live turn lifecycle.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_agent_shell_prompt_starts_live_turn_lifecycle() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-prompt","input":"summarize the pane"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"turn_started""#), "{response}");
    assert!(response.contains(r#""command":null"#), "{response}");
    assert!(response.contains(r#""body":null"#), "{response}");
    assert!(response.contains(r#""state":"running""#), "{response}");
    let response_json: serde_json::Value = serde_json::from_str(&response).unwrap();
    let turn = &response_json["result"]["turn"];
    assert_eq!(turn["id"], "turn-1", "{response}");
    assert_eq!(turn["version"], serde_json::json!(1), "{response}");
    assert_eq!(turn["agent_id"], "agent-%1", "{response}");
    assert_eq!(turn["state"], "running", "{response}");
    assert!(turn["created_at"].as_str().is_some(), "{response}");
    assert!(turn["started_at"].as_str().is_some(), "{response}");
    assert_eq!(turn["finished_at"], serde_json::Value::Null, "{response}");
    assert_eq!(turn["prompt_preview"], "summarize the pane", "{response}");
    assert_eq!(turn["approval_ids"], serde_json::json!([]), "{response}");
    assert_eq!(
        turn["result_summary"],
        serde_json::Value::Null,
        "{response}"
    );
    assert!(
        turn["extensions"]["context_blocks"].as_u64().is_some(),
        "{response}"
    );
    let tasks = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"tasks","method":"agent/task/list","params":{"target":{"pane_id":"%1"}}}"#,
        &primary,
    );
    assert!(tasks.contains(r#""id":"turn-1""#), "{tasks}");
    assert!(tasks.contains(r#""state":"running""#), "{tasks}");
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].turn_id, "turn-1");
    assert_eq!(pending[0].model_profile.provider, "openai");
    assert_eq!(pending[0].model_profile.model, "gpt-5.6-terra");
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("agent: working on"), "{pane_text}");
}

/// Verifies that a user prompt and a non-command agent response are written
/// into the pane's normal terminal buffer instead of a transient prompt
/// overlay. This preserves the Codex-like interaction transcript as copyable
/// terminal text while still retaining terminal style spans for user-facing
/// color. Each injected line keeps the same Mezzanine UI prefix used by the
/// pane-local prompt so message boundaries are visible in the terminal buffer.
#[test]
fn runtime_agent_prompt_and_say_response_are_interleaved_in_pane_buffer() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 12).unwrap(), 100).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-visible-say","input":"summarize visible output"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap say response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "say-1".to_string(),
                    rationale: "answer in the pane".to_string(),
                    payload: mez_agent::AgentActionPayload::Say {
                        status: mez_agent::SayStatus::Final,
                        text: "The pane is ready.".to_string(),
                        content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
                    },
                }],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("user> summarize visible output"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("▐ user> summarize visible output"),
        "{pane_text}"
    );
    assert!(pane_text.contains("mez> The pane is ready."), "{pane_text}");
    assert!(
        pane_text.contains("▐ mez> The pane is ready."),
        "{pane_text}"
    );
    assert!(
        !pane_text.contains("mez> answer in the pane"),
        "{pane_text}"
    );
    assert!(!pane_text.contains("agent: turn turn-1"), "{pane_text}");
    let theme = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap()
        .ui_theme;
    let assistant_line = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines()
        .into_iter()
        .find(|line| line.text.contains("mez> The pane is ready."))
        .unwrap();
    assert!(assistant_line.text.starts_with("▐ "));
    assert!(!assistant_line.style_spans.is_empty());
    let assistant_body_start = "▐ mez> ".chars().count();
    assert!(
        assistant_line
            .style_spans
            .iter()
            .all(|span| span.start.saturating_add(span.length) <= assistant_body_start),
        "assistant body text should use default terminal color: {:?}",
        assistant_line.style_spans
    );
    assert!(
        assistant_line.style_spans.iter().any(|span| {
            span.start == 0
                && span.rendition.foreground
                    == Some(theme.colors.agent_transcript_assistant.foreground)
                && span.rendition.background.is_none()
                && span.rendition.bold
        }),
        "assistant gutter and label should use themed foreground without a background: {:?}",
        assistant_line.style_spans
    );
    let user_line = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines()
        .into_iter()
        .find(|line| line.text.contains("user> summarize visible output"))
        .unwrap();
    let user_body_start = "▐ user> ".chars().count();
    assert!(
        user_line
            .style_spans
            .iter()
            .all(|span| span.start.saturating_add(span.length) <= user_body_start),
        "user prompt body text should use default terminal color: {:?}",
        user_line.style_spans
    );
    assert!(
        user_line.style_spans.iter().any(|span| {
            span.start == 0
                && span.rendition.foreground == Some(theme.colors.agent_transcript_user.foreground)
                && span.rendition.background.is_none()
                && span.rendition.bold
        }),
        "user gutter and label should use themed foreground without a background: {:?}",
        user_line.style_spans
    );
    service
        .append_agent_error_text_to_terminal_buffer("%1", "agent error: failed")
        .unwrap();
    service
        .append_agent_command_preview_to_terminal_buffer("%1", "ls -la")
        .unwrap();
    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let error_line = styled_lines
        .iter()
        .find(|line| line.text.contains("agent error: failed"))
        .unwrap();
    assert!(
        error_line.style_spans.iter().any(|span| {
            span.start == 0
                && span.rendition.foreground == Some(theme.colors.agent_transcript_error.foreground)
                && span.rendition.background.is_none()
                && span.rendition.bold
        }),
        "error transcript lines should use themed error foreground without a background: {:?}",
        error_line.style_spans
    );
    let command_line = styled_lines
        .iter()
        .find(|line| line.text.contains("$ ls -la"))
        .unwrap();
    assert!(
        command_line.style_spans.iter().any(|span| {
            span.start == 0
                && span.rendition.foreground
                    == Some(theme.colors.agent_transcript_command.foreground)
                && span.rendition.background.is_none()
                && span.rendition.bold
        }),
        "command transcript lines should use themed command foreground without a background: {:?}",
        command_line.style_spans
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies visible-pane user prompt transcript lines wrap to the bounded pane
/// width with a sixth-column hanging indent for continuation rows.
///
/// Long user-entered transcript lines should use the same bounded renderer as
/// other visible pane logs so they stay within the pane width or the 120-column
/// cap. Wrapped continuation rows align with the `mez> ` continuation column
/// instead of repeating the `user> ` label so the copied transcript remains
/// readable.
#[test]
fn runtime_user_prompt_logs_wrap_with_sixth_column_hanging_indent() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(24, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(24, 12).unwrap(), 100).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    service
        .append_agent_user_prompt_to_terminal_buffer("%1", "alpha beta gamma delta epsilon")
        .unwrap();

    let user_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .into_iter()
        .filter(|line| line.starts_with("▐ "))
        .collect::<Vec<_>>();
    assert!(
        user_lines.iter().any(|line| line == "▐ user> alpha beta"),
        "{user_lines:#?}"
    );
    assert!(
        user_lines.iter().any(|line| line == "▐      gamma delta"),
        "{user_lines:#?}"
    );
    assert!(
        user_lines.iter().any(|line| line == "▐      epsilon"),
        "{user_lines:#?}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies pasted provider diagnostics remain normal prompt text.
///
/// Users often paste the previous terminal failure back into the agent shell for
/// diagnosis. That text can contain JSON error payloads, wrapped words, and the
/// provider_error marker, but it is still user-authored prompt content. The
/// runtime should render it through the agent transcript presentation path
/// without surfacing a secondary terminal presentation failure.
#[test]
fn runtime_agent_user_prompt_renders_pasted_provider_error_without_terminal_failure() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 12).unwrap(), 120)
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 12).unwrap(), 120).unwrap(),
    );
    let prompt = "provider_error: InvalidState: OpenAI Responses-compatible provider `lmstudio` is not authenticated\nInvalidState: terminal step failed: {\"code\":-32004,\n\"data\":{\"mezzanine_code\":\"invalid_state\"},\"message\":\"agent terminal presentation feed panicked while appending styled agent\n lines\"}";

    service
        .append_agent_user_prompt_to_terminal_buffer("%1", prompt)
        .unwrap();

    let pane_text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("provider_error: InvalidState"),
        "{pane_text}"
    );
    assert!(pane_text.contains("terminal step failed"), "{pane_text}");
}
