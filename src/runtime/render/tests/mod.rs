//! Runtime render behavior tests.

use super::super::service_state::RuntimeDisplayOverlay;
use super::{
    RichTextLine, RichTextLineKind, RuntimeSessionService, agent_action_execution_display_header,
    agent_action_result_uses_diff_preview, agent_thinking_display_lines_for_width,
    command_preview_terminal_rendered_lines, overlay_rendered_line_style_spans,
    overlay_rendered_selection_start, readable_agent_diff_display_lines,
    readable_agent_diff_display_lines_for_width, render_command_markdown_body_lines,
    rendered_line_rendition_at, runtime_agent_shell_markdown_overlay_content,
    runtime_command_display_overlay_content, runtime_human_readable_display_lines,
    runtime_pane_agent_selector_rendition, wrap_agent_terminal_text, wrap_rich_text_line_to_width,
    wrapped_prefixed_agent_terminal_lines,
};
use crate::terminal::PaneAgentStatusField;
use mez_agent::{AgentAction, AgentActionPayload};
use mez_mux::layout::Size;
use mez_mux::overlay::{
    OverlaySearchMatch, OverlaySelection, OverlaySelectionKind, overlay_selection_prefix_columns,
};
use mez_mux::theme::default_ui_theme;
use mez_terminal::{GraphicRendition, TerminalStyleSpan};

mod action_presentation;
mod human_readable;
mod link_styling;
mod overlay_interaction;
