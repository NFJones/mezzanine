//! Registered native navigation through real command and process owners.
//!
//! Generated local directories and a manager-owned sleeping root separate agent
//! navigation from pane-shell readiness. Workers use production preparation and
//! actor adoption; no provider, shell callback or navigation helper is involved.

use super::*;
use crate::runtime::{RuntimeAgentCommandAsyncOutcome, RuntimeAgentCommandAsyncWork};
use mez_agent::SpawnedShellExecutor as _;

/// Owns one local root and its manager-owned process until explicit settlement.
struct NavigationFixture {
    /// Runtime actor exercised through ordinary command dispatch.
    service: RuntimeSessionService,
    /// Authenticated primary used by the command ingress.
    primary: mez_core::ids::ClientId,
    /// Generated fixture ancestor and trusted project root.
    root: PathBuf,
    /// Actual pane-process launch directory.
    source: PathBuf,
    /// Literal metacharacter directory, never shell source.
    target: PathBuf,
}

impl NavigationFixture {
    /// Starts an owned foreground program so plain navigation cannot require an
    /// idle interactive shell. All config, trust and filesystem inputs are local.
    fn new(label: &str) -> Self {
        let root = temp_root(label);
        let source = root.join("source");
        let target = root.join("target $literal;[x]");
        for path in [
            &source,
            &target,
            &root.join(".git"),
            &root.join("config/skills"),
            &root.join("config/macros"),
            &root.join("home/nested"),
            &root.join("pane-home"),
        ] {
            fs::create_dir_all(path).unwrap();
        }
        let root = fs::canonicalize(root).unwrap();
        let source = fs::canonicalize(source).unwrap();
        let target = fs::canonicalize(target).unwrap();
        let mut service = test_runtime_service();
        service.set_server_environment_for_tests(vec![
            mez_mux::process::RawEnvironmentEntry {
                key: b"HOME".to_vec(),
                value: root.join("home").to_str().unwrap().as_bytes().to_vec(),
            },
            mez_mux::process::RawEnvironmentEntry {
                key: b"PATH".to_vec(),
                value: b"/usr/bin:/bin".to_vec(),
            },
        ]);
        service.set_config_root(root.join("config"));
        service.replace_config_layers(vec![ConfigLayer {
            name: "native-navigation".into(), path: None,
            format: ConfigFormat::Toml, scope: ConfigScope::Primary, trusted: true,
            text: "[agents]\nshell_mode = \"native\"\n[permissions]\nsandbox = \"policy-only\"\n[history]\nlines = 10\n".into(),
        }]).unwrap();
        let mut trust = ProjectTrustStore::default();
        trust
            .decide_at(root.clone(), TrustDecision::Trusted, None, 1)
            .unwrap();
        service.set_project_trust_store(trust, None);
        let primary = service
            .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        service
            .start_initial_pane_process_with_launch_context(
                Some("sleep 120"),
                Some(&source),
                Some(&[(
                    "HOME".into(),
                    root.join("pane-home").to_str().unwrap().into(),
                )]),
            )
            .unwrap();
        let pid = service.primary_pid_for_live_pane_process("%1").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if mez_mux::process::process_executable_identity_for_pid(pid).is_ok_and(|identity| {
                identity.executable_path.file_name() == Some(std::ffi::OsStr::new("sleep"))
            }) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "owned foreground fixture did not reach sleep: {:?}",
                service.poll_pane_processes()
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        Self {
            service,
            primary,
            root,
            source,
            target,
        }
    }

    /// Runs the registered command through acceptance, worker read and adoption.
    fn navigate(&mut self, input: &str) -> String {
        let accepted = self
            .service
            .execute_agent_shell_command(&self.primary, input)
            .unwrap();
        assert!(accepted.contains(r#""body":null"#), "{accepted}");
        self.service
            .run_pending_deferred_agent_command_for_tests()
            .unwrap()
            .unwrap()
    }

    /// Claims one accepted worker while retaining its original operation owner.
    fn claim(&mut self) -> RuntimeAgentCommandAsyncWork {
        self.service
            .execute_agent_shell_command(&self.primary, "/cd '../target $literal;[x]'")
            .unwrap();
        let mut pending = self.service.take_pending_deferred_agent_commands();
        assert_eq!(pending.len(), 1);
        let work = pending.remove(0);
        self.service
            .claim_agent_command_work(
                &work.primary_client_id,
                &work.pane_id,
                &work.command,
                &work.input,
                work.claim_generation,
                &work.conversation_id,
            )
            .unwrap()
            .unwrap()
    }

    /// Requires the fixture's manager-owned root to settle before removing data.
    fn finish(mut self) {
        let exits = self.service.terminate_all_pane_processes().unwrap();
        assert_eq!(exits.len(), 1);
        assert!(self.service.tracked_runtime_pane_process_ids().is_empty());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match fs::remove_dir_all(&self.root) {
                Ok(()) => break,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(error)
                    if error.kind() == std::io::ErrorKind::DirectoryNotEmpty
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => panic!(
                    "owned fixture directory cleanup failed: {error}; remaining={:?}",
                    fs::read_dir(&self.root).map(|entries| entries
                        .map(|entry| entry.map(|entry| entry.file_name()))
                        .collect::<Vec<_>>())
                ),
            }
        }
    }
}

impl Drop for NavigationFixture {
    /// Attempts only owned fixture cleanup if an assertion unwinds before finish.
    fn drop(&mut self) {
        let _ = self.service.terminate_all_pane_processes();
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Registered navigation must prepare target guidance/config and drive multiple
/// real native cwd/PWD launches, while the foreground pane process, daemon cwd,
/// another pane's observation and conversation remain unchanged. A later relative
/// move clears stale instructions after positively successful empty discovery.
#[test]
fn native_navigation_registered_command_moves_only_agent_and_followup_launches() {
    let mut fixture = NavigationFixture::new("native-navigation-launches");
    fs::write(fixture.target.join("AGENTS.md"), "target guidance").unwrap();
    fs::create_dir_all(fixture.target.join(".mezzanine")).unwrap();
    fs::write(
        fixture.target.join(".mezzanine/config.toml"),
        format!(
            "version = {}\n[history]\nlines = 17\n",
            crate::config::CURRENT_CONFIG_SCHEMA_VERSION
        ),
    )
    .unwrap();
    let daemon = std::env::current_dir().unwrap();
    let pid = fixture
        .service
        .primary_pid_for_live_pane_process("%1")
        .unwrap();
    let root_identity = mez_mux::process::process_executable_identity_for_pid(pid).unwrap();
    let conversation = fixture
        .service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    fixture
        .service
        .set_pane_current_working_directory("%2", fixture.root.clone());
    let result = fixture.navigate("/cd '../target $literal;[x]'");
    assert!(result.contains("agent working directory"), "{result}");
    assert_eq!(
        fixture.service.agent_effective_working_directory("%1"),
        Some(fixture.target.clone())
    );
    assert_eq!(fixture.service.terminal_history_limit(), 17);
    let snapshot = fixture
        .service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .native_directory_snapshot()
        .unwrap();
    assert_eq!(
        snapshot.instruction_files().unwrap()[0].content,
        "target guidance"
    );
    for _ in 0..2 {
        let context = fixture
            .service
            .native_agent_shell_context_for_pane("%1")
            .unwrap();
        let shell = context.shell_path().to_path_buf();
        let mut executor = crate::runtime::processes::SpawnedShellExecutor::new(context);
        let request = mez_agent::ShellExecutionRequest {
            action_id: "navigation-proof".into(),
            transaction: mez_agent::ShellTransaction::new(
                crate::runtime::runtime_random_marker_token("navigation-proof").unwrap(),
                "navigation-turn",
                "agent-%1",
                "%1",
                &shell,
                "pwd; printf '%s\\n' \"$PWD\"",
            )
            .unwrap(),
            timeout_ms: Some(5_000),
            interactive: false,
            stateful: false,
        };
        let output = executor.execute_shell(&request).unwrap();
        assert_eq!(output.exit_code, Some(0));
        assert_eq!(
            output.stdout.lines().collect::<Vec<_>>(),
            vec![fixture.target.to_str().unwrap(); 2]
        );
        assert_eq!(
            mez_mux::process::process_executable_identity_for_pid(pid),
            Ok(root_identity.clone()),
            "pane root changed during native launch; exits={:?}",
            fixture.service.poll_pane_processes()
        );
    }
    assert_eq!(
        fixture.service.primary_pid_for_live_pane_process("%1"),
        Some(pid)
    );
    assert_eq!(
        fixture
            .service
            .native_shell_context_for_pane("%1")
            .unwrap()
            .working_directory(),
        fixture.source
    );
    assert_eq!(
        fixture.service.pane_current_working_directory("%1"),
        Some(fixture.source.clone())
    );
    assert_eq!(
        fixture.service.pane_current_working_directory("%2"),
        Some(fixture.root.clone())
    );
    assert_eq!(std::env::current_dir().unwrap(), daemon);
    assert_eq!(
        mez_mux::process::current_working_directory_for_pid(pid),
        Some(fixture.source.clone())
    );
    assert_eq!(
        fixture
            .service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .session_id,
        conversation
    );
    assert!(fixture.service.pending_agent_provider_tasks().is_empty());
    let result = fixture.navigate("/cd ../source");
    assert!(result.contains("agent working directory"), "{result}");
    let snapshot = fixture
        .service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .native_directory_snapshot()
        .unwrap();
    assert_eq!(snapshot.directory(), fixture.source);
    assert!(snapshot.instruction_files().unwrap().is_empty());
    assert_eq!(fixture.service.terminal_history_limit(), 10);
    fixture.finish();
}

/// A prepared result cannot publish after Stop, and Native/Pane/Native ABA must
/// invalidate a result even when the final mode and cwd text match acceptance.
/// Neither case changes the old cwd or starts provider work.
#[test]
fn native_navigation_stop_and_mode_aba_discard_prepared_results() {
    let mut fixture = NavigationFixture::new("native-navigation-stale");
    for stop in [true, false] {
        let work = fixture.claim();
        let outcome = RuntimeSessionService::execute_deferred_agent_command(&work);
        assert!(matches!(
            outcome,
            RuntimeAgentCommandAsyncOutcome::NativeDirectory(_)
        ));
        let refused = fixture
            .service
            .execute_agent_shell_command(&fixture.primary, "new work while navigating")
            .unwrap();
        assert!(refused.contains("native navigation is active"), "{refused}");
        if stop {
            let result = fixture
                .service
                .execute_agent_shell_command(&fixture.primary, "/stop")
                .unwrap();
            assert!(result.contains("navigation cancelled"), "{result}");
            assert!(
                !fixture
                    .service
                    .complete_agent_command_work(&work, outcome)
                    .unwrap()
            );
        } else {
            fixture
                .service
                .set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Pane));
            fixture.service.set_agent_shell_mode_override(
                "%1",
                Some(crate::runtime::config::ShellMode::Native),
            );
            assert!(
                fixture
                    .service
                    .complete_agent_command_work(&work, outcome)
                    .unwrap()
            );
        }
        assert_eq!(
            fixture.service.agent_effective_working_directory("%1"),
            Some(fixture.source.clone())
        );
        assert!(
            fixture
                .service
                .agent_shell_store()
                .get("%1")
                .unwrap()
                .native_directory_snapshot()
                .is_none()
        );
        assert!(fixture.service.pending_agent_provider_tasks().is_empty());
        assert!(
            fixture
                .service
                .agent
                .native_directory_work("%1", work.claim_generation)
                .is_none()
        );
    }
    fixture.finish();
}

/// Pane mode and unsupported synchronization must reject before any root or
/// filesystem preparation. Ordinary native bad targets fail in the worker and
/// retain the prior cwd, instead of succeeding with unknown/partial discovery.
#[test]
fn native_navigation_rejects_modes_and_invalid_targets_without_mutation() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Pane));
    for input in ["/cd /", "/cd --sync-pane /"] {
        let response = service
            .execute_agent_shell_command(&primary, input)
            .unwrap();
        assert!(
            response.contains("only available in native shell mode"),
            "{response}"
        );
        assert!(service.take_pending_deferred_agent_commands().is_empty());
    }
    service.set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Native));
    let response = service
        .execute_agent_shell_command(&primary, "/cd --sync-pane /")
        .unwrap();
    assert!(response.contains("not implemented"), "{response}");
    assert!(service.take_pending_deferred_agent_commands().is_empty());
    let conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    service
        .begin_agent_command_claim("%1", &conversation)
        .unwrap();
    let response = service
        .execute_agent_shell_command(&primary, "/cd /")
        .unwrap();
    assert!(
        response.contains("history preparation is active"),
        "{response}"
    );
    assert!(service.take_pending_deferred_agent_commands().is_empty());
    service.agent.cancel_agent_command("%1");
    let mut fixture = NavigationFixture::new("native-navigation-invalid");
    fs::write(fixture.source.join("file"), "ordinary file").unwrap();
    for input in ["/cd missing", "/cd file"] {
        let response = fixture.navigate(input);
        assert!(response.contains("existing directory"), "{response}");
        assert_eq!(
            fixture.service.agent_effective_working_directory("%1"),
            Some(fixture.source.clone())
        );
        assert!(!fixture.service.native_directory_command_is_pending("%1"));
    }
    fixture.finish();
}

/// Home resolution uses the configured native workload's HOME, and symbolic-link
/// traversal publishes its physical target. Pane HOME and the test process's
/// actual HOME differ from the allowlisted server snapshot. Omitting HOME from
/// workload policy must fail without borrowing either alternate source.
#[test]
fn native_navigation_uses_workload_home_and_physical_literal_targets() {
    let mut fixture = NavigationFixture::new("native-navigation-home");
    assert_eq!(
        fixture
            .service
            .native_agent_shell_context_for_pane("%1")
            .unwrap()
            .workload_environment()
            .workload_value("HOME"),
        fixture.root.join("home").to_str(),
        "fixture native workload must carry its explicitly supplied HOME"
    );
    let response = fixture.navigate("/cd");
    assert!(response.contains("agent working directory"), "{response}");
    assert_eq!(
        fixture.service.agent_effective_working_directory("%1"),
        Some(fixture.root.join("home"))
    );
    let response = fixture.navigate("/cd ~/nested");
    assert!(response.contains("agent working directory"), "{response}");
    assert_eq!(
        fixture.service.agent_effective_working_directory("%1"),
        Some(fixture.root.join("home/nested"))
    );
    let link = fixture.root.join("home/nested/link");
    std::os::unix::fs::symlink(&fixture.target, &link).unwrap();
    let response = fixture.navigate("/cd link");
    assert!(response.contains("agent working directory"), "{response}");
    assert_eq!(
        fixture.service.agent_effective_working_directory("%1"),
        Some(fixture.target.clone())
    );
    fixture.service.replace_config_layers(vec![ConfigLayer {
        name: "navigation-no-home".into(), path: None, format: ConfigFormat::Toml, scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\nshell_mode = \"native\"\n[permissions]\nsandbox = \"policy-only\"\nenv_whitelist = [\"PATH\"]\n".into(),
    }]).unwrap();
    let response = fixture.navigate("/cd");
    assert!(
        response.contains("home directory is unavailable"),
        "{response}"
    );
    assert_eq!(
        fixture.service.agent_effective_working_directory("%1"),
        Some(fixture.target.clone())
    );
    let response = fixture.navigate("/cd ../source");
    assert!(response.contains("agent working directory"), "{response}");
    assert_eq!(
        fixture.service.agent_effective_working_directory("%1"),
        Some(fixture.source.clone())
    );
    fixture.finish();
}

/// A prepared target cannot survive replacement of its inode, a config-root
/// change outside the ordinary layer projection, or a same-text source cwd
/// generation change. Typed worker completion is never authority to rebase.
#[test]
fn native_navigation_fences_target_config_root_and_source_generation() {
    for change in ["target", "config-root", "source-generation"] {
        let mut fixture = NavigationFixture::new(&format!("native-navigation-fence-{change}"));
        let work = fixture.claim();
        let outcome = RuntimeSessionService::execute_deferred_agent_command(&work);
        assert!(matches!(
            outcome,
            RuntimeAgentCommandAsyncOutcome::NativeDirectory(_)
        ));
        match change {
            "target" => {
                fs::rename(&fixture.target, fixture.root.join("previous-target")).unwrap();
                fs::create_dir(&fixture.target).unwrap();
            }
            "config-root" => fixture
                .service
                .set_config_root(fixture.root.join("other-config")),
            _ => {
                fixture
                    .service
                    .agent_shell_store_mut()
                    .ensure_session("%1")
                    .unwrap()
                    .record_native_working_directory(fixture.source.clone(), 0)
                    .unwrap();
            }
        }
        assert!(
            fixture
                .service
                .complete_agent_command_work(&work, outcome)
                .unwrap()
        );
        assert_eq!(
            fixture.service.agent_effective_working_directory("%1"),
            Some(fixture.source.clone())
        );
        assert!(
            fixture
                .service
                .agent
                .native_directory_work("%1", work.claim_generation)
                .is_none()
        );
        fixture.finish();
    }
}

/// Choosing a trusted target is not permission to read beyond explicit policy
/// or a child's frozen grants. Both reject the complete navigation before cwd
/// publication, without leaking denied instruction content in the response.
#[test]
fn native_navigation_preserves_explicit_and_inherited_read_limits() {
    for child in [false, true] {
        let mut fixture = NavigationFixture::new(if child {
            "native-navigation-child-read"
        } else {
            "native-navigation-explicit-read"
        });
        fs::write(
            fixture.target.join("AGENTS.md"),
            "denied-navigation-guidance",
        )
        .unwrap();
        if child {
            fixture.service.set_subagent_scope_declaration(
                "agent-%1",
                mez_agent::SubagentScopeDeclaration {
                    cooperation_mode: mez_agent::CooperationMode::ExploreOnly,
                    approval_provenance: mez_agent::SubagentApprovalProvenance::Requested,
                    current_directory: fixture.source.to_str().unwrap().into(),
                    read_scopes: vec![fixture.source.to_str().unwrap().into()],
                    write_scopes: Vec::new(),
                    permission_preset: None,
                },
            );
        } else {
            fixture.service.replace_config_layers(vec![ConfigLayer {
                name: "explicit-navigation-read".into(), path: None, format: ConfigFormat::Toml, scope: ConfigScope::Primary, trusted: true,
                text: format!("[agents]\nshell_mode = \"native\"\n[permissions]\nsandbox = \"policy-only\"\nread_scopes = [{}]\n", serde_json::to_string(fixture.source.to_str().unwrap()).unwrap()),
            }]).unwrap();
        }
        let response = fixture.navigate("/cd '../target $literal;[x]'");
        assert!(response.contains("read scopes"), "{response}");
        assert!(!response.contains("denied-navigation-guidance"));
        assert_eq!(
            fixture.service.agent_effective_working_directory("%1"),
            Some(fixture.source.clone())
        );
        assert!(
            fixture
                .service
                .agent_shell_store()
                .get("%1")
                .unwrap()
                .native_directory_snapshot()
                .is_none()
        );
        fixture.finish();
    }
}
