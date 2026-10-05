//! Runtime tests for terminal overlays behavior.

use super::*;

/// Browser execution evidence uses one effective active profile rather than
/// mixing routed model and next-selection reasoning. Generic registrations on
/// the same pane cannot borrow native profile evidence.
#[test]
fn runtime_agent_browser_execution_metadata_is_identity_and_profile_scoped() {
    let mut service = test_runtime_service();
    service.replace_config_layers(vec![ConfigLayer {
        name: "primary".into(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"work\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"configured-model\"]\ndefault_model = \"configured-model\"\n[model_profiles.work]\nprovider = \"openai\"\nmodel = \"configured-model\"\nreasoning_profile = \"high\"\n".into(),
    }]).unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service.start_agent_prompt_turn("%1", "initial").unwrap();
    let mut effective = service
        .agent_turn_model_profile(&started.turn_id)
        .unwrap()
        .clone();
    effective.model = "routed-model".into();
    effective.reasoning_profile = None;
    effective.provider_options.remove("reasoning_effort");
    service.set_agent_turn_model_profile(&started.turn_id, effective);
    let unknown = service.message_service_mut().register_agent(
        mez_core::ids::PaneId::opaque("%1"),
        None,
        "generic",
        vec!["agent-harness".into()],
    );
    let inspect = |service: &mut RuntimeSessionService, id: &str| {
        service
            .agent_management_browser(&primary)
            .unwrap()
            .0
            .records()
            .iter()
            .find(|record| record.id == id)
            .unwrap()
            .metadata
            .clone()
    };
    let active = inspect(&mut service, "agent-%1");
    assert!(active.contains(&("Harness".into(), "mezzanine".into())));
    assert!(active.contains(&("Model".into(), "routed-model".into())));
    assert!(active.contains(&("Reasoning".into(), "—".into())));
    let (browser, _) = service.agent_management_browser(&primary).unwrap();
    let markdown = browser.render_page().raw_markdown;
    assert!(markdown.contains("| ID | Name | Kind | Harness | Model | Reasoning | State | Pane | Window | Group | Role | Objective | Project |"), "{markdown}");
    assert!(markdown.contains("routed-model"), "{markdown}");
    assert!(!markdown.contains("| Controls |"), "{markdown}");
    for query in ["mezzanine", "routed-model"] {
        let filtered = browser.render_page_matching(query).raw_markdown;
        assert!(filtered.contains("agent-%1"), "{filtered}");
        assert!(
            !filtered.contains(&format!("**{}**", unknown.agent_id)),
            "{filtered}"
        );
    }
    let theme = mez_mux::render::RichTextTheme {
        heading: mez_terminal::TerminalColor::Indexed(7),
        structural: mez_terminal::TerminalColor::Indexed(7),
        link: mez_terminal::TerminalColor::Indexed(7),
        inline_code: mez_terminal::TerminalColor::Indexed(7),
        table_alternate_row: mez_terminal::TerminalColor::Indexed(7),
        diff_addition: mez_terminal::TerminalColor::Indexed(2),
        diff_deletion: mez_terminal::TerminalColor::Indexed(1),
        syntax: None,
    };
    for width in [52, 53, 80, 100, 120, 240] {
        let layout = browser
            .render_list_layout("routed-model", width, &theme)
            .unwrap();
        assert!(!layout.record_ranges.is_empty());
        let payload_start = layout
            .record_ranges
            .iter()
            .map(|range| range.line)
            .min()
            .unwrap();
        for range in layout.record_ranges {
            assert_eq!(browser.records()[range.row].id, "agent-%1");
            assert!(range.line < layout.lines.len());
        }
        assert!(
            layout
                .lines
                .iter()
                .skip(payload_start)
                .all(|line| unicode_width::UnicodeWidthStr::width(line.display.as_str()) <= width),
            "execution table payload exceeds width={width}"
        );
    }
    let mut effective = service
        .agent_turn_model_profile(&started.turn_id)
        .unwrap()
        .clone();
    effective
        .provider_options
        .insert("reasoning_effort".into(), "medium".into());
    service.set_agent_turn_model_profile(&started.turn_id, effective.clone());
    assert!(inspect(&mut service, "agent-%1").contains(&("Reasoning".into(), "medium".into())));
    effective.reasoning_profile = Some("low".into());
    service.set_agent_turn_model_profile(&started.turn_id, effective);
    assert!(inspect(&mut service, "agent-%1").contains(&("Reasoning".into(), "low".into())));
    service
        .execute_attached_display_command(&primary, "list-agents")
        .unwrap();
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"/agent-%1\r".to_vec(),
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    let mut changed = service
        .agent_turn_model_profile(&started.turn_id)
        .unwrap()
        .clone();
    changed.model = "long-provider-model-with-searchable-suffix-雪".into();
    changed.reasoning_profile = Some("medium".into());
    service.set_agent_turn_model_profile(&started.turn_id, changed);
    service.refresh_agent_management_overlay(&primary).unwrap();
    let overlay = service.primary_display_overlay().unwrap();
    assert_eq!(overlay.search_query.as_deref(), Some("agent-%1"));
    let refreshed = &overlay.record_browser.as_ref().unwrap().browser;
    assert_eq!(refreshed.active_record_id(), Some("agent-%1"));
    for query in ["searchable-suffix-雪", "medium"] {
        let page = refreshed.render_page_matching(query).raw_markdown;
        assert!(page.contains("agent-%1"), "{page}");
        assert!(
            page.contains("long-provider-model-with-searchable-suffix-雪"),
            "{page}"
        );
    }
    let foreign = inspect(&mut service, unknown.agent_id.as_str());
    for key in ["Harness", "Model", "Reasoning"] {
        assert!(
            foreign.contains(&(key.into(), "unavailable".into())),
            "{foreign:?}"
        );
    }
    service
        .finish_agent_turn("%1", &started.turn_id, AgentTurnState::Completed)
        .unwrap();
    let idle = inspect(&mut service, "agent-%1");
    assert!(idle.contains(&("Model".into(), "configured-model".into())));
    assert!(idle.contains(&("Reasoning".into(), "high".into())));
    service
        .agent_shell_store_mut()
        .start_new_conversation("%1")
        .unwrap();
    service.ensure_primary_agent_name("%1").unwrap();
    let rebound = inspect(&mut service, "agent-%1");
    assert!(rebound.contains(&("Model".into(), "configured-model".into())));
    assert!(!rebound.contains(&("Model".into(), "routed-model".into())));
}

/// Project labels follow each live pane's canonical current directory and
/// deepest trust snapshot, not token mappings, titles or inherited projects.
/// Moving the pane and withholding nested trust must refresh only inert labels
/// while retaining exact lifecycle controls and searchable full project paths.
#[test]
fn runtime_agent_management_browser_projects_follow_current_trust() {
    use crate::security::project::{ProjectTrustStore, TrustDecision};

    let root = temp_root("agent-browser-projects");
    let first = root.join("first-project");
    let second = root.join("second-project");
    let nested = second.join("nested");
    fs::create_dir_all(&first).unwrap();
    fs::create_dir_all(&nested).unwrap();
    let first = fs::canonicalize(first).unwrap();
    let second = fs::canonicalize(second).unwrap();
    let nested = fs::canonicalize(nested).unwrap();
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "fixture task")
        .unwrap();
    service.stop_agent_turn_for_pane("%1").unwrap();
    let mut trust = ProjectTrustStore::default();
    trust
        .decide_at(first.clone(), TrustDecision::Trusted, None, 1)
        .unwrap();
    trust
        .decide_at(second.clone(), TrustDecision::Trusted, None, 1)
        .unwrap();
    service.set_project_trust_store(trust.clone(), None);
    service.set_pane_current_working_directory("%1", first.clone());
    let (browser, targets) = service.agent_management_browser(&primary).unwrap();
    let record = browser
        .records()
        .iter()
        .find(|record| record.id == "agent-%1")
        .unwrap();
    assert!(
        record
            .metadata
            .contains(&("Project".into(), first.to_string_lossy().into_owned()))
    );
    assert!(record.metadata.iter().any(|(key, _)| key == "Controls"));
    assert!(targets["agent-%1"].lifecycle.is_some());
    assert!(browser.render_page().markdown.contains("Project"));
    assert!(!browser.render_page().markdown.contains("| Controls |"));
    // Actual interactive opening must reuse the textual command's snapshot.
    // This detects the former second trust refresh and per-pane lookup pass.
    crate::runtime::control::agent_browser::take_agent_browser_project_lookup_counts();
    service
        .execute_attached_display_command(&primary, "list-agents")
        .unwrap();
    assert_eq!(
        crate::runtime::control::agent_browser::take_agent_browser_project_lookup_counts(),
        (1, 1)
    );
    let mounted = &service
        .primary_display_overlay()
        .unwrap()
        .record_browser
        .as_ref()
        .unwrap()
        .browser;
    assert_eq!(mounted.records(), browser.records());
    assert!(
        browser
            .render_page_matching("first-project")
            .markdown
            .contains("agent-%1")
    );
    service.set_pane_current_working_directory("%1", nested.clone());
    let (browser, _) = service.agent_management_browser(&primary).unwrap();
    let record = browser
        .records()
        .iter()
        .find(|record| record.id == "agent-%1")
        .unwrap();
    assert!(
        record
            .metadata
            .contains(&("Project".into(), second.to_string_lossy().into_owned()))
    );
    trust
        .decide_at(nested.clone(), TrustDecision::Rejected, None, 2)
        .unwrap();
    service.set_project_trust_store(trust, None);
    let (browser, _) = service.agent_management_browser(&primary).unwrap();
    let record = browser
        .records()
        .iter()
        .find(|record| record.id == "agent-%1")
        .unwrap();
    assert!(record.metadata.contains(&("Project".into(), "—".into())));
    service.set_pane_current_working_directory("%1", root.join("missing"));
    let (browser, _) = service.agent_management_browser(&primary).unwrap();
    let record = browser
        .records()
        .iter()
        .find(|record| record.id == "agent-%1")
        .unwrap();
    assert!(
        record
            .metadata
            .contains(&("Project".into(), "unavailable".into()))
    );
    service.set_pane_current_working_directory("%1", nested.clone());
    let database = root.join("trust.sqlite");
    let mut persisted = ProjectTrustStore::default();
    persisted
        .decide_at(second.clone(), TrustDecision::Trusted, None, 3)
        .unwrap();
    persisted.save_to_file(&database).unwrap();
    service.set_project_trust_store(persisted.clone(), Some(database.clone()));
    persisted
        .decide_at(nested, TrustDecision::Revoked, None, 4)
        .unwrap();
    persisted.save_to_file(&database).unwrap();
    let (browser, _) = service.agent_management_browser(&primary).unwrap();
    let record = browser
        .records()
        .iter()
        .find(|record| record.id == "agent-%1")
        .unwrap();
    assert!(record.metadata.contains(&("Project".into(), "—".into())));
    assert!(
        record
            .metadata
            .contains(&("Project status".into(), "project-trust-revoked".into()))
    );
    fs::remove_file(&database).unwrap();
    fs::create_dir(&database).unwrap();
    let (browser, _) = service.agent_management_browser(&primary).unwrap();
    let record = browser
        .records()
        .iter()
        .find(|record| record.id == "agent-%1")
        .unwrap();
    assert!(
        record
            .metadata
            .contains(&("Project".into(), "unavailable".into()))
    );
    assert!(
        browser
            .render_page()
            .markdown
            .contains("Project trust refresh failed")
    );
    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// A persisted selection whose live apply initially fails must retain the list,
/// actual active marker and original diagnostic after guarded reconciliation.
#[test]
fn runtime_theme_browser_partial_failure_refreshes_actual_active_theme() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let root = temp_root("theme-browser-partial");
    service.set_config_root(root.clone());
    service
        .execute_attached_display_command(&primary, "list-themes")
        .unwrap();
    let input = |service: &mut RuntimeSessionService, bytes: &[u8]| {
        service
            .apply_attached_terminal_step_plan(
                &primary,
                &AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::ForwardToPane(bytes.to_vec())],
                    output_lines: Vec::new(),
                    output_line_style_spans: Vec::new(),
                    input_hangup: false,
                    output_hangup: false,
                    error_roles: Vec::new(),
                },
            )
            .unwrap();
    };
    input(&mut service, b"/dracula\r");
    service.integration.set_theme_selection_fault("apply");
    input(&mut service, b"\r");
    assert_eq!(service.ui_theme().name, "dracula");
    let overlay = service.primary_display_overlay().unwrap();
    assert_eq!(overlay.search_query.as_deref(), Some("dracula"));
    assert!(
        overlay
            .lines
            .iter()
            .any(|line| line.contains("persisted=true"))
    );
    let record = overlay
        .record_browser
        .as_ref()
        .unwrap()
        .browser
        .records()
        .iter()
        .find(|record| record.id == "dracula")
        .unwrap();
    assert!(
        record
            .metadata
            .iter()
            .any(|(key, value)| key == "Active" && value == "★ active")
    );
    let failed = overlay
        .record_browser
        .as_ref()
        .unwrap()
        .browser
        .render_page()
        .markdown;
    assert!(failed.starts_with("Error:"), "{failed}");
    input(&mut service, b"\r");
    let succeeded = service
        .primary_display_overlay()
        .unwrap()
        .record_browser
        .as_ref()
        .unwrap()
        .browser
        .render_page()
        .markdown;
    assert!(succeeded.starts_with("Notice:"), "{succeeded}");
    assert!(!succeeded.contains("Error:"), "{succeeded}");
    fs::remove_dir_all(root).unwrap();
}

/// Theme navigation must remain inert; explicit Enter applies once and retains
/// the searchable list rather than opening detail or dismissing the picker.
#[test]
fn runtime_theme_browser_applies_and_refreshes_in_place() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(35, 12).unwrap(), 120)
        .unwrap();
    let initial = service.ui_theme().name.clone();
    service
        .execute_attached_display_command(&primary, "list-themes")
        .unwrap();
    assert!(
        service
            .primary_display_overlay()
            .unwrap()
            .record_browser
            .is_some()
    );
    let input = |service: &mut RuntimeSessionService, bytes: &[u8]| {
        service
            .apply_attached_terminal_step_plan(
                &primary,
                &AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::ForwardToPane(bytes.to_vec())],
                    output_lines: Vec::new(),
                    output_line_style_spans: Vec::new(),
                    input_hangup: false,
                    output_hangup: false,
                    error_roles: Vec::new(),
                },
            )
            .unwrap();
    };
    input(&mut service, b"/dracula\r");
    assert_eq!(service.ui_theme().name, initial);
    let stale = service.primary_display_overlay().unwrap().selections[0].action_id;
    input(&mut service, b"\r");
    assert_eq!(service.ui_theme().name, "dracula");
    let overlay = service.primary_display_overlay().unwrap();
    assert_eq!(overlay.search_query.as_deref(), Some("dracula"));
    let feedback = overlay
        .record_browser
        .as_ref()
        .unwrap()
        .browser
        .render_page()
        .markdown;
    assert!(feedback.starts_with("Notice: theme=dracula"), "{feedback}");
    assert!(feedback.contains("persisted=false"), "{feedback}");
    assert!(!feedback.contains("Error:"), "{feedback}");
    assert!(
        !overlay
            .record_browser
            .as_ref()
            .unwrap()
            .browser
            .is_detail_view()
    );
    assert!(
        service
            .execute_primary_display_overlay_action(&primary, stale)
            .unwrap()
    );
    input(&mut service, b"/absent\r");
    input(&mut service, b"\r");
    assert_eq!(service.ui_theme().name, "dracula");
    assert!(
        service
            .primary_display_overlay()
            .unwrap()
            .selections
            .is_empty()
    );
    input(&mut service, b"/kanagawa\r");
    let action = service.primary_display_overlay().unwrap().selections[0].action_id;
    assert!(
        service
            .execute_primary_display_overlay_action(&primary, action)
            .unwrap()
    );
    assert_eq!(service.ui_theme().name, "kanagawa");
    let feedback = service
        .primary_display_overlay()
        .unwrap()
        .record_browser
        .as_ref()
        .unwrap()
        .browser
        .render_page()
        .markdown;
    assert!(feedback.starts_with("Notice: theme=kanagawa"), "{feedback}");
    assert!(!feedback.contains("Error:"), "{feedback}");
    input(&mut service, b"r");
    let refreshed = service
        .primary_display_overlay()
        .unwrap()
        .record_browser
        .as_ref()
        .unwrap()
        .browser
        .render_page()
        .markdown;
    assert!(
        !refreshed.contains("Notice:") && !refreshed.contains("Error:"),
        "{refreshed}"
    );
    assert_eq!(
        service
            .primary_display_overlay()
            .unwrap()
            .search_query
            .as_deref(),
        Some("kanagawa")
    );
}

/// A task settling while close is armed invalidates that exact confirmation;
/// confirmation cannot close a replacement task or drift to a neighboring row.
#[test]
fn runtime_agent_management_browser_rejects_changed_close_target() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    let remote = service
        .create_window_with_pane_process(&primary, "remote", true, None)
        .unwrap();
    let pane = remote.pane_id.to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane)
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "remote task")
        .unwrap();
    service.session.select_pane_global(&primary, "%1").unwrap();
    service
        .execute_attached_display_command(&primary, "list-agents")
        .unwrap();
    let input = |service: &mut RuntimeSessionService, bytes: &[u8]| {
        service
            .apply_attached_terminal_step_plan(
                &primary,
                &AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::ForwardToPane(bytes.to_vec())],
                    output_lines: Vec::new(),
                    output_line_style_spans: Vec::new(),
                    input_hangup: false,
                    output_hangup: false,
                    error_roles: Vec::new(),
                },
            )
            .unwrap();
    };
    input(&mut service, format!("/agent-{pane}\r").as_bytes());
    input(&mut service, b"d");
    service.stop_agent_turn_for_pane(&pane).unwrap();
    input(&mut service, b"y");
    assert!(service.find_pane_descriptor(&pane).is_some());
    assert_eq!(service.active_pane_id().unwrap().as_str(), "%1");
    assert!(
        service
            .primary_display_overlay()
            .unwrap()
            .lines
            .iter()
            .any(|line| line.contains("changed"))
    );
    input(&mut service, b"\r");
    assert!(service.primary_display_overlay().is_none());
    assert_eq!(service.active_pane_id().unwrap().as_str(), pane);
    service.terminate_all_pane_processes().unwrap();
}

/// Management keys must affect the selected agent across windows, not the pane
/// that opened the browser. Pause/resume and interruption retain focus; close
/// requires explicit target-labelled confirmation and supports cancellation.
#[test]
fn runtime_agent_management_browser_controls_selected_remote_agent() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "owner task")
        .unwrap();
    service.stop_agent_turn_for_pane("%1").unwrap();
    let remote = service
        .create_window_with_pane_process(&primary, "remote", true, None)
        .unwrap();
    let pane = remote.pane_id.to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane)
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "remote task")
        .unwrap();
    service.session.select_pane_global(&primary, "%1").unwrap();
    service
        .execute_attached_display_command(&primary, "list-agents")
        .unwrap();
    let input = |service: &mut RuntimeSessionService, bytes: &[u8]| {
        service
            .apply_attached_terminal_step_plan(
                &primary,
                &AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::ForwardToPane(bytes.to_vec())],
                    output_lines: Vec::new(),
                    output_line_style_spans: Vec::new(),
                    input_hangup: false,
                    output_hangup: false,
                    error_roles: Vec::new(),
                },
            )
            .unwrap();
    };
    input(&mut service, format!("/agent-{pane}\r").as_bytes());
    input(&mut service, b"p");
    assert_eq!(service.agent_human_pause_status(&pane), Some("paused"));
    assert_eq!(service.active_pane_id().unwrap().as_str(), "%1");
    input(&mut service, b"p");
    assert!(service.agent_human_pause_status(&pane).is_none());
    input(&mut service, b"i");
    assert_eq!(service.active_pane_id().unwrap().as_str(), "%1");
    assert!(!service.agent_shell_pane_has_active_turn(&pane));
    input(&mut service, b"d");
    assert!(service.find_pane_descriptor(&pane).is_some());
    input(&mut service, b"n");
    assert!(service.find_pane_descriptor(&pane).is_some());
    input(&mut service, b"d");
    input(&mut service, b"y");
    assert!(service.find_pane_descriptor(&pane).is_none());
    assert!(service.find_pane_descriptor("%1").is_some());
    assert_eq!(service.active_pane_id().unwrap().as_str(), "%1");
    service.terminate_all_pane_processes().unwrap();
}

/// The administrative list mounts from a normal terminal without creating an
/// agent session, and must not inherit the model-facing discovery row cap.
#[test]
fn runtime_agent_management_browser_mounts_uncapped() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    for index in 0..70 {
        service.message_service_mut().register_agent(
            None,
            None,
            format!("fixture-{index}"),
            Vec::new(),
        );
    }
    service
        .execute_attached_display_command(&primary, "list-agents")
        .unwrap();
    let state = service
        .primary_display_overlay()
        .unwrap()
        .record_browser
        .as_ref()
        .unwrap();
    assert_eq!(state.browser.records().len(), 70);
    assert!(service.agent_shell_store().get("%1").is_none());
    assert!(
        service
            .execute_terminal_command(&primary, "list-agents --all")
            .is_err()
    );
}

/// A configured command binding must use the same typed browser handoff as
/// prompt submission, preserving attached execution report effects.
#[test]
fn runtime_terminal_record_browser_configured_binding_mounts() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .create_window_with_pane_process(&primary, "work", false, None)
        .unwrap();
    let mut config = TerminalClientLoopConfig::default();
    config.command_bindings.insert(
        mez_mux::input::KeyChord::new(mez_mux::input::KeyCode::Char('x')),
        "choose-window".into(),
    );
    let action = crate::host::terminal::route_client_input(b"\x01x", &config).unwrap();
    assert!(matches!(
        action,
        TerminalClientLoopAction::ExecuteCommand(_)
    ));
    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![action],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(report.mux_actions_applied, 1);
    assert!(report.registry_persistence_required);
    assert!(
        service
            .primary_display_overlay()
            .and_then(|overlay| overlay.record_browser.as_ref())
            .is_some()
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Explicit chooser focus must preserve zen focus-label feedback and its
/// receipt contract even though overlay input bypasses ordinary mux wrappers.
#[test]
fn runtime_terminal_record_browser_activation_preserves_zen_feedback() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .create_window_with_pane_process(&primary, "work", false, None)
        .unwrap();
    service.set_terminal_zen_mode_for_tests(true);
    service
        .execute_attached_display_command(&primary, "choose-window")
        .unwrap();
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"\x1b[B\r".to_vec(),
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(service.session().active_window().unwrap().name, "work");
    let response: serde_json::Value = serde_json::from_str(&service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"zen-view","method":"terminal/view","params":{"client_size":{"columns":80,"rows":24}}}"#, &primary)).unwrap();
    assert_eq!(
        response["result"]["presentation_ids"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Normal command-prompt submission mounts the typed chooser. Later sequence
/// output replaces it rather than reusing a stale handoff; plain RPC execution
/// retains textual output without installing interactive state.
#[test]
fn runtime_terminal_record_browser_prompt_and_sequence_ownership() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .create_window_with_pane_process(&primary, "work", false, None)
        .unwrap();
    let output = service
        .execute_terminal_command(&primary, "choose-window")
        .unwrap();
    assert!(output.contains("choose-window"));
    assert!(service.primary_display_overlay().is_none());
    service.enter_primary_command_prompt("").unwrap();
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"choose-window\r".to_vec(),
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
        service
            .primary_display_overlay()
            .unwrap()
            .record_browser
            .is_some()
    );
    service
        .execute_attached_display_command(&primary, "choose-window; list-windows")
        .unwrap();
    assert!(
        service
            .primary_display_overlay()
            .unwrap()
            .record_browser
            .is_none()
    );
    service
        .execute_attached_display_command(&primary, "choose-window")
        .unwrap();
    assert!(
        service
            .primary_display_overlay()
            .unwrap()
            .record_browser
            .is_some()
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Search and refresh cannot activate hidden records or retained stale actions.
/// A second primary keeps its own overlay, and Enter uses the same stable ID
/// as mouse activation without creating an agent shell session.
#[test]
fn runtime_terminal_record_browser_filters_refreshes_and_isolates_clients() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(35, 12).unwrap(), 120)
        .unwrap();
    service
        .create_window_with_pane_process(&primary, "work 界", false, None)
        .unwrap();
    let original = service.session().active_window().unwrap().id.clone();
    service
        .execute_attached_display_command(&primary, "choose-window")
        .unwrap();
    let stale = service.primary_display_overlay().unwrap().selections[0].action_id;
    let input = |service: &mut RuntimeSessionService, bytes: &[u8]| {
        service
            .apply_attached_terminal_step_plan(
                &primary,
                &AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::ForwardToPane(bytes.to_vec())],
                    output_lines: Vec::new(),
                    output_line_style_spans: Vec::new(),
                    input_hangup: false,
                    output_hangup: false,
                    error_roles: Vec::new(),
                },
            )
            .unwrap();
    };
    input(&mut service, b"/absent\r");
    input(&mut service, b"\r");
    assert_eq!(service.session().active_window().unwrap().id, original);
    assert!(
        service
            .primary_display_overlay()
            .unwrap()
            .selections
            .is_empty()
    );
    input(&mut service, b"r");
    assert_eq!(
        service
            .primary_display_overlay()
            .unwrap()
            .search_query
            .as_deref(),
        Some("absent")
    );
    assert!(
        service
            .execute_primary_display_overlay_action(&primary, stale)
            .unwrap()
    );
    assert_eq!(service.session().active_window().unwrap().id, original);
    let other = service
        .attach_primary("other", true, Size::new(35, 12).unwrap(), 121)
        .unwrap();
    service
        .prepare_client_render(&other, ClientViewRole::Primary)
        .unwrap();
    assert!(service.primary_display_overlay().is_none());
    service
        .prepare_client_render(&primary, ClientViewRole::Primary)
        .unwrap();
    assert!(
        service
            .primary_display_overlay()
            .unwrap()
            .record_browser
            .is_some()
    );
    input(&mut service, b"/work\r");
    input(&mut service, b"r");
    assert_eq!(
        service
            .primary_display_overlay()
            .unwrap()
            .search_query
            .as_deref(),
        Some("work")
    );
    input(&mut service, b"\r");
    assert!(service.primary_display_overlay().is_none());
    assert_eq!(service.session().active_window().unwrap().name, "work 界");
    service.terminate_all_pane_processes().unwrap();
}

/// Terminal chooser commands must mount retained browser state without an agent
/// session. Navigation is inert and explicit selection alone changes focus.
#[test]
fn runtime_terminal_record_browser_mounts_from_attached_command() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(35, 12).unwrap(), 120)
        .unwrap();
    service
        .create_window_with_pane_process(&primary, "work", false, None)
        .unwrap();
    let original = service.session().active_window().unwrap().id.clone();
    service
        .execute_attached_display_command(&primary, "choose-window")
        .unwrap();
    assert!(
        service
            .primary_display_overlay()
            .unwrap()
            .record_browser
            .is_some()
    );
    assert!(service.agent_shell_store().get("%1").is_none());
    assert_eq!(service.session().active_window().unwrap().id, original);
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies that command display output is owned by runtime state instead of a
/// nested terminal loop. The modal overlay must render through the normal
/// primary client view, consume user input while active, and clear on Escape or
/// `q` without forwarding those bytes into the active pane.
#[test]
fn runtime_primary_display_overlay_renders_and_clears_via_terminal_step() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(40, 6).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    service
        .apply_pane_output_bytes(pane_id, b"prompt$ ".to_vec())
        .unwrap();
    let base_view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(40, 6).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert!(base_view.cursor_visible);
    service
        .show_primary_display_overlay(vec![
            "first display line".to_string(),
            "second display line".to_string(),
        ])
        .unwrap();

    let overlay_view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(40, 6).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(overlay_view.lines[0].trim_end(), "mezzanine command output");
    assert!(
        overlay_view
            .lines
            .iter()
            .any(|line| line.contains("first display line")),
        "{:?}",
        overlay_view.lines
    );
    assert!(!overlay_view.cursor_visible);

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
    assert!(report.full_redraw_required);

    let cleared_view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(40, 6).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert!(
        !cleared_view
            .lines
            .iter()
            .any(|line| line.contains("mezzanine command output")),
        "{:?}",
        cleared_view.lines
    );
    assert!(cleared_view.cursor_visible);
    assert_eq!(cleared_view.cursor_row, base_view.cursor_row);
    assert_eq!(cleared_view.cursor_column, base_view.cursor_column);

    service
        .show_primary_display_overlay(vec!["third display line".to_string()])
        .unwrap();
    let quit = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"q".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(quit.forwarded_bytes, 0);
    assert!(quit.view_refresh_required);
    assert!(service.primary_display_overlay().is_none());
}

/// Verifies the client-local Iroh health pill disappears while a primary pager
/// owns the view and returns after dismissal. The pager already replaces the
/// ordinary window-status surface, so retaining the locally composed pill
/// would leak a status element over modal command content for remote clients.
#[test]
fn runtime_iroh_status_slot_hides_while_primary_display_overlay_is_active() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(40, 6).unwrap(), 120)
        .unwrap();
    let config = TerminalClientLoopConfig {
        frame_context: crate::host::terminal::TerminalFrameContext {
            window_status: Some(mez_mux::presentation::TerminalWindowStatusContext {
                template: "#{iroh.status}".to_string(),
                active_pane_working_directory: None,
                status_pills: std::collections::BTreeMap::new(),
                system_uptime: String::new(),
                datetime_local: String::new(),
            }),
            ..crate::host::terminal::TerminalFrameContext::default()
        },
        ..TerminalClientLoopConfig::default()
    };
    let base_view = service
        .render_client_view(ClientViewRole::Primary, Size::new(40, 6).unwrap(), &config)
        .unwrap()
        .unwrap();
    assert!(
        service
            .terminal_iroh_status_slot(&base_view, &config)
            .is_some()
    );

    service
        .show_primary_display_overlay(vec!["pager content".to_string()])
        .unwrap();
    let pager_view = service
        .render_client_view(ClientViewRole::Primary, Size::new(40, 6).unwrap(), &config)
        .unwrap()
        .unwrap();
    assert!(
        service
            .terminal_iroh_status_slot(&pager_view, &config)
            .is_none()
    );

    service
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
    let restored_view = service
        .render_client_view(ClientViewRole::Primary, Size::new(40, 6).unwrap(), &config)
        .unwrap()
        .unwrap();
    assert!(
        service
            .terminal_iroh_status_slot(&restored_view, &config)
            .is_some()
    );
}

/// Verifies zen mode removes the client-local Iroh pill slot.
///
/// Remote attach clients compose the connection pill from optional server
/// metadata, so a zen transition must omit the slot rather than merely erase
/// the server-rendered window frame text.
#[test]
fn runtime_zen_mode_omits_iroh_status_slot() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(40, 6).unwrap(), 120)
        .unwrap();
    service.set_terminal_zen_mode_for_tests(true);
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let view = service
        .render_client_view(ClientViewRole::Primary, Size::new(40, 6).unwrap(), &config)
        .unwrap()
        .unwrap();

    assert!(service.terminal_iroh_status_slot(&view, &config).is_none());
}

/// Verifies ordinary input that has no pager binding remains captured by the
/// modal overlay instead of falling through to the active pane.
#[test]
fn runtime_primary_display_overlay_consumes_unbound_pane_input() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(40, 6).unwrap(), 120)
        .unwrap();
    service
        .show_primary_display_overlay(vec!["modal display line".to_string()])
        .unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"x".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert!(!report.view_refresh_required);
    assert!(service.primary_display_overlay().is_some());
}

/// Verifies ordinary pager output preserves logical rows without word wrapping.
#[test]
fn runtime_primary_display_overlay_preserves_unwrapped_plain_content() {
    let mut service = test_runtime_service_with_size(Size::new(12, 6).unwrap());
    service
        .show_primary_display_overlay(vec!["alpha beta gamma".to_string()])
        .unwrap();

    let overlay = service.primary_display_overlay().unwrap();
    assert_eq!(overlay.lines, vec!["alpha beta gamma"]);
}

/// Verifies the Iroh status pager retains exact-client source identity and
/// advertises its refresh cadence to remote attached clients.
#[test]
fn runtime_iroh_status_overlay_retains_exact_client_live_source() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    service
        .execute_attached_display_command(&primary, "show-iroh-status")
        .unwrap();

    let source = service
        .primary_display_overlay()
        .and_then(|overlay| overlay.live_source.as_ref())
        .expect("show-iroh-status should install a live source");
    let wide_lines = service.primary_display_overlay().unwrap().lines.clone();
    let next_due_ms = source.next_due_ms;
    assert!(matches!(
        &source.source,
        crate::runtime::service_state::RuntimeLiveOverlaySourceKind::IrohStatus {
            client_id,
        } if client_id == primary.as_str()
    ));
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        view.animation_refresh_interval_ms,
        source.refresh_interval_ms
    );
    service
        .resize_attached_primary_terminal(&primary, Size::new(30, 12).unwrap())
        .unwrap();
    let resized = service.primary_display_overlay().unwrap();
    assert_ne!(resized.lines, wide_lines);
    assert_eq!(
        resized
            .live_source
            .as_ref()
            .map(|source| source.next_due_ms),
        Some(next_due_ms)
    );
}

/// Verifies terminal command feedback that does not need the pager uses the
/// window status line and never mutates the active pane's retained log.
#[test]
fn runtime_non_pager_command_feedback_stays_out_of_pane_log() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    service
        .apply_pane_output_bytes(pane_id.clone(), b"prompt$ ".to_vec())
        .unwrap();
    let before = service
        .pane_screen(&pane_id)
        .unwrap()
        .normal_content_lines();

    service
        .execute_attached_display_command(&primary, "refresh-client")
        .unwrap();

    assert!(service.primary_display_overlay().is_none());
    assert!(service.primary_error_status_overlay().is_some());
    assert_eq!(
        service
            .pane_screen(&pane_id)
            .unwrap()
            .normal_content_lines(),
        before
    );
}

/// Verifies zen mode does not recreate a status bar for transient feedback.
///
/// Feedback remains retained for explicit history and command surfaces, but
/// rendering must preserve the reclaimed bottom content row instead of using
/// the legacy no-window-frame fallback.
#[test]
fn runtime_zen_mode_does_not_overlay_transient_status_on_content() {
    let mut service = test_runtime_service_with_size(Size::new(20, 4).unwrap());
    let primary = service
        .attach_primary("primary", true, Size::new(20, 4).unwrap(), 120)
        .unwrap();
    service.set_terminal_zen_mode_for_tests(true);
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"one\r\ntwo\r\nthree\r\nbottom");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .execute_attached_display_command(&primary, "refresh-client")
        .unwrap();
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let view = service
        .render_client_view(ClientViewRole::Primary, Size::new(20, 4).unwrap(), &config)
        .unwrap()
        .unwrap();

    assert!(service.primary_error_status_overlay().is_some());
    assert!(
        view.lines
            .last()
            .is_some_and(|line| line.contains("bottom"))
    );
    assert!(!view.lines.iter().any(|line| line.contains("refreshed")));
}

/// Verifies topology list commands open the command pager with rendered table
/// rows instead of exposing their compact machine-oriented state strings.
///
/// Windows use the shared session formatter while groups, panes, and clients
/// use runtime-aware formatters, so this covers every command path that must
/// emit pager-friendly Markdown tables.
#[test]
fn runtime_topology_lists_render_tables_in_command_pager() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    for (command, heading) in [
        ("list-windows", "window"),
        ("list-groups", "group"),
        ("list-panes", "pane"),
        ("list-clients", "client"),
    ] {
        service
            .execute_attached_display_command(&primary, command)
            .unwrap();
        let overlay = service
            .primary_display_overlay()
            .expect("topology list command should open the command pager");
        assert!(
            overlay.lines.iter().any(|line| line.contains(heading)),
            "{command} should retain its table heading: {:?}",
            overlay.lines
        );
        assert!(
            overlay.lines.iter().any(|line| line.contains('│')),
            "{command} should render Markdown as table rows: {:?}",
            overlay.lines
        );
    }
}

/// Verifies keyboard movement inside a primary command-output pager refreshes
/// through the retained-frame diff path.
///
/// Navigating a selectable pager row only changes the active highlight and
/// optional viewport offset. It must not invalidate the whole attached output
/// frame, otherwise remote terminals flicker during routine list navigation.
#[test]
fn runtime_primary_display_overlay_keyboard_navigation_requests_diff_refresh() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .create_window_with_pane_process(&primary, "work", false, None)
        .unwrap();

    service
        .execute_attached_display_command(&primary, "choose-window")
        .unwrap();
    assert_eq!(
        service
            .primary_display_overlay()
            .and_then(|overlay| overlay.active_selection_index),
        Some(0)
    );
    let initial_view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    let initial_active_row = initial_view
        .lines
        .iter()
        .position(|line| line.starts_with("> "))
        .expect("overlay should show an active selector gutter");
    assert!(
        initial_view
            .lines
            .iter()
            .enumerate()
            .any(|(index, line)| index != initial_active_row && line.starts_with("  ")),
        "{initial_view:?}"
    );

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b[B".to_vec())],
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
            .primary_display_overlay()
            .and_then(|overlay| overlay.active_selection_index),
        Some(1)
    );
    let moved_view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    let moved_active_row = moved_view
        .lines
        .iter()
        .position(|line| line.starts_with("> "))
        .expect("overlay should keep an active selector gutter after navigation");
    assert_ne!(moved_active_row, initial_active_row, "{moved_view:?}");
    assert!(
        moved_view
            .lines
            .iter()
            .enumerate()
            .any(|(index, line)| index != moved_active_row && line.starts_with("  ")),
        "{moved_view:?}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies mouse-wheel scrolling inside a primary command-output pager uses a
/// light view refresh instead of a full terminal-frame redraw.
///
/// The overlay renderer already produces a complete next view for the changed
/// rows, so the attach client can keep diffing against the retained frame.
#[test]
fn runtime_primary_display_overlay_mouse_scroll_requests_diff_refresh() {
    let mut service = test_runtime_service_with_size(Size::new(40, 6).unwrap());
    let primary = service
        .attach_primary("primary", true, Size::new(40, 6).unwrap(), 120)
        .unwrap();
    service
        .show_primary_display_overlay(
            (0..20)
                .map(|index| format!("display line {index:02}"))
                .collect(),
        )
        .unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::ScrollDisplayOverlay { lines: 2 },
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
            .primary_display_overlay()
            .map(|overlay| overlay.scroll_offset),
        Some(2)
    );
}

/// Verifies forward text search inside a primary command-output pager, including
/// empty-query repeat and wraparound back to the first matching line.
#[test]
fn runtime_primary_display_overlay_search_repeats_and_wraps() {
    let mut service = test_runtime_service_with_size(Size::new(80, 10).unwrap());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 10).unwrap(), 120)
        .unwrap();
    service
        .show_primary_display_overlay(vec![
            "alpha opening".to_string(),
            "needle first".to_string(),
            "middle text".to_string(),
            "needle second".to_string(),
            "closing text".to_string(),
        ])
        .unwrap();

    let initial_search = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"/".to_vec()),
                    TerminalClientLoopAction::ForwardToPane(b"needle".to_vec()),
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

    assert_eq!(initial_search.forwarded_bytes, 0);
    assert!(initial_search.view_refresh_required);
    assert!(!initial_search.full_redraw_required);
    assert_eq!(
        service
            .primary_display_overlay()
            .and_then(|overlay| overlay.search_query.as_deref()),
        Some("needle")
    );
    assert_eq!(
        service.primary_display_overlay().and_then(|overlay| overlay
            .search_match
            .map(|search_match| search_match.line_index)),
        Some(1)
    );

    let next_match = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"/".to_vec()),
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

    assert_eq!(next_match.forwarded_bytes, 0);
    assert!(next_match.view_refresh_required);
    assert_eq!(
        service.primary_display_overlay().and_then(|overlay| overlay
            .search_match
            .map(|search_match| search_match.line_index)),
        Some(3)
    );
    assert_eq!(
        service
            .primary_display_overlay()
            .and_then(|overlay| overlay.search_status.as_deref()),
        None
    );

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"/".to_vec()),
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

    assert_eq!(
        service.primary_display_overlay().and_then(|overlay| overlay
            .search_match
            .map(|search_match| search_match.line_index)),
        Some(1)
    );
    assert_eq!(
        service
            .primary_display_overlay()
            .and_then(|overlay| overlay.search_status.as_deref()),
        None
    );

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"/".to_vec()),
                    TerminalClientLoopAction::ForwardToPane(b"absent".to_vec()),
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

    let overlay = service.primary_display_overlay().unwrap();
    assert_eq!(
        overlay
            .search_match
            .map(|search_match| search_match.line_index),
        Some(1)
    );
    assert_eq!(
        overlay.search_status.as_deref(),
        Some("pattern not found: absent")
    );
}

/// Verifies live overlay replacement preserves durable interaction state and
/// does not report a change when rebuilt content is identical.
#[test]
fn runtime_live_display_overlay_replacement_reconciles_interaction_state() {
    let mut service = test_runtime_service_with_size(Size::new(40, 6).unwrap());
    service
        .show_primary_display_overlay(vec![
            "header".to_string(),
            "needle old".to_string(),
            "tail".to_string(),
        ])
        .unwrap();
    {
        let overlay = service.primary_display_overlay_mut().unwrap();
        overlay.scroll_offset = 2;
        overlay.search_query = Some("needle".to_string());
        overlay.search_match = Some(mez_mux::overlay::OverlaySearchMatch {
            line_index: 1,
            start_column: 0,
            width: 6,
        });
        overlay.mouse_selection = Some((
            mez_mux::copy::CopyPosition { line: 1, column: 0 },
            mez_mux::copy::CopyPosition { line: 1, column: 6 },
        ));
    }
    let content = crate::runtime::render::RuntimeCommandDisplayOverlayContent {
        command: Some("status".to_string()),
        live_source: None,
        lines: vec!["needle new".to_string()],
        line_style_spans: vec![Vec::new()],
        line_kinds: vec![mez_mux::render::RichTextLineKind::Normal],
        line_copy_texts: vec![None],
        actions: Vec::new(),
    };

    assert!(service.replace_primary_display_overlay_content(content.clone()));
    let overlay = service.primary_display_overlay().unwrap();
    assert_eq!(overlay.scroll_offset, 0);
    assert_eq!(overlay.search_query.as_deref(), Some("needle"));
    assert_eq!(
        overlay
            .search_match
            .map(|search_match| search_match.line_index),
        Some(0)
    );
    assert!(overlay.mouse_selection.is_none());
    assert!(!service.replace_primary_display_overlay_content(content));
}

/// Verifies that command chooser output rendered in the primary overlay is not
/// inert text. Rows that advertise an `action=` command must retain selectable
/// metadata so a mouse click can execute the command through the normal
/// terminal command path and then close or replace the overlay.
#[test]
fn runtime_primary_display_overlay_executes_selectable_command_rows() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .create_window_with_pane_process(&primary, "work", false, None)
        .unwrap();

    service
        .execute_attached_display_command(&primary, "choose-window")
        .unwrap();
    let overlay = service
        .primary_display_overlay()
        .expect("choose-window should open a command display overlay");
    let work_index = service
        .primary_display_overlay_action_targets()
        .iter()
        .position(|target| matches!(target, crate::runtime::render::OverlayActionTarget::RecordBrowserSelect { record_id } if record_id == "@2"))
        .expect("work window row should advertise a selectable action");
    let work_selection = overlay
        .selections
        .get(work_index)
        .expect("work window row should retain its registered range");
    let clicked_row = work_selection.line_index.saturating_add(1);
    let clicked_column = work_selection.start_column.saturating_add(2);

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::SelectDisplayOverlay {
                        position: CopyPosition {
                            line: clicked_row,
                            column: clicked_column,
                        },
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

    assert!(report.view_refresh_required);
    assert!(service.primary_display_overlay().is_none());
    assert_eq!(service.session().active_window().unwrap().name, "work");
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a display-overlay action identity is honored only for the
/// generation that registered it.
///
/// A stale identity, an identity that was never registered, and an identity
/// retained across a redraw must all stay inert instead of repeating an earlier
/// action, while the current generation still dispatches exactly once.
#[test]
fn runtime_primary_display_overlay_rejects_stale_and_unknown_action_identities() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .create_window_with_pane_process(&primary, "work", false, None)
        .unwrap();
    let original_window = service.session().active_window().unwrap().name.clone();

    service
        .execute_attached_display_command(&primary, "choose-window")
        .unwrap();
    let register_window_action = |service: &crate::runtime::RuntimeSessionService| {
        let overlay = service.primary_display_overlay()?;
        let index = service
            .primary_display_overlay_action_targets()
            .iter()
            .position(|target| {
                matches!(target, crate::runtime::render::OverlayActionTarget::RecordBrowserSelect { record_id } if record_id == "@2")
            })?;
        overlay
            .selections
            .get(index)
            .map(|selection| selection.action_id)
    };
    let first_generation_action =
        register_window_action(&service).expect("work window row should register an action");

    // Reopening the chooser starts a new generation, so the retained identity no
    // longer resolves even though the same row is visible again.
    service
        .execute_attached_display_command(&primary, "choose-window")
        .unwrap();
    assert!(
        service
            .execute_primary_display_overlay_action(&primary, first_generation_action)
            .unwrap()
    );
    assert!(service.primary_display_overlay().is_some());
    assert_eq!(
        service.session().active_window().unwrap().name,
        original_window
    );

    assert!(
        service
            .execute_primary_display_overlay_action(
                &primary,
                mez_mux::overlay::OverlayActionId(u64::MAX)
            )
            .unwrap()
    );
    assert!(service.primary_display_overlay().is_some());
    assert_eq!(
        service.session().active_window().unwrap().name,
        original_window
    );

    let current_action =
        register_window_action(&service).expect("reopened chooser should register the row again");
    assert!(
        service
            .execute_primary_display_overlay_action(&primary, current_action)
            .unwrap()
    );
    assert_eq!(service.session().active_window().unwrap().name, "work");
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies only the attached primary client may execute a display-overlay
/// action, even when it presents the current generation's identity.
#[test]
fn runtime_primary_display_overlay_action_requires_attached_primary_client() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let observer = service
        .session
        .attach_observer_with_terminal("observer", None, 1)
        .unwrap();
    service
        .create_window_with_pane_process(&primary, "work", false, None)
        .unwrap();
    let original_window = service.session().active_window().unwrap().name.clone();

    service
        .execute_attached_display_command(&primary, "choose-window")
        .unwrap();
    let action_id = service
        .primary_display_overlay()
        .and_then(|overlay| {
            let index = service
                .primary_display_overlay_action_targets()
                .iter()
                .position(|target| {
                    matches!(target, crate::runtime::render::OverlayActionTarget::RecordBrowserSelect { record_id } if record_id == "@2")
                })?;
            overlay.selections.get(index)
        })
        .map(|selection| selection.action_id)
        .expect("work window row should register an action");

    assert!(
        !service
            .execute_primary_display_overlay_action(&observer, action_id)
            .unwrap(),
        "a non-primary client must never execute a primary overlay action"
    );
    assert_eq!(
        service.session().active_window().unwrap().name,
        original_window
    );
    assert!(service.primary_display_overlay().is_some());

    assert!(
        service
            .execute_primary_display_overlay_action(&primary, action_id)
            .unwrap()
    );
    assert_eq!(service.session().active_window().unwrap().name, "work");
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies selectable command rows exposed by the primary display overlay can
/// be chosen from the keyboard. Mouse clicks and keyboard Enter must execute the
/// same stored command metadata so chooser output does not depend on scraping
/// the rendered text.
#[test]
fn runtime_primary_display_overlay_executes_keyboard_selected_command_rows() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .create_window_with_pane_process(&primary, "work", false, None)
        .unwrap();

    service
        .execute_attached_display_command(&primary, "choose-window")
        .unwrap();
    assert!(service.primary_display_overlay().is_some());
    assert_eq!(service.session().active_window().unwrap().name, "0");

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"\x1b[B".to_vec()),
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
    assert!(report.full_redraw_required);
    assert!(service.primary_display_overlay().is_none());
    assert_eq!(service.session().active_window().unwrap().name, "work");
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies command overlays can expose multiple selectable choices on one row.
/// The user should be able to distinguish routine and destructive choices by
/// color, move between them with selector keys, and execute the active choice
/// without scraping command text out of the rendered row.
#[test]
fn runtime_primary_display_overlay_executes_multiple_action_chips() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .paste_buffers_mut()
        .set_with_origin("main", "pasted\n", Some("test".to_string()))
        .unwrap();
    service.set_active_paste_buffer(Some("main".to_string()));

    service
        .execute_attached_display_command(&primary, "choose-buffer")
        .unwrap();
    service
        .primary_display_overlay()
        .expect("choose-buffer should open a command display overlay");
    let targets = service.primary_display_overlay_action_targets();
    let paste = targets
        .iter()
        .position(|target| {
            target.terminal_command_line().as_deref() == Some("paste-buffer -b main")
        })
        .expect("buffer row should expose a paste choice");
    let delete = targets
        .iter()
        .position(|target| target.terminal_command_line().as_deref() == Some("delete-buffer main"))
        .expect("buffer row should expose a delete choice");
    assert_eq!(delete, paste.saturating_add(1));

    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    let row = view
        .lines
        .iter()
        .position(|line| line.contains("[paste]") && line.contains("[delete]"))
        .expect("overlay should render compact action chips");
    assert!(view.lines[row].contains("[paste]"));
    assert!(view.lines[row].contains("[delete]"));
    assert!(
        view.line_style_spans[row].iter().any(|span| {
            span.length == "[paste]".len()
                && !span.rendition.inverse
                && span.rendition.background
                    == Some(service.ui_theme().colors.agent_reasoning.background)
                && span.rendition.foreground
                    == Some(service.ui_theme().colors.agent_reasoning.foreground)
                && span.rendition.bold
                && span.rendition.underline
        }),
        "{view:?}"
    );
    assert!(
        view.line_style_spans[row].iter().any(|span| {
            span.length == "[delete]".len()
                && !span.rendition.inverse
                && span.rendition.background.is_none()
                && span.rendition.foreground
                    == Some(service.ui_theme().colors.display_overlay.foreground)
                && span.rendition.bold
                && span.rendition.underline
        }),
        "{view:?}"
    );

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"\x1b[C".to_vec()),
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
    assert!(service.paste_buffers().get("main").is_none());
    assert_eq!(service.active_paste_buffer(), None);
    assert!(service.primary_display_overlay().is_none());
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies mouse selection resolves the clicked chip when multiple choices are
/// present on the same display row. This keeps multi-action rows from falling
/// back to ambiguous whole-row execution.
#[test]
fn runtime_primary_display_overlay_mouse_selects_action_chip() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .paste_buffers_mut()
        .set_with_origin("main", "pasted\n", Some("test".to_string()))
        .unwrap();
    service.set_active_paste_buffer(Some("main".to_string()));

    service
        .execute_attached_display_command(&primary, "choose-buffer")
        .unwrap();
    let (clicked_line, clicked_column) = service
        .primary_display_overlay()
        .and_then(|overlay| {
            let targets = service.primary_display_overlay_action_targets();
            overlay
                .selections
                .iter()
                .zip(targets)
                .find(|(_, target)| {
                    target.terminal_command_line().as_deref() == Some("delete-buffer main")
                })
                .map(|(selection, _)| {
                    (
                        selection.line_index.saturating_add(1),
                        selection.start_column.saturating_add(2),
                    )
                })
        })
        .expect("delete-buffer choice should be selectable");

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::SelectDisplayOverlay {
                        position: CopyPosition {
                            line: clicked_line,
                            column: clicked_column,
                        },
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

    assert!(service.paste_buffers().get("main").is_none());
    assert_eq!(service.active_paste_buffer(), None);
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies that recoverable error overlays render as transient status-bar
/// notices. The next input should clear the notice as presentational state
/// without being forwarded or replayed, so repeating an error-causing action
/// does not immediately trigger the same error while dismissing the overlay.
#[test]
fn runtime_primary_error_overlay_dismisses_on_any_input() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(40, 6).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .show_primary_error_overlay(vec!["error: simulated".to_string()])
        .unwrap();

    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(40, 6).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert!(
        view.lines
            .last()
            .is_some_and(|line| line.contains("simulated")),
        "{:?}",
        view.lines
    );
    assert!(view.cursor_visible);

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"x".to_vec())],
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
    assert!(report.full_redraw_required);
    assert!(service.primary_error_status_overlay().is_none());
    assert!(service.primary_display_overlay().is_none());
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a program-controlled pane title cannot inject an executable choice
/// into a compact display row.
///
/// Pane titles come from OSC output, and compact display rows separate fields
/// with `:` and `=`. An unsanitized title such as `evil:action=kill-session`
/// would add a second, attacker-chosen executable chip to the `display-panes`
/// chooser, so the row must show the title text literally while only the
/// product-authored `select-pane` action stays selectable.
#[test]
fn runtime_display_rows_ignore_injected_fields_from_pane_titles() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .session
        .set_pane_title_explicit("%1", "evil:action=kill-session")
        .unwrap();

    service
        .execute_attached_display_command(&primary, "display-panes")
        .unwrap();

    let lines = service
        .primary_display_overlay_action_targets()
        .iter()
        .map(|target| target.terminal_command_line())
        .collect::<Vec<_>>();
    assert!(
        lines.iter().all(|line| line
            .as_deref()
            .is_none_or(|line| !line.contains("kill-session"))),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|line| line
            .as_deref()
            .is_some_and(|line| line.starts_with("select-pane"))),
        "{lines:?}"
    );
    service.terminate_all_pane_processes().unwrap();
}
