//! Agent shell commands tests.

use super::*;

/// Builds active-pane bootstrap evidence with optional Rust toolchain roots.
fn toolchain_environment(environment_managers: Vec<String>) -> mez_agent::EnvironmentSignature {
    mez_agent::EnvironmentSignature::new(
        "linux",
        "x86_64",
        None,
        "test-host",
        "test-user",
        "/bin/sh",
        mez_agent::ShellClassification::PosixSh,
        None,
        Some("/usr/bin:/bin".to_string()),
        "/workspace",
        Some("/workspace".to_string()),
        true,
        None,
        environment_managers,
    )
    .unwrap()
}

/// Creates a visible primary agent shell backed by one disk config layer.
fn toolchain_command_service(
    name: &str,
    config_text: &str,
) -> (RuntimeSessionService, mez_core::ids::ClientId, PathBuf) {
    let root = temp_root(name);
    fs::create_dir_all(&root).unwrap();
    let path = root.join("config.toml");
    fs::write(&path, config_text).unwrap();
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: Some(path.clone()),
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: config_text.to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    (service, primary, path)
}

/// Verifies `/toolchain` status and detection consume only active-pane
/// bootstrap evidence and do not mutate config text or generation state.
#[test]
fn runtime_agent_shell_toolchain_status_and_detect_are_read_only() {
    let config =
        "[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = []\n";
    let (mut service, primary, path) =
        toolchain_command_service("runtime-toolchain-read-only", config);
    service.set_pane_environment_signature_for_tests(
        "%1",
        toolchain_environment(vec![
            "cargo-bin:/home/test-user/.cargo/bin".to_string(),
            "rustup:/home/test-user/.rustup".to_string(),
        ]),
    );
    let generation = service.session.config_generation;
    let before = fs::read_to_string(&path).unwrap();

    let status = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"toolchain-status","method":"agent/shell/command","params":{"idempotency_key":"toolchain-status","input":"/toolchain"}}"#,
        &primary,
    );
    let detect = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"toolchain-detect","method":"agent/shell/command","params":{"idempotency_key":"toolchain-detect","input":"/toolchain detect rust"}}"#,
        &primary,
    );

    assert!(status.contains(r#""command":"toolchain""#), "{status}");
    assert!(status.contains("effective=available-disabled"), "{status}");
    assert!(status.contains("discovery=available"), "{status}");
    assert!(detect.contains("operation=detect"), "{detect}");
    assert!(detect.contains("available=true"), "{detect}");
    assert!(detect.contains("source=active-pane-bootstrap"), "{detect}");
    assert_eq!(service.session.config_generation, generation);
    assert_eq!(fs::read_to_string(&path).unwrap(), before);
    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// Verifies confirmed enable and disable persist only the typed kind, hot-apply
/// to subsequent actions, and advance generation exactly once per real change.
#[test]
fn runtime_agent_shell_toolchain_enable_disable_and_no_op_are_generation_exact() {
    let config =
        "[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = []\n";
    let (mut service, primary, path) =
        toolchain_command_service("runtime-toolchain-mutation", config);
    service.set_pane_environment_signature_for_tests(
        "%1",
        toolchain_environment(vec![
            "cargo-bin:/home/test-user/.cargo/bin".to_string(),
            "rustup:/home/test-user/.rustup".to_string(),
        ]),
    );
    let initial_generation = service.session.config_generation;

    let missing_confirmation = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"toolchain-unconfirmed","method":"agent/shell/command","params":{"idempotency_key":"toolchain-unconfirmed","input":"/toolchain enable rust"}}"#,
        &primary,
    );
    assert!(
        missing_confirmation.contains("expects status, list, detect"),
        "{missing_confirmation}"
    );
    assert_eq!(service.session.config_generation, initial_generation);

    let enabled = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"toolchain-enable","method":"agent/shell/command","params":{"idempotency_key":"toolchain-enable","input":"/toolchain enable rust --yes"}}"#,
        &primary,
    );
    assert!(enabled.contains(r#""kind":"mutated""#), "{enabled}");
    assert!(enabled.contains("changed=true"), "{enabled}");
    assert!(enabled.contains("persisted_kind_only=true"), "{enabled}");
    assert_eq!(service.session.config_generation, initial_generation + 1);
    let persisted = fs::read_to_string(&path).unwrap();
    assert!(persisted.contains("toolchains = [\"rust\"]"), "{persisted}");
    assert!(!persisted.contains("/home/test-user"), "{persisted}");

    let enabled_again = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"toolchain-enable-again","method":"agent/shell/command","params":{"idempotency_key":"toolchain-enable-again","input":"/toolchain enable rust --yes"}}"#,
        &primary,
    );
    assert!(enabled_again.contains("changed=false"), "{enabled_again}");
    assert_eq!(service.session.config_generation, initial_generation + 1);

    let disabled = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"toolchain-disable","method":"agent/shell/command","params":{"idempotency_key":"toolchain-disable","input":"/toolchain disable rust --yes"}}"#,
        &primary,
    );
    assert!(disabled.contains("changed=true"), "{disabled}");
    assert_eq!(service.session.config_generation, initial_generation + 2);
    assert!(
        fs::read_to_string(&path)
            .unwrap()
            .contains("toolchains = []")
    );
    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// Confirmed Zig detection and enablement consume only active-pane bootstrap
/// evidence and persist no discovered host path into runtime configuration.
#[test]
fn runtime_agent_shell_zig_toolchain_detects_and_persists_only_kind() {
    let config =
        "[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = []\n";
    let (mut service, primary, path) =
        toolchain_command_service("runtime-zig-toolchain-mutation", config);
    let zig_root = path.parent().unwrap().join("zig-0.14.0");
    fs::create_dir_all(zig_root.join("lib")).unwrap();
    fs::write(zig_root.join("zig"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(zig_root.join("zig"), fs::Permissions::from_mode(0o755)).unwrap();
    let zig_root = zig_root.canonicalize().unwrap();
    service.set_pane_environment_signature_for_tests(
        "%1",
        toolchain_environment(vec![format!("zig:{}", zig_root.display())]),
    );

    let detect = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"zig-detect","method":"agent/shell/command","params":{"idempotency_key":"zig-detect","input":"/toolchain detect zig"}}"#,
        &primary,
    );
    assert!(detect.contains("kind=zig"), "{detect}");
    assert!(detect.contains("available=true"), "{detect}");

    let enabled = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"zig-enable","method":"agent/shell/command","params":{"idempotency_key":"zig-enable","input":"/toolchain enable zig --yes"}}"#,
        &primary,
    );
    assert!(enabled.contains("kind=zig"), "{enabled}");
    assert!(enabled.contains("changed=true"), "{enabled}");
    let persisted = fs::read_to_string(&path).unwrap();
    assert!(persisted.contains("toolchains = [\"zig\"]"), "{persisted}");
    assert!(
        !persisted.contains(&zig_root.to_string_lossy().into_owned()),
        "{persisted}"
    );

    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// Confirmed Go detection and enablement consume only active-pane SDK
/// evidence and never persist the discovered host root, GOPATH, or GOBIN.
#[test]
fn runtime_agent_shell_go_toolchain_detects_and_persists_only_kind() {
    let config =
        "[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = []\n";
    let (mut service, primary, path) =
        toolchain_command_service("runtime-go-toolchain-mutation", config);
    let go_root = path.parent().unwrap().join("go-sdk");
    fs::create_dir_all(go_root.join("bin")).unwrap();
    fs::create_dir_all(go_root.join("src")).unwrap();
    fs::write(go_root.join("bin/go"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(go_root.join("bin/go"), fs::Permissions::from_mode(0o755)).unwrap();
    let go_root = go_root.canonicalize().unwrap();
    service.set_pane_environment_signature_for_tests(
        "%1",
        toolchain_environment(vec![format!("go:{}", go_root.display())]),
    );

    let detect = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"go-detect","method":"agent/shell/command","params":{"idempotency_key":"go-detect","input":"/toolchain detect go"}}"#,
        &primary,
    );
    assert!(detect.contains("kind=go"), "{detect}");
    assert!(detect.contains("available=true"), "{detect}");

    let enabled = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"go-enable","method":"agent/shell/command","params":{"idempotency_key":"go-enable","input":"/toolchain enable go --yes"}}"#,
        &primary,
    );
    assert!(enabled.contains("kind=go"), "{enabled}");
    assert!(enabled.contains("changed=true"), "{enabled}");
    let persisted = fs::read_to_string(&path).unwrap();
    assert!(persisted.contains("toolchains = [\"go\"]"), "{persisted}");
    assert!(
        !persisted.contains(&go_root.to_string_lossy().into_owned()),
        "{persisted}"
    );

    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// Verifies `/toolchain reload` invokes the full disk-backed config reload and
/// reports before/after typed state rather than applying only one field.
#[test]
fn runtime_agent_shell_toolchain_reload_reapplies_full_disk_config() {
    let config = "[history]\nlines = 7\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = []\n";
    let (mut service, primary, path) =
        toolchain_command_service("runtime-toolchain-reload", config);
    fs::write(
        &path,
        "[history]\nlines = 13\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = [\"rust\"]\n",
    )
    .unwrap();

    let reload = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"toolchain-reload","method":"agent/shell/command","params":{"idempotency_key":"toolchain-reload","input":"/toolchain reload"}}"#,
        &primary,
    );

    assert!(reload.contains("full_config_reload=true"), "{reload}");
    assert!(reload.contains("before_configured=none"), "{reload}");
    assert!(reload.contains("after_configured=rust"), "{reload}");
    assert_eq!(service.terminal_history_limit(), 13);
    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// Verifies the runtime executor defensively rejects a non-primary caller even
/// when the caller bypasses the ordinary JSON-RPC authorization boundary.
#[test]
fn runtime_agent_shell_toolchain_rejects_non_primary_client() {
    let config =
        "[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = []\n";
    let (mut service, _primary, path) =
        toolchain_command_service("runtime-toolchain-non-primary", config);
    let non_primary = mez_core::ids::ClientId::opaque("c-observer").unwrap();

    let error = service
        .execute_agent_shell_command(&non_primary, "/toolchain status")
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
    assert!(error.message().contains("primary client"), "{error}");
    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// Verifies durable toolchain audit records retain typed operation and
/// generation metadata without persisting bootstrap-derived host roots.
#[test]
fn runtime_agent_shell_toolchain_audit_redacts_discovered_roots() {
    let config =
        "[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = []\n";
    let (mut service, primary, path) = toolchain_command_service("runtime-toolchain-audit", config);
    let audit_path = path.parent().unwrap().join("audit.jsonl");
    service.set_audit_log(AuditLog::new(crate::security::audit::AuditConfig {
        enabled: true,
        path: audit_path.clone(),
        hash_chain: false,
        required: true,
    }));
    service.set_pane_environment_signature_for_tests(
        "%1",
        toolchain_environment(vec![
            "cargo-bin:/private/toolchains/.cargo/bin".to_string(),
            "rustup:/private/toolchains/.rustup".to_string(),
        ]),
    );

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"toolchain-audit","method":"agent/shell/command","params":{"idempotency_key":"toolchain-audit","input":"/toolchain detect rust"}}"#,
        &primary,
    );

    assert!(response.contains("available=true"), "{response}");
    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(audit.contains(r#""event_type":"toolchain""#), "{audit}");
    assert!(audit.contains(r#""action":"detect""#), "{audit}");
    assert!(audit.contains(r#""kind":"rust""#), "{audit}");
    assert!(audit.contains("config_generation"), "{audit}");
    assert!(!audit.contains("/private/toolchains"), "{audit}");
    assert!(!audit.contains("cargo_bin"), "{audit}");
    assert!(!audit.contains("rustup_home"), "{audit}");
    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// Verifies that the runtime `agent/shell/command` `/list-mcp` path uses the live
/// MCP registry and exposes unavailable or session-blacklisted details. This
/// protects the spec requirement that agent-shell MCP visibility match control
/// and command surfaces instead of returning a generic runtime placeholder.
#[test]
fn runtime_agent_shell_mcp_command_reports_live_registry_detail() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .mcp_registry_mut()
        .add_server(mez_agent::mcp::McpServerConfig::stdio(
            "fs",
            "filesystem",
            "mcp-fs",
            Vec::new(),
        ))
        .unwrap();
    service
        .mcp_registry_mut()
        .mark_available(
            "fs",
            vec![mez_agent::mcp::McpToolState {
                server_id: String::new(),
                name: "read_file".to_string(),
                available: true,
                blacklisted: false,
                permission_required: true,
                effects: mez_agent::mcp::McpToolEffects::none(),
                approval: mez_agent::mcp::McpApprovalSetting::Inherit,
                description: "read a file".to_string(),
                input_schema_json: "{}".to_string(),
            }],
            1,
        )
        .unwrap();
    service
        .mcp_registry_mut()
        .blacklist_for_session("fs", "failed handshake", 1)
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-mcp","method":"agent/shell/command","params":{"idempotency_key":"agent-mcp","input":"/list-mcp"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(response.contains(r#""command":"list-mcp""#), "{response}");
    assert!(response.contains("## MCP Servers"), "{response}");
    assert!(response.contains("Servers: 1"), "{response}");
    assert!(response.contains("Tools: 1"), "{response}");
    assert!(response.contains("Source: runtime-mcp"), "{response}");
    assert!(response.contains("### `fs` - filesystem"), "{response}");
    assert!(response.contains("- State: blacklisted"), "{response}");
    assert!(
        response.contains("- Session blacklisted: true"),
        "{response}"
    );
    assert!(response.contains("- Retryable: true"), "{response}");
    assert!(
        response.contains("- Reason: failed handshake"),
        "{response}"
    );
    assert!(
        response.contains("| `read_file` | blacklisted |"),
        "{response}"
    );
    assert!(!response.contains("requires_runtime"), "{response}");
}

/// Verifies that `/status` is backed by live runtime state rather than only
/// the shell session fallback. The status view is a user-visible conformance
/// surface, so it must include model selection, policy, identity, writable
/// scope state, current context tracking, and provider token counters in one
/// response.
#[test]
fn runtime_agent_shell_status_reports_live_runtime_state() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-fast\"]\ndefault_model = \"gpt-fast\"\n\n[permissions]\npreset = \"auto\"\napproval_policy = \"full-access\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let second_pane = service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some("cat >/dev/null"))
        .unwrap()
        .pane_id;
    service.session.select_pane(&primary, "%1").unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume(second_pane.as_str())
        .unwrap();
    service.record_agent_provider_token_usage(
        "%1",
        mez_agent::ModelTokenUsage {
            input_tokens: 120,
            output_tokens: 34,
            reasoning_tokens: 9,
            cached_input_tokens: Some(80),
            cache_write_input_tokens: None,
        },
    );
    service.record_agent_provider_token_usage(
        "%1",
        mez_agent::ModelTokenUsage {
            input_tokens: 40,
            output_tokens: 0,
            reasoning_tokens: 0,
            cached_input_tokens: None,
            cache_write_input_tokens: None,
        },
    );
    let deepseek_profile = runtime_model_profile("deepseek", "deepseek-chat");
    service.record_agent_provider_token_usage_with_profile(
        "%1",
        mez_agent::ModelTokenUsage {
            input_tokens: 200,
            output_tokens: 50,
            reasoning_tokens: 20,
            cached_input_tokens: Some(100),
            cache_write_input_tokens: None,
        },
        mez_agent::ModelTokenUsage {
            input_tokens: 200,
            output_tokens: 50,
            reasoning_tokens: 20,
            cached_input_tokens: Some(100),
            cache_write_input_tokens: None,
        },
        Some(&deepseek_profile),
    );
    service.record_agent_provider_token_usage(
        second_pane.as_str(),
        mez_agent::ModelTokenUsage {
            input_tokens: 60,
            output_tokens: 10,
            reasoning_tokens: 4,
            cached_input_tokens: Some(30),
            cache_write_input_tokens: None,
        },
    );
    service
        .integration
        .runtime_metrics_mut()
        .record_provider_token_usage(
            mez_agent::ModelTokenUsage {
                input_tokens: 300,
                output_tokens: 75,
                reasoning_tokens: 15,
                cached_input_tokens: Some(120),
                cache_write_input_tokens: None,
            },
            mez_agent::ModelTokenUsage {
                input_tokens: 300,
                output_tokens: 75,
                reasoning_tokens: 15,
                cached_input_tokens: Some(120),
                cache_write_input_tokens: None,
            },
            &mez_agent::ModelTokenUsageKey::new("runtime-metrics", "metrics-only"),
        );
    service
        .register_subagent_write_scopes_for_tests(
            "agent-%1",
            CooperationMode::OwnedWrite,
            &["src".to_string()],
            None,
        )
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "summarize the pane")
        .unwrap();
    assert_eq!(started.state, AgentTurnState::Running);

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-status","method":"agent/shell/command","params":{"idempotency_key":"agent-status","input":"/status"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(response.contains(r#""command":"status""#), "{response}");
    assert!(
        response.contains(r#""content_type":"text/markdown; charset=utf-8""#),
        "{response}"
    );
    assert!(response.contains("## Agent Status"), "{response}");
    assert!(response.contains("| Field | Value |"), "{response}");
    assert!(response.contains("| Agent id | agent-%1 |"), "{response}");
    assert!(response.contains("| Window id | @1 |"), "{response}");
    assert!(
        response.contains("| Model | gpt-fast via openai (profile: default"),
        "{response}"
    );
    assert!(
        response.contains("| Prompt profile | default v32 |"),
        "{response}"
    );
    assert!(
        response.contains(
            "| Permissions | preset auto (session-config; owner none), approval full-access (session-config; owner none), bypass false (session) |"
        ),
        "{response}"
    );
    assert!(
        response.contains("| src | agent-%1 | owned-write |"),
        "{response}"
    );
    assert!(response.contains("| Context | 1 blocks"), "{response}");
    assert!(
        response.contains("| Pane agent tokens | 2 models; see Pane Agent Token Usage |"),
        "{response}"
    );
    assert!(
        response.contains("### Pane Agent Token Usage"),
        "{response}"
    );
    assert!(
        response.contains("| Cumulative cache hit | unknown |"),
        "{response}"
    );
    assert!(
        response.contains(
            "| Latest request cache hit | 50.00% (deepseek-chat via deepseek; cached_input=100 input=200) |"
        ),
        "{response}"
    );
    assert!(
        response.contains("| Cumulative Cache Hit % |"),
        "{response}"
    );
    let session_heading = response
        .find("### Pane Agent Token Usage")
        .expect("session token usage heading should be present");
    let instance_heading = response
        .find("### Mez Session Token Usage")
        .expect("instance token usage heading should be present");
    assert!(session_heading < instance_heading, "{response}");
    assert!(
        response.contains("| openai | gpt-fast | 160 | unknown | 34 | 9 | unknown |"),
        "{response}"
    );
    assert!(
        response.contains("| deepseek | deepseek-chat | 100 | 100 | 50 | 20 | 50.00% |"),
        "{response}"
    );
    assert!(
        response.contains("| openai | gpt-fast | 220 | unknown | 44 | 13 | unknown |"),
        "{response}"
    );
    assert!(
        !response.contains("| runtime-metrics | metrics-only |"),
        "{response}"
    );
    assert!(!response.contains("Provider rate limits"), "{response}");
    assert!(!response.contains("### Quota Usage"), "{response}");
    assert!(
        response.contains("| Latest turn | turn-1 (running) |"),
        "{response}"
    );
    assert!(!response.contains("requires_runtime"), "{response}");

    let session_usage_before_reset = service.total_agent_token_usage_by_model();
    let second_pane_usage_before_reset = service.agent_token_usage_for_pane(second_pane.as_str());
    let reset_response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-reset-status","method":"agent/shell/command","params":{"idempotency_key":"agent-reset-status","input":"/reset-status"}}"#,
        &primary,
    );

    assert!(
        reset_response.contains(r#""kind":"mutated""#),
        "{reset_response}"
    );
    assert!(
        reset_response.contains(r#""command":"reset-status""#),
        "{reset_response}"
    );
    assert!(
        reset_response.contains("pane_token_usage_reset=true changed=true"),
        "{reset_response}"
    );
    assert!(service.agent_token_usage_for_pane("%1").is_empty());
    assert_eq!(
        service.agent_token_usage_for_pane(second_pane.as_str()),
        second_pane_usage_before_reset
    );
    assert_eq!(
        service.total_agent_token_usage_by_model(),
        session_usage_before_reset
    );
}

/// Verifies that `/diff` reads the active pane's Git repository and includes
/// both modified tracked content and untracked files. This covers the spec
/// requirement that the agent shell diff view expose the working tree rather
/// than returning a generic runtime-required placeholder.
#[test]
fn runtime_agent_shell_diff_reports_git_worktree_and_untracked_files() {
    let root = temp_root("runtime-agent-diff");
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init"]);
    fs::write(root.join("tracked.txt"), "before\n").unwrap();
    git(&["add", "tracked.txt"]);
    fs::write(root.join("tracked.txt"), "before\nafter\n").unwrap();
    fs::write(root.join("new.txt"), "untracked\n").unwrap();

    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let descriptor = service.initial_pane_descriptor().unwrap();
    service
        .start_pane_process_with_start_directory(descriptor, Some("sleep 30"), Some(&root))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-diff","method":"agent/shell/command","params":{"idempotency_key":"agent-diff","input":"/diff"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(response.contains(r#""command":"diff""#), "{response}");
    assert!(response.contains("source=runtime-vcs-diff"), "{response}");
    assert!(response.contains("untracked_files=1"), "{response}");
    assert!(response.contains("tracked.txt"), "{response}");
    assert!(response.contains("+after"), "{response}");
    assert!(response.contains("file=new.txt"), "{response}");
    assert!(response.contains("+untracked"), "{response}");
    assert!(!response.contains("requires_runtime"), "{response}");
    service.kill_session(&primary, true).unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies that `/init` creates a project instruction scaffold in the active
/// pane's working directory and leaves an existing scaffold intact. This covers
/// the baseline file-mutation slash command without writing to the repository
/// root used by the test harness.
#[test]
fn runtime_agent_shell_init_creates_project_instruction_scaffold() {
    let root = temp_root("runtime-agent-init");
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let descriptor = service.initial_pane_descriptor().unwrap();
    service
        .start_pane_process_with_start_directory(descriptor, Some("sleep 30"), Some(&root))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-init","method":"agent/shell/command","params":{"idempotency_key":"agent-init","input":"/init"}}"#,
        &primary,
    );

    let scaffold = root.join("AGENTS.md");
    assert!(response.contains(r#""kind":"mutated""#), "{response}");
    assert!(response.contains(r#""command":"init""#), "{response}");
    assert!(response.contains("created=true"), "{response}");
    assert!(response.contains("source=runtime-init"), "{response}");
    assert!(!response.contains("requires_runtime"), "{response}");
    let text = fs::read_to_string(&scaffold).unwrap();
    assert!(text.contains("# Repository Guidelines"), "{text}");
    assert!(
        text.contains("## Build, Test, and Development Commands"),
        "{text}"
    );

    let existing = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-init-existing","method":"agent/shell/command","params":{"idempotency_key":"agent-init-existing","input":"/init"}}"#,
        &primary,
    );

    assert!(existing.contains(r#""kind":"display""#), "{existing}");
    assert!(existing.contains(r#""command":"init""#), "{existing}");
    assert!(existing.contains("created=false"), "{existing}");
    assert!(existing.contains("existing=true"), "{existing}");
    assert!(!existing.contains("requires_runtime"), "{existing}");
    service.kill_session(&primary, true).unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies that `/logout` executes through the runtime auth store and removes
/// stored credentials without exposing a duplicate terminal logout command.
#[test]
fn runtime_agent_shell_logout_uses_attached_auth_store() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-agent-logout");
    let auth_store = AuthStore::new(crate::security::auth::AuthPaths::under_config_root(&root));
    auth_store
        .login_provider_api_key_with_selected_store(
            "openai",
            "work",
            "sk-runtime-secret",
            Some("file"),
        )
        .unwrap();
    service.set_auth_store(auth_store);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-logout","method":"agent/shell/command","params":{"idempotency_key":"agent-logout","input":"/logout"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"mutated""#), "{response}");
    assert!(response.contains(r#""command":"logout""#), "{response}");
    assert!(response.contains("logged_out=true"), "{response}");
    assert!(!response.contains("requires_runtime"), "{response}");
    assert!(!response.contains("sk-runtime-secret"), "{response}");
    let status = service
        .execute_agent_shell_command(&primary, "/auth-status")
        .unwrap();
    assert!(status.contains("authenticated=false"), "{status}");
    assert!(status.contains("Authentication Status"), "{status}");
    let _ = fs::remove_dir_all(root);
}

/// Verifies `/approval` changes only the issuing pane while preserving the
/// configured session policy as the baseline for unrelated panes.
#[test]
fn runtime_agent_shell_approval_command_mutates_live_policy() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-permissions","method":"agent/shell/command","params":{"idempotency_key":"agent-permissions","input":"/approval full-access"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"mutated""#), "{response}");
    assert!(response.contains(r#""command":"approval""#), "{response}");
    assert!(response.contains("field=approval_policy"), "{response}");
    assert!(response.contains("requested=full-access"), "{response}");
    assert!(response.contains("changed=true"), "{response}");
    assert!(!response.contains("requires_runtime"), "{response}");
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::Ask
    );
    assert_eq!(
        service.permission_policy_for_pane("%1").approval_policy,
        ApprovalPolicy::FullAccess
    );
}

/// Verifies pane-local permission commands do not leak preset or approval
/// changes into an unrelated root pane or the configured session baseline.
#[test]
fn runtime_agent_shell_permission_commands_isolate_root_panes() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let second_pane = service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some("cat >/dev/null"))
        .unwrap()
        .pane_id;
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume(second_pane.as_str())
        .unwrap();
    service.session.select_pane(&primary, "%1").unwrap();

    let approval = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"pane-approval","method":"agent/shell/command","params":{"idempotency_key":"pane-approval","input":"/approval full-access"}}"#,
        &primary,
    );
    let preset = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"pane-preset","method":"agent/shell/command","params":{"idempotency_key":"pane-preset","input":"/permissions preset auto"}}"#,
        &primary,
    );

    assert!(approval.contains("changed=true"), "{approval}");
    assert!(preset.contains("changed=true"), "{preset}");
    assert_eq!(
        service.permission_policy_for_pane("%1").approval_policy,
        ApprovalPolicy::FullAccess
    );
    assert_eq!(
        service.permission_policy_for_pane("%1").preset,
        mez_agent::PermissionPreset::Auto
    );
    assert_eq!(
        service
            .permission_policy_for_pane(second_pane.as_str())
            .approval_policy,
        ApprovalPolicy::Ask
    );
    assert_eq!(
        service
            .permission_policy_for_pane(second_pane.as_str())
            .preset,
        mez_agent::PermissionPreset::ReadOnly
    );
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::Ask
    );
    assert_eq!(
        service.permission_policy().preset,
        mez_agent::PermissionPreset::ReadOnly
    );
    let frame_context = service.terminal_frame_context();
    assert_eq!(
        frame_context
            .panes
            .get("%1")
            .and_then(|context| context.policy_mode.as_deref()),
        Some("full-access")
    );
    assert_eq!(
        frame_context
            .panes
            .get(second_pane.as_str())
            .and_then(|context| context.policy_mode.as_deref()),
        Some("ask")
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies active descendants resolve parent changes dynamically, child
/// field overrides shadow only their subtree, and clearing restores inheritance.
#[test]
fn runtime_pane_permission_overrides_inherit_and_shadow_by_field() {
    let mut service = test_runtime_service();
    service.set_subagent_lineage(
        "agent-%2",
        RuntimeSubagentLineage {
            parent_agent_id: "agent-%1".to_string(),
            root_agent_id: "agent-%1".to_string(),
            depth: 1,
            display_name: "child".to_string(),
        },
    );
    service.set_subagent_lineage(
        "agent-%3",
        RuntimeSubagentLineage {
            parent_agent_id: "agent-%2".to_string(),
            root_agent_id: "agent-%1".to_string(),
            depth: 2,
            display_name: "grandchild".to_string(),
        },
    );

    service.set_pane_permission_preset_override("%1", Some(mez_agent::PermissionPreset::Auto));
    service.set_pane_approval_policy_override("%1", Some(ApprovalPolicy::FullAccess));
    assert_eq!(
        service.permission_policy_for_agent("agent-%3").preset,
        mez_agent::PermissionPreset::Auto
    );
    assert_eq!(
        service
            .permission_policy_for_agent("agent-%3")
            .approval_policy,
        ApprovalPolicy::FullAccess
    );

    service.set_pane_approval_policy_override("%2", Some(ApprovalPolicy::Ask));
    let child = service.permission_policy_for_agent("agent-%2");
    let grandchild = service.permission_policy_for_agent("agent-%3");
    assert_eq!(child.preset, mez_agent::PermissionPreset::Auto);
    assert_eq!(child.approval_policy, ApprovalPolicy::Ask);
    assert_eq!(grandchild.preset, mez_agent::PermissionPreset::Auto);
    assert_eq!(grandchild.approval_policy, ApprovalPolicy::Ask);
    let grandchild_status = service.permission_policy_status_for_pane("%3");
    assert_eq!(
        grandchild_status.preset_source.source,
        "ancestor-pane-override"
    );
    assert_eq!(
        grandchild_status.preset_source.owner_pane_id.as_deref(),
        Some("%1")
    );
    assert_eq!(
        grandchild_status.approval_policy_source.source,
        "ancestor-pane-override"
    );
    assert_eq!(
        grandchild_status
            .approval_policy_source
            .owner_pane_id
            .as_deref(),
        Some("%2")
    );
    assert_eq!(
        service.permission_policy_for_pane("%1").approval_policy,
        ApprovalPolicy::FullAccess
    );

    service.set_pane_approval_policy_override("%2", None);
    assert_eq!(
        service
            .permission_policy_for_agent("agent-%3")
            .approval_policy,
        ApprovalPolicy::FullAccess
    );
    service.set_pane_permission_preset_override("%1", None);
    assert_eq!(
        service.permission_policy_for_agent("agent-%3").preset,
        mez_agent::PermissionPreset::ReadOnly
    );
    let grandchild_status = service.permission_policy_status_for_pane("%3");
    assert_eq!(grandchild_status.preset_source.source, "session-config");
    assert_eq!(grandchild_status.preset_source.owner_pane_id, None);
}

/// Verifies pane cleanup removes only that pane's explicit permission fields.
///
/// Closing one descendant must not erase an ancestor's independently owned
/// override or leave the removed pane carrying stale authority if its id is
/// queried before reuse.
#[test]
fn runtime_pane_permission_cleanup_isolated_to_removed_pane() {
    let mut service = test_runtime_service();
    service.set_pane_approval_policy_override("%1", Some(ApprovalPolicy::FullAccess));
    service.set_pane_approval_policy_override("%2", Some(ApprovalPolicy::HostAccess));

    service.cleanup_removed_pane_runtime_state("%2");

    assert_eq!(
        service.permission_policy_for_pane("%1").approval_policy,
        ApprovalPolicy::FullAccess
    );
    assert_eq!(
        service.permission_policy_for_pane("%2").approval_policy,
        ApprovalPolicy::Ask
    );
}

/// Verifies pane permission slash commands can clear explicit fields and
/// restore dynamic inheritance from the configured session baseline.
#[test]
fn runtime_agent_shell_permission_commands_clear_to_inherit() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    for (id, input) in [
        ("set-approval", "/approval full-access"),
        ("set-preset", "/permissions preset auto"),
        ("clear-approval", "/approval inherit"),
        ("clear-preset", "/permissions preset clear"),
    ] {
        let response = service.dispatch_runtime_control_body(
            &format!(
                r#"{{"jsonrpc":"2.0","id":"{id}","method":"agent/shell/command","params":{{"idempotency_key":"{id}","input":"{input}"}}}}"#
            ),
            &primary,
        );
        assert!(response.contains("changed=true"), "{response}");
    }

    let policy = service.permission_policy_for_pane("%1");
    assert_eq!(policy.approval_policy, ApprovalPolicy::Ask);
    assert_eq!(policy.preset, mez_agent::PermissionPreset::ReadOnly);
}

/// Verifies a configured subagent profile preset remains a non-broadenable
/// restriction after pane-subtree policy composition.
#[test]
fn runtime_subagent_profile_preset_restricts_pane_override() {
    let mut service = test_runtime_service();
    service.set_subagent_lineage(
        "agent-%2",
        RuntimeSubagentLineage {
            parent_agent_id: "agent-%1".to_string(),
            root_agent_id: "agent-%1".to_string(),
            depth: 1,
            display_name: "child".to_string(),
        },
    );
    service.set_subagent_scope_declaration(
        "agent-%2",
        mez_agent::SubagentScopeDeclaration {
            cooperation_mode: CooperationMode::ExploreOnly,
            current_directory: "/repo".to_string(),
            read_scopes: vec!["/repo".to_string()],
            write_scopes: Vec::new(),
            permission_preset: Some(mez_agent::PermissionPreset::ReadOnly),
        },
    );
    service.set_pane_permission_preset_override("%1", Some(mez_agent::PermissionPreset::Auto));
    service.set_pane_permission_preset_override("%2", Some(mez_agent::PermissionPreset::Auto));
    let turn = mez_agent::AgentTurnRecord {
        turn_id: "profile-restriction".to_string(),
        agent_id: "agent-%2".to_string(),
        pane_id: "%2".to_string(),
        trigger: mez_agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 1,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        state: mez_agent::AgentTurnState::Running,
        cooperation_mode: Some("explore-only".to_string()),
        initial_capability: None,
    };

    assert_eq!(
        service.permission_policy_for_turn(&turn).preset,
        mez_agent::PermissionPreset::ReadOnly
    );
}

/// Verifies malformed cyclic delegation lineage fails closed instead of
/// looping or retaining a broader pane override.
#[test]
fn runtime_pane_permission_override_cycle_fails_closed() {
    let mut service = test_runtime_service();
    service.set_pane_permission_preset_override("%1", Some(mez_agent::PermissionPreset::Auto));
    service.set_pane_approval_policy_override("%1", Some(ApprovalPolicy::HostAccess));
    service.set_subagent_lineage(
        "agent-%1",
        RuntimeSubagentLineage {
            parent_agent_id: "agent-%2".to_string(),
            root_agent_id: "agent-%1".to_string(),
            depth: 1,
            display_name: "first".to_string(),
        },
    );
    service.set_subagent_lineage(
        "agent-%2",
        RuntimeSubagentLineage {
            parent_agent_id: "agent-%1".to_string(),
            root_agent_id: "agent-%1".to_string(),
            depth: 2,
            display_name: "second".to_string(),
        },
    );

    let policy = service.permission_policy_for_agent("agent-%2");
    assert_eq!(policy.preset, mez_agent::PermissionPreset::ReadOnly);
    assert_eq!(policy.approval_policy, ApprovalPolicy::Ask);
}

/// Verifies only the attached primary user's pane command can select host
/// access without broadening the configured session baseline.
#[test]
fn runtime_agent_shell_approval_command_selects_host_access() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-host-access","method":"agent/shell/command","params":{"idempotency_key":"agent-host-access","input":"/approval host-access"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"mutated""#), "{response}");
    assert!(response.contains("requested=host-access"), "{response}");
    assert!(
        response.contains("authority_change=broadening"),
        "{response}"
    );
    assert!(
        response.contains("approved_by=primary-command"),
        "{response}"
    );
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::Ask
    );
    assert_eq!(
        service.permission_policy_for_pane("%1").approval_policy,
        ApprovalPolicy::HostAccess
    );
}

/// Verifies terse slash-command display output uses transient status feedback.
///
/// One-line status acknowledgements should stay out of the durable agent pane
/// transcript while still giving brief feedback in the window status bar.
#[test]
fn runtime_agent_shell_single_line_display_uses_transient_status_without_overlay() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"/approval\r".to_vec(),
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    assert!(service.primary_display_overlay().is_none());
    assert!(
        service
            .primary_error_status_overlay()
            .is_some_and(|message| message.contains("approval policy: ask")),
        "{:?}",
        service.primary_error_status_overlay()
    );
    let pane_text = service
        .pane_screen("%1")
        .map(|screen| screen.normal_content_lines().join("\n"))
        .unwrap_or_default();
    assert!(!pane_text.contains("approval policy: ask"), "{pane_text}");
    assert!(!pane_text.contains("source: runtime-policy"), "{pane_text}");
}

/// Verifies an explicit pane `/approval` choice survives unrelated configured
/// baseline reloads without becoming a session-global override.
///
/// This protects full-access mode from being silently reset when a config
/// reload reapplies an older `permissions.approval_policy` value.
#[test]
fn runtime_agent_shell_approval_command_survives_config_reload() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-approval-live-override");
    let path = root.join("config.toml");
    fs::write(
        &path,
        "[history]\nlines = 7\n[permissions]\napproval_policy = \"ask\"\n",
    )
    .unwrap();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: Some(path.clone()),
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: fs::read_to_string(&path).unwrap(),
        }])
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-approval","method":"agent/shell/command","params":{"idempotency_key":"agent-approval-live","input":"/approval full-access"}}"#,
        &primary,
    );

    assert!(response.contains("requested=full-access"), "{response}");
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::Ask
    );
    assert_eq!(
        service.permission_policy_for_pane("%1").approval_policy,
        ApprovalPolicy::FullAccess
    );

    fs::write(
        &path,
        "[history]\nlines = 11\n[permissions]\napproval_policy = \"ask\"\n",
    )
    .unwrap();
    let reload = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"reload-approval","method":"config/reload","params":{"idempotency_key":"reload-approval-live"}}"#,
        &primary,
    );

    assert!(reload.contains(r#""operation":"reload""#), "{reload}");
    assert_eq!(service.terminal_history_limit(), 11);
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::Ask
    );
    assert_eq!(
        service.permission_policy_for_pane("%1").approval_policy,
        ApprovalPolicy::FullAccess
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies that the removed `/statusline` command is rejected without
/// mutating the live pane status-line rendering fields.
#[test]
fn runtime_agent_shell_statusline_is_rejected_without_mutating_pane_frame_fields() {
    let mut service = test_runtime_service();
    let expected_frame_fields = service.pane_frame_visible_fields().to_vec();
    let expected_frame_template = service.pane_frame_template().to_string();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-statusline","method":"agent/shell/command","params":{"idempotency_key":"agent-statusline","input":"/statusline agent.status agent.model pane.mode"}}"#,
        &primary,
    );

    assert!(response.contains("unknown slash command"), "{response}");
    assert!(!response.contains(r#""kind":"mutated""#), "{response}");
    assert!(service.pane_frames_enabled());
    assert_eq!(service.pane_frame_visible_fields(), expected_frame_fields);
    assert_eq!(service.pane_frame_template(), expected_frame_template);
}

/// Verifies that `/title` reads and mutates the active runtime window title
/// through the live command path. This covers the agent shell title command
/// without allowing the slash surface to target or rename unrelated windows.
#[test]
fn runtime_agent_shell_title_displays_and_renames_active_window() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let display = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-title-display","method":"agent/shell/command","params":{"idempotency_key":"agent-title-display","input":"/title"}}"#,
        &primary,
    );

    assert!(display.contains(r#""kind":"display""#), "{display}");
    assert!(display.contains(r#""command":"title""#), "{display}");
    assert!(display.contains("source=runtime-title"), "{display}");
    assert!(display.contains("window_id=@1"), "{display}");
    assert!(display.contains("window_title=shell"), "{display}");
    assert!(display.contains("pane=%1"), "{display}");
    assert!(display.contains("pane_title=shell"), "{display}");
    assert!(!display.contains("requires_runtime"), "{display}");

    let rename = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-title-rename","method":"agent/shell/command","params":{"idempotency_key":"agent-title-rename","input":"/title build shell"}}"#,
        &primary,
    );

    assert!(rename.contains(r#""kind":"mutated""#), "{rename}");
    assert!(rename.contains(r#""command":"title""#), "{rename}");
    assert!(rename.contains("source=runtime-title"), "{rename}");
    assert!(rename.contains("changed=true"), "{rename}");
    assert!(rename.contains("window_title=build shell"), "{rename}");
    assert!(!rename.contains("requires_runtime"), "{rename}");
    assert_eq!(
        service.session().active_window().unwrap().name,
        "build shell"
    );
}

/// Verifies that `/debug-config` reports live effective configuration, layer
/// order, and policy diagnostics from runtime state instead of the generic
/// runtime-required slash placeholder.
#[test]
fn runtime_agent_shell_debug_config_reports_live_runtime_config() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[history]\nlines = 7\n[permissions]\npreset = \"auto\"\napproval_policy = \"full-access\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"debug-config","method":"agent/shell/command","params":{"idempotency_key":"debug-config","input":"/debug-config history.lines"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(
        response.contains(r#""command":"debug-config""#),
        "{response}"
    );
    assert!(response.contains("source=runtime-config"), "{response}");
    assert!(response.contains("layers=1"), "{response}");
    assert!(response.contains("applied_layers=1"), "{response}");
    assert!(response.contains("permission_preset=auto"), "{response}");
    assert!(
        response.contains("approval_policy=full-access"),
        "{response}"
    );
    assert!(response.contains("layer=primary"), "{response}");
    assert!(response.contains("scope=primary"), "{response}");
    assert!(response.contains("format=toml"), "{response}");
    assert!(response.contains("value path=history.lines"), "{response}");
    assert!(response.contains("value=7"), "{response}");
    assert!(!response.contains("requires_runtime"), "{response}");
}

/// Verifies that planning-time shell action failures stay visible without
/// exposing the exact command in the default pane buffer. The user still sees
/// the policy failure, while command details remain reserved for verbose or
/// trace mode.
#[test]
fn runtime_agent_shell_planning_failure_hides_command_by_default() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 12).unwrap(), 20).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().add_rule(
        mez_agent::permissions::CommandRule::new(["ls"], RuleDecision::Forbid, RuleMatch::Prefix)
            .unwrap(),
    );

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-failed-command","input":"list files"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap shell response".to_string(),
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
                    id: "shell-1".to_string(),
                    rationale: "list files".to_string(),
                    payload: mez_agent::AgentActionPayload::ShellCommand {
                        summary: "List files".to_string(),
                        command: "ls".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
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

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert_eq!(execution.action_results[0].status, ActionStatus::Denied);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: List files (shell command denied before execution"),
        "{pane_text}"
    );
    assert!(!pane_text.contains("before execution: ls"), "{pane_text}");
    assert!(!pane_text.contains("$ ls"), "{pane_text}");
    service.terminate_all_pane_processes().unwrap();
}
