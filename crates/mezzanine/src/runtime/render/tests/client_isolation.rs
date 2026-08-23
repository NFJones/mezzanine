//! Exact-client runtime presentation isolation regressions.

use super::super::{RuntimePresentationComponent, default_runtime_agent_prompt_input};
use crate::host::terminal::CopyMode;
use crate::runtime::PaneSurfaceKind;
use mez_core::ids::ClientId;
use mez_mux::layout::Size;
use mez_terminal::TerminalScreen;

/// Verifies switching the compatibility projection preserves each client's
/// prefix, error, and copy-mode state without leaking it to another client.
#[test]
fn client_presentation_projection_isolates_transient_interaction_state() {
    let first = ClientId::new('c', 1);
    let second = ClientId::new('c', 2);
    let key = ("%1".to_string(), PaneSurfaceKind::Process);
    let screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 16).unwrap();
    let copy_mode = CopyMode::from_visible_screen(&screen, 4).unwrap();
    let mut presentation = RuntimePresentationComponent::default();

    presentation.activate_client_state(&first);
    presentation.primary_prefix_key_pending = true;
    presentation.primary_error_status_overlay = Some("first-error".to_string());
    presentation
        .copy
        .active_copy_modes
        .insert(key.clone(), copy_mode);
    presentation.capture_projected_client_state();

    presentation.activate_client_state(&second);
    assert!(!presentation.primary_prefix_key_pending);
    assert!(presentation.primary_error_status_overlay.is_none());
    assert!(presentation.copy.active_copy_modes.is_empty());
    presentation.primary_error_status_overlay = Some("second-error".to_string());
    presentation.capture_projected_client_state();

    presentation.activate_client_state(&first);
    assert!(presentation.primary_prefix_key_pending);
    assert_eq!(
        presentation.primary_error_status_overlay.as_deref(),
        Some("first-error")
    );
    assert!(presentation.copy.active_copy_modes.contains_key(&key));

    presentation.activate_client_state(&second);
    assert!(!presentation.primary_prefix_key_pending);
    assert_eq!(
        presentation.primary_error_status_overlay.as_deref(),
        Some("second-error")
    );
    assert!(presentation.copy.active_copy_modes.is_empty());
}

/// Verifies detaching one client drops only that client's transient state.
#[test]
fn removing_client_presentation_retains_other_client_state() {
    let first = ClientId::new('c', 3);
    let second = ClientId::new('c', 4);
    let mut presentation = RuntimePresentationComponent::default();

    presentation.activate_client_state(&first);
    presentation.primary_prefix_key_pending = true;
    presentation.capture_projected_client_state();
    presentation.activate_client_state(&second);
    presentation.primary_error_status_overlay = Some("retained".to_string());
    presentation.capture_projected_client_state();

    presentation.remove_client_state(&first);
    assert!(!presentation.client_states.contains_key(&first));
    presentation.activate_client_state(&second);
    assert_eq!(
        presentation.primary_error_status_overlay.as_deref(),
        Some("retained")
    );
}

/// Verifies pane-agent drafts are client-local and pane teardown removes the
/// matching draft and copy state from every retained client.
#[test]
fn client_agent_drafts_are_isolated_and_removed_with_their_pane() {
    let first = ClientId::new('c', 5);
    let second = ClientId::new('c', 6);
    let key = ("%1".to_string(), PaneSurfaceKind::Process);
    let screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 16).unwrap();
    let copy_mode = CopyMode::from_visible_screen(&screen, 4).unwrap();
    let mut presentation = RuntimePresentationComponent::default();

    presentation.activate_client_state(&first);
    let mut first_prompt = default_runtime_agent_prompt_input();
    first_prompt.prompt.buffer.set_line("first draft");
    presentation
        .agent_prompt_inputs
        .insert("%1".to_string(), first_prompt);
    presentation
        .copy
        .active_copy_modes
        .insert(key.clone(), copy_mode.clone());
    presentation.capture_projected_client_state();

    presentation.activate_client_state(&second);
    let mut second_prompt = default_runtime_agent_prompt_input();
    second_prompt.prompt.buffer.set_line("second draft");
    presentation
        .agent_prompt_inputs
        .insert("%1".to_string(), second_prompt);
    presentation
        .copy
        .active_copy_modes
        .insert(key.clone(), copy_mode);
    presentation.capture_projected_client_state();

    presentation.activate_client_state(&first);
    assert_eq!(
        presentation.agent_prompt_inputs["%1"].prompt.buffer.line(),
        "first draft"
    );
    presentation.remove_pane_state_for_all_clients("%1");

    for client_id in [&first, &second] {
        presentation.activate_client_state(client_id);
        assert!(!presentation.agent_prompt_inputs.contains_key("%1"));
        assert!(!presentation.copy.active_copy_modes.contains_key(&key));
    }
}
