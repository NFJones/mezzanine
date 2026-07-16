//! Mouse, command binding, config report, prompt, history, and placement records.

use super::*;

/// Carries Mouse Selection Drag State state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime) struct MouseSelectionDragState {
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_id: String,
    /// Stores the position value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub position: CopyPosition,
    /// Stores the origin position value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub origin_position: CopyPosition,
    /// Stores the autoscroll position value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub autoscroll_position: Option<CopyPosition>,
}

/// Last pane-content click used to recognize double-click word selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime) struct RuntimeMouseClickState {
    /// Pane whose content received the click.
    pub pane_id: String,
    /// Pane-local terminal cell clicked by the user.
    pub position: CopyPosition,
    /// Monotonic-enough wall-clock timestamp used for a small double-click window.
    pub clicked_at_unix_ms: u64,
}

/// Carries Mouse Resize Drag State state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime) enum MouseResizeDragState {
    /// Represents the Vertical case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Vertical {
        /// Stores the min column value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        min_column: u16,
        /// Stores the max column value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        max_column: u16,
        /// Stores the left indices value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        left_indices: Vec<usize>,
        /// Stores the right indices value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        right_indices: Vec<usize>,
        /// Stores the geometries value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        geometries: Vec<PaneGeometry>,
    },
    /// Represents the Horizontal case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Horizontal {
        /// Stores the min row value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        min_row: u16,
        /// Stores the max row value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        max_row: u16,
        /// Stores the row offset value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        row_offset: u16,
        /// Stores the top indices value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        top_indices: Vec<usize>,
        /// Stores the bottom indices value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        bottom_indices: Vec<usize>,
        /// Stores the geometries value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        geometries: Vec<PaneGeometry>,
    },
}

/// Carries Runtime Command Binding state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime) struct RuntimeCommandBinding {
    /// Stores the notation value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub notation: String,
    /// Stores the command value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub command: String,
    /// Stores the source layer value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub source_layer: String,
}

/// Carries Runtime Config Apply Report state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfigApplyReport {
    /// Stores the applied layers value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub applied_layers: Vec<String>,
    /// Stores the skipped layers value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub skipped_layers: Vec<String>,
    /// Stores the terminal history limit value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub terminal_history_limit: usize,
    /// Stores the terminal history rotation line count value for this data
    /// structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub terminal_history_rotate_lines: usize,
    /// Stores the terminal term value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub terminal_term: String,
    /// Stores the window frames enabled value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub window_frames_enabled: bool,
    /// Stores the pane frames enabled value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_frames_enabled: bool,
    /// Stores the max concurrent agents value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub max_concurrent_agents: usize,
    /// Stores the permission policy applied value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub permission_policy_applied: bool,
    /// Stores the mcp servers configured value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub mcp_servers_configured: usize,
    /// Stores the mcp servers blacklisted value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub mcp_servers_blacklisted: Vec<String>,
    /// Stores the providers configured value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub providers_configured: usize,
    /// Stores the model profiles configured value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub model_profiles_configured: usize,
    /// Stores the default model profile value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub default_model_profile: Option<String>,
    /// Stores the hooks configured value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub hooks_configured: usize,
    /// Stores the project trust prompts announced value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub project_trust_prompts_announced: usize,
    /// Stores the ui theme value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub ui_theme: String,
}

/// Carries Runtime Agent Prompt Turn Start state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeAgentPromptTurnStart {
    /// Stores the turn id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub turn_id: String,
    /// Stores the agent id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub agent_id: String,
    /// Stores the state value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub state: AgentTurnState,
    /// Stores the created at unix seconds value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub created_at_unix_seconds: u64,
    /// Stores the started at unix seconds value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub started_at_unix_seconds: Option<u64>,
    /// Stores the finished at unix seconds value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub finished_at_unix_seconds: Option<u64>,
    /// Stores the prompt preview value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub prompt_preview: String,
    /// Stores the approval ids value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub approval_ids: Vec<String>,
    /// Stores the result summary value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub result_summary: Option<String>,
    /// Stores the context blocks value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub context_blocks: usize,
}

/// Carries Runtime Agent Turn Stop state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeAgentTurnStop {
    /// Stores the turn id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub turn_id: String,
    /// Stores the scheduler cancelled value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub scheduler_cancelled: bool,
    /// Stores the interrupted shell transactions value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub interrupted_shell_transactions: usize,
    /// Stores the visibility value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub visibility: AgentShellVisibility,
}

/// Latest model-authored `say` action retained for user-facing copy operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime) struct RuntimeAgentCopyOutput {
    /// Stores the turn id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) turn_id: String,
    /// Raw `say.text` payload that should be copied without rendered prefixes.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) output: String,
    /// Declared `say.content_type` for pane-target rendering.
    ///
    /// Clipboard and paste-buffer targets use `output` directly, while pane
    /// output reuses the regular assistant renderer so markdown and diff
    /// content behaves like the original say action.
    pub(in crate::runtime) content_type: String,
}

/// Aggregated file-modification counts for one pane-local agent conversation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::runtime) struct RuntimeAgentModifiedFileSummary {
    /// Relative path presented to users.
    pub(in crate::runtime) path: String,
    /// Number of added lines observed across successful patch diffs.
    pub(in crate::runtime) added: usize,
    /// Number of removed lines observed across successful patch diffs.
    pub(in crate::runtime) removed: usize,
}

/// Runtime-local editable prompt and display state for one pane's agent shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime) struct RuntimeAgentPromptInput {
    /// Stores the prompt value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) prompt: ReadlinePrompt,
    /// Stores the decoder value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) decoder: ReadlineInputDecoder,
    /// Stores the display lines value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) display_lines: Vec<String>,
    /// First idle Ctrl+C timestamp waiting for the second confirmation press.
    ///
    /// Ctrl+C is easy to hit accidentally in a pane-local prompt. Idle prompt
    /// exit therefore requires a second Ctrl+C within a short window while
    /// active turns still use Ctrl+C as an immediate interrupt.
    pub(in crate::runtime) pending_ctrl_c_exit_at_unix_ms: Option<u64>,
}

/// Runtime-local editable prompt state for the primary command surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime) struct RuntimePrimaryPromptInput {
    /// Stores the prompt value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) prompt: ReadlinePrompt,
    /// Stores the decoder value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) decoder: ReadlineInputDecoder,
}

/// Carries Runtime Subagent Placement state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime) enum RuntimeSubagentPlacement {
    /// Represents the New Pane case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    NewPane {
        /// Stores the direction value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        direction: SplitDirection,
        /// Stores the select value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        select: bool,
    },
    /// Represents the New Window case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    NewWindow {
        /// Stores the name value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        name: String,
        /// Stores the select value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        select: bool,
    },
}
