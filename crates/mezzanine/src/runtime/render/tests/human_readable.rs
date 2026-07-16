//! Runtime render human readable tests.

use super::*;

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
        vec!["actions: [paste] [delete] | buffer: main | bytes: 5 | origin: test | preview: hello"]
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
