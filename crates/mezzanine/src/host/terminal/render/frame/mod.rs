//! Frame and status-bar rendering helpers.
//!
//! This facade preserves the terminal rendering API while focused leaves own
//! style policy, pane composition, window status, pillbox entries, and layout writes.

mod entries;
mod layout;
mod pane;
mod style;
mod window;

pub use entries::window_group_frame_pillbox_cells;
pub(in crate::host::terminal::render) use entries::{
    WindowFramePillboxTarget, group_frame_pillbox_entries,
    pane_agent_status_field_from_frame_field, pillbox_segment_local_columns,
    window_action_pillbox_entries, window_frame_action_entry, window_frame_field_value,
    window_frame_pillbox_entries, window_frame_pillbox_entries_from_context,
    window_frame_pillbox_segments, window_frame_pillbox_text_from_entries,
};
#[cfg(test)]
pub(in crate::host::terminal::render) use layout::write_merged_pane_frames_on_dividers;
pub(in crate::host::terminal::render) use layout::{
    pane_frame_field_value, write_styled_merged_pane_frames_on_dividers,
};
#[cfg(test)]
pub(in crate::host::terminal::render) use pane::render_pane_lines;
pub(in crate::host::terminal::render) use pane::{
    PaneFrameRightStatusSegment, compact_pane_working_directory, pane_agent_prompt_space_reserved,
    pane_agent_prompt_transparent, pane_agent_shell_visible, pane_frame_fill_char,
    pane_frame_row_layout, render_styled_pane_lines,
};
#[cfg(test)]
pub(in crate::host::terminal::render) use style::group_frame_text;
pub(in crate::host::terminal::render) use style::{
    AGENT_STATUS_SCAN_BAND_WIDTH, pane_border_rendition, pane_frame_rendition,
    pane_frame_right_status_style_spans, styled_group_frame_line, styled_pane_frame_line,
    styled_window_frame_line, window_pillbox_rendition,
};
pub(in crate::host::terminal::render) use window::{
    WindowStatusSegmentKind, render_window_frame_text, render_window_status_template,
    window_right_status_layout, window_status_style_spans,
};
pub use window::{
    pane_frame_agent_status_pillbox_cells, window_frame_action_pillbox_cells,
    window_frame_pillbox_cells,
};
