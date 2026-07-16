//! One-step attached-terminal planning over mux-owned presentation state.

use super::*;

/// Plans one host-terminal client step over neutral mux presentation state.
pub fn plan_attached_terminal_client_step(
    readiness: &[AttachedTerminalFdReadiness],
    input: Option<&[u8]>,
    view: Option<&RenderedClientView>,
    status: Option<&ClientStatusLine>,
    config: &TerminalClientLoopConfig,
) -> Result<AttachedTerminalClientStepPlan> {
    let mut host_bracketed_paste_active = config.host_bracketed_paste_active;
    let mut host_bracketed_paste_buffer = config.host_bracketed_paste_buffer.clone();
    let mut host_bracketed_paste_started_at = config.host_bracketed_paste_started_at;
    let mut host_paste = HostBracketedPasteBufferState {
        active: &mut host_bracketed_paste_active,
        buffer: &mut host_bracketed_paste_buffer,
        started_at: &mut host_bracketed_paste_started_at,
    };
    plan_attached_terminal_client_step_with_host_paste_buffer(
        readiness,
        input,
        view,
        status,
        config,
        &mut host_paste,
    )
}

/// Plans one attached-terminal client step while buffering incomplete host
/// bracketed paste payloads across terminal-read chunks.
pub(crate) fn plan_attached_terminal_client_step_with_host_paste_buffer(
    readiness: &[AttachedTerminalFdReadiness],
    input: Option<&[u8]>,
    view: Option<&RenderedClientView>,
    status: Option<&ClientStatusLine>,
    config: &TerminalClientLoopConfig,
    host_paste: &mut HostBracketedPasteBufferState<'_>,
) -> Result<AttachedTerminalClientStepPlan> {
    let readiness =
        mez_mux::presentation::classify_attached_client_readiness(readiness.iter().map(|ready| {
            mez_mux::presentation::AttachedClientEndpointReadiness {
                role: ready.role,
                input: ready.role == AttachedTerminalFdRole::Input,
                output: ready.role == AttachedTerminalFdRole::Output,
                readable: ready.readable,
                writable: ready.writable,
                hangup: ready.hangup,
                error: ready.error,
            }
        }));

    let mut actions = Vec::new();
    if readiness.input_readable
        && let Some(input) = input
        && !input.is_empty()
    {
        if view.is_some_and(|view| view.primary_prompt_active) {
            actions.push(TerminalClientLoopAction::ForwardToPane(input.to_vec()));
        } else {
            actions.extend(route_client_input_actions_with_host_paste_buffer_state(
                input, config, host_paste,
            )?);
        }
    }
    if actions.is_empty()
        && let Some(position) = config.mouse_selection_autoscroll_position
    {
        actions.push(TerminalClientLoopAction::HandleMouse(
            MouseAction::CopySelectionUpdate(position),
        ));
    }

    let output = view.map(|view| compose_client_presentation_with_styles(view, status));

    Ok(mez_mux::presentation::plan_attached_client_step(
        readiness, actions, output,
    ))
}
