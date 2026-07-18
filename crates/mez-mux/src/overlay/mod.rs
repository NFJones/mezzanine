//! Neutral modal overlay state and interaction policy.
//!
//! Overlay state owns lines, styles, search, selection, scrolling, and an
//! optional record-browser stack. The record-browser source is generic so the
//! product crate can retain issue and memory query adapters without creating a
//! dependency from mux presentation to the agent domain.

mod input;
mod interaction;
mod state;

pub use input::{
    OverlayInputAction, OverlayInputOutcome, SelectorInputAction, SelectorInputOutcome,
    apply_overlay_input, apply_selector_input, keep_selector_active_visible, move_selector,
    overlay_input_action, scroll_selector, selector_input_action, selector_step_index,
    set_selector_index,
};
pub use interaction::{
    OVERLAY_ACTIVE_SELECTOR, OVERLAY_INACTIVE_SELECTOR, apply_overlay_scroll_delta,
    clamp_overlay_scroll, overlay_active_line_index, overlay_body_style_spans,
    overlay_copy_selection, overlay_footer, overlay_line_prefix_columns, overlay_line_slice,
    overlay_link_rendition, overlay_next_search_match, overlay_render_lines,
    overlay_rendered_line_style_spans, overlay_rendered_selection_start,
    overlay_search_match_on_line, overlay_selection_gutter_rendition,
    overlay_selection_index_at_position, overlay_selection_index_is_visible,
    overlay_selection_prefix_columns, overlay_selection_rendition, overlay_text_at,
    scroll_overlay_to_line, update_overlay_active_selection_for_viewport,
};
pub use state::{
    AnchoredSelector, DisplayOverlay, OverlaySearchMatch, OverlaySelection, OverlaySelectionKind,
    RecordBrowserOverlayFrame, RecordBrowserOverlayState,
};
