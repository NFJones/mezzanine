//! Config validation tests.

use super::*;

/// Verifies that custom subagent profiles are part of the baseline config
/// schema, including nested shell environment overrides, while unknown profile
/// keys remain rejected.
#[test]
fn validates_custom_subagent_profile_schema() {
    let valid = validate_config_text(
        ConfigFormat::Toml,
        "[subagents.reviewer]\nname = \"Reviewer\"\ndescription = \"Reviews changes\"\ndeveloper_instructions = \"Focus on correctness.\"\nmodel_profile = \"default\"\npermission_preset = \"read-only\"\nmcp_servers = [\"filesystem\"]\ndefault_cooperation_mode = \"explore-only\"\ndefault_read_scopes = [\"src\"]\ndefault_write_scopes = []\n[subagents.reviewer.shell_env]\nREVIEW_MODE = \"strict\"\n",
        ConfigScope::Primary,
    );

    assert!(valid.valid, "{:?}", valid.diagnostics);

    let invalid = validate_config_text(
        ConfigFormat::Toml,
        "[subagents.reviewer]\nunknown = true\n",
        ConfigScope::Primary,
    );

    assert!(!invalid.valid);
    assert!(invalid.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "subagents.reviewer.unknown"
            && diagnostic.message == "unknown subagent profile configuration key"
    }));
}

/// Verifies that user-defined personality profiles are part of the baseline
/// config schema while unknown profile keys remain rejected.
///
/// Personality profiles affect provider prompt construction and pane-local
/// agent preferences, so their table shape must be validated before runtime
/// config application stores those values in live agent state.
#[test]
fn validates_custom_personality_profile_schema() {
    let valid = validate_config_text(
        ConfigFormat::Toml,
        "[agents]\ncustom_system_prompt = \"Follow local conventions.\"\ndefault_personality = \"careful\"\n[personalities.careful]\nname = \"Careful\"\nsystem_prompt = \"Be precise.\"\nresponse_style = \"terse\"\nmodel_profile = \"default\"\nplanning_enabled = true\nrouting_enabled = true\n",
        ConfigScope::Primary,
    );

    assert!(valid.valid, "{:?}", valid.diagnostics);

    let invalid = validate_config_text(
        ConfigFormat::Toml,
        "[personalities.careful]\nunknown = true\n",
        ConfigScope::Primary,
    );

    assert!(!invalid.valid);
    assert!(invalid.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "personalities.careful.unknown"
            && diagnostic.message == "unknown personality profile configuration key"
    }));
}

/// Verifies that named model profiles are accepted as a first-class
/// configuration table, including nested non-secret provider options, while
/// unknown model-profile keys are rejected.
#[test]
fn validates_named_model_profile_schema() {
    let valid = validate_config_text(
        ConfigFormat::Toml,
        "[model_profiles.default]\nprovider = \"openai\"\nmodel = \"gpt-5.2\"\nreasoning_profile = \"medium\"\nlatency_preference = \"default\"\nmultimodal_required = false\ncontext_window_tokens = 128000\nmax_output_tokens = 12000\nsafety_tier = \"high\"\nprivacy_tier = \"standard\"\nresidency = \"global\"\napproval_policy = \"ask\"\nfallback_profiles = [\"fast\"]\n",
        ConfigScope::Primary,
    );

    assert!(valid.valid, "{:?}", valid.diagnostics);

    let invalid = validate_config_text(
        ConfigFormat::Toml,
        "[model_profiles.default]\nunknown = true\n",
        ConfigScope::Primary,
    );

    assert!(!invalid.valid);
    assert!(invalid.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "model_profiles.default.unknown"
            && diagnostic.message == "unknown model profile configuration key"
    }));

    let invalid_approval_policy = validate_config_text(
        ConfigFormat::Toml,
        "[model_profiles.default]\napproval_policy = \"on-request\"\n",
        ConfigScope::Primary,
    );

    assert!(!invalid_approval_policy.valid);
    assert!(
        invalid_approval_policy
            .diagnostics
            .iter()
            .any(|diagnostic| {
                diagnostic.path == "model_profiles.default.approval_policy"
                    && diagnostic.message
                        == "unsupported approval policy; use ask, auto-allow, or full-access"
            })
    );

    let invalid_max_output_tokens = validate_config_text(
        ConfigFormat::Toml,
        "[model_profiles.default]\nmax_output_tokens = 0\n",
        ConfigScope::Primary,
    );

    assert!(!invalid_max_output_tokens.valid);
    assert!(
        invalid_max_output_tokens
            .diagnostics
            .iter()
            .any(|diagnostic| {
                diagnostic.path == "model_profiles.default.max_output_tokens"
                    && diagnostic.message
                        == "model_profiles.default.max_output_tokens must be a positive integer"
            })
    );
}

/// Verifies that implementation-exposed audit config keys remain listed in the
/// normative Section 8.2 configuration table.
#[test]
fn specification_lists_all_audit_schema_keys() {
    let specification = include_str!("../../../../../SPEC.md");

    for key in super::super::schema::AUDIT_KEYS {
        assert!(
            specification.contains(&format!("`{key}`")),
            "SPEC.md must list audit.{key}"
        );
    }
}

/// Verifies rejects invalid frame display values.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_invalid_frame_display_values() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[frames.window]\nenabled = \"yes\"\nposition = \"middle\"\nstyle = \"blink\"\n[frames.pane]\nposition = \"side\"\nstyle = \"loud\"\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "frames.window.enabled"
            && diagnostic.message == "frames.window.enabled must be true or false"
    }));
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "frames.window.position"
            && diagnostic.message == "frames.window.position must be top, bottom, or border"
    }));
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "frames.window.style"
            && diagnostic.message
                == "frames.window.style must be default, bold, underline, inverse, or reverse"
    }));
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "frames.pane.position"
            && diagnostic.message == "frames.pane.position must be top, bottom, or border"
    }));
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "frames.pane.style"
            && diagnostic.message
                == "frames.pane.style must be default, bold, underline, inverse, or reverse"
    }));
}

/// Verifies allows declared dynamic config maps.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn allows_declared_dynamic_config_maps() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[keys.command_bindings]\nrefresh = \"refresh-client\"\n[providers.openai.options]\nreasoning_effort = \"medium\"\n[hooks.notify.env]\nLOG_LEVEL = \"debug\"\n[extensions.example]\nenabled = true\n",
        ConfigScope::Primary,
    );

    assert!(validation.valid, "{:?}", validation.diagnostics);
}

/// Verifies rejects forbidden session default command.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_forbidden_session_default_command() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[session]\ndefault_command = \"vim\"\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert!(
        validation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path == "session.default_command")
    );
}

/// Verifies rejects shell path override.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_shell_path_override() {
    let validation = validate_config_text(
        ConfigFormat::Yaml,
        "shell:\n  path: /bin/bash\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert!(
        validation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path == "shell.path")
    );
}

/// Verifies rejects auth secrets in json config.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_auth_secrets_in_json_config() {
    let validation = validate_config_text(
        ConfigFormat::Json,
        r#"{ "auth": { "access_token": "secret" } }"#,
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert_eq!(validation.diagnostics[0].path, "auth.access_token");
}

/// Verifies rejects project overlay secret material.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_project_overlay_secret_material() {
    let validation = validate_config_text(
        ConfigFormat::Yaml,
        "providers:\n  local:\n    token: secret\n",
        ConfigScope::ProjectOverlay,
    );

    assert!(!validation.valid);
    assert_eq!(validation.diagnostics[0].path, "providers.local.token");
}

/// Verifies validates known mcp server keys.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn validates_known_mcp_server_keys() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[mcp_servers.fs]\ncommand = \"mcp-fs\"\nargs = [\"--root\", \".\"]\nenv_vars = [\"MCP_TOKEN\"]\ncwd = \".\"\nenabled_tools = [\"read_file\"]\ndisabled_tools = [\"delete_file\"]\nstartup_timeout_sec = 10\ntool_timeout_sec = 60\nenabled = true\napproval = \"prompt\"\n[mcp_servers.fs.env]\nLOG_LEVEL = \"debug\"\n[mcp_servers.fs.http_headers]\nX_Client = \"mez\"\n[mcp_servers.fs.tool_approvals]\nread_file = \"prompt\"\n[mcp_servers.fs.external_capability]\npurpose = \"File reads and project tree inspection\"\nusage_instructions = \"Use read_file only when the task needs file contents.\"\n",
        ConfigScope::Primary,
    );

    assert!(validation.valid, "{:?}", validation.diagnostics);
}

/// Verifies rejects unknown mcp server keys.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_unknown_mcp_server_keys() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[mcp_servers.fs]\nmagic = true\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert_eq!(validation.diagnostics[0].path, "mcp_servers.fs.magic");
}

/// Verifies rejects inline mcp secret material.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_inline_mcp_secret_material() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[mcp_servers.fs.env]\nAPI_TOKEN = \"secret\"\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert_eq!(
        validation.diagnostics[0].path,
        "mcp_servers.fs.env.API_TOKEN"
    );
}

/// Verifies rejects unsupported permission modes.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_unsupported_permission_modes() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[permissions]\napproval_policy = \"on-failure\"\npreset = \"unsupported\"\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert!(
        validation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path == "permissions.approval_policy")
    );
    assert!(
        validation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path == "permissions.preset")
    );
}

/// Verifies that configuration cannot directly enter the explicit approval
/// bypass state. The specification requires bypass activation to go through an
/// obvious user-selected flow with primary authority and audit visibility, so
/// config validation must still allow the documented default `false` value
/// while rejecting an enabling value before it reaches the runtime policy.
#[test]
fn rejects_config_enabled_approval_bypass_mode() {
    let enabled = validate_config_text(
        ConfigFormat::Toml,
        "[permissions]\nbypass_mode = true\n",
        ConfigScope::Primary,
    );
    let disabled = validate_config_text(
        ConfigFormat::Toml,
        "[permissions]\nbypass_mode = false\n",
        ConfigScope::Primary,
    );

    assert!(!enabled.valid);
    assert_eq!(enabled.diagnostics[0].path, "permissions.bypass_mode");
    assert!(
        enabled.diagnostics[0]
            .message
            .contains("cannot be enabled from configuration")
    );
    assert!(disabled.valid, "{:?}", disabled.diagnostics);
}

/// Verifies rejects invalid history limit values.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_invalid_history_limit_values() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[history]\nlines = 0\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert_eq!(validation.diagnostics[0].path, "history.lines");
    assert!(
        validation.diagnostics[0]
            .message
            .contains("positive integer")
    );

    let rotation_validation = validate_config_text(
        ConfigFormat::Toml,
        "[history]\nrotate_lines = 0\n",
        ConfigScope::Primary,
    );

    assert!(!rotation_validation.valid);
    assert_eq!(
        rotation_validation.diagnostics[0].path,
        "history.rotate_lines"
    );
    assert!(
        rotation_validation.diagnostics[0]
            .message
            .contains("positive integer")
    );
}

/// Verifies rejects invalid agent concurrency values.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_invalid_agent_concurrency_values() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[agents]\nmax_concurrent_agents = 0\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert_eq!(
        validation.diagnostics[0].path,
        "agents.max_concurrent_agents"
    );
    assert!(
        validation.diagnostics[0]
            .message
            .contains("positive integer")
    );
}

/// Verifies rejects invalid action-failure retry limits.
///
/// Retry limits must be positive so model-correctable action failures have a
/// clear bounded repair policy instead of an ambiguous zero-attempt state.
#[test]
fn rejects_invalid_action_failure_retry_limit_values() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[agents]\naction_failure_retry_limit = 0\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert_eq!(
        validation.diagnostics[0].path,
        "agents.action_failure_retry_limit"
    );
    assert!(
        validation.diagnostics[0]
            .message
            .contains("positive integer")
    );
}

/// Verifies rejects invalid implementation-pressure shell-action thresholds.
///
/// A zero threshold would make every turn carry pressure before any shell
/// evidence exists, so validation requires the advisory trigger to be a
/// positive integer like other agent loop-control settings.
#[test]
fn rejects_invalid_implementation_pressure_after_shell_actions_values() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[agents]\nimplementation_pressure_after_shell_actions = 0\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert_eq!(
        validation.diagnostics[0].path,
        "agents.implementation_pressure_after_shell_actions"
    );
    assert!(
        validation.diagnostics[0]
            .message
            .contains("positive integer")
    );
}

/// Verifies rejects invalid agent loop iteration limits.
///
/// A zero loop limit would make `/loop` unable to perform even the initial work
/// iteration while still accepting a command whose purpose is bounded automatic
/// continuation, so validation requires a positive integer.
#[test]
fn rejects_invalid_agent_loop_limit_values() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[agents]\nloop_limit = 0\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert_eq!(validation.diagnostics[0].path, "agents.loop_limit");
    assert!(
        validation.diagnostics[0]
            .message
            .contains("positive integer")
    );
}

/// Verifies rejects invalid compaction raw-retention percentages.
///
/// The retained raw tail is configured as a percentage of the active model
/// context budget. Zero or over-100 values would either remove the exact recent
/// tail or exceed the budget contract, so validation rejects them before
/// runtime compaction can apply the setting.
#[test]
fn rejects_invalid_compaction_raw_retention_percent_values() {
    let zero = validate_config_text(
        ConfigFormat::Toml,
        "[agents]\ncompaction_raw_retention_percent = 0\n",
        ConfigScope::Primary,
    );
    let too_large = validate_config_text(
        ConfigFormat::Toml,
        "[agents]\ncompaction_raw_retention_percent = 101\n",
        ConfigScope::Primary,
    );
    let valid = validate_config_text(
        ConfigFormat::Toml,
        "[agents]\ncompaction_raw_retention_percent = 25\n",
        ConfigScope::Primary,
    );

    assert!(!zero.valid);
    assert_eq!(
        zero.diagnostics[0].path,
        "agents.compaction_raw_retention_percent"
    );
    assert!(
        zero.diagnostics[0]
            .message
            .contains("integer from 1 to 100")
    );
    assert!(!too_large.valid);
    assert_eq!(
        too_large.diagnostics[0].path,
        "agents.compaction_raw_retention_percent"
    );
    assert!(valid.valid, "{:?}", valid.diagnostics);
}

/// Verifies rejects invalid root subagent width values.
///
/// The root delegation limit bounds how many direct helpers a pane agent can
/// keep active. A zero value would make every configured pane agent unable to
/// delegate while still advertising subagent capability, so validation must
/// reject it before runtime policy is applied.
#[test]
fn rejects_invalid_root_subagent_width_values() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[agents]\nmax_root_subagents = 0\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert_eq!(validation.diagnostics[0].path, "agents.max_root_subagents");
    assert!(
        validation.diagnostics[0]
            .message
            .contains("positive integer")
    );
}

/// Verifies rejects invalid nested subagent width values.
///
/// Child subagents can delegate further only within a configured branching
/// factor. Zero would make the delegation contract depend on parent depth in a
/// surprising way, so the static validator keeps the runtime policy strictly
/// positive and diagnosable.
#[test]
fn rejects_invalid_child_subagent_width_values() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[agents]\nmax_subagents_per_subagent = 0\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert_eq!(
        validation.diagnostics[0].path,
        "agents.max_subagents_per_subagent"
    );
    assert!(
        validation.diagnostics[0]
            .message
            .contains("positive integer")
    );
}

/// Verifies rejects invalid subagent depth values.
///
/// Depth controls whether a spawned child can create another generation of
/// helpers. A positive value keeps the root-agent and child-agent cases
/// distinct while preventing accidental recursive delegation loops.
#[test]
fn rejects_invalid_subagent_depth_values() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[agents]\nmax_depth = 0\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert_eq!(validation.diagnostics[0].path, "agents.max_depth");
    assert!(
        validation.diagnostics[0]
            .message
            .contains("positive integer")
    );
}

/// Verifies rejects invalid subagent pane bucket values.
///
/// Subagent windows use a positive pane-capacity limit before a new background
/// window is created. Zero would strand placement policy without a usable
/// bucket, so the static validator must reject it at config load time.
#[test]
fn rejects_invalid_subagent_window_capacity_values() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[agents]\nmax_subagent_panes_per_window = 0\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert_eq!(
        validation.diagnostics[0].path,
        "agents.max_subagent_panes_per_window"
    );
    assert!(
        validation.diagnostics[0]
            .message
            .contains("positive integer")
    );
}

/// Verifies rejects unsupported subagent wait policy values.
///
/// Parent/subagent coordination changes scheduler semantics, so the static
/// validator must reject typos before runtime config application can fall back
/// to an unintended default.
#[test]
fn rejects_invalid_subagent_wait_policy_values() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[agents]\nsubagent_wait_policy = \"background\"\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert_eq!(
        validation.diagnostics[0].path,
        "agents.subagent_wait_policy"
    );
    assert!(
        validation.diagnostics[0]
            .message
            .contains("unsupported subagent wait policy")
    );
}

/// Verifies rejects unsupported local action executor values.
///
/// The executor setting controls whether accepted local MAAP actions are sent
/// through the pane shell or through a strict native transport. Validation must
/// reject typos so local file and process effects cannot silently use the wrong
/// Verifies rejects invalid terminal term and profile values.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_invalid_terminal_term_and_profile_values() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[terminal]\nterm = \"\"\nprofile = \"ansi\"\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert!(
        validation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path == "terminal.term")
    );
    assert!(
        validation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path == "terminal.profile")
    );
}

/// Verifies rejects invalid terminal presentation values.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_invalid_terminal_presentation_values() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[terminal]\ncursor_style = \"beam\"\ncursor_blink = \"sometimes\"\nemoji_width = \"auto\"\nreduced_motion = \"sometimes\"\ncursor_blink_interval_ms = 0\nresize_debounce_ms = 0\nrender_rate_limit_fps = -1\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "terminal.cursor_style"
            && diagnostic.message == "terminal.cursor_style must be block, underline, or bar"
    }));
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "terminal.cursor_blink"
            && diagnostic.message == "terminal.cursor_blink must be true or false"
    }));
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "terminal.reduced_motion"
            && diagnostic.message == "terminal.reduced_motion must be true or false"
    }));
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "terminal.emoji_width"
            && diagnostic.message == "terminal.emoji_width must be wide or narrow"
    }));
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "terminal.cursor_blink_interval_ms"
            && diagnostic.message == "terminal.cursor_blink_interval_ms must be a positive integer"
    }));
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "terminal.resize_debounce_ms"
            && diagnostic.message == "terminal.resize_debounce_ms must be a positive integer"
    }));
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "terminal.render_rate_limit_fps"
            && diagnostic.message == "terminal.render_rate_limit_fps must be a non-negative integer"
    }));
}

/// Verifies rejects host terminal identity in default profile.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_host_terminal_identity_in_default_profile() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[terminal]\nterm = \"xterm-256color\"\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "terminal.term" && diagnostic.message.contains("host terminal")
    }));
}
