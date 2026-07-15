//! Command Dispatch implementation.
//!
//! This module owns the command dispatch boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::plans::{
    CommandPlan, PaneSelectionPlan, ResizePanePlan, SwapPaneNeighbor, SwapPanePlan,
    SynchronizePanesMode, command_plan_from_invocation,
};
use super::{
    AuditLog, AuthStore, ClientId, CommandInvocation, CommandOutcome, ConfigMutation,
    ConfigMutationOperation, ConfigPaths, ConfigScope, KeyChord, MezError, PaneNavigationDirection,
    PaneReadinessOverrideStore, PaneReadinessState, PathBuf, Result, Session,
    attach_session_display, auth_status_display, auth_status_store_display, bind_key_args,
    binding_config_key, capture_pane_display, choose_buffer_display, choose_client_display,
    choose_group_display, choose_observer_display, choose_window_display, clear_history_display,
    command_help_display, command_target_pane_id, config_set_string, config_unset,
    copy_mode_display, copy_selection_display, create_buffer_display, export_history_display,
    flag_value, key_chord_notation, list_baseline_commands, list_buffers_display, list_clients,
    list_current_session, list_default_key_bindings, list_default_themes, list_groups,
    list_observers, list_panes, list_windows, load_layout_selector, mark_pane_ready_audit_record,
    mark_pane_ready_warning_display, mcp_server_id, mcp_status_plan_display,
    mcp_status_store_display, mutated_pane_command_outcome, pane_readiness_state_name,
    parse_command_sequence, parse_config_command_value, paste_buffer_display,
    paste_clipboard_display, persist_command_config_mutation, persist_command_theme_config,
    persist_config_text, pipe_pane_display, positional_args, save_buffer_display, save_layout_name,
    search_history_display, set_option_args, set_theme_arg, show_default_options,
    show_messages_display, show_metrics_display, validate_config_file,
};

use crate::mcp::{
    mcp_config_command_display, mcp_config_command_from_words, mcp_config_command_report,
    persist_mcp_config_command,
};
use std::fs;

// In-memory command execution entry points.

/// Runs the execute command sequence operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn execute_command_sequence(
    session: &mut Session,
    primary_client_id: &ClientId,
    input: &str,
) -> Result<Vec<CommandOutcome>> {
    let invocations = parse_command_sequence(input)?;
    let mut outcomes = Vec::with_capacity(invocations.len());

    for invocation in &invocations {
        outcomes.push(execute_command(session, primary_client_id, invocation)?);
    }

    Ok(outcomes)
}

/// Runs the execute auth command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn execute_auth_command(
    auth_store: &AuthStore,
    invocation: &CommandInvocation,
) -> Result<CommandOutcome> {
    match invocation.name.as_str() {
        "auth-status" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: auth_status_store_display(auth_store.status()?),
        }),
        "mcp-status" => {
            let server_id = mcp_server_id(invocation, "mcp-status requires a server id")?;
            Ok(CommandOutcome::Display {
                command: invocation.name.clone(),
                body: mcp_status_store_display(auth_store.mcp_status(server_id, None, None)?),
            })
        }
        _ => Err(MezError::invalid_args(format!(
            "command `{}` is not an auth command",
            invocation.name
        ))),
    }
}

/// Runs the select pane target or alias operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn select_pane_target_or_alias(
    session: &mut Session,
    primary_client_id: &ClientId,
    target: &str,
) -> Result<()> {
    match target {
        "next" => {
            session.select_adjacent_pane(primary_client_id, PaneNavigationDirection::Right)?;
        }
        "previous" | "prev" => {
            session.select_adjacent_pane(primary_client_id, PaneNavigationDirection::Left)?;
        }
        "last" => {
            session.select_last_pane(primary_client_id)?;
        }
        _ => {
            session.select_pane(primary_client_id, target)?;
        }
    }
    Ok(())
}

/// Runs the swap pane neighbor target operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn swap_pane_neighbor_target(
    session: &Session,
    neighbor: SwapPaneNeighbor,
) -> Result<Option<String>> {
    let window = session
        .active_window()
        .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
    if window.panes().len() < 2 {
        return Ok(None);
    }
    let active = window.active_pane_index();
    let target = match neighbor {
        SwapPaneNeighbor::Previous => {
            if active == 0 {
                window.panes().len() - 1
            } else {
                active - 1
            }
        }
        SwapPaneNeighbor::Next => (active + 1) % window.panes().len(),
    };
    Ok(Some(target.to_string()))
}

/// Runs the move window target index operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// Runs the execute config store command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn execute_config_store_command(
    paths: &ConfigPaths,
    invocation: &CommandInvocation,
) -> Result<CommandOutcome> {
    match invocation.name.as_str() {
        "set-option" => {
            let (path, value) = set_option_args(invocation)?;
            let plan = persist_command_config_mutation(
                paths,
                ConfigMutation {
                    path: path.to_string(),
                    operation: ConfigMutationOperation::Set(parse_config_command_value(value)),
                },
            )?;
            Ok(CommandOutcome::Display {
                command: invocation.name.clone(),
                body: format!(
                    "path={path}:changed={}:reload_required={}:source=config-store",
                    plan.changed, plan.reload_required
                ),
            })
        }
        "set-theme" => {
            let theme = set_theme_arg(invocation)?;
            let plan = persist_command_theme_config(paths, theme)?;
            Ok(CommandOutcome::Display {
                command: invocation.name.clone(),
                body: format!(
                    "theme={theme}:changed={}:reload_required={}:source=config-store:aliases={}:color_slots={}",
                    plan.changed, plan.reload_required, plan.alias_count, plan.color_slot_count
                ),
            })
        }
        "bind-key" => {
            let (key, command) = bind_key_args(invocation)?;
            let chord = KeyChord::parse(key)?;
            let notation = key_chord_notation(chord);
            let config_key = binding_config_key(&notation);
            let plan = persist_command_config_mutation(
                paths,
                config_set_string(format!("keys.command_bindings.{config_key}"), &command),
            )?;
            Ok(CommandOutcome::Display {
                command: invocation.name.clone(),
                body: format!(
                    "key={notation}:config_key={config_key}:command={}:changed={}:reload_required={}:source=config-store",
                    command, plan.changed, plan.reload_required
                ),
            })
        }
        "unbind-key" => {
            let key = positional_args(invocation)
                .first()
                .copied()
                .ok_or_else(|| MezError::invalid_args("unbind-key requires a key"))?;
            let chord = KeyChord::parse(key)?;
            let notation = key_chord_notation(chord);
            let config_key = binding_config_key(&notation);
            let plan = persist_command_config_mutation(
                paths,
                config_unset(format!("keys.command_bindings.{config_key}")),
            )?;
            Ok(CommandOutcome::Display {
                command: invocation.name.clone(),
                body: format!(
                    "key={notation}:config_key={config_key}:removed=true:changed={}:reload_required={}:source=config-store",
                    plan.changed, plan.reload_required
                ),
            })
        }
        "mcp" => {
            let command = mcp_config_command_from_words(&invocation.args)?;
            let plans = persist_mcp_config_command(paths, &command)?;
            let report = mcp_config_command_report(&plans);
            Ok(CommandOutcome::Display {
                command: invocation.name.clone(),
                body: mcp_config_command_display(&command, report),
            })
        }
        "source-file" => {
            let path = positional_args(invocation)
                .first()
                .copied()
                .ok_or_else(|| MezError::invalid_args("source-file requires a path"))?;
            let scope = if invocation.has_flag("--project", "--project") {
                ConfigScope::ProjectOverlay
            } else {
                ConfigScope::Primary
            };
            let source_path = PathBuf::from(path);
            let validation = validate_config_file(&source_path, scope)?;
            let target_path = match scope {
                ConfigScope::ProjectOverlay => source_path.clone(),
                ConfigScope::Primary => paths
                    .select_primary_file()?
                    .unwrap_or_else(|| paths.default_primary_file()),
                ConfigScope::LiveOverride => {
                    return Err(MezError::config(
                        "source-file cannot persist live override scope through config store",
                    ));
                }
            };
            let source_text = fs::read_to_string(&source_path)?;
            let previous_text = fs::read_to_string(&target_path).ok();
            let changed = previous_text.as_deref() != Some(source_text.as_str());
            persist_config_text(&target_path, scope, &source_text)?;
            Ok(CommandOutcome::Display {
                command: invocation.name.clone(),
                body: format!(
                    "path={path}:valid={}:diagnostics={}:applied=true:changed={changed}:reload_required={changed}:target={}:source=config-store",
                    validation.valid,
                    validation.diagnostics.len(),
                    target_path.display()
                ),
            })
        }
        _ => Err(MezError::invalid_args(format!(
            "command `{}` is not a config store command",
            invocation.name
        ))),
    }
}

/// Runs the execute mark pane ready command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn execute_mark_pane_ready_command(
    session: &Session,
    primary_client_id: &ClientId,
    store: &mut PaneReadinessOverrideStore,
    invocation: &CommandInvocation,
    current_state: PaneReadinessState,
    current_epoch: u64,
    audit_log: Option<&mut AuditLog>,
) -> Result<CommandOutcome> {
    if invocation.name != "mark-pane-ready" {
        return Err(MezError::invalid_args(format!(
            "command `{}` is not a pane readiness command",
            invocation.name
        )));
    }

    session.require_primary(primary_client_id)?;
    let pane_id = command_target_pane_id(session, invocation)?;
    let reason = flag_value(&invocation.args, "--reason")
        .unwrap_or("primary accepted uncertain shell boundary")
        .to_string();
    let acknowledgement = invocation
        .args
        .iter()
        .any(|arg| matches!(arg.as_str(), "--acknowledge-risk" | "--ack"));

    if !acknowledgement {
        return Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: mark_pane_ready_warning_display(&pane_id, current_state),
        });
    }

    let pending_probe_cleared = store.has_pending_probe(&pane_id);
    store.mark_ready_for_epoch(&pane_id, current_epoch, &reason, true)?;
    let audit_written = audit_log.is_some();
    if let Some(audit_log) = audit_log {
        audit_log.append(mark_pane_ready_audit_record(
            session,
            primary_client_id,
            &pane_id,
            current_state,
            current_epoch,
            &reason,
        ))?;
    }

    Ok(CommandOutcome::Display {
        command: invocation.name.clone(),
        body: format!(
            "pane={pane_id}:readiness_state={}:override=applied:epoch={current_epoch}:pending_probe_cleared={pending_probe_cleared}:audit={}:source=readiness-store",
            pane_readiness_state_name(current_state),
            if audit_written {
                "written"
            } else {
                "not-configured"
            }
        ),
    })
}

/// Executes a typed session-mutation command plan when the invocation maps to one.
///
/// The return value is `Ok(None)` for display-only or store-backed commands that
/// still use the lightweight fallback dispatcher.
fn execute_command_plan(
    session: &mut Session,
    primary_client_id: &ClientId,
    invocation: &CommandInvocation,
) -> Result<Option<CommandOutcome>> {
    let outcome = match command_plan_from_invocation(invocation)? {
        CommandPlan::Fallback => return Ok(None),
        CommandPlan::NewWindow(plan) => {
            session.new_window(primary_client_id, plan.name, plan.select)?;
            mutated_pane_command_outcome(invocation, plan.shell_command, plan.start_directory)
        }
        CommandPlan::NewGroup(plan) => {
            session.new_group(primary_client_id, plan.name, plan.select)?;
            mutated_pane_command_outcome(invocation, plan.shell_command, plan.start_directory)
        }
        CommandPlan::RenameGroup(plan) => {
            session.rename_group(primary_client_id, plan.target.as_deref(), plan.name)?;
            CommandOutcome::Mutated {
                command: plan.command,
            }
        }
        CommandPlan::SelectGroup(plan) => {
            session.select_group(primary_client_id, &plan.target)?;
            CommandOutcome::Mutated {
                command: plan.command,
            }
        }
        CommandPlan::NextGroup { command } => {
            session.next_group(primary_client_id)?;
            CommandOutcome::Mutated { command }
        }
        CommandPlan::PreviousGroup { command } => {
            session.previous_group(primary_client_id)?;
            CommandOutcome::Mutated { command }
        }
        CommandPlan::LastGroup { command } => {
            session.last_group(primary_client_id)?;
            CommandOutcome::Mutated { command }
        }
        CommandPlan::KillGroup(plan) => {
            session.kill_group(primary_client_id, plan.target.as_deref(), plan.force)?;
            CommandOutcome::Mutated {
                command: plan.command,
            }
        }
        CommandPlan::RenameWindow(plan) => {
            session.rename_window(primary_client_id, plan.target.as_deref(), plan.name)?;
            CommandOutcome::Mutated {
                command: plan.command,
            }
        }
        CommandPlan::SelectWindow(plan) => {
            session.select_window(primary_client_id, &plan.target)?;
            CommandOutcome::Mutated {
                command: plan.command,
            }
        }
        CommandPlan::NextWindow { command } => {
            session.next_window(primary_client_id)?;
            CommandOutcome::Mutated { command }
        }
        CommandPlan::PreviousWindow { command } => {
            session.previous_window(primary_client_id)?;
            CommandOutcome::Mutated { command }
        }
        CommandPlan::LastWindow { command } => {
            session.last_window(primary_client_id)?;
            CommandOutcome::Mutated { command }
        }
        CommandPlan::MoveWindow(plan) => {
            session.move_window(primary_client_id, plan.source.as_deref(), plan.target_index)?;
            CommandOutcome::Mutated {
                command: plan.command,
            }
        }
        CommandPlan::KillWindow(plan) => {
            session.kill_window(primary_client_id, plan.target.as_deref(), plan.force)?;
            CommandOutcome::Mutated {
                command: plan.command,
            }
        }
        CommandPlan::SplitWindow(plan) => {
            session.split_active_pane_select(primary_client_id, plan.direction, plan.select_new)?;
            mutated_pane_command_outcome(invocation, plan.shell_command, plan.start_directory)
        }
        CommandPlan::SelectPane(plan) => {
            match plan.selection {
                PaneSelectionPlan::Target(target) => {
                    select_pane_target_or_alias(session, primary_client_id, &target)?;
                }
                PaneSelectionPlan::Direction(direction) => {
                    session.select_adjacent_pane(primary_client_id, direction)?;
                }
            }
            CommandOutcome::Mutated {
                command: plan.command,
            }
        }
        CommandPlan::NextPane { command } => {
            session.select_adjacent_pane(primary_client_id, PaneNavigationDirection::Right)?;
            CommandOutcome::Mutated { command }
        }
        CommandPlan::PreviousPane { command } => {
            session.select_adjacent_pane(primary_client_id, PaneNavigationDirection::Left)?;
            CommandOutcome::Mutated { command }
        }
        CommandPlan::LastPane { command } => {
            session.select_last_pane(primary_client_id)?;
            CommandOutcome::Mutated { command }
        }
        CommandPlan::RotatePane(plan) => {
            session.rotate_panes(primary_client_id, plan.reverse)?;
            CommandOutcome::Mutated {
                command: plan.command,
            }
        }
        CommandPlan::SelectLayout(plan) => {
            let policy = session.select_layout(primary_client_id, &plan.layout_name)?;
            CommandOutcome::Display {
                command: plan.command,
                body: format!("layout={}", policy.name()),
            }
        }
        CommandPlan::NextLayout { command } => {
            let policy = session.cycle_layout(primary_client_id)?;
            CommandOutcome::Display {
                command,
                body: format!("layout={}", policy.name()),
            }
        }
        CommandPlan::RebalanceWindow { command } => {
            let policy = session.rebalance_window(primary_client_id)?;
            CommandOutcome::Display {
                command,
                body: format!("layout={}", policy.name()),
            }
        }
        CommandPlan::SynchronizePanes(plan) => {
            let enabled = match plan.mode {
                SynchronizePanesMode::On => {
                    session.set_active_window_panes_synchronized(primary_client_id, true)?
                }
                SynchronizePanesMode::Off => {
                    session.set_active_window_panes_synchronized(primary_client_id, false)?
                }
                SynchronizePanesMode::Toggle => {
                    session.toggle_active_window_panes_synchronized(primary_client_id)?
                }
                SynchronizePanesMode::Status => session.active_window_panes_synchronized(),
            };
            CommandOutcome::Display {
                command: plan.command,
                body: format!("synchronize-panes={}", if enabled { "on" } else { "off" }),
            }
        }
        CommandPlan::ZoomPane { command } => {
            let zoomed = session.toggle_active_pane_zoom(primary_client_id)?;
            zoom_command_outcome(command, zoomed)
        }
        CommandPlan::ResizePane(ResizePanePlan::Zoom { command }) => {
            let zoomed = session.toggle_active_pane_zoom(primary_client_id)?;
            zoom_command_outcome(command, zoomed)
        }
        CommandPlan::ResizePane(ResizePanePlan::Resize {
            command,
            target,
            spec,
        }) => {
            session.resize_pane_with_spec(primary_client_id, target.as_deref(), spec)?;
            CommandOutcome::Mutated { command }
        }
        CommandPlan::KillPane(plan) => {
            session.kill_pane(primary_client_id, plan.target.as_deref(), plan.force)?;
            CommandOutcome::Mutated {
                command: plan.command,
            }
        }
        CommandPlan::SwapPane(SwapPanePlan::Target {
            command,
            source,
            target,
        }) => {
            session.swap_panes(primary_client_id, source.as_deref(), &target)?;
            CommandOutcome::Mutated { command }
        }
        CommandPlan::SwapPane(SwapPanePlan::Neighbor { command, neighbor }) => {
            let Some(target) = swap_pane_neighbor_target(session, neighbor)? else {
                return Ok(Some(CommandOutcome::Noop { command }));
            };
            session.swap_panes(primary_client_id, None, &target)?;
            CommandOutcome::Mutated { command }
        }
        CommandPlan::BreakPane(plan) => {
            session.break_pane(
                primary_client_id,
                plan.target.as_deref(),
                plan.name,
                plan.select,
            )?;
            CommandOutcome::Mutated {
                command: plan.command,
            }
        }
        CommandPlan::JoinPane(plan) => {
            session.join_pane(
                primary_client_id,
                plan.source.as_deref(),
                &plan.target,
                plan.direction,
                plan.select,
            )?;
            CommandOutcome::Mutated {
                command: plan.command,
            }
        }
        CommandPlan::ApproveObserver(plan) => {
            session.approve_observer_target(primary_client_id, &plan.target)?;
            CommandOutcome::Mutated {
                command: plan.command,
            }
        }
        CommandPlan::RejectObserver(plan) => {
            session.reject_observer_target(primary_client_id, &plan.target)?;
            CommandOutcome::Mutated {
                command: plan.command,
            }
        }
        CommandPlan::RevokeObserver(plan) => {
            session.revoke_observer_client(primary_client_id, &plan.target)?;
            CommandOutcome::Mutated {
                command: plan.command,
            }
        }
        CommandPlan::RenameSession(plan) => {
            session.rename_session(primary_client_id, plan.name)?;
            CommandOutcome::Mutated {
                command: plan.command,
            }
        }
        CommandPlan::KillSession(plan) => {
            session.kill_session(primary_client_id, plan.force)?;
            CommandOutcome::Mutated {
                command: plan.command,
            }
        }
        CommandPlan::DetachClient(plan) => {
            let target = plan.target.as_deref().unwrap_or(primary_client_id.as_str());
            session.detach_client_target(primary_client_id, target)?;
            CommandOutcome::Mutated {
                command: plan.command,
            }
        }
    };
    Ok(Some(outcome))
}

/// Formats the common pane-zoom command response.
fn zoom_command_outcome(command: String, zoomed: Option<mez_core::ids::PaneId>) -> CommandOutcome {
    CommandOutcome::Display {
        command,
        body: format!(
            "zoomed={}",
            zoomed
                .map(|pane_id| pane_id.to_string())
                .unwrap_or_else(|| "none".to_string())
        ),
    }
}

/// Runs the execute command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn execute_command(
    session: &mut Session,
    primary_client_id: &ClientId,
    invocation: &CommandInvocation,
) -> Result<CommandOutcome> {
    if let Some(outcome) = execute_command_plan(session, primary_client_id, invocation)? {
        return Ok(outcome);
    }

    match invocation.name.as_str() {
        "display-panes" | "displayp" => {
            let active = session
                .active_window()
                .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
            let mut body = String::new();
            for pane in active.panes() {
                body.push_str(&format!(
                    "{}:{}:action=select-pane -t {}\n",
                    pane.index, pane.id, pane.index
                ));
            }
            Ok(CommandOutcome::Display {
                command: invocation.name.clone(),
                body,
            })
        }
        "list-groups" | "listg" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: list_groups(session),
        }),
        "choose-group" | "chooseg" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: choose_group_display(session),
        }),
        "list-windows" | "listw" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: list_windows(session),
        }),
        "list-panes" | "listp" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: list_panes(session)?,
        }),
        "list-clients" | "listc" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: list_clients(session),
        }),
        "choose-client" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: choose_client_display(session),
        }),
        "choose-window" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: choose_window_display(session),
        }),
        "list-observers" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: list_observers(session),
        }),
        "choose-observer" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: choose_observer_display(session),
        }),
        "list-sessions" | "lists" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: list_current_session(session),
        }),
        "attach-session" | "attach" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: attach_session_display(session),
        }),
        "help" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: command_help_display(),
        }),
        "list-commands" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: list_baseline_commands(),
        }),
        "list-themes" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: list_default_themes(),
        }),
        "send-prefix" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: "sent=false:reason=live-terminal-state-unavailable".to_string(),
        }),
        "copy-mode" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: copy_mode_display(invocation),
        }),
        "copy-selection" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: copy_selection_display(invocation),
        }),
        "paste-clipboard" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: paste_clipboard_display(invocation),
        }),
        "paste-buffer" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: paste_buffer_display(invocation),
        }),
        "create-buffer" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: create_buffer_display(invocation),
        }),
        "list-buffers" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: list_buffers_display(),
        }),
        "choose-buffer" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: choose_buffer_display(),
        }),
        "delete-buffer" => Err(MezError::new(
            crate::error::MezErrorKind::NotFound,
            "paste buffer not found",
        )),
        "capture-pane" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: capture_pane_display(invocation),
        }),
        "save-buffer" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: save_buffer_display(invocation),
        }),
        "clear-history" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: clear_history_display(invocation),
        }),
        "search-history" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: search_history_display(invocation),
        }),
        "export-history" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: export_history_display(invocation),
        }),
        "pipe-pane" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: pipe_pane_display(invocation),
        }),
        "save-layout" => Ok(CommandOutcome::LayoutSave {
            command: invocation.name.clone(),
            name: save_layout_name(invocation),
        }),
        "load-layout" => Ok(CommandOutcome::LayoutLoad {
            command: invocation.name.clone(),
            selector: load_layout_selector(invocation),
        }),
        "show-messages" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: show_messages_display(),
        }),
        "show-metrics" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: show_metrics_display(),
        }),
        "list-keys" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: list_default_key_bindings(),
        }),
        "bind-key" => {
            let (key, command) = bind_key_args(invocation)?;
            let chord = KeyChord::parse(key)?;
            Ok(CommandOutcome::Display {
                command: invocation.name.clone(),
                body: format!(
                    "key={}:command={command}:changed=false:reason=live-config-control-unavailable",
                    key_chord_notation(chord)
                ),
            })
        }
        "unbind-key" => {
            let key = positional_args(invocation)
                .first()
                .copied()
                .ok_or_else(|| MezError::invalid_args("unbind-key requires a key"))?;
            let chord = KeyChord::parse(key)?;
            Ok(CommandOutcome::Display {
                command: invocation.name.clone(),
                body: format!(
                    "key={}:removed=false:reason=live-config-control-unavailable",
                    key_chord_notation(chord)
                ),
            })
        }
        "show-options" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: show_default_options(),
        }),
        "set-option" => {
            let (path, value) = set_option_args(invocation)?;
            Ok(CommandOutcome::Display {
                command: invocation.name.clone(),
                body: format!(
                    "path={path}:value={value}:changed=false:reason=live-config-control-unavailable"
                ),
            })
        }
        "set-theme" => {
            let theme = set_theme_arg(invocation)?;
            Ok(CommandOutcome::Display {
                command: invocation.name.clone(),
                body: format!("theme={theme}:changed=false:reason=live-config-control-unavailable"),
            })
        }
        "source-file" => {
            let path = positional_args(invocation)
                .first()
                .copied()
                .ok_or_else(|| MezError::invalid_args("source-file requires a path"))?;
            Ok(CommandOutcome::Display {
                command: invocation.name.clone(),
                body: format!("path={path}:applied=false:reason=live-config-control-unavailable"),
            })
        }
        "refresh-client" | "refresh" => Ok(CommandOutcome::Noop {
            command: invocation.name.clone(),
        }),
        "refresh-provider-info" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: "refreshed=false:reason=async-runtime-required".to_string(),
        }),
        "agent-shell" => Ok(CommandOutcome::Noop {
            command: invocation.name.clone(),
        }),
        "auth-status" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: auth_status_display(),
        }),
        "mcp-status" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: mcp_status_plan_display(invocation)?,
        }),
        "mark-pane-ready" => {
            session.require_primary(primary_client_id)?;
            let pane_id = command_target_pane_id(session, invocation)?;
            Ok(CommandOutcome::Display {
                command: invocation.name.clone(),
                body: format!(
                    "{}:source=not-connected",
                    mark_pane_ready_warning_display(&pane_id, PaneReadinessState::Unknown)
                ),
            })
        }
        _ => Err(MezError::invalid_args(format!(
            "unknown command `{}`",
            invocation.name
        ))),
    }
}
