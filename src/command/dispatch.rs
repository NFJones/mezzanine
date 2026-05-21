//! Command Dispatch implementation.
//!
//! This module owns the command dispatch boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AuditLog, AuthStore, ClientId, CommandInvocation, CommandOutcome, ConfigMutation,
    ConfigMutationOperation, ConfigPaths, ConfigScope, KeyChord, MezError, PaneNavigationDirection,
    PaneReadinessOverrideStore, PaneReadinessState, PaneSizeSpec, PathBuf, ResizeAxis,
    ResizeDirection, Result, Session, SplitDirection, attach_session_display,
    auth_login_plan_display, auth_status_display, auth_status_store_display, bind_key_args,
    binding_config_key, capture_pane_display, choose_buffer_display, choose_client_display,
    choose_group_display, choose_observer_display, choose_window_display, clear_history_display,
    command_help_display, command_target_pane_id, config_set_string, config_unset,
    copy_mode_display, copy_selection_display, create_buffer_display, execute_auth_login,
    export_history_display, flag_value, key_chord_notation, list_baseline_commands,
    list_buffers_display, list_clients, list_current_session, list_default_key_bindings,
    list_default_themes, list_groups, list_observers, list_panes, list_windows,
    mark_pane_ready_audit_record, mark_pane_ready_warning_display, mcp_add_plan_display,
    mcp_remove_plan_display, mcp_retry_plan_display, mcp_server_id, mutated_pane_command_outcome,
    mutation_plans_changed, mutation_plans_reload_required, new_window_name,
    new_window_shell_command, pane_readiness_state_name, parse_command_sequence,
    parse_config_command_value, paste_buffer_display, paste_clipboard_display,
    persist_command_config_mutation, persist_command_theme_config, persist_mcp_add,
    persist_mcp_remove, pipe_pane_display, positional_args, resume_session_display,
    save_buffer_display, search_history_display, set_option_args, set_theme_arg,
    show_default_options, show_messages_display, snapshot_session_display,
    split_window_shell_command, validate_config_file,
};

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
        "auth-login" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: execute_auth_login(auth_store, invocation)?,
        }),
        _ => Err(MezError::invalid_args(format!(
            "command `{}` is not an auth command",
            invocation.name
        ))),
    }
}

/// Runs the resize spec from invocation operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn resize_spec_from_invocation(invocation: &CommandInvocation) -> Result<PaneSizeSpec> {
    if let Some(percent) = flag_value(&invocation.args, "--percent") {
        let percent = parse_resize_amount(percent, "resize-pane percent is invalid")?;
        let axis = match flag_value(&invocation.args, "--axis").unwrap_or("both") {
            "columns" | "horizontal" => ResizeAxis::Columns,
            "rows" | "vertical" => ResizeAxis::Rows,
            "both" => ResizeAxis::Both,
            _ => return Err(MezError::invalid_args("resize-pane axis is invalid")),
        };
        return Ok(PaneSizeSpec::Percent { percent, axis });
    }
    if let Some(direction) = flag_value(&invocation.args, "--delta") {
        let direction = ResizeDirection::from_name(direction)
            .ok_or_else(|| MezError::invalid_args("resize-pane delta direction is invalid"))?;
        return Ok(PaneSizeSpec::Delta {
            direction,
            amount: resize_amount_flag(invocation)?,
        });
    }
    if let Some(edge) = flag_value(&invocation.args, "--edge") {
        let edge = ResizeDirection::from_name(edge)
            .ok_or_else(|| MezError::invalid_args("resize-pane edge is invalid"))?;
        return Ok(PaneSizeSpec::Edge {
            edge,
            amount: resize_amount_flag(invocation)?,
        });
    }
    for (flag, direction) in [
        ("-L", ResizeDirection::Left),
        ("-R", ResizeDirection::Right),
        ("-U", ResizeDirection::Up),
        ("-D", ResizeDirection::Down),
    ] {
        if invocation.args.iter().any(|arg| arg == flag) {
            return Ok(PaneSizeSpec::Delta {
                direction,
                amount: optional_flag_amount(&invocation.args, flag)?,
            });
        }
    }

    let columns = flag_value(&invocation.args, "-x")
        .or_else(|| flag_value(&invocation.args, "--columns"))
        .map(|value| parse_resize_amount(value, "resize-pane columns are invalid"))
        .transpose()?;
    let rows = flag_value(&invocation.args, "-y")
        .or_else(|| flag_value(&invocation.args, "--rows"))
        .map(|value| parse_resize_amount(value, "resize-pane rows are invalid"))
        .transpose()?;
    if columns.is_none() && rows.is_none() {
        return Err(MezError::invalid_args(
            "resize-pane requires a size, percent, delta, or edge",
        ));
    }
    Ok(PaneSizeSpec::Cells { columns, rows })
}

/// Runs the resize amount flag operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn resize_amount_flag(invocation: &CommandInvocation) -> Result<u16> {
    flag_value(&invocation.args, "--amount")
        .map(|value| parse_resize_amount(value, "resize-pane amount is invalid"))
        .transpose()?
        .ok_or_else(|| MezError::invalid_args("resize-pane requires --amount"))
}

/// Runs the optional flag amount operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_flag_amount(args: &[String], flag: &str) -> Result<u16> {
    let Some(index) = args.iter().position(|arg| arg == flag) else {
        return Ok(1);
    };
    let Some(value) = args.get(index.saturating_add(1)) else {
        return Ok(1);
    };
    if value.starts_with('-') {
        return Ok(1);
    }
    parse_resize_amount(value, "resize-pane amount is invalid")
}

/// Runs the parse resize amount operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_resize_amount(value: &str, message: &'static str) -> Result<u16> {
    value
        .parse::<u16>()
        .map_err(|_| MezError::invalid_args(message))
}

/// Runs the select pane direction operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn select_pane_direction(
    invocation: &CommandInvocation,
) -> Result<Option<PaneNavigationDirection>> {
    let mut matched = [
        ("-U", PaneNavigationDirection::Up),
        ("--up", PaneNavigationDirection::Up),
        ("-D", PaneNavigationDirection::Down),
        ("--down", PaneNavigationDirection::Down),
        ("-L", PaneNavigationDirection::Left),
        ("--left", PaneNavigationDirection::Left),
        ("-R", PaneNavigationDirection::Right),
        ("--right", PaneNavigationDirection::Right),
    ]
    .into_iter()
    .filter_map(|(flag, direction)| {
        invocation
            .args
            .iter()
            .any(|arg| arg == flag)
            .then_some(direction)
    });
    let direction = matched.next();
    if matched.next().is_some() {
        return Err(MezError::invalid_args(
            "select-pane accepts only one direction flag",
        ));
    }
    Ok(direction)
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

/// Carries Swap Pane Neighbor state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SwapPaneNeighbor {
    /// Represents the Previous case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Previous,
    /// Represents the Next case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Next,
}

/// Runs the swap pane neighbor operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn swap_pane_neighbor(invocation: &CommandInvocation) -> Result<Option<SwapPaneNeighbor>> {
    let mut matched = [
        ("-U", SwapPaneNeighbor::Previous),
        ("--up", SwapPaneNeighbor::Previous),
        ("-D", SwapPaneNeighbor::Next),
        ("--down", SwapPaneNeighbor::Next),
    ]
    .into_iter()
    .filter_map(|(flag, neighbor)| {
        invocation
            .args
            .iter()
            .any(|arg| arg == flag)
            .then_some(neighbor)
    });
    let neighbor = matched.next();
    if matched.next().is_some() {
        return Err(MezError::invalid_args(
            "swap-pane accepts only one direction flag",
        ));
    }
    Ok(neighbor)
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
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn move_window_target_index(invocation: &CommandInvocation) -> Result<usize> {
    let target = invocation
        .target_arg()
        .or_else(|| positional_args(invocation).first().copied())
        .ok_or_else(|| MezError::invalid_args("move-window requires a target index"))?;
    target
        .parse::<usize>()
        .map_err(|_| MezError::invalid_args("move-window target must be a window index"))
}

/// Runs the execute mcp config command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn execute_mcp_config_command(
    paths: &ConfigPaths,
    invocation: &CommandInvocation,
) -> Result<CommandOutcome> {
    match invocation.name.as_str() {
        "mcp-add" => {
            let (server_id, transport, target, plans) = persist_mcp_add(paths, invocation)?;
            Ok(CommandOutcome::Display {
                command: invocation.name.clone(),
                body: format!(
                    "server={server_id}:transport={transport}:target={target}:changed={}:reload_required={}:source=config-store",
                    mutation_plans_changed(&plans),
                    mutation_plans_reload_required(&plans)
                ),
            })
        }
        "mcp-remove" => {
            let server_id = mcp_server_id(invocation, "mcp-remove requires a server id")?;
            let plans = persist_mcp_remove(paths, server_id)?;
            Ok(CommandOutcome::Display {
                command: invocation.name.clone(),
                body: format!(
                    "server={server_id}:removed=true:changed={}:reload_required={}:source=config-store",
                    mutation_plans_changed(&plans),
                    mutation_plans_reload_required(&plans)
                ),
            })
        }
        _ => Err(MezError::invalid_args(format!(
            "command `{}` is not an MCP configuration command",
            invocation.name
        ))),
    }
}

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
            let validation = validate_config_file(&PathBuf::from(path), scope)?;
            Ok(CommandOutcome::Display {
                command: invocation.name.clone(),
                body: format!(
                    "path={path}:valid={}:diagnostics={}:applied=false:reload_required={}:source=config-store",
                    validation.valid,
                    validation.diagnostics.len(),
                    validation.valid
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
    match invocation.name.as_str() {
        "new-window" | "neww" => {
            let name = new_window_name(invocation);
            let shell_command = new_window_shell_command(invocation)?;
            let start_directory = invocation.start_directory_arg().map(ToOwned::to_owned);
            let select = !invocation.has_flag("-d", "--detached");
            session.new_window(primary_client_id, name, select)?;
            Ok(mutated_pane_command_outcome(
                invocation,
                shell_command,
                start_directory,
            ))
        }
        "new-group" | "newg" => {
            let name = new_window_name(invocation);
            let shell_command = new_window_shell_command(invocation)?;
            let start_directory = invocation.start_directory_arg().map(ToOwned::to_owned);
            let select = !invocation.has_flag("-d", "--detached");
            session.new_group(primary_client_id, name, select)?;
            Ok(mutated_pane_command_outcome(
                invocation,
                shell_command,
                start_directory,
            ))
        }
        "rename-group" | "renameg" => {
            let name = positional_args(invocation).join(" ");
            if name.is_empty() {
                return Err(MezError::invalid_args("rename-group requires a name"));
            }
            session.rename_group(primary_client_id, invocation.target_arg(), name)?;
            Ok(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            })
        }
        "select-group" | "selectg" => {
            let target = invocation
                .target_arg()
                .or_else(|| positional_args(invocation).first().copied())
                .ok_or_else(|| MezError::invalid_args("select-group requires a target"))?;
            session.select_group(primary_client_id, target)?;
            Ok(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            })
        }
        "next-group" | "nextg" => {
            session.next_group(primary_client_id)?;
            Ok(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            })
        }
        "previous-group" | "prevg" => {
            session.previous_group(primary_client_id)?;
            Ok(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            })
        }
        "last-group" | "lastg" => {
            session.last_group(primary_client_id)?;
            Ok(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            })
        }
        "kill-group" | "killg" => {
            let force = invocation.has_flag("-f", "--force");
            session.kill_group(primary_client_id, invocation.target_arg(), force)?;
            Ok(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            })
        }
        "rename-window" | "renamew" => {
            let name = positional_args(invocation).join(" ");
            if name.is_empty() {
                return Err(MezError::invalid_args("rename-window requires a name"));
            }
            session.rename_window(primary_client_id, invocation.target_arg(), name)?;
            Ok(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            })
        }
        "select-window" | "selectw" => {
            let target = invocation
                .target_arg()
                .or_else(|| positional_args(invocation).first().copied())
                .ok_or_else(|| MezError::invalid_args("select-window requires a target"))?;
            session.select_window(primary_client_id, target)?;
            Ok(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            })
        }
        "next-window" | "next" | "nextw" => {
            session.next_window(primary_client_id)?;
            Ok(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            })
        }
        "previous-window" | "previous" | "prev" | "prevw" => {
            session.previous_window(primary_client_id)?;
            Ok(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            })
        }
        "last-window" | "lastw" => {
            session.last_window(primary_client_id)?;
            Ok(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            })
        }
        "move-window" | "movew" => {
            let target_index = move_window_target_index(invocation)?;
            session.move_window(primary_client_id, invocation.source_arg(), target_index)?;
            Ok(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            })
        }
        "kill-window" | "killw" => {
            let force = invocation.has_flag("-f", "--force");
            session.kill_window(primary_client_id, invocation.target_arg(), force)?;
            Ok(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            })
        }
        "split-window" | "splitw" => {
            let direction = if invocation.has_flag("-h", "--horizontal") {
                SplitDirection::Horizontal
            } else {
                SplitDirection::Vertical
            };
            let shell_command = split_window_shell_command(invocation)?;
            let start_directory = invocation.start_directory_arg().map(ToOwned::to_owned);
            let select_new = split_window_selects_new_pane(invocation)?;
            session.split_active_pane_select(primary_client_id, direction, select_new)?;
            Ok(mutated_pane_command_outcome(
                invocation,
                shell_command,
                start_directory,
            ))
        }
        "select-pane" | "selectp" => {
            if let Some(target) = invocation
                .target_arg()
                .or_else(|| positional_args(invocation).first().copied())
            {
                select_pane_target_or_alias(session, primary_client_id, target)?;
            } else if let Some(direction) = select_pane_direction(invocation)? {
                session.select_adjacent_pane(primary_client_id, direction)?;
            } else {
                return Err(MezError::invalid_args("select-pane requires a target"));
            }
            Ok(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            })
        }
        "next-pane" | "nextp" => {
            session.select_adjacent_pane(primary_client_id, PaneNavigationDirection::Right)?;
            Ok(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            })
        }
        "previous-pane" | "prev-pane" | "prevp" => {
            session.select_adjacent_pane(primary_client_id, PaneNavigationDirection::Left)?;
            Ok(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            })
        }
        "last-pane" | "lastp" => {
            session.select_last_pane(primary_client_id)?;
            Ok(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            })
        }
        "rotate-pane" | "rotatep" => {
            session.rotate_panes(primary_client_id, invocation.has_flag("-D", "--reverse"))?;
            Ok(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            })
        }
        "select-layout" => {
            let layout_name = positional_args(invocation)
                .first()
                .copied()
                .ok_or_else(|| MezError::invalid_args("select-layout requires a layout"))?;
            let policy = session.select_layout(primary_client_id, layout_name)?;
            Ok(CommandOutcome::Display {
                command: invocation.name.clone(),
                body: format!("layout={}", policy.name()),
            })
        }
        "next-layout" => {
            let policy = session.cycle_layout(primary_client_id)?;
            Ok(CommandOutcome::Display {
                command: invocation.name.clone(),
                body: format!("layout={}", policy.name()),
            })
        }
        "rebalance-window" => {
            let policy = session.rebalance_window(primary_client_id)?;
            Ok(CommandOutcome::Display {
                command: invocation.name.clone(),
                body: format!("layout={}", policy.name()),
            })
        }
        "zoom-pane" => {
            let zoomed = session.toggle_active_pane_zoom(primary_client_id)?;
            Ok(CommandOutcome::Display {
                command: invocation.name.clone(),
                body: format!(
                    "zoomed={}",
                    zoomed
                        .map(|pane_id| pane_id.to_string())
                        .unwrap_or_else(|| "none".to_string())
                ),
            })
        }
        "resize-pane" | "resizep" => {
            if invocation.has_flag("-Z", "--zoom") {
                let zoomed = session.toggle_active_pane_zoom(primary_client_id)?;
                return Ok(CommandOutcome::Display {
                    command: invocation.name.clone(),
                    body: format!(
                        "zoomed={}",
                        zoomed
                            .map(|pane_id| pane_id.to_string())
                            .unwrap_or_else(|| "none".to_string())
                    ),
                });
            }
            let spec = resize_spec_from_invocation(invocation)?;
            session.resize_pane_with_spec(primary_client_id, invocation.target_arg(), spec)?;
            Ok(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            })
        }
        "kill-pane" | "killp" => {
            let force = invocation.has_flag("-f", "--force");
            session.kill_pane(primary_client_id, invocation.target_arg(), force)?;
            Ok(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            })
        }
        "swap-pane" | "swapp" => {
            if let Some(target) = invocation
                .target_arg()
                .or_else(|| positional_args(invocation).first().copied())
            {
                session.swap_panes(primary_client_id, invocation.source_arg(), target)?;
                Ok(CommandOutcome::Mutated {
                    command: invocation.name.clone(),
                })
            } else if let Some(neighbor) = swap_pane_neighbor(invocation)? {
                if invocation.source_arg().is_some() {
                    return Err(MezError::invalid_args(
                        "swap-pane direction flags operate on the active pane",
                    ));
                }
                let Some(target) = swap_pane_neighbor_target(session, neighbor)? else {
                    return Ok(CommandOutcome::Noop {
                        command: invocation.name.clone(),
                    });
                };
                session.swap_panes(primary_client_id, None, &target)?;
                Ok(CommandOutcome::Mutated {
                    command: invocation.name.clone(),
                })
            } else {
                Err(MezError::invalid_args("swap-pane requires a target"))
            }
        }
        "break-pane" | "breakp" => {
            let name = flag_value(&invocation.args, "-n")
                .or_else(|| flag_value(&invocation.args, "--name"))
                .map(ToString::to_string);
            let select = !invocation.has_flag("-d", "--detached");
            let target = invocation
                .target_arg()
                .or_else(|| positional_args(invocation).first().copied());
            session.break_pane(primary_client_id, target, name, select)?;
            Ok(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            })
        }
        "join-pane" | "joinp" => {
            let target = invocation
                .target_arg()
                .or_else(|| positional_args(invocation).first().copied())
                .ok_or_else(|| MezError::invalid_args("join-pane requires a target"))?;
            let direction = if invocation.has_flag("-h", "--horizontal") {
                SplitDirection::Horizontal
            } else {
                SplitDirection::Vertical
            };
            let select = invocation.args.iter().any(|arg| arg == "--select");
            session.join_pane(
                primary_client_id,
                invocation.source_arg(),
                target,
                direction,
                select,
            )?;
            Ok(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            })
        }
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
        "approve-observer" => {
            let observer_id = invocation
                .target_arg()
                .or_else(|| positional_args(invocation).first().copied())
                .ok_or_else(|| MezError::invalid_args("approve-observer requires a target"))?;
            session.approve_observer_target(primary_client_id, observer_id)?;
            Ok(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            })
        }
        "reject-observer" => {
            let observer_id = invocation
                .target_arg()
                .or_else(|| positional_args(invocation).first().copied())
                .ok_or_else(|| MezError::invalid_args("reject-observer requires a target"))?;
            session.reject_observer_target(primary_client_id, observer_id)?;
            Ok(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            })
        }
        "revoke-observer" => {
            let client_id = invocation
                .target_arg()
                .or_else(|| positional_args(invocation).first().copied())
                .ok_or_else(|| MezError::invalid_args("revoke-observer requires a client id"))?;
            session.revoke_observer_client(primary_client_id, client_id)?;
            Ok(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            })
        }
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
        "snapshot-session" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: snapshot_session_display(invocation),
        }),
        "resume-session" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: resume_session_display(invocation),
        }),
        "show-messages" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: show_messages_display(),
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
        "agent-shell" => Ok(CommandOutcome::Noop {
            command: invocation.name.clone(),
        }),
        "auth-status" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: auth_status_display(),
        }),
        "auth-login" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: auth_login_plan_display(invocation),
        }),
        "mcp-add" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: mcp_add_plan_display(invocation)?,
        }),
        "mcp-remove" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: mcp_remove_plan_display(invocation)?,
        }),
        "mcp-retry" => Ok(CommandOutcome::Display {
            command: invocation.name.clone(),
            body: mcp_retry_plan_display(invocation)?,
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
        "rename-session" | "renames" => {
            let name = positional_args(invocation).join(" ");
            if name.is_empty() {
                return Err(MezError::invalid_args("rename-session requires a name"));
            }
            session.rename_session(primary_client_id, name)?;
            Ok(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            })
        }
        "kill-session" => {
            let force = invocation.has_flag("-f", "--force");
            session.kill_session(primary_client_id, force)?;
            Ok(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            })
        }
        "detach-client" | "detach" => {
            let target = invocation
                .target_arg()
                .unwrap_or(primary_client_id.as_str());
            session.detach_client_target(primary_client_id, target)?;
            Ok(CommandOutcome::Mutated {
                command: invocation.name.clone(),
            })
        }
        _ => Err(MezError::invalid_args(format!(
            "unknown command `{}`",
            invocation.name
        ))),
    }
}

/// Runs the split window selects new pane operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn split_window_selects_new_pane(invocation: &CommandInvocation) -> Result<bool> {
    let explicit_select = invocation.args.iter().any(|arg| arg == "--select");
    let detached = invocation.has_flag("-d", "--detached")
        || invocation.args.iter().any(|arg| arg == "--no-select");
    if explicit_select && detached {
        return Err(MezError::invalid_args(
            "split-window cannot combine --select with -d/--no-select",
        ));
    }
    Ok(!detached)
}
