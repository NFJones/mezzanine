//! Runtime tests for session attachment behavior.

use super::*;

/// Verifies runtime service tracks attach detach lifecycle.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_service_tracks_attach_detach_lifecycle() {
    let mut service = test_runtime_service();

    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    assert_eq!(service.lifecycle_state(), RuntimeLifecycleState::Running);
    assert_eq!(service.last_attach_at_unix_seconds(), Some(120));
    assert_eq!(service.session().primary_client_id(), Some(&primary));
    assert_eq!(
        service.session().authoritative_size,
        Size::new(100, 40).unwrap()
    );

    service
        .detach_primary(&primary, Size::new(132, 43).unwrap())
        .unwrap();
    assert_eq!(service.lifecycle_state(), RuntimeLifecycleState::Detached);
    assert!(service.session().primary_client_id().is_none());
    assert_eq!(
        service.session().authoritative_size,
        Size::new(132, 43).unwrap()
    );

    let reattached = service
        .attach_primary("reattach", true, Size::new(90, 30).unwrap(), 180)
        .unwrap();
    assert_eq!(service.lifecycle_state(), RuntimeLifecycleState::Running);
    assert_eq!(service.session().primary_client_id(), Some(&reattached));
    assert_eq!(service.last_attach_at_unix_seconds(), Some(180));
    assert_ne!(primary, reattached);
}

/// Verifies runtime control initialize can reattach primary without existing primary.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_control_initialize_can_reattach_primary_without_existing_primary() {
    let mut service = test_runtime_service();
    let mut connection = ControlConnectionState::new(true, true);
    let initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"requested_role":"primary","requested_version":1,"client_name":"mez-cli","client":{"name":"mez-cli","interactive":true,"terminal":{"columns":100,"rows":40,"term":"xterm-256color"}}}}"#,
    );
    let get =
        encode_control_body(r#"{"jsonrpc":"2.0","id":"get","method":"session/get","params":{}}"#);
    let mut input = initialize;
    input.extend_from_slice(&get);

    let (output, consumed) = service
        .handle_control_input_for_connection(&input, 4096, &mut connection)
        .unwrap();
    let (first_body, first_consumed) = decode_control_frame(&output, 4096).unwrap();
    let (second_body, _) = decode_control_frame(&output[first_consumed..], 4096).unwrap();

    assert_eq!(consumed, input.len());
    assert!(first_body.contains(r#""granted_role":"primary""#));
    assert!(second_body.contains(r#""session_id":"$1""#));
    assert!(connection.caller_client_id().is_some());
    assert!(service.session().primary_client_id().is_some());
    assert_eq!(
        service.session().authoritative_size,
        Size::new(100, 40).unwrap()
    );
    assert!(service.last_attach_at_unix_seconds().is_some());
}

/// Verifies that the live control attach path applies the primary terminal size
/// to an already-started initial pane. The daemon starts the first pane before
/// the CLI sends `control/initialize`, so the initialize side effect must use
/// the same resize/sync path as direct attaches instead of only recording the
/// authoritative size.
#[test]
fn runtime_control_initialize_resizes_started_initial_pane_for_primary_terminal() {
    let mut service = test_runtime_service();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let initial_descriptor = service
        .tracked_pane_descriptors()
        .into_iter()
        .find(|descriptor| descriptor.pane_id.as_str() == "%1")
        .unwrap();
    assert_eq!(initial_descriptor.size, Size::new(80, 22).unwrap());

    let mut connection = ControlConnectionState::new(true, true);
    let initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"requested_role":"primary","requested_version":1,"client_name":"mez-cli","client":{"name":"mez-cli","interactive":true,"terminal":{"columns":100,"rows":40,"term":"xterm-256color"}}}}"#,
    );

    let (output, consumed) = service
        .handle_control_input_for_connection(&initialize, 4096, &mut connection)
        .unwrap();
    let (body, _) = decode_control_frame(&output, 4096).unwrap();

    assert_eq!(consumed, initialize.len());
    assert!(body.contains(r#""granted_role":"primary""#), "{body}");
    assert_eq!(
        service.session().authoritative_size,
        Size::new(100, 40).unwrap()
    );
    assert_eq!(
        service.session().active_window().unwrap().size,
        Size::new(100, 40).unwrap()
    );
    assert_eq!(
        service
            .session()
            .active_window()
            .unwrap()
            .active_pane()
            .size,
        Size::new(100, 40).unwrap()
    );
    let resized_descriptor = service
        .tracked_pane_descriptors()
        .into_iter()
        .find(|descriptor| descriptor.pane_id.as_str() == "%1")
        .unwrap();
    assert_eq!(resized_descriptor.size, Size::new(100, 38).unwrap());
    assert_eq!(
        service.pane_screen("%1").unwrap().size(),
        Size::new(100, 38).unwrap()
    );

    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(100, 40).unwrap(),
            &config,
        )
        .unwrap()
        .unwrap();
    let region = view.agent_prompt_region.unwrap();
    assert_eq!(view.lines.len(), 40);
    assert_eq!(region.columns, 100);
    assert_eq!(region.rows, 38);
    assert!(
        view.cursor_row >= 38,
        "agent prompt cursor should render at attached terminal bottom: {view:?}"
    );

    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies observer `control/initialize` requests are visible immediately.
///
/// The control dispatcher already creates the pending observer record. The
/// runtime side effect must also log the request, write a visible active-pane
/// status line with the request id, and make `:list-observers` usable as the
/// same pager/action surface as `:choose-observer`.
#[test]
fn runtime_control_initialize_observer_logs_and_lists_pending_request() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut connection = ControlConnectionState::new(true, true);
    let initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"requested_role":"observer","requested_version":1,"client_name":"observer-cli","client":{"name":"observer-cli","interactive":true,"terminal":{"columns":100,"rows":40,"term":"xterm-256color"}}}}"#,
    );

    let (output, consumed) = service
        .handle_control_input_for_connection(&initialize, 4096, &mut connection)
        .unwrap();
    let (body, _) = decode_control_frame(&output, 4096).unwrap();
    let observer = service.session().observers().first().unwrap();
    let observer_id = observer.id.to_string();

    assert_eq!(consumed, initialize.len());
    assert!(
        body.contains(r#""granted_role":"pending_observer""#),
        "{body}"
    );
    assert!(body.contains(&observer_id), "{body}");
    let events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(
        events.iter().any(|event| {
            event.kind == EventKind::ObserverRequested && event.payload.contains(&observer_id)
        }),
        "{events:?}"
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .visible_lines()
        .join("\n");
    assert!(
        pane_text.contains(&format!("observer request {observer_id}")),
        "{pane_text}"
    );

    service
        .execute_attached_display_command(&primary, "list-observers")
        .unwrap();
    let overlay = service
        .primary_display_overlay()
        .expect("list-observers should open a command display overlay");
    assert!(
        overlay
            .selections
            .iter()
            .any(|selection| selection.command == format!("approve-observer -t {observer_id}")),
        "{overlay:?}"
    );
    assert!(
        overlay
            .selections
            .iter()
            .any(|selection| selection.command == format!("reject-observer -t {observer_id}")),
        "{overlay:?}"
    );
}

/// Verifies that the runtime service refreshes the filesystem registry when a
/// control connection claims the primary role. Without this write, `mez list`
/// could advertise a detached session as primary-available after an attach, and
/// default attach resolution could pick that busy session instead of another
/// attachable live daemon.
#[test]
fn runtime_control_initialize_persists_attached_registry_state() {
    let root = std::env::temp_dir().join(format!(
        "mez-runtime-registry-initialize-{}-{:?}",
        std::process::id(),
        thread::current().id()
    ));
    let _ = fs::remove_dir_all(&root);
    let registry = SessionRegistry::new(root.clone(), effective_uid());
    let mut service = test_runtime_service();
    service.set_session_registry(registry.clone());
    service.persist_registry_update().unwrap();
    let mut connection = ControlConnectionState::new(true, true);
    let initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"requested_role":"primary","requested_version":1,"client_name":"mez-cli","client":{"name":"mez-cli","interactive":true,"terminal":{"columns":100,"rows":40,"term":"xterm-256color"}}}}"#,
    );

    service
        .handle_control_input_for_connection(&initialize, 4096, &mut connection)
        .unwrap();

    let records = registry.list().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].session_id, service.session().id.to_string());
    assert_eq!(records[0].state, RegistrySessionState::Running);
    assert!(!records[0].primary_available);
    assert_eq!(records[0].authoritative_columns, 100);
    assert_eq!(records[0].authoritative_rows, 40);
    assert!(records[0].last_attach_at_unix_seconds.is_some());

    let _ = fs::remove_dir_all(root);
}

/// Verifies that primary detach actions issued by the attached terminal loop
/// update the registry immediately. This covers the default prefix escape path,
/// which mutates runtime state outside the framed control request loop and
/// otherwise could leave `mez list` showing the session as still busy.
#[test]
fn attached_terminal_detach_action_persists_available_registry_state() {
    let root = std::env::temp_dir().join(format!(
        "mez-runtime-registry-detach-action-{}-{:?}",
        std::process::id(),
        thread::current().id()
    ));
    let _ = fs::remove_dir_all(&root);
    let registry = SessionRegistry::new(root.clone(), effective_uid());
    let mut service = test_runtime_service();
    service.set_session_registry(registry.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let busy_records = registry.list().unwrap();
    assert_eq!(busy_records.len(), 1);
    assert!(!busy_records[0].primary_available);
    let detach_step = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::ExecuteMux(
            MuxAction::DetachPrimaryClient,
        )],
        output_lines: Vec::new(),
        output_line_style_spans: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };

    service
        .apply_attached_terminal_step_plan(&primary, &detach_step)
        .unwrap();

    let records = registry.list().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].state, RegistrySessionState::Detached);
    assert!(records[0].primary_available);
    assert_eq!(records[0].last_attach_at_unix_seconds, Some(120));

    let _ = fs::remove_dir_all(root);
}

/// Verifies runtime service registry plan preserves authoritative detached size.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_service_registry_plan_preserves_authoritative_detached_size() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();

    service
        .detach_primary(&primary, Size::new(132, 43).unwrap())
        .unwrap();

    let RuntimeRegistryUpdatePlan::Upsert(record) = service.registry_update_plan() else {
        panic!("detached live service must plan a registry upsert");
    };
    assert_eq!(record.state, RegistrySessionState::Detached);
    assert_eq!(record.last_attach_at_unix_seconds, Some(120));
    assert!(record.primary_available);
    assert_eq!(record.authoritative_columns, 132);
    assert_eq!(record.authoritative_rows, 43);
}

/// Verifies runtime applies attached terminal step actions.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_applies_attached_terminal_step_actions() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let step = AttachedTerminalClientStepPlan {
        actions: vec![
            TerminalClientLoopAction::ForwardToPane(b"hello\n".to_vec()),
            TerminalClientLoopAction::ExecuteMux(MuxAction::SplitPaneVertical),
            TerminalClientLoopAction::ExecuteMux(MuxAction::FocusPane(PaneFocusDirection::Left)),
            TerminalClientLoopAction::EnterPrefixKeyMode,
            TerminalClientLoopAction::ExecuteMux(MuxAction::EnterCopyMode),
        ],
        output_lines: Vec::new(),
        output_line_style_spans: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };

    let report = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();

    assert_eq!(report.forwarded_bytes, 6);
    assert_eq!(report.mux_actions_applied, 3);
    assert!(report.view_refresh_required);
    assert!(report.full_redraw_required);
    assert!(report.unsupported_actions.is_empty());
    assert!(!service.active_copy_modes().is_empty());
    assert_eq!(service.session().windows()[0].panes().len(), 2);
    assert_eq!(service.pane_processes().len(), 2);
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies observer chooser rows expose the concrete available decisions as
/// compact action chips. The row also contains descriptive `actions=` metadata,
/// but the executable choices must come from the command list so keyboard and
/// mouse selection run real terminal commands.
#[test]
fn runtime_primary_display_overlay_exposes_observer_action_chips() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 24).unwrap(), 120)
        .unwrap();
    let (_observer_client, observer_request) = service
        .session
        .request_observer_with_terminal("observer", None);

    service
        .execute_attached_display_command(&primary, "choose-observer")
        .unwrap();
    let overlay = service
        .primary_display_overlay()
        .expect("choose-observer should open a command display overlay");
    assert!(
        overlay
            .selections
            .iter()
            .any(|selection| selection.command
                == format!("approve-observer -t {observer_request}")),
        "{overlay:?}"
    );
    assert!(
        overlay
            .selections
            .iter()
            .any(|selection| selection.command == format!("reject-observer -t {observer_request}")),
        "{overlay:?}"
    );
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(100, 24).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert!(
        view.lines
            .iter()
            .any(|line| line.contains("[approve]") && line.contains("[reject]")),
        "{view:?}"
    );
}

/// Verifies runtime attached split mux action focuses new pane.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_attached_split_mux_action_focuses_new_pane() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    assert!(
        service
            .apply_attached_mux_action(&primary, MuxAction::SplitPaneVertical)
            .unwrap()
    );

    let window = &service.session().windows()[0];
    assert_eq!(window.panes().len(), 2);
    assert_eq!(window.active_pane().id.as_str(), "%2");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that the attached-terminal detach binding runs through the runtime
/// lifecycle path rather than mutating session client state directly. The
/// lifecycle helper updates the service state and emits the client-detached
/// event that hooks, registry state, and observers use as the authoritative
/// detach signal.
#[test]
fn runtime_attached_detach_mux_action_emits_lifecycle_state() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    assert!(
        service
            .apply_attached_mux_action(&primary, MuxAction::DetachPrimaryClient)
            .unwrap()
    );

    assert_eq!(service.lifecycle_state(), RuntimeLifecycleState::Detached);
    assert!(service.session().primary_client_id().is_none());
    let events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(
        events
            .iter()
            .any(|event| event.kind == EventKind::ClientDetached
                && event.payload.contains(r#""role":"primary""#)),
        "{events:?}"
    );
}
