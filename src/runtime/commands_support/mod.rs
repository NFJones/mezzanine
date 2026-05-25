//! Runtime Commands Support implementation.
//!
//! This module owns the runtime commands support boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.
mod keybindings;
mod mcp;

use super::types::RuntimeAgentPatchRecord;
use super::{
    AgentTurnRecord, AgentTurnState, AgentTurnTrigger, ApprovalPolicy, ArgumentPolicy, AuditActor,
    AuditRecord, CommandInvocation, CommandOutcome, CommandRule, CommandRuleScope, ConfigFormat,
    ConfigLayer, ConfigMutation, ConfigMutationOperation, ConfigMutationValue, ConfigPaths,
    ConfigScope, CopyMode, DEFAULT_COMMAND_SHELL_CLASSIFICATION, DeferredConfigFileWrite,
    EventAudience, EventKind, HookExecutionStatus, KeyChord, KeyCode, McpServerStatus, MezError,
    ObserverDecisionState, PaneReadinessState, PasteBuffer, PathBuf, PermissionAuthorityChange,
    PermissionPolicy, PermissionPreset, Result, RuleDecision, RuleMatch, RuntimeLifecycleState,
    RuntimeSessionService, SearchDirection, Session, TerminalScreen, UiThemeDefinition, Value,
    agent_shell_visibility_json_name, bind_key_args, binding_config_key,
    builtin_ui_theme_definition, compare_approval_policy_authority,
    compare_permission_preset_authority, compose_effective_config, current_unix_seconds,
    event_type_name, execute_auth_command, execute_command, execute_mark_pane_ready_command, fs,
    json_escape, key_chord_input_bytes, key_chord_notation, new_window_name,
    new_window_shell_command, parse_command_sequence, persist_config_text, plan_config_mutation,
    resize_spec_from_invocation, resolve_ui_theme, runtime_config_apply_event_payload,
    runtime_effective_config_value, runtime_hook_event_name, runtime_hook_execution_status_name,
    runtime_pane_readiness_state_name, split_window_selects_new_pane, split_window_shell_command,
    validate_config_text,
};
use crate::agent::{
    ContextSourceKind, ModelMessageRole, ModelRequest, append_mcp_context,
    assemble_model_request_with_retained_tail_percent,
};
use crate::layout::SplitDirection;
use crate::terminal::{BUILTIN_UI_THEME_NAMES, UI_COLOR_SLOT_NAMES};
use std::collections::BTreeMap;

pub(super) use keybindings::*;
pub(super) use mcp::*;

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
    for invocation in &invocations {
        if let Some(outcome) =
            execute_runtime_live_terminal_command(service, primary_client_id, invocation)?
        {
            outcomes.push(outcome);
            continue;
        }
        if let Some(outcome) =
            execute_runtime_layout_terminal_command(service, primary_client_id, invocation)?
        {
            outcomes.push(outcome);
            continue;
        }
        let outcome = execute_command(&mut service.session, primary_client_id, invocation)?;
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
    for invocation in &invocations {
        if let Some(outcome) =
            execute_runtime_live_terminal_command_async(service, primary_client_id, invocation)
                .await?
        {
            outcomes.push(outcome);
            continue;
        }
        if let Some(outcome) =
            execute_runtime_layout_terminal_command(service, primary_client_id, invocation)?
        {
            outcomes.push(outcome);
            continue;
        }
        let outcome = execute_command(&mut service.session, primary_client_id, invocation)?;
        if runtime_command_requires_pty_sync(invocation) {
            service.sync_tracked_pty_sizes()?;
        }
        outcomes.push(outcome);
    }
    Ok(outcomes)
}

/// Executes terminal commands whose layout effects must share the runtime pane
/// creation and resize paths used by key bindings, control requests, and MAAP.
fn execute_runtime_layout_terminal_command(
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

/// Runs the runtime command requires pty sync operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_command_requires_pty_sync(invocation: &CommandInvocation) -> bool {
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
        "refresh-provider-info" => Err(MezError::invalid_state(
            "refresh-provider-info requires the async runtime command path",
        )),
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
        "auth-login" | "auth-status" => {
            let outcome = {
                let Some(auth_store) = service.auth_store() else {
                    return Ok(None);
                };
                execute_auth_command(auth_store, invocation)?
            };
            runtime_append_auth_command_audit(service, invocation, &outcome)?;
            Ok(Some(outcome))
        }
        "mcp-add" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_mcp_add_command(service, invocation)?,
        })),
        "mcp-remove" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_mcp_remove_command(service, invocation)?,
        })),
        "mcp-retry" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_mcp_retry_command(service, invocation)?,
        })),
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

/// Captures one assembled model request dump before it is written to a target.
struct RuntimeAgentContextDump {
    /// Exact provider-facing request context dump.
    dump: String,
    /// Turn identifier used to assemble the context dump.
    turn_id: String,
    /// Number of model messages in the assembled request.
    message_count: usize,
    /// Optional source marker for synthetic context previews.
    source: Option<&'static str>,
}

/// Result of attempting to assemble a model request context dump.
enum RuntimeAgentContextDumpResult {
    /// A dump was assembled and is ready to write.
    Written(RuntimeAgentContextDump),
    /// A dump could not be assembled for the live runtime state.
    NotWritten {
        /// Running turn, when one existed.
        turn_id: Option<String>,
        /// Stable reason fragment for status reporting.
        reason: &'static str,
        /// Human-readable pane message.
        message: String,
    },
}

/// Writes the assembled model request for the current agent turn to a target.
///
/// # Parameters
/// - `service`: The runtime service that owns pane-local agent state.
/// - `pane_id`: The pane whose current or idle model context should be copied.
/// - `args`: Shared copy-target arguments: `pane`, `buffer [name]`, or
///   `clipboard`.
pub(super) fn runtime_write_agent_context_for_pane(
    service: &mut RuntimeSessionService,
    pane_id: &str,
    args: &str,
) -> Result<(String, bool)> {
    let target = runtime_agent_export_target(args, "copy-context", "agent-context")?;
    match runtime_agent_context_dump_for_pane(service, pane_id)? {
        RuntimeAgentContextDumpResult::Written(context_dump) => {
            let lines = context_dump.dump.lines().count();
            let bytes = context_dump.dump.len();
            let source = context_dump
                .source
                .map(|source| format!(":source={source}"))
                .unwrap_or_default();
            match target {
                RuntimeAgentExportTarget::Pane => {
                    service
                        .append_agent_status_text_to_terminal_buffer(pane_id, &context_dump.dump)?;
                    Ok((
                        format!(
                            "target={pane_id}:context_dump=written:destination=pane:turn={}:messages={}:lines={lines}:bytes={bytes}:format=model-request-json{source}",
                            context_dump.turn_id, context_dump.message_count
                        ),
                        false,
                    ))
                }
                RuntimeAgentExportTarget::Buffer(name) => {
                    service.paste_buffers.set_with_origin(
                        &name,
                        &context_dump.dump,
                        Some(format!("pane:{pane_id}:agent-context")),
                    )?;
                    Ok((
                        format!(
                            "target={pane_id}:context_dump=written:destination=buffer:name=\"{}\":turn={}:messages={}:lines={lines}:bytes={bytes}:format=model-request-json{source}",
                            json_escape(&name),
                            context_dump.turn_id,
                            context_dump.message_count
                        ),
                        true,
                    ))
                }
                RuntimeAgentExportTarget::Clipboard => {
                    service.copy_text_to_buffer_and_host_clipboard(
                        "clipboard",
                        context_dump.dump.clone(),
                        format!("pane:{pane_id}:agent-context"),
                    )?;
                    Ok((
                        format!(
                            "target={pane_id}:context_dump=written:destination=clipboard:turn={}:messages={}:lines={lines}:bytes={bytes}:format=model-request-json{source}",
                            context_dump.turn_id, context_dump.message_count
                        ),
                        true,
                    ))
                }
            }
        }
        RuntimeAgentContextDumpResult::NotWritten {
            turn_id,
            reason,
            message,
        } => {
            if matches!(target, RuntimeAgentExportTarget::Pane) {
                service.append_agent_error_text_to_terminal_buffer(pane_id, &message)?;
            }
            let turn = turn_id
                .map(|turn_id| format!(":turn={turn_id}"))
                .unwrap_or_default();
            Ok((
                format!("target={pane_id}:context_dump=not-written{turn}:reason={reason}"),
                false,
            ))
        }
    }
}

/// Assembles the model request context for the current or idle pane state.
fn runtime_agent_context_dump_for_pane(
    service: &mut RuntimeSessionService,
    pane_id: &str,
) -> Result<RuntimeAgentContextDumpResult> {
    let pane_id = pane_id.to_string();
    let running_turn_id = service
        .agent_shell_store
        .get(&pane_id)
        .and_then(|session| session.running_turn_id.as_deref())
        .map(ToOwned::to_owned);
    let Some(turn_id) = running_turn_id else {
        return runtime_idle_agent_context_dump_for_pane(service, &pane_id)
            .map(RuntimeAgentContextDumpResult::Written);
    };
    if !service.agent_turn_contexts.contains_key(&turn_id) {
        let message = format!("agent context dump: running turn {turn_id} has no stored context");
        return Ok(RuntimeAgentContextDumpResult::NotWritten {
            turn_id: Some(turn_id),
            reason: "context-not-found",
            message,
        });
    }
    let Some(turn) = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == turn_id)
        .cloned()
    else {
        let message = format!("agent context dump: running turn {turn_id} has no ledger record");
        return Ok(RuntimeAgentContextDumpResult::NotWritten {
            turn_id: Some(turn_id),
            reason: "turn-not-found",
            message,
        });
    };
    let Some(model_profile) = service.agent_turn_model_profiles.get(&turn_id).cloned() else {
        let message = format!("agent context dump: running turn {turn_id} has no model profile");
        return Ok(RuntimeAgentContextDumpResult::NotWritten {
            turn_id: Some(turn_id),
            reason: "model-profile-not-found",
            message,
        });
    };
    service.refresh_agent_turn_project_guidance_context(&turn)?;
    let context = service
        .agent_turn_contexts
        .get(&turn_id)
        .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
    let mcp_summary = service.mcp_registry.prompt_summary();
    let context = append_mcp_context(context.clone(), &mcp_summary)?;
    let mut request = assemble_model_request_with_retained_tail_percent(
        &model_profile,
        &turn,
        &context,
        service.agent_compaction_raw_retention_percent,
    )?;
    request.available_mcp_tools = mcp_summary.available_tools.clone();
    let dump = runtime_model_request_context_dump(&pane_id, &turn_id, &request)?;
    let message_count = request.messages.len();
    Ok(RuntimeAgentContextDumpResult::Written(
        RuntimeAgentContextDump {
            dump,
            turn_id,
            message_count,
            source: None,
        },
    ))
}

/// Assembles the model request context that would be used for the next idle prompt.
///
/// # Parameters
/// - `service`: The live runtime service that owns pane context assembly.
/// - `pane_id`: The pane whose idle model context should be previewed.
fn runtime_idle_agent_context_dump_for_pane(
    service: &mut RuntimeSessionService,
    pane_id: &str,
) -> Result<RuntimeAgentContextDump> {
    let agent_id = format!("agent-{pane_id}");
    let (model_profile_name, model_profile) =
        service.active_model_profile_for_pane(pane_id, &agent_id, None)?;
    let context = service.agent_context_for_pane_prompt(
        pane_id,
        "[idle dump placeholder: next user prompt will be inserted here]",
        100,
    )?;
    let context = service.apply_agent_shell_preference_context(pane_id, context)?;
    let mcp_summary = service.mcp_registry.prompt_summary();
    let context = append_mcp_context(context, &mcp_summary)?;
    let turn_id = format!("idle-context-preview-{pane_id}");
    let turn = AgentTurnRecord {
        turn_id: turn_id.clone(),
        agent_id,
        pane_id: pane_id.to_string(),
        trigger: AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: current_unix_seconds().max(1),
        policy_profile: "runtime".to_string(),
        model_profile: model_profile_name,
        parent_turn_id: None,
        cooperation_mode: None,
        state: AgentTurnState::Queued,
    };
    let mut request = assemble_model_request_with_retained_tail_percent(
        &model_profile,
        &turn,
        &context,
        service.agent_compaction_raw_retention_percent,
    )?;
    request.available_mcp_tools = mcp_summary.available_tools.clone();
    let dump = runtime_model_request_context_dump(pane_id, &turn_id, &request)?;
    let message_count = request.messages.len();
    Ok(RuntimeAgentContextDump {
        dump,
        turn_id,
        message_count,
        source: Some("idle-preview"),
    })
}

/// Writes the retained bounded pane trace log to the requested destination.
pub(super) fn runtime_write_agent_trace_log_for_pane(
    service: &mut RuntimeSessionService,
    pane_id: &str,
    args: &str,
) -> Result<(String, bool)> {
    let target = runtime_agent_export_target(args, "copy-trace-log", "agent-trace")?;
    let Some(trace_log) = service.agent_pane_trace_log_text(pane_id) else {
        if matches!(target, RuntimeAgentExportTarget::Pane) {
            service.append_agent_status_text_to_terminal_buffer(
                pane_id,
                &format!("agent trace log: no retained trace entries for pane {pane_id}"),
            )?;
        }
        return Ok((
            format!("target={pane_id}:trace_log=not-written:reason=empty"),
            false,
        ));
    };
    let dump = format!("agent trace log for pane {pane_id}\n{trace_log}");
    let lines = dump.lines().count();
    let bytes = dump.len();
    match target {
        RuntimeAgentExportTarget::Pane => {
            service.append_agent_status_text_to_terminal_buffer(pane_id, &dump)?;
            Ok((
                format!(
                    "target={pane_id}:trace_log=written:destination=pane:lines={lines}:bytes={bytes}"
                ),
                false,
            ))
        }
        RuntimeAgentExportTarget::Buffer(name) => {
            service.paste_buffers.set_with_origin(
                name.as_str(),
                dump,
                Some(format!("pane:{pane_id}:agent-trace-log")),
            )?;
            Ok((
                format!(
                    "target={pane_id}:trace_log=written:destination=buffer:buffer={}:lines={lines}:bytes={bytes}",
                    json_escape(&name)
                ),
                true,
            ))
        }
        RuntimeAgentExportTarget::Clipboard => {
            service.copy_text_to_buffer_and_host_clipboard(
                "clipboard",
                dump,
                format!("pane:{pane_id}:agent-trace-log"),
            )?;
            Ok((
                format!(
                    "target={pane_id}:trace_log=written:destination=clipboard:buffer=clipboard:lines={lines}:bytes={bytes}"
                ),
                true,
            ))
        }
    }
}

/// Writes the latest model-authored `say` text to the requested destination.
pub(super) fn runtime_write_agent_copy_output_for_pane(
    service: &mut RuntimeSessionService,
    pane_id: &str,
    args: &str,
) -> Result<(String, bool)> {
    let target = runtime_agent_export_target(args, "copy", "agent-output")?;
    let Some((turn_id, output, content_type)) = service.latest_agent_copy_output_for_pane(pane_id)
    else {
        if matches!(target, RuntimeAgentExportTarget::Pane) {
            service.append_agent_status_text_to_terminal_buffer(
                pane_id,
                &format!("agent copy: no retained say action text for pane {pane_id}"),
            )?;
        }
        return Ok((
            format!(
                "target={pane_id}:say=not-written:reason=no-say-action:source=runtime-agent-say"
            ),
            false,
        ));
    };
    let lines = output.lines().count();
    let bytes = output.len();
    let turn = json_escape(&turn_id);
    match target {
        RuntimeAgentExportTarget::Pane => {
            service.append_agent_assistant_content_to_terminal_buffer(
                pane_id,
                &output,
                &content_type,
            )?;
            Ok((
                format!(
                    "target={pane_id}:say=written:destination=pane:turn={turn}:lines={lines}:bytes={bytes}:source=runtime-agent-say"
                ),
                false,
            ))
        }
        RuntimeAgentExportTarget::Buffer(name) => {
            service.paste_buffers.set_with_origin(
                name.as_str(),
                output,
                Some(format!("agent:{turn_id}:say")),
            )?;
            Ok((
                format!(
                    "target={pane_id}:say=written:destination=buffer:buffer={}:turn={turn}:lines={lines}:bytes={bytes}:source=runtime-agent-say",
                    json_escape(&name)
                ),
                true,
            ))
        }
        RuntimeAgentExportTarget::Clipboard => {
            service.copy_text_to_buffer_and_host_clipboard(
                "clipboard",
                output,
                format!("agent:{turn_id}:say"),
            )?;
            Ok((
                format!(
                    "target={pane_id}:say=written:destination=clipboard:buffer=clipboard:turn={turn}:lines={lines}:bytes={bytes}:source=runtime-agent-say"
                ),
                true,
            ))
        }
    }
}

/// Writes retained `apply_patch` payloads and statuses to the target destination.
pub(super) fn runtime_write_agent_patches_for_pane(
    service: &mut RuntimeSessionService,
    pane_id: &str,
    args: &str,
) -> Result<(String, bool)> {
    let target = runtime_agent_export_target(args, "copy-patches", "agent-patches")?;
    let session_id = service
        .agent_shell_store
        .get(pane_id)
        .map(|session| session.session_id.clone())
        .ok_or_else(|| MezError::invalid_state("agent shell session missing for copy-patches"))?;
    let Some(records) = service
        .agent_session_patch_records
        .get(&session_id)
        .filter(|records| !records.is_empty())
    else {
        if matches!(target, RuntimeAgentExportTarget::Pane) {
            service.append_agent_status_text_to_terminal_buffer(
                pane_id,
                &format!("agent patches: no retained apply_patch actions for pane {pane_id}"),
            )?;
        }
        return Ok((
            format!("target={pane_id}:patches=not-written:reason=empty"),
            false,
        ));
    };
    let dump = runtime_agent_patch_dump(pane_id, &session_id, records);
    let lines = dump.lines().count();
    let bytes = dump.len();
    let patches = records.len();
    match target {
        RuntimeAgentExportTarget::Pane => {
            service.append_agent_status_text_to_terminal_buffer(pane_id, &dump)?;
            Ok((
                format!(
                    "target={pane_id}:patches=written:destination=pane:patches={patches}:lines={lines}:bytes={bytes}"
                ),
                false,
            ))
        }
        RuntimeAgentExportTarget::Buffer(name) => {
            service.paste_buffers.set_with_origin(
                name.as_str(),
                dump,
                Some(format!("pane:{pane_id}:agent-patches")),
            )?;
            Ok((
                format!(
                    "target={pane_id}:patches=written:destination=buffer:buffer={}:patches={patches}:lines={lines}:bytes={bytes}",
                    json_escape(&name)
                ),
                true,
            ))
        }
        RuntimeAgentExportTarget::Clipboard => {
            service.copy_text_to_buffer_and_host_clipboard(
                "clipboard",
                dump,
                format!("pane:{pane_id}:agent-patches"),
            )?;
            Ok((
                format!(
                    "target={pane_id}:patches=written:destination=clipboard:buffer=clipboard:patches={patches}:lines={lines}:bytes={bytes}"
                ),
                true,
            ))
        }
    }
}

/// Formats retained patch records as a plain text export.
fn runtime_agent_patch_dump(
    pane_id: &str,
    session_id: &str,
    records: &[RuntimeAgentPatchRecord],
) -> String {
    let mut lines = vec![format!(
        "agent patches for pane {pane_id} session {session_id}"
    )];
    for (index, record) in records.iter().enumerate() {
        let strip = record
            .strip
            .map(|strip| format!(" strip={strip}"))
            .unwrap_or_default();
        let error = match (&record.error_code, &record.error_message) {
            (Some(code), Some(message)) => format!(
                r#" error_code={} error_message="{}""#,
                json_escape(code),
                json_escape(&message.replace('\n', "\\n"))
            ),
            (Some(code), None) => format!(" error_code={}", json_escape(code)),
            (None, Some(message)) => format!(
                r#" error_message="{}""#,
                json_escape(&message.replace('\n', "\\n"))
            ),
            (None, None) => String::new(),
        };
        lines.push(format!(
            "patch {}: turn={} action={} status={} bytes={}{}{}",
            index.saturating_add(1),
            record.turn_id,
            record.action_id,
            record.status,
            record.patch.len(),
            strip,
            error
        ));
        lines.push(record.patch.clone());
    }
    lines.join("\n")
}

/// Destination for an agent diagnostic export.
enum RuntimeAgentExportTarget {
    /// Write the export into the pane buffer.
    Pane,
    /// Write the export into one named internal paste buffer.
    Buffer(String),
    /// Write the export into the clipboard paste buffer and host clipboard.
    Clipboard,
}

/// Parses shared agent export target arguments.
fn runtime_agent_export_target(
    args: &str,
    command: &str,
    default_buffer_name: &str,
) -> Result<RuntimeAgentExportTarget> {
    let mut parts = args.split_whitespace();
    let target = parts.next().unwrap_or("pane");
    match target {
        "pane" => {
            if parts.next().is_some() {
                return Err(MezError::invalid_args(format!(
                    "{command} pane does not accept additional arguments"
                )));
            }
            Ok(RuntimeAgentExportTarget::Pane)
        }
        "buffer" => {
            let name = parts.next().unwrap_or(default_buffer_name).to_string();
            if parts.next().is_some() {
                return Err(MezError::invalid_args(format!(
                    "{command} buffer accepts at most one buffer name"
                )));
            }
            Ok(RuntimeAgentExportTarget::Buffer(name))
        }
        "clipboard" => {
            if parts.next().is_some() {
                return Err(MezError::invalid_args(format!(
                    "{command} clipboard does not accept additional arguments"
                )));
            }
            Ok(RuntimeAgentExportTarget::Clipboard)
        }
        _ => Err(MezError::invalid_args(format!(
            "{command} expects one of: pane, buffer [name], clipboard"
        ))),
    }
}

/// Formats the exact provider-facing model request context as JSON.
fn runtime_model_request_context_dump(
    pane_id: &str,
    turn_id: &str,
    request: &ModelRequest,
) -> Result<String> {
    let payload = serde_json::json!({
        "kind": "model_request_context_dump",
        "pane_id": pane_id,
        "turn_id": turn_id,
        "provider": &request.provider,
        "model": &request.model,
        "agent_id": &request.agent_id,
        "interaction_kind": request.interaction_kind.as_str(),
        "allowed_actions": request.allowed_actions.action_type_names(),
        "available_mcp_tools": request
            .available_mcp_tools
            .iter()
            .map(|tool| serde_json::json!({
                "server_id": &tool.server_id,
                "tool_name": &tool.tool_name,
                "description": &tool.description,
                "approval_required": tool.approval_required,
                "input_schema": serde_json::from_str::<serde_json::Value>(&tool.input_schema_json)
                    .unwrap_or_else(|_| serde_json::json!(&tool.input_schema_json))
            }))
            .collect::<Vec<_>>(),
        "messages": request
            .messages
            .iter()
            .enumerate()
            .map(|(index, message)| serde_json::json!({
                "index": index,
                "role": runtime_model_message_role_name_for_dump(message.role),
                "source": runtime_context_source_name(message.source),
                "content_bytes": message.content.len(),
                "content": &message.content
            }))
            .collect::<Vec<_>>()
    });
    serde_json::to_string_pretty(&payload).map_err(|error| {
        MezError::invalid_state(format!("model request context dump JSON failed: {error}"))
    })
}

/// Returns the stable display label for a context block source.
fn runtime_context_source_name(source: ContextSourceKind) -> &'static str {
    match source {
        ContextSourceKind::System => "system",
        ContextSourceKind::UserInstruction => "user-instruction",
        ContextSourceKind::DeveloperInstruction => "developer-instruction",
        ContextSourceKind::Policy => "policy",
        ContextSourceKind::Configuration => "configuration",
        ContextSourceKind::LocalMessage => "local-message",
        ContextSourceKind::ProjectGuidance => "project-guidance",
        ContextSourceKind::Memory => "memory",
        ContextSourceKind::Transcript => "transcript",
        ContextSourceKind::TranscriptUser => "transcript-user",
        ContextSourceKind::TranscriptAssistant => "transcript-assistant",
        ContextSourceKind::TranscriptTool => "transcript-tool",
        ContextSourceKind::EvidenceLedger => "evidence-ledger",
        ContextSourceKind::ActionResult => "action-result",
    }
}

/// Returns the stable display label for a model request message role.
fn runtime_model_message_role_name_for_dump(role: ModelMessageRole) -> &'static str {
    match role {
        ModelMessageRole::System => "system",
        ModelMessageRole::Developer => "developer",
        ModelMessageRole::User => "user",
        ModelMessageRole::Assistant => "assistant",
        ModelMessageRole::Tool => "tool",
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
        "mcp-add" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_mcp_add_command_async(service, invocation).await?,
        })),
        "mcp-remove" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_mcp_remove_command_async(service, invocation).await?,
        })),
        "mcp-retry" => Ok(Some(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: runtime_mcp_retry_command_async(service, invocation).await?,
        })),
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

/// Runs the runtime capture lines operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_capture_lines(
    screen: &TerminalScreen,
    invocation: &CommandInvocation,
) -> Vec<String> {
    if invocation.has_flag("-S", "--history") {
        screen.normal_content_lines()
    } else {
        screen.visible_lines()
    }
}

/// Runs the runtime buffer name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_buffer_name(invocation: &CommandInvocation) -> Option<&str> {
    runtime_flag_value(&invocation.args, "-b")
        .or_else(|| runtime_flag_value(&invocation.args, "--buffer"))
        .or_else(|| runtime_positional_args(invocation).first().copied())
}

/// Resolves the buffer name used by copy-mode commands.
///
/// Explicit command arguments take precedence, then the interactive active
/// buffer selection, then the default clipboard buffer.
pub(super) fn runtime_copy_target_buffer_name(
    service: &RuntimeSessionService,
    invocation: &CommandInvocation,
) -> String {
    runtime_buffer_name(invocation)
        .map(ToOwned::to_owned)
        .or_else(|| service.active_paste_buffer.clone())
        .unwrap_or_else(|| "clipboard".to_string())
}

/// Runs the runtime copy mode command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_copy_mode_command(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<()> {
    let descriptor = service.active_window_pane_descriptor(invocation.target_arg())?;
    let pane_id = descriptor.pane_id.to_string();
    if invocation
        .args
        .iter()
        .any(|arg| arg == "--cancel" || arg == "-q")
    {
        service.active_copy_modes.remove(pane_id.as_str());
        return Ok(());
    }
    if !service.active_copy_modes.contains_key(pane_id.as_str()) {
        let screen = service.pane_screens.get(pane_id.as_str()).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "pane screen not found",
            )
        })?;
        let viewport_rows = service.copy_mode_viewport_rows_for_pane(pane_id.as_str());
        service.active_copy_modes.insert(
            pane_id.clone(),
            CopyMode::from_screen(screen, viewport_rows)?,
        );
    }
    let copy_target_buffer = invocation
        .args
        .iter()
        .any(|arg| arg == "--copy")
        .then(|| runtime_copy_target_buffer_name(service, invocation));
    let mut copied = None;
    {
        let copy_mode = service
            .active_copy_modes
            .get_mut(pane_id.as_str())
            .ok_or_else(|| MezError::invalid_state("copy mode was not retained"))?;
        if invocation
            .args
            .iter()
            .any(|arg| arg == "-u" || arg == "--page-up")
        {
            copy_mode.page_up();
        }
        if invocation.args.iter().any(|arg| arg == "--page-down") {
            copy_mode.page_down();
        }
        if invocation.args.iter().any(|arg| arg == "--top") {
            copy_mode.scroll_to_top();
        }
        if invocation.args.iter().any(|arg| arg == "--bottom") {
            copy_mode.scroll_to_bottom();
        }
        if let Some(name) = copy_target_buffer.as_ref() {
            copied = Some((name.to_string(), copy_mode.copy_selection()?));
        }
    }
    if let Some((name, copied)) = copied {
        service.copy_text_to_buffer_and_host_clipboard(
            name.as_str(),
            copied,
            format!("pane:{pane_id}:copy-mode"),
        )?;
    }
    Ok(())
}

/// Runs the runtime copy selection command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_copy_selection_command(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let descriptor = service.active_window_pane_descriptor(invocation.target_arg())?;
    let pane_id = descriptor.pane_id.to_string();
    let buffer_name = runtime_copy_target_buffer_name(service, invocation);
    let Some(copy_mode) = service.active_copy_modes.get(pane_id.as_str()) else {
        return Ok(format!(
            "target={pane_id}:copy=not-copied:reason=copy-mode-inactive"
        ));
    };
    let copied = copy_mode.copy_selection()?;
    let bytes = copied.len();
    service.copy_text_to_buffer_and_host_clipboard(
        buffer_name.as_str(),
        copied,
        format!("pane:{pane_id}:copy-mode"),
    )?;
    if invocation.has_flag("-x", "--exit") {
        service.active_copy_modes.remove(pane_id.as_str());
    }
    Ok(format!(
        "target={pane_id}:copy=copied:buffer={buffer_name}:bytes={bytes}"
    ))
}

/// Runs the runtime paste clipboard command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_paste_clipboard_command(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let descriptor = service.active_window_pane_descriptor(invocation.target_arg())?;
    let primary = service
        .session
        .primary_client_id()
        .cloned()
        .ok_or_else(|| {
            MezError::invalid_state("paste-clipboard requires an attached primary client")
        })?;
    match service.paste_clipboard_or_most_recent_buffer_to_pane(&primary, &descriptor) {
        Ok(true) => Ok(format!(
            "target={}:paste=sent:source=clipboard-or-buffer",
            descriptor.pane_id
        )),
        Ok(false) => Ok(format!(
            "target={}:paste=not-sent:reason=clipboard-and-buffer-empty",
            descriptor.pane_id
        )),
        Err(err) if err.kind() == crate::error::MezErrorKind::NotFound => Ok(format!(
            "target={}:paste=not-sent:reason=pane-process-unavailable",
            descriptor.pane_id
        )),
        Err(err) => Err(err),
    }
}

/// Runs the runtime choose buffer command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_choose_buffer_command(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    if let Some(buffer_name) = runtime_positional_args(invocation).first() {
        let created = if service.paste_buffers.get(buffer_name).is_none() {
            service.paste_buffers.set_with_origin(
                *buffer_name,
                "",
                Some("runtime:choose-buffer".to_string()),
            )?;
            true
        } else {
            false
        };
        service.active_paste_buffer = Some((*buffer_name).to_string());
        return Ok(format!(
            "buffer={}:selected=true:copy_target=active:paste_source=active:created={} source=runtime",
            buffer_name, created
        ));
    }
    Ok(runtime_choose_buffer_display(
        service.paste_buffers.list(),
        service.active_paste_buffer.as_deref(),
    ))
}

/// Runs the runtime create buffer command operation for this subsystem.
///
/// The command creates a named internal paste buffer without overwriting an
/// existing buffer unless `--replace` is provided. `--select` makes the buffer
/// active for later copy and paste operations.
pub(super) fn runtime_create_buffer_command(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let buffer_name = runtime_buffer_name(invocation)
        .ok_or_else(|| MezError::invalid_args("create-buffer requires a buffer name"))?;
    let content = runtime_flag_value(&invocation.args, "--content")
        .or_else(|| runtime_positional_args(invocation).get(1).copied())
        .unwrap_or("");
    let replace = invocation
        .args
        .iter()
        .any(|arg| matches!(arg.as_str(), "-r" | "--replace"));
    let select = invocation.args.iter().any(|arg| arg == "--select");

    let existed = service.paste_buffers.get(buffer_name).is_some();
    let (created, replaced, bytes) = if existed && !replace {
        (
            false,
            false,
            service
                .paste_buffers
                .get(buffer_name)
                .map(str::len)
                .unwrap_or(0),
        )
    } else {
        let created = if replace {
            service.paste_buffers.set_with_origin(
                buffer_name,
                content,
                Some("runtime:create-buffer".to_string()),
            )?;
            !existed
        } else {
            service.paste_buffers.create_with_origin(
                buffer_name,
                content,
                Some("runtime:create-buffer".to_string()),
            )?
        };
        (created, existed && replace, content.len())
    };

    if select {
        service.active_paste_buffer = Some(buffer_name.to_string());
    }

    Ok(format!(
        "buffer={buffer_name}:created={created}:replaced={replaced}:exists={}:bytes={bytes}:selected={select} source=runtime",
        existed && !created
    ))
}

/// Runs the runtime choose buffer display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_choose_buffer_display(
    buffers: Vec<PasteBuffer>,
    active: Option<&str>,
) -> String {
    if buffers.is_empty() {
        return "buffers=0 chooser=empty source=runtime".to_string();
    }
    let lines = buffers
        .iter()
        .map(|buffer| {
            let origin = buffer.origin.as_deref().unwrap_or("unknown");
            format!(
                "buffer={}:bytes={}:origin={}:preview={}:actions=paste-buffer -b {},delete-buffer {}",
                buffer.name,
                buffer.bytes,
                json_escape(origin),
                json_escape(&buffer.preview),
                buffer.name,
                buffer.name
            )
        })
        .collect::<Vec<_>>();
    format!(
        "buffers={} chooser=select-by-command active={} source=runtime\n{}",
        buffers.len(),
        active.unwrap_or("none"),
        lines.join("\n")
    )
}

/// Runs the runtime show messages display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_show_messages_display(service: &RuntimeSessionService) -> String {
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
            status_lines,
        );
    };
    let events = event_log.replay_for(&EventAudience::Primary);
    if events.is_empty() {
        return runtime_show_messages_body(
            0,
            "source=runtime-event-log status=empty",
            &summary,
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
    runtime_show_messages_body(events.len(), "source=runtime-event-log", &summary, lines)
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
    lines: Vec<String>,
) -> String {
    let header = format!("messages={messages} {status} {summary}");
    if lines.is_empty() {
        header
    } else {
        format!("{header}\n{}", lines.join("\n"))
    }
}

/// on duplicated control-flow logic.
pub(super) fn runtime_permissions_command(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let args = runtime_positional_args(invocation);
    if args.is_empty() || matches!(args.as_slice(), ["status"] | ["show"]) {
        return Ok(runtime_permission_policy_display(
            service.permission_policy(),
        ));
    }

    let body = match args.as_slice() {
        ["preset", requested] | ["set-preset", requested] => {
            let Ok(requested) = runtime_parse_permission_preset(requested) else {
                return Ok(format!(
                    "field=preset:requested={requested}:changed=false:reason=unsupported-permission-preset:source=runtime-policy"
                ));
            };
            let current = service.permission_policy().preset;
            let change = compare_permission_preset_authority(current, requested);
            runtime_apply_permission_live_override(
                service,
                None,
                "permissions.preset",
                runtime_permission_preset_name(requested),
                "terminal/command:permissions",
            )?;
            runtime_append_permission_audit(
                service,
                "permissions.preset",
                "permission_change",
                runtime_permission_preset_name(requested),
                "changed",
            )?;
            runtime_permission_change_display(
                "preset",
                runtime_permission_preset_name(current),
                runtime_permission_preset_name(requested),
                change,
                true,
            )
        }
        ["approval-policy", requested] | ["approval_policy", requested] => {
            let Ok(requested) = runtime_parse_approval_policy(requested) else {
                return Ok(format!(
                    "field=approval_policy:requested={requested}:changed=false:reason=unsupported-approval-policy:source=runtime-policy"
                ));
            };
            let current = service.permission_policy().approval_policy;
            let change = compare_approval_policy_authority(current, requested);
            runtime_apply_permission_live_override(
                service,
                None,
                "permissions.approval_policy",
                runtime_approval_policy_name(requested),
                "terminal/command:permissions",
            )?;
            runtime_append_permission_audit(
                service,
                "permissions.approval_policy",
                "permission_change",
                runtime_approval_policy_name(requested),
                "changed",
            )?;
            runtime_permission_change_display(
                "approval_policy",
                runtime_approval_policy_name(current),
                runtime_approval_policy_name(requested),
                change,
                true,
            )
        }
        [requested] => match runtime_parse_permission_preset(requested) {
            Ok(requested) => {
                let current = service.permission_policy().preset;
                let change = compare_permission_preset_authority(current, requested);
                runtime_apply_permission_live_override(
                    service,
                    None,
                    "permissions.preset",
                    runtime_permission_preset_name(requested),
                    "terminal/command:permissions",
                )?;
                runtime_append_permission_audit(
                    service,
                    "permissions.preset",
                    "permission_change",
                    runtime_permission_preset_name(requested),
                    "changed",
                )?;
                runtime_permission_change_display(
                    "preset",
                    runtime_permission_preset_name(current),
                    runtime_permission_preset_name(requested),
                    change,
                    true,
                )
            }
            Err(_) => {
                format!(
                    "requested={requested}:changed=false:reason=unsupported-permission-command:source=runtime-policy"
                )
            }
        },
        _ => {
            "changed=false:reason=unsupported-permission-command:source=runtime-policy".to_string()
        }
    };
    Ok(body)
}

/// Runs the runtime approval command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_approval_command(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let args = runtime_positional_args(invocation);
    if args.is_empty() || matches!(args.as_slice(), ["status"] | ["show"]) {
        return Ok(format!(
            "approval_policy={} source=runtime-policy",
            runtime_approval_policy_name(service.permission_policy().approval_policy)
        ));
    }
    let [requested] = args.as_slice() else {
        return Ok(
            "changed=false:reason=unsupported-approval-command:source=runtime-policy".to_string(),
        );
    };
    let Ok(requested) = runtime_parse_approval_policy(requested) else {
        return Ok(format!(
            "field=approval_policy:requested={requested}:changed=false:reason=unsupported-approval-policy:source=runtime-policy"
        ));
    };
    let current = service.permission_policy().approval_policy;
    let change = compare_approval_policy_authority(current, requested);
    runtime_apply_permission_live_override(
        service,
        None,
        "permissions.approval_policy",
        runtime_approval_policy_name(requested),
        "terminal/command:approval",
    )?;
    runtime_append_permission_audit(
        service,
        "permissions.approval_policy",
        "permission_change",
        runtime_approval_policy_name(requested),
        "changed",
    )?;
    Ok(runtime_permission_change_display(
        "approval_policy",
        runtime_approval_policy_name(current),
        runtime_approval_policy_name(requested),
        change,
        true,
    ))
}

/// Runs the runtime permission policy display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_permission_policy_display(policy: &PermissionPolicy) -> String {
    format!(
        "preset={} approval_policy={} bypass={} rules={} source=runtime-policy",
        runtime_permission_preset_name(policy.preset),
        runtime_approval_policy_name(policy.approval_policy),
        policy.approval_bypass(),
        policy.rules().len()
    )
}

/// Runs the runtime permission change display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_permission_change_display(
    field: &str,
    current: &str,
    requested: &str,
    change: PermissionAuthorityChange,
    changed: bool,
) -> String {
    let approval_required = matches!(change, PermissionAuthorityChange::Broadening);
    let approved_by = if approval_required {
        ":approved_by=primary-command"
    } else {
        ""
    };
    format!(
        "field={field}:current={current}:requested={requested}:authority_change={}:approval_required={approval_required}{approved_by}:changed={changed}:source=runtime-policy",
        runtime_permission_authority_change_name(change)
    )
}

/// Runs the runtime list command rules display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_list_command_rules_display(policy: &PermissionPolicy) -> String {
    if policy.rules().is_empty() {
        return "rules=0 source=runtime-policy".to_string();
    }
    policy
        .rules()
        .iter()
        .enumerate()
        .map(|(index, rule)| runtime_command_rule_display_line(index + 1, rule))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Runs the runtime command rule display line operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_command_rule_display_line(index: usize, rule: &CommandRule) -> String {
    format!(
        "rule{}:scope={}:decision={}:match={}:pattern={}:argument_policy={}:source=runtime-policy",
        index,
        runtime_command_rule_scope_name(rule.scope),
        runtime_rule_decision_name(rule.decision),
        runtime_rule_match_name(&rule.rule_match),
        rule.pattern.join(" "),
        runtime_argument_policy_name(&rule.argument_policy)
    )
}

/// Runs the runtime add command rule operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_add_command_rule(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let rule = runtime_command_rule_from_invocation(invocation)?;
    let decision = runtime_rule_decision_name(rule.decision);
    let prefix = rule.pattern.join(" ");
    let scope = runtime_command_rule_scope_name(rule.scope);
    let rule_match = runtime_rule_match_name(&rule.rule_match);
    let approval_required = rule.decision == RuleDecision::Allow;
    service.permission_policy_mut().add_rule(rule);
    runtime_append_permission_audit(
        service,
        "permissions.command_rules",
        "command_rule",
        decision,
        "added",
    )?;
    Ok(format!(
        "decision={decision}:scope={scope}:match={rule_match}:prefix={prefix}:approval_required={approval_required}:approved_by=primary-command:changed=true:source=runtime-policy"
    ))
}

/// Runs the runtime remove command rule operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_remove_command_rule(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let rule_id = runtime_positional_args(invocation)
        .first()
        .copied()
        .ok_or_else(|| MezError::invalid_args("remove-command-rule requires a rule id"))?;
    let removed = service.permission_policy_mut().remove_rule(rule_id)?;
    runtime_append_permission_audit(
        service,
        "permissions.command_rules",
        "command_rule",
        runtime_rule_decision_name(removed.decision),
        "removed",
    )?;
    Ok(format!(
        "rule={rule_id}:removed=true:decision={}:scope={}:source=runtime-policy",
        runtime_rule_decision_name(removed.decision),
        runtime_command_rule_scope_name(removed.scope)
    ))
}

/// Runs the runtime command rule from invocation operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_command_rule_from_invocation(
    invocation: &CommandInvocation,
) -> Result<CommandRule> {
    let decision = match invocation.name.as_str() {
        "allow-command" => RuleDecision::Allow,
        "deny-command" => RuleDecision::Forbid,
        "prompt-command" => RuleDecision::Prompt,
        _ => {
            return Err(MezError::invalid_args(format!(
                "command `{}` cannot create a command rule",
                invocation.name
            )));
        }
    };
    let scope = runtime_command_rule_scope(invocation)?;
    let rule_match = runtime_command_rule_match(invocation)?;
    let mut rule = match rule_match {
        RuleMatch::ExactSha256 { .. } => {
            let digest =
                runtime_flag_value(&invocation.args, "--exact-sha256").ok_or_else(|| {
                    MezError::invalid_args("exact_sha256 command rules require --exact-sha256")
                })?;
            CommandRule::from_exact_sha256_digest(
                digest,
                runtime_flag_value(&invocation.args, "--shell-classification")
                    .unwrap_or(DEFAULT_COMMAND_SHELL_CLASSIFICATION),
                decision,
            )?
        }
        RuleMatch::Prefix | RuleMatch::Exact => {
            let pattern = runtime_command_rule_pattern_args(invocation);
            if pattern.is_empty() {
                return Err(MezError::invalid_args(
                    "command rule requires a command prefix",
                ));
            }
            CommandRule::new(pattern, decision, rule_match)?
        }
    }
    .with_scope(scope);
    if let Some(justification) = runtime_flag_value(&invocation.args, "--justification") {
        rule = rule.with_justification(justification);
    }
    Ok(rule)
}

/// Runs the runtime command rule pattern args operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_command_rule_pattern_args(invocation: &CommandInvocation) -> Vec<&str> {
    let mut values = Vec::new();
    let mut index = 0;
    while index < invocation.args.len() {
        let arg = invocation.args[index].as_str();
        if arg == "--" {
            values.extend(invocation.args[index + 1..].iter().map(String::as_str));
            break;
        }
        if matches!(
            arg,
            "--scope" | "--match" | "--exact-sha256" | "--shell-classification" | "--justification"
        ) {
            index = index.saturating_add(2);
            continue;
        }
        values.push(arg);
        index += 1;
    }
    values
}

/// Runs the runtime command rule scope operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_command_rule_scope(
    invocation: &CommandInvocation,
) -> Result<CommandRuleScope> {
    let value = runtime_flag_value(&invocation.args, "--scope").unwrap_or("session");
    match value {
        "session" => Ok(CommandRuleScope::Session),
        "project" => Ok(CommandRuleScope::Project),
        "user" | "global" => Ok(CommandRuleScope::User),
        "managed" => Ok(CommandRuleScope::Managed),
        "built-in" => Err(MezError::invalid_args(
            "built-in command rules cannot be added through the live policy",
        )),
        _ => Err(MezError::invalid_args(
            "command rule scope must be session, project, user, global, or managed",
        )),
    }
}

/// Runs the runtime command rule match operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_command_rule_match(invocation: &CommandInvocation) -> Result<RuleMatch> {
    if let Some(digest) = runtime_flag_value(&invocation.args, "--exact-sha256") {
        return Ok(RuleMatch::ExactSha256 {
            digest_hex: digest.to_string(),
            shell_classification: runtime_flag_value(&invocation.args, "--shell-classification")
                .unwrap_or(DEFAULT_COMMAND_SHELL_CLASSIFICATION)
                .to_string(),
        });
    }
    match runtime_flag_value(&invocation.args, "--match").unwrap_or("prefix") {
        "prefix" => Ok(RuleMatch::Prefix),
        "exact" => Ok(RuleMatch::Exact),
        "exact_sha256" => Err(MezError::invalid_args(
            "exact_sha256 command rules require --exact-sha256",
        )),
        _ => Err(MezError::invalid_args(
            "command rule match must be prefix or exact",
        )),
    }
}

/// Runs the runtime bypass approvals command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_bypass_approvals_command(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let requested = runtime_bypass_action(invocation).unwrap_or("status");
    let current = service.permission_policy().approval_bypass();
    let body = match requested {
        "status" | "show" => format!("bypass={current}:source=runtime-policy"),
        "enable" | "on" | "true" => {
            if current {
                return Ok(
                    "requested=enable:bypass=true:changed=false:source=runtime-policy".to_string(),
                );
            }
            if !runtime_has_bypass_confirmation(invocation) {
                return Ok("requested=enable:bypass=false:changed=false:confirmation_required=true:reason=explicit-confirmation-required:source=runtime-policy".to_string());
            }
            service.set_live_approval_bypass_override(true);
            runtime_append_permission_audit(
                service,
                "permissions.bypass_mode",
                "approval_bypass",
                "enabled",
                "changed",
            )?;
            "requested=enable:bypass=true:changed=true:confirmed=true:source=runtime-policy"
                .to_string()
        }
        "disable" | "off" | "false" => {
            service.set_live_approval_bypass_override(false);
            if current {
                runtime_append_permission_audit(
                    service,
                    "permissions.bypass_mode",
                    "approval_bypass",
                    "disabled",
                    "changed",
                )?;
            }
            format!(
                "requested=disable:bypass=false:changed={}:source=runtime-policy",
                current
            )
        }
        _ => format!(
            "requested={requested}:bypass={current}:changed=false:reason=unsupported-bypass-command:source=runtime-policy"
        ),
    };
    Ok(body)
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

/// Runs the runtime show options command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_show_options_command(
    service: &RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let effective = compose_effective_config(&service.config_layers)?;
    let filter = runtime_positional_args(invocation).first().copied();
    let mut lines = vec![format!(
        "options={}:applied_layers={}:skipped_layers={}:source=runtime-config",
        effective.values().len(),
        effective.applied_layers().len(),
        effective.skipped_layers().len()
    )];
    for (path, value) in effective.values() {
        if let Some(filter) = filter
            && path != filter
        {
            continue;
        }
        lines.push(format!(
            "path={path}:value={}:source={}:live_mutable={}",
            json_escape(&value.value),
            json_escape(&value.source_layer),
            runtime_option_live_mutable(path)
        ));
    }
    Ok(lines.join("\n"))
}

/// Runs the runtime set option command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_set_option_command(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let args = runtime_positional_args(invocation);
    let path = args
        .first()
        .copied()
        .ok_or_else(|| MezError::invalid_args("set-option requires an option path"))?;
    let value = args
        .get(1)
        .copied()
        .ok_or_else(|| MezError::invalid_args("set-option requires a value"))?;
    if path == "theme.active" {
        let definition = runtime_theme_definition_for_selection(service, value)?;
        let mutations = runtime_theme_config_mutations(value, &definition)?;
        let plan = runtime_apply_theme_live_override(service, &mutations)?;
        let report = service.apply_runtime_config_layers()?;
        service.append_lifecycle_event(
            EventKind::ConfigChanged,
            runtime_config_apply_event_payload("terminal/command:set-option", &report),
        )?;
        return Ok(format!(
            "path={path}:value={value}:changed={}:reload_required={}:source=runtime-config:layer={TERMINAL_COMMAND_LIVE_OVERRIDE_LAYER}:aliases={}:color_slots={}",
            plan.changed,
            plan.reload_required,
            definition.aliases.len(),
            UI_COLOR_SLOT_NAMES.len()
        ));
    }
    let mutation = ConfigMutation {
        path: path.to_string(),
        operation: ConfigMutationOperation::Set(runtime_config_command_value(value)),
    };
    let plan = runtime_plan_live_override_mutation(service, mutation)?;
    runtime_store_live_override_plan(service, &plan.text);
    let report = service.apply_runtime_config_layers()?;
    service.append_lifecycle_event(
        EventKind::ConfigChanged,
        runtime_config_apply_event_payload("terminal/command:set-option", &report),
    )?;
    Ok(format!(
        "path={path}:value={value}:changed={}:reload_required={}:source=runtime-config:layer={TERMINAL_COMMAND_LIVE_OVERRIDE_LAYER}",
        plan.changed, plan.reload_required
    ))
}

/// Runs the runtime set theme command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_set_theme_command(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let args = runtime_positional_args(invocation);
    let theme = args
        .first()
        .copied()
        .ok_or_else(|| MezError::invalid_args("set-theme requires a theme name"))?;
    if args.len() > 1 {
        return Err(MezError::invalid_args(
            "set-theme accepts exactly one theme name",
        ));
    }
    if !runtime_theme_available(service, theme)? {
        return Err(MezError::invalid_args(format!(
            "set-theme unknown theme `{theme}`; run list-themes to see available themes"
        )));
    }

    let definition = runtime_theme_definition_for_selection(service, theme)?;
    let mutations = runtime_theme_config_mutations(theme, &definition)?;
    let persist_plan = runtime_plan_theme_persistence(service, &mutations)?;
    let live_plan = runtime_apply_theme_live_override(service, &mutations)?;
    let persist_report = runtime_persist_theme_plan(service, persist_plan)?;
    let report = service.apply_runtime_config_layers()?;
    service.append_lifecycle_event(
        EventKind::ConfigChanged,
        runtime_config_apply_event_payload("terminal/command:set-theme", &report),
    )?;
    let persisted_path = persist_report
        .path
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "none".to_string());
    Ok(format!(
        "theme={theme}:changed={}:reload_required={}:source=runtime-config:layer={TERMINAL_COMMAND_LIVE_OVERRIDE_LAYER}:persisted={}:persisted_changed={}:persisted_reload_required={}:persisted_path={}:aliases={}:color_slots={}",
        live_plan.changed,
        live_plan.reload_required,
        persist_report.persisted,
        persist_report.changed,
        persist_report.reload_required,
        json_escape(&persisted_path),
        definition.aliases.len(),
        UI_COLOR_SLOT_NAMES.len()
    ))
}

/// Accumulates a sequence of scalar configuration mutations into one validated
/// replacement document.
struct RuntimeConfigMutationBatch {
    /// Final config text after all mutations are applied.
    text: String,
    /// Whether any mutation changed the input text.
    changed: bool,
    /// Whether applying the mutations requires runtime config reload.
    reload_required: bool,
}

/// Planned persisted theme update for the primary config file.
struct RuntimeThemePersistencePlan {
    /// Primary config file to rewrite.
    path: PathBuf,
    /// Config format inferred from the primary file extension.
    format: ConfigFormat,
    /// Final validated primary config text.
    text: String,
    /// Whether the final text differs from the current primary config.
    changed: bool,
    /// Whether runtime reload is needed after persistence.
    reload_required: bool,
}

/// Result of applying a persisted theme update.
struct RuntimeThemePersistenceReport {
    /// Whether a primary config target was available and updated.
    persisted: bool,
    /// Whether the persisted target changed.
    changed: bool,
    /// Whether the persisted update would require reload.
    reload_required: bool,
    /// Primary config file that received the selected theme, when available.
    path: Option<PathBuf>,
}

/// Result of applying a persisted model-authored config mutation batch.
pub(super) struct RuntimePersistedConfigMutationBatchReport {
    /// Primary config file that received the batch.
    pub path: PathBuf,
    /// Whether the persisted target changed.
    pub changed: bool,
    /// Whether the batch required a runtime reload.
    pub reload_required: bool,
    /// Number of scalar mutations included in the batch.
    pub mutation_count: usize,
    /// Whether persistence was deferred to the async side-effect writer.
    pub deferred: bool,
}

/// Returns the full theme definition that should be materialized for a selected
/// theme name.
fn runtime_theme_definition_for_selection(
    service: &RuntimeSessionService,
    theme: &str,
) -> Result<UiThemeDefinition> {
    if let Some(definition) = builtin_ui_theme_definition(theme) {
        resolve_ui_theme(theme, definition.clone())?;
        return Ok(definition);
    }

    let structured = runtime_effective_config_value(&service.config_layers)?;
    let Some(custom_theme) = structured
        .get("themes")
        .and_then(Value::as_object)
        .and_then(|themes| themes.get(theme))
    else {
        return Err(MezError::invalid_args(format!(
            "set-theme unknown theme `{theme}`; run list-themes to see available themes"
        )));
    };

    let mut definition = builtin_ui_theme_definition("deepforest")
        .ok_or_else(|| MezError::config("built-in deepforest theme is unavailable"))?;
    definition.merge(runtime_theme_definition_from_json(
        custom_theme,
        &format!("themes.{theme}"),
    )?);
    resolve_ui_theme(theme, definition.clone())?;
    Ok(definition)
}

/// Extracts a string-based theme definition from structured config JSON.
fn runtime_theme_definition_from_json(value: &Value, path: &str) -> Result<UiThemeDefinition> {
    let object = value
        .as_object()
        .ok_or_else(|| MezError::config(format!("{path} must be a table")))?;
    Ok(UiThemeDefinition {
        aliases: runtime_string_map_from_json(object.get("aliases"), &format!("{path}.aliases"))?,
        colors: runtime_string_map_from_json(object.get("colors"), &format!("{path}.colors"))?,
    })
}

/// Extracts a string-to-string map from a structured config object.
fn runtime_string_map_from_json(
    value: Option<&Value>,
    path: &str,
) -> Result<BTreeMap<String, String>> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let object = value
        .as_object()
        .ok_or_else(|| MezError::config(format!("{path} must be a table")))?;
    object
        .iter()
        .map(|(key, value)| {
            value
                .as_str()
                .map(|value| (key.clone(), value.to_string()))
                .ok_or_else(|| MezError::config(format!("{path}.{key} must be a string")))
        })
        .collect()
}

/// Builds the scalar config mutations that make a selected theme self-contained
/// in a root `theme` table.
fn runtime_theme_config_mutations(
    theme: &str,
    definition: &UiThemeDefinition,
) -> Result<Vec<ConfigMutation>> {
    let missing_slots = UI_COLOR_SLOT_NAMES
        .iter()
        .filter(|slot| !definition.colors.contains_key(**slot))
        .copied()
        .collect::<Vec<_>>();
    if !missing_slots.is_empty() {
        return Err(MezError::config(format!(
            "theme `{theme}` is missing color slots: {}",
            missing_slots.join(", ")
        )));
    }

    let mut mutations =
        Vec::with_capacity(1 + definition.aliases.len() + UI_COLOR_SLOT_NAMES.len());
    mutations.push(ConfigMutation {
        path: "theme.active".to_string(),
        operation: ConfigMutationOperation::Set(ConfigMutationValue::String(theme.to_string())),
    });
    for (alias, value) in &definition.aliases {
        mutations.push(ConfigMutation {
            path: format!("theme.aliases.{alias}"),
            operation: ConfigMutationOperation::Set(ConfigMutationValue::String(value.clone())),
        });
    }
    for slot in UI_COLOR_SLOT_NAMES {
        let value = definition.colors.get(*slot).ok_or_else(|| {
            MezError::config(format!("theme `{theme}` is missing color slot `{slot}`"))
        })?;
        mutations.push(ConfigMutation {
            path: format!("theme.colors.{slot}"),
            operation: ConfigMutationOperation::Set(ConfigMutationValue::String(value.clone())),
        });
    }
    Ok(mutations)
}

/// Applies a theme mutation batch to the terminal-command live override layer.
fn runtime_apply_theme_live_override(
    service: &mut RuntimeSessionService,
    mutations: &[ConfigMutation],
) -> Result<RuntimeConfigMutationBatch> {
    let current_text = service
        .config_layers
        .iter()
        .find(|layer| {
            layer.name == TERMINAL_COMMAND_LIVE_OVERRIDE_LAYER
                && layer.scope == ConfigScope::LiveOverride
        })
        .map(|layer| layer.text.as_str())
        .unwrap_or("");
    let batch = runtime_plan_config_mutations(
        ConfigFormat::Toml,
        current_text,
        ConfigScope::LiveOverride,
        mutations,
    )?;
    runtime_store_live_override_plan(service, &batch.text);
    Ok(batch)
}

/// Plans a persisted primary-config update for a selected theme.
fn runtime_plan_theme_persistence(
    service: &RuntimeSessionService,
    mutations: &[ConfigMutation],
) -> Result<Option<RuntimeThemePersistencePlan>> {
    let Some(path) = runtime_primary_config_path(service)? else {
        return Ok(None);
    };
    let format = ConfigFormat::from_path(&path)?;
    let text = fs::read_to_string(&path)?;
    let batch = runtime_plan_config_mutations(format, &text, ConfigScope::Primary, mutations)?;
    Ok(Some(RuntimeThemePersistencePlan {
        path,
        format,
        text: batch.text,
        changed: batch.changed,
        reload_required: batch.reload_required,
    }))
}

/// Persists a planned selected-theme update and mirrors the updated primary
/// layer text into the live runtime service.
fn runtime_persist_theme_plan(
    service: &mut RuntimeSessionService,
    plan: Option<RuntimeThemePersistencePlan>,
) -> Result<RuntimeThemePersistenceReport> {
    let Some(plan) = plan else {
        return Ok(RuntimeThemePersistenceReport {
            persisted: false,
            changed: false,
            reload_required: false,
            path: None,
        });
    };

    if plan.changed {
        persist_config_text(&plan.path, ConfigScope::Primary, &plan.text)?;
    }
    runtime_store_primary_config_text(service, plan.path.clone(), plan.format, plan.text.clone());
    Ok(RuntimeThemePersistenceReport {
        persisted: true,
        changed: plan.changed,
        reload_required: plan.reload_required,
        path: Some(plan.path),
    })
}

/// Applies one validated persisted config mutation batch and reloads once.
///
/// # Parameters
/// - `service`: Runtime service receiving the applied configuration.
/// - `path`: Primary config path to update or queue for persistence.
/// - `mutations`: Ordered scalar mutations to fold into the target document.
/// - `event_source`: Event payload source used for the resulting config-change
///   lifecycle event.
pub(super) fn runtime_apply_persisted_config_mutation_batch(
    service: &mut RuntimeSessionService,
    path: PathBuf,
    mutations: &[ConfigMutation],
    event_source: &str,
) -> Result<RuntimePersistedConfigMutationBatchReport> {
    if mutations.is_empty() {
        return Err(MezError::invalid_args(
            "persisted config mutation batch requires at least one mutation",
        ));
    }
    let format = ConfigFormat::from_path(&path)?;
    let current_text = service
        .config_layers
        .iter()
        .find(|layer| layer.scope == ConfigScope::Primary && layer.path.as_ref() == Some(&path))
        .map(|layer| Ok(layer.text.clone()))
        .unwrap_or_else(|| fs::read_to_string(&path))?;
    let batch =
        runtime_plan_config_mutations(format, &current_text, ConfigScope::Primary, mutations)?;
    if batch.changed {
        if !service.defer_config_file_writes {
            persist_config_text(&path, ConfigScope::Primary, &batch.text)?;
        }
        let previous_layers = service.config_layers.clone();
        runtime_store_primary_config_text(service, path.clone(), format, batch.text.clone());
        match service.apply_runtime_config_layers() {
            Ok(report) => {
                service.append_lifecycle_event(
                    EventKind::ConfigChanged,
                    runtime_config_apply_event_payload(event_source, &report),
                )?;
                if service.defer_config_file_writes {
                    service
                        .deferred_config_file_writes
                        .push(DeferredConfigFileWrite {
                            path: path.clone(),
                            scope: ConfigScope::Primary,
                            text: batch.text.clone(),
                        });
                }
                service.session.advance_config_generation();
            }
            Err(error) => {
                service.config_layers = previous_layers;
                let _ = service.apply_runtime_config_layers();
                return Err(error);
            }
        }
    }
    Ok(RuntimePersistedConfigMutationBatchReport {
        path,
        changed: batch.changed,
        reload_required: batch.reload_required,
        mutation_count: mutations.len(),
        deferred: service.defer_config_file_writes,
    })
}

/// Finds or creates the primary config file used for persisted command changes.
fn runtime_primary_config_path(service: &RuntimeSessionService) -> Result<Option<PathBuf>> {
    if let Some(path) = service
        .config_layers
        .iter()
        .find(|layer| layer.scope == ConfigScope::Primary && layer.path.is_some())
        .and_then(|layer| layer.path.clone())
    {
        return Ok(Some(path));
    }
    let Some(root) = service.config_root.as_ref() else {
        return Ok(None);
    };
    ConfigPaths::from_root(root.clone())
        .ensure_default_config()
        .map(Some)
}

/// Updates the in-memory primary config layer after persisting a selected theme.
fn runtime_store_primary_config_text(
    service: &mut RuntimeSessionService,
    path: PathBuf,
    format: ConfigFormat,
    text: String,
) {
    if let Some(layer) = service
        .config_layers
        .iter_mut()
        .find(|layer| layer.scope == ConfigScope::Primary && layer.path.as_ref() == Some(&path))
    {
        layer.text = text;
        layer.format = format;
        return;
    }
    service.config_layers.push(ConfigLayer {
        name: "primary".to_string(),
        path: Some(path),
        format,
        scope: ConfigScope::Primary,
        trusted: true,
        text,
    });
}

/// Applies a validated sequence of scalar config mutations to in-memory text.
fn runtime_plan_config_mutations(
    format: ConfigFormat,
    text: &str,
    scope: ConfigScope,
    mutations: &[ConfigMutation],
) -> Result<RuntimeConfigMutationBatch> {
    let mut text = text.to_string();
    let mut changed = false;
    let mut reload_required = false;
    for mutation in mutations {
        let plan = plan_config_mutation(format, &text, scope, mutation.clone())?;
        changed |= plan.changed;
        reload_required |= plan.reload_required;
        text = plan.text;
    }
    Ok(RuntimeConfigMutationBatch {
        text,
        changed,
        reload_required,
    })
}

/// Runs the runtime theme available operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_theme_available(service: &RuntimeSessionService, theme: &str) -> Result<bool> {
    if BUILTIN_UI_THEME_NAMES.contains(&theme) {
        return Ok(true);
    }
    let structured = runtime_effective_config_value(&service.config_layers)?;
    Ok(structured
        .get("themes")
        .and_then(|value| value.as_object())
        .map(|themes| themes.contains_key(theme))
        .unwrap_or(false))
}

/// Runs the runtime list themes command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_list_themes_command(service: &RuntimeSessionService) -> Result<String> {
    let structured = runtime_effective_config_value(&service.config_layers)?;
    let mut custom_theme_names = structured
        .get("themes")
        .and_then(|value| value.as_object())
        .map(|themes| themes.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    custom_theme_names.sort();
    custom_theme_names.dedup();

    let mut lines = BUILTIN_UI_THEME_NAMES
        .iter()
        .map(|theme| {
            format!(
                "theme={theme}:source=builtin:active={}:action=set-theme {theme}",
                *theme == service.ui_theme.name
            )
        })
        .collect::<Vec<_>>();
    lines.extend(
        custom_theme_names
            .iter()
            .filter(|theme| !BUILTIN_UI_THEME_NAMES.contains(&theme.as_str()))
            .map(|theme| {
                format!(
                    "theme={theme}:source=config:active={}:action=set-theme {theme}",
                    theme == &service.ui_theme.name
                )
            }),
    );
    Ok(lines.join("\n"))
}

/// Runs the runtime source file command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_source_file_command(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let path = runtime_positional_args(invocation)
        .first()
        .copied()
        .ok_or_else(|| MezError::invalid_args("source-file requires a path"))?;
    let path = runtime_expand_user_path(path);
    let format = ConfigFormat::from_path(&path)?;
    let text = fs::read_to_string(&path)?;
    let validation = validate_config_text(format, &text, ConfigScope::LiveOverride);
    if !validation.valid {
        return Err(MezError::config(format!(
            "source-file rejected invalid config: {}",
            validation
                .diagnostics
                .iter()
                .map(|diagnostic| format!("{}: {}", diagnostic.path, diagnostic.message))
                .collect::<Vec<_>>()
                .join("; ")
        )));
    }
    let layer_name = format!("source-file:{}", path.display());
    if let Some(layer) = service
        .config_layers
        .iter_mut()
        .find(|layer| layer.name == layer_name)
    {
        layer.text = text;
        layer.format = format;
        layer.path = Some(path.clone());
        layer.scope = ConfigScope::LiveOverride;
        layer.trusted = true;
    } else {
        service.config_layers.push(ConfigLayer {
            name: layer_name.clone(),
            path: Some(path.clone()),
            format,
            scope: ConfigScope::LiveOverride,
            trusted: true,
            text,
        });
    }
    let report = service.apply_runtime_config_layers()?;
    service.append_lifecycle_event(
        EventKind::ConfigChanged,
        runtime_config_apply_event_payload("terminal/command:source-file", &report),
    )?;
    Ok(format!(
        "path={}:applied=true:changed=true:source=runtime-config:layer={}",
        path.display(),
        json_escape(&layer_name)
    ))
}

/// Runs the runtime refresh client command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_refresh_client_command(
    service: &mut RuntimeSessionService,
) -> Result<String> {
    let primary = service
        .session
        .primary_client_id()
        .cloned()
        .ok_or_else(|| MezError::invalid_state("refresh-client requires an attached primary"))?;
    let size = service.session.authoritative_size;
    service.append_lifecycle_event(
        EventKind::Diagnostic,
        format!(
            r#"{{"client_id":"{}","refresh_client":true,"columns":{},"rows":{}}}"#,
            json_escape(primary.as_str()),
            size.columns,
            size.rows
        ),
    )?;
    Ok(format!(
        "client={primary}:refreshed=true:columns={}:rows={}:source=runtime-client-state",
        size.columns, size.rows
    ))
}

/// Runs the runtime plan live override mutation operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_plan_live_override_mutation(
    service: &RuntimeSessionService,
    mutation: ConfigMutation,
) -> Result<crate::config::ConfigMutationPlan> {
    let current_text = service
        .config_layers
        .iter()
        .find(|layer| {
            layer.name == TERMINAL_COMMAND_LIVE_OVERRIDE_LAYER
                && layer.scope == ConfigScope::LiveOverride
        })
        .map(|layer| layer.text.as_str())
        .unwrap_or("");
    plan_config_mutation(
        ConfigFormat::Toml,
        current_text,
        ConfigScope::LiveOverride,
        mutation,
    )
}

/// Runs the runtime store live override plan operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_store_live_override_plan(service: &mut RuntimeSessionService, text: &str) {
    if let Some(layer) = service.config_layers.iter_mut().find(|layer| {
        layer.name == TERMINAL_COMMAND_LIVE_OVERRIDE_LAYER
            && layer.scope == ConfigScope::LiveOverride
    }) {
        layer.text = text.to_string();
    } else {
        service.config_layers.push(ConfigLayer {
            name: TERMINAL_COMMAND_LIVE_OVERRIDE_LAYER.to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::LiveOverride,
            trusted: true,
            text: text.to_string(),
        });
    }
}

/// Runs the runtime config command value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_config_command_value(value: &str) -> ConfigMutationValue {
    match value {
        "true" => ConfigMutationValue::Boolean(true),
        "false" => ConfigMutationValue::Boolean(false),
        _ => value
            .parse::<i64>()
            .map(ConfigMutationValue::Integer)
            .unwrap_or_else(|_| ConfigMutationValue::String(value.to_string())),
    }
}

/// Runs the runtime option live mutable operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_option_live_mutable(path: &str) -> bool {
    path.starts_with("mcp_servers.")
        || path.starts_with("agents.auto_sizing.")
        || path.starts_with("theme.aliases.")
        || path.starts_with("theme.colors.")
        || matches!(
            path,
            "history.lines"
                | "history.rotate_lines"
                | "agents.max_concurrent_agents"
                | "agents.max_root_subagents"
                | "agents.max_subagents_per_subagent"
                | "agents.max_depth"
                | "agents.auto_compact"
                | "agents.auto_compact_threshold"
                | "agents.compaction_raw_retention_percent"
                | "agents.routing"
                | "agents.action_failure_retry_limit"
                | "agents.shell_only"
                | "agents.subagent_placement"
                | "agents.subagent_wait_policy"
                | "frames.window.enabled"
                | "frames.window.template"
                | "frames.window.right_status"
                | "frames.window.position"
                | "frames.window.style"
                | "frames.window.visible_fields"
                | "frames.pane.enabled"
                | "frames.pane.template"
                | "frames.pane.position"
                | "frames.pane.style"
                | "frames.pane.visible_fields"
                | "terminal.term"
                | "terminal.profile"
                | "terminal.cursor_style"
                | "terminal.cursor_blink"
                | "terminal.cursor_blink_interval_ms"
                | "terminal.reduced_motion"
                | "terminal.resize_debounce_ms"
                | "terminal.render_rate_limit_fps"
                | "terminal.true_color"
                | "terminal.mouse"
                | "terminal.clipboard"
                | "terminal.bracketed_paste"
                | "terminal.focus_events"
                | "terminal.alternate_screen"
                | "theme.active"
                | "permissions.preset"
                | "permissions.approval_policy"
                | "permissions.bypass_mode"
                | "permissions.network_policy"
                | "permissions.destructive_action_policy"
                | "instructions.max_bytes"
                | "instructions.include_hidden_directories"
                | "instructions.on_truncation"
        )
}

/// Runs the runtime append permission audit operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_append_permission_audit(
    service: &mut RuntimeSessionService,
    permission_id: &str,
    action_kind: &str,
    decision: &str,
    outcome: &str,
) -> Result<()> {
    let policy_mode = runtime_permission_preset_name(service.permission_policy.preset).to_string();
    let Some(audit_log) = service.audit_log.as_mut() else {
        return Ok(());
    };
    let record = AuditRecord::permission_decision(
        service.session.id.to_string(),
        AuditActor {
            kind: "client".to_string(),
            id: "primary-command".to_string(),
        },
        permission_id.to_string(),
        action_kind.to_string(),
        decision.to_string(),
        policy_mode,
        outcome.to_string(),
    );
    let _ = audit_log.append(record)?;
    Ok(())
}

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
    let actor = AuditActor {
        kind: "client".to_string(),
        id: "primary-command".to_string(),
    };
    let record = match invocation.name.as_str() {
        "auth-login" if body.contains("logged_in=true") || body.contains("authenticated=true") => {
            AuditRecord::auth_change(
                service.session.id.to_string(),
                actor,
                "openai",
                "default",
                "login",
                "succeeded",
            )
        }
        _ => return Ok(()),
    };
    let _ = audit_log.append(record)?;
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

/// Runs the runtime bypass action operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_bypass_action(invocation: &CommandInvocation) -> Option<&str> {
    invocation
        .args
        .iter()
        .find(|arg| {
            !matches!(
                arg.as_str(),
                "--confirm" | "--yes" | "--dangerously-bypass-approvals"
            )
        })
        .map(String::as_str)
}

/// Runs the runtime has bypass confirmation operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_has_bypass_confirmation(invocation: &CommandInvocation) -> bool {
    invocation.args.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "--confirm" | "--yes" | "--dangerously-bypass-approvals"
        )
    })
}

/// Runs the runtime parse permission preset operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_parse_permission_preset(
    value: &str,
) -> std::result::Result<PermissionPreset, ()> {
    match value {
        "read-only" | "readonly" => Ok(PermissionPreset::ReadOnly),
        "auto" => Ok(PermissionPreset::Auto),
        _ => Err(()),
    }
}

/// Runs the runtime parse approval policy operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_parse_approval_policy(
    value: &str,
) -> std::result::Result<ApprovalPolicy, ()> {
    match value {
        "ask" => Ok(ApprovalPolicy::Ask),
        "auto-allow" | "auto_allow" => Ok(ApprovalPolicy::AutoAllow),
        "full-access" | "full_access" => Ok(ApprovalPolicy::FullAccess),
        _ => Err(()),
    }
}

/// Runs the runtime permission preset name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_permission_preset_name(preset: PermissionPreset) -> &'static str {
    match preset {
        PermissionPreset::ReadOnly => "read-only",
        PermissionPreset::Auto => "auto",
    }
}

/// Runs the runtime approval policy name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_approval_policy_name(policy: ApprovalPolicy) -> &'static str {
    match policy {
        ApprovalPolicy::Ask => "ask",
        ApprovalPolicy::AutoAllow => "auto-allow",
        ApprovalPolicy::FullAccess => "full-access",
    }
}

/// Runs the runtime permission authority change name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_permission_authority_change_name(
    change: PermissionAuthorityChange,
) -> &'static str {
    match change {
        PermissionAuthorityChange::Narrowing => "narrowing",
        PermissionAuthorityChange::NoChange => "no-change",
        PermissionAuthorityChange::Broadening => "broadening",
    }
}

/// Runs the runtime command rule scope name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_command_rule_scope_name(scope: CommandRuleScope) -> &'static str {
    match scope {
        CommandRuleScope::BuiltIn => "built-in",
        CommandRuleScope::Session => "session",
        CommandRuleScope::Project => "project",
        CommandRuleScope::User => "user",
        CommandRuleScope::Managed => "managed",
    }
}

/// Runs the runtime rule decision name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_rule_decision_name(decision: RuleDecision) -> &'static str {
    match decision {
        RuleDecision::Forbid => "deny",
        RuleDecision::Prompt => "prompt",
        RuleDecision::Allow => "allow",
    }
}

/// Runs the runtime rule match name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_rule_match_name(rule_match: &RuleMatch) -> &'static str {
    match rule_match {
        RuleMatch::Prefix => "prefix",
        RuleMatch::Exact => "exact",
        RuleMatch::ExactSha256 { .. } => "exact_sha256",
    }
}

/// Runs the runtime argument policy name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_argument_policy_name(argument_policy: &ArgumentPolicy) -> &'static str {
    match argument_policy {
        ArgumentPolicy::None => "none",
        ArgumentPolicy::ExecutableProbe { .. } => "executable_probe",
        ArgumentPolicy::UnameProbe => "uname_probe",
        ArgumentPolicy::LiteralOutput => "literal_output",
        ArgumentPolicy::ReadPaths { .. } => "read_paths",
        ArgumentPolicy::ScriptThenReadPaths { .. } => "script_then_read_paths",
        ArgumentPolicy::FindReadOnly => "find_read_only",
        ArgumentPolicy::GitReadOnly { .. } => "git_read_only",
    }
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

/// Runs the runtime paste bytes operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_paste_bytes(screen: Option<&TerminalScreen>, content: &str) -> Vec<u8> {
    if screen.is_some_and(TerminalScreen::bracketed_paste_enabled) {
        let mut bytes = Vec::with_capacity(content.len().saturating_add(12));
        bytes.extend_from_slice(b"\x1b[200~");
        bytes.extend_from_slice(content.as_bytes());
        bytes.extend_from_slice(b"\x1b[201~");
        bytes
    } else {
        content.as_bytes().to_vec()
    }
}

/// Runs the runtime list buffers display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_list_buffers_display(buffers: Vec<PasteBuffer>) -> String {
    if buffers.is_empty() {
        return "buffers=0 source=runtime status=empty".to_string();
    }
    let lines = buffers
        .iter()
        .map(|buffer| {
            let origin = buffer.origin.as_deref().unwrap_or("unknown");
            format!(
                "buffer={}:bytes={}:created_at={}:origin={}:preview={}",
                buffer.name,
                buffer.bytes,
                buffer.created_at_unix_seconds,
                json_escape(origin),
                json_escape(&buffer.preview)
            )
        })
        .collect::<Vec<_>>();
    format!(
        "buffers={} source=runtime\n{}",
        buffers.len(),
        lines.join("\n")
    )
}

/// Runs the runtime list panes display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_list_panes_display(service: &RuntimeSessionService) -> Result<String> {
    let window = service
        .session
        .active_window()
        .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
    let lines = window
        .panes()
        .iter()
        .map(|pane| {
            let primary_pid = service
                .primary_pid_for_live_pane_process(pane.id.as_str())
                .map(|pid| pid.to_string())
                .unwrap_or_else(|| "none".to_string());
            let agent_id = service
                .agent_shell_store
                .get(pane.id.as_str())
                .map(|_| format!("agent-{}", pane.id))
                .unwrap_or_else(|| "none".to_string());
            format!(
                "pane={}:index={}:title={}:active={}:primary_pid={}:size={}x{}:agent_id={}:live={}:source=runtime",
                pane.id,
                pane.index,
                json_escape(&pane.title),
                pane.active,
                primary_pid,
                pane.size.columns,
                pane.size.rows,
                agent_id,
                pane.live
            )
        })
        .collect::<Vec<_>>();
    Ok(format!(
        "panes={} window={} source=runtime\n{}",
        lines.len(),
        window.id,
        lines.join("\n")
    ))
}

/// Runs the runtime display panes display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_display_panes_display(service: &RuntimeSessionService) -> Result<String> {
    let window = service
        .session
        .active_window()
        .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
    let lines = window
        .panes()
        .iter()
        .map(|pane| {
            format!(
                "pane={}:index={}:label={}:active={}:title={}:size={}x{}:action=select-pane -t {}",
                pane.id,
                pane.index,
                pane.index,
                pane.active,
                json_escape(&pane.title),
                pane.size.columns,
                pane.size.rows,
                pane.id
            )
        })
        .collect::<Vec<_>>();
    Ok(format!(
        "panes={} window={} chooser=select-pane-index source=runtime\n{}",
        lines.len(),
        window.id,
        lines.join("\n")
    ))
}

/// Runs the runtime list observers display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_list_observers_display(service: &RuntimeSessionService) -> String {
    let observers = service.session.observers();
    if observers.is_empty() {
        return "observers=0 pending=0 chooser=empty source=runtime\nNo observer requests or approved observers.".to_string();
    }
    let pending = observers
        .iter()
        .filter(|observer| observer.state == crate::session::ObserverDecisionState::Pending)
        .count();
    let lines = observers
        .iter()
        .map(|observer| {
            format!(
                "observer={}:client={}:state={}:requested_at={}:decided_at={}:decided_by={}:visible_from={}:visible_from_time={}:descriptor={}:interactive={}:terminal={}:reason={}:actions={}:commands={}",
                observer.id,
                observer.client_id,
                runtime_observer_state_name(observer.state),
                runtime_optional_unix_seconds(observer.requested_at_unix_seconds),
                runtime_optional_unix_seconds(observer.decided_at_unix_seconds),
                observer
                    .decided_by_client_id
                    .as_deref()
                    .map(json_escape)
                    .unwrap_or_else(|| "none".to_string()),
                observer
                    .visible_from_event_id
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "none".to_string()),
                runtime_optional_unix_seconds(observer.visible_from_unix_seconds),
                json_escape(&observer.descriptor_name),
                observer.descriptor_interactive,
                runtime_observer_terminal_display(observer),
                observer
                    .reason
                    .as_deref()
                    .map(json_escape)
                    .unwrap_or_else(|| "none".to_string()),
                runtime_observer_actions(observer.state),
                runtime_observer_action_commands(observer)
            )
        })
        .collect::<Vec<_>>();
    format!(
        "observers={} pending={pending} chooser=select-observer-action source=runtime\n{}",
        observers.len(),
        lines.join("\n")
    )
}

/// Runs the runtime choose observer display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_choose_observer_display(service: &RuntimeSessionService) -> String {
    let observers = service.session.observers();
    if observers.is_empty() {
        return "observers=0 pending=0 chooser=empty source=runtime".to_string();
    }
    let pending = observers
        .iter()
        .filter(|observer| observer.state == crate::session::ObserverDecisionState::Pending)
        .count();
    let lines = observers
        .iter()
        .map(|observer| {
            format!(
                "observer={}:client={}:state={}:requested_at={}:terminal={}:descriptor={}:interactive={}:actions={}:commands={}",
                observer.id,
                observer.client_id,
                runtime_observer_state_name(observer.state),
                runtime_optional_unix_seconds(observer.requested_at_unix_seconds),
                runtime_observer_terminal_display(observer),
                json_escape(&observer.descriptor_name),
                observer.descriptor_interactive,
                runtime_observer_actions(observer.state),
                runtime_observer_action_commands(observer)
            )
        })
        .collect::<Vec<_>>();
    format!(
        "observers={}:pending={pending}:chooser=select-observer-action:source=runtime\n{}",
        observers.len(),
        lines.join("\n")
    )
}

/// Runs the runtime choose client display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_choose_client_display(service: &RuntimeSessionService) -> String {
    let clients = service.session.clients();
    let observers = service.session.observers();
    if clients.is_empty() {
        return format!(
            "clients=0 observers={} chooser=empty source=runtime",
            observers.len()
        );
    }
    let observer_context = observers
        .iter()
        .map(|observer| {
            (
                observer.client_id.to_string(),
                format!(
                    "{}:{}",
                    observer.id,
                    runtime_observer_state_name(observer.state)
                ),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let lines = clients
        .iter()
        .map(|client| {
            let observer = observer_context
                .get(&client.id.to_string())
                .cloned()
                .unwrap_or_else(|| "none".to_string());
            format!(
                "client={}:name={}:role={}:state={}:interactive={}:observer={}:action=detach-client -t {}",
                client.id,
                json_escape(&client.name),
                runtime_client_role_name(client.role),
                runtime_client_state_name(client.state),
                client.interactive,
                observer,
                client.id
            )
        })
        .collect::<Vec<_>>();
    format!(
        "clients={}:observers={}:chooser=detach-client:source=runtime\n{}",
        clients.len(),
        observers.len(),
        lines.join("\n")
    )
}

/// Runs the runtime list clients display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_list_clients_display(service: &RuntimeSessionService) -> String {
    let clients = service.session.clients();
    if clients.is_empty() {
        return "clients=0 source=runtime".to_string();
    }
    let lines = clients
        .iter()
        .map(|client| {
            let observer = service
                .session
                .observers()
                .iter()
                .find(|observer| observer.client_id == client.id);
            format!(
                "client={}:name={}:role={}:state={}:interactive={}:attached_at={}:last_seen_at={}:terminal={}:approval={}",
                client.id,
                json_escape(&client.name),
                runtime_client_role_name(client.role),
                runtime_client_state_name(client.state),
                client.interactive,
                runtime_optional_unix_seconds(client_attached_at_for_display(service, client)),
                runtime_optional_unix_seconds(client_last_seen_at_for_display(service, client)),
                runtime_client_terminal_display(service, client, observer),
                runtime_client_approval_display(observer)
            )
        })
        .collect::<Vec<_>>();
    format!(
        "clients={}:observers={}:source=runtime\n{}",
        clients.len(),
        service.session.observers().len(),
        lines.join("\n")
    )
}

/// Runs the runtime choose window display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_choose_window_display(service: &RuntimeSessionService) -> String {
    let windows = service.session.active_group_windows();
    if windows.is_empty() {
        return "windows=0 chooser=empty source=runtime".to_string();
    }
    let active_id = service
        .session
        .active_window()
        .map(|window| window.id.to_string())
        .unwrap_or_else(|| "none".to_string());
    let lines = windows
        .iter()
        .enumerate()
        .map(|(index, window)| {
            format!(
                "window={}:index={}:name={}:active={}:panes={}:size={}x{}:action=select-window -t {}",
                window.id,
                index,
                json_escape(&window.name),
                window.id.to_string() == active_id,
                window.panes().len(),
                window.size.columns,
                window.size.rows,
                window.id
            )
        })
        .collect::<Vec<_>>();
    format!(
        "windows={}:active={active_id}:chooser=select-window:source=runtime\n{}",
        windows.len(),
        lines.join("\n")
    )
}

/// Returns runtime window group rows for `list-groups`.
pub(super) fn runtime_list_groups_display(service: &RuntimeSessionService) -> String {
    service
        .session
        .window_groups()
        .iter()
        .map(|group| {
            format!(
                "{}:{}:{}:active={}:windows={}",
                group.index,
                group.id,
                json_escape(&group.name),
                service
                    .session
                    .active_group()
                    .is_some_and(|active| active.id == group.id),
                group.window_ids.len()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Returns runtime group chooser rows with concrete selection actions.
pub(super) fn runtime_choose_group_display(service: &RuntimeSessionService) -> String {
    let groups = service.session.window_groups();
    if groups.is_empty() {
        return "groups=0:chooser=empty:source=runtime".to_string();
    }
    let lines = groups
        .iter()
        .map(|group| {
            format!(
                "group={}:index={}:name={}:active={}:windows={}:action=select-group -t {}",
                group.id,
                group.index,
                json_escape(&group.name),
                service
                    .session
                    .active_group()
                    .is_some_and(|active| active.id == group.id),
                group.window_ids.len(),
                group.id
            )
        })
        .collect::<Vec<_>>();
    format!(
        "groups={}:chooser=select-group:source=runtime\n{}",
        groups.len(),
        lines.join("\n")
    )
}

/// Runs the client attached at for display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn client_attached_at_for_display(
    service: &RuntimeSessionService,
    client: &crate::session::Client,
) -> Option<u64> {
    if service
        .session
        .primary_client_id()
        .is_some_and(|primary| primary == &client.id)
    {
        service
            .last_attach_at_unix_seconds()
            .or(client.attached_at_unix_seconds)
    } else {
        client.attached_at_unix_seconds
    }
}

/// Runs the client last seen at for display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn client_last_seen_at_for_display(
    service: &RuntimeSessionService,
    client: &crate::session::Client,
) -> Option<u64> {
    if service
        .session
        .primary_client_id()
        .is_some_and(|primary| primary == &client.id)
    {
        service
            .last_attach_at_unix_seconds()
            .or(client.last_seen_at_unix_seconds)
    } else {
        client.last_seen_at_unix_seconds
    }
}

/// Runs the runtime optional unix seconds operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_optional_unix_seconds(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "none".to_string())
}

/// Runs the runtime client terminal display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_client_terminal_display(
    service: &RuntimeSessionService,
    client: &crate::session::Client,
    observer: Option<&crate::session::ObserverRequest>,
) -> String {
    if service
        .session
        .primary_client_id()
        .is_some_and(|primary| primary == &client.id)
    {
        return format!(
            "{}x{}:term={}",
            service.session.authoritative_size.columns,
            service.session.authoritative_size.rows,
            json_escape(service.terminal_term())
        );
    }
    if let Some(terminal) = client.terminal.as_ref() {
        return format!(
            "{}x{}:term={}",
            terminal.columns,
            terminal.rows,
            json_escape(&terminal.term)
        );
    }
    if let Some(terminal) = observer.and_then(|observer| observer.descriptor_terminal.as_ref()) {
        return format!(
            "{}x{}:term={}",
            terminal.columns,
            terminal.rows,
            json_escape(&terminal.term)
        );
    }
    "none".to_string()
}

/// Runs the runtime client approval display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_client_approval_display(observer: Option<&crate::session::ObserverRequest>) -> String {
    observer
        .map(|observer| {
            format!(
                "{}:{}",
                observer.id,
                runtime_observer_state_name(observer.state)
            )
        })
        .unwrap_or_else(|| "none".to_string())
}

/// Runs the runtime observer terminal display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_observer_terminal_display(observer: &crate::session::ObserverRequest) -> String {
    observer
        .descriptor_terminal
        .as_ref()
        .map(|terminal| {
            format!(
                "{}x{}:term={}",
                terminal.columns,
                terminal.rows,
                json_escape(&terminal.term)
            )
        })
        .unwrap_or_else(|| "none".to_string())
}

/// Runs the runtime client role name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_client_role_name(role: crate::session::ClientRole) -> &'static str {
    match role {
        crate::session::ClientRole::Primary => "primary",
        crate::session::ClientRole::PendingObserver => "pending_observer",
        crate::session::ClientRole::Observer => "observer",
        crate::session::ClientRole::Agent => "agent",
        crate::session::ClientRole::Automation => "automation",
    }
}

/// Runs the runtime client state name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_client_state_name(state: crate::session::ClientState) -> &'static str {
    match state {
        crate::session::ClientState::Attached => "attached",
        crate::session::ClientState::Pending => "pending",
        crate::session::ClientState::Detached => "detached",
        crate::session::ClientState::Revoked => "revoked",
        crate::session::ClientState::Failed => "failed",
    }
}

/// Runs the runtime observer state name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_observer_state_name(state: crate::session::ObserverDecisionState) -> &'static str {
    match state {
        crate::session::ObserverDecisionState::Pending => "pending",
        crate::session::ObserverDecisionState::Approved => "approved",
        crate::session::ObserverDecisionState::Rejected => "rejected",
        crate::session::ObserverDecisionState::Revoked => "revoked",
    }
}

/// Runs the runtime observer actions operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_observer_actions(state: crate::session::ObserverDecisionState) -> &'static str {
    match state {
        crate::session::ObserverDecisionState::Pending => "inspect,approve,reject",
        crate::session::ObserverDecisionState::Approved => "inspect,revoke,detach",
        crate::session::ObserverDecisionState::Rejected => "inspect",
        crate::session::ObserverDecisionState::Revoked => "inspect",
    }
}

/// Runs the runtime observer action commands operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_observer_action_commands(observer: &crate::session::ObserverRequest) -> String {
    match observer.state {
        crate::session::ObserverDecisionState::Pending => format!(
            "approve-observer -t {}|reject-observer -t {}",
            observer.id, observer.id
        ),
        crate::session::ObserverDecisionState::Approved => format!(
            "revoke-observer -t {}|detach-client -t {}",
            observer.client_id, observer.client_id
        ),
        crate::session::ObserverDecisionState::Rejected
        | crate::session::ObserverDecisionState::Revoked => "none".to_string(),
    }
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
