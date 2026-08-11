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
                        == "unsupported approval policy; use ask, auto-allow, full-access, or host-access"
            })
    );

    for key in ["max_input_tokens", "max_output_tokens"] {
        let invalid_token_limit = validate_config_text(
            ConfigFormat::Toml,
            &format!("[model_profiles.default]\n{key} = 0\n"),
            ConfigScope::Primary,
        );

        assert!(!invalid_token_limit.valid);
        assert!(invalid_token_limit.diagnostics.iter().any(|diagnostic| {
            diagnostic.path == format!("model_profiles.default.{key}")
                && diagnostic.message
                    == format!("model_profiles.default.{key} must be a positive integer")
        }));
    }
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

/// Verifies host access is accepted only as a primary user permission and is
/// rejected from project overlays and model profiles with a stable diagnostic.
#[test]
fn host_access_is_user_only_primary_policy() {
    let primary = validate_config_text(
        ConfigFormat::Toml,
        "[permissions]\napproval_policy = \"host-access\"\n",
        ConfigScope::Primary,
    );
    let overlay = validate_config_text(
        ConfigFormat::Toml,
        &format!(
            "version = {CURRENT_CONFIG_SCHEMA_VERSION}\n[permissions]\napproval_policy = \"host-access\"\n"
        ),
        ConfigScope::ProjectOverlay,
    );
    let profile = validate_config_text(
        ConfigFormat::Toml,
        "[model_profiles.unsafe]\napproval_policy = \"host-access\"\n",
        ConfigScope::Primary,
    );

    assert!(primary.valid, "{:?}", primary.diagnostics);
    for validation in [overlay, profile] {
        assert!(!validation.valid);
        assert!(
            validation
                .diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.message.starts_with("user_only_host_access:") })
        );
    }
}

/// Verifies trusted project overlays cannot change the sandbox or execution
/// authority that protects an agent able to write project-local config.
#[test]
fn project_overlays_cannot_change_execution_authority() {
    let overlay = validate_config_text(
        ConfigFormat::Toml,
        &format!(
            "version = {CURRENT_CONFIG_SCHEMA_VERSION}\n\
             [permissions]\n\
             approval_policy = \"full-access\"\n\
             preset = \"auto\"\n\
             sandbox = \"policy-only\"\n\
             read_scopes = [\".\", \"/tmp\"]\n\
             write_scopes = [\".\"]\n\
             network_policy = \"allow\"\n\
             destructive_action_policy = \"allow\"\n\
             bypass_mode = false\n\
             [permissions.bubblewrap]\n\
             executable = \"/usr/local/bin/bwrap\"\n\
             [model_profiles.default]\n\
             approval_policy = \"full-access\"\n"
        ),
        ConfigScope::ProjectOverlay,
    );

    assert!(!overlay.valid);
    for path in [
        "permissions.approval_policy",
        "permissions.preset",
        "permissions.sandbox",
        "permissions.read_scopes",
        "permissions.write_scopes",
        "permissions.network_policy",
        "permissions.destructive_action_policy",
        "permissions.bypass_mode",
        "permissions.bubblewrap.executable",
        "model_profiles.default.approval_policy",
    ] {
        assert!(overlay.diagnostics.iter().any(|diagnostic| {
            diagnostic.path == path
                && diagnostic
                    .message
                    .starts_with("primary_user_only_execution_authority:")
        }));
    }
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

/// Verifies the Tokio worker count must remain positive so runtime construction
/// cannot silently recreate the single-thread scheduler starvation condition.
#[test]
fn rejects_zero_runtime_cpu_count() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[runtime]\ncpu_count = 0\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert_eq!(validation.diagnostics[0].path, "runtime.cpu_count");
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

/// Verifies schema 20 rejects the removed implementation-pressure setting.
///
/// Primary migration deletes the schema-19 key before validation. A document
/// that already claims schema 20 must not silently retain obsolete behavior.
#[test]
fn rejects_removed_implementation_pressure_setting_in_schema_20() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "version = 20\n[agents]\nimplementation_pressure_after_shell_actions = 3\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "agents.implementation_pressure_after_shell_actions"
            && diagnostic.message.contains("unknown")
    }));
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

/// Verifies active-turn sleep inhibition accepts only its explicit host-power
/// policy values, preventing misspellings from silently disabling or enabling
/// idle-sleep behavior.
#[test]
fn rejects_invalid_active_turn_sleep_inhibition_values() {
    let valid = validate_config_text(
        ConfigFormat::Toml,
        "[agents]\nactive_turn_sleep_inhibition = \"system-and-display\"\n",
        ConfigScope::Primary,
    );
    let invalid = validate_config_text(
        ConfigFormat::Toml,
        "[agents]\nactive_turn_sleep_inhibition = \"always\"\n",
        ConfigScope::Primary,
    );

    assert!(valid.valid);
    assert!(!invalid.valid);
    assert!(invalid.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "agents.active_turn_sleep_inhibition"
            && diagnostic.message
                == "agents.active_turn_sleep_inhibition must be disabled, system, or system-and-display"
    }));
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
        "[terminal]\npane_spawn_directory = \"daemon\"\npane_spawn_view = \"editor\"\ncursor_style = \"beam\"\ncursor_blink = \"sometimes\"\nemoji_width = \"auto\"\nreduced_motion = \"sometimes\"\nenhanced_keyboard_reporting = \"sometimes\"\ncompletion_attention_flashing = \"sometimes\"\ncursor_blink_interval_ms = 0\nresize_debounce_ms = 0\nrender_rate_limit_fps = -1\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "terminal.pane_spawn_directory"
            && diagnostic.message == "terminal.pane_spawn_directory must be home or same-directory"
    }));
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "terminal.pane_spawn_view"
            && diagnostic.message == "terminal.pane_spawn_view must be shell or agent"
    }));
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
        diagnostic.path == "terminal.enhanced_keyboard_reporting"
            && diagnostic.message == "terminal.enhanced_keyboard_reporting must be true or false"
    }));
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "terminal.completion_attention_flashing"
            && diagnostic.message == "terminal.completion_attention_flashing must be true or false"
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

/// Verifies the xterm-compatible default profile accepts xterm terminfo.
///
/// The pane TERM is user-configurable, so accepting the standard xterm name
/// keeps explicit configuration consistent with the default profile.
#[test]
fn accepts_xterm_terminal_identity_in_default_profile() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[terminal]\nterm = \"xterm-256color\"\n",
        ConfigScope::Primary,
    );

    assert!(validation.valid, "{:?}", validation.diagnostics);
}

/// Verifies that the root auto-sizing routing policy accepts stable policy
/// names and rejects values that cannot select a defined execution path.
#[test]
fn validates_root_auto_sizing_routing_policy() {
    let valid = validate_config_text(
        ConfigFormat::Toml,
        "[agents.auto_sizing]\nroot_routing_policy = \"in-place\"\n",
        ConfigScope::Primary,
    );
    assert!(valid.valid, "{:?}", valid.diagnostics);

    let invalid = validate_config_text(
        ConfigFormat::Toml,
        "[agents.auto_sizing]\nroot_routing_policy = \"unknown\"\n",
        ConfigScope::Primary,
    );
    assert!(!invalid.valid);
    assert!(invalid.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "agents.auto_sizing.root_routing_policy"
            && diagnostic.message == "unsupported root routing policy; use subagent or in-place"
    }));
}

/// Verifies Bubblewrap accepts only a complete printable Git identity pair so
/// validation cannot admit a partial or empty author projection.
#[test]
fn validates_paired_sanitized_bubblewrap_git_identity() {
    let valid = validate_config_text(
        ConfigFormat::Toml,
        "version = 25\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ngit_user_name = \"Sandbox Author\"\ngit_user_email = \"sandbox@example.invalid\"\n",
        ConfigScope::Primary,
    );
    assert!(valid.valid, "{:?}", valid.diagnostics);

    let incomplete = validate_config_text(
        ConfigFormat::Toml,
        "version = 25\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ngit_user_name = \"Sandbox Author\"\n",
        ConfigScope::Primary,
    );
    assert!(incomplete.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "permissions.bubblewrap"
            && diagnostic.message.contains("must be configured together")
    }));

    let blank = validate_config_text(
        ConfigFormat::Toml,
        "version = 25\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ngit_user_name = \"   \"\ngit_user_email = \"sandbox@example.invalid\"\n",
        ConfigScope::Primary,
    );
    assert!(blank.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "permissions.bubblewrap.git_user_name"
            && diagnostic.message == "Bubblewrap Git identity must be non-empty printable text"
    }));
}

/// Verifies the current schema rejects removed built-in and custom toolchain
/// keys in both primary configuration and project overlays.
#[test]
fn rejects_removed_toolchain_configuration() {
    for scope in [ConfigScope::Primary, ConfigScope::ProjectOverlay] {
        for (key, value) in [
            ("toolchains", "[\"rust\"]"),
            (
                "custom_toolchains",
                "{ acme = { roots = [\"/opt/acme\"] } }",
            ),
        ] {
            let text = format!(
                "version = {CURRENT_CONFIG_SCHEMA_VERSION}\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\n{key} = {value}\n"
            );
            let validation = validate_config_text(ConfigFormat::Toml, &text, scope);
            let path = format!("permissions.bubblewrap.{key}");
            assert!(!validation.valid, "{scope:?} accepted {path}");
            assert!(
                validation.diagnostics.iter().any(|diagnostic| {
                    diagnostic.path == path && diagnostic.message.contains("unknown")
                }),
                "missing removed-key diagnostic for {path}: {:?}",
                validation.diagnostics
            );
        }
    }
}

/// Verifies schema v48 accepts a bounded primary exact group list and rejects
/// malformed or project-supplied host group authority before NSS resolution.
#[test]
fn group_whitelist_are_structurally_validated_and_primary_only() {
    let valid = format!(
        "version = {CURRENT_CONFIG_SCHEMA_VERSION}\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ngroup_whitelist = [\"sudo\", \"docker\"]\n"
    );
    let primary = validate_config_text(ConfigFormat::Toml, &valid, ConfigScope::Primary);
    assert!(primary.valid, "{:?}", primary.diagnostics);

    let overlay = validate_config_text(ConfigFormat::Toml, &valid, ConfigScope::ProjectOverlay);
    assert!(overlay.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "permissions.bubblewrap.group_whitelist"
            && diagnostic
                .message
                .starts_with("primary_user_only_execution_authority:")
    }));

    for value in ["[\"\"]", "[\"27\"]", "[\"sudo\", \"sudo\"]", "[1]"] {
        let invalid = format!(
            "version = {CURRENT_CONFIG_SCHEMA_VERSION}\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ngroup_whitelist = {value}\n"
        );
        let validation = validate_config_text(ConfigFormat::Toml, &invalid, ConfigScope::Primary);
        assert!(
            validation
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.path == "permissions.bubblewrap.group_whitelist"),
            "missing diagnostic for {value}: {:?}",
            validation.diagnostics
        );
    }
}

/// Verifies schema v50 accepts only bounded portable environment names and
/// keeps environment forwarding primary-user-only.
#[test]
fn env_whitelist_is_structurally_validated_and_primary_only() {
    let valid = format!(
        "version = {CURRENT_CONFIG_SCHEMA_VERSION}\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\nenv_whitelist = [\"TERM_PROGRAM\", \"CI\"]\n"
    );
    let primary = validate_config_text(ConfigFormat::Toml, &valid, ConfigScope::Primary);
    assert!(primary.valid, "{:?}", primary.diagnostics);
    let overlay = validate_config_text(ConfigFormat::Toml, &valid, ConfigScope::ProjectOverlay);
    assert!(overlay.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "permissions.bubblewrap.env_whitelist"
            && diagnostic
                .message
                .starts_with("primary_user_only_execution_authority:")
    }));
    for value in ["[\"\"]", "[\"BAD-NAME\"]", "[\"CI\", \"CI\"]", "[1]"] {
        let invalid = format!(
            "version = {CURRENT_CONFIG_SCHEMA_VERSION}\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\nenv_whitelist = {value}\n"
        );
        let validation = validate_config_text(ConfigFormat::Toml, &invalid, ConfigScope::Primary);
        assert!(
            validation
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.path == "permissions.bubblewrap.env_whitelist"),
            "{value}: {:?}",
            validation.diagnostics
        );
    }
}
