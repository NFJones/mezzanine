//! Product input, prefix, mouse, and bracketed-paste routing.

use std::time::Instant;

use super::types::{HostBracketedPasteBufferState, TerminalClientLoopAction};
use super::{MouseAction, MouseEvent, Result, TerminalClientLoopConfig, parse_sgr_mouse};
use crate::terminal::mouse::mouse_copy_position;
use mez_mux::attached_client::{
    application_cursor_forwarding_bytes, application_mouse_forwarding_bytes,
    classify_attached_mouse_event, earliest_sequence_start, input_sequence_start,
    malformed_sgr_mouse_prefix_len, prefix_sequence_len, sgr_mouse_sequence_len,
    sgr_mouse_sequence_start,
};
use mez_mux::copy::{CopyModeKeyAction, classify_copy_mode_key_action};
use mez_mux::input::{
    MuxAction, TerminalInputClassification, WindowFocusTarget, classify_prefix_binding,
    classify_terminal_input_with_command_bindings, key_chord_input_bytes, parse_key_chord_bytes,
};

/// Routes one host-input unit into a product terminal-loop action.
pub fn route_client_input(
    input: &[u8],
    config: &TerminalClientLoopConfig,
) -> Result<TerminalClientLoopAction> {
    if config.prefix_key_pending {
        let Some((action, _)) = route_pending_prefix_client_input_action(input, config)? else {
            return Ok(TerminalClientLoopAction::ReportUnboundPrefix(
                config.bindings.escape,
            ));
        };
        return Ok(action);
    }

    if input.starts_with(b"\x1b[<") {
        let Some(event) = parse_sgr_mouse(input) else {
            return Ok(TerminalClientLoopAction::HandleMouse(MouseAction::Ignore));
        };
        return route_mouse_event(input, event, config);
    }

    if config.mouse_policy.copy_mode_active {
        if config.scrollback_copy_mode_active {
            if let Some(action) = classify_copy_mode_key_action(input) {
                return Ok(TerminalClientLoopAction::HandleCopyMode(action));
            }
        } else {
            let action = classify_copy_mode_key_action(input).unwrap_or(CopyModeKeyAction::Ignore);
            return Ok(TerminalClientLoopAction::HandleCopyMode(action));
        }
    }
    match classify_terminal_input_with_command_bindings(
        input,
        &config.bindings,
        &config.command_bindings,
    )? {
        TerminalInputClassification::ForwardToPane => Ok(TerminalClientLoopAction::ForwardToPane(
            application_cursor_forwarding_bytes(input, config.mouse_policy)
                .unwrap_or_else(|| input.to_vec()),
        )),
        TerminalInputClassification::PrefixKeyMode => {
            Ok(TerminalClientLoopAction::EnterPrefixKeyMode)
        }
        TerminalInputClassification::UnboundPrefix(chord) => {
            Ok(TerminalClientLoopAction::ReportUnboundPrefix(chord))
        }
        TerminalInputClassification::Mouse(event) => route_mouse_event(input, event, config),
        TerminalInputClassification::CommandBinding(command) => {
            Ok(TerminalClientLoopAction::ExecuteCommand(command))
        }
        TerminalInputClassification::Mux(action) => {
            Ok(TerminalClientLoopAction::ExecuteMux(action))
        }
    }
}

/// Runs the route mouse event operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn route_mouse_event(
    input: &[u8],
    event: MouseEvent,
    config: &TerminalClientLoopConfig,
) -> Result<TerminalClientLoopAction> {
    if config.primary_display_overlay_active {
        return Ok(TerminalClientLoopAction::HandleMouse(
            match (event.kind, event.button) {
                (super::MouseEventKind::Press, super::MouseButton::Left) => {
                    MouseAction::BeginDisplayOverlaySelection {
                        position: mouse_copy_position(event),
                    }
                }
                (super::MouseEventKind::Drag, super::MouseButton::Left) => {
                    MouseAction::UpdateDisplayOverlaySelection {
                        position: mouse_copy_position(event),
                    }
                }
                (super::MouseEventKind::Release, super::MouseButton::Left) => {
                    MouseAction::FinishDisplayOverlaySelection {
                        position: mouse_copy_position(event),
                    }
                }
                (super::MouseEventKind::Scroll, super::MouseButton::WheelUp) => {
                    MouseAction::ScrollDisplayOverlay { lines: -3 }
                }
                (super::MouseEventKind::Scroll, super::MouseButton::WheelDown) => {
                    MouseAction::ScrollDisplayOverlay { lines: 3 }
                }
                _ => MouseAction::Ignore,
            },
        ));
    }

    let mut policy = config.mouse_policy;
    let pane_region = config
        .mouse_pane_regions
        .iter()
        .find(|region| region.contains(event.column, event.row));
    if let Some(region) = pane_region {
        policy.pane_application_mouse_mode = region.application_mouse_mode;
        policy.pane_sgr_mouse_mode = region.application_sgr_mouse_mode;
        policy.copy_mode_active = config.mouse_selection_active || region.copy_mode_active;
    } else {
        policy.pane_application_mouse_mode = false;
        policy.pane_sgr_mouse_mode = false;
        policy.copy_mode_active = config.mouse_selection_active;
    }
    policy.over_pane_border |= config
        .mouse_border_cells
        .iter()
        .any(|cell| cell.column == event.column && cell.row == event.row);
    if let Some(cell) = config
        .mouse_pane_agent_selector_cells
        .iter()
        .find(|cell| cell.column == event.column && cell.row == event.row)
    {
        return Ok(TerminalClientLoopAction::HandleMouse(
            match (event.kind, event.button) {
                (super::MouseEventKind::Scroll, super::MouseButton::WheelUp) => {
                    MouseAction::ScrollPaneAgentStatusSelector {
                        pane_index: cell.pane_index,
                        field: cell.field,
                        lines: -3,
                    }
                }
                (super::MouseEventKind::Scroll, super::MouseButton::WheelDown) => {
                    MouseAction::ScrollPaneAgentStatusSelector {
                        pane_index: cell.pane_index,
                        field: cell.field,
                        lines: 3,
                    }
                }
                (super::MouseEventKind::Release, super::MouseButton::Left) => {
                    MouseAction::SelectPaneAgentStatusSelector {
                        pane_index: cell.pane_index,
                        field: cell.field,
                        item_index: cell.item_index,
                    }
                }
                (
                    super::MouseEventKind::Press | super::MouseEventKind::Drag,
                    super::MouseButton::Left,
                ) => MouseAction::HoverPaneAgentStatusSelector {
                    pane_index: cell.pane_index,
                    field: cell.field,
                    item_index: cell.item_index,
                },
                _ => MouseAction::Ignore,
            },
        ));
    }
    if let Some(cell) = config
        .mouse_pane_agent_status_cells
        .iter()
        .find(|cell| cell.column == event.column && cell.row == event.row)
    {
        return Ok(TerminalClientLoopAction::HandleMouse(
            match (event.kind, event.button) {
                (super::MouseEventKind::Release, super::MouseButton::Left) => {
                    MouseAction::OpenPaneAgentStatusSelector {
                        pane_index: cell.pane_index,
                        field: cell.field,
                    }
                }
                _ => MouseAction::Ignore,
            },
        ));
    }
    if let Some(cell) = config
        .mouse_window_action_frame_cells
        .iter()
        .find(|cell| cell.column == event.column && cell.row == event.row)
        && matches!(
            (event.kind, event.button),
            (super::MouseEventKind::Press, super::MouseButton::Left)
        )
    {
        return Ok(TerminalClientLoopAction::HandleMouse(
            MouseAction::PressWindowAction {
                action: cell.action.clone(),
            },
        ));
    }
    if config.frame_context.pressed_window_action.is_some()
        && matches!(
            (event.kind, event.button),
            (super::MouseEventKind::Release, super::MouseButton::Left)
        )
    {
        let action = config
            .mouse_window_action_frame_cells
            .iter()
            .find(|cell| cell.column == event.column && cell.row == event.row)
            .map(|cell| cell.action.clone());
        return Ok(TerminalClientLoopAction::HandleMouse(
            if let Some(action) = action
                && Some(action.clone()) == config.frame_context.pressed_window_action
            {
                MouseAction::ReleaseWindowAction { action }
            } else {
                MouseAction::CancelWindowAction
            },
        ));
    }
    if let Some(cell) = config
        .mouse_window_group_frame_cells
        .iter()
        .find(|cell| cell.column == event.column && cell.row == event.row)
        && matches!(
            (event.kind, event.button),
            (super::MouseEventKind::Press, super::MouseButton::Left)
        )
    {
        return Ok(TerminalClientLoopAction::HandleMouse(
            MouseAction::FocusGroup {
                index: cell.group_index,
            },
        ));
    }
    if let Some(cell) = config
        .mouse_window_frame_cells
        .iter()
        .find(|cell| cell.column == event.column && cell.row == event.row)
        && matches!(
            (event.kind, event.button),
            (super::MouseEventKind::Press, super::MouseButton::Left)
        )
    {
        return Ok(TerminalClientLoopAction::HandleMouse(
            MouseAction::FocusWindow {
                index: cell.window_index,
            },
        ));
    }
    if !config.mouse_pane_agent_selector_cells.is_empty()
        && matches!(
            (event.kind, event.button),
            (super::MouseEventKind::Press, super::MouseButton::Left)
        )
    {
        return Ok(TerminalClientLoopAction::HandleMouse(
            MouseAction::ClosePaneAgentStatusSelector,
        ));
    }
    policy.over_window_frame |= config
        .mouse_window_frame_cells
        .iter()
        .any(|cell| cell.column == event.column && cell.row == event.row)
        || config
            .mouse_window_action_frame_cells
            .iter()
            .any(|cell| cell.column == event.column && cell.row == event.row)
        || config
            .mouse_window_group_frame_cells
            .iter()
            .any(|cell| cell.column == event.column && cell.row == event.row)
        || config
            .mouse_pane_agent_status_cells
            .iter()
            .any(|cell| cell.column == event.column && cell.row == event.row)
        || config
            .mouse_pane_agent_selector_cells
            .iter()
            .any(|cell| cell.column == event.column && cell.row == event.row);
    if let Some(region) = pane_region
        && region.application_mouse_mode
        && !region.active
        && !policy.pane_resize_active
        && !policy.copy_mode_active
        && !policy.over_window_frame
        && !policy.over_pane_border
        && matches!(event.kind, super::MouseEventKind::Press)
    {
        return Ok(TerminalClientLoopAction::HandleMouse(
            MouseAction::FocusPaneOnly(mez_mux::copy::CopyPosition {
                line: usize::from(event.row),
                column: usize::from(event.column),
            }),
        ));
    }
    let action = MouseAction::from(classify_attached_mouse_event(event, policy));
    if action == MouseAction::ForwardToPane {
        if let Some(region) = pane_region {
            if let Some(input) = application_mouse_forwarding_bytes(event, region) {
                Ok(TerminalClientLoopAction::ForwardMouseToPane {
                    pane_id: region.pane_id.clone(),
                    input,
                })
            } else {
                Ok(TerminalClientLoopAction::HandleMouse(MouseAction::Ignore))
            }
        } else {
            Ok(TerminalClientLoopAction::ForwardToPane(input.to_vec()))
        }
    } else {
        Ok(TerminalClientLoopAction::HandleMouse(action))
    }
}

/// Splits a raw attached-terminal input buffer into mux, prompt, mouse, and
/// pane-forwarding actions without letting batched bytes hide a prefix command.
pub(crate) fn route_client_input_actions(
    input: &[u8],
    config: &TerminalClientLoopConfig,
) -> Result<Vec<TerminalClientLoopAction>> {
    let mut host_bracketed_paste_active = config.host_bracketed_paste_active;
    route_client_input_actions_with_host_paste_state(
        input,
        config,
        &mut host_bracketed_paste_active,
    )
}

/// Splits attached-terminal input while preserving host bracketed paste state.
///
/// Pasted payloads must be treated as opaque bytes. Otherwise a large clipboard
/// paste can accidentally trigger mux-prefix commands or mouse handling when a
/// payload chunk happens to contain the configured prefix or SGR-shaped text.
pub(crate) fn route_client_input_actions_with_host_paste_state(
    input: &[u8],
    config: &TerminalClientLoopConfig,
    host_bracketed_paste_active: &mut bool,
) -> Result<Vec<TerminalClientLoopAction>> {
    const HOST_BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
    const HOST_BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

    let mut remaining = input;
    let mut actions = Vec::new();
    let mut config = config.clone();
    let prefix = key_chord_input_bytes(config.bindings.escape);

    while !remaining.is_empty() {
        if *host_bracketed_paste_active {
            config.prefix_key_pending = false;
            if let Some(end_start) = input_sequence_start(remaining, HOST_BRACKETED_PASTE_END) {
                let consumed = end_start.saturating_add(HOST_BRACKETED_PASTE_END.len());
                actions.push(TerminalClientLoopAction::ForwardToPane(
                    remaining[..consumed].to_vec(),
                ));
                *host_bracketed_paste_active = false;
                remaining = &remaining[consumed..];
                continue;
            }
            actions.push(TerminalClientLoopAction::ForwardToPane(remaining.to_vec()));
            break;
        }

        let paste_start = input_sequence_start(remaining, HOST_BRACKETED_PASTE_START);
        if paste_start == Some(0) {
            config.prefix_key_pending = false;
            if let Some(end_start) = input_sequence_start(remaining, HOST_BRACKETED_PASTE_END) {
                let consumed = end_start.saturating_add(HOST_BRACKETED_PASTE_END.len());
                actions.push(TerminalClientLoopAction::ForwardToPane(
                    remaining[..consumed].to_vec(),
                ));
                remaining = &remaining[consumed..];
                continue;
            }
            actions.push(TerminalClientLoopAction::ForwardToPane(remaining.to_vec()));
            *host_bracketed_paste_active = true;
            break;
        }

        if config.prefix_key_pending {
            let Some((action, consumed)) =
                route_pending_prefix_client_input_action(remaining, &config)?
            else {
                actions.push(TerminalClientLoopAction::ReportUnboundPrefix(
                    config.bindings.escape,
                ));
                config.prefix_key_pending = false;
                break;
            };
            let enters_prompt = action_enters_client_prompt(&action);
            actions.push(action);
            config.prefix_key_pending = false;
            remaining = &remaining[consumed..];
            if enters_prompt {
                break;
            }
            continue;
        }

        let mouse_start = sgr_mouse_sequence_start(remaining);
        let prefix_start = prefix
            .as_deref()
            .and_then(|prefix| input_sequence_start(remaining, prefix));
        let Some(special_start) = earliest_sequence_start([paste_start, mouse_start, prefix_start])
        else {
            actions.push(route_client_input(remaining, &config)?);
            break;
        };

        if special_start > 0 {
            actions.push(route_client_input(&remaining[..special_start], &config)?);
            remaining = &remaining[special_start..];
            continue;
        }

        let prefix_first = prefix_start == Some(0) && mouse_start != Some(0);
        if prefix_first && let Some(prefix) = prefix.as_deref() {
            let Some((action, consumed)) =
                route_prefix_client_input_action(remaining, prefix, &config)?
            else {
                actions.push(route_client_input(remaining, &config)?);
                break;
            };
            let enters_prompt = action_enters_client_prompt(&action);
            let enters_prefix_key_mode =
                matches!(action, TerminalClientLoopAction::EnterPrefixKeyMode);
            actions.push(action);
            if enters_prefix_key_mode {
                config.prefix_key_pending = true;
            }
            remaining = &remaining[consumed..];
            if enters_prompt {
                break;
            }
            continue;
        }

        let Some(mouse_len) = sgr_mouse_sequence_len(remaining) else {
            if let Some(malformed_mouse_prefix_len) = malformed_sgr_mouse_prefix_len(remaining) {
                actions.push(TerminalClientLoopAction::HandleMouse(MouseAction::Ignore));
                remaining = &remaining[malformed_mouse_prefix_len..];
                continue;
            }
            actions.push(TerminalClientLoopAction::HandleMouse(MouseAction::Ignore));
            break;
        };
        let action = route_client_input(&remaining[..mouse_len], &config)?;
        apply_batched_mouse_action_side_effects(&mut config, &action);
        actions.push(action);
        remaining = &remaining[mouse_len..];
    }

    Ok(actions)
}

/// Splits attached-terminal input while buffering incomplete host pastes.
///
/// Unlike `route_client_input_actions_with_host_paste_state`, this path waits
/// for the closing bracketed-paste delimiter before forwarding the payload.
/// That preserves shell heredoc ordering for very large pastes by preventing a
/// partial clipboard body from entering the pane before the terminal has
/// delivered the complete paste frame.
#[cfg(test)]
pub(crate) fn route_client_input_actions_with_host_paste_buffer(
    input: &[u8],
    config: &TerminalClientLoopConfig,
    host_bracketed_paste_active: &mut bool,
    host_bracketed_paste_buffer: &mut Vec<u8>,
) -> Result<Vec<TerminalClientLoopAction>> {
    let mut host_bracketed_paste_started_at = config.host_bracketed_paste_started_at;
    let mut host_paste = HostBracketedPasteBufferState {
        active: host_bracketed_paste_active,
        buffer: host_bracketed_paste_buffer,
        started_at: &mut host_bracketed_paste_started_at,
    };
    route_client_input_actions_with_host_paste_buffer_state(input, config, &mut host_paste)
}

/// Splits attached-terminal input while carrying buffered host paste timing.
pub(super) fn route_client_input_actions_with_host_paste_buffer_state(
    input: &[u8],
    config: &TerminalClientLoopConfig,
    host_paste: &mut HostBracketedPasteBufferState<'_>,
) -> Result<Vec<TerminalClientLoopAction>> {
    let mut decoder = mez_mux::host_input::HostBracketedPasteDecoder::from_parts(
        *host_paste.active,
        host_paste.buffer.clone(),
        *host_paste.started_at,
    );
    let segments = decoder.decode_at(input, Instant::now());
    *host_paste.active = decoder.active();
    host_paste.buffer.clear();
    host_paste.buffer.extend_from_slice(decoder.buffer());
    *host_paste.started_at = decoder.started_at();

    let mut actions = Vec::new();
    for segment in segments {
        match segment {
            mez_mux::host_input::HostInputSegment::BracketedPaste(bytes) => {
                actions.push(TerminalClientLoopAction::ForwardToPane(bytes));
            }
            mez_mux::host_input::HostInputSegment::Ordinary(bytes) => {
                let mut paste_active = false;
                actions.extend(route_client_input_actions_with_host_paste_state(
                    &bytes,
                    config,
                    &mut paste_active,
                )?);
            }
        }
    }
    Ok(actions)
}

/// Applies routing state transitions for mouse actions emitted from a batched
/// attached-terminal input scan.
fn apply_batched_mouse_action_side_effects(
    config: &mut TerminalClientLoopConfig,
    action: &TerminalClientLoopAction,
) {
    match action {
        TerminalClientLoopAction::HandleMouse(MouseAction::ResizePane { .. }) => {
            config.mouse_policy.pane_resize_active = true;
        }
        TerminalClientLoopAction::HandleMouse(MouseAction::FocusPaneOnly(position)) => {
            set_mouse_region_active_at(config, position.column, position.line);
        }
        TerminalClientLoopAction::HandleMouse(MouseAction::FocusPane(_)) => {
            config.mouse_selection_active = true;
        }
        TerminalClientLoopAction::HandleMouse(MouseAction::CopySelectionStart(_)) => {
            config.mouse_selection_active = true;
        }
        TerminalClientLoopAction::HandleMouse(MouseAction::CopySelectionUpdate(_)) => {
            config.mouse_selection_active = true;
        }
        TerminalClientLoopAction::HandleMouse(MouseAction::FinishResizePane) => {
            config.mouse_policy.pane_resize_active = false;
        }
        TerminalClientLoopAction::HandleMouse(MouseAction::CopySelectionFinish(_)) => {
            config.mouse_selection_active = false;
        }
        TerminalClientLoopAction::HandleMouse(MouseAction::PressWindowAction { action }) => {
            config.frame_context.pressed_window_action = Some(action.clone());
        }
        TerminalClientLoopAction::HandleMouse(
            MouseAction::ReleaseWindowAction { .. } | MouseAction::CancelWindowAction,
        ) => {
            config.frame_context.pressed_window_action = None;
        }
        _ => {}
    }
}

/// Runs the route prefix client input action operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn route_prefix_client_input_action(
    input: &[u8],
    prefix: &[u8],
    config: &TerminalClientLoopConfig,
) -> Result<Option<(TerminalClientLoopAction, usize)>> {
    let Some(consumed) = prefix_sequence_len(input, prefix) else {
        return Ok(None);
    };
    Ok(Some((
        route_client_input(&input[..consumed], config)?,
        consumed,
    )))
}

/// Routes the next key through the prefix table.
///
/// # Parameters
/// - `input`: The raw input beginning with the key that should consume the
///   pending prefix state.
/// - `config`: The active client loop routing configuration.
fn route_pending_prefix_client_input_action(
    input: &[u8],
    config: &TerminalClientLoopConfig,
) -> Result<Option<(TerminalClientLoopAction, usize)>> {
    let Some((chord, consumed)) = parse_key_chord_bytes(input) else {
        return Ok(None);
    };
    if let Some(command) = config.command_bindings.get(&chord) {
        return Ok(Some((
            TerminalClientLoopAction::ExecuteCommand(command.to_string()),
            consumed,
        )));
    }
    let action = classify_prefix_binding(chord, &config.bindings)
        .map(TerminalClientLoopAction::ExecuteMux)
        .unwrap_or(TerminalClientLoopAction::ReportUnboundPrefix(chord));
    Ok(Some((action, consumed)))
}

/// Runs the action enters client prompt operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn action_enters_client_prompt(action: &TerminalClientLoopAction) -> bool {
    matches!(
        action,
        TerminalClientLoopAction::ExecuteMux(
            MuxAction::EnterCommandPrompt
                | MuxAction::RenameWindow
                | MuxAction::KillWindowAfterConfirmation
                | MuxAction::KillPaneAfterConfirmation
                | MuxAction::FocusWindow(WindowFocusTarget::PromptForIndex)
                | MuxAction::FocusWindow(WindowFocusTarget::PromptForNewIndex)
        )
    )
}

/// Runs the set mouse region active at operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn set_mouse_region_active_at(config: &mut TerminalClientLoopConfig, column: usize, row: usize) {
    let Ok(column) = u16::try_from(column) else {
        return;
    };
    let Ok(row) = u16::try_from(row) else {
        return;
    };
    let active_pane_id = config
        .mouse_pane_regions
        .iter()
        .find(|region| region.contains(column, row))
        .map(|region| region.pane_id.clone());
    if let Some(active_pane_id) = active_pane_id {
        for region in &mut config.mouse_pane_regions {
            region.active = region.pane_id == active_pane_id;
        }
    }
}
