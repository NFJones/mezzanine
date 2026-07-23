//! Agent conversation snapshots tests.

use super::*;

/// Verifies terminal snapshot commands use the live runtime snapshot repository.
///
/// The command prompt should no longer return command-layer placeholders for
/// `save-layout` or `load-layout` when a daemon has configured snapshot
/// storage. This protects the bridge from parsed colon commands to the same
/// runtime control paths used by JSON-RPC snapshot clients, and verifies the
/// primary client can keep using the session immediately after `load-layout`.
#[test]
fn runtime_terminal_snapshot_commands_create_and_resume_snapshots() {
    let root = temp_root("terminal-snapshot-commands");
    let snapshots = SnapshotRepository::new(root.join("snapshots"));
    let mut service = test_runtime_service();
    service.set_snapshot_repository(snapshots);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let old_pane_start = service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let old_pane_id = old_pane_start.pane_id.clone();
    assert!(service.pane_processes().contains_pane(&old_pane_id));

    let create = service
        .execute_terminal_command(&primary, "save-layout --name checkpoint")
        .unwrap();
    assert!(create.contains(r#""command":"save-layout""#), "{create}");
    assert!(create.contains(r#""kind":"display""#), "{create}");
    assert!(
        create.contains(r#""body":"saved layout checkpoint""#),
        "{create}"
    );
    assert!(!create.contains(r#"\"snapshot\""#), "{create}");

    let resume = service
        .execute_terminal_command(&primary, "load-layout --latest")
        .unwrap();
    assert!(resume.contains(r#""command":"load-layout""#), "{resume}");
    assert!(resume.contains(r#""kind":"noop""#), "{resume}");
    assert!(!resume.contains("loaded latest layout"), "{resume}");
    assert!(!resume.contains(r#"\"resumed\":true"#), "{resume}");
    assert!(!service.pane_processes().contains_pane(&old_pane_id));
    let tracked_pane_ids = service.pane_processes().tracked_pane_ids();
    assert_eq!(tracked_pane_ids.len(), 1);
    assert_ne!(tracked_pane_ids[0], old_pane_id);
    let live_pane_ids = service
        .session()
        .windows()
        .iter()
        .flat_map(|window| window.panes().iter().map(|pane| pane.id.to_string()))
        .collect::<Vec<_>>();
    assert!(!live_pane_ids.contains(&old_pane_id));
    let events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(
        events
            .iter()
            .any(|event| event.payload.contains(r#""layout":"resized""#)),
        "{events:?}"
    );

    let create_after_resume = service
        .execute_terminal_command(&primary, "save-layout --name checkpoint-after-load")
        .unwrap();
    assert!(
        create_after_resume.contains(r#""body":"saved layout checkpoint-after-load""#),
        "{create_after_resume}"
    );

    let _ = fs::remove_dir_all(root);
}

/// Verifies unscoped terminal snapshot resume selects the newest restorable snapshot.
///
/// A user-visible `:load-layout --latest` command should be able to restore
/// the snapshot most recently created by `:save-layout` even when the live
/// daemon has a different session id after restart. Scoping `--latest` to the
/// current session id made the command unable to find persisted snapshots from
/// previous daemon sessions, so this regression uses two runtime services that
/// share one repository root. Resume also keeps the receiving runtime's session
/// and primary client identity because it only recreates the saved topology and
/// fresh pane shells rather than adopting snapshotted connection state.
#[test]
fn runtime_terminal_snapshot_resume_latest_uses_repository_latest_across_sessions() {
    let root = temp_root("terminal-snapshot-latest-cross-session");
    let snapshots = SnapshotRepository::new(root.join("snapshots"));
    let mut creating_service = test_runtime_service();
    creating_service.set_snapshot_repository(snapshots.clone());
    let creating_primary = creating_service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let create = creating_service
        .execute_terminal_command(&creating_primary, "save-layout --name restart-point")
        .unwrap();
    assert!(
        create.contains(r#""body":"saved layout restart-point""#),
        "{create}"
    );

    let mut resuming_service = test_runtime_service();
    resuming_service.set_snapshot_repository(snapshots);
    let resuming_primary = resuming_service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let live_session_id = resuming_service.session.id.to_string();

    let resume = resuming_service
        .execute_terminal_command(&resuming_primary, "load-layout --latest")
        .unwrap();
    assert!(resume.contains(r#""kind":"noop""#), "{resume}");
    assert!(!resume.contains("loaded latest layout"), "{resume}");
    assert_eq!(resuming_service.session.id.to_string(), live_session_id);

    let _ = fs::remove_dir_all(root);
}

/// Verifies `:load-layout --latest` revives a detached snapshot into a live
/// running session before restored pane restart begins.
///
/// Snapshot payloads preserve detached lifecycle state so users can resume a
/// saved detached daemon later. The live resume path must still mark the
/// restored session running before it restarts panes, otherwise the hierarchy
/// installs and the first restart step crashes on the live-session guard.
#[test]
fn runtime_terminal_snapshot_resume_latest_revives_detached_snapshot_session() {
    let root = temp_root("terminal-snapshot-resume-detached-state");
    let snapshots = SnapshotRepository::new(root.join("snapshots"));
    let mut creating_service = test_runtime_service();
    creating_service.set_snapshot_repository(snapshots.clone());
    let creating_primary = creating_service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let create = creating_service
        .execute_terminal_command(&creating_primary, "save-layout --name detached-restart")
        .unwrap();
    assert!(
        create.contains(r#""body":"saved layout detached-restart""#),
        "{create}"
    );

    creating_service
        .detach_primary(&creating_primary, Size::new(80, 24).unwrap())
        .unwrap();

    let mut resuming_service = test_runtime_service();
    resuming_service.set_snapshot_repository(snapshots);
    let resuming_primary = resuming_service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let resume = resuming_service
        .execute_terminal_command(&resuming_primary, "load-layout --latest")
        .unwrap();
    assert!(resume.contains(r#""kind":"noop""#), "{resume}");
    assert!(!resume.contains("loaded latest layout"), "{resume}");
    assert_eq!(resuming_service.session.state, SessionState::Running);

    let _ = fs::remove_dir_all(root);
}
