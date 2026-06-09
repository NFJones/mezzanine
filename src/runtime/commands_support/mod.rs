//! Runtime Commands Support implementation.
//!
//! This module owns the runtime commands support boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.
mod agent_export;
mod buffers;
mod config;
mod displays;
mod keybindings;
mod layout;
mod mcp;
mod permissions;

use super::{
    AuditActor, AuditRecord, CommandInvocation, CommandOutcome, ConfigMutation,
    ConfigMutationOperation, ConfigMutationValue, CopyMode, EventAudience, EventKind,
    HookExecutionStatus, KeyChord, KeyCode, MezError, ObserverDecisionState, PaneReadinessState,
    PasteBuffer, PathBuf, Result, RuntimeLifecycleState, RuntimeSessionService, SearchDirection,
    Session, TerminalScreen, agent_shell_visibility_json_name, bind_key_args, binding_config_key,
    compose_effective_config, current_unix_seconds, event_type_name, execute_auth_command,
    execute_command, execute_mark_pane_ready_command, fs, json_escape, key_chord_input_bytes,
    key_chord_notation, parse_command_sequence, runtime_config_apply_event_payload,
    runtime_hook_event_name, runtime_hook_execution_status_name, runtime_pane_readiness_state_name,
};
use crate::agent::{ModelTokenUsage, ModelTokenUsageKey};
use crate::terminal::wrap_agent_log_lines;
use std::collections::BTreeMap;

pub(super) use agent_export::*;
pub(super) use buffers::*;
pub(super) use config::*;
use displays::*;
pub(super) use keybindings::*;
use layout::*;
pub(super) use mcp::*;
pub(super) use permissions::*;

// Runtime command display and command helper functions.

/// Runs the execute runtime command sequence operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn execute_runtime_command_sequence(
    service: &mut RuntimeSessionService,
    primary_client_id: &crate::ids::ClientId,
    input: &str,
) -> Result<Vec<CommandOutcome>> {
    let invocations = parse_command_sequence(input)?;
    let mut outcomes = Vec::with_capacity(invocations.len());
    let mut active_client_id = primary_client_id.clone();
    for invocation in &invocations {
        if let Some(outcome) =
            execute_runtime_live_terminal_command(service, &active_client_id, invocation)?
        {
            outcomes.push(outcome);
            continue;
        }
        if let Some(outcome) =
            execute_runtime_layout_terminal_command(service, &active_client_id, invocation)?
        {
            outcomes.push(outcome);
            continue;
        }
        let outcome = execute_command(&mut service.session, &active_client_id, invocation)?;
        let outcome =
            resolve_runtime_layout_command_outcome(service, &mut active_client_id, outcome)?;
        if runtime_command_requires_pty_sync(invocation) {
            service.sync_tracked_pty_sizes()?;
        }
        outcomes.push(outcome);
    }
    Ok(outcomes)
}

/// Runs the execute runtime command sequence async operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) async fn execute_runtime_command_sequence_async(
    service: &mut RuntimeSessionService,
    primary_client_id: &crate::ids::ClientId,
    input: &str,
) -> Result<Vec<CommandOutcome>> {
    let invocations = parse_command_sequence(input)?;
    let mut outcomes = Vec::with_capacity(invocations.len());
    let mut active_client_id = primary_client_id.clone();
    for invocation in &invocations {
        if let Some(outcome) =
            execute_runtime_live_terminal_command_async(service, &active_client_id, invocation)
                .await?
        {
            outcomes.push(outcome);
            continue;
        }
        if let Some(outcome) =
            execute_runtime_layout_terminal_command(service, &active_client_id, invocation)?
        {
            outcomes.push(outcome);
            continue;
        }
        let outcome = execute_command(&mut service.session, &active_client_id, invocation)?;
        let outcome =
            resolve_runtime_layout_command_outcome(service, &mut active_client_id, outcome)?;
        if runtime_command_requires_pty_sync(invocation) {
            service.sync_tracked_pty_sizes()?;
        }
        outcomes.push(outcome);
    }
    Ok(outcomes)
}

/// Runs the runtime send prefix command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_send_prefix_command(
    service: &mut RuntimeSessionService,
    primary_client_id: &crate::ids::ClientId,
) -> Result<String> {
    let input = key_chord_input_bytes(service.key_bindings.escape)
        .ok_or_else(|| MezError::invalid_state("configured prefix key cannot be sent to pane"))?;
    let dispatch = service.write_input_to_pane(primary_client_id, None, &input)?;
    Ok(format!(
        "pane={}:primary_pid={}:bytes={}:sent=true",
        dispatch.pane_id, dispatch.primary_pid, dispatch.bytes_written
    ))
}

/// Runs the execute runtime live terminal command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn execute_runtime_live_terminal_command(
    service: &mut RuntimeSessionService,
    primary_client_id: &crate::ids::ClientId,
    invocation: &CommandInvocation,
) -> Result<Option<CommandOutcome>> {
    match invocation.name.as_str() {
        "send-prefix" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_send_prefix_command(service, primary_client_id)?,
        })),
        "list-buffers" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_list_buffers_display(service.paste_buffers.list()),
        })),
        "choose-buffer" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_choose_buffer_command(service, invocation)?,
        })),
        "copy-mode" => {
            runtime_copy_mode_command(service, invocation)?;
            Ok(Some(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            }))
        }
        "copy-selection" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_copy_selection_command(service, invocation)?,
        })),
        "paste-clipboard" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_paste_clipboard_command(service, invocation)?,
        })),
        "display-panes" | "displayp" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_display_panes_display(service)?,
        })),
        "list-observers" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_list_observers_display(service),
        })),
        "choose-observer" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_choose_observer_display(service),
        })),
        "choose-client" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_choose_client_display(service),
        })),
        "choose-window" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_choose_window_display(service),
        })),
        "list-groups" | "listg" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_list_groups_display(service),
        })),
        "choose-group" | "chooseg" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_choose_group_display(service),
        })),
        "list-clients" | "listc" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_list_clients_display(service),
        })),
        "list-panes" | "listp" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_list_panes_display(service)?,
        })),
        "show-messages" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_show_messages_display(service),
        })),
        "show-metrics" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_show_metrics_display(service),
        })),
        "list-keys" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_list_key_bindings_display(service)?,
        })),
        "bind-key" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_bind_key_command(service, invocation)?,
        })),
        "unbind-key" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_unbind_key_command(service, invocation)?,
        })),
        "mark-pane-ready" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_mark_pane_ready_command(service, primary_client_id, invocation)?,
        })),
        "show-options" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_show_options_command(service, invocation)?,
        })),
        "list-themes" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_list_themes_command(service)?,
        })),
        "set-theme" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_set_theme_command(service, invocation)?,
        })),
        "set-option" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_set_option_command(service, invocation)?,
        })),
        "source-file" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_source_file_command(service, invocation)?,
        })),
        "refresh-client" | "refresh" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_refresh_client_command(service)?,
        })),
        "refresh-provider-info" => Ok(None),
        "kill-pane" | "killp" => Ok(Some(runtime_kill_pane_command(
            service,
            primary_client_id,
            invocation,
        )?)),
        "kill-window" | "killw" => Ok(Some(runtime_kill_window_command(
            service,
            primary_client_id,
            invocation,
        )?)),
        "kill-group" | "killg" => Ok(Some(runtime_kill_group_command(
            service,
            primary_client_id,
            invocation,
        )?)),
        "approve-observer" => {
            let observer_id = invocation
                .target_arg()
                .or_else(|| runtime_positional_args(invocation).first().copied())
                .ok_or_else(|| MezError::invalid_args("approve-observer requires a target"))?
                .to_string();
            service.approve_observer_with_runtime_cutoff(primary_client_id, &observer_id)?;
            service.append_lifecycle_event(
                EventKind::ObserverDecided,
                format!(
                    r#"{{"observer_request_id":"{}","decision":"approved"}}"#,
                    json_escape(&observer_id)
                ),
            )?;
            runtime_append_observer_decision_audit(
                service,
                primary_client_id,
                "observer_request",
                &observer_id,
                "approved",
            )?;
            Ok(Some(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            }))
        }
        "reject-observer" => {
            let observer_id = invocation
                .target_arg()
                .or_else(|| runtime_positional_args(invocation).first().copied())
                .ok_or_else(|| MezError::invalid_args("reject-observer requires a target"))?
                .to_string();
            service
                .session
                .reject_observer_target(primary_client_id, &observer_id)?;
            service.append_lifecycle_event(
                EventKind::ObserverDecided,
                format!(
                    r#"{{"observer_request_id":"{}","decision":"rejected"}}"#,
                    json_escape(&observer_id)
                ),
            )?;
            runtime_append_observer_decision_audit(
                service,
                primary_client_id,
                "observer_request",
                &observer_id,
                "rejected",
            )?;
            Ok(Some(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            }))
        }
        "revoke-observer" => {
            let client_id = invocation
                .target_arg()
                .or_else(|| runtime_positional_args(invocation).first().copied())
                .ok_or_else(|| MezError::invalid_args("revoke-observer requires a client id"))?
                .to_string();
            service
                .session
                .revoke_observer_client(primary_client_id, &client_id)?;
            service.append_lifecycle_event(
                EventKind::ObserverDecided,
                format!(
                    r#"{{"client_id":"{}","decision":"revoked"}}"#,
                    json_escape(&client_id)
                ),
            )?;
            runtime_append_observer_decision_audit(
                service,
                primary_client_id,
                "client",
                &client_id,
                "revoked",
            )?;
            Ok(Some(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            }))
        }
        "agent-shell" => {
            let (pane_id, conversation_id, visibility) = service.toggle_active_agent_shell()?;
            Ok(Some(CommandOutcome::Display {
                command: invocation.name.clone(),
                body: format!(
                    "pane={pane_id}:conversation_id={conversation_id}:visibility={}",
                    agent_shell_visibility_json_name(visibility)
                ),
            }))
        }
        "auth-status" | "mcp-status" => {
            let outcome = {
                let Some(auth_store) = service.auth_store() else {
                    return Ok(None);
                };
                execute_auth_command(auth_store, invocation)?
            };
            runtime_append_auth_command_audit(service, invocation, &outcome)?;
            Ok(Some(outcome))
        }
        "pipe-pane" => {
            let body = runtime_pipe_pane_command(service, invocation)?;
            Ok(Some(CommandOutcome::Display {
                command: invocation.name.clone(),
                body,
            }))
        }
        "create-buffer" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_create_buffer_command(service, invocation)?,
        })),
        "delete-buffer" => {
            let requested = runtime_buffer_name(invocation);
            let deleted = if let Some(name) = requested {
                if service.paste_buffers.delete(name) {
                    Some(name.to_string())
                } else {
                    None
                }
            } else {
                service.paste_buffers.delete_most_recent()
            };
            let Some(name) = deleted else {
                return Err(MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "paste buffer not found",
                ));
            };
            if service.active_paste_buffer.as_deref() == Some(name.as_str()) {
                service.active_paste_buffer = None;
            }
            Ok(Some(CommandOutcome::Mutated {
                command: format!("{} {name}", invocation.name),
            }))
        }
        "paste-buffer" => {
            let descriptor = service.active_window_pane_descriptor(invocation.target_arg())?;
            let requested = runtime_buffer_name(invocation)
                .map(str::to_string)
                .or_else(|| {
                    service.active_paste_buffer.clone().or_else(|| {
                        service
                            .paste_buffers
                            .most_recent_name()
                            .map(ToOwned::to_owned)
                    })
                });
            let Some(buffer_name) = requested else {
                return Err(MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "paste buffer not found",
                ));
            };
            let Some(content) = service
                .paste_buffers
                .get(&buffer_name)
                .map(ToOwned::to_owned)
            else {
                return Err(MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "paste buffer not found",
                ));
            };
            let paste_bytes = runtime_paste_bytes(
                service.pane_screens.get(descriptor.pane_id.as_str()),
                content.as_str(),
            );
            let primary = service
                .session
                .primary_client_id()
                .cloned()
                .ok_or_else(|| {
                    MezError::invalid_state("paste-buffer requires an attached primary client")
                })?;
            match service.write_input_to_pane(
                &primary,
                Some(descriptor.pane_id.as_str()),
                &paste_bytes,
            ) {
                Ok(dispatch) => Ok(Some(CommandOutcome::Display {
                    command: invocation.name.clone(),
                    body: format!(
                        "buffer={buffer_name}:paste=sent:target={}:bytes={}",
                        dispatch.pane_id, dispatch.bytes_written
                    ),
                })),
                Err(err) if err.kind() == crate::error::MezErrorKind::NotFound => {
                    Ok(Some(CommandOutcome::Display {
                        command: invocation.name.clone(),
                        body: format!(
                            "buffer={buffer_name}:paste=not-sent:target={}:reason=pane-process-unavailable",
                            descriptor.pane_id
                        ),
                    }))
                }
                Err(err) => Err(err),
            }
        }
        "capture-pane" => {
            let descriptor = service.active_window_pane_descriptor(invocation.target_arg())?;
            let Some(screen) = service.pane_screens.get(descriptor.pane_id.as_str()) else {
                return Ok(Some(CommandOutcome::Display {
                    command: invocation.name.clone(),
                    body: format!(
                        "target={}:capture=not-read:reason=terminal-screen-unavailable",
                        descriptor.pane_id
                    ),
                }));
            };
            let mut lines = runtime_capture_lines(screen, invocation);
            if invocation.has_flag("-J", "--join") {
                let joined = lines.join("");
                lines = vec![joined];
            }
            let content = lines.join("\n");
            if invocation.has_flag("-p", "--print") {
                return Ok(Some(CommandOutcome::Display {
                    command: invocation.name.clone(),
                    body: format!(
                        "target={}:capture=read:lines={}:content={}",
                        descriptor.pane_id,
                        lines.len(),
                        json_escape(&content)
                    ),
                }));
            }
            let buffer_name = runtime_buffer_name(invocation).unwrap_or("capture");
            service.paste_buffers.set_with_origin(
                buffer_name,
                content.as_str(),
                Some(format!("pane:{}:capture", descriptor.pane_id)),
            )?;
            Ok(Some(CommandOutcome::Display {
                command: invocation.name.clone(),
                body: format!(
                    "target={}:capture=buffered:buffer={buffer_name}:lines={}:bytes={}",
                    descriptor.pane_id,
                    lines.len(),
                    content.len()
                ),
            }))
        }
        "save-buffer" => {
            let buffer_name = runtime_buffer_name(invocation)
                .map(str::to_string)
                .or_else(|| {
                    service
                        .paste_buffers
                        .most_recent_name()
                        .map(ToOwned::to_owned)
                })
                .unwrap_or_else(|| "most-recent".to_string());
            let Some(content) = service.paste_buffers.get(&buffer_name) else {
                return Err(MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "paste buffer not found",
                ));
            };
            let output = runtime_output_target(invocation, 1).unwrap_or("stdout");
            let body = if output == "stdout" || output == "-" {
                format!(
                    "buffer={buffer_name}:save=stdout:bytes={}:content={}",
                    content.len(),
                    json_escape(content)
                )
            } else {
                runtime_write_text_output(output, content)?;
                format!(
                    "buffer={buffer_name}:save=written:output={output}:bytes={}",
                    content.len()
                )
            };
            Ok(Some(CommandOutcome::Display {
                command: invocation.name.clone(),
                body,
            }))
        }
        "clear-history" => {
            let descriptor = service.active_window_pane_descriptor(invocation.target_arg())?;
            if !runtime_clear_history_confirmed(invocation) {
                return Ok(Some(CommandOutcome::Display {
                    command: invocation.name.clone(),
                    body: format!(
                        "target={}:cleared=false:confirmation_required=true:reason=explicit-confirmation-required",
                        descriptor.pane_id
                    ),
                }));
            }
            let Some(screen) = service.pane_screens.get_mut(descriptor.pane_id.as_str()) else {
                return Ok(Some(CommandOutcome::Display {
                    command: invocation.name.clone(),
                    body: format!(
                        "target={}:cleared=false:reason=terminal-screen-unavailable",
                        descriptor.pane_id
                    ),
                }));
            };
            let cleared = screen.history().len();
            screen.clear_history();
            Ok(Some(CommandOutcome::Display {
                command: invocation.name.clone(),
                body: format!(
                    "target={}:cleared=true:history_lines={cleared}",
                    descriptor.pane_id
                ),
            }))
        }
        "search-history" => {
            let descriptor = service.active_window_pane_descriptor(invocation.target_arg())?;
            let Some(screen) = service.pane_screens.get(descriptor.pane_id.as_str()) else {
                return Ok(Some(CommandOutcome::Display {
                    command: invocation.name.clone(),
                    body: format!(
                        "target={}:matches=0:query=none:source=terminal-screen-unavailable",
                        descriptor.pane_id
                    ),
                }));
            };
            let query = runtime_positional_args(invocation).join(" ");
            if query.trim().is_empty() {
                return Err(MezError::invalid_args("search-history requires a query"));
            }
            if invocation
                .args
                .iter()
                .any(|arg| arg == "--copy-mode" || arg == "-C")
            {
                let viewport_rows = usize::from(descriptor.size.rows).saturating_sub(1).max(1);
                let mut copy_mode = CopyMode::from_screen(screen, viewport_rows)?;
                let position = copy_mode.search(query.as_str(), SearchDirection::Forward)?;
                service
                    .active_copy_modes
                    .insert(descriptor.pane_id.to_string(), copy_mode);
                return Ok(Some(CommandOutcome::Display {
                    command: invocation.name.clone(),
                    body: format!(
                        "target={}:matches={}:query={}:copy_mode=entered",
                        descriptor.pane_id,
                        usize::from(position.is_some()),
                        json_escape(&query)
                    ),
                }));
            }
            let matches = screen
                .normal_content_lines()
                .into_iter()
                .enumerate()
                .filter_map(|(index, line)| {
                    if line.contains(query.as_str()) {
                        Some(format!(
                            "{{\"line\":{},\"content\":\"{}\"}}",
                            index,
                            json_escape(&line)
                        ))
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            Ok(Some(CommandOutcome::Display {
                command: invocation.name.clone(),
                body: format!(
                    "target={}:matches={}:query={}:results=[{}]",
                    descriptor.pane_id,
                    matches.len(),
                    json_escape(&query),
                    matches.join(",")
                ),
            }))
        }
        "export-history" => {
            let descriptor = service.active_window_pane_descriptor(invocation.target_arg())?;
            let Some(screen) = service.pane_screens.get(descriptor.pane_id.as_str()) else {
                return Ok(Some(CommandOutcome::Display {
                    command: invocation.name.clone(),
                    body: format!(
                        "target={}:export=not-written:reason=terminal-screen-unavailable",
                        descriptor.pane_id
                    ),
                }));
            };
            let content = screen.normal_content_lines().join("\n");
            let output = runtime_output_target(invocation, 0).unwrap_or("stdout");
            let body = if output == "stdout" || output == "-" {
                format!(
                    "target={}:export=stdout:lines={}:content={}",
                    descriptor.pane_id,
                    screen.normal_content_lines().len(),
                    json_escape(&content)
                )
            } else {
                runtime_write_text_output(output, &content)?;
                format!(
                    "target={}:export=written:output={output}:lines={}:bytes={}",
                    descriptor.pane_id,
                    screen.normal_content_lines().len(),
                    content.len()
                )
            };
            Ok(Some(CommandOutcome::Display {
                command: invocation.name.clone(),
                body,
            }))
        }
        _ => Ok(None),
    }
}

/// Runs the execute runtime live terminal command async operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) async fn execute_runtime_live_terminal_command_async(
    service: &mut RuntimeSessionService,
    primary_client_id: &crate::ids::ClientId,
    invocation: &CommandInvocation,
) -> Result<Option<CommandOutcome>> {
    match invocation.name.as_str() {
        "refresh-provider-info" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_refresh_provider_info_command_async(service, invocation).await?,
        })),
        _ => execute_runtime_live_terminal_command(service, primary_client_id, invocation),
    }
}

/// Runs the runtime kill pane command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_kill_pane_command(
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

/// Runs the runtime kill window command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_kill_window_command(
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
fn runtime_kill_group_command(
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
        EventKind::WindowChanged,
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

/// Runs the runtime command target window operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
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

/// Runs the runtime show messages display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_show_messages_display(service: &RuntimeSessionService) -> String {
    let terminal_width = service.session.authoritative_size.columns;
    let pending_observers = service
        .session
        .observers()
        .iter()
        .filter(|observer| observer.state == ObserverDecisionState::Pending)
        .collect::<Vec<_>>();
    let pending_approvals = service.blocked_approvals().pending();
    let hook_failures = service
        .focused_shell_hook_results()
        .iter()
        .filter(|result| {
            matches!(
                result.status,
                HookExecutionStatus::Failed | HookExecutionStatus::TimedOut
            ) || result.failure.is_some()
        })
        .collect::<Vec<_>>();
    let mut status_lines = Vec::new();
    for observer in &pending_observers {
        status_lines.push(format!(
            "pending_observer={}:client={}:state=pending",
            observer.id, observer.client_id
        ));
    }
    for approval in &pending_approvals {
        status_lines.push(format!(
            "pending_approval={}:agent={}:pane={}:action={}",
            json_escape(&approval.id),
            json_escape(&approval.requesting_agent_id),
            json_escape(&approval.pane_id),
            json_escape(&approval.action_summary)
        ));
    }
    for result in &hook_failures {
        status_lines.push(format!(
            "hook_failure={}:event={}:status={}:exit_code={}",
            json_escape(&result.hook_id),
            runtime_hook_event_name(result.event),
            runtime_hook_execution_status_name(result.status),
            result
                .exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "none".to_string())
        ));
    }
    let summary = format!(
        "pending_observers={} pending_approvals={} hook_failures={}",
        pending_observers.len(),
        pending_approvals.len(),
        hook_failures.len()
    );
    let Some(event_log) = service.event_log() else {
        return runtime_show_messages_body(
            0,
            "source=runtime-event-log status=unavailable",
            &summary,
            terminal_width,
            status_lines,
        );
    };
    let events = event_log.replay_for(&EventAudience::Primary);
    if events.is_empty() {
        return runtime_show_messages_body(
            0,
            "source=runtime-event-log status=empty",
            &summary,
            terminal_width,
            status_lines,
        );
    }
    let mut lines = status_lines;
    lines.extend(
        events
            .iter()
            .rev()
            .map(|event| {
                format!(
                    "event_id={}:time={}:type={}:session={}:payload={}",
                    event.id,
                    json_escape(&event.time),
                    event_type_name(event.kind),
                    event
                        .session_id
                        .as_deref()
                        .map(json_escape)
                        .unwrap_or_else(|| "none".to_string()),
                    json_escape(&event.payload)
                )
            })
            .collect::<Vec<_>>(),
    );
    runtime_show_messages_body(
        events.len(),
        "source=runtime-event-log",
        &summary,
        terminal_width,
        lines,
    )
}
/// Formats one runtime histogram summary and bucket listing for pager output.
fn runtime_metrics_histogram_lines(
    name: &str,
    histogram: &crate::async_runtime::RuntimeHistogram,
) -> Vec<String> {
    let average = if histogram.observations == 0 {
        0.0
    } else {
        histogram.sum as f64 / histogram.observations as f64
    };
    let mut lines = vec![format!(
        "{name}: observations={} min={} max={} average={average:.2}",
        histogram.observations,
        histogram
            .min
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_string()),
        histogram
            .max
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_string()),
    )];
    lines.extend(histogram.buckets.iter().map(|bucket| {
        let upper_bound = if bucket.upper_bound == u64::MAX {
            "+inf".to_string()
        } else {
            bucket.upper_bound.to_string()
        };
        format!("  <= {upper_bound}: {}", bucket.count)
    }));
    lines
}

/// Formats provider token usage for the runtime metrics command.
fn runtime_provider_token_usage_metrics(usage: ModelTokenUsage) -> String {
    format!(
        "input={} cached_input={} output={} reasoning={} cache_hit={} total={}",
        usage.billed_input_tokens(),
        usage.cached_input_tokens_display(),
        usage.output_tokens,
        usage.reasoning_tokens,
        usage.cached_input_hit_ratio_display(),
        usage.total_tokens()
    )
}

/// Builds stable per-model provider token metrics lines.
fn runtime_provider_token_usage_by_model_lines(
    usage_by_model: &BTreeMap<ModelTokenUsageKey, ModelTokenUsage>,
) -> Vec<String> {
    let mut lines = Vec::new();
    if usage_by_model.is_empty() {
        lines.push("provider_model_tokens = none".to_string());
        return lines;
    }
    for (key, usage) in usage_by_model {
        lines.push(format!(
            "provider_model_tokens[{}] = provider={} model={} {}",
            key.display_name(),
            key.provider,
            key.model,
            runtime_provider_token_usage_metrics(*usage)
        ));
    }
    lines
}

/// Runs the runtime show metrics display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_show_metrics_display(service: &RuntimeSessionService) -> String {
    let runtime_metrics = service.runtime_metrics();
    let mut lines = vec![
        "metrics source=runtime-service status=available".to_string(),
        "".to_string(),
        "[runtime counts]".to_string(),
        format!(
            "agent_turns_started = {}",
            runtime_metrics.agent_turns_started
        ),
        format!(
            "agent_turns_completed = {}",
            runtime_metrics.agent_turns_completed
        ),
        format!(
            "agent_turns_failed = {}",
            runtime_metrics.agent_turns_failed
        ),
        format!(
            "agent_turns_interrupted = {}",
            runtime_metrics.agent_turns_interrupted
        ),
        format!(
            "agent_turns_blocked = {}",
            runtime_metrics.agent_turns_blocked
        ),
        format!(
            "provider_requests_started = {}",
            runtime_metrics.provider_requests_started
        ),
        format!(
            "provider_request_capability_decision = {}",
            runtime_metrics.provider_request_capability_decision
        ),
        format!(
            "provider_request_action_execution = {}",
            runtime_metrics.provider_request_action_execution
        ),
        format!(
            "provider_request_repair = {}",
            runtime_metrics.provider_request_repair
        ),
        format!(
            "provider_request_auto_sizing = {}",
            runtime_metrics.provider_request_auto_sizing
        ),
        format!(
            "provider_responses_succeeded = {}",
            runtime_metrics.provider_responses_succeeded
        ),
        format!(
            "provider_responses_failed = {}",
            runtime_metrics.provider_responses_failed
        ),
        format!(
            "provider_prompt_cache_diagnostics_available = {}",
            runtime_metrics.provider_prompt_cache_diagnostics_available
        ),
        format!(
            "provider_prompt_cache_diagnostics_failed = {}",
            runtime_metrics.provider_prompt_cache_diagnostics_failed
        ),
        format!(
            "provider_cached_input_reports = {}",
            runtime_metrics.provider_cached_input_reports
        ),
        format!(
            "provider_cached_input_unknown = {}",
            runtime_metrics.provider_cached_input_unknown
        ),
        format!(
            "provider_cached_input_zero_hits = {}",
            runtime_metrics.provider_cached_input_zero_hits
        ),
        format!(
            "provider_input_tokens = {}",
            runtime_metrics.provider_input_tokens
        ),
        format!(
            "provider_output_tokens = {}",
            runtime_metrics.provider_output_tokens
        ),
        format!(
            "provider_reasoning_tokens = {}",
            runtime_metrics.provider_reasoning_tokens
        ),
        format!(
            "provider_cached_input_tokens = {}",
            runtime_metrics.provider_cached_input_tokens
        ),
        format!(
            "provider_billed_input_tokens = {}",
            runtime_metrics.provider_billed_input_tokens
        ),
        format!(
            "shell_action_batches = {}",
            runtime_metrics.shell_action_batches
        ),
        format!(
            "shell_actions_dispatched = {}",
            runtime_metrics.shell_actions_dispatched
        ),
        format!(
            "shell_transactions_observed = {}",
            runtime_metrics.shell_transactions_observed
        ),
        format!(
            "shell_transactions_succeeded = {}",
            runtime_metrics.shell_transactions_succeeded
        ),
        format!(
            "shell_transactions_failed = {}",
            runtime_metrics.shell_transactions_failed
        ),
        format!(
            "shell_transaction_protocol_violations = {}",
            runtime_metrics.shell_transaction_protocol_violations
        ),
        "".to_string(),
        "[runtime latest]".to_string(),
        format!(
            "last_provider = {}",
            runtime_metrics.last_provider.as_deref().unwrap_or("none")
        ),
        format!(
            "last_model = {}",
            runtime_metrics.last_model.as_deref().unwrap_or("none")
        ),
        format!(
            "last_interaction_kind = {}",
            runtime_metrics
                .last_interaction_kind
                .as_deref()
                .unwrap_or("none")
        ),
        format!(
            "last_allowed_actions = {}",
            runtime_metrics
                .last_allowed_actions
                .as_deref()
                .unwrap_or("none")
        ),
        format!(
            "last_prompt_cache_key = {}",
            runtime_metrics
                .last_prompt_cache_key
                .as_deref()
                .unwrap_or("none")
        ),
        format!(
            "last_stable_prompt_prefix_sha256 = {}",
            runtime_metrics
                .last_stable_prompt_prefix_sha256
                .as_deref()
                .unwrap_or("none")
        ),
        format!(
            "last_provider_request_shape_sha256 = {}",
            runtime_metrics
                .last_provider_request_shape_sha256
                .as_deref()
                .unwrap_or("none")
        ),
        format!(
            "last_tool_choice_sha256 = {}",
            runtime_metrics
                .last_tool_choice_sha256
                .as_deref()
                .unwrap_or("none")
        ),
        "".to_string(),
        "[runtime histograms]".to_string(),
    ];
    for (name, histogram) in [
        (
            "provider_request_message_counts",
            &runtime_metrics.provider_request_message_counts,
        ),
        (
            "provider_request_message_bytes",
            &runtime_metrics.provider_request_message_bytes,
        ),
        (
            "provider_prompt_instructions_bytes",
            &runtime_metrics.provider_prompt_instructions_bytes,
        ),
        (
            "provider_prompt_response_format_bytes",
            &runtime_metrics.provider_prompt_response_format_bytes,
        ),
        (
            "provider_prompt_tools_bytes",
            &runtime_metrics.provider_prompt_tools_bytes,
        ),
        (
            "provider_prompt_tool_choice_bytes",
            &runtime_metrics.provider_prompt_tool_choice_bytes,
        ),
        (
            "provider_prompt_stable_input_bytes",
            &runtime_metrics.provider_prompt_stable_input_bytes,
        ),
        (
            "provider_prompt_volatile_input_bytes",
            &runtime_metrics.provider_prompt_volatile_input_bytes,
        ),
        (
            "provider_prompt_stable_prefix_bytes",
            &runtime_metrics.provider_prompt_stable_prefix_bytes,
        ),
        (
            "provider_request_shape_bytes",
            &runtime_metrics.provider_request_shape_bytes,
        ),
        (
            "provider_prompt_cacheable_prefix_bytes",
            &runtime_metrics.provider_prompt_cacheable_prefix_bytes,
        ),
        (
            "provider_input_tokens_per_response",
            &runtime_metrics.provider_input_tokens_per_response,
        ),
        (
            "provider_output_tokens_per_response",
            &runtime_metrics.provider_output_tokens_per_response,
        ),
        (
            "provider_cached_input_tokens_per_response",
            &runtime_metrics.provider_cached_input_tokens_per_response,
        ),
        (
            "provider_cached_input_hit_ratio_basis_points",
            &runtime_metrics.provider_cached_input_hit_ratio_basis_points,
        ),
        (
            "provider_response_action_counts",
            &runtime_metrics.provider_response_action_counts,
        ),
        (
            "shell_actions_dispatched_per_batch",
            &runtime_metrics.shell_actions_dispatched_per_batch,
        ),
        (
            "shell_transaction_duration_ms",
            &runtime_metrics.shell_transaction_duration_ms,
        ),
        (
            "shell_transaction_output_bytes",
            &runtime_metrics.shell_transaction_output_bytes,
        ),
    ] {
        lines.extend(runtime_metrics_histogram_lines(name, histogram));
    }
    lines.push("".to_string());
    lines.push("[runtime provider tokens by model]".to_string());
    lines.extend(runtime_provider_token_usage_by_model_lines(
        &runtime_metrics.provider_token_usage_by_model,
    ));
    lines.push("".to_string());
    let Some(metrics) = service.async_runtime_metrics() else {
        lines.push("metrics source=async-runtime status=unavailable".to_string());
        return lines.join("\n");
    };
    lines.extend([
        "metrics source=async-runtime status=available".to_string(),
        "".to_string(),
        "[async runtime counts]".to_string(),
        format!("commands_processed = {}", metrics.commands_processed),
        format!(
            "render_client_view_requests = {}",
            metrics.render_client_view_requests
        ),
        format!(
            "render_client_frame_requests = {}",
            metrics.render_client_frame_requests
        ),
        format!(
            "terminal_step_control_requests = {}",
            metrics.terminal_step_control_requests
        ),
        format!(
            "terminal_view_control_requests = {}",
            metrics.terminal_view_control_requests
        ),
        format!("runtime_event_batches = {}", metrics.runtime_event_batches),
        format!(
            "runtime_events_accepted = {}",
            metrics.runtime_events_accepted
        ),
        format!(
            "runtime_events_applied = {}",
            metrics.runtime_events_applied
        ),
        format!(
            "runtime_side_effects_queued = {}",
            metrics.runtime_side_effects_queued
        ),
        format!(
            "runtime_side_effects_drained = {}",
            metrics.runtime_side_effects_drained
        ),
        format!("pane_output_chunks = {}", metrics.pane_output_chunks),
        format!("pane_output_bytes = {}", metrics.pane_output_bytes),
        format!(
            "render_invalidations_coalesced = {}",
            metrics.render_invalidations_coalesced
        ),
        format!(
            "runtime_timer_schedules_queued = {}",
            metrics.runtime_timer_schedules_queued
        ),
        format!(
            "runtime_timer_cancellations_queued = {}",
            metrics.runtime_timer_cancellations_queued
        ),
        format!(
            "runtime_timer_events_ignored = {}",
            metrics.runtime_timer_events_ignored
        ),
        format!(
            "side_effect_queue_depth = {}",
            metrics.side_effect_queue_depth
        ),
        format!(
            "side_effect_queue_high_water = {}",
            metrics.side_effect_queue_high_water
        ),
        format!(
            "message_delivery_notifications = {}",
            metrics.message_delivery_notifications
        ),
        format!(
            "event_delivery_notifications = {}",
            metrics.event_delivery_notifications
        ),
        format!(
            "side_effect_delivery_notifications = {}",
            metrics.side_effect_delivery_notifications
        ),
        format!(
            "lifecycle_state_notifications = {}",
            metrics.lifecycle_state_notifications
        ),
        "".to_string(),
        "[async runtime histograms]".to_string(),
    ]);
    for (name, histogram) in [
        (
            "runtime_event_batch_sizes",
            &metrics.runtime_event_batch_sizes,
        ),
        (
            "runtime_side_effect_enqueue_sizes",
            &metrics.runtime_side_effect_enqueue_sizes,
        ),
        (
            "runtime_side_effect_drain_sizes",
            &metrics.runtime_side_effect_drain_sizes,
        ),
        ("pane_output_chunk_bytes", &metrics.pane_output_chunk_bytes),
        (
            "side_effect_queue_depth_samples",
            &metrics.side_effect_queue_depth_samples,
        ),
    ] {
        lines.extend(runtime_metrics_histogram_lines(name, histogram));
    }
    lines.join("\n")
}

/// Runs the runtime show messages body operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_show_messages_body(
    messages: usize,
    status: &str,
    summary: &str,
    terminal_width: u16,
    lines: Vec<String>,
) -> String {
    let header = format!("messages={messages} {status} {summary}");
    if lines.is_empty() {
        header
    } else {
        let wrapped = wrap_show_messages_lines(&lines, terminal_width);
        format!("{header}\n{}", wrapped.join("\n"))
    }
}

/// Wraps message-log detail rows to the configured terminal width.
///
/// The first physical row keeps the normal message text. Continuation rows are
/// indented by four spaces so long log entries remain readable without losing
/// their association with the preceding row.
fn wrap_show_messages_lines(lines: &[String], terminal_width: u16) -> Vec<String> {
    lines
        .iter()
        .flat_map(|line| {
            let continuation_width = terminal_width.saturating_sub(4).max(1);
            wrap_agent_log_lines(std::slice::from_ref(line), terminal_width)
                .into_iter()
                .enumerate()
                .flat_map(move |(index, wrapped)| {
                    if index == 0 {
                        vec![wrapped]
                    } else {
                        wrap_agent_log_lines(std::slice::from_ref(&wrapped), continuation_width)
                            .into_iter()
                            .map(|continued| format!("    {continued}"))
                            .collect::<Vec<_>>()
                    }
                })
        })
        .collect()
}

/// Runs the runtime mark pane ready command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_mark_pane_ready_command(
    service: &mut RuntimeSessionService,
    primary_client_id: &crate::ids::ClientId,
    invocation: &CommandInvocation,
) -> Result<String> {
    let pane_id = runtime_mark_pane_ready_target_pane_id(&service.session, invocation)?;
    let current_state = service.pane_readiness_state(&pane_id);
    let current_epoch = current_unix_seconds().max(1);
    let outcome = execute_mark_pane_ready_command(
        &service.session,
        primary_client_id,
        &mut service.pane_readiness_overrides,
        invocation,
        current_state,
        current_epoch,
        service.audit_log.as_mut(),
    )?;
    let CommandOutcome::Display { body, .. } = outcome else {
        return Err(MezError::invalid_state(
            "mark-pane-ready did not return a display outcome",
        ));
    };
    if body.contains("override=applied") {
        service.set_pane_readiness(&pane_id, PaneReadinessState::Ready);
        let queued_provider_continuations =
            service.queue_ready_provider_continuation_for_pane(&pane_id);
        service.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","readiness_override":"applied","previous_state":"{}","state":"ready","epoch":{current_epoch},"agent_provider_tasks_queued":{}}}"#,
                json_escape(&pane_id),
                runtime_pane_readiness_state_name(current_state),
                queued_provider_continuations
            ),
        )?;
    }
    Ok(body)
}

/// Runs the runtime mark pane ready target pane id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_mark_pane_ready_target_pane_id(
    session: &Session,
    invocation: &CommandInvocation,
) -> Result<String> {
    let target = invocation
        .target_arg()
        .or_else(|| runtime_mark_pane_ready_positional_target(invocation));
    match target {
        None => session
            .active_window()
            .map(|window| window.active_pane().id.to_string())
            .ok_or_else(|| MezError::invalid_state("session has no active window")),
        Some(target) => {
            if let Some(window) = session.active_window()
                && let Some(pane) = window
                    .panes()
                    .iter()
                    .find(|pane| pane.id.as_str() == target || pane.index.to_string() == target)
            {
                return Ok(pane.id.to_string());
            }

            session
                .windows()
                .iter()
                .flat_map(|window| window.panes())
                .find(|pane| pane.id.as_str() == target)
                .map(|pane| pane.id.to_string())
                .ok_or_else(|| {
                    MezError::new(crate::error::MezErrorKind::NotFound, "pane not found")
                })
        }
    }
}

/// Runs the runtime mark pane ready positional target operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_mark_pane_ready_positional_target(
    invocation: &CommandInvocation,
) -> Option<&str> {
    let mut index = 0;
    while index < invocation.args.len() {
        let arg = invocation.args[index].as_str();
        if matches!(arg, "-t" | "-s" | "-c" | "--reason" | "--epoch") {
            index = index.saturating_add(2);
            continue;
        }
        if arg.starts_with('-') {
            index += 1;
            continue;
        }
        return Some(arg);
    }
    None
}

/// Defines the TERMINAL COMMAND LIVE OVERRIDE LAYER const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const TERMINAL_COMMAND_LIVE_OVERRIDE_LAYER: &str = "terminal-command-live-override";

/// Runs the runtime append observer decision audit operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_append_observer_decision_audit(
    service: &mut RuntimeSessionService,
    primary_client_id: &crate::ids::ClientId,
    target_kind: &str,
    target_id: &str,
    decision: &str,
) -> Result<()> {
    let Some(audit_log) = service.audit_log.as_mut() else {
        return Ok(());
    };
    let record = AuditRecord::observer_decision(
        service.session.id.to_string(),
        AuditActor {
            kind: "client".to_string(),
            id: primary_client_id.to_string(),
        },
        target_kind.to_string(),
        target_id.to_string(),
        decision.to_string(),
        "succeeded",
    );
    let _ = audit_log.append(record)?;
    Ok(())
}

/// Runs the runtime append auth command audit operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_append_auth_command_audit(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
    outcome: &CommandOutcome,
) -> Result<()> {
    let Some(audit_log) = service.audit_log.as_mut() else {
        return Ok(());
    };
    let CommandOutcome::Display { body, .. } = outcome else {
        return Ok(());
    };
    let _ = (audit_log, body, invocation);
    Ok(())
}

/// Runs the runtime append auth logout audit operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_append_auth_logout_audit(
    service: &mut RuntimeSessionService,
    changed: bool,
) -> Result<()> {
    let Some(audit_log) = service.audit_log.as_mut() else {
        return Ok(());
    };
    let actor = AuditActor {
        kind: "client".to_string(),
        id: "agent-shell".to_string(),
    };
    let record = AuditRecord::logout(
        service.session.id.to_string(),
        actor,
        "openai",
        "default",
        if changed { "succeeded" } else { "unchanged" },
    );
    let _ = audit_log.append(record)?;
    Ok(())
}

/// Runs the runtime pipe pane command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_pipe_pane_command(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    if invocation
        .args
        .iter()
        .any(|arg| arg == "--list" || arg == "-l")
    {
        return Ok(service.active_pane_pipe_display());
    }

    let descriptor = service.active_window_pane_descriptor(invocation.target_arg())?;
    if invocation
        .args
        .iter()
        .any(|arg| arg == "--stop" || arg == "-S")
    {
        let stopped = service.stop_active_pane_pipe(descriptor.pane_id.as_str())?;
        let failure = stopped
            .failure
            .as_ref()
            .map(|failure| format!(":failure={}", json_escape(failure)))
            .unwrap_or_default();
        return Ok(format!(
            "target={}:pipe=stopped:mode={}:target_label={}:bytes={}:active_pipes={}{}",
            stopped.pane_id,
            stopped.mode,
            stopped.target,
            stopped.bytes_written,
            service.active_pane_pipes.len(),
            failure
        ));
    }

    if let Some(output) = runtime_flag_value(&invocation.args, "-o")
        .or_else(|| runtime_flag_value(&invocation.args, "--output"))
    {
        if output.trim().is_empty() {
            return Err(MezError::invalid_args(
                "pipe-pane output path must not be empty",
            ));
        }
        return service.start_file_pane_pipe(descriptor.pane_id.to_string(), PathBuf::from(output));
    }

    let command = runtime_positional_args(invocation).join(" ");
    if command.trim().is_empty() {
        return Ok(service.active_pane_pipe_display());
    }
    service.start_command_pane_pipe(descriptor.pane_id.to_string(), command)
}

/// Runs the runtime output target operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_output_target(
    invocation: &CommandInvocation,
    positional_index: usize,
) -> Option<&str> {
    runtime_flag_value(&invocation.args, "-o")
        .or_else(|| runtime_flag_value(&invocation.args, "--output"))
        .or_else(|| {
            runtime_positional_args(invocation)
                .get(positional_index)
                .copied()
        })
}

/// Runs the runtime clear history confirmed operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_clear_history_confirmed(invocation: &CommandInvocation) -> bool {
    invocation
        .args
        .iter()
        .any(|arg| matches!(arg.as_str(), "-f" | "--force" | "--confirm" | "--yes"))
}

/// Runs the runtime flag value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|window| window.first().is_some_and(|arg| arg == flag))
        .and_then(|window| window.get(1))
        .map(String::as_str)
}

/// Runs the runtime positional args operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_positional_args(invocation: &CommandInvocation) -> Vec<&str> {
    let mut values = Vec::new();
    let mut skip_next = false;
    for arg in &invocation.args {
        if skip_next {
            skip_next = false;
            continue;
        }
        match arg.as_str() {
            "-t" | "-b" | "--buffer" | "-o" | "--output" | "-c" | "-s" | "--content" => {
                skip_next = true;
            }
            "-p" | "--print" | "-J" | "--join" | "-S" | "--history" => {}
            _ if arg.starts_with('-') => {}
            _ => values.push(arg.as_str()),
        }
    }
    values
}

/// Expands a leading `~` path component using the current user's home
/// directory.
///
/// Only bare `~` and `~/...` are expanded. Other strings, including `~user`,
/// are preserved because portable shell expansion for other users is not
/// available from the runtime command parser.
pub(super) fn runtime_expand_user_path(path: &str) -> PathBuf {
    if path == "~" {
        return std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(path));
    }
    let Some(rest) = path.strip_prefix("~/") else {
        return PathBuf::from(path);
    };
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .map(|home| home.join(rest))
        .unwrap_or_else(|| PathBuf::from(path))
}

/// Runs the runtime write text output operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_write_text_output(path: &str, content: &str) -> Result<()> {
    if path.trim().is_empty() {
        return Err(MezError::invalid_args("output path must not be empty"));
    }
    fs::write(runtime_expand_user_path(path), content)?;
    Ok(())
}
