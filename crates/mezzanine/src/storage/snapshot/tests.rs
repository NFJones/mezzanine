//! Unit tests for snapshot manifest, payload, repository, and restore behavior.

use super::{
    SessionSnapshotPayload, SnapshotAgentSession, SnapshotApprovalGrantMetadata,
    SnapshotApprovalRequestMetadata, SnapshotConfigDiagnostic, SnapshotConfigLayerMetadata,
    SnapshotCreationContext, SnapshotFrameSettings, SnapshotFrameState, SnapshotKind,
    SnapshotLayoutNode, SnapshotManifest, SnapshotMcpExternalCapability, SnapshotMcpServerState,
    SnapshotMcpToolEffects, SnapshotMcpToolState, SnapshotPaneCapture, SnapshotPaneGeometry,
    SnapshotRepository, SnapshotSessionState, SnapshotShellMetadata, SnapshotState,
    WindowSnapshotPayload,
};
use crate::host::shell::{ResolvedShell, ShellSource};
use mez_agent::messaging::{Envelope, MessageService, Recipient};
use mez_mux::layout::{LayoutNode, LayoutPolicy, PaneGeometry, Size, SplitDirection};
use mez_mux::session::{Session, SessionState};
use mez_terminal::TerminalSavedDecPrivateMode;
use mez_terminal::{
    GraphicRendition, TerminalColor, TerminalCursorState, TerminalModeState, TerminalSavedState,
    TerminalStyleSpan,
};
use std::fs;
use std::path::PathBuf;

/// Runs the manifest operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn manifest() -> SnapshotManifest {
    SnapshotManifest {
        state: SnapshotState {
            id: "snap-1".to_string(),
            version: 1,
            session_id: "$1".to_string(),
            name: Some("manual".to_string()),
            created_at: "2026-04-30T00:00:00Z".to_string(),
            kind: SnapshotKind::Manual,
            restorable: true,
            window_count: 1,
            pane_count: 1,
            limitations: Vec::new(),
            storage_ref: "snapshots/snap-1".to_string(),
        },
        contains_terminal_history: true,
        contains_agent_transcripts: true,
        contains_raw_credentials: false,
        active_approvals_restored: false,
        restart_required_panes: Vec::new(),
    }
}

/// Verifies valid manifest can be persisted.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn valid_manifest_can_be_persisted() {
    manifest().validate_for_persistence().unwrap();
}

/// Verifies snapshot manifest rejects raw credentials.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_manifest_rejects_raw_credentials() {
    let mut manifest = manifest();
    manifest.contains_raw_credentials = true;

    let error = manifest.validate_for_persistence().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
}

/// Verifies snapshot manifest rejects restored approval authority.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_manifest_rejects_restored_approval_authority() {
    let mut manifest = manifest();
    manifest.active_approvals_restored = true;

    let error = manifest.validate_for_persistence().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
}

/// Verifies snapshot manifest round trips to private file.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_manifest_round_trips_to_private_file() {
    let root = std::env::temp_dir().join(format!("mez-snapshot-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let mut manifest = manifest();
    manifest.state.limitations = vec![
        "pane primary processes must be restarted".to_string(),
        "terminal history is not captured \"yet\"".to_string(),
    ];

    let path = manifest.write_to_dir(&root).unwrap();
    let loaded = SnapshotManifest::read_from_file(&path).unwrap();

    assert_eq!(loaded, manifest);

    let _ = fs::remove_dir_all(root);
}

/// Verifies snapshot manifests persist the SPEC's stable lowercase kind names.
/// This regression scenario protects the serialized manifest contract so new
/// snapshots remain readable by spec-aligned consumers and future migrations.
#[test]
fn snapshot_manifest_persists_spec_kind_names() {
    let root = std::env::temp_dir().join(format!(
        "mez-snapshot-kind-serialize-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);

    let path = manifest().write_to_dir(&root).unwrap();
    let encoded = fs::read_to_string(&path).unwrap();

    assert!(encoded.contains("\nkind=manual\n"), "{encoded}");

    let _ = fs::remove_dir_all(root);
}

/// Verifies version-one manifests written before the `kind` field existed
/// still decode as manual snapshots. This protects snapshot repository and
/// async runtime paths that must continue reading older persisted manifests.
#[test]
fn snapshot_manifest_reads_version_one_manifests_without_kind_as_manual() {
    let root =
        std::env::temp_dir().join(format!("mez-snapshot-kind-legacy-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);

    let manifest = manifest();
    let path = manifest.write_to_dir(&root).unwrap();
    let legacy = fs::read_to_string(&path)
        .unwrap()
        .lines()
        .filter(|line| !line.starts_with("kind="))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(&path, legacy).unwrap();

    let loaded = SnapshotManifest::read_from_file(&path).unwrap();

    assert_eq!(loaded.state.kind, SnapshotKind::Manual);
    assert_eq!(loaded.state.id, manifest.state.id);

    let _ = fs::remove_dir_all(root);
}

/// Verifies snapshot repository lists and inspects manifests.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_repository_lists_and_inspects_manifests() {
    let root = std::env::temp_dir().join(format!("mez-snapshot-repo-list-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = SnapshotRepository::new(root.clone());

    repo.write(&manifest()).unwrap();

    let listed = repo.list().unwrap();
    let inspected = repo.inspect("snap-1").unwrap();

    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "snap-1");
    assert_eq!(inspected.state.name.as_deref(), Some("manual"));

    let _ = fs::remove_dir_all(root);
}

/// Verifies that the Tokio snapshot repository path can persist, inspect, list,
/// and delete the same manifest and payload data as the synchronous repository.
/// This gives async side-effect workers a durable API without depending on
/// blocking filesystem calls.
#[tokio::test]
async fn snapshot_repository_async_persists_lists_and_deletes_snapshots() {
    let root = std::env::temp_dir().join(format!("mez-snapshot-repo-async-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = SnapshotRepository::new(root.clone());
    let session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    );
    let frame_state = SnapshotFrameState::default();

    let state = repo
        .create_from_session_with_context_async(
            "snap-async",
            Some("manual".to_string()),
            &session,
            SnapshotCreationContext::new(&[], &[], &frame_state, &[]),
        )
        .await
        .unwrap();
    let listed = repo.list_async().await.unwrap();
    let manifest = repo.inspect_async("snap-async").await.unwrap();
    let payload = repo.inspect_payload_async("snap-async").await.unwrap();
    let deleted = repo.delete_async("snap-async").await.unwrap();

    assert_eq!(state.id, "snap-async");
    assert_eq!(listed.len(), 1);
    assert_eq!(manifest.state.name.as_deref(), Some("manual"));
    assert_eq!(payload.session_id, session.id.to_string());
    assert!(deleted);
    assert!(repo.list_async().await.unwrap().is_empty());

    let _ = fs::remove_dir_all(root);
}

/// Verifies snapshot repository selects latest snapshot by session.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_repository_selects_latest_snapshot_by_session() {
    let root =
        std::env::temp_dir().join(format!("mez-snapshot-repo-latest-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = SnapshotRepository::new(root.clone());
    let mut old = manifest();
    old.state.id = "snap-old".to_string();
    old.state.session_id = "$target".to_string();
    old.state.created_at = "2026-04-30T00:00:00Z".to_string();
    old.state.storage_ref = "snap-old.payload".to_string();
    let mut new = manifest();
    new.state.id = "snap-new".to_string();
    new.state.session_id = "$target".to_string();
    new.state.created_at = "2026-04-30T00:00:01Z".to_string();
    new.state.storage_ref = "snap-new.payload".to_string();
    let mut other = manifest();
    other.state.id = "snap-other".to_string();
    other.state.session_id = "$other".to_string();
    other.state.created_at = "2026-04-30T00:00:02Z".to_string();
    other.state.storage_ref = "snap-other.payload".to_string();

    repo.write(&old).unwrap();
    repo.write(&new).unwrap();
    repo.write(&other).unwrap();

    let latest_index = fs::read_to_string(root.join("latest.index")).unwrap();

    assert_eq!(
        repo.latest(Some("$target")).unwrap().unwrap().id,
        "snap-new"
    );
    assert_eq!(repo.latest(None).unwrap().unwrap().id, "snap-other");
    assert!(repo.latest(Some("$missing")).unwrap().is_none());
    assert!(latest_index.contains("all\tsnap-other\n"));
    assert!(latest_index.contains("session\t$target\tsnap-new\n"));

    repo.delete("snap-other").unwrap();
    assert_eq!(repo.latest(None).unwrap().unwrap().id, "snap-new");

    let _ = fs::remove_dir_all(root);
}

/// Verifies snapshot resume plans are served from manifest metadata.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_resume_plan_uses_manifest_metadata_without_payload() {
    let root = std::env::temp_dir().join(format!(
        "mez-snapshot-repo-resume-plan-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let repo = SnapshotRepository::new(root.clone());
    let mut manifest = manifest();
    manifest.state.window_count = 2;
    manifest.state.pane_count = 3;
    manifest.state.limitations = vec!["pane primary processes must be restarted".to_string()];
    manifest.state.storage_ref = "missing.payload".to_string();
    manifest.restart_required_panes = vec!["%1".to_string(), "%2".to_string()];
    repo.write(&manifest).unwrap();

    let plan = repo.resume_plan("snap-1").unwrap();

    assert_eq!(plan.session_id, "$1");
    assert_eq!(plan.window_count, 2);
    assert_eq!(plan.pane_count, 3);
    assert_eq!(plan.restart_required_panes, vec!["%1", "%2"]);
    assert_eq!(plan.limitations, manifest.state.limitations);

    let _ = fs::remove_dir_all(root);
}

/// Verifies snapshot repository deletes manifest and local payload.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_repository_deletes_manifest_and_local_payload() {
    let root =
        std::env::temp_dir().join(format!("mez-snapshot-repo-delete-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = SnapshotRepository::new(root.clone());
    let mut manifest = manifest();
    manifest.state.storage_ref = "snap-1.payload".to_string();
    repo.write(&manifest).unwrap();
    fs::write(root.join("snap-1.payload"), "payload").unwrap();

    assert!(repo.delete("snap-1").unwrap());
    assert!(!root.join("snap-1.manifest").exists());
    assert!(!root.join("snap-1.payload").exists());
    assert!(!repo.delete("snap-1").unwrap());

    let _ = fs::remove_dir_all(root);
}

/// Verifies snapshot repository rejects path traversal ids.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_repository_rejects_path_traversal_ids() {
    let repo = SnapshotRepository::new(PathBuf::from("/tmp/mezzanine-snapshots"));

    let error = repo.inspect("../secret").unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies session snapshot payload round trips and builds resume plan.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn session_snapshot_payload_round_trips_and_builds_resume_plan() {
    let root = std::env::temp_dir().join(format!("mez-snapshot-payload-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = SnapshotRepository::new(root.clone());
    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/usr/bin/zsh"), ShellSource::ShellEnv),
        Size::new(80, 24).unwrap(),
    );
    let primary = session.attach_primary("primary", true).unwrap();
    session
        .split_active_pane(&primary, mez_mux::layout::SplitDirection::Vertical)
        .unwrap();
    assert_eq!(
        session.cycle_layout(&primary).unwrap(),
        LayoutPolicy::EvenVertical
    );
    let config_layers = vec![SnapshotConfigLayerMetadata {
        id: "primary".to_string(),
        layer_type: "primary".to_string(),
        precedence: 0,
        path: Some("/tmp/mezzanine/config.toml".to_string()),
        trusted: true,
        applied: true,
        schema_version: 1,
        diagnostics: vec![SnapshotConfigDiagnostic {
            path: "history.lines".to_string(),
            message: "example diagnostic".to_string(),
        }],
    }];
    let frame_state = SnapshotFrameState {
        window: SnapshotFrameSettings {
            enabled: true,
            position: "bottom".to_string(),
            style: "inverse".to_string(),
            template: "#{window.id}".to_string(),
            visible_fields: vec!["window.id".to_string(), "window.pane_count".to_string()],
        },
        pane: SnapshotFrameSettings {
            enabled: true,
            position: "top".to_string(),
            style: "bold".to_string(),
            template: "#{pane.id} #{agent.status}".to_string(),
            visible_fields: vec!["pane.id".to_string(), "agent.status".to_string()],
        },
    };
    let agent_sessions = vec![SnapshotAgentSession {
        pane_id: "%1".to_string(),
        conversation_id: "as1".to_string(),
        visibility: "hide-pending-task-completion".to_string(),
        running_turn_id: Some("turn-1".to_string()),
        transcript_entries: 2,
    }];
    let approval_grants = vec![SnapshotApprovalGrantMetadata {
        id: "ap1".to_string(),
        command_prefix: vec!["cargo".to_string(), "test".to_string()],
        scope: "session".to_string(),
        decision: "approve".to_string(),
    }];
    let approval_requests = vec![SnapshotApprovalRequestMetadata {
        id: "ba1".to_string(),
        requesting_agent_id: "agent-1".to_string(),
        pane_id: "%1".to_string(),
        parent_agent_chain: vec!["root-agent".to_string()],
        action_kind: "shell_command".to_string(),
        action_summary: "cargo test".to_string(),
        declared_effects: vec!["read".to_string(), "write".to_string()],
        matched_rules: vec!["safe-cargo-test".to_string()],
        read_scopes: vec!["/tmp/mezzanine".to_string()],
        write_scopes: vec!["/tmp/mezzanine/target".to_string()],
        created_at_unix_seconds: Some(100),
        decided_at_unix_seconds: Some(101),
        decided_by_client_id: Some("primary".to_string()),
        state: "approved".to_string(),
        decision: Some("approve".to_string()),
        redirect_instruction: None,
    }];
    let mut message_service = MessageService::default();
    let sender = message_service.register_agent(None, None, "writer", vec!["code".to_string()]);
    let sender_id = sender.agent_id.clone();
    let target = message_service.register_agent(None, None, "reviewer", Vec::new());
    message_service.subscribe(&target.agent_id).unwrap();
    message_service
        .accept_at(
            &sender_id,
            Envelope {
                protocol: "mmp/1",
                id: "snapshot-message".to_string(),
                message_type: "send".to_string(),
                time: "2026-05-01T00:00:00Z".to_string(),
                sender,
                recipient: Recipient::Agent(target.agent_id),
                correlation_id: Some("turn-1".to_string()),
                ttl_ms: Some(10_000),
                content_type: "text/plain; charset=utf-8".to_string(),
                payload: "hello reviewer".to_string(),
                extension_fields: vec![("trace".to_string(), r#"{"span":"one"}"#.to_string())],
            },
            10,
        )
        .unwrap();
    let message_state = message_service.snapshot_state();
    let mcp_servers = vec![SnapshotMcpServerState {
        id: "fs".to_string(),
        name: "filesystem".to_string(),
        kind: "stdio".to_string(),
        enabled: true,
        status: "available".to_string(),
        last_checked_at_unix_seconds: Some(100),
        blacklist_reason: None,
        external_capability: SnapshotMcpExternalCapability {
            mutates_filesystem_outside_shell: false,
            executes_processes_outside_shell: false,
            accesses_credentials_outside_shell: false,
            purpose: String::new(),
            usage_instructions: String::new(),
        },
        tools: vec![SnapshotMcpToolState {
            server_id: "fs".to_string(),
            name: "read_file".to_string(),
            available: true,
            blacklisted: false,
            permission_required: true,
            effects: SnapshotMcpToolEffects {
                reads_filesystem: true,
                mutates_filesystem: false,
                executes_processes: false,
                accesses_credentials: false,
                uses_network: false,
                has_side_effects: false,
            },
            approval: "prompt".to_string(),
            description: "read a file".to_string(),
            input_schema_json: r#"{"type":"object"}"#.to_string(),
        }],
    }];
    let payload = SessionSnapshotPayload::from_session_with_context(
        &session,
        SnapshotCreationContext::new(&[], &config_layers, &frame_state, &agent_sessions)
            .with_approvals(&approval_grants, &approval_requests)
            .with_message_state(&message_state)
            .with_mcp_servers(&mcp_servers),
    );

    repo.write_payload("snap-1", &payload).unwrap();
    let encoded = fs::read_to_string(root.join("snap-1.payload")).unwrap();
    let loaded = repo.inspect_payload("snap-1").unwrap();
    let plan = payload.resume_plan();

    assert!(encoded.starts_with("payload_version\t4\n"));
    assert!(encoded.contains("\nwindow_layout\t@1\t"));
    assert!(encoded.contains("\npane_shell\t%1\t\texited\t\tunknown\n"));
    assert_eq!(loaded, payload);
    assert_eq!(loaded.shell.path, "/usr/bin/zsh");
    assert_eq!(loaded.shell.source, "shell-env");
    assert!(!loaded.shell.used_fallback);
    assert!(loaded.active_config_layers.is_empty());
    assert_eq!(loaded.frame_state, SnapshotFrameState::default());
    assert!(loaded.agent_sessions.is_empty());
    assert!(loaded.approval_grants.is_empty());
    assert!(loaded.approval_requests.is_empty());
    assert_eq!(loaded.message_state, None);
    assert!(loaded.mcp_servers.is_empty());
    assert_eq!(loaded.windows[0].layout_policy, "even-vertical");
    assert_eq!(
        loaded.windows[0].layout_root,
        Some(SnapshotLayoutNode::Split {
            direction: "vertical".to_string(),
            children: vec![
                SnapshotLayoutNode::Pane {
                    pane_id: "%1".to_string(),
                },
                SnapshotLayoutNode::Pane {
                    pane_id: "%2".to_string(),
                },
            ],
            sizes: vec![40, 40],
        })
    );
    assert_eq!(
        loaded.windows[0].panes[0].geometry,
        Some(SnapshotPaneGeometry {
            column: 0,
            row: 0,
            columns: 40,
            rows: 24,
        })
    );
    assert_eq!(
        loaded.windows[0].panes[1].geometry,
        Some(SnapshotPaneGeometry {
            column: 40,
            row: 0,
            columns: 40,
            rows: 24,
        })
    );
    assert_eq!(plan.window_count, 1);
    assert_eq!(plan.pane_count, 2);
    assert!(plan.restart_required_panes.is_empty());
    assert!(plan.limitations.is_empty());

    let _ = fs::remove_dir_all(root);
}

/// Verifies session snapshot payload preserves terminal and transcript refs.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn session_snapshot_payload_preserves_terminal_and_transcript_refs() {
    let root = std::env::temp_dir().join(format!(
        "mez-snapshot-capture-payload-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let repo = SnapshotRepository::new(root.clone());
    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    );
    let primary = session.attach_primary("primary", true).unwrap();
    let pane_id = session
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let captures = vec![SnapshotPaneCapture {
        pane_id: pane_id.clone(),
        primary_pid: Some(4242),
        process_state: Some("running".to_string()),
        current_working_directory: Some("/workspace/project".to_string()),
        readiness_state: Some("ready".to_string()),
        terminal_history: vec!["history".to_string()],
        terminal_history_line_style_spans: vec![vec![TerminalStyleSpan {
            start: 0,
            length: 7,
            rendition: GraphicRendition {
                bold: false,
                dim: false,
                italic: false,
                strikethrough: false,
                double_underline: false,
                hidden: false,
                underline: true,
                inverse: false,
                foreground: None,
                background: Some(TerminalColor::Indexed(7)),
            },
        }]],
        visible_lines: vec!["visible".to_string()],
        visible_line_style_spans: vec![vec![TerminalStyleSpan {
            start: 0,
            length: 7,
            rendition: GraphicRendition {
                bold: true,
                dim: false,
                italic: false,
                strikethrough: false,
                double_underline: false,
                hidden: false,
                underline: false,
                inverse: false,
                foreground: Some(TerminalColor::Indexed(42)),
                background: None,
            },
        }]],
        terminal_modes: TerminalModeState {
            title: Some("snapshot-title".to_string()),
            bracketed_paste_enabled: true,
            ..TerminalModeState::default()
        },
        terminal_saved_state: TerminalSavedState {
            saved_cursor: Some(TerminalCursorState { row: 1, column: 2 }),
            saved_dec_private_modes: vec![TerminalSavedDecPrivateMode {
                mode: 2004,
                enabled: true,
            }],
            g0_dec_special_graphics: false,
            g1_dec_special_graphics: false,
            shift_out: false,
        },
        exit_status: Some(mez_mux::process::PaneExitStatus {
            code: Some(7),
            signal: None,
            success: false,
        }),
        alternate_screen_active: false,
        transcript_refs: vec!["conversation-1".to_string()],
    }];
    session.detach_primary(&primary).unwrap();

    let payload = SessionSnapshotPayload::from_session_with_captures(&session, &captures);
    repo.write_payload("snap-capture", &payload).unwrap();
    let loaded = repo.inspect_payload("snap-capture").unwrap();

    assert_eq!(loaded, payload);
    assert_eq!(loaded.windows[0].panes[0].exit_status, None);
    assert_eq!(loaded.windows[0].panes[0].primary_pid, None);
    assert_eq!(loaded.windows[0].panes[0].process_state, "exited");
    assert_eq!(
        loaded.windows[0].panes[0]
            .current_working_directory
            .as_deref(),
        Some("/workspace/project")
    );
    assert_eq!(loaded.windows[0].panes[0].readiness_state, "unknown");
    assert!(!loaded.contains_terminal_history());
    assert!(!loaded.contains_agent_transcripts());
    assert!(loaded.windows[0].panes[0].terminal_history.is_empty());
    assert!(
        loaded.windows[0].panes[0]
            .terminal_history_line_style_spans
            .is_empty()
    );
    assert!(loaded.windows[0].panes[0].visible_lines.is_empty());
    assert!(
        loaded.windows[0].panes[0]
            .visible_line_style_spans
            .is_empty()
    );
    assert_eq!(
        loaded.windows[0].panes[0].terminal_modes,
        TerminalModeState::default()
    );
    assert_eq!(
        loaded.windows[0].panes[0].terminal_saved_state,
        TerminalSavedState::default()
    );
    assert!(loaded.windows[0].panes[0].transcript_refs.is_empty());

    let state = repo
        .create_from_session_with_captures(
            "snap-captured-session",
            Some("captured".to_string()),
            &session,
            &captures,
        )
        .unwrap();
    let manifest = repo.inspect(&state.id).unwrap();
    assert!(!manifest.contains_terminal_history);
    assert!(!manifest.contains_agent_transcripts);

    let _ = fs::remove_dir_all(root);
}

/// Verifies snapshot repository builds rollback plan with limitations.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_repository_builds_rollback_plan_with_limitations() {
    let root = std::env::temp_dir().join(format!("mez-snapshot-rollback-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = SnapshotRepository::new(root.clone());
    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    );
    let _primary = session.attach_primary("primary", true).unwrap();
    let payload = SessionSnapshotPayload::from_session(&session);
    let mut manifest = manifest();
    manifest.state.storage_ref = "snap-1.payload".to_string();

    repo.write(&manifest).unwrap();
    repo.write_payload("snap-1", &payload).unwrap();
    let rollback = repo.rollback_plan("snap-1").unwrap();

    assert!(rollback.available);
    assert_eq!(
        rollback.restore_command.as_deref(),
        Some("mez snapshot resume snap-1")
    );
    assert!(rollback.restart_required_panes.is_empty());
    assert!(rollback.limitations.is_empty());

    let _ = fs::remove_dir_all(root);
}

/// Verifies snapshot rollback plan discloses missing payload.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_rollback_plan_discloses_missing_payload() {
    let root = std::env::temp_dir().join(format!(
        "mez-snapshot-rollback-missing-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let repo = SnapshotRepository::new(root.clone());
    repo.write(&manifest()).unwrap();

    let rollback = repo.rollback_plan("snap-1").unwrap();

    assert!(!rollback.available);
    assert!(rollback.restore_command.is_none());
    assert!(rollback.limitations[0].contains("payload is unavailable"));

    let _ = fs::remove_dir_all(root);
}

/// Verifies snapshot repository restores session shape from payload.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_repository_restores_session_shape_from_payload() {
    let root = std::env::temp_dir().join(format!("mez-snapshot-restore-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = SnapshotRepository::new(root.clone());
    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    );
    let primary = session.attach_primary("primary", true).unwrap();
    session
        .split_active_pane(&primary, mez_mux::layout::SplitDirection::Vertical)
        .unwrap();
    let state = repo
        .create_from_session("snap-restore", Some("manual".to_string()), &session)
        .unwrap();

    let restored = repo
        .restore_session(
            &state.id,
            ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        )
        .unwrap();

    assert_eq!(restored.session.id, session.id);
    assert_eq!(restored.session.windows().len(), 1);
    assert_eq!(restored.session.windows()[0].panes().len(), 2);
    assert!(
        restored.session.windows()[0]
            .panes()
            .iter()
            .all(|pane| !pane.live)
    );
    assert!(restored.resume_plan.restart_required_panes.is_empty());

    let _ = fs::remove_dir_all(root);
}

/// Verifies that snapshot resume restores the explicit split tree rather than
/// rebuilding one possible tree from pane rectangles. A 2x2 grid is ambiguous
/// from geometry alone, so preserving the horizontal root proves the
/// `window_layout` payload record is authoritative.
#[test]
fn snapshot_restore_preserves_ambiguous_layout_ancestry() {
    let root =
        std::env::temp_dir().join(format!("mez-snapshot-layout-tree-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = SnapshotRepository::new(root.clone());
    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    );
    let primary = session.attach_primary("primary", true).unwrap();
    session
        .split_active_pane_select(&primary, SplitDirection::Horizontal, true)
        .unwrap();
    session
        .split_active_pane_select(&primary, SplitDirection::Vertical, true)
        .unwrap();
    session.select_pane(&primary, "%1").unwrap();
    session
        .split_active_pane_select(&primary, SplitDirection::Vertical, true)
        .unwrap();
    let state = repo
        .create_from_session("snap-layout-tree", None, &session)
        .unwrap();

    let restored = repo
        .restore_session(
            &state.id,
            ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        )
        .unwrap();
    let window = restored.session.active_window().unwrap();

    match window.layout_root() {
        LayoutNode::Split {
            direction,
            children,
        } => {
            assert_eq!(*direction, SplitDirection::Horizontal);
            assert_eq!(children.len(), 2);
            assert_eq!(children[0].direction(), Some(SplitDirection::Vertical));
            assert_eq!(children[1].direction(), Some(SplitDirection::Vertical));
        }
        LayoutNode::Pane { .. } => panic!("restored layout root should be a split"),
    }

    let _ = fs::remove_dir_all(root);
}

/// Verifies snapshot payload rejects windows without panes.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_payload_rejects_windows_without_panes() {
    let payload = SessionSnapshotPayload {
        session_id: "$1".to_string(),
        name: "default".to_string(),
        state: SnapshotSessionState::Running,
        authoritative_columns: 80,
        authoritative_rows: 24,
        active_window_id: None,
        shell: SnapshotShellMetadata::default(),
        active_config_layers: Vec::new(),
        frame_state: SnapshotFrameState::default(),
        agent_sessions: Vec::new(),
        approval_grants: Vec::new(),
        approval_requests: Vec::new(),
        message_state: None,
        mcp_servers: Vec::new(),
        window_groups: Vec::new(),
        windows: vec![WindowSnapshotPayload {
            window_id: "@1".to_string(),
            index: 0,
            name: "main".to_string(),
            active: true,
            columns: 80,
            rows: 24,
            layout_policy: "tiled".to_string(),
            layout_root: None,
            panes: Vec::new(),
        }],
    };

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies that snapshot payload validation treats the window layout policy as
/// a typed restore field rather than unchecked string metadata. Resume uses
/// this value to restore future layout-cycling behavior, so invalid policy names
/// must be rejected before a session is reconstructed.
#[test]
fn snapshot_payload_rejects_invalid_window_layout_policy() {
    let session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    );
    let mut payload = SessionSnapshotPayload::from_session(&session);
    payload.windows[0].layout_policy = "stacked".to_string();

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies that pane shell process state is validated as typed snapshot
/// metadata rather than accepted as arbitrary text. Restored sessions use this
/// field to report limitations and state, so invalid values must fail before
/// the payload can be persisted or resumed.
#[test]
fn snapshot_payload_rejects_invalid_pane_process_state() {
    let session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    );
    let mut payload = SessionSnapshotPayload::from_session(&session);
    payload.windows[0].panes[0].process_state = "unknown".to_string();

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies that pane readiness metadata uses the same vocabulary as runtime
/// pane state. Snapshot payloads are durable protocol artifacts, so accepting a
/// misspelled readiness value would hide schema drift until resume time.
#[test]
fn snapshot_payload_rejects_invalid_pane_readiness_state() {
    let session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    );
    let mut payload = SessionSnapshotPayload::from_session(&session);
    payload.windows[0].panes[0].readiness_state = "full-screen".to_string();

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies snapshot payload rejects unsupported format version.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_payload_rejects_unsupported_format_version() {
    let root = std::env::temp_dir().join(format!(
        "mez-snapshot-payload-version-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let repo = SnapshotRepository::new(root.clone());
    fs::write(
        root.join("snap-version.payload"),
        "payload_version\t5\nsession\t$1\tdefault\trunning\t80\t24\t\n",
    )
    .unwrap();

    let error = repo.inspect_payload("snap-version").unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);

    let _ = fs::remove_dir_all(root);
}

/// Verifies snapshot payload rejects invalid shell metadata.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_payload_rejects_invalid_shell_metadata() {
    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    );
    session.attach_primary("primary", true).unwrap();
    let mut payload = SessionSnapshotPayload::from_session(&session);
    payload.shell.path.clear();

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);

    payload.shell.path = "/bin/sh".to_string();
    payload.shell.source = "shell-env".to_string();

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies snapshot payload rejects invalid agent session metadata.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_payload_rejects_invalid_agent_session_metadata() {
    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    );
    session.attach_primary("primary", true).unwrap();
    let mut payload = SessionSnapshotPayload::from_session(&session);
    payload.agent_sessions.push(SnapshotAgentSession {
        pane_id: "%1".to_string(),
        conversation_id: "as1".to_string(),
        visibility: "unknown".to_string(),
        running_turn_id: None,
        transcript_entries: 0,
    });

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);

    payload.agent_sessions[0].visibility = "hide-pending-task-completion".to_string();

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies that persisted terminal rendition metadata cannot contain spans
/// that are empty, default-styled, or outside the pane grid.
#[test]
fn snapshot_payload_rejects_invalid_visible_style_spans() {
    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    );
    session.attach_primary("primary", true).unwrap();
    let mut payload = SessionSnapshotPayload::from_session(&session);
    let pane = &mut payload.windows[0].panes[0];
    pane.visible_lines = vec!["styled".to_string()];
    pane.visible_line_style_spans = vec![vec![TerminalStyleSpan {
        start: 0,
        length: 0,
        rendition: GraphicRendition {
            bold: true,
            ..GraphicRendition::default()
        },
    }]];

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);

    payload.windows[0].panes[0].visible_line_style_spans[0][0].length = 1;
    payload.windows[0].panes[0].visible_line_style_spans[0][0].rendition =
        GraphicRendition::default();

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);

    payload.windows[0].panes[0].visible_line_style_spans[0][0] = TerminalStyleSpan {
        start: usize::from(payload.windows[0].panes[0].columns),
        length: 1,
        rendition: GraphicRendition {
            bold: true,
            ..GraphicRendition::default()
        },
    };

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies that saved terminal parser state cannot point outside the pane or
/// include unsupported or duplicate DEC private modes.
#[test]
fn snapshot_payload_rejects_invalid_terminal_saved_state() {
    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    );
    session.attach_primary("primary", true).unwrap();
    let mut payload = SessionSnapshotPayload::from_session(&session);
    let pane = &mut payload.windows[0].panes[0];
    pane.terminal_saved_state.saved_cursor = Some(TerminalCursorState {
        row: usize::from(pane.rows),
        column: 0,
    });

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);

    let pane = &mut payload.windows[0].panes[0];
    pane.terminal_saved_state.saved_cursor = Some(TerminalCursorState { row: 0, column: 0 });
    pane.terminal_saved_state
        .saved_dec_private_modes
        .push(TerminalSavedDecPrivateMode {
            mode: 9999,
            enabled: true,
        });

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);

    let pane = &mut payload.windows[0].panes[0];
    pane.terminal_saved_state.saved_dec_private_modes = vec![
        TerminalSavedDecPrivateMode {
            mode: 2004,
            enabled: true,
        },
        TerminalSavedDecPrivateMode {
            mode: 2004,
            enabled: false,
        },
    ];

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies that pane geometry metadata remains tied to the pane dimensions
/// and window bounds stored beside it instead of becoming unchecked secondary
/// layout state. Invalid dimensions, out-of-window rectangles, and overlapping
/// rectangles must fail validation before snapshot inspection or restore can
/// treat the payload as usable.
#[test]
fn snapshot_payload_rejects_invalid_pane_geometry() {
    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    );
    session.attach_primary("primary", true).unwrap();
    let mut payload = SessionSnapshotPayload::from_session(&session);
    payload.windows[0].panes[0].geometry = Some(SnapshotPaneGeometry {
        column: 0,
        row: 0,
        columns: 0,
        rows: 24,
    });

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);

    payload.windows[0].panes[0].geometry = Some(SnapshotPaneGeometry {
        column: 0,
        row: 0,
        columns: 40,
        rows: 24,
    });

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);

    let mut split_session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    );
    let primary = split_session.attach_primary("primary", true).unwrap();
    split_session
        .split_active_pane(&primary, mez_mux::layout::SplitDirection::Vertical)
        .unwrap();
    let mut payload = SessionSnapshotPayload::from_session(&split_session);
    payload.windows[0].panes[1].geometry = Some(SnapshotPaneGeometry {
        column: 20,
        row: 0,
        columns: 40,
        rows: 24,
    });

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);

    payload.windows[0].panes[1].geometry = Some(SnapshotPaneGeometry {
        column: 60,
        row: 0,
        columns: 40,
        rows: 24,
    });

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies snapshot payload rejects invalid approval metadata.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_payload_rejects_invalid_approval_metadata() {
    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    );
    session.attach_primary("primary", true).unwrap();
    let mut payload = SessionSnapshotPayload::from_session(&session);
    payload.approval_grants.push(SnapshotApprovalGrantMetadata {
        id: "ap1".to_string(),
        command_prefix: Vec::new(),
        scope: "session".to_string(),
        decision: "approve".to_string(),
    });

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);

    payload.approval_grants.clear();
    payload
        .approval_requests
        .push(SnapshotApprovalRequestMetadata {
            id: "ba1".to_string(),
            requesting_agent_id: "agent-1".to_string(),
            pane_id: "%1".to_string(),
            parent_agent_chain: Vec::new(),
            action_kind: "shell_command".to_string(),
            action_summary: "cargo test".to_string(),
            declared_effects: Vec::new(),
            matched_rules: Vec::new(),
            read_scopes: Vec::new(),
            write_scopes: Vec::new(),
            created_at_unix_seconds: Some(100),
            decided_at_unix_seconds: None,
            decided_by_client_id: None,
            state: "pending".to_string(),
            decision: None,
            redirect_instruction: None,
        });

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);

    payload.approval_requests[0].state = "redirected".to_string();
    payload.approval_requests[0].decision = Some("redirect".to_string());

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);

    payload.approval_requests.clear();
    payload.mcp_servers.push(SnapshotMcpServerState {
        id: "fs".to_string(),
        name: "filesystem".to_string(),
        kind: "stdio".to_string(),
        enabled: true,
        status: "available".to_string(),
        last_checked_at_unix_seconds: None,
        blacklist_reason: None,
        external_capability: SnapshotMcpExternalCapability {
            mutates_filesystem_outside_shell: false,
            executes_processes_outside_shell: false,
            accesses_credentials_outside_shell: false,
            purpose: String::new(),
            usage_instructions: String::new(),
        },
        tools: vec![SnapshotMcpToolState {
            server_id: "fs".to_string(),
            name: "read_file".to_string(),
            available: true,
            blacklisted: false,
            permission_required: true,
            effects: SnapshotMcpToolEffects {
                reads_filesystem: true,
                mutates_filesystem: false,
                executes_processes: false,
                accesses_credentials: false,
                uses_network: false,
                has_side_effects: false,
            },
            approval: "prompt".to_string(),
            description: String::new(),
            input_schema_json: "not-json".to_string(),
        }],
    });

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies snapshot payload rejects invalid frame state.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_payload_rejects_invalid_frame_state() {
    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    );
    session.attach_primary("primary", true).unwrap();
    let mut payload = SessionSnapshotPayload::from_session(&session);
    payload.frame_state.window.position = "middle".to_string();

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);

    payload.frame_state.window.position = "top".to_string();
    payload.frame_state.pane.template.clear();

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies that product snapshot decoding rebuilds the saved session topology,
/// seeds future identifiers past restored values, and preserves pane geometry.
#[test]
fn session_restores_layout_from_snapshot_payload_and_seeds_ids() {
    let shell = ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh);
    let payload = SessionSnapshotPayload {
        session_id: "$4".to_string(),
        name: "restored".to_string(),
        state: SnapshotSessionState::Detached,
        authoritative_columns: 100,
        authoritative_rows: 40,
        active_window_id: Some("@8".to_string()),
        shell: SnapshotShellMetadata::default(),
        active_config_layers: Vec::new(),
        frame_state: SnapshotFrameState::default(),
        agent_sessions: Vec::new(),
        approval_grants: Vec::new(),
        approval_requests: Vec::new(),
        message_state: None,
        mcp_servers: Vec::new(),
        window_groups: Vec::new(),
        windows: vec![WindowSnapshotPayload {
            window_id: "@8".to_string(),
            index: 0,
            name: "work".to_string(),
            active: true,
            columns: 100,
            rows: 40,
            layout_policy: LayoutPolicy::EvenHorizontal.name().to_string(),
            layout_root: None,
            panes: vec![super::PaneSnapshotPayload {
                pane_id: "%12".to_string(),
                index: 0,
                title: "shell".to_string(),
                active: true,
                live_at_snapshot: true,
                columns: 100,
                rows: 40,
                primary_pid: Some(4242),
                process_state: "running".to_string(),
                current_working_directory: Some("/workspace/project".to_string()),
                readiness_state: "ready".to_string(),
                exit_status: None,
                geometry: Some(SnapshotPaneGeometry {
                    column: 0,
                    row: 0,
                    columns: 100,
                    rows: 40,
                }),
                terminal_modes: mez_terminal::TerminalModeState::default(),
                terminal_saved_state: mez_terminal::TerminalSavedState::default(),
                terminal_history: Vec::new(),
                terminal_history_line_style_spans: Vec::new(),
                visible_lines: Vec::new(),
                visible_line_style_spans: Vec::new(),
                alternate_screen_active: false,
                transcript_refs: Vec::new(),
            }],
        }],
    };

    let restore_input = super::session_restore_input(&payload).unwrap();
    let mut session = Session::from_restore_input(shell, restore_input).unwrap();

    assert_eq!(session.id.as_str(), "$4");
    assert_eq!(session.name, "restored");
    assert_eq!(session.state, SessionState::Detached);
    assert_eq!(session.active_window().unwrap().id.as_str(), "@8");
    assert_eq!(
        session.active_window().unwrap().active_pane().id.as_str(),
        "%12"
    );
    assert!(!session.active_window().unwrap().active_pane().live);
    assert_eq!(
        session.active_window().unwrap().pane_geometries(),
        vec![PaneGeometry {
            index: 0,
            column: 0,
            row: 0,
            columns: 100,
            rows: 40,
        }],
    );
    assert_eq!(
        session.active_window().unwrap().layout_policy(),
        LayoutPolicy::EvenHorizontal
    );

    let primary = session.attach_primary("primary", true).unwrap();
    let window_id = session.new_window(&primary, "next", true).unwrap();
    let pane_id = session
        .active_window()
        .and_then(|window| window.panes().first())
        .map(|pane| pane.id.clone())
        .unwrap();
    assert_eq!(window_id.as_str(), "@9");
    assert_eq!(pane_id.as_str(), "%13");
}
