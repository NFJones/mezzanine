//! Runtime tests for terminal rendering behavior.

use super::*;

/// Verifies that frame-context animation stays static when no live agent footer
/// is visible in the active window. This keeps idle redraws from paying for
/// animated footer state when agent mode is inactive or quiescent.
#[test]
fn runtime_frame_context_disables_animation_without_live_agent_footer() {
    let service = test_runtime_service();
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    assert_eq!(config.frame_context.animation_tick_ms, 0);
}

/// Verifies that a live agent footer re-enables animated frame ticks so active
/// agent progress indicators keep their motion while work is still running.
#[test]
fn runtime_frame_context_animates_live_agent_footer() {
    let mut service = test_runtime_service();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    service.agent_compacting_panes.insert(pane_id, 1);
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    assert!(config.frame_context.animation_tick_ms > 0);
}

/// Verifies that active pane-frame agent status enables animation even when
/// the agent shell footer is not visible. Pane headers and live footers share
/// the same frame tick, so header-only status indicators must not freeze when
/// no prompt overlay is being rendered.
#[test]
fn runtime_frame_context_animates_active_agent_status_without_live_footer() {
    let mut service = test_runtime_service();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    service
        .agent_turn_ledger
        .start_turn(mez_agent::AgentTurnRecord {
            turn_id: "turn-running".to_string(),
            agent_id: format!("agent-{pane_id}"),
            pane_id: pane_id.clone(),
            trigger: mez_agent::AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 1,
            policy_profile: "default".to_string(),
            model_profile: "default".to_string(),
            parent_turn_id: None,
            cooperation_mode: None,
            state: AgentTurnState::Running,

            initial_capability: None,
        })
        .unwrap();

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(pane_context.agent_status.as_deref(), Some("running"));
    assert!(pane_context.agent_prompt.is_none());
    assert!(config.frame_context.animation_tick_ms > 0);
}

/// Verifies that frame context renders the real normalized exit status when a
/// non-live pane has known exit metadata. This prevents pane frames from
/// collapsing all exited processes into a generic `exited` placeholder.
#[test]
fn runtime_frame_context_uses_known_pane_exit_status() {
    let mut service = test_runtime_service();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    service
        .session
        .set_pane_live_state(&pane_id, false)
        .unwrap();
    service.pane_exit_records.insert(
        pane_id.clone(),
        PaneExitRecord {
            exit_status: mez_mux::process::PaneExitStatus {
                code: Some(7),
                signal: None,
                success: false,
            },
        },
    );

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(pane_context.exit_status.as_deref(), Some("exit=7"));
}

/// Verifies that pane-frame runtime context includes the best known current
/// working directory in the compact home-relative form used by the status
/// pill. This keeps the renderer independent from process probing while still
/// giving users location context when shell prompts are hidden or overwritten.
#[test]
fn runtime_frame_context_reports_home_relative_pane_working_directory() {
    let mut service = test_runtime_service();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from);
    let path = home
        .as_ref()
        .map(|home| home.join("Documents/repos/mezzanine"))
        .unwrap_or_else(|| PathBuf::from("/tmp/mezzanine"));
    let expected = home
        .as_ref()
        .map(|_| "~/Documents/repos/mezzanine".to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());
    service
        .pane_current_working_directories
        .insert(pane_id.clone(), path);

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(
        pane_context.current_working_directory.as_deref(),
        Some(expected.as_str())
    );
    assert_eq!(
        config
            .frame_context
            .window_status
            .as_ref()
            .and_then(|status| status.active_pane_working_directory.as_deref()),
        Some(expected.as_str())
    );
}

/// Verifies that deep pane working directories collapse to the last three path
/// segments in the default window status. This keeps the footer compact while
/// still surfacing the most actionable cwd context for narrow frame rows.
#[test]
fn runtime_frame_context_compacts_deep_pane_working_directory() {
    let mut service = test_runtime_service();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from);
    let path = home
        .as_ref()
        .map(|home| home.join("Documents/repos/mezzanine/src/runtime"))
        .unwrap_or_else(|| PathBuf::from("/tmp/worktrees/mez/src/runtime"));
    service
        .pane_current_working_directories
        .insert(pane_id.clone(), path);

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(
        pane_context.current_working_directory.as_deref(),
        Some("…/mezzanine/src/runtime")
    );
    assert_eq!(
        config
            .frame_context
            .window_status
            .as_ref()
            .and_then(|status| status.active_pane_working_directory.as_deref()),
        Some("…/mezzanine/src/runtime")
    );
}

/// Verifies that frame context leaves unused dynamic right-status fields empty
/// when the configured template only references pane working-directory data.
/// This avoids repeated uptime and datetime formatting work on redraws that do
/// not display those fields.
#[test]
fn runtime_frame_context_skips_unused_dynamic_window_status_fields() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[frames.window]\nright_status = \"#{pane.pwd}\"\n".to_string(),
        }])
        .unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from);
    let path = home
        .as_ref()
        .map(|home| home.join("Documents/repos/mezzanine"))
        .unwrap_or_else(|| PathBuf::from("/tmp/mezzanine"));
    let expected = home
        .as_ref()
        .map(|_| "~/Documents/repos/mezzanine".to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());
    service
        .pane_current_working_directories
        .insert(pane_id, path);
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let status = config.frame_context.window_status.as_ref().unwrap();
    assert_eq!(
        status.active_pane_working_directory.as_deref(),
        Some(expected.as_str())
    );
    assert!(status.system_uptime.is_empty());
    assert!(status.datetime_local.is_empty());
}

/// Verifies that the pane-frame status reports compaction as its own active
/// running substate. Compaction is provider work, but it is distinct enough
/// from ordinary response generation that users need a direct state label.
#[test]
fn runtime_frame_context_reports_agent_compacting_substate() {
    let mut service = test_runtime_service();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    service
        .agent_turn_ledger
        .start_turn(mez_agent::AgentTurnRecord {
            turn_id: "turn-completed".to_string(),
            agent_id: format!("agent-{pane_id}"),
            pane_id: pane_id.clone(),
            trigger: mez_agent::AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 1,
            policy_profile: "default".to_string(),
            model_profile: "default".to_string(),
            parent_turn_id: None,
            cooperation_mode: None,
            state: AgentTurnState::Queued,

            initial_capability: None,
        })
        .unwrap();
    service
        .agent_turn_ledger
        .finish_turn("turn-completed", AgentTurnState::Completed)
        .unwrap();
    service.agent_compacting_panes.insert(pane_id.clone(), 1);

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(pane_context.agent_status.as_deref(), Some("compacting"));
    assert_eq!(
        config
            .frame_context
            .window_agent_active_counts
            .get(service.session().active_window().unwrap().id.as_str())
            .copied(),
        Some(1)
    );
}

/// Verifies pane context usage percentages for named OpenAI-compatible
/// providers use live provider-catalog context windows instead of the generic
/// fallback denominator.
#[test]
fn runtime_frame_context_uses_cached_catalog_context_window_for_named_compatible_provider() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"compat\"\ndefault_model_profile = \"work\"\n[providers.compat]\nkind = \"openai-compatible\"\nmodels = [\"baseline-model\"]\ndefault_model = \"baseline-model\"\n[model_profiles.work]\nprovider = \"compat\"\nmodel = \"baseline-model\"\n"
                .to_string(),
        }])
        .unwrap();
    service.cache_provider_model_catalog_for_tests(
        "compat",
        vec![mez_agent::ProviderModelInfo {
            id: "catalog-only-model".to_string(),
            display_name: None,
            reasoning_levels: vec!["low".to_string()],
            context_window_tokens: Some(2_000_000),
            capabilities: Vec::new(),
        }],
        vec!["low".to_string()],
    );
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    let model_response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"compatible-model","method":"agent/shell/command","params":{"idempotency_key":"compatible-model","input":"/model catalog-only-model"}}"#,
        &primary,
    );
    assert!(
        model_response.contains("catalog-only-model"),
        "{model_response}"
    );

    service.record_agent_provider_token_usage(
        &pane_id,
        mez_agent::ModelTokenUsage {
            input_tokens: 500_000,
            output_tokens: 34,
            reasoning_tokens: 9,
            cached_input_tokens: Some(80),
            cache_write_input_tokens: None,
        },
    );
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(pane_context.agent_context_usage.as_deref(), Some("25%"));
}

/// Verifies mouse drag selection copies the visible alternate-screen grid.
///
/// Full-screen terminal applications are intentionally excluded from normal
/// history and copy-mode buffers, but an explicit mouse drag is a user copy
/// operation over the displayed pane body. This regression protects less/nano
/// style alternate-screen copying without making alternate-screen content part
/// of scrollback or default agent context.
#[test]
fn runtime_mouse_drag_copies_visible_alternate_screen_content() {
    let mut service = test_runtime_service_with_size(Size::new(20, 4).unwrap());
    service.set_frame_visibility_for_tests(false, false);
    let primary = service
        .attach_primary("primary", true, Size::new(20, 4).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"normal-only\n\x1b[?1049halpha beta\nsecond row");
    assert!(screen.alternate_screen_active());
    assert!(
        !screen
            .normal_content_lines()
            .iter()
            .any(|line| line.contains("alpha beta"))
    );
    service.pane_screens.insert(pane_id.clone(), screen);

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::HandleMouse(MouseAction::CopySelectionStart(
                        CopyPosition { line: 0, column: 0 },
                    )),
                    TerminalClientLoopAction::HandleMouse(MouseAction::CopySelectionFinish(
                        CopyPosition { line: 1, column: 6 },
                    )),
                ],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(
        service.paste_buffers.get("mouse"),
        Some("alpha beta\nsecond")
    );
    assert!(!service.active_copy_modes.contains_key(&pane_id));
}

/// Verifies mouse focus uses the same pane-frame row accounting as rendering.
///
/// A top pane frame that is merged into an interior divider does not consume the
/// first content row of the pane below it. Mouse targeting must therefore allow a
/// click on that first rendered content row to focus the lower pane instead of
/// treating the row as an inert frame.
#[test]
fn runtime_mouse_focus_targets_content_below_merged_top_pane_frame() {
    let mut service = test_runtime_service_with_size(Size::new(20, 8).unwrap());
    service.set_frame_visibility_for_tests(false, true);
    service.set_pane_frame_position_for_tests(mez_mux::presentation::TerminalFramePosition::Top);
    let primary = service
        .attach_primary("primary", true, Size::new(20, 8).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    assert!(
        service
            .apply_attached_mux_action(&primary, MuxAction::SplitPaneHorizontal)
            .unwrap()
    );
    service.session.select_pane(&primary, "%1").unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::FocusPaneOnly(CopyPosition { line: 4, column: 0 }),
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert!(report.view_refresh_required);
    assert!(!report.full_redraw_required);
    assert_eq!(
        service.session().windows()[0].active_pane().id.as_str(),
        "%2"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that the pane agent status latency selector opens, populates with
/// the three allowed latency values, applies a selection as a pane-local
/// override, closes after selection, and surfaces the latency value in the
/// pane-frame context for pill rendering.
#[test]
fn runtime_pane_agent_status_selector_applies_latency_preference() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\"]\ndefault_model = \"gpt-5.5\"\n\n[model_profiles.default]\nprovider = \"openai\"\nmodel = \"gpt-5.5\"\nreasoning_profile = \"low\"\nlatency_preference = \"default\"\n\n[model_profiles.default.provider_options]\nreasoning_effort = \"low\"\n"
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
    service.cache_provider_model_catalog_for_tests(
        "openai",
        vec![mez_agent::ProviderModelInfo {
            id: "gpt-5.5".to_string(),
            display_name: None,
            reasoning_levels: vec!["low".to_string()],
            context_window_tokens: Some(1_050_000),
            capabilities: Vec::new(),
        }],
        vec!["low".to_string(), "high".to_string()],
    );

    let open_report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Latency,
                    },
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert!(open_report.view_refresh_required);
    assert!(!open_report.full_redraw_required);
    let latency_items = service
        .pane_agent_status_selector()
        .map(|selector| selector.items.clone())
        .unwrap_or_default();
    assert_eq!(
        latency_items,
        vec![
            "slow".to_string(),
            "default".to_string(),
            "fast".to_string()
        ]
    );
    let fast_index = latency_items
        .iter()
        .position(|item| item == "fast")
        .expect("latency selector should include fast");
    let select_report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::SelectPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Latency,
                        item_index: fast_index,
                    },
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert!(select_report.view_refresh_required);
    assert!(service.pane_agent_status_selector().is_none());
    let (_name, latency_profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(latency_profile.model, "gpt-5.5");
    assert_eq!(latency_profile.reasoning_profile.as_deref(), Some("low"));
    assert_eq!(latency_profile.latency_preference.as_deref(), Some("fast"));

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get("%1").unwrap();
    assert_eq!(pane_context.agent_latency.as_deref(), Some("fast"));
    assert_eq!(pane_context.agent_model.as_deref(), Some("gpt-5.5"));
    assert_eq!(pane_context.agent_reasoning.as_deref(), Some("low"));
}

/// Verifies that pane-frame latency controls are hidden for providers that do
/// not support a provider-visible latency preference.
///
/// DeepSeek profiles can still carry `latency_preference` metadata for identity
/// and preset display, but exposing a clickable latency selector would suggest
/// a provider request behavior that DeepSeek does not implement.
#[test]
fn runtime_pane_agent_status_hides_latency_for_unsupported_provider() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"
[agents]
default_provider = "deepseek"
default_model_profile = "deepseek-default"

[providers.deepseek]
kind = "deepseek"
models = ["deepseek-v4-pro"]
default_model = "deepseek-v4-pro"

[model_profiles.deepseek-default]
provider = "deepseek"
model = "deepseek-v4-pro"
reasoning_profile = "high"
latency_preference = "fast"

[model_profiles.deepseek-default.provider_options]
reasoning_effort = "high"
"#
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

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get("%1").unwrap();
    assert_eq!(
        pane_context.agent_latency, None,
        "unsupported providers should not render a latency status pill"
    );

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Latency,
                    },
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert!(
        service.pane_agent_status_selector().is_none(),
        "unsupported providers should not expose a latency dropdown"
    );
}

/// Verifies the DeepSeek thinking status pill is an immediate toggle rather
/// than a dropdown selector.
///
/// The pane frame exposes thinking next to reasoning only when the provider
/// supports the capability. Clicking it should reuse the `/thinking toggle`
/// runtime mutation path, update the pane-local generated profile, and refresh
/// the frame context without opening selector state.
#[test]
fn runtime_pane_agent_status_thinking_pill_toggles_deepseek_profile() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"deepseek\"\ndefault_model_profile = \"default\"\n\n[providers.deepseek]\nkind = \"deepseek\"\nmodels = [\"deepseek-v4-pro\"]\ndefault_model = \"deepseek-v4-pro\"\n\n[model_profiles.default]\nprovider = \"deepseek\"\nmodel = \"deepseek-v4-pro\"\nreasoning_profile = \"high\"\n\n[model_profiles.default.provider_options]\nreasoning_effort = \"high\"\n"
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

    let first_toggle = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Thinking,
                    },
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert!(first_toggle.view_refresh_required);
    assert!(!first_toggle.full_redraw_required);
    assert!(service.pane_agent_status_selector().is_none());
    let (_off_name, off_profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(off_profile.thinking_enabled(), Some(false));
    let off_config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    assert_eq!(
        off_config
            .frame_context
            .panes
            .get("%1")
            .and_then(|pane| pane.agent_thinking.as_deref()),
        Some("off")
    );

    let second_toggle = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Thinking,
                    },
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert!(second_toggle.view_refresh_required);
    assert!(service.pane_agent_status_selector().is_none());
    let (_on_name, on_profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(on_profile.thinking_enabled(), Some(true));
}

/// Verifies that pane-frame agent selectors remain modal until the user makes
/// an explicit selection or cancels them. Escape must close the selector
/// without leaking the escape byte into the active pane.
#[test]
fn runtime_pane_agent_status_selector_esc_closes_without_forwarding() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\", \"gpt-5.4\"]\ndefault_model = \"gpt-5.5\"\n\n[providers.openai.options]\nreasoning_effort = \"medium\"\n"
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
    let open_report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Model,
                    },
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert!(open_report.view_refresh_required);
    assert!(!open_report.full_redraw_required);
    assert!(service.pane_agent_status_selector().is_some());

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert!(report.view_refresh_required);
    assert!(!report.full_redraw_required);
    assert!(service.pane_agent_status_selector().is_none());
}

/// Verifies pane-frame model and reasoning dropdowns support keyboard
/// navigation. The active row should move with arrow input and Enter should
/// apply the same pane-scoped `/model` mutation as mouse selection.
#[test]
fn runtime_pane_agent_status_selector_accepts_keyboard_navigation() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\", \"gpt-5.4\"]\ndefault_model = \"gpt-5.5\"\n\n[providers.openai.options]\nreasoning_effort = \"medium\"\n"
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
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Model,
                    },
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    let (active_index, target_index) = service
        .pane_agent_status_selector()
        .map(|selector| {
            (
                selector.active_index,
                selector
                    .items
                    .iter()
                    .position(|item| item == "openai: gpt-5.4")
                    .expect("model selector should include gpt-5.4"),
            )
        })
        .expect("model selector should be open");
    let movement = if target_index < active_index {
        b"\x1b[A".to_vec()
    } else {
        b"\x1b[B".to_vec()
    };

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(movement),
                    TerminalClientLoopAction::ForwardToPane(b"\r".to_vec()),
                ],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert!(report.view_refresh_required);
    assert!(!report.full_redraw_required);
    assert!(service.pane_agent_status_selector().is_none());
    let (_name, model_profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(model_profile.model, "gpt-5.4");
}

/// Verifies mouse-wheel input over an open pane agent selector scrolls the
/// selector itself rather than falling through to pane scrollback.
#[test]
fn runtime_pane_agent_status_selector_scrolls_only_dropdown_contents() {
    let mut service = test_runtime_service();
    let models = (0..40)
        .map(|index| format!("\"gpt-test-{index:02}\""))
        .collect::<Vec<_>>()
        .join(", ");
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [{models}]\ndefault_model = \"gpt-test-00\"\n\n[model_profiles.default]\nprovider = \"openai\"\nmodel = \"gpt-test-00\"\n"
            ),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 12).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Model,
                    },
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(
        service
            .pane_agent_status_selector()
            .map(|selector| selector.scroll_offset),
        Some(0)
    );

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::ScrollPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Model,
                        lines: 3,
                    },
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
    assert!(report.view_refresh_required);
    assert!(!report.full_redraw_required);
    assert_eq!(
        service
            .pane_agent_status_selector()
            .map(|selector| selector.scroll_offset),
        Some(3)
    );

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::ScrollPaneAgentStatusSelector {
                        pane_index: 0,
                        field: PaneAgentStatusField::Model,
                        lines: -30,
                    },
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(
        service
            .pane_agent_status_selector()
            .map(|selector| selector.scroll_offset),
        Some(0)
    );
}
