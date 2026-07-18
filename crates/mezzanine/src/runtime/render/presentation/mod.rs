//! Agent transcript and action-result presentation helpers.
//!
//! This module owns pure formatting for model-authored pane transcript content,
//! command previews, markdown rendering, diff previews, and bounded action
//! result display. Keeping these helpers outside the runtime service facade
//! makes visible output behavior easier to test without mixing it with pane
//! state transitions.

use super::super::{RenderedClientView, ShellClassification, runtime_mezzanine_error_code};
use mez_mux::render::overlay_text_cells;

use crate::host::terminal::agent_wrap_column_cap;
use mez_agent::semantic_patch_planning::apply_patch_touched_paths;
use mez_agent::{AgentAction, AgentActionPayload};
use mez_mux::copy::COPY_SKIP_LINE as AGENT_COPY_SKIP_LINE;
use mez_mux::render::{
    DiffDisplayLine, DiffDisplaySection, RichTextLine, RichTextLineKind, append_syntax_spans,
    diff_highlighter_for_path, diff_section_path, format_diff_display_line, frame_markdown_lines,
    overlay_fixed_column_style_spans, parse_unified_diff_sections, wrap_rich_text_line_to_width,
    wrap_rich_text_lines_to_width,
};
use mez_mux::render::{
    MARKDOWN_DARK_MUTED_FOREGROUND, MARKDOWN_DARK_NEUTRAL_FOREGROUND,
    MARKDOWN_LIGHT_NEUTRAL_FOREGROUND, RichTextTheme, prefix_rich_text_lines, render_markdown,
};
use mez_mux::render::{
    SyntaxHighlighter, SyntaxTheme, SyntaxThemePalette, syntax_highlighter_for_extension,
    syntax_theme,
};
use mez_mux::theme::{UiColorPair, UiTheme};
use mez_terminal::active_terminal_grapheme_width as terminal_grapheme_width;
use mez_terminal::{GraphicRendition, TerminalColor, TerminalStyleSpan, TerminalStyledLine};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

mod actions;
mod buffer_apply;
mod diff;
mod style;
mod text;

#[cfg(test)]
pub(crate) use actions::{
    agent_action_execution_display_header, agent_thinking_display_lines_for_width,
};
#[cfg(test)]
pub(crate) use diff::{
    agent_action_result_uses_diff_preview, readable_agent_diff_display_lines,
    readable_agent_diff_display_lines_for_width,
};
pub(crate) use style::AgentTerminalPresentationStyle;
pub(crate) use text::{
    agent_display_lines_are_error, agent_display_lines_are_low_level_status,
    agent_prompt_error_display_lines, overlay_styled_lines,
    render_command_markdown_body_lines_for_width, sanitized_agent_terminal_line,
};
#[cfg(test)]
pub(crate) use text::{
    command_preview_terminal_rendered_lines, render_command_markdown_body_lines,
    rendered_line_rendition_at, wrap_agent_terminal_text, wrapped_prefixed_agent_terminal_lines,
};
