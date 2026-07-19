//! Runtime render action presentation tests.

use super::*;

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
            refresh: false,
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
    let ui_theme = mez_mux::theme::deepforest_ui_theme();
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
    let ui_theme = mez_mux::theme::deepforest_ui_theme();
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
    let ui_theme = mez_mux::theme::deepforest_ui_theme();
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
    let ui_theme = mez_mux::theme::deepforest_ui_theme();
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
    let mut definition = mez_mux::theme::builtin_ui_theme_definition("deepforest").unwrap();
    definition
        .colors
        .insert("syntax_type_fg".to_string(), "#010203".to_string());
    let ui_theme = mez_mux::theme::resolve_ui_theme("syntax-test", definition).unwrap();
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
    let mut definition = mez_mux::theme::builtin_ui_theme_definition("deepforest").unwrap();
    definition
        .colors
        .insert("syntax_keyword_fg".to_string(), "#010203".to_string());
    let ui_theme = mez_mux::theme::resolve_ui_theme("syntax-test", definition).unwrap();
    let lines = command_preview_terminal_rendered_lines(
        "if true; then echo \"ok\"; fi",
        80,
        10,
        mez_agent::ShellClassification::Bash,
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
    let wrapped = wrap_rich_text_line_to_width(
        RichTextLine {
            display: "mez> alpha beta gamma".to_string(),
            style_spans: Vec::new(),
            copy_text: None,
            kind: RichTextLineKind::Normal,
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
    let wrapped = wrap_rich_text_line_to_width(
        RichTextLine {
            display: "mez> aaaaaaaaaaaaaaaa".to_string(),
            style_spans: Vec::new(),
            copy_text: None,
            kind: RichTextLineKind::Normal,
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
    let wrapped = wrap_rich_text_line_to_width(
        RichTextLine {
            display: "漢abc".to_string(),
            style_spans: Vec::new(),
            copy_text: None,
            kind: RichTextLineKind::Normal,
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
    let ui_theme = mez_mux::theme::deepforest_ui_theme();
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
    let ui_theme = mez_mux::theme::deepforest_ui_theme();
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
    let mut definition = mez_mux::theme::builtin_ui_theme_definition("deepforest").unwrap();
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
    let ui_theme = mez_mux::theme::resolve_ui_theme("constant-diff-test", definition).unwrap();
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
