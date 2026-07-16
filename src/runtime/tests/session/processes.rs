//! Runtime tests for session processes behavior.

use super::*;

/// Verifies terminal-generated response bytes are forwarded back to the pane.
///
/// CSI 6n is a pane application query, not visible output. When the terminal
/// parser emits a cursor-position report, the runtime must write that reply to
/// the pane input path so full-screen applications waiting on CPR can continue.
#[test]
fn runtime_pane_output_device_status_report_is_forwarded_to_pane_input() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    let _process = service
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();

    service
        .apply_pane_output_bytes(pane_id.clone(), b"\x1b[3;5H\x1b[6n".to_vec())
        .unwrap();

    let deferred = service.drain_pane_io_transition().side_effects;
    assert_eq!(deferred.len(), 1);
    assert_eq!(deferred[0].pane_input_parts().0, pane_id);
    assert_eq!(deferred[0].pane_input_parts().1, b"\x1b[3;5R");
}

/// Verifies that runtime frame context sources `pane.process_name` from the
/// live host process metadata instead of only echoing the configured shell path.
#[cfg(target_os = "linux")]
/// Verifies runtime frame context uses host process name when available.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_frame_context_uses_host_process_name_when_available() {
    let mut service = test_runtime_service();
    service.start_initial_pane_process(Some("sleep 2")).unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();

    let mut process_name = None;
    for _ in 0..10_000 {
        process_name = service.pane_processes().process_name(&pane_id);
        if process_name.as_deref() == Some("sleep") {
            break;
        }
        thread::yield_now();
    }

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(process_name.as_deref(), Some("sleep"));
    assert_eq!(pane_context.process_name.as_deref(), Some("sleep"));
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies that a failed new-window process spawn is transactional. The window
/// is inserted before the PTY spawn path runs, so a spawn-layer failure must
/// restore the previous window list and active-window selection instead of
/// leaving a processless pane behind for later rendering or input dispatch.
#[test]
fn runtime_new_window_spawn_failure_rolls_back_window_creation() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let active_window_id = service.session().active_window().unwrap().id.clone();
    service.session.shell = ResolvedShell::new(
        PathBuf::from("/tmp/mez-runtime-missing-shell-new-window"),
        ShellSource::FallbackBinSh,
    )
    .into();

    let error = service
        .create_window_with_pane_process(&primary, "bad", true, None)
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Io);
    assert_eq!(service.session().windows().len(), 1);
    assert_eq!(
        service.session().active_window().unwrap().id,
        active_window_id
    );
    assert!(service.pane_processes().is_empty());
}

/// Verifies that a failed split process spawn restores the pre-split layout.
/// Existing panes are resized before the new pane process is started, so the
/// rollback must also return the active pane geometry to its original size and
/// leave only the already-running process tracked by the runtime.
#[test]
fn runtime_split_spawn_failure_rolls_back_layout_creation() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let active_pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .clone();
    service.session.shell = ResolvedShell::new(
        PathBuf::from("/tmp/mez-runtime-missing-shell-split"),
        ShellSource::FallbackBinSh,
    )
    .into();

    let error = service
        .split_pane_with_process(&primary, SplitDirection::Vertical, None)
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Io);
    let window = service.session().active_window().unwrap();
    assert_eq!(window.panes().len(), 1);
    assert_eq!(window.active_pane().id, active_pane_id);
    assert_eq!(window.active_pane().size, Size::new(80, 24).unwrap());
    assert_eq!(service.pane_processes().tracked_pane_ids(), vec!["%1"]);
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies that terminal-command splits use the same transactional runtime
/// helper as direct mux/control splits. A failed process spawn must restore the
/// pre-split layout instead of leaving a processless command-created pane with
/// stale geometry behind.
#[test]
fn runtime_terminal_command_split_spawn_failure_rolls_back_layout_creation() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let active_pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .clone();
    service.session.shell = ResolvedShell::new(
        PathBuf::from("/tmp/mez-runtime-missing-shell-command-split"),
        ShellSource::FallbackBinSh,
    )
    .into();

    let error = service
        .execute_terminal_command(&primary, "split-window")
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Io);
    let window = service.session().active_window().unwrap();
    assert_eq!(window.panes().len(), 1);
    assert_eq!(window.active_pane().id, active_pane_id);
    assert_eq!(window.active_pane().size, Size::new(80, 24).unwrap());
    assert_eq!(service.pane_processes().tracked_pane_ids(), vec!["%1"]);
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies pane-move terminal commands synchronize screens through the
/// runtime-owned resize adapters. Break and join must apply the explicit
/// session effects instead of relying on generic post-dispatch rediscovery.
#[test]
fn runtime_terminal_pane_move_commands_apply_resize_effects() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some("cat >/dev/null"))
        .unwrap();

    service
        .execute_terminal_command(&primary, "swapp -U")
        .unwrap();
    service
        .execute_terminal_command(&primary, "breakp -n moved")
        .unwrap();
    assert_eq!(service.session().windows().len(), 2);
    let broken_size = service.pane_screen("%2").unwrap().size();

    service
        .execute_terminal_command(&primary, "joinp -t 0 --select")
        .unwrap();
    assert_eq!(service.session().windows().len(), 1);
    assert!(service.pane_screen("%2").unwrap().size().columns < broken_size.columns);

    service.terminate_all_pane_processes().unwrap();
}

/// Verifies runtime service starts initial pane process through resolved shell.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_service_starts_initial_pane_process_through_resolved_shell() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let started = service.start_initial_pane_process(Some("true")).unwrap();

    assert_eq!(started.session_id, service.session().id.to_string());
    assert_eq!(started.window_id, "@1");
    assert_eq!(started.pane_id, "%1");
    assert!(started.primary_pid > 0);
    assert_eq!(
        service.pane_processes().primary_pid("%1"),
        Some(started.primary_pid)
    );
    assert!(matches!(
        started.registry_update,
        RuntimeRegistryUpdatePlan::Upsert(_)
    ));

    let events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(
        events
            .iter()
            .any(|event| event.kind == EventKind::PaneChanged
                && event.payload.contains(r#""process_state":"running""#))
    );
    assert!(
        events
            .iter()
            .any(|event| event.kind == EventKind::Diagnostic
                && event.payload.contains("fell back to /bin/sh"))
    );

    let _ = primary;
    poll_until_exit(&mut service);
}

/// Verifies that runtime services can hand a running pane process to an async
/// owner and restore it if the handoff is cancelled. The service keeps session
/// and terminal metadata while only the process/PTY handle leaves the
/// synchronous manager.
#[test]
fn runtime_service_can_handoff_running_pane_process_to_async_owner() {
    let mut service = test_runtime_service();
    let started = service
        .start_initial_pane_process(Some("sleep 30"))
        .unwrap();

    let process = service
        .take_running_pane_process_for_adapter(&started.pane_id)
        .unwrap();

    assert!(!service.pane_processes().contains_pane(&started.pane_id));
    let window = service.session().active_window().unwrap();
    let pane_state = service.runtime_control_pane_state_json(window, window.active_pane());
    assert!(
        pane_state.contains(&format!(r#""primary_pid":{}"#, started.primary_pid)),
        "{pane_state}"
    );
    assert!(
        pane_state.contains(r#""process_state":"running""#),
        "{pane_state}"
    );
    service
        .apply_pane_foreground_process_event(
            &started.pane_id,
            "vim",
            started.primary_pid.saturating_add(1),
            Some("/tmp/mez-async-cwd".to_string()),
        )
        .unwrap();
    assert_eq!(
        service
            .pane_current_working_directory(&started.pane_id)
            .as_deref(),
        Some(Path::new("/tmp/mez-async-cwd"))
    );
    assert_eq!(
        service
            .restore_running_pane_process_from_adapter(&started.pane_id, process)
            .unwrap(),
        started.primary_pid
    );
    assert_eq!(
        service.pane_processes().primary_pid(&started.pane_id),
        Some(started.primary_pid)
    );
    service
        .pane_processes_mut()
        .terminate_pane_with_grace(&started.pane_id, Duration::from_millis(50))
        .unwrap();
}

/// Verifies stale async process-exit events cannot close a pane after its id is reused.
///
/// `load-layout` can restart a fresh process for a restored pane id while an
/// older async watcher still holds a late exit event for the previous process.
/// The runtime must compare the event's primary PID with the currently live
/// primary PID and ignore mismatches so the new pane generation remains live.
#[test]
fn runtime_service_ignores_stale_process_exit_with_mismatched_primary_pid() {
    let mut service = test_runtime_service();
    let started = service
        .start_initial_pane_process(Some("sleep 30"))
        .unwrap();
    let stale_primary_pid = started.primary_pid.saturating_add(1);

    let update = service
        .apply_pane_process_exit_event(
            &started.pane_id,
            stale_primary_pid,
            mez_mux::process::PaneExitStatus {
                code: Some(0),
                signal: None,
                success: true,
            },
        )
        .unwrap();

    assert_eq!(update, None);
    assert_eq!(
        service.pane_processes().primary_pid(&started.pane_id),
        Some(started.primary_pid)
    );
    assert!(
        service
            .session()
            .windows()
            .iter()
            .flat_map(|window| window.panes())
            .any(|pane| pane.id.as_str() == started.pane_id.as_str() && pane.live)
    );
    service
        .pane_processes_mut()
        .terminate_pane_with_grace(&started.pane_id, Duration::from_millis(50))
        .unwrap();
}

/// Verifies late pane output after a pane exit event is ignored.
///
/// Full-screen and alternate-screen applications commonly emit shutdown bytes
/// while the PTY is exiting. Once runtime teardown removes the pane from the
/// session, those late bytes must be treated as a normal shutdown race instead
/// of a fatal missing-pane error.
#[test]
fn runtime_service_ignores_late_pane_output_after_exit_event() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let started = service
        .start_initial_pane_process(Some("sleep 30"))
        .unwrap();
    let second_pane = service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();

    let update = service
        .apply_pane_process_exit_event(
            &started.pane_id,
            started.primary_pid,
            mez_mux::process::PaneExitStatus {
                code: Some(0),
                signal: None,
                success: true,
            },
        )
        .unwrap();

    assert!(update.is_some());
    assert!(
        service
            .session()
            .windows()
            .iter()
            .flat_map(|window| window.panes())
            .all(|pane| pane.id.as_str() != started.pane_id.as_str())
    );
    assert!(
        service
            .session()
            .windows()
            .iter()
            .flat_map(|window| window.panes())
            .any(|pane| pane.id.as_str() == second_pane.as_str())
    );

    let late_output = service
        .apply_pane_output_bytes(started.pane_id.clone(), b"\x1b[?1004l\x1b[?1049l".to_vec())
        .unwrap();

    assert_eq!(late_output, None);
}

/// Verifies runtime service restarts restored panes with fresh primary pids.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_service_restarts_restored_panes_with_fresh_primary_pids() {
    let mut original = test_session();
    let primary = original.attach_primary("primary", true).unwrap();
    original
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    let payload = crate::storage::snapshot::SessionSnapshotPayload::from_session(&original);
    let restore_input = crate::storage::snapshot::session_restore_input(&payload).unwrap();
    let restored = Session::from_restore_input(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        restore_input,
    )
    .unwrap();
    assert!(
        restored
            .windows()
            .iter()
            .flat_map(|window| window.panes())
            .all(|pane| !pane.live)
    );
    let mut service = RuntimeSessionService::with_event_log(
        restored,
        PathBuf::from("/tmp/mez-1000/restored.sock"),
        100,
        10,
        1024,
    )
    .unwrap();

    let starts = service
        .restart_restored_pane_processes(Some("true"))
        .unwrap();

    assert_eq!(starts.len(), 2);
    assert!(starts.iter().all(|start| start.primary_pid > 0));
    assert_ne!(starts[0].primary_pid, starts[1].primary_pid);
    assert_eq!(service.pane_processes().len(), 2);
    assert!(starts.iter().all(|start| {
        service.pane_readiness_state(&start.pane_id) == PaneReadinessState::PromptCandidate
    }));
    assert!(
        service
            .session()
            .windows()
            .iter()
            .flat_map(|window| window.panes())
            .all(|pane| pane.live)
    );
    let events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(
        events
            .iter()
            .any(|event| event.payload.contains(r#""restarted":true"#))
    );
    poll_until_exit(&mut service);
}

/// Verifies runtime service restarts restored panes at the rendered PTY size
/// instead of the raw saved layout pane size.
///
/// Restored shells must start with the same content-area dimensions used by
/// normal pane creation so cursor placement and shell redraws stay aligned with
/// framed and split layouts immediately after `load-layout`.
#[test]
fn runtime_service_restarts_restored_panes_with_rendered_process_sizes() {
    let mut original = test_session();
    let primary = original.attach_primary("primary", true).unwrap();
    original
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    let payload = crate::storage::snapshot::SessionSnapshotPayload::from_session(&original);
    let restore_input = crate::storage::snapshot::session_restore_input(&payload).unwrap();
    let restored = Session::from_restore_input(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        restore_input,
    )
    .unwrap();
    let restored_pane_sizes: Vec<Size> = restored
        .windows()
        .iter()
        .flat_map(|window| window.panes().iter().map(|pane| pane.size))
        .collect();
    let mut service = RuntimeSessionService::with_event_log(
        restored,
        PathBuf::from("/tmp/mez-1000/restored-size-alignment.sock"),
        100,
        10,
        1024,
    )
    .unwrap();

    let starts = service
        .restart_restored_pane_processes(Some("true"))
        .unwrap();
    let started_sizes: Vec<Size> = starts.iter().map(|start| start.size).collect();
    let tracked_sizes: Vec<Size> = service
        .tracked_pane_descriptors()
        .into_iter()
        .map(|descriptor| descriptor.size)
        .collect();

    assert_eq!(started_sizes.len(), restored_pane_sizes.len());
    assert_eq!(started_sizes, tracked_sizes);
    assert_ne!(started_sizes, restored_pane_sizes);
    poll_until_exit(&mut service);
}

/// Verifies runtime snapshot resume treats saved pane working directories as
/// best-effort metadata when fresh pane process startup cannot use them.
///
/// A snapshot can contain a directory that existed during restore planning but
/// becomes unusable when the fresh pane process starts. Resume must keep the
/// restored layout and names, retry the pane from the user's home directory,
/// and leave the restored session usable instead of unwinding after topology
/// installation.
#[test]
fn runtime_service_restarts_restored_panes_from_home_when_saved_cwd_fails() {
    let root = temp_root("runtime-restored-pane-cwd-fallback");
    let inaccessible_cwd = root.join("inaccessible-cwd");
    fs::create_dir_all(&inaccessible_cwd).unwrap();
    let original_permissions = fs::metadata(&inaccessible_cwd).unwrap().permissions();
    fs::set_permissions(&inaccessible_cwd, fs::Permissions::from_mode(0o000)).unwrap();
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap();
    assert!(home.is_dir());

    let original = test_session();
    let mut payload = crate::storage::snapshot::SessionSnapshotPayload::from_session(&original);
    payload.name = "restored-name".to_string();
    payload.windows[0].name = "saved-window".to_string();
    payload.windows[0].panes[0].title = "saved-pane".to_string();
    payload.windows[0].panes[0].current_working_directory =
        Some(inaccessible_cwd.to_string_lossy().into_owned());
    let restore_input = crate::storage::snapshot::session_restore_input(&payload).unwrap();
    let restored = Session::from_restore_input(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        restore_input,
    )
    .unwrap();
    let pane_id = restored.active_window().unwrap().active_pane().id.clone();
    let mut service = RuntimeSessionService::with_event_log(
        restored,
        PathBuf::from("/tmp/mez-1000/restored-cwd-fallback.sock"),
        100,
        10,
        1024,
    )
    .unwrap();

    let starts = service
        .restart_restored_pane_processes(Some("true"))
        .unwrap();

    assert_eq!(starts.len(), 1);
    assert_eq!(service.session().name, "restored-name");
    assert_eq!(
        service.session().active_window().unwrap().name,
        "saved-window"
    );
    assert_eq!(
        service
            .session()
            .active_window()
            .unwrap()
            .active_pane()
            .title,
        "saved-pane"
    );
    assert_eq!(
        service
            .pane_current_working_directory(pane_id.as_str())
            .as_deref(),
        Some(home.as_path())
    );
    let events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(events.iter().any(|event| {
        event
            .payload
            .contains("snapshot resume pane cwd unavailable; retrying from home")
    }));

    poll_until_exit(&mut service);
    fs::set_permissions(&inaccessible_cwd, original_permissions).unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies runtime service starts processes for created windows and panes.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_service_starts_processes_for_created_windows_and_panes() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let window_start = service
        .create_window_with_pane_process(&primary, "build", true, Some("true"))
        .unwrap();
    assert_eq!(window_start.window_id, "@2");
    assert_eq!(window_start.pane_id, "%2");
    assert_eq!(
        service.pane_processes().primary_pid(&window_start.pane_id),
        Some(window_start.primary_pid)
    );

    let split_start = service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some("true"))
        .unwrap();
    assert_eq!(split_start.window_id, "@2");
    assert_eq!(split_start.pane_id, "%3");
    assert_eq!(
        service.pane_processes().primary_pid(&split_start.pane_id),
        Some(split_start.primary_pid)
    );

    let mut exited = poll_until_exit(&mut service).len();
    while exited < 2 {
        exited += poll_until_exit(&mut service).len();
    }
    assert_eq!(exited, 2);
}

/// Verifies a later foreground-process event recovers interactive-blocked
/// readiness after alternate-screen exit missed the prompt-candidate transition.
///
/// Some full-screen programs leave the alternate screen before the async
/// foreground-process update reports that the shell owns the PTY again. The
/// cached foreground-process event should reopen prompt-candidate recovery so
/// later shell actions do not stay stranded in interactive-blocked state.
#[test]
fn runtime_foreground_process_event_recovers_after_alternate_screen_exit() {
    let mut service = test_runtime_service();
    service.start_initial_pane_process(None).unwrap();
    let primary_pid = service.pane_processes().primary_pid("%1").unwrap();
    service
        .pane_processes_mut()
        .set_foreground_process_group_id_for_test("%1", None);

    service
        .apply_pane_output_bytes("%1", b"[?1049hfullscreen".to_vec())
        .unwrap();
    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::InteractiveBlocked
    );

    service
        .apply_pane_output_bytes("%1", b"[?1049l$ ".to_vec())
        .unwrap();
    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::InteractiveBlocked
    );

    service
        .apply_pane_foreground_process_event("%1", "sh", primary_pid, None)
        .unwrap();

    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::PromptCandidate
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies stale readiness recovery can use async foreground metadata when
/// synchronous PTY foreground queries are temporarily unavailable.
///
/// New panes and panes recreated by `load-layout` can have a valid shell prompt
/// while a direct `tcgetpgrp` style foreground query is still unavailable. The
/// async pane worker reports foreground process groups separately, and readiness
/// recovery should use that cached observation instead of leaving shell actions
/// stranded behind stale interactive-blocked state.
/// Verifies program-emitted pane titles stay sticky across automatic foreground
/// title refreshes, then restore the previous title mode when the foreground
/// program changes. This protects pane status pill titles from rapidly flipping
/// between OSC titles and auto-generated process titles.
#[test]
fn runtime_pane_program_title_stays_sticky_until_foreground_process_changes() {
    let mut service = test_runtime_service();
    service.start_initial_pane_process(None).unwrap();
    let primary_pid = service.pane_processes().primary_pid("%1").unwrap();

    service
        .apply_pane_foreground_process_event("%1", "vim", 4242, None)
        .unwrap();
    assert_eq!(
        service
            .session()
            .active_window()
            .unwrap()
            .active_pane()
            .title,
        "vim"
    );

    service
        .apply_pane_process_output(
            mez_mux::process::PaneProcessOutput {
                pane_id: "%1".to_string(),
                primary_pid,
                bytes: b"\x1b]2;editing notes\x07".to_vec(),
            },
            &mut std::collections::BTreeSet::new(),
        )
        .unwrap();
    assert_eq!(
        service
            .session()
            .active_window()
            .unwrap()
            .active_pane()
            .title,
        "editing notes"
    );

    service
        .apply_pane_foreground_process_event("%1", "vim", 4242, None)
        .unwrap();
    assert_eq!(
        service
            .session()
            .active_window()
            .unwrap()
            .active_pane()
            .title,
        "editing notes"
    );

    service
        .apply_pane_process_output(
            mez_mux::process::PaneProcessOutput {
                pane_id: "%1".to_string(),
                primary_pid,
                bytes: b"\x1b]2;editing tests\x07".to_vec(),
            },
            &mut std::collections::BTreeSet::new(),
        )
        .unwrap();
    assert_eq!(
        service
            .session()
            .active_window()
            .unwrap()
            .active_pane()
            .title,
        "editing tests"
    );

    service
        .apply_pane_foreground_process_event("%1", "sh", primary_pid, None)
        .unwrap();
    assert_eq!(
        service
            .session()
            .active_window()
            .unwrap()
            .active_pane()
            .title,
        "shell"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies async pane write failures settle shell-backed file actions.
///
/// File mutations are sent through the pane shell as generated transactions. If
/// the async pane worker cannot write that transaction input, the action must
/// become a failed action result and queue model recovery instead of remaining
/// in the running-transaction table forever.
#[test]
fn runtime_pane_write_failure_fails_running_file_action() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let mut process = service
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();
    mark_test_pane_ready(&mut service, &pane_id);
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-write-failure","input":"create a note"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.remove_pending_agent_provider_task("turn-1");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "write file".to_string(),
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
                    id: "patch-fail".to_string(),
                    rationale: "write a note".to_string(),
                    payload: mez_agent::AgentActionPayload::ApplyPatch {
                        patch: "*** Begin Patch\n*** Add File: note.txt\n+note\n*** End Patch"
                            .to_string(),
                        strip: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let first = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(first.terminal_state, AgentTurnState::Running);
    assert_eq!(service.drain_pane_io_transition().side_effects.len(), 1);
    assert!(
        service
            .running_shell_transactions_for_tests()
            .values()
            .any(|transaction| matches!(
                transaction.kind,
                RunningShellTransactionKind::AgentAction { ref action_id }
                    if action_id == "patch-fail"
            ))
    );

    assert!(
        service
            .apply_pane_write_failure_event(&pane_id, "synthetic PTY write failure")
            .unwrap()
    );

    assert!(
        service
            .running_shell_transactions_for_tests()
            .values()
            .all(|transaction| !matches!(
                transaction.kind,
                RunningShellTransactionKind::AgentAction { .. }
            ))
    );
    assert_eq!(service.pending_agent_provider_tasks().len(), 1);
    assert!(!service.agent_turn_executions().contains_key("turn-1"));
    let context = service.agent_turn_contexts().get("turn-1").unwrap();
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("[action_result patch-fail apply_patch failed]")
            && block.content.contains("pane input write failed")
    }));

    let _ = process.terminate(Duration::from_millis(10));
}
