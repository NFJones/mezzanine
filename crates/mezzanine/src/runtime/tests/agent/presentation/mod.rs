//! Secondary behavior groups for runtime agent presentation tests.

use super::*;
use crate::runtime::CopyMode;

/// Installs one conversation-bound agent screen for presentation-focused tests.
///
/// Presentation writers require a pane agent session so delayed output cannot
/// be attached to an unowned terminal surface. Tests that exercise process
/// copy or terminal behavior should continue to use `set_pane_screen` instead.
fn set_agent_pane_screen_for_test(
    service: &mut RuntimeSessionService,
    pane_id: impl AsRef<str>,
    screen: TerminalScreen,
) {
    let pane_id = pane_id.as_ref();
    let conversation_id = service
        .agent_shell_store_mut()
        .ensure_session(pane_id)
        .unwrap()
        .session_id
        .clone();
    service.set_agent_pane_screen(pane_id.to_string(), conversation_id, screen);
}

/// Opens copy mode directly over a retained agent screen for presentation tests.
///
/// Production copy-mode surface selection is tracked by the separate
/// surface-specific interaction issue. This helper keeps presentation source
/// metadata coverage explicit without reading the process terminal screen.
fn ensure_agent_copy_mode_for_test<'a>(
    service: &'a mut RuntimeSessionService,
    pane_id: &str,
) -> &'a mut CopyMode {
    let viewport_rows = service.copy_mode_viewport_rows_for_pane(pane_id);
    let copy_mode = CopyMode::from_screen(
        service
            .agent_pane_screen(pane_id)
            .expect("agent presentation screen"),
        viewport_rows,
    )
    .expect("agent presentation copy mode");
    service.insert_active_copy_mode_for_presented_surface(pane_id, copy_mode);
    service
        .active_copy_mode_for_presented_surface_mut(pane_id)
        .expect("retained agent presentation copy mode")
}

mod copying;
mod logging;
mod markdown;
mod terminal_ui;
