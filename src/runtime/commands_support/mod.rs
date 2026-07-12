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
mod status;

use super::{
    AuditActor, AuditRecord, CommandInvocation, CommandOutcome, ConfigMutation,
    ConfigMutationOperation, ConfigMutationValue, CopyMode, EventAudience, EventKind,
    HookExecutionStatus, KeyChord, KeyCode, MezError, ObserverDecisionState, PaneReadinessState,
    PasteBuffer, PathBuf, Result, RuntimeSessionService, SearchDirection, Session, TerminalScreen,
    agent_shell_visibility_json_name, bind_key_args, binding_config_key, compose_effective_config,
    current_unix_seconds, event_type_name, execute_auth_command, execute_command,
    execute_mark_pane_ready_command, fs, json_escape, key_chord_input_bytes, key_chord_notation,
    parse_command_sequence, runtime_config_apply_event_payload, runtime_hook_event_name,
    runtime_hook_execution_status_name, runtime_pane_readiness_state_name,
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
pub(super) use status::*;

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
        "exit" => {
            if !invocation.args.is_empty() {
                return Err(MezError::invalid_args("exit accepts no arguments"));
            }
            service.kill_session(primary_client_id, true)?;
            Ok(Some(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            }))
        }
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
        "help" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_command_help_display(service)?,
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
        "mcp" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_mcp_command(service, invocation)?,
        })),
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
