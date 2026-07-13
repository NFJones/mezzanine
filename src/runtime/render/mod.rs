//! Runtime Render implementation.
//!
//! This module owns the runtime render boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::service_state::{
    RunningShellTransactionKind, RuntimeDisplayOverlay, RuntimeDisplayOverlaySearchMatch,
    RuntimeDisplayOverlaySelection, RuntimeDisplayOverlaySelectionKind, RuntimeMouseClickState,
    RuntimePaneAgentStatusSelector, RuntimePrimaryPromptInput,
};
use super::{
    AgentShellVisibility, AgentTurnRecord, AgentTurnState, AttachedClientStepApplication,
    AttachedTerminalClientStepPlan, ClientViewRole, CopyMode, CopyModeKeyAction, EventKind,
    MezError, MouseAction, MouseBorderCell, MousePaneRegion, MouseSelectionDragState,
    MouseWindowActionFrameCell, MouseWindowFrameCell, MuxAction, ObserverDecisionState,
    PaneDescriptor, PaneGeometry, PaneInputDispatch, PaneNavigationDirection, PasteBufferTarget,
    ReadlineInputDecoder, ReadlineOutcome, ReadlinePrompt, ReadlinePromptKind, RenderedClientView,
    Result, RuntimeAgentModifiedFileSummary, RuntimeAgentPromptInput, RuntimeSessionService,
    RuntimeSideEffect, Size, SplitDirection, TerminalClientLoopAction, TerminalClientLoopConfig,
    TerminalFrameContext, TerminalScreen, WindowFocusTarget, WindowFrameAction,
    agent_prompt_reserved_line_count, current_unix_millis, current_unix_seconds, json_escape,
    key_chord_input_bytes, mouse_action_name, mux_action_command_prompt_prefill, mux_action_name,
    pane_border_cells_for_geometries, pane_content_size_for_geometry,
    pane_frame_merges_into_divider, pane_navigation_direction,
    pane_render_region_size_for_geometry, parse_command_sequence, render_attached_client_view,
    rendered_pane_geometries, rendered_window_body_size, runtime_agent_shell_command_response_json,
    runtime_agent_turn_duration_display, runtime_agent_turn_state_name,
    runtime_approval_policy_name, runtime_copy_position_for_view, runtime_fit_status_line,
    runtime_paste_bytes, window_frame_action_pillbox_cells, window_frame_pillbox_cells,
};
/// Maximum elapsed time between two pane-content clicks recognized as a double click.
const DOUBLE_CLICK_WORD_SELECTION_WINDOW_MS: u64 = 500;
/// How long the copied-word highlight remains visible after a double click.
const DOUBLE_CLICK_WORD_SELECTION_HIGHLIGHT_MS: u64 = 500;

use crate::agent::{
    AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE, ActionResult, agent_output_content_type_is_diff,
    agent_output_content_type_is_markdown,
};
use crate::command::baseline_commands;
use crate::mcp::McpServerStatus;
use crate::readline::DEFAULT_READLINE_HISTORY_LIMIT;
use crate::selector::{
    SelectorCandidate, SelectorCandidateKind, SelectorExtraCandidate, SelectorSurface,
};
use crate::terminal::{
    CopyPosition, GraphicRendition, GroupFocusTarget, MousePaneAgentSelectorCell,
    MousePaneAgentStatusCell, PaneAgentStatusField, TerminalStyleSpan, TerminalStyledLine, UiTheme,
    WindowFrameCommandKind, compose_modal_display_overlay_lines,
    compose_prompt_overlay_presentation_with_styles, modal_display_overlay_max_scroll,
    modal_display_overlay_page_rows, pane_frame_agent_status_pillbox_cells,
    terminal_grapheme_width, terminal_graphemes, terminal_text_width,
    window_group_frame_pillbox_cells,
};
use crate::transcript::AgentPresentationEntry;
use mez_mux::presentation::{
    TerminalFramePosition, TerminalPaneFrameContext, TerminalWindowFrameContext,
    TerminalWindowGroupFrameContext, TerminalWindowStatusContext,
};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

mod attached_step;
mod client_view;
mod copy_mode;
mod geometry;
mod input;
mod mouse;
mod mux;
mod overlay;
mod paste;
mod presentation;
mod time;

use geometry::clipped_overlay_style_span;
use input::{
    RuntimeDisplayOverlayInputAction, RuntimeSelectorInputAction,
    runtime_display_overlay_input_action, runtime_selector_input_action,
    runtime_selector_step_index,
};
use overlay::*;
use presentation::*;
use time::{runtime_human_system_uptime, runtime_local_datetime_seconds_string};

// Attached terminal input application and client view rendering.

/// Root pane-agent display name shown in pane status surfaces.
const ROOT_AGENT_DISPLAY_NAME: &str = "manager";

/// Carries Mouse Pane Target state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MousePaneTarget {
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pane_id: String,
    /// Stores the position value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    position: CopyPosition,
}

/// Carries Mouse Selection Edge state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MouseSelectionEdge {
    /// Represents the Above case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Above,
    /// Represents the Below case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Below,
}

/// Carries Mouse Selection Target state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MouseSelectionTarget {
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pane_id: String,
    /// Stores the position value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    position: CopyPosition,
    /// Stores the edge value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    edge: Option<MouseSelectionEdge>,
}

impl MouseSelectionEdge {
    /// Runs the scroll delta operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn scroll_delta(self, origin: CopyPosition, current: CopyPosition) -> isize {
        let lines = origin.line.abs_diff(current.line).max(1);
        let lines = isize::try_from(lines).unwrap_or(isize::MAX);
        match self {
            MouseSelectionEdge::Above => -lines,
            MouseSelectionEdge::Below => lines,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::service_state::{
        RuntimeDisplayOverlay, RuntimeDisplayOverlaySearchMatch, RuntimeDisplayOverlaySelection,
        RuntimeDisplayOverlaySelectionKind,
    };
    use super::{
        AgentRenderedLine, AgentRenderedLineKind, RuntimeSessionService,
        agent_action_execution_display_header, agent_action_result_uses_diff_preview,
        agent_thinking_display_lines_for_width, command_preview_terminal_rendered_lines,
        readable_agent_diff_display_lines, readable_agent_diff_display_lines_for_width,
        render_command_markdown_body_lines, rendered_line_rendition_at,
        runtime_agent_shell_markdown_overlay_content, runtime_command_display_overlay_content,
        runtime_display_overlay_rendered_line_style_spans,
        runtime_display_overlay_rendered_selection_start,
        runtime_display_overlay_selection_prefix_columns, runtime_human_readable_display_lines,
        runtime_pane_agent_selector_rendition, wrap_agent_rendered_line_to_width,
        wrap_agent_terminal_text, wrapped_prefixed_agent_terminal_lines,
    };
    use crate::agent::{AgentAction, AgentActionPayload};
    use crate::layout::Size;
    use crate::terminal::{
        GraphicRendition, PaneAgentStatusField, TerminalStyleSpan, default_ui_theme,
    };

    /// Verifies normal-mode mutation result rendering treats patches as the
    /// only diff-producing file mutation operation.
    ///
    /// The semantic shell helper emits unified diffs for this action; this
    /// guard keeps the runtime display gate aligned so users see the readable
    /// change preview in normal logs.
    #[test]
    fn agent_action_result_diff_preview_includes_apply_patch_only() {
        let patch = AgentAction {
            id: "patch".to_string(),
            rationale: String::new(),
            payload: AgentActionPayload::ApplyPatch {
                patch: "*** Begin Patch\n*** Update File: note.txt\n@@\n-old\n+new\n*** End Patch"
                    .to_string(),
                strip: None,
            },
        };

        assert!(agent_action_result_uses_diff_preview(&patch));
    }

    /// Verifies memory actions render a concise one-line execution header.
    ///
    /// Memory search/store actions should retain the compact thinking-log feel
    /// while still showing enough bounded context to understand the query or
    /// stored record without opening verbose logs.
    #[test]
    fn agent_action_execution_header_summarizes_memory_actions() {
        let search = AgentAction {
            id: "memory-search".to_string(),
            rationale: String::new(),
            payload: AgentActionPayload::MemorySearch {
                query: "prompt cache details".to_string(),
                limit: Some(3),
            },
        };
        let store = AgentAction {
            id: "memory-store".to_string(),
            rationale: String::new(),
            payload: AgentActionPayload::MemoryStore {
                kind: "preference".to_string(),
                priority: Some(80),
                scope: Some("project".to_string()),
                keywords: vec!["prompt".to_string(), "cache".to_string()],
                content: "remember prompt cache details for future sessions".to_string(),
                expires_in_days: Some(7),
            },
        };

        assert_eq!(
            agent_action_execution_display_header(&search).as_deref(),
            Some("memory search: prompt cache details limit=3")
        );
        assert_eq!(
            agent_action_execution_display_header(&store).as_deref(),
            Some(
                "memory store: kind=preference keywords=2 content=remember prompt cache details for future sessions scope=project priority=80 ttl_days=7"
            )
        );
    }

    /// Verifies issue actions render compact argument-aware execution headers.
    ///
    /// Local issue actions should use the same concise action-line grammar as
    /// MCP and memory actions so users can see which issue operation and key
    /// arguments are being submitted without expanding verbose protocol logs.
    #[test]
    fn agent_action_execution_header_summarizes_issue_actions() {
        let add = AgentAction {
            id: "issue-add".to_string(),
            rationale: String::new(),
            payload: AgentActionPayload::IssueAdd {
                kind: "defect".to_string(),
                title: "Fix issue rendering".to_string(),
                body: Some("show useful issue arguments".to_string()),
                notes: Some("progress notes".to_string()),
                depends_on: Vec::new(),
            },
        };
        let update = AgentAction {
            id: "issue-update".to_string(),
            rationale: String::new(),
            payload: AgentActionPayload::IssueUpdate {
                id: "issue-123".to_string(),
                kind: Some("task".to_string()),
                state: Some("resolved".to_string()),
                title: Some("Update issue rendering".to_string()),
                body: None,
                clear_body: true,
                notes: Some("validated".to_string()),
                clear_notes: false,
                depends_on: None,
                clear_depends_on: false,
            },
        };
        let query = AgentAction {
            id: "issue-query".to_string(),
            rationale: String::new(),
            payload: AgentActionPayload::IssueQuery {
                kind: Some("task".to_string()),
                state: Some("open".to_string()),
                text: Some("rendering".to_string()),
                limit: Some(5),
            },
        };
        let delete = AgentAction {
            id: "issue-delete".to_string(),
            rationale: String::new(),
            payload: AgentActionPayload::IssueDelete {
                id: "issue-123".to_string(),
            },
        };

        assert_eq!(
            agent_action_execution_display_header(&add).as_deref(),
            Some(
                "issue add: kind=defect title=Fix issue rendering body=show useful issue arguments notes=progress notes"
            )
        );
        assert_eq!(
            agent_action_execution_display_header(&update).as_deref(),
            Some(
                "issue update: id=issue-123 kind=task title=Update issue rendering clear_body=true notes=validated"
            )
        );
        assert_eq!(
            agent_action_execution_display_header(&query).as_deref(),
            Some("issue query: kind=task state=open text=rendering limit=5")
        );
        assert_eq!(
            agent_action_execution_display_header(&delete).as_deref(),
            Some("issue delete: id=issue-123")
        );
    }

    /// Verifies semantic action diff output is parsed into compact display rows
    /// while removing Mezzanine-owned prompt and wrapper lines. This protects
    /// normal agent logs from showing raw PTY transaction mechanics around a
    /// filesystem change.
    #[test]
    fn readable_agent_diff_display_lines_parse_noisy_unified_diff() {
        let ui_theme = crate::terminal::deepforest_ui_theme();
        let lines = readable_agent_diff_display_lines(
            "\n∙\nMEZ_RESTORE_NOUNSET_NOW=$MEZ_RESTORE_NOUNSET\n\
             diff -- update file\n--- a/src/runtime/agent.rs\n+++ b/src/runtime/agent.rs\n\
             @@ -10,3 +10,3 @@\n context\n-old\n+new\n\
             @@ -20,2 +20,2 @@\n-again\n+done\n\n",
            &ui_theme,
        )
        .into_iter()
        .map(|line| line.display)
        .collect::<Vec<_>>();

        assert_eq!(
            lines,
            vec![
                "--- src/runtime/agent.rs",
                "+++ src/runtime/agent.rs",
                "@@ -10,3 +10,3 @@",
                "    10     10  context",
                "    11        -old",
                "           11 +new",
                "@@ -20,2 +20,2 @@",
                "    20        -again",
                "           20 +done",
            ]
        );
    }

    /// Verifies cleaned semantic diff output preserves valid blank context rows
    /// and body text that resembles Mezzanine shell-wrapper traffic.
    ///
    /// Unified diffs encode an unchanged blank line as a single leading space,
    /// and user changes can legitimately contain strings such as `MEZ_STATUS`.
    /// The preview cleaner should remove wrapper echoes around the diff without
    /// making the parsed diff lossy once hunk body parsing has started.
    #[test]
    fn readable_agent_diff_display_lines_preserve_diff_body_blank_and_wrapper_text() {
        let ui_theme = crate::terminal::deepforest_ui_theme();
        let lines = readable_agent_diff_display_lines(
            "diff -- update file\n--- a/src/config.txt\n+++ b/src/config.txt\n\
             @@ -1,3 +1,3 @@\n \n-MEZ_STATUS=old\n+unset MEZ_STATUS\n",
            &ui_theme,
        )
        .into_iter()
        .map(|line| line.display)
        .collect::<Vec<_>>();

        assert_eq!(
            lines,
            vec![
                "--- src/config.txt",
                "+++ src/config.txt",
                "@@ -1,3 +1,3 @@",
                "     1      1  ",
                "     2        -MEZ_STATUS=old",
                "            2 +unset MEZ_STATUS",
            ]
        );
    }

    /// Verifies readable diff rows wrap to the supplied display width.
    ///
    /// Diff output should follow the same readability cap as other agent output:
    /// wrap at a prior space and indent continuation rows under the diff gutter,
    /// while leaving unbreakable long words for the pane to wrap naturally.
    #[test]
    fn readable_agent_diff_display_lines_wrap_at_spaces_only() {
        let ui_theme = crate::terminal::deepforest_ui_theme();
        let lines = readable_agent_diff_display_lines_for_width(
            "diff -- update file\n--- a/src/main.rs\n+++ b/src/main.rs\n\
             @@ -1,1 +1,1 @@\n+alpha beta gamma delta epsilon zeta\n\
             +averyveryverylongunbreakabletoken\n",
            &ui_theme,
            32,
        )
        .into_iter()
        .map(|line| line.display)
        .collect::<Vec<_>>();

        assert!(
            lines
                .iter()
                .any(|line| line == "            1 +alpha beta gamma"),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "          delta epsilon zeta"),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("averyveryverylongunbre")),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.contains("akabletoken")),
            "{lines:?}"
        );
    }

    /// Verifies path-only mutation previews are rendered as concise summaries
    /// rather than raw `diff -- delete path` blocks. Directory and missing-path
    /// changes use this preview format instead of unified hunks.
    #[test]
    fn readable_agent_diff_display_lines_parse_path_delta() {
        let ui_theme = crate::terminal::deepforest_ui_theme();
        let lines = readable_agent_diff_display_lines("diff -- delete path\n- a.txt\n", &ui_theme)
            .into_iter()
            .map(|line| line.display)
            .collect::<Vec<_>>();

        assert_eq!(lines, vec!["• Deleted a.txt (+0 -1)", "         - a.txt"]);
    }

    /// Verifies parsed unified diffs carry syntax token spans for known file
    /// types in addition to the structural diff gutter. This protects the
    /// rendered presentation from regressing to whole-line addition/deletion
    /// coloring once a path provides enough information to pick a syntax.
    #[test]
    fn readable_agent_diff_display_lines_highlight_known_file_type() {
        let mut definition = crate::terminal::builtin_ui_theme_definition("deepforest").unwrap();
        definition
            .colors
            .insert("syntax_type_fg".to_string(), "#010203".to_string());
        let ui_theme = crate::terminal::resolve_ui_theme("syntax-test", definition).unwrap();
        let lines = readable_agent_diff_display_lines(
            "diff -- update file\n--- a/src/main.rs\n+++ b/src/main.rs\n\
             @@ -1,1 +1,1 @@\n-fn old() {}\n+fn new() {}\n",
            &ui_theme,
        );
        let addition = lines
            .iter()
            .find(|line| line.display.contains("+fn new() {}"))
            .unwrap();

        assert!(
            addition
                .style_spans
                .iter()
                .any(|span| span.start >= 15 && span.rendition.foreground.is_some()),
            "{addition:?}"
        );
        assert!(
            addition.style_spans.iter().any(|span| {
                span.start >= 15
                    && matches!(
                        span.rendition.foreground,
                        Some(foreground)
                            if foreground == ui_theme.colors.syntax_keyword.foreground
                                || foreground == ui_theme.colors.syntax_type.foreground
                                || foreground == ui_theme.colors.syntax_function.foreground
                                || foreground == ui_theme.colors.syntax_plain.foreground
                    )
            }),
            "syntax keyword spans should follow the active theme palette: {addition:?}"
        );
    }

    /// Verifies shell command previews use the same theme-backed syntax
    /// highlighter as diff bodies while preserving the existing `$` prompt
    /// prefix. This protects normal command logs from losing syntax spans when
    /// commands are rendered without separate assistant summary lines.
    #[test]
    fn command_preview_terminal_rendered_lines_highlight_shell_syntax() {
        let mut definition = crate::terminal::builtin_ui_theme_definition("deepforest").unwrap();
        definition
            .colors
            .insert("syntax_keyword_fg".to_string(), "#010203".to_string());
        let ui_theme = crate::terminal::resolve_ui_theme("syntax-test", definition).unwrap();
        let lines = command_preview_terminal_rendered_lines(
            "if true; then echo \"ok\"; fi",
            80,
            10,
            crate::agent::ShellClassification::Bash,
            &ui_theme,
        );

        assert_eq!(
            lines
                .iter()
                .map(|line| line.display.as_str())
                .collect::<Vec<_>>(),
            vec!["$ if true; then echo \"ok\"; fi"]
        );
        assert!(lines[0].style_spans.iter().any(|span| {
            span.start >= 2
                && span.rendition.foreground == Some(mez_terminal::TerminalColor::Rgb(1, 2, 3))
        }));
    }

    /// Verifies command previews wrap at a whitespace boundary before the
    /// display limit instead of splitting a word at the exact column. This keeps
    /// shell command logs readable on narrow panes while preserving the existing
    /// prompt prefix and continuation indentation behavior.
    #[test]
    fn command_preview_wraps_at_space_before_boundary() {
        assert_eq!(
            wrap_agent_terminal_text("alpha beta gamma", 12),
            vec!["alpha beta".to_string(), "gamma".to_string()]
        );
    }

    /// Verifies command previews fall back to the exact display boundary when
    /// no whitespace boundary exists before the display limit.
    ///
    /// Word boundaries keep ordinary commands readable, but unbroken text still
    /// needs bounded rows so terminal rendering remains stable.
    #[test]
    fn command_preview_hard_wraps_unbroken_tokens_when_needed() {
        assert_eq!(
            wrap_agent_terminal_text("aaaaaaaaaaaaaaaa", 8),
            vec!["aaaaaaaa".to_string(), "aaaaaaaa".to_string()]
        );
    }

    /// Verifies agent thinking lines wrap to the bounded pane width and indent
    /// continuations after the `thinking:` label. This keeps rationale output
    /// readable without relying on terminal soft wrapping for normal text.
    #[test]
    fn agent_thinking_lines_wrap_with_label_indent() {
        assert_eq!(
            agent_thinking_display_lines_for_width("alpha beta gamma", 18),
            vec![
                "thinking: alpha".to_string(),
                "          beta".to_string(),
                "          gamma".to_string()
            ]
        );
    }

    /// Verifies compact routing records render as terse sentences in
    /// normal agent logs instead of exposing raw key/value fields.
    #[test]
    fn human_readable_display_lines_format_routing_sentence() {
        assert_eq!(
            runtime_human_readable_display_lines(
                "pane=%1 enabled=true default=false changed=true source=runtime-routing"
            ),
            vec!["routing is enabled for pane %1; default is disabled; changed.".to_string()]
        );
    }

    /// Verifies compact runtime-policy records render as direct status
    /// statements so approval changes are easier to scan in the pane log.
    #[test]
    fn human_readable_display_lines_format_policy_sentence() {
        assert_eq!(
            runtime_human_readable_display_lines(
                "field=approval_policy:current=ask:requested=full-access:authority_change=broadening:approval_required=true:approved_by=primary-command:changed=true:source=runtime-policy"
            ),
            vec![
                "approval policy changed from ask to full-access; authority broadening approved by primary-command.".to_string()
            ]
        );
    }

    /// Verifies agent-say copy rows render as a sentence rather than raw
    /// key/value fields with internal runtime source metadata.
    #[test]
    fn human_readable_display_lines_format_agent_say_copy_sentence() {
        assert_eq!(
            runtime_human_readable_display_lines(
                "target=%1:say=written:destination=buffer:buffer=agent-output:turn=turn-3:lines=1:bytes=38:source=runtime-agent-say"
            ),
            vec!["copied 38 bytes from turn-3 to buffer agent-output.".to_string()]
        );
        assert_eq!(
            runtime_human_readable_display_lines(
                "target=%1:say=not-written:reason=no-say-action:source=runtime-agent-say"
            ),
            vec!["agent say text was not copied: no-say-action.".to_string()]
        );
        assert_eq!(
            runtime_human_readable_display_lines(
                "target=%1:say=written:destination=clipboard:buffer=clipboard:turn=turn-3:lines=1:bytes=38:source=runtime-agent-say"
            ),
            vec!["copied 38 bytes from turn-3 to clipboard.".to_string()]
        );
    }

    /// Verifies transcript fork rows render in user terms while preserving the
    /// conversation and pane identifiers needed to reason about where content
    /// moved.
    #[test]
    fn human_readable_display_lines_format_agent_fork_sentence() {
        assert_eq!(
            runtime_human_readable_display_lines(
                "source=17aeaf99 conversation_id=ca770d entries=4 source_pane=%2 pane=%4 forked=true"
            ),
            vec!["forked 4 transcript entries from %2 into %4; conversation ca770d.".to_string()]
        );
    }

    /// Verifies markdown presentation wraps at a prior whitespace boundary and
    /// indents continuation rows after the agent prompt. This protects rendered
    /// markdown from drifting away from the element-aligned wrapping expected
    /// in agent transcript panes.
    #[test]
    fn markdown_presentation_wraps_at_space_with_continuation_indent() {
        let wrapped = wrap_agent_rendered_line_to_width(
            AgentRenderedLine {
                display: "mez> alpha beta gamma".to_string(),
                style_spans: Vec::new(),
                copy_text: None,
                kind: AgentRenderedLineKind::Normal,
            },
            18,
        )
        .into_iter()
        .map(|line| line.display)
        .collect::<Vec<_>>();

        assert_eq!(
            wrapped,
            vec!["mez> alpha beta".to_string(), "     gamma".to_string()]
        );
    }

    /// Verifies markdown presentation preserves an overflowing unbroken token.
    ///
    /// The markdown contract asks non-table prose to avoid inserting hard
    /// splits when there is no usable whitespace boundary, leaving terminal
    /// soft wrapping to handle the long token.
    #[test]
    fn markdown_presentation_preserves_unbroken_token_after_prompt() {
        let wrapped = wrap_agent_rendered_line_to_width(
            AgentRenderedLine {
                display: "mez> aaaaaaaaaaaaaaaa".to_string(),
                style_spans: Vec::new(),
                copy_text: None,
                kind: AgentRenderedLineKind::Normal,
            },
            12,
        )
        .into_iter()
        .map(|line| line.display)
        .collect::<Vec<_>>();

        assert_eq!(wrapped, vec!["mez> aaaaaaaaaaaaaaaa".to_string()]);
    }

    /// Verifies a leading grapheme wider than the segment is made representable.
    ///
    /// A leading two-cell grapheme cannot fit in a one-cell wrapping segment.
    /// The wrapper should consume it with a one-cell placeholder instead of
    /// emitting a row that exceeds the segment before any progress is possible.
    #[test]
    fn markdown_presentation_replaces_overwide_leading_grapheme() {
        let wrapped = wrap_agent_rendered_line_to_width(
            AgentRenderedLine {
                display: "漢abc".to_string(),
                style_spans: Vec::new(),
                copy_text: None,
                kind: AgentRenderedLineKind::Normal,
            },
            1,
        )
        .into_iter()
        .map(|line| line.display)
        .collect::<Vec<_>>();

        assert_eq!(wrapped, vec!["…".to_string(), "abc".to_string()]);
    }

    /// Verifies command overlay markdown keeps internal `mez-agent:` links
    /// selectable without rendering their destination text.
    ///
    /// Saved-session rows use these links for clickable `/resume` commands, but
    /// the visible list should show the bold session UUID rather than a
    /// parenthesized implementation URI.
    #[test]
    fn agent_shell_markdown_overlay_hides_internal_agent_link_destinations() {
        let theme = default_ui_theme();
        let content = runtime_agent_shell_markdown_overlay_content(
            Some("list-sessions".to_string()),
            "- [**saved-session**](mez-agent:/resume%20saved-session)",
            &theme,
        );

        assert_eq!(content.lines, vec!["• saved-session".to_string()]);
        assert_eq!(content.selections.len(), 1);
        assert_eq!(content.selections[0].command, "/resume saved-session");
        assert_eq!(content.selections[0].start_column, 2);
        assert_eq!(content.selections[0].width, "saved-session".len());
    }

    /// Verifies plain assistant text uses the same prompt-aligned continuation
    /// indentation as markdown output.
    #[test]
    fn plain_agent_output_wraps_under_agent_indicator() {
        let wrapped = wrapped_prefixed_agent_terminal_lines("mez> ", "alpha beta gamma delta", 18)
            .into_iter()
            .map(|line| line.display)
            .collect::<Vec<_>>();

        assert_eq!(
            wrapped,
            vec![
                "mez> alpha beta".to_string(),
                "     gamma delta".to_string()
            ]
        );
    }

    /// Verifies unknown file types still render readable diff rows.
    ///
    /// Syntax highlighting is an enhancement over the structural diff display.
    /// Unsupported extensions should keep the line-number gutter and diff
    /// marker coloring instead of dropping the changed line or panicking while
    /// resolving a syntax.
    #[test]
    fn readable_agent_diff_display_lines_falls_back_for_unknown_file_type() {
        let ui_theme = crate::terminal::deepforest_ui_theme();
        let lines = readable_agent_diff_display_lines(
            "diff -- update file\n--- a/file.unknown-mez\n+++ b/file.unknown-mez\n\
             @@ -1,1 +1,1 @@\n-old value\n+new value\n",
            &ui_theme,
        );
        let addition = lines
            .iter()
            .find(|line| line.display.contains("+new value"))
            .unwrap();

        assert_eq!(addition.display, "            1 +new value");
        assert!(
            addition.style_spans.iter().all(|span| span.start == 0),
            "{addition:?}"
        );
    }

    /// Verifies command markdown can color compact diff counts.
    ///
    /// `/list-modified-files` emits compact markdown rows with renderer-owned
    /// span classes so additions and removals stay visually distinct without
    /// forcing that command into a bespoke renderer.
    #[test]
    fn command_markdown_renders_modified_file_count_spans() {
        let ui_theme = crate::terminal::deepforest_ui_theme();
        let lines = render_command_markdown_body_lines(
            "- edited `src/lib.rs` (<span class=\"mez-diff-addition\">+12</span> <span class=\"mez-diff-deletion\">-3</span>)",
            &ui_theme,
        );
        let line = lines
            .iter()
            .find(|line| line.display.contains("+12") && line.display.contains("-3"))
            .unwrap();

        assert!(
            line.style_spans.iter().any(|span| {
                span.rendition.foreground == Some(ui_theme.colors.agent_transcript_user.foreground)
                    && span.rendition.bold
            }),
            "{line:?}"
        );
        assert!(
            line.style_spans.iter().any(|span| {
                span.rendition.foreground == Some(ui_theme.colors.agent_transcript_error.foreground)
                    && span.rendition.bold
            }),
            "{line:?}"
        );
    }

    /// Verifies apply-patch diff previews follow the active theme while keeping
    /// one render's resolved colors stable across the preview.
    ///
    /// This regression protects semantic diff output from borrowing pane-focus
    /// overlays while still requiring the renderer to use the active resolved
    /// transcript and syntax colors for diff gutters and file-aware syntax spans.
    #[test]
    fn readable_agent_diff_display_lines_follow_active_theme_palette() {
        let mut definition = crate::terminal::builtin_ui_theme_definition("deepforest").unwrap();
        definition.colors.insert(
            "agent_transcript_user_fg".to_string(),
            "#010203".to_string(),
        );
        definition.colors.insert(
            "agent_transcript_error_fg".to_string(),
            "#040506".to_string(),
        );
        definition.colors.insert(
            "agent_transcript_status_fg".to_string(),
            "#070809".to_string(),
        );
        definition
            .colors
            .insert("syntax_keyword_fg".to_string(), "#0a0b0c".to_string());
        definition
            .colors
            .insert("syntax_plain_fg".to_string(), "#0d0e0f".to_string());
        definition
            .colors
            .insert("syntax_type_fg".to_string(), "#101112".to_string());
        definition
            .colors
            .insert("syntax_function_fg".to_string(), "#131415".to_string());
        definition
            .colors
            .insert("syntax_operator_fg".to_string(), "#161718".to_string());
        let ui_theme = crate::terminal::resolve_ui_theme("constant-diff-test", definition).unwrap();
        let lines = readable_agent_diff_display_lines(
            "diff -- update file\n--- a/src/main.rs\n+++ b/src/main.rs\n\
             @@ -1,1 +1,1 @@\n-old_value()\n+fn new_value() {}\n",
            &ui_theme,
        );
        let addition = lines
            .iter()
            .find(|line| line.display.contains("+fn new_value() {}"))
            .unwrap();
        let deletion = lines
            .iter()
            .find(|line| line.display.contains("-old_value()"))
            .unwrap();

        assert!(
            addition.style_spans.iter().any(|span| {
                span.start == 0
                    && span.length == addition.display.chars().count()
                    && span.rendition.foreground == Some(mez_terminal::TerminalColor::Rgb(1, 2, 3))
            }),
            "{addition:?}"
        );
        assert!(
            deletion.style_spans.iter().any(|span| {
                span.start == 0
                    && span.length == deletion.display.chars().count()
                    && span.rendition.foreground == Some(mez_terminal::TerminalColor::Rgb(4, 5, 6))
            }),
            "{deletion:?}"
        );
        assert!(
            addition.style_spans.iter().any(|span| {
                span.start >= 15
                    && matches!(
                        span.rendition.foreground,
                        Some(
                            mez_terminal::TerminalColor::Rgb(10, 11, 12)
                                | mez_terminal::TerminalColor::Rgb(13, 14, 15)
                                | mez_terminal::TerminalColor::Rgb(16, 17, 18)
                                | mez_terminal::TerminalColor::Rgb(19, 20, 21)
                                | mez_terminal::TerminalColor::Rgb(22, 23, 24)
                        )
                    )
            }),
            "{addition:?}"
        );
        assert!(
            addition.style_spans.iter().all(|span| {
                span.start == 0
                    || matches!(
                        span.rendition.foreground,
                        Some(
                            mez_terminal::TerminalColor::Rgb(10, 11, 12)
                                | mez_terminal::TerminalColor::Rgb(13, 14, 15)
                                | mez_terminal::TerminalColor::Rgb(16, 17, 18)
                                | mez_terminal::TerminalColor::Rgb(19, 20, 21)
                                | mez_terminal::TerminalColor::Rgb(22, 23, 24)
                        )
                    )
            }),
            "{addition:?}"
        );
    }

    /// Verifies agent slash markdown shown in the command overlay keeps
    /// `mez-agent:` links selectable after markdown rendering. This preserves
    /// `/list-sessions` resume links while moving informational slash output
    /// out of the pane transcript.
    #[test]
    fn agent_shell_markdown_overlay_preserves_agent_links() {
        let ui_theme = crate::terminal::deepforest_ui_theme();
        let content = runtime_agent_shell_markdown_overlay_content(
            Some("list-sessions".to_string()),
            "- [`saved`](mez-agent:%2Fresume%20saved)",
            &ui_theme,
        );

        assert_eq!(content.command.as_deref(), Some("list-sessions"));
        assert!(
            content
                .lines
                .iter()
                .any(|line| line.contains("saved") && !line.contains("mez-agent:")),
            "{content:?}"
        );
        assert!(
            content
                .selections
                .iter()
                .any(|selection| selection.command == "/resume saved"),
            "{content:?}"
        );
        assert_eq!(
            content
                .selections
                .iter()
                .filter(|selection| selection.command == "/resume saved")
                .count(),
            1,
            "{content:?}"
        );
    }
    /// Verifies selectable pager links keep the markdown link styling emitted
    /// by the CommonMark renderer.
    ///
    /// `/list-sessions` and similar markdown-backed command overlays should
    /// keep links readable as ordinary text links while remaining keyboard and
    /// mouse selectable, so the overlay must retain the rendered line spans in
    /// addition to the selection metadata.
    #[test]
    fn agent_shell_markdown_overlay_preserves_selectable_link_style_spans() {
        let ui_theme = crate::terminal::deepforest_ui_theme();
        let content = runtime_agent_shell_markdown_overlay_content(
            Some("list-sessions".to_string()),
            "- [`saved`](mez-agent:%2Fresume%20saved)",
            &ui_theme,
        );
        assert_eq!(content.selections.len(), 1, "{content:?}");
        let selection = &content.selections[0];
        let line = content.lines.get(selection.line_index).unwrap();
        let column = runtime_display_overlay_rendered_selection_start(
            &RuntimeDisplayOverlay {
                lines: content.lines.clone(),
                line_style_spans: content.line_style_spans.clone(),
                scroll_offset: 0,
                selections: content.selections.clone(),
                active_selection_index: Some(0),
                dismiss_on_any_input: false,
                search_input: None,
                search_query: None,
                search_match: None,
                search_status: None,
                mouse_selection: None,
                record_browser: None,
            },
            selection,
        );
        assert_eq!(&line[column..column + selection.width], "saved");
        assert!(
            content.line_style_spans[selection.line_index]
                .iter()
                .any(|span| {
                    span.start == selection.start_column
                        && span.length == selection.width
                        && span.rendition.bold
                        && span.rendition.underline
                        && !span.rendition.inverse
                        && span.rendition.background.is_none()
                        && span.rendition.foreground
                            == Some(ui_theme.colors.agent_transcript_command.foreground)
                }),
            "{content:?}"
        );
    }
    /// Verifies an active pager link keeps link styling on every rendered cell.
    ///
    /// Selected command-overlay links layer selector and markdown spans on the
    /// same columns. The final rendered row must preserve the markdown link
    /// rendition through the last link character instead of letting the
    /// fallback selection span leak onto the tail cell.
    #[test]
    fn active_markdown_overlay_link_keeps_tail_cell_link_styling() {
        let ui_theme = crate::terminal::deepforest_ui_theme();
        let content = runtime_agent_shell_markdown_overlay_content(
            Some("list-sessions".to_string()),
            "- [`saved`](mez-agent:%2Fresume%20saved)",
            &ui_theme,
        );
        let overlay = RuntimeDisplayOverlay {
            lines: content.lines.clone(),
            line_style_spans: content.line_style_spans.clone(),
            scroll_offset: 0,
            selections: content.selections.clone(),
            active_selection_index: Some(0),
            dismiss_on_any_input: false,
            search_input: None,
            search_query: None,
            search_match: None,
            search_status: None,
            mouse_selection: None,
            record_browser: None,
        };
        let selection = &overlay.selections[0];
        let start = runtime_display_overlay_rendered_selection_start(&overlay, selection);
        let spans = runtime_display_overlay_rendered_line_style_spans(&overlay, 0, 80, &ui_theme);
        for column in start..start.saturating_add(selection.width) {
            let rendition = rendered_line_rendition_at(&spans, column);
            assert!(
                rendition.bold,
                "column {column} lost bold styling: {spans:?}"
            );
            assert!(
                rendition.underline,
                "column {column} lost underline styling: {spans:?}"
            );
            assert!(
                !rendition.inverse,
                "column {column} became inverse: {spans:?}"
            );
            assert_eq!(
                rendition.background,
                Some(ui_theme.colors.agent_model.background),
                "column {column} lost active selection background: {spans:?}"
            );
            assert_eq!(
                rendition.foreground,
                Some(ui_theme.colors.agent_transcript_command.foreground),
                "column {column} lost link foreground: {spans:?}"
            );
        }
    }
    /// Verifies an active saved-session UUID row keeps link styling on the
    /// final visible UUID character.
    ///
    /// `/list-sessions` rows are emitted as hidden `mez-agent:` resume links
    /// with bold UUID labels. The command overlay must preserve that link
    /// rendition across the full visible UUID when the row is selected,
    /// including the final character that previously fell back to plain text.
    #[test]
    fn active_saved_session_overlay_uuid_keeps_tail_cell_link_styling() {
        let ui_theme = crate::terminal::deepforest_ui_theme();
        let session_id = "018f6b3a-1b2c-7000-9000-cafebabefeed";
        let content = runtime_agent_shell_markdown_overlay_content(
            Some("list-sessions".to_string()),
            &format!("- [**{session_id}**](mez-agent:%2Fresume%20{session_id})"),
            &ui_theme,
        );
        let overlay = RuntimeDisplayOverlay {
            lines: content.lines.clone(),
            line_style_spans: content.line_style_spans.clone(),
            scroll_offset: 0,
            selections: content.selections.clone(),
            active_selection_index: Some(0),
            dismiss_on_any_input: false,
            search_input: None,
            search_query: None,
            search_match: None,
            search_status: None,
            mouse_selection: None,
            record_browser: None,
        };
        let selection = &overlay.selections[0];
        let start = runtime_display_overlay_rendered_selection_start(&overlay, selection);
        let spans = runtime_display_overlay_rendered_line_style_spans(&overlay, 0, 120, &ui_theme);
        for column in start..start.saturating_add(selection.width) {
            let rendition = rendered_line_rendition_at(&spans, column);
            assert!(
                rendition.bold,
                "column {column} lost bold styling: {spans:?}"
            );
            assert!(
                rendition.underline,
                "column {column} lost underline styling: {spans:?}"
            );
            assert!(
                !rendition.inverse,
                "column {column} became inverse: {spans:?}"
            );
            assert_eq!(
                rendition.background,
                Some(ui_theme.colors.agent_model.background),
                "column {column} lost active selection background: {spans:?}"
            );
            assert_eq!(
                rendition.foreground,
                Some(ui_theme.colors.agent_transcript_command.foreground),
                "column {column} lost link foreground: {spans:?}"
            );
        }
    }

    /// Verifies an active saved-session UUID row does not shift link styling
    /// onto the preceding bullet separator cell.
    ///
    /// `/resume` opens a selectable saved-session pager whose rows render as a
    /// bullet plus a bold linked UUID label. The selected-link foreground,
    /// underline, and active background must begin on the first UUID cell
    /// rather than leaking one column left onto the separator space.
    #[test]
    fn active_saved_session_overlay_uuid_does_not_style_previous_cell() {
        let ui_theme = crate::terminal::deepforest_ui_theme();
        let session_id = "018f6b3a-1b2c-7000-9000-cafebabefeed";
        let content = runtime_agent_shell_markdown_overlay_content(
            Some("list-sessions".to_string()),
            &format!("- [**{session_id}**](mez-agent:%2Fresume%20{session_id})"),
            &ui_theme,
        );
        let overlay = RuntimeDisplayOverlay {
            lines: content.lines.clone(),
            line_style_spans: content.line_style_spans.clone(),
            scroll_offset: 0,
            selections: content.selections.clone(),
            active_selection_index: Some(0),
            dismiss_on_any_input: false,
            search_input: None,
            search_query: None,
            search_match: None,
            search_status: None,
            mouse_selection: None,
            record_browser: None,
        };
        let selection = &overlay.selections[0];
        let start = runtime_display_overlay_rendered_selection_start(&overlay, selection);
        let spans = runtime_display_overlay_rendered_line_style_spans(&overlay, 0, 120, &ui_theme);
        let previous_rendition = rendered_line_rendition_at(&spans, start.saturating_sub(1));

        assert_ne!(
            previous_rendition.foreground,
            Some(ui_theme.colors.agent_transcript_command.foreground),
            "saved-session link foreground shifted left into the separator cell: {spans:?}"
        );
        assert!(
            !previous_rendition.underline,
            "saved-session link underline shifted left into the separator cell: {spans:?}"
        );
        assert_ne!(
            previous_rendition.background,
            Some(ui_theme.colors.agent_model.background),
            "saved-session active background shifted left into the separator cell: {spans:?}"
        );
    }

    /// Verifies the active selector gutter stays isolated from a link that
    /// begins at the first visible body column.
    ///
    /// `/status` renders some selectable links without a list-prefix gap. When
    /// the active row's selector gutter abuts that first link cell, the gutter
    /// must remain a standalone styled cell so the link highlight does not
    /// visually shift left into the gutter column.
    #[test]
    fn active_markdown_overlay_front_of_line_link_keeps_gutter_separate() {
        let ui_theme = crate::terminal::deepforest_ui_theme();
        let content = runtime_agent_shell_markdown_overlay_content(
            Some("status".to_string()),
            "[`saved`](mez-agent:%2Fresume%20saved)",
            &ui_theme,
        );
        let overlay = RuntimeDisplayOverlay {
            lines: content.lines.clone(),
            line_style_spans: content.line_style_spans.clone(),
            scroll_offset: 0,
            selections: content.selections.clone(),
            active_selection_index: Some(0),
            dismiss_on_any_input: false,
            search_input: None,
            search_query: None,
            search_match: None,
            search_status: None,
            mouse_selection: None,
            record_browser: None,
        };
        let selection = &overlay.selections[0];
        let start = runtime_display_overlay_rendered_selection_start(&overlay, selection);
        let spans = runtime_display_overlay_rendered_line_style_spans(&overlay, 0, 80, &ui_theme);
        assert_eq!(
            start,
            runtime_display_overlay_selection_prefix_columns(),
            "{spans:?}"
        );
        assert!(
            spans.iter().any(|span| {
                span.start == 0 && span.length == runtime_display_overlay_selection_prefix_columns()
            }),
            "missing isolated selector gutter span: {spans:?}"
        );
        let gutter_rendition = rendered_line_rendition_at(&spans, 0);
        let gutter_trailing_rendition = rendered_line_rendition_at(&spans, start - 1);
        let first_link_rendition = rendered_line_rendition_at(&spans, start);
        assert_eq!(
            gutter_rendition.foreground, None,
            "gutter inherited selected-link foreground styling: {spans:?}"
        );
        assert!(
            !gutter_rendition.bold,
            "gutter inherited bold link styling: {spans:?}"
        );
        assert!(
            !gutter_rendition.underline,
            "gutter inherited underline link styling: {spans:?}"
        );
        assert_eq!(
            gutter_rendition.background, None,
            "gutter picked up active body highlight: {spans:?}"
        );
        assert_eq!(
            gutter_trailing_rendition.foreground, None,
            "selector gutter trailing cell inherited selected-link foreground styling: {spans:?}"
        );
        assert!(
            !gutter_trailing_rendition.bold,
            "selector gutter trailing cell inherited bold link styling: {spans:?}"
        );
        assert!(
            !gutter_trailing_rendition.underline,
            "selector gutter trailing cell inherited underline link styling: {spans:?}"
        );
        assert_eq!(
            gutter_trailing_rendition.background, None,
            "selector gutter trailing cell picked up active body highlight: {spans:?}"
        );
        assert_eq!(
            first_link_rendition.foreground,
            Some(ui_theme.colors.agent_transcript_command.foreground),
            "front-of-line link styling shifted into the gutter: {spans:?}"
        );
        assert_eq!(
            first_link_rendition.background,
            Some(ui_theme.colors.agent_model.background),
            "front-of-line link lost active body highlight: {spans:?}"
        );
        assert!(
            first_link_rendition.underline,
            "front-of-line link lost underline: {spans:?}"
        );
    }

    /// Verifies selected-link styling stops at the selected link boundary.
    ///
    /// Active selected-link spans should preserve link foreground and underline
    /// on the link body without leaking that rendition into the following
    /// display cell, because cursor presentation and adjacent overlay text are
    /// composed after the selected-link span list.
    #[test]
    fn active_markdown_overlay_link_style_stops_before_following_cell() {
        let ui_theme = crate::terminal::deepforest_ui_theme();
        let content = runtime_agent_shell_markdown_overlay_content(
            Some("status".to_string()),
            "[`saved`](mez-agent:%2Fresume%20saved) next",
            &ui_theme,
        );
        let overlay = RuntimeDisplayOverlay {
            lines: content.lines.clone(),
            line_style_spans: content.line_style_spans.clone(),
            scroll_offset: 0,
            selections: content.selections.clone(),
            active_selection_index: Some(0),
            dismiss_on_any_input: false,
            search_input: None,
            search_query: None,
            search_match: None,
            search_status: None,
            mouse_selection: None,
            record_browser: None,
        };
        let selection = &overlay.selections[0];
        let start = runtime_display_overlay_rendered_selection_start(&overlay, selection);
        let following_column = start.saturating_add(selection.width);
        let spans = runtime_display_overlay_rendered_line_style_spans(&overlay, 0, 80, &ui_theme);
        let following_rendition = rendered_line_rendition_at(&spans, following_column);
        assert_ne!(
            following_rendition.foreground,
            Some(ui_theme.colors.agent_transcript_command.foreground),
            "link foreground leaked past selected link: {spans:?}"
        );
        assert!(
            !following_rendition.underline,
            "link underline leaked past selected link: {spans:?}"
        );
        assert_eq!(
            following_rendition.background, None,
            "active selection background leaked past selected link: {spans:?}"
        );
    }

    /// Verifies pane-agent selector rows fully replace overlapping pane text styling.
    ///
    /// Selector dropdown rows render over live pane content whose cells may
    /// already carry bold, underline, or inverse attributes. The overlay must
    /// clip any overlapping pane spans and provide a clean selector rendition
    /// so per-cell span merging cannot leak those underlying attributes into
    /// the dropdown text.
    #[test]
    fn pane_agent_selector_overlay_clips_underlying_pane_styling() {
        let ui_theme = crate::terminal::deepforest_ui_theme();
        let overlay_column = 4;
        let overlay_width = 8;
        let pane_rendition = GraphicRendition {
            bold: true,
            underline: true,
            inverse: true,
            ..GraphicRendition::default()
        };
        let selector_rendition =
            runtime_pane_agent_selector_rendition(PaneAgentStatusField::Model, false, &ui_theme);
        let mut spans = vec![TerminalStyleSpan {
            start: 0,
            length: 16,
            rendition: pane_rendition,
        }];

        RuntimeSessionService::clip_line_style_spans_for_overlay(
            &mut spans,
            overlay_column,
            overlay_width,
        );
        spans.push(TerminalStyleSpan {
            start: overlay_column,
            length: overlay_width,
            rendition: selector_rendition,
        });

        let before_overlay = rendered_line_rendition_at(&spans, overlay_column - 1);
        let first_overlay_cell = rendered_line_rendition_at(&spans, overlay_column);
        let final_overlay_cell =
            rendered_line_rendition_at(&spans, overlay_column + overlay_width - 1);
        let after_overlay = rendered_line_rendition_at(&spans, overlay_column + overlay_width);

        assert_eq!(
            before_overlay, pane_rendition,
            "left pane styling was clipped too broadly: {spans:?}"
        );
        assert_eq!(
            first_overlay_cell, selector_rendition,
            "selector row inherited pane styling on its first cell: {spans:?}"
        );
        assert_eq!(
            final_overlay_cell, selector_rendition,
            "selector row inherited pane styling on its trailing cell: {spans:?}"
        );
        assert_eq!(
            after_overlay, pane_rendition,
            "right pane styling was clipped too broadly: {spans:?}"
        );
    }

    /// Verifies pager search highlighting is limited to the matched range.
    ///
    /// Search state stores a concrete body-column range instead of just the
    /// matching line, so rendering should style only the submitted match and
    /// leave surrounding text with its original body/link rendition.
    #[test]
    fn display_overlay_search_highlights_only_matching_columns() {
        let ui_theme = crate::terminal::deepforest_ui_theme();
        let link_rendition = GraphicRendition {
            underline: true,
            foreground: Some(ui_theme.colors.agent_transcript_command.foreground),
            ..GraphicRendition::default()
        };
        let overlay = RuntimeDisplayOverlay {
            lines: vec!["prefix needle suffix".to_string()],
            line_style_spans: vec![vec![TerminalStyleSpan {
                start: 0,
                length: 20,
                rendition: link_rendition,
            }]],
            scroll_offset: 0,
            selections: Vec::new(),
            active_selection_index: None,
            dismiss_on_any_input: false,
            search_input: None,
            search_query: Some("needle".to_string()),
            search_match: Some(RuntimeDisplayOverlaySearchMatch {
                line_index: 0,
                start_column: 7,
                width: 6,
            }),
            search_status: None,
            mouse_selection: None,
            record_browser: None,
        };

        let spans = runtime_display_overlay_rendered_line_style_spans(&overlay, 0, 80, &ui_theme);
        let before_match = rendered_line_rendition_at(&spans, 6);
        let first_match = rendered_line_rendition_at(&spans, 7);
        let final_match = rendered_line_rendition_at(&spans, 12);
        let after_match = rendered_line_rendition_at(&spans, 13);

        assert_eq!(
            before_match.foreground,
            Some(ui_theme.colors.agent_transcript_command.foreground),
            "style before match was overwritten: {spans:?}"
        );
        assert!(
            before_match.underline,
            "style before match lost underline: {spans:?}"
        );
        assert_eq!(
            first_match,
            ui_theme.colors.copy_selection.rendition(),
            "first match cell was not highlighted: {spans:?}"
        );
        assert_eq!(
            final_match,
            ui_theme.colors.copy_selection.rendition(),
            "final match cell was not highlighted: {spans:?}"
        );
        assert_eq!(
            after_match.foreground,
            Some(ui_theme.colors.agent_transcript_command.foreground),
            "style after match was overwritten: {spans:?}"
        );
        assert!(
            after_match.underline,
            "style after match lost underline: {spans:?}"
        );
    }

    /// Verifies pager search highlighting skips matches outside the visible row.
    ///
    /// A match range past the clipped viewport should not emit a fallback row
    /// highlight, otherwise the visible text appears to match a query that is
    /// actually off-screen.
    #[test]
    fn display_overlay_search_skips_offscreen_match_ranges() {
        let ui_theme = crate::terminal::deepforest_ui_theme();
        let overlay = RuntimeDisplayOverlay {
            lines: vec!["visible text then hidden needle".to_string()],
            line_style_spans: vec![Vec::new()],
            scroll_offset: 0,
            selections: Vec::new(),
            active_selection_index: None,
            dismiss_on_any_input: false,
            search_input: None,
            search_query: Some("needle".to_string()),
            search_match: Some(RuntimeDisplayOverlaySearchMatch {
                line_index: 0,
                start_column: 25,
                width: 6,
            }),
            search_status: None,
            mouse_selection: None,
            record_browser: None,
        };

        let spans = runtime_display_overlay_rendered_line_style_spans(&overlay, 0, 12, &ui_theme);

        assert!(
            spans
                .iter()
                .all(|span| span.rendition != ui_theme.colors.copy_selection.rendition()),
            "off-screen match produced a visible highlight: {spans:?}"
        );
    }

    /// Verifies `/list-sessions` only linkifies the first visible occurrence of
    /// a saved conversation id.
    ///
    /// The markdown source keeps a hidden `mez-agent:` resume link on the
    /// session row. If the same UUID-like id appears again in explanatory text,
    /// that later occurrence should remain plain text so keyboard and mouse
    /// navigation expose one selection per logical session.
    #[test]
    fn agent_shell_markdown_overlay_linkifies_each_session_id_once() {
        let ui_theme = crate::terminal::deepforest_ui_theme();
        let content = runtime_agent_shell_markdown_overlay_content(
            Some("list-sessions".to_string()),
            "- [`018f6b3a-1b2c-7000-9000-cafebabefeed`](mez-agent:%2Fresume%20018f6b3a-1b2c-7000-9000-cafebabefeed)",
            &ui_theme,
        );

        assert_eq!(
            content
                .selections
                .iter()
                .filter(|selection| {
                    selection.command == "/resume 018f6b3a-1b2c-7000-9000-cafebabefeed"
                })
                .count(),
            1,
            "{content:?}"
        );
        assert_eq!(content.selections[0].line_index, 0);
    }

    /// Verifies hidden markdown command links are mapped to their rendered
    /// occurrence instead of an earlier duplicate plain-text label.
    ///
    /// Command-overlay markdown hides `mez-agent:` destinations, so selectable
    /// metadata must be derived from the source/rendered row pair. A plain text
    /// occurrence before the actual markdown link should not receive link
    /// styling or become the mouse target for the hidden command.
    #[test]
    fn agent_shell_markdown_overlay_maps_hidden_links_to_exact_rendered_occurrence() {
        let ui_theme = crate::terminal::deepforest_ui_theme();
        let content = runtime_agent_shell_markdown_overlay_content(
            Some("status".to_string()),
            "saved before [`saved`](mez-agent:%2Fresume%20saved)",
            &ui_theme,
        );

        assert_eq!(content.lines, vec!["saved before saved".to_string()]);
        assert_eq!(content.selections.len(), 1, "{content:?}");
        let selection = &content.selections[0];
        assert_eq!(selection.command, "/resume saved");
        assert_eq!(selection.line_index, 0);
        assert_eq!(selection.start_column, "saved before ".len());
        assert_eq!(selection.width, "saved".len());
        assert!(
            content.line_style_spans[0]
                .iter()
                .all(|span| span.start != 0),
            "earlier duplicate text received link styling: {content:?}"
        );
    }

    /// Verifies single-link overlay mouse hit testing remains column bounded.
    ///
    /// Rows with one selectable command still contain inert gutter, whitespace,
    /// and descriptive text. Mouse selection should execute only clicks inside
    /// the advertised choice range, matching multi-chip rows.
    #[test]
    fn display_overlay_single_selection_hit_testing_requires_link_bounds() {
        let overlay = RuntimeDisplayOverlay {
            lines: vec!["text before [open] after".to_string()],
            line_style_spans: vec![Vec::new()],
            scroll_offset: 0,
            selections: vec![RuntimeDisplayOverlaySelection {
                line_index: 0,
                start_column: "text before ".len(),
                width: "[open]".len(),
                command: "/open".to_string(),
                kind: RuntimeDisplayOverlaySelectionKind::Primary,
            }],
            active_selection_index: Some(0),
            dismiss_on_any_input: false,
            search_input: None,
            search_query: None,
            search_match: None,
            search_status: None,
            mouse_selection: None,
            record_browser: None,
        };
        let rendered_start =
            runtime_display_overlay_rendered_selection_start(&overlay, &overlay.selections[0]);

        assert_eq!(
            super::runtime_display_overlay_selection_index_at_position(&overlay, 0, 0),
            None
        );
        assert_eq!(
            super::runtime_display_overlay_selection_index_at_position(
                &overlay,
                0,
                rendered_start.saturating_add(1),
            ),
            Some(0)
        );
    }

    /// Verifies scrolling moves the active command selection to the visible
    /// viewport before Enter can execute it.
    ///
    /// Mouse-wheel and page-scroll paths should not leave keyboard execution
    /// armed on an off-screen action after the overlay viewport changes.
    #[test]
    fn display_overlay_scroll_keeps_active_selection_visible() {
        let mut overlay = RuntimeDisplayOverlay {
            lines: vec![
                "first".to_string(),
                "plain".to_string(),
                "also plain".to_string(),
                "second".to_string(),
                "tail".to_string(),
            ],
            line_style_spans: vec![Vec::new(); 5],
            scroll_offset: 0,
            selections: vec![
                RuntimeDisplayOverlaySelection {
                    line_index: 0,
                    start_column: 0,
                    width: 5,
                    command: "/first".to_string(),
                    kind: RuntimeDisplayOverlaySelectionKind::Primary,
                },
                RuntimeDisplayOverlaySelection {
                    line_index: 3,
                    start_column: 0,
                    width: 6,
                    command: "/second".to_string(),
                    kind: RuntimeDisplayOverlaySelectionKind::Primary,
                },
            ],
            active_selection_index: Some(0),
            dismiss_on_any_input: false,
            search_input: None,
            search_query: None,
            search_match: None,
            search_status: None,
            mouse_selection: None,
            record_browser: None,
        };

        assert!(super::apply_display_overlay_scroll_delta(
            &mut overlay,
            3,
            Size::new(80, 4).unwrap(),
        ));
        assert_eq!(overlay.scroll_offset, 3);
        assert_eq!(overlay.active_selection_index, Some(1));
    }

    /// Verifies compact colon-delimited command display records render as
    /// readable one-line rows for terminal overlays while preserving the
    /// exact field values that users may need to copy into follow-up commands.
    #[test]
    fn human_readable_display_lines_format_colon_delimited_records() {
        let lines = runtime_human_readable_display_lines(
            "theme=kanagawa:source=builtin:active=true:preview=█████:preview_colors=#111111,#222222,#333333,#444444,#555555\nkey=C-a x:source=runtime-config:command=split-window -h",
        );

        assert_eq!(
            lines,
            vec![
                "theme: kanagawa | source: builtin | active: yes | preview: █████",
                "key: C-a x | source: runtime-config | command: split-window -h",
            ]
        );
    }

    /// Verifies compact display rows that include a non-key prefix keep the
    /// prefix as the first compact row segment. This covers
    /// selectors such as window, pane, and group lists whose first columns are
    /// positional identifiers rather than named fields.
    #[test]
    fn human_readable_display_lines_preserve_non_key_prefixes() {
        let lines = runtime_human_readable_display_lines(
            "0:g1:work:active=false:windows=2:action=select-group -t g1",
        );

        assert_eq!(
            lines,
            vec!["actions: [select] | 0 g1 work | active: no | windows: 2"]
        );
    }

    /// Verifies multi-action chooser records render as compact action chips.
    /// This is important for command rows such as `choose-buffer`, where a
    /// single item row may expose both a routine paste action and a destructive
    /// delete action.
    #[test]
    fn human_readable_display_lines_format_multiple_action_chips() {
        let lines = runtime_human_readable_display_lines(
            "buffer=main:bytes=5:origin=test:preview=hello:actions=paste-buffer -b main,delete-buffer main",
        );

        assert_eq!(
            lines,
            vec![
                "actions: [paste] [delete] | buffer: main | bytes: 5 | origin: test | preview: hello"
            ]
        );
    }

    /// Verifies descriptive action metadata is not promoted to an executable
    /// selector. Auth and status records often use `action=` to describe state,
    /// and those labels must remain readable text rather than interactive
    /// command choices.
    #[test]
    fn command_display_overlay_ignores_descriptive_action_metadata() {
        let body = serde_json::json!({
            "outcomes": [{
                "kind": "display",
                "body": "provider=openai method=browser action=interactive-required reason=run-auth source=auth-store"
            }]
        })
        .to_string();
        let content = runtime_command_display_overlay_content(&body, &default_ui_theme()).unwrap();

        assert!(content.selections.is_empty());
        assert_eq!(
            content.lines,
            vec![
                "provider: openai | method: browser | action: interactive-required | reason: run-auth | source: auth-store"
            ]
        );
    }

    /// Verifies non-field help and prose text pass through unchanged. The
    /// humanizer is intentionally narrow so command guides, errors, and shell
    /// output are not reformatted merely because they contain punctuation.
    #[test]
    fn human_readable_display_lines_leave_plain_text_unchanged() {
        let lines = runtime_human_readable_display_lines(
            "mezzanine command help\n  split-window          Split the active pane.",
        );

        assert_eq!(
            lines,
            vec![
                "mezzanine command help",
                "  split-window          Split the active pane.",
            ]
        );
    }

    /// Verifies space-delimited runtime status rows are also displayed as one
    /// readable row when every token is a compact key/value pair.
    #[test]
    fn human_readable_display_lines_format_space_delimited_records() {
        let lines = runtime_human_readable_display_lines(
            "approval_policy=ask source=runtime-policy bypass=false",
        );

        assert_eq!(
            lines,
            vec!["approval policy: ask | source: runtime-policy | bypass: no"]
        );
    }
}
