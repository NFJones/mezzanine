//! Runtime command overlay and pane-agent selector helpers.
//!
//! This module owns command-display overlay parsing, selectable command/link
//! rendering, overlay scrolling/style composition, and pane-agent status selector
//! placement. Keeping these pure presentation helpers outside the runtime render
//! facade makes overlay behavior easier to maintain without mixing it with pane
//! input dispatch and frame composition.

mod display_content;
mod product_content;
mod record_adapter;
mod selection_adapter;
mod service;

#[cfg(test)]
pub(crate) use display_content::runtime_human_readable_display_lines;
pub(crate) use display_content::{
    runtime_command_display_overlay_content, runtime_command_display_should_open_overlay,
};
#[cfg(test)]
pub(crate) use product_content::runtime_agent_shell_markdown_overlay_content;
pub(crate) use product_content::{
    RuntimeAgentShellDisplayOutput, agent_command_link_at_line_column,
    agent_shell_mcp_display_state_name, default_runtime_agent_prompt_input,
    runtime_agent_shell_display_output, runtime_agent_shell_visibility,
    runtime_primary_prompt_input,
};
pub(crate) use selection_adapter::{
    runtime_pane_agent_status_selector_keep_active_visible,
    runtime_pane_agent_status_selector_layout, runtime_selector_line,
};
pub(crate) use service::runtime_pane_agent_selector_rendition;
