//! Command readiness tests.

use super::*;

/// Verifies paste and history commands report live terminal requirements.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn paste_and_history_commands_report_live_terminal_requirements() {
    let (mut session, primary) = test_session();

    let buffers = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("list-buffers").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(buffers, "buffers=0 source=not-connected status=empty");

    let paste = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("paste-buffer -b build").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(
        paste,
        "buffer=build:paste=not-sent:reason=live-terminal-state-unavailable"
    );

    let create = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("create-buffer -b build --content seed").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(
        create,
        "buffer=build:created=false:reason=live-paste-buffer-unavailable"
    );

    let paste_clipboard = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("paste-clipboard -t 0").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(
        paste_clipboard,
        "target=0:paste=not-sent:source=clipboard-or-buffer:reason=live-terminal-state-unavailable"
    );

    let copy_selection = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("copy-selection -t 0").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(
        copy_selection,
        "target=0:copy=not-copied:reason=live-terminal-state-unavailable"
    );

    let capture = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("capture-pane -p -t 0").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(
        capture,
        "target=0:capture=not-read:output=stdout:reason=live-terminal-state-unavailable"
    );

    let save = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("save-buffer -b build --output /tmp/buf.txt").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(
        save,
        "buffer=build:save=not-written:output=/tmp/buf.txt:reason=live-paste-buffer-unavailable"
    );

    let clear = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("clear-history -t 0").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(
        clear,
        "target=0:cleared=false:reason=live-terminal-state-unavailable"
    );

    let search = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("search-history error").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(
        search,
        "target=active-pane:matches=0:query=error:source=not-connected"
    );

    let export = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("export-history --output /tmp/history.txt").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(
        export,
        "target=active-pane:export=not-written:output=/tmp/history.txt:reason=live-terminal-state-unavailable"
    );

    let pipe = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("pipe-pane -t 0 cat >/tmp/pane.log").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(
        pipe,
        "target=0:pipe=not-started:command=cat >/tmp/pane.log:reason=live-terminal-state-unavailable"
    );

    let snapshot = execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("save-layout --name checkpoint").unwrap()[0],
    )
    .unwrap();
    match snapshot {
        CommandOutcome::LayoutSave { command, name } => {
            assert_eq!(command, "save-layout");
            assert_eq!(name.as_deref(), Some("checkpoint"));
        }
        outcome => panic!("expected snapshot create outcome, got {outcome:?}"),
    }

    let resume = execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("load-layout --name checkpoint").unwrap()[0],
    )
    .unwrap();
    match resume {
        CommandOutcome::LayoutLoad { command, selector } => {
            assert_eq!(command, "load-layout");
            assert_eq!(selector, LayoutLoadSelector::Name("checkpoint".to_string()));
        }
        outcome => panic!("expected layout load outcome, got {outcome:?}"),
    }

    let resume_latest = execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("load-layout").unwrap()[0],
    )
    .unwrap();
    match resume_latest {
        CommandOutcome::LayoutLoad { command, selector } => {
            assert_eq!(command, "load-layout");
            assert_eq!(selector, LayoutLoadSelector::Latest);
        }
        outcome => panic!("expected latest layout load outcome, got {outcome:?}"),
    }

    let error = execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("delete-buffer missing").unwrap()[0],
    )
    .unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::NotFound);
}

/// Verifies mark pane ready requires acknowledgement before store mutation.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn mark_pane_ready_requires_acknowledgement_before_store_mutation() {
    let (session, primary) = test_session();
    let pane_id = session
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let mut store = PaneReadinessOverrideStore::default();
    store.record_pending_probe(&pane_id, "probe-1").unwrap();

    let warning = display_body(
        execute_mark_pane_ready_command(
            &session,
            &primary,
            &mut store,
            &parse_command_sequence("mark-pane-ready").unwrap()[0],
            PaneReadinessState::Unknown,
            3,
            None,
        )
        .unwrap(),
    );

    assert!(warning.contains(&format!("pane={pane_id}")));
    assert!(warning.contains("acknowledgement_required=true"));
    assert!(warning.contains("override=not-applied"));
    assert!(store.has_pending_probe(&pane_id));

    let applied = display_body(
        execute_mark_pane_ready_command(
            &session,
            &primary,
            &mut store,
            &parse_command_sequence("mark-pane-ready --acknowledge-risk").unwrap()[0],
            PaneReadinessState::PromptCandidate,
            3,
            None,
        )
        .unwrap(),
    );

    assert_eq!(
        applied,
        format!(
            "pane={pane_id}:readiness_state=prompt-candidate:override=applied:epoch=3:pending_probe_cleared=true:audit=not-configured:source=readiness-store"
        )
    );
    assert!(store.allows_epoch(&pane_id, 3));
    assert!(!store.has_pending_probe(&pane_id));
}

/// Verifies mark pane ready can emit audit record.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn mark_pane_ready_can_emit_audit_record() {
    let (session, primary) = test_session();
    let pane_id = session
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let mut store = PaneReadinessOverrideStore::default();
    let root =
        std::env::temp_dir().join(format!("mez-mark-pane-ready-audit-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let audit_path = root.join("audit.jsonl");
    let mut audit_log = AuditLog::new(crate::security::audit::AuditConfig {
        enabled: true,
        path: audit_path.clone(),
        hash_chain: false,
        required: true,
    });

    let applied = display_body(
        execute_mark_pane_ready_command(
            &session,
            &primary,
            &mut store,
            &parse_command_sequence("mark-pane-ready --acknowledge-risk --reason manual").unwrap()
                [0],
            PaneReadinessState::Degraded,
            4,
            Some(&mut audit_log),
        )
        .unwrap(),
    );

    assert!(applied.contains("audit=written"));
    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(audit.contains(r#""event_type":"agent_readiness""#));
    assert!(audit.contains(r#""action":"mark_pane_ready""#));
    assert!(audit.contains(r#""outcome":"applied""#));
    assert!(audit.contains(&format!(r#""pane_id":"{}""#, pane_id)));
    assert!(audit.contains(r#""reason":"manual""#));

    let _ = fs::remove_dir_all(root);
}
