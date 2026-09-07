//! Title-only identity rendering for transient zen overlays.
//!
//! This adapter reuses ordinary title expansion and active pill styling, but
//! never resolves status rails, actions, providers, or attention animations.

use super::{entries, style};
use crate::host::terminal::TerminalClientLoopConfig;
use mez_mux::layout::Window;
use mez_mux::presentation::FocusLabelScope;
use mez_mux::render::{char_count, render_frame_pill_text_fitted, sanitize_frame_text};
use mez_terminal::{TerminalStyleSpan, TerminalStyledLine};

/// Builds a bounded active identity pill using current display metadata.
/// Missing identities or zero cell budgets yield no pill; empty pane templates
/// fall back to the ordinary pane title and then its stable identity.
pub(crate) fn render_focus_label(
    scope: FocusLabelScope,
    window: &Window,
    config: &TerminalClientLoopConfig,
    width: usize,
) -> Option<TerminalStyledLine> {
    let context = &config.frame_context;
    let (text, rendition) = match scope {
        FocusLabelScope::Group => {
            let group = context.groups.iter().find(|group| group.active)?;
            (
                entries::window_group_frame_entry(group).text,
                style::window_pillbox_rendition(
                    true,
                    false,
                    false,
                    false,
                    context,
                    config.window_frame_style,
                    &config.ui_theme,
                ),
            )
        }
        FocusLabelScope::Window => {
            let text = context
                .windows
                .iter()
                .find(|item| item.id == window.id.as_str())
                .map(|item| entries::window_frame_entry(item).text)
                .unwrap_or_else(|| {
                    format!("{} {}", window.index, sanitize_frame_text(&window.title()))
                });
            (
                text,
                style::window_pillbox_rendition(
                    true,
                    false,
                    false,
                    false,
                    context,
                    config.window_frame_style,
                    &config.ui_theme,
                ),
            )
        }
        FocusLabelScope::Pane => {
            let active = window.active_pane();
            let mut text = sanitize_frame_text(&mez_mux::render::render_frame_template(
                &config.pane_frame_template,
                |field| match field {
                    "window.buttons" | "window.actions" => String::new(),
                    _ => super::pane_frame_field_value(window, active, context, field),
                },
            ));
            if text.trim().is_empty() {
                text = sanitize_frame_text(&active.title);
            }
            if text.trim().is_empty() {
                text = active.id.to_string();
            }
            (
                text,
                style::pane_frame_rendition(
                    active,
                    false,
                    false,
                    context,
                    config.pane_frame_style,
                    &config.ui_theme,
                ),
            )
        }
    };
    let text = render_frame_pill_text_fitted(&text, width);
    let columns = char_count(&text);
    (!text.trim().is_empty()).then(|| TerminalStyledLine {
        text,
        copy_text: None,
        style_spans: vec![TerminalStyleSpan {
            start: 0,
            length: columns,
            rendition,
        }],
    })
}
