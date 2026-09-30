//! Canonical media-type projection shared by live say output and durable replay.
//!
//! Rendering consumes explicit geometry and immutable theme inputs. It creates
//! rows and copy metadata only; screen ownership, installation and settlement
//! remain with the actor-owned application components.

use super::*;

impl RuntimeSessionService {
    /// Builds one live or persisted projection through the ordinary say renderers.
    pub(super) fn streaming_say_projection(
        &self,
        action: &RuntimeStreamingSayAction,
        frame_width: usize,
        table_width: usize,
    ) -> StreamingSayProjection {
        Self::streaming_say_projection_with_theme(
            action,
            frame_width,
            table_width,
            &self.presentation.settings.ui_theme,
        )
    }

    /// Builds one projection against an immutable worker-owned theme.
    pub(super) fn streaming_say_projection_with_theme(
        action: &RuntimeStreamingSayAction,
        frame_width: usize,
        table_width: usize,
        ui_theme: &mez_mux::theme::UiTheme,
    ) -> StreamingSayProjection {
        if agent_output_content_type_is_markdown(&action.content_type)
            && !agent_say_text_is_displayed_patch_block(&action.text)
        {
            let body = wrap_rich_text_lines_to_width(
                render_agent_markdown_body_lines(&action.text, ui_theme, table_width),
                frame_width,
                table_width,
            );
            let body_count = body.len();
            let rendered_lines = frame_markdown_lines(body, frame_width);
            let raw_lines = if action.text.is_empty() {
                vec![String::new()]
            } else {
                action.text.split('\n').map(str::to_string).collect()
            };
            let copy_lines = markdown_block_copy_lines(
                &rendered_lines,
                body_count,
                raw_lines,
                AGENT_TERMINAL_MESSAGE_PREFIX,
            );
            return StreamingSayProjection {
                style: AgentTerminalPresentationStyle::Assistant,
                rendered_lines,
                copy_lines,
            };
        }
        if agent_output_content_type_is_diff(&action.content_type) {
            let rendered_lines = streaming_agent_diff_display_lines_for_width(
                &action.text,
                ui_theme,
                frame_width
                    .saturating_sub(UnicodeWidthStr::width("mez> "))
                    .max(1),
            );
            if rendered_lines.is_empty() {
                return StreamingSayProjection {
                    style: AgentTerminalPresentationStyle::Assistant,
                    rendered_lines: wrapped_prefixed_agent_terminal_lines(
                        "mez> ",
                        &action.text,
                        frame_width,
                    ),
                    copy_lines: Vec::new(),
                };
            }
            return StreamingSayProjection {
                style: AgentTerminalPresentationStyle::Assistant,
                rendered_lines: prefix_rich_text_lines(rendered_lines, "mez> ", "     "),
                copy_lines: Vec::new(),
            };
        }
        StreamingSayProjection {
            style: AgentTerminalPresentationStyle::Assistant,
            rendered_lines: wrapped_prefixed_agent_terminal_lines(
                "mez> ",
                &action.text,
                frame_width,
            ),
            copy_lines: Vec::new(),
        }
    }
}
