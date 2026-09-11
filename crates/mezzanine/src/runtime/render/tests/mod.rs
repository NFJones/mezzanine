//! Runtime render behavior tests.

use super::super::service_state::RuntimeDisplayOverlay;
use super::{
    RichTextLine, RichTextLineKind, RuntimeSessionService, agent_action_execution_display_header,
    agent_action_result_uses_diff_preview, agent_thinking_display_lines_for_width,
    command_preview_terminal_rendered_lines, overlay_link_rendition,
    overlay_rendered_line_style_spans, overlay_rendered_selection_start,
    readable_agent_diff_display_lines, readable_agent_diff_display_lines_for_width,
    render_agent_markdown_body_lines, render_command_markdown_body_lines,
    rendered_line_rendition_at, runtime_agent_shell_markdown_overlay_content,
    runtime_command_display_overlay_content, runtime_human_readable_display_lines,
    runtime_pane_agent_selector_rendition, wrap_agent_terminal_text, wrap_rich_text_line_to_width,
    wrapped_prefixed_agent_terminal_lines,
};
use crate::host::terminal::PaneAgentStatusField;
use mez_agent::{AgentAction, AgentActionPayload};
use mez_mux::layout::Size;
use mez_mux::overlay::{
    OverlayActionId, OverlaySearchMatch, OverlaySelection, OverlaySelectionKind,
    overlay_selection_prefix_columns,
};
use mez_mux::theme::default_ui_theme;
use mez_terminal::{GraphicRendition, TerminalStyleSpan};

/// Visual-only pane pill changes preserve provider cache state, while an
/// executable provider change still invalidates the retained value.
#[test]
fn pane_status_color_only_settings_change_preserves_provider_cache() {
    use crate::host::terminal::{
        FramePillColorOverrides, PaneStatusField, PaneStatusPillDefinition,
        PaneStatusProviderDefinition, PaneStatusProviderEmptyBehavior,
        PaneStatusProviderErrorBehavior,
    };
    use crate::runtime::status_pills::{
        RuntimePaneStatusProviderKey, RuntimePaneStatusProviderRequest, RuntimeStatusPillSurface,
    };

    let provider = PaneStatusProviderDefinition {
        command: "printf ready".to_string(),
        origin: None,
        interval_ms: 30_000,
        initial: Some("cached".to_string()),
        timeout_ms: 750,
        empty_behavior: PaneStatusProviderEmptyBehavior::Hide,
        error_behavior: PaneStatusProviderErrorBehavior::Hide,
        max_output_chars: 32,
    };
    let mut definition = PaneStatusPillDefinition::builtin(PaneStatusField::Provider);
    definition.provider = Some(provider.clone());
    let mut presentation = super::RuntimePresentationComponent::default();
    presentation
        .settings
        .pane_status
        .pills
        .insert("branch".to_string(), definition);
    let key = RuntimePaneStatusProviderKey {
        surface: RuntimeStatusPillSurface::Pane,
        pane_id: "%1".to_string(),
        name: "branch".to_string(),
        cwd: "/workspace".to_string(),
        config_generation: 1,
        context_generation: 1,
    };
    presentation
        .pane_status_provider_cache
        .borrow_mut()
        .reconcile(vec![RuntimePaneStatusProviderRequest {
            key,
            definition: provider.clone(),
        }]);
    assert_eq!(
        presentation
            .pane_status_provider_cache
            .borrow()
            .values_for_pane("%1")
            .get("branch")
            .map(String::as_str),
        Some("cached")
    );

    let mut colors = presentation.settings.clone();
    colors
        .pane_status
        .pills
        .get_mut("branch")
        .unwrap()
        .color_overrides = FramePillColorOverrides {
        foreground: Some("primary_text".to_string()),
        background: Some("primary".to_string()),
    };
    presentation.apply_settings(colors);
    assert_eq!(
        presentation
            .pane_status_provider_cache
            .borrow()
            .values_for_pane("%1")
            .get("branch")
            .map(String::as_str),
        Some("cached")
    );

    let mut executable = presentation.settings.clone();
    executable
        .pane_status
        .pills
        .get_mut("branch")
        .unwrap()
        .provider
        .as_mut()
        .unwrap()
        .command = "printf changed".to_string();
    presentation.apply_settings(executable);
    assert!(
        presentation
            .pane_status_provider_cache
            .borrow()
            .values_for_pane("%1")
            .is_empty()
    );
}

mod action_presentation;
mod client_isolation;
mod human_readable;
mod link_styling;
mod overlay_interaction;
