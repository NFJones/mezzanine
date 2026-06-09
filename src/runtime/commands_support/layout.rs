//! Runtime layout and snapshot terminal-command helpers.
//!
//! Command-support execution still lives in the parent module. This child
//! module owns the layout-affecting terminal commands that must route through
//! runtime pane creation, resize, and snapshot control paths instead of the
//! pure session command executor.

use super::super::{
    CommandInvocation, CommandOutcome, MezError, Result, RuntimeLifecycleState,
    RuntimeSessionService, current_unix_seconds, json_escape, new_window_name,
    new_window_shell_command, resize_spec_from_invocation, split_window_selects_new_pane,
    split_window_shell_command,
};
use super::runtime_expand_user_path;
use crate::command::LayoutLoadSelector;
use crate::control::ControlConnectionState;
use crate::layout::SplitDirection;
use crate::snapshot::{SnapshotRepository, SnapshotState};

/// Resolves typed snapshot command outcomes through the live runtime snapshot repository.
pub(super) fn resolve_runtime_layout_command_outcome(
    service: &mut RuntimeSessionService,
    active_client_id: &mut crate::ids::ClientId,
    outcome: CommandOutcome,
) -> Result<CommandOutcome> {
    match outcome {
        CommandOutcome::LayoutSave { command, name } => {
            let Some(snapshots) = service.snapshot_repository.clone() else {
                return Ok(CommandOutcome::LayoutSave { command, name });
            };
            let body = runtime_layout_save_command(service, active_client_id, &snapshots, name)?;
            Ok(CommandOutcome::Display { command, body })
        }
        CommandOutcome::LayoutLoad { command, selector } => {
            let Some(snapshots) = service.snapshot_repository.clone() else {
                return Ok(CommandOutcome::LayoutLoad { command, selector });
            };
            let body =
                runtime_layout_load_command(service, active_client_id, &snapshots, &selector)?;
            Ok(CommandOutcome::Display { command, body })
        }
        outcome => Ok(outcome),
    }
}

/// Creates a version-four UUID string for an unnamed saved layout.
fn new_layout_uuid() -> String {
    let mut bytes: [u8; 16] = rand::random();
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

/// Saves the current layout through the runtime snapshot repository path.
fn runtime_layout_save_command(
    service: &mut RuntimeSessionService,
    active_client_id: &mut crate::ids::ClientId,
    snapshots: &SnapshotRepository,
    name: Option<String>,
) -> Result<String> {
    let snapshot_count = snapshots.list()?.len();
    let layout_name = name.unwrap_or_else(new_layout_uuid);
    let idempotency_key = format!(
        "terminal-command:save-layout:{}:{}:{}:{}",
        service.session.id,
        current_unix_seconds(),
        snapshot_count,
        layout_name
    );
    let body = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"snapshot/create","params":{{"target":{{"default":true}},"name":"{}","idempotency_key":"{}"}}}}"#,
        json_escape(&layout_name),
        json_escape(&idempotency_key)
    );
    dispatch_runtime_snapshot_terminal_command(service, active_client_id, snapshots, &body)
}

/// Resumes a live session snapshot through the runtime snapshot resume control path.
fn runtime_layout_load_command(
    service: &mut RuntimeSessionService,
    active_client_id: &mut crate::ids::ClientId,
    snapshots: &SnapshotRepository,
    selector: &LayoutLoadSelector,
) -> Result<String> {
    let snapshot_id = runtime_layout_id_for_selector(snapshots, selector)?;
    let idempotency_key = format!("terminal-command:load-layout:{snapshot_id}");
    let body = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"snapshot/resume","params":{{"snapshot_id":"{}","idempotency_key":"{}"}}}}"#,
        json_escape(&snapshot_id),
        json_escape(&idempotency_key)
    );
    dispatch_runtime_snapshot_terminal_command(service, active_client_id, snapshots, &body)
}

/// Dispatches a snapshot control request and tracks a re-bound primary client after resume.
fn dispatch_runtime_snapshot_terminal_command(
    service: &mut RuntimeSessionService,
    active_client_id: &mut crate::ids::ClientId,
    snapshots: &SnapshotRepository,
    body: &str,
) -> Result<String> {
    let mut connection = ControlConnectionState::trusted_existing_client(active_client_id.clone());
    let response = service.dispatch_runtime_control_body_for_connection_with_snapshots(
        body,
        &mut connection,
        snapshots,
    );
    if let Some(client_id) = connection.caller_client_id().cloned() {
        *active_client_id = client_id;
    }
    Ok(response)
}

/// Resolves a load-layout selector to a concrete snapshot id.
fn runtime_layout_id_for_selector(
    snapshots: &SnapshotRepository,
    selector: &LayoutLoadSelector,
) -> Result<String> {
    match selector {
        LayoutLoadSelector::Name(name) => runtime_latest_named_layout_id(snapshots, name),
        LayoutLoadSelector::Latest => runtime_latest_layout_id(snapshots, None),
    }
}

/// Returns the latest restorable layout id with the requested user-visible name.
fn runtime_latest_named_layout_id(snapshots: &SnapshotRepository, name: &str) -> Result<String> {
    snapshots
        .list()?
        .into_iter()
        .filter(|snapshot| snapshot.restorable && snapshot.name.as_deref() == Some(name))
        .max_by(runtime_compare_snapshot_recency)
        .map(|snapshot| snapshot.id)
        .ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                format!("no stored layout named {name} found"),
            )
        })
}

/// Returns the latest restorable snapshot id, optionally scoped to a session.
fn runtime_latest_layout_id(
    snapshots: &SnapshotRepository,
    session_id: Option<&str>,
) -> Result<String> {
    snapshots
        .list()?
        .into_iter()
        .filter(|snapshot| {
            snapshot.restorable
                && session_id.is_none_or(|session_id| snapshot.session_id == session_id)
        })
        .max_by(runtime_compare_snapshot_recency)
        .map(|snapshot| snapshot.id)
        .ok_or_else(|| {
            let scope = session_id
                .map(|session_id| format!(" for session {session_id}"))
                .unwrap_or_default();
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                format!("no restorable snapshot found{scope}"),
            )
        })
}

/// Orders snapshots by creation timestamp and id for latest-snapshot selection.
fn runtime_compare_snapshot_recency(
    left: &SnapshotState,
    right: &SnapshotState,
) -> std::cmp::Ordering {
    left.created_at
        .cmp(&right.created_at)
        .then_with(|| left.id.cmp(&right.id))
}

/// Executes terminal commands whose layout effects must share the runtime pane
/// creation and resize paths used by key bindings, control requests, and MAAP.
pub(super) fn execute_runtime_layout_terminal_command(
    service: &mut RuntimeSessionService,
    primary_client_id: &crate::ids::ClientId,
    invocation: &CommandInvocation,
) -> Result<Option<CommandOutcome>> {
    match invocation.name.as_str() {
        "new-window" | "neww" => {
            let name = new_window_name(invocation);
            let shell_command = new_window_shell_command(invocation)?;
            let start_directory_path = invocation
                .start_directory_arg()
                .map(runtime_expand_user_path);
            let start_directory_arg = start_directory_path
                .as_ref()
                .map(|path| path.display().to_string());
            let start_directory = start_directory_path.as_deref();
            let select = !invocation.has_flag("-d", "--detached");
            service.create_window_with_pane_process_with_options(
                primary_client_id,
                name,
                select,
                shell_command.as_deref(),
                start_directory,
                None,
            )?;
            Ok(Some(runtime_mutated_pane_command_outcome(
                invocation,
                shell_command,
                start_directory_arg,
            )))
        }
        "new-group" | "newg" => {
            let name = new_window_name(invocation);
            let shell_command = new_window_shell_command(invocation)?;
            let start_directory_path = invocation
                .start_directory_arg()
                .map(runtime_expand_user_path);
            let start_directory_arg = start_directory_path
                .as_ref()
                .map(|path| path.display().to_string());
            let start_directory = start_directory_path.as_deref();
            let select = !invocation.has_flag("-d", "--detached");
            service.create_group_with_pane_process(
                primary_client_id,
                name,
                select,
                shell_command.as_deref(),
                start_directory,
            )?;
            Ok(Some(runtime_mutated_pane_command_outcome(
                invocation,
                shell_command,
                start_directory_arg,
            )))
        }
        "split-window" | "splitw" => {
            let direction = if invocation.has_flag("-h", "--horizontal") {
                SplitDirection::Horizontal
            } else {
                SplitDirection::Vertical
            };
            let shell_command = split_window_shell_command(invocation)?;
            let start_directory_path = invocation
                .start_directory_arg()
                .map(runtime_expand_user_path);
            let start_directory_arg = start_directory_path
                .as_ref()
                .map(|path| path.display().to_string());
            let start_directory = start_directory_path.as_deref();
            let select_new = split_window_selects_new_pane(invocation)?;
            service.split_pane_with_process_with_options(
                primary_client_id,
                direction,
                select_new,
                shell_command.as_deref(),
                start_directory,
                None,
            )?;
            Ok(Some(runtime_mutated_pane_command_outcome(
                invocation,
                shell_command,
                start_directory_arg,
            )))
        }
        "resize-pane" | "resizep" if !invocation.has_flag("-Z", "--zoom") => {
            let spec = resize_spec_from_invocation(invocation)?;
            service.resize_pane_pty_with_spec(primary_client_id, invocation.target_arg(), spec)?;
            Ok(Some(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            }))
        }
        _ => Ok(None),
    }
}

/// Closes a live pane and terminates the runtime pane process it owns.
pub(super) fn runtime_kill_pane_command(
    service: &mut RuntimeSessionService,
    primary_client_id: &crate::ids::ClientId,
    invocation: &CommandInvocation,
) -> Result<CommandOutcome> {
    let force = invocation.has_flag("-f", "--force");
    let target = invocation.target_arg();
    let descriptor = service.active_window_pane_descriptor(target)?;
    let pane_live = service
        .session
        .windows()
        .iter()
        .flat_map(|window| window.panes())
        .find(|pane| pane.id == descriptor.pane_id)
        .map(|pane| pane.live)
        .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "pane not found"))?;
    if force || !pane_live {
        service
            .fail_agent_turns_for_pane_shutdown(&[descriptor.pane_id.to_string()], "pane closed")?;
    }
    let removed = service
        .session
        .kill_pane(primary_client_id, target, force)?;
    let terminated = if let Some(pane) = removed {
        let pane_id = pane.id.to_string();
        service.pane_closing.insert(pane_id.clone());
        let _ = service.stop_active_pane_pipe(pane.id.as_str());
        let terminated = usize::from(service.terminate_runtime_pane_process(&pane_id, force)?);
        service.cleanup_removed_pane_runtime_state(&pane_id);
        terminated
    } else {
        0
    };
    let synced = if service.session.windows().is_empty() {
        0
    } else {
        service.sync_tracked_pty_sizes()?.len()
    };
    service.lifecycle_state = RuntimeLifecycleState::from_session_state(service.session.state);
    service.append_pane_close_event(
        descriptor.pane_id.as_str(),
        descriptor.window_id.as_str(),
        terminated,
        service.session.windows().is_empty(),
    )?;
    Ok(CommandOutcome::Display {
        command: invocation.name.clone(),
        body: format!(
            "closed=true:terminated_panes={}:session_empty={}:synced_panes={synced}",
            terminated,
            service.session.windows().is_empty()
        ),
    })
}

/// Closes a live window and terminates all runtime pane processes it owns.
pub(super) fn runtime_kill_window_command(
    service: &mut RuntimeSessionService,
    primary_client_id: &crate::ids::ClientId,
    invocation: &CommandInvocation,
) -> Result<CommandOutcome> {
    let force = invocation.has_flag("-f", "--force");
    let target = invocation.target_arg();
    let window = runtime_command_target_window(service, target)?;
    let window_id = window.id.to_string();
    let pane_ids = window
        .panes()
        .iter()
        .map(|pane| pane.id.to_string())
        .collect::<Vec<_>>();
    let panes_have_live_process = window.panes().iter().any(|pane| pane.live);
    if force || !panes_have_live_process {
        service.fail_agent_turns_for_pane_shutdown(&pane_ids, "window closed")?;
    }
    let removed = service
        .session
        .kill_window(primary_client_id, target, force)?;
    let removed_pane_ids = removed
        .panes()
        .iter()
        .map(|pane| pane.id.to_string())
        .collect::<Vec<_>>();
    let removed_pane_id_refs = removed_pane_ids
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    service.stop_active_pane_pipes_for(removed_pane_id_refs.as_slice());
    let terminated = service.terminate_runtime_pane_processes(removed_pane_id_refs, force)?;
    for pane_id in &removed_pane_ids {
        service.cleanup_removed_pane_runtime_state(pane_id);
    }
    let synced = if service.session.windows().is_empty() {
        0
    } else {
        service.sync_tracked_pty_sizes()?.len()
    };
    service.lifecycle_state = RuntimeLifecycleState::from_session_state(service.session.state);
    service.append_window_close_event(
        window_id.as_str(),
        terminated,
        service.session.windows().is_empty(),
    )?;
    Ok(CommandOutcome::Display {
        command: invocation.name.clone(),
        body: format!(
            "closed=true:terminated_panes={}:session_empty={}:synced_panes={synced}",
            terminated,
            service.session.windows().is_empty()
        ),
    })
}

/// Closes a live window group and terminates all runtime pane processes it owns.
pub(super) fn runtime_kill_group_command(
    service: &mut RuntimeSessionService,
    primary_client_id: &crate::ids::ClientId,
    invocation: &CommandInvocation,
) -> Result<CommandOutcome> {
    let force = invocation.has_flag("-f", "--force");
    let target = invocation.target_arg();
    let group = runtime_command_target_group(service, target)?;
    let group_id = group.id.to_string();
    let window_ids = group.window_ids.clone();
    let pane_ids = service
        .session
        .windows()
        .iter()
        .filter(|window| window_ids.iter().any(|id| id == &window.id))
        .flat_map(|window| window.panes().iter().map(|pane| pane.id.to_string()))
        .collect::<Vec<_>>();
    let panes_have_live_process = service
        .session
        .windows()
        .iter()
        .filter(|window| window_ids.iter().any(|id| id == &window.id))
        .flat_map(|window| window.panes())
        .any(|pane| pane.live);
    if force || !panes_have_live_process {
        service.fail_agent_turns_for_pane_shutdown(&pane_ids, "window group closed")?;
    }
    let removed = service
        .session
        .kill_group(primary_client_id, target, force)?;
    let removed_pane_ids = removed
        .iter()
        .flat_map(|window| window.panes().iter().map(|pane| pane.id.to_string()))
        .collect::<Vec<_>>();
    let removed_pane_id_refs = removed_pane_ids
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    service.stop_active_pane_pipes_for(removed_pane_id_refs.as_slice());
    let terminated = service.terminate_runtime_pane_processes(removed_pane_id_refs, force)?;
    for pane_id in &removed_pane_ids {
        service.cleanup_removed_pane_runtime_state(pane_id);
    }
    let synced = if service.session.windows().is_empty() {
        0
    } else {
        service.sync_tracked_pty_sizes()?.len()
    };
    service.lifecycle_state = RuntimeLifecycleState::from_session_state(service.session.state);
    service.append_lifecycle_event(
        super::super::EventKind::WindowChanged,
        format!(
            r#"{{"group_id":"{}","state":"closed","terminated_panes":{}}}"#,
            json_escape(&group_id),
            terminated
        ),
    )?;
    Ok(CommandOutcome::Display {
        command: invocation.name.clone(),
        body: format!(
            "closed=true:group={}:windows={}:terminated_panes={}:session_empty={}:synced_panes={synced}",
            group_id,
            removed.len(),
            terminated,
            service.session.windows().is_empty()
        ),
    })
}

/// Resolves a runtime window group target for command diagnostics.
fn runtime_command_target_group<'a>(
    service: &'a RuntimeSessionService,
    target: Option<&str>,
) -> Result<&'a crate::session::WindowGroup> {
    match target {
        None => service
            .session
            .active_group()
            .ok_or_else(|| MezError::invalid_state("session has no active window group")),
        Some(target) => service
            .session
            .window_groups()
            .iter()
            .find(|group| {
                group.id.as_str() == target
                    || group.index.to_string() == target
                    || group.name == target
            })
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "window group not found",
                )
            }),
    }
}

/// Resolves a runtime window target for command diagnostics.
fn runtime_command_target_window<'a>(
    service: &'a RuntimeSessionService,
    target: Option<&str>,
) -> Result<&'a crate::layout::Window> {
    match target {
        None => service
            .session
            .active_window()
            .ok_or_else(|| MezError::invalid_state("session has no active window")),
        Some(target) => service
            .session
            .windows()
            .iter()
            .find(|window| {
                window.id.as_str() == target
                    || window.index.to_string() == target
                    || window.name == target
            })
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "window not found")),
    }
}

/// Builds the command-language outcome for pane creation commands already
/// executed through the runtime creation helpers.
fn runtime_mutated_pane_command_outcome(
    invocation: &CommandInvocation,
    shell_command: Option<String>,
    start_directory: Option<String>,
) -> CommandOutcome {
    match shell_command {
        Some(shell_command) => CommandOutcome::MutatedWithPaneCommand {
            command: invocation.name.clone(),
            shell_command,
            start_directory,
        },
        None => CommandOutcome::Mutated {
            command: invocation.name.clone(),
        },
    }
}

/// Returns whether a command outcome should be followed by tracked PTY-size synchronization.
pub(super) fn runtime_command_requires_pty_sync(invocation: &CommandInvocation) -> bool {
    matches!(
        invocation.name.as_str(),
        "resize-pane"
            | "resizep"
            | "rotate-pane"
            | "rotatep"
            | "zoom-pane"
            | "select-layout"
            | "next-layout"
            | "rebalance-window"
            | "swap-pane"
            | "swapp"
            | "break-pane"
            | "breakp"
            | "join-pane"
            | "joinp"
            | "move-pane"
            | "movep"
    )
}
