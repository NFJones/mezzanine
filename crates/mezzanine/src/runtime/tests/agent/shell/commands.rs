//! Agent shell commands tests.

use std::collections::BTreeMap;

use super::*;
use crate::runtime::SandboxConfig;

/// Creates a visible primary agent shell backed by one disk configuration
/// layer for generic sandbox command tests.
fn sandbox_command_service(
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

/// Verifies durable history is queried only for `/status --extended`, uses the
/// existing accounting columns in deterministic window order, survives a pane
/// reset, and remains visible after constructing a new runtime on the store.
#[test]
fn runtime_agent_shell_extended_status_persists_rolling_token_usage() {
    let root = temp_root("runtime-agent-extended-status");
    let store = crate::storage::token_usage::TokenUsageStore::new(root.join("token-usage.sqlite"));
    store.initialize(0).unwrap();

    let mut service = test_runtime_service();
    service.set_token_usage_store(store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let profile = runtime_model_profile("openai", "gpt-durable");
    service.record_agent_provider_token_usage_with_profile(
        "%1",
        mez_agent::ModelTokenUsage {
            input_tokens: 100,
            output_tokens: 20,
            reasoning_tokens: 5,
            cached_input_tokens: Some(40),
            cache_write_input_tokens: None,
        },
        mez_agent::ModelTokenUsage::default(),
        Some(&profile),
    );
    service.record_agent_provider_token_usage_by_model(
        "%1",
        &BTreeMap::from([(
            mez_agent::ModelTokenUsageKey::new("router", "route-small"),
            mez_agent::ModelTokenUsage {
                input_tokens: 12,
                output_tokens: 2,
                reasoning_tokens: 0,
                cached_input_tokens: None,
                cache_write_input_tokens: None,
            },
        )]),
    );

    let plain = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"plain-status","method":"agent/shell/command","params":{"idempotency_key":"plain-status","input":"/status"}}"#,
        &primary,
    );
    assert!(!plain.contains("### 7-Day Token Usage"), "{plain}");
    assert!(
        !plain.contains("Rolling Token Usage Unavailable"),
        "{plain}"
    );

    let extended = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"extended-status","method":"agent/shell/command","params":{"idempotency_key":"extended-status","input":"/status --extended"}}"#,
        &primary,
    );
    let headings = [7, 30, 60, 90].map(|days| {
        extended
            .find(&format!("### {days}-Day Token Usage"))
            .expect("rolling window heading should be present")
    });
    assert!(
        headings.windows(2).all(|pair| pair[0] < pair[1]),
        "{extended}"
    );
    assert!(
        extended.contains(
            "| Provider | Model | Billed input | Cached input | Output | Reasoning | Cumulative Cache Hit % |"
        ),
        "{extended}"
    );
    assert!(
        extended.contains("| openai | gpt-durable | 60 | 40 | 20 | 5 | 40.00% |"),
        "{extended}"
    );
    assert!(
        extended.contains("| router | route-small | 12 | unknown | 2 | 0 | unknown |"),
        "{extended}"
    );

    service.reset_agent_token_usage_for_pane("%1");
    let after_reset = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"extended-after-reset","method":"agent/shell/command","params":{"idempotency_key":"extended-after-reset","input":"/status --extended"}}"#,
        &primary,
    );
    assert!(after_reset.contains("| openai | gpt-durable | 60 | 40 | 20 | 5 | 40.00% |"));

    let mut restarted = test_runtime_service();
    restarted.set_token_usage_store(store);
    let restarted_primary = restarted
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    restarted
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let after_restart = restarted.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"extended-after-restart","method":"agent/shell/command","params":{"idempotency_key":"extended-after-restart","input":"/status --extended"}}"#,
        &restarted_primary,
    );
    assert!(after_restart.contains("| openai | gpt-durable | 60 | 40 | 20 | 5 | 40.00% |"));
}

/// Verifies an attached empty store renders all stable table shapes, invalid
/// status arguments are rejected, and query failure is never shown as zero use.
#[test]
fn runtime_agent_shell_extended_status_handles_empty_and_degraded_stores() {
    let root = temp_root("runtime-agent-extended-status-empty");
    let store = crate::storage::token_usage::TokenUsageStore::new(root.join("token-usage.sqlite"));
    store.initialize(0).unwrap();
    let mut service = test_runtime_service();
    service.set_token_usage_store(store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let empty = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"empty-extended","method":"agent/shell/command","params":{"idempotency_key":"empty-extended","input":"/status --extended"}}"#,
        &primary,
    );
    for days in [7, 30, 60, 90] {
        assert!(
            empty.contains(&format!("### {days}-Day Token Usage")),
            "{empty}"
        );
    }
    assert!(
        empty.contains("| Provider | Model | Billed input |"),
        "{empty}"
    );

    let invalid = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"invalid-extended","method":"agent/shell/command","params":{"idempotency_key":"invalid-extended","input":"/status --verbose"}}"#,
        &primary,
    );
    assert!(invalid.contains("status accepts only the optional --extended argument"));

    let broken_path = root.join("database-is-a-directory");
    fs::create_dir_all(&broken_path).unwrap();
    service.set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(
        broken_path,
    ));
    service.record_agent_provider_token_usage(
        "%1",
        mez_agent::ModelTokenUsage {
            input_tokens: 5,
            output_tokens: 1,
            reasoning_tokens: 0,
            cached_input_tokens: None,
            cache_write_input_tokens: None,
        },
    );
    assert_eq!(
        service
            .agent_token_usage_for_pane("%1")
            .values()
            .next()
            .map(|usage| usage.input_tokens),
        Some(5),
    );
    let degraded = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"degraded-extended","method":"agent/shell/command","params":{"idempotency_key":"degraded-extended","input":"/status --extended"}}"#,
        &primary,
    );
    assert!(
        degraded.contains("### Rolling Token Usage Unavailable"),
        "{degraded}"
    );
    assert!(degraded.contains("storage write failure"), "{degraded}");
    assert!(!degraded.contains("### 7-Day Token Usage"), "{degraded}");
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

/// Verifies `/auth-status` renders one ordered, secret-safe row for every
/// configured provider, including providers without stored credentials.
#[test]
fn runtime_agent_shell_auth_status_lists_configured_provider_rows() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.deepseek]\nkind = \"deepseek\"\nmodels = [\"deepseek-v4-pro\"]\ndefault_model = \"deepseek-v4-pro\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\"]\ndefault_model = \"gpt-5.5\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-agent-auth-status-table");
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

    let status = service
        .execute_agent_shell_command(&primary, "/auth-status")
        .unwrap();

    assert!(status.contains("## Authentication Status"), "{status}");
    assert!(
        status.contains("| Provider | Authenticated | Profile | Credential store | State |"),
        "{status}"
    );
    assert!(
        status.contains("| deepseek | false | none | none | logged-out |"),
        "{status}"
    );
    assert!(
        status.contains("| openai | true | work | file | available |"),
        "{status}"
    );
    assert!(
        status.find("| deepseek |").unwrap() < status.find("| openai |").unwrap(),
        "{status}"
    );
    assert!(!status.contains("sk-runtime-secret"), "{status}");

    let _ = fs::remove_dir_all(root);
}

/// Verifies `/auth-status` retains every configured provider row when no auth
/// store is attached, making unavailable credential storage explicit instead
/// of omitting configured providers or selecting an unrelated default status.
#[test]
fn runtime_agent_shell_auth_status_marks_unavailable_auth_store_per_provider() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.deepseek]\nkind = \"deepseek\"\nmodels = [\"deepseek-v4-pro\"]\ndefault_model = \"deepseek-v4-pro\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\"]\ndefault_model = \"gpt-5.5\"\n"
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

    let status = service
        .execute_agent_shell_command(&primary, "/auth-status")
        .unwrap();

    assert!(
        status.contains("| deepseek | unknown | none | unavailable | auth-store-unavailable |"),
        "{status}"
    );
    assert!(
        status.contains("| openai | unknown | none | unavailable | auth-store-unavailable |"),
        "{status}"
    );
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

/// Verifies sandbox enable and disable default to one exact pane, leave the
/// persisted global backend and generation unchanged, report provenance, and
/// discard the override when the pane's runtime state is removed.
#[test]
fn runtime_agent_shell_sandbox_mutations_are_pane_local_by_default() {
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
    let generation = service.session.config_generation;

    let enabled = service
        .execute_agent_shell_command(&primary, "/sandbox enable --yes")
        .unwrap();
    assert!(enabled.contains(r#""command":"sandbox""#), "{enabled}");
    assert!(enabled.contains("scope=pane"), "{enabled}");
    assert!(matches!(
        service.sandbox_config_for_pane("%1"),
        SandboxConfig::Bubblewrap(_)
    ));
    assert!(matches!(
        service.sandbox_config_for_pane(second_pane.as_str()),
        SandboxConfig::PolicyOnly
    ));
    assert!(matches!(
        service.configured_permissions().sandbox,
        SandboxConfig::PolicyOnly
    ));
    assert_eq!(service.session.config_generation, generation);

    let status = service
        .execute_agent_shell_command(&primary, "/sandbox status")
        .unwrap();
    assert!(
        status.contains("Effective backend | `bubblewrap`"),
        "{status}"
    );
    assert!(status.contains("Source | pane override"), "{status}");
    assert_eq!(service.session.config_generation, generation);

    service.cleanup_removed_pane_runtime_state("%1").unwrap();
    assert!(matches!(
        service.sandbox_config_for_pane("%1"),
        SandboxConfig::PolicyOnly
    ));
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies `--global` persists and hot-applies the default backend while an
/// exact pane override continues to win over later global changes.
#[test]
fn runtime_agent_shell_sandbox_global_mutation_preserves_pane_override() {
    let config = "[permissions]\nsandbox = \"policy-only\"\n";
    let (mut service, primary, path) = sandbox_command_service("runtime-sandbox-global", config);
    let initial_generation = service.session.config_generation;

    let enabled = service
        .execute_agent_shell_command(&primary, "/sandbox enable --global --yes")
        .unwrap();
    assert!(enabled.contains("scope=global"), "{enabled}");
    assert!(matches!(
        service.configured_permissions().sandbox,
        SandboxConfig::Bubblewrap(_)
    ));
    assert_eq!(service.session.config_generation, initial_generation + 1);
    assert!(
        fs::read_to_string(&path)
            .unwrap()
            .contains("sandbox = \"bubblewrap\"")
    );

    service
        .execute_agent_shell_command(&primary, "/sandbox disable --yes")
        .unwrap();
    let global_status = service
        .execute_agent_shell_command(&primary, "/sandbox status --global")
        .unwrap();
    assert!(
        global_status.contains("Backend | `bubblewrap`"),
        "{global_status}"
    );
    assert!(matches!(
        service.sandbox_config_for_pane("%1"),
        SandboxConfig::PolicyOnly
    ));
    assert!(service.pane_has_sandbox_override("%1"));
    let _ = fs::remove_dir_all(path.parent().unwrap());
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

    service.cleanup_removed_pane_runtime_state("%2").unwrap();

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
        conversation_id: "conversation-1".to_string(),
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
