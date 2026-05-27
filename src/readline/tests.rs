//! Unit tests for readline editing, decoding, and bounded prompt loops.

use super::{
    ReadlineBuffer, ReadlineEdit, ReadlineInputDecoder, ReadlineOutcome, ReadlinePrompt,
    ReadlinePromptKind, ReadlinePromptLoopConfig, ReadlinePromptLoopIo, run_readline_prompt_loop,
};
use crate::error::Result;
/// Verifies readline insert and cursor movement edit in place.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn readline_insert_and_cursor_movement_edit_in_place() {
    let mut buffer = ReadlineBuffer::new();

    buffer.insert_text("helo");
    assert_eq!(buffer.line(), "helo");
    assert_eq!(buffer.cursor(), 4);

    assert!(buffer.move_left());
    buffer.insert_char('l');

    assert_eq!(buffer.line(), "hello");
    assert_eq!(buffer.cursor(), 4);
    assert!(buffer.move_end());
    assert_eq!(buffer.cursor(), 5);
}

/// Verifies readline deletion respects utf8 boundaries.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn readline_deletion_respects_utf8_boundaries() {
    let mut buffer = ReadlineBuffer::new();
    buffer.insert_text("aé🙂b");

    assert!(buffer.move_left());
    assert!(buffer.backspace());
    assert_eq!(buffer.line(), "aéb");
    assert_eq!(buffer.cursor(), "aé".len());

    assert!(buffer.move_home());
    assert!(buffer.delete_forward());
    assert_eq!(buffer.line(), "éb");
    assert_eq!(buffer.cursor(), 0);
}

/// Verifies readline kill commands remove text around cursor.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn readline_kill_commands_remove_text_around_cursor() {
    let mut buffer = ReadlineBuffer::new();
    buffer.insert_text("prefix suffix");
    for _ in 0..7 {
        assert!(buffer.move_left());
    }

    assert!(buffer.kill_to_start());
    assert_eq!(buffer.line(), " suffix");
    assert_eq!(buffer.cursor(), 0);

    assert!(buffer.move_end());
    for _ in 0..3 {
        assert!(buffer.move_left());
    }
    assert!(buffer.kill_to_end());
    assert_eq!(buffer.line(), " suf");
}

/// Verifies shell-style word movement and deletion for prompts.
///
/// Word navigation should skip whitespace and then move across one visible
/// token, matching the behavior users expect from common readline bindings in
/// terminal command and agent prompts.
#[test]
fn readline_word_navigation_and_deletion_use_shell_style_segments() {
    let mut buffer = ReadlineBuffer::new();
    buffer.insert_text("alpha beta  gamma");

    assert!(buffer.move_word_left());
    assert_eq!(buffer.cursor(), "alpha beta  ".len());
    assert!(buffer.move_word_left());
    assert_eq!(buffer.cursor(), "alpha ".len());
    assert!(buffer.move_word_right());
    assert_eq!(buffer.cursor(), "alpha beta".len());

    assert!(buffer.kill_word_left());
    assert_eq!(buffer.line(), "alpha   gamma");
    assert_eq!(buffer.cursor(), "alpha ".len());
    assert!(buffer.kill_word_right());
    assert_eq!(buffer.line(), "alpha ");
}

/// Verifies multiline readline row navigation preserves visible columns and
/// only falls back to prompt history when no adjacent row exists.
#[test]
fn readline_multiline_row_navigation_precedes_history_navigation() {
    let mut buffer = ReadlineBuffer::new();
    buffer.insert_text("one");
    assert_eq!(buffer.submit(), "one");
    buffer.insert_text("first line\nsecond\nthird line");
    assert!(buffer.move_buffer_start());
    for _ in 0.."first ".chars().count() {
        assert!(buffer.move_right());
    }

    assert!(buffer.move_row_down_or_history_next());
    assert_eq!(buffer.cursor(), "first line\nsecond".len());
    assert!(buffer.move_row_down_or_history_next());
    assert_eq!(buffer.cursor(), "first line\nsecond\nthird ".len());
    assert!(buffer.move_row_up_or_history_previous());
    assert_eq!(buffer.cursor(), "first line\nsecond".len());

    assert!(buffer.move_row_up_or_history_previous());
    assert_eq!(buffer.cursor(), "first ".len());
    assert!(buffer.move_row_up_or_history_previous());
    assert_eq!(buffer.line(), "one");
}

/// Verifies soft-wrapped prompt row navigation precedes history navigation.
///
/// Long agent prompts can visually occupy multiple rows even when they contain no
/// explicit newline. Up and Down should move through those visible rows before
/// they recall history entries.
#[test]
fn readline_visual_row_navigation_precedes_history_navigation() {
    let mut buffer = ReadlineBuffer::new();
    buffer.insert_text("history");
    assert_eq!(buffer.submit(), "history");
    buffer.insert_text("alpha beta gamma");

    assert!(buffer.move_visual_row_up_or_history_previous(12));
    assert_eq!(buffer.cursor(), "alpha".len());
    assert!(buffer.move_visual_row_down_or_history_next(12));
    assert_eq!(buffer.cursor(), "alpha beta gamma".len());
    assert!(buffer.move_visual_row_up_or_history_previous(12));
    assert!(buffer.move_visual_row_up_or_history_previous(12));
    assert_eq!(buffer.line(), "history");
}

/// Verifies multi-line history entries remain whole entries while traversing
/// history and only become row-navigable after an explicit edit/navigation move.
#[test]
fn readline_history_navigation_skips_multiline_entries_until_editing() {
    let mut buffer = ReadlineBuffer::new();
    buffer.insert_text("older entry");
    assert_eq!(buffer.submit(), "older entry");
    buffer.insert_text("first line\nsecond line");
    assert_eq!(buffer.submit(), "first line\nsecond line");
    buffer.insert_text("draft");

    assert!(buffer.history_previous());
    assert_eq!(buffer.line(), "first line\nsecond line");
    assert!(buffer.move_row_up_or_history_previous());
    assert_eq!(buffer.line(), "older entry");
    assert!(buffer.history_next());
    assert_eq!(buffer.line(), "first line\nsecond line");

    assert!(buffer.move_left());
    assert!(buffer.move_row_up_or_history_previous());
    assert_eq!(buffer.line(), "first line\nsecond line");
    assert!(buffer.cursor() < "first line\n".len());
}

/// Verifies readline submission records bounded history.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn readline_submission_records_bounded_history() {
    let mut buffer = ReadlineBuffer::with_history_limit(2);

    buffer.insert_text("first");
    assert_eq!(buffer.submit(), "first");
    buffer.insert_text("second");
    assert_eq!(buffer.submit(), "second");
    buffer.insert_text("third");
    assert_eq!(buffer.submit(), "third");

    assert_eq!(
        buffer.history(),
        &[String::from("second"), String::from("third")]
    );
}

/// Verifies readline history navigation preserves draft.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn readline_history_navigation_preserves_draft() {
    let mut buffer = ReadlineBuffer::new();
    buffer.insert_text("build");
    assert_eq!(buffer.submit(), "build");
    buffer.insert_text("test");
    assert_eq!(buffer.submit(), "test");
    buffer.insert_text("dr");

    assert!(buffer.history_previous());
    assert_eq!(buffer.line(), "test");
    assert!(buffer.history_previous());
    assert_eq!(buffer.line(), "build");
    assert!(!buffer.history_previous());
    assert_eq!(buffer.line(), "build");

    assert!(buffer.history_next());
    assert_eq!(buffer.line(), "test");
    assert!(buffer.history_next());
    assert_eq!(buffer.line(), "dr");
    assert!(!buffer.history_next());
}

/// Verifies that Ctrl+R style reverse search uses the active draft as a query
/// and cycles backward through older matching prompt submissions.
#[test]
fn readline_history_search_backward_uses_draft_query() {
    let mut buffer = ReadlineBuffer::new();
    buffer.insert_text("git status");
    assert_eq!(buffer.submit(), "git status");
    buffer.insert_text("cargo test");
    assert_eq!(buffer.submit(), "cargo test");
    buffer.insert_text("git diff");
    assert_eq!(buffer.submit(), "git diff");
    buffer.insert_text("git");

    assert!(buffer.history_search_backward());
    assert_eq!(buffer.line(), "git diff");
    assert!(buffer.history_search_backward());
    assert_eq!(buffer.line(), "git status");
    assert!(!buffer.history_search_backward());
    assert_eq!(buffer.line(), "git status");
}

/// Verifies that reverse search finds internal substrings in the nearest older
/// matching history entry instead of requiring strict prefixes.
#[test]
fn readline_history_search_backward_matches_internal_substrings() {
    let mut buffer = ReadlineBuffer::new();
    buffer.insert_text("list-buffers");
    assert_eq!(buffer.submit(), "list-buffers");
    buffer.insert_text("show list-buffers");
    assert_eq!(buffer.submit(), "show list-buffers");
    buffer.insert_text("li");

    assert!(buffer.history_search_backward());

    assert_eq!(buffer.line(), "show list-buffers");
}

/// Verifies reverse search accepts ordered-character fuzzy matches so compact
/// fzf-style queries can find command names without typing contiguous text.
#[test]
fn readline_history_search_backward_matches_ordered_query_characters() {
    let mut buffer = ReadlineBuffer::new();
    buffer.insert_text("list-buffers");
    assert_eq!(buffer.submit(), "list-buffers");
    buffer.insert_text("show sessions");
    assert_eq!(buffer.submit(), "show sessions");
    buffer.insert_text("lb");

    assert!(buffer.history_search_backward());

    assert_eq!(buffer.line(), "list-buffers");
}

/// Verifies readline editing history entry exits history navigation.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn readline_editing_history_entry_exits_history_navigation() {
    let mut buffer = ReadlineBuffer::new();
    buffer.insert_text("status");
    assert_eq!(buffer.submit(), "status");
    buffer.insert_text("draft");

    assert!(buffer.history_previous());
    buffer.insert_text(" --short");
    assert_eq!(buffer.line(), "status --short");
    assert!(!buffer.history_next());
}

/// Verifies readline apply reports submission and noops.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn readline_apply_reports_submission_and_noops() {
    let mut buffer = ReadlineBuffer::new();

    assert_eq!(buffer.apply(ReadlineEdit::MoveLeft), ReadlineOutcome::Noop);
    assert_eq!(
        buffer.apply(ReadlineEdit::InsertText(String::from("agent"))),
        ReadlineOutcome::Edited
    );
    assert_eq!(
        buffer.apply(ReadlineEdit::Submit),
        ReadlineOutcome::Submitted(String::from("agent"))
    );
    assert_eq!(buffer.line(), "");
}

/// Verifies large pasted text renders as one compact editable block while
/// submission still recovers the exact original payload.
#[test]
fn readline_large_paste_blocks_render_compactly_and_submit_raw_text() {
    let large = "a".repeat(1229);
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Command);

    prompt.buffer.insert_text("before ");
    prompt.buffer.insert_text(&large);
    prompt.buffer.insert_text(" after");

    assert_eq!(prompt.render(), ":before [Pasted 1.2 KiB] after");
    assert_eq!(
        prompt.rendered_cursor_column(),
        ":before [Pasted 1.2 KiB] after".chars().count()
    );
    assert_eq!(
        prompt.buffer.expanded_line(),
        format!("before {large} after")
    );
    assert_eq!(
        prompt.buffer.apply(ReadlineEdit::Submit),
        ReadlineOutcome::SubmittedWithDisplay {
            text: format!("before {large} after"),
            display: String::from("before [Pasted 1.2 KiB] after"),
        }
    );
}

/// Verifies delete and backspace treat collapsed pasted blocks as one logical
/// prompt character so users can remove the whole payload predictably.
#[test]
fn readline_large_paste_blocks_delete_as_single_character() {
    let large = "b".repeat(1229);
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Agent);

    prompt.buffer.insert_text("a");
    prompt.buffer.insert_text(&large);
    prompt.buffer.insert_text("c");
    assert_eq!(prompt.render(), "mez> a[Pasted 1.2 KiB]c");

    assert!(prompt.buffer.move_left());
    assert!(prompt.buffer.backspace());
    assert_eq!(prompt.render(), "mez> ac");
    assert_eq!(prompt.buffer.expanded_line(), "ac");

    prompt.buffer.insert_text(&large);
    assert_eq!(prompt.render(), "mez> a[Pasted 1.2 KiB]c");
    assert!(prompt.buffer.move_left());
    assert!(prompt.buffer.delete_forward());
    assert_eq!(prompt.render(), "mez> ac");
    assert_eq!(prompt.buffer.expanded_line(), "ac");
}

/// Verifies readline terminal input maps common editing sequences.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn readline_terminal_input_maps_common_editing_sequences() {
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Command);

    assert_eq!(
        prompt_outcome(&mut prompt, b"helo"),
        ReadlineOutcome::Edited
    );
    assert_eq!(
        prompt_outcome(&mut prompt, b"\x02"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt_outcome(&mut prompt, b"l"), ReadlineOutcome::Edited);
    assert_eq!(prompt.buffer.line(), "hello");
    assert_eq!(prompt.render(), ":hello");
    assert_eq!(prompt.rendered_cursor_column(), 5);

    assert_eq!(
        prompt_outcome(&mut prompt, b"\r"),
        ReadlineOutcome::Submitted(String::from("hello"))
    );
}

/// Verifies prompt cursor columns use terminal display width rather than raw
/// Unicode scalar counts.
///
/// Wide CJK characters and emoji occupy more than one terminal cell, so prompt
/// cursor placement must use display cells to keep editing navigation aligned
/// with the rendered prompt.
#[test]
fn readline_prompt_cursor_columns_use_display_width() {
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Agent);

    assert_eq!(
        prompt_outcome(&mut prompt, "a界".as_bytes()),
        ReadlineOutcome::Edited
    );

    assert_eq!(prompt.rendered_cursor_column(), "mez> ".len() + 3);
    assert_eq!(
        prompt_outcome(&mut prompt, b"\x1b[D"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.rendered_cursor_column(), "mez> ".len() + 1);
}

/// Verifies readline terminal input maps word navigation and word deletion
/// bindings for the primary command prompt surface.
#[test]
fn readline_command_prompt_maps_word_navigation_and_deletion_sequences() {
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Command);

    assert_eq!(
        prompt_outcome(&mut prompt, b"alpha beta gamma"),
        ReadlineOutcome::Edited
    );
    assert_eq!(
        prompt_outcome(&mut prompt, b"\x1bb"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.cursor(), "alpha beta ".len());
    assert_eq!(
        prompt_outcome(&mut prompt, b"\x1b[1;5D"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.cursor(), "alpha ".len());
    assert_eq!(
        prompt_outcome(&mut prompt, b"\x1b[1;5C"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.cursor(), "alpha beta".len());
    assert_eq!(
        prompt_outcome(&mut prompt, b"\x1b[1;5F"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.cursor(), prompt.buffer.line().len());
    assert_eq!(
        prompt_outcome(&mut prompt, b"\x1b[1;5D"),
        ReadlineOutcome::Edited
    );
    assert_eq!(
        prompt_outcome(&mut prompt, b"\x17"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.line(), "alpha gamma");
    assert_eq!(
        prompt_outcome(&mut prompt, b"\x1bd"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.line(), "alpha ");
}

/// Verifies agent prompt navigation behaves like a readline text entry surface
/// when multiline input is present.
#[test]
fn readline_agent_prompt_maps_multiline_row_navigation() {
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Agent);
    prompt.set_prompt_body_columns(12);
    prompt
        .buffer
        .set_history(vec!["previous prompt".to_string()]);

    assert_eq!(
        prompt_outcome(&mut prompt, b"first"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt_outcome(&mut prompt, b"\n"), ReadlineOutcome::Edited);
    assert_eq!(
        prompt_outcome(&mut prompt, b"second"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt_outcome(&mut prompt, b"\n"), ReadlineOutcome::Edited);
    assert_eq!(
        prompt_outcome(&mut prompt, b"third"),
        ReadlineOutcome::Edited
    );

    assert_eq!(
        prompt_outcome(&mut prompt, b"\x1b[H"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.cursor(), "first\nsecond\n".len());
    assert_eq!(
        prompt_outcome(&mut prompt, b"\x1b[A"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.cursor(), "first\n".len());
    assert_eq!(
        prompt_outcome(&mut prompt, b"\x1b[B"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.cursor(), "first\nsecond\n".len());
    assert_eq!(
        prompt_outcome(&mut prompt, b"\x1b[1;5H"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.cursor(), 0);
    assert_eq!(
        prompt_outcome(&mut prompt, b"\x1b[A"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.line(), "previous prompt");
}

/// Verifies pane-local agent prompts use visible-row navigation inside
/// multiline drafts before recalling prompt history when the runtime has
/// supplied body width.
#[test]
fn readline_agent_prompt_uses_visible_row_navigation_before_history() {
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Agent);
    prompt.set_prompt_body_columns(12);
    prompt
        .buffer
        .set_history(vec!["previous prompt".to_string()]);

    assert_eq!(
        prompt_outcome(&mut prompt, b"first line"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt_outcome(&mut prompt, b"\n"), ReadlineOutcome::Edited);
    assert_eq!(
        prompt_outcome(&mut prompt, b"second line wraps"),
        ReadlineOutcome::Edited
    );
    assert_eq!(
        prompt.buffer.cursor(),
        "first line\nsecond line wraps".len()
    );

    assert_eq!(
        prompt_outcome(&mut prompt, b"\x1b[A"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.line(), "first line\nsecond line wraps");

    assert_eq!(
        prompt_outcome(&mut prompt, b"\x1b[A"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.line(), "first line\nsecond line wraps");
    assert_eq!(
        prompt_outcome(&mut prompt, b"\x1b[A"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.line(), "previous prompt");
}

/// Verifies application-cursor arrow sequences are decoded as prompt
/// navigation, not literal text or no-op escape fragments.
///
/// Attached panes can leave application cursor mode enabled while the
/// Mezzanine-owned agent prompt is visible. The prompt must still treat SS3
/// arrow sequences as readline navigation so pane-local text entry is stable
/// regardless of the underlying PTY mode.
#[test]
fn readline_agent_prompt_maps_ss3_arrows_to_visible_row_navigation() {
    let mut decoder = ReadlineInputDecoder::new();
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Agent);
    prompt.set_prompt_body_columns(10);

    assert_eq!(
        decoder
            .apply_to_prompt(&mut prompt, b"alpha beta gamma")
            .unwrap(),
        vec![ReadlineOutcome::Edited]
    );
    assert_eq!(
        decoder.apply_to_prompt(&mut prompt, b"\x1bOA").unwrap(),
        vec![ReadlineOutcome::Edited]
    );
    assert_eq!(prompt.buffer.line(), "alpha beta gamma");
    let row_up_cursor = prompt.buffer.cursor();
    assert!(row_up_cursor < "alpha beta gamma".len());

    assert_eq!(
        decoder.apply_to_prompt(&mut prompt, b"\x1bOB").unwrap(),
        vec![ReadlineOutcome::Edited]
    );
    assert!(prompt.buffer.cursor() > row_up_cursor);
}

/// Verifies that Tab invokes the shared selector for Mezzanine commands and
/// cycles candidates without submitting the prompt.
#[test]
fn readline_command_prompt_tabs_through_mezzanine_command_selector() {
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Command);

    assert_eq!(prompt_outcome(&mut prompt, b"new"), ReadlineOutcome::Edited);
    assert_eq!(prompt_outcome(&mut prompt, b"\t"), ReadlineOutcome::Edited);

    assert_eq!(prompt.buffer.line(), "new-window ");
    assert!(prompt.selector.is_some());
}

/// Verifies that Shift-Tab moves backward through the selector candidate list
/// and that the selector also covers command argument values.
#[test]
fn readline_command_prompt_selects_command_arguments() {
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Command);

    assert_eq!(
        prompt_outcome(&mut prompt, b"mcp-add st"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt_outcome(&mut prompt, b"\t"), ReadlineOutcome::Edited);

    assert_eq!(prompt.buffer.line(), "mcp-add stdio ");
}

/// Verifies that agent prompts use the same selector path for slash commands
/// and slash-command argument values.
#[test]
fn readline_agent_prompt_selects_slash_commands_and_arguments() {
    let mut command_prompt = ReadlinePrompt::new(ReadlinePromptKind::Agent);

    assert_eq!(
        prompt_outcome(&mut command_prompt, b"/log"),
        ReadlineOutcome::Edited
    );
    assert_eq!(
        prompt_outcome(&mut command_prompt, b"\t"),
        ReadlineOutcome::Edited
    );
    assert_eq!(command_prompt.buffer.line(), "/log-level ");

    let mut arg_prompt = ReadlinePrompt::new(ReadlinePromptKind::Agent);
    assert_eq!(
        prompt_outcome(&mut arg_prompt, b"/log-level de"),
        ReadlineOutcome::Edited
    );
    assert_eq!(
        prompt_outcome(&mut arg_prompt, b"\t"),
        ReadlineOutcome::Edited
    );
    assert_eq!(arg_prompt.buffer.line(), "/log-level debug ");
}

/// Verifies that readline renders selector hints as transient shadow text while
/// leaving the editable prompt buffer and cursor position unchanged.
#[test]
fn readline_command_prompt_renders_shadow_hints_without_editing() {
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Command);

    assert_eq!(
        prompt_outcome(&mut prompt, b"mcp-add "),
        ReadlineOutcome::Edited
    );

    assert_eq!(prompt.buffer.line(), "mcp-add ");
    assert_eq!(
        prompt.render_with_shadow_hint(),
        ":mcp-add  <name> --transport <stdio|streamable-http>"
    );
    assert_eq!(
        prompt.rendered_shadow_hint_columns(),
        Some((":mcp-add ".len(), 43))
    );
}

/// Verifies that agent prompts expose slash-command prefix hints before Tab is
/// pressed, so users can discover command completions without changing input.
#[test]
fn readline_agent_prompt_renders_slash_command_shadow_hint() {
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Agent);

    assert_eq!(
        prompt_outcome(&mut prompt, b"/log"),
        ReadlineOutcome::Edited
    );

    assert_eq!(prompt.buffer.line(), "/log");
    assert_eq!(prompt.render_with_shadow_hint(), "mez> /log-level");
    assert_eq!(prompt.rendered_shadow_hint_columns(), Some((11, 6)));
}

/// Verifies agent prompts use an interactive Ctrl+R reverse history search.
#[test]
fn readline_agent_prompt_ctrl_r_opens_reverse_i_search() {
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Agent);
    prompt.buffer.set_history(vec![
        "list sessions".to_string(),
        "show list sessions".to_string(),
    ]);
    prompt.buffer.set_line("li");

    assert_eq!(
        prompt.apply_terminal_input(b"\x12").unwrap(),
        ReadlineOutcome::Edited
    );

    assert_eq!(prompt.buffer.line(), "show list sessions");
    assert_eq!(
        prompt.render(),
        "(reverse-i-search'li'): show list sessions"
    );
    assert!(prompt.reverse_search_active());
}

/// Verifies reverse search accepts a typed substring after it is opened.
#[test]
fn readline_reverse_i_search_updates_query_from_typed_input() {
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Command);
    prompt.buffer.set_history(vec![
        "list files".to_string(),
        "build project".to_string(),
        "project status".to_string(),
    ]);

    assert_eq!(
        prompt.apply_terminal_input(b"\x12").unwrap(),
        ReadlineOutcome::Edited
    );
    assert_eq!(
        prompt.apply_terminal_input(b"proj").unwrap(),
        ReadlineOutcome::Edited
    );

    assert_eq!(prompt.buffer.line(), "project status");
    assert_eq!(prompt.render(), "(reverse-i-search'proj'): project status");
}

/// Verifies repeated Ctrl+R walks backward, Tab walks forward, and Shift-Tab
/// walks backward through matching reverse-search history entries.
#[test]
fn readline_reverse_i_search_cycles_backward_and_forward() {
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Agent);
    prompt.buffer.set_history(vec![
        "list files".to_string(),
        "build project".to_string(),
        "list tests".to_string(),
    ]);
    prompt.buffer.set_line("list");

    assert_eq!(
        prompt.apply_terminal_input(b"\x12").unwrap(),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.line(), "list tests");
    assert_eq!(
        prompt.apply_terminal_input(b"\x12").unwrap(),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.line(), "list files");
    assert_eq!(
        prompt.apply_terminal_input(b"\t").unwrap(),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.line(), "list tests");
    assert_eq!(
        prompt.apply_terminal_input(b"\x1b[Z").unwrap(),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.line(), "list files");
}

/// Verifies reverse search accepts matches without submitting the prompt.
///
/// Users often use Enter or Right arrow to choose a found item and then edit it
/// before submission. Accepting the match must therefore leave the buffer in
/// normal editing mode instead of immediately sending the command or prompt.
#[test]
fn readline_reverse_i_search_accepts_without_submit() {
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Command);
    prompt.buffer.set_history(vec![
        "list files".to_string(),
        "build project".to_string(),
        "project status".to_string(),
    ]);

    assert_eq!(
        prompt.apply_terminal_input(b"\x12").unwrap(),
        ReadlineOutcome::Edited
    );
    assert_eq!(
        prompt.apply_terminal_input(b"proj").unwrap(),
        ReadlineOutcome::Edited
    );
    assert_eq!(
        prompt.apply_terminal_input(b"\r").unwrap(),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.line(), "project status");
    assert!(!prompt.reverse_search_active());

    assert_eq!(
        prompt.apply_terminal_input(b" --json").unwrap(),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.line(), "project status --json");
}

/// Verifies reverse search cancel/navigation restores the original draft.
///
/// Left, Up, Down, Escape, and Ctrl+C should get users out of incremental
/// search without leaking escape bytes into the search query or submitting the
/// currently displayed history match.
#[test]
fn readline_reverse_i_search_navigation_cancels_to_draft() {
    let inputs: [&[u8]; 5] = [b"\x1b[D", b"\x1b[A", b"\x1b[B", b"\x1b", b"\x03"];
    for input in inputs {
        let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Agent);
        prompt.buffer.set_history(vec![
            "list files".to_string(),
            "build project".to_string(),
            "project status".to_string(),
        ]);
        prompt.buffer.set_line("proj");

        assert_eq!(
            prompt.apply_terminal_input(b"\x12").unwrap(),
            ReadlineOutcome::Edited
        );
        assert_eq!(prompt.buffer.line(), "project status");
        assert_eq!(
            prompt.apply_terminal_input(input).unwrap(),
            ReadlineOutcome::Edited
        );
        assert_eq!(prompt.buffer.line(), "proj");
        assert!(!prompt.reverse_search_active());
    }
}

/// Verifies that agent prompts reserve line-feed for an embedded newline while
/// carriage return remains the submission key. Raw terminals distinguish Enter
/// from Ctrl+J this way, so the agent shell can support multi-line prompts
/// without making normal Enter ambiguous.
#[test]
fn readline_agent_prompt_ctrl_j_inserts_newline_and_enter_submits() {
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Agent);

    assert_eq!(
        prompt_outcome(&mut prompt, b"first"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt_outcome(&mut prompt, b"\n"), ReadlineOutcome::Edited);
    assert_eq!(
        prompt_outcome(&mut prompt, b"second"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.line(), "first\nsecond");
    assert_eq!(
        prompt_outcome(&mut prompt, b"\r"),
        ReadlineOutcome::Submitted(String::from("first\nsecond"))
    );
}
/// Verifies standalone Escape clears pane-local agent prompt text without
/// cancelling or submitting the prompt.
///
/// Agent-shell Escape is a draft-clearing key, not an exit path. Ctrl+C and
/// empty Ctrl+D remain the dedicated exit signals.
#[test]
fn readline_agent_prompt_escape_clears_input_without_cancelling() {
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Agent);
    assert_eq!(
        prompt_outcome(&mut prompt, b"draft text"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.line(), "draft text");
    assert_eq!(
        prompt_outcome(&mut prompt, b"\x1b"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.line(), "");
    assert_eq!(prompt_outcome(&mut prompt, b"\x1b"), ReadlineOutcome::Noop);
}

/// Verifies readline terminal input maps history navigation and draft restore.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn readline_terminal_input_maps_history_navigation_and_draft_restore() {
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Agent);
    assert_eq!(
        prompt_outcome(&mut prompt, b"first"),
        ReadlineOutcome::Edited
    );
    assert_eq!(
        prompt_outcome(&mut prompt, b"\r"),
        ReadlineOutcome::Submitted(String::from("first"))
    );
    assert_eq!(
        prompt_outcome(&mut prompt, b"second"),
        ReadlineOutcome::Edited
    );
    assert_eq!(
        prompt_outcome(&mut prompt, b"\r"),
        ReadlineOutcome::Submitted(String::from("second"))
    );
    assert_eq!(
        prompt_outcome(&mut prompt, b"draft"),
        ReadlineOutcome::Edited
    );

    assert_eq!(
        prompt_outcome(&mut prompt, b"\x1b[A"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.line(), "second");
    assert_eq!(
        prompt_outcome(&mut prompt, b"\x1b[A"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.line(), "first");
    assert_eq!(
        prompt_outcome(&mut prompt, b"\x1b[B"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.line(), "second");
    assert_eq!(
        prompt_outcome(&mut prompt, b"\x1b[B"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.line(), "draft");
}

/// Verifies that terminal Ctrl+R maps to reverse history search for the agent
/// prompt, giving loaded agent prompt history the same discoverability as shell
/// history without leaving readline editing mode.
#[test]
fn readline_terminal_input_maps_ctrl_r_history_search() {
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Agent);
    prompt.buffer.set_history(vec![
        "list files".to_string(),
        "build project".to_string(),
        "list tests".to_string(),
    ]);

    assert_eq!(
        prompt_outcome(&mut prompt, b"list"),
        ReadlineOutcome::Edited
    );
    assert_eq!(
        prompt_outcome(&mut prompt, b"\x12"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.line(), "list tests");
    assert_eq!(prompt.render(), "(reverse-i-search'list'): list tests");
    assert_eq!(
        prompt_outcome(&mut prompt, b"\x12"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.line(), "list files");
}

/// Verifies that encoded Ctrl+R key sequences from modern terminals map to the
/// same reverse fuzzy history search as the legacy ASCII control byte.
///
/// Some terminals emit CSI-u or xterm modifyOtherKeys sequences for modified
/// printable keys. The attached agent and command prompts still need those
/// encodings to behave like a normal readline Ctrl+R.
#[test]
fn readline_terminal_input_maps_encoded_ctrl_r_history_search() {
    let mut csi_u_prompt = ReadlinePrompt::new(ReadlinePromptKind::Agent);
    csi_u_prompt.buffer.set_history(vec![
        "list files".to_string(),
        "build project".to_string(),
        "list tests".to_string(),
    ]);
    assert_eq!(
        prompt_outcome(&mut csi_u_prompt, b"list"),
        ReadlineOutcome::Edited
    );
    assert_eq!(
        prompt_outcome(&mut csi_u_prompt, b"\x1b[114;5u"),
        ReadlineOutcome::Edited
    );
    assert_eq!(csi_u_prompt.buffer.line(), "list tests");
    assert_eq!(
        csi_u_prompt.render(),
        "(reverse-i-search'list'): list tests"
    );

    let mut modify_other_keys_prompt = ReadlinePrompt::new(ReadlinePromptKind::Command);
    modify_other_keys_prompt.buffer.set_history(vec![
        "list-buffers".to_string(),
        "show list-buffers".to_string(),
    ]);
    assert_eq!(
        prompt_outcome(&mut modify_other_keys_prompt, b"li"),
        ReadlineOutcome::Edited
    );
    assert_eq!(
        prompt_outcome(&mut modify_other_keys_prompt, b"\x1b[27;5;114~"),
        ReadlineOutcome::Edited
    );
    assert_eq!(modify_other_keys_prompt.buffer.line(), "show list-buffers");
    assert_eq!(
        modify_other_keys_prompt.render(),
        "(reverse-i-search'li'): show list-buffers"
    );
}

/// Verifies readline terminal input supports cancel eof and control noops.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn readline_terminal_input_supports_cancel_eof_and_control_noops() {
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Command);

    assert_eq!(
        prompt_outcome(&mut prompt, b"\x03"),
        ReadlineOutcome::Cancelled
    );
    assert_eq!(prompt_outcome(&mut prompt, b"\x04"), ReadlineOutcome::Eof);
    assert_eq!(
        prompt_outcome(&mut prompt, b"\x1b[15~"),
        ReadlineOutcome::Noop
    );
}

/// Verifies readline decoder splits text control and escape sequences.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn readline_decoder_splits_text_control_and_escape_sequences() {
    let mut decoder = ReadlineInputDecoder::new();
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Command);

    let outcomes = decoder
        .apply_to_prompt(&mut prompt, b"ab\x1b[Dc\n")
        .unwrap();

    assert_eq!(
        outcomes,
        vec![
            ReadlineOutcome::Edited,
            ReadlineOutcome::Edited,
            ReadlineOutcome::Edited,
            ReadlineOutcome::Submitted(String::from("acb")),
        ]
    );
    assert_eq!(prompt.buffer.line(), "");
    assert_eq!(decoder.pending_len(), 0);
}

/// Verifies readline decoder buffers partial escape and utf8 sequences.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn readline_decoder_buffers_partial_escape_and_utf8_sequences() {
    let mut decoder = ReadlineInputDecoder::new();
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Command);

    assert_eq!(
        decoder.apply_to_prompt(&mut prompt, b"a\xc3").unwrap(),
        vec![ReadlineOutcome::Edited]
    );
    assert_eq!(decoder.pending_len(), 1);
    assert_eq!(
        decoder.apply_to_prompt(&mut prompt, b"\xa9b").unwrap(),
        vec![ReadlineOutcome::Edited]
    );
    assert_eq!(prompt.buffer.line(), "aéb");

    assert_eq!(
        decoder.apply_to_prompt(&mut prompt, b"\x1b[").unwrap(),
        Vec::<ReadlineOutcome>::new()
    );
    assert_eq!(decoder.pending_len(), 2);
    assert_eq!(
        decoder.apply_to_prompt(&mut prompt, b"D").unwrap(),
        vec![ReadlineOutcome::Edited]
    );
    assert_eq!(prompt.buffer.cursor(), "aé".len());
    assert_eq!(decoder.pending_len(), 0);
}

/// Verifies split bracketed paste payloads are inserted literally, including
/// embedded newlines, without submitting the prompt until the user presses
/// Enter after the paste.
#[test]
fn readline_decoder_collapses_split_bracketed_paste_payloads() {
    let large = format!("{}{}", "x".repeat(600), "y".repeat(629));
    let mut decoder = ReadlineInputDecoder::new();
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Agent);

    let first = format!("pre \x1b[200~{}", &large[..600]);
    assert_eq!(
        decoder
            .apply_to_prompt(&mut prompt, first.as_bytes())
            .unwrap(),
        vec![ReadlineOutcome::Edited]
    );
    assert_eq!(prompt.render(), "mez> pre ");

    let second = format!("{}\x1b[201~ post\r", &large[600..]);
    let outcomes = decoder
        .apply_to_prompt(&mut prompt, second.as_bytes())
        .unwrap();

    assert_eq!(
        outcomes,
        vec![
            ReadlineOutcome::Edited,
            ReadlineOutcome::Edited,
            ReadlineOutcome::SubmittedWithDisplay {
                text: format!("pre {large} post"),
                display: String::from("pre [Pasted 1.2 KiB] post"),
            },
        ]
    );
    assert_eq!(decoder.pending_len(), 0);
    assert_eq!(prompt.buffer.line(), "");
}

/// Verifies that a prompt loop can distinguish a literal Escape key from a split
/// escape sequence after a no-input readiness poll. Without this flush point, a
/// user who presses Escape inside an attached prompt leaves a pending byte in the
/// decoder and has no visible way to exit the prompt.
#[test]
fn readline_decoder_flushes_pending_escape_as_cancel() {
    let mut decoder = ReadlineInputDecoder::new();
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Agent);

    assert_eq!(
        decoder.apply_to_prompt(&mut prompt, b"\x1b").unwrap(),
        Vec::<ReadlineOutcome>::new()
    );
    assert_eq!(
        decoder.flush_pending_escape_as_cancel(),
        Some(ReadlineOutcome::Cancelled)
    );
    assert_eq!(decoder.pending_len(), 0);
}

/// Verifies readline prompt loop renders and collects submissions.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn readline_prompt_loop_renders_and_collects_submissions() {
    let mut io = FakeReadlinePromptLoopIo {
        input_batches: vec![b"status\r".to_vec(), b"next\r".to_vec()],
        ..Default::default()
    };
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Agent);

    let report =
        run_readline_prompt_loop(&mut io, &mut prompt, ReadlinePromptLoopConfig::default())
            .unwrap();

    assert_eq!(
        report.submissions,
        vec![String::from("status"), String::from("next")]
    );
    assert_eq!(report.outcomes.len(), 4);
    assert!(!report.cancelled);
    assert!(!report.eof);
    assert_eq!(prompt.buffer.line(), "");
    assert_eq!(
        io.rendered_prompts,
        vec![
            String::from("mez> "),
            String::from("mez> "),
            String::from("mez> ")
        ]
    );
    assert_eq!(report.prompts_rendered, 3);
}

/// Verifies readline prompt loop reports cancel and eof.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn readline_prompt_loop_reports_cancel_and_eof() {
    let mut cancel_io = FakeReadlinePromptLoopIo {
        input_batches: vec![b"\x03".to_vec()],
        ..Default::default()
    };
    let mut cancel_prompt = ReadlinePrompt::new(ReadlinePromptKind::Command);
    let cancel_report = run_readline_prompt_loop(
        &mut cancel_io,
        &mut cancel_prompt,
        ReadlinePromptLoopConfig::default(),
    )
    .unwrap();

    assert!(cancel_report.cancelled);
    assert!(!cancel_report.eof);

    let mut eof_io = FakeReadlinePromptLoopIo {
        input_batches: vec![Vec::new()],
        ..Default::default()
    };
    let mut eof_prompt = ReadlinePrompt::new(ReadlinePromptKind::Command);
    let eof_report = run_readline_prompt_loop(
        &mut eof_io,
        &mut eof_prompt,
        ReadlinePromptLoopConfig::default(),
    )
    .unwrap();

    assert!(eof_report.eof);
}

/// Verifies readline prompt loop rejects unbounded settings.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn readline_prompt_loop_rejects_unbounded_settings() {
    let mut io = FakeReadlinePromptLoopIo::default();
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Command);

    let no_iterations = run_readline_prompt_loop(
        &mut io,
        &mut prompt,
        ReadlinePromptLoopConfig {
            max_iterations: 0,
            ..ReadlinePromptLoopConfig::default()
        },
    )
    .unwrap_err();
    assert_eq!(
        no_iterations.kind(),
        crate::error::MezErrorKind::InvalidArgs
    );

    let no_input = run_readline_prompt_loop(
        &mut io,
        &mut prompt,
        ReadlinePromptLoopConfig {
            max_input_bytes: 0,
            ..ReadlinePromptLoopConfig::default()
        },
    )
    .unwrap_err();
    assert_eq!(no_input.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Runs the prompt outcome operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn prompt_outcome(prompt: &mut ReadlinePrompt, input: &[u8]) -> ReadlineOutcome {
    match prompt.apply_terminal_input(input) {
        Ok(outcome) => outcome,
        Err(error) => panic!("unexpected readline error: {error}"),
    }
}

/// Carries Fake Readline Prompt Loop Io state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Default)]
struct FakeReadlinePromptLoopIo {
    /// Stores the input batches value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    input_batches: Vec<Vec<u8>>,
    /// Stores the rendered prompts value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    rendered_prompts: Vec<String>,
}

impl ReadlinePromptLoopIo for FakeReadlinePromptLoopIo {
    /// Runs the input ready operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn input_ready(&mut self) -> Result<bool> {
        Ok(!self.input_batches.is_empty())
    }

    /// Runs the read input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn read_input(&mut self, max_bytes: usize) -> Result<Vec<u8>> {
        if self.input_batches.is_empty() {
            return Ok(Vec::new());
        }
        let mut batch = self.input_batches.remove(0);
        if batch.len() > max_bytes {
            let remaining = batch.split_off(max_bytes);
            self.input_batches.insert(0, remaining);
        }
        Ok(batch)
    }

    /// Runs the write prompt operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn write_prompt(&mut self, prompt: &ReadlinePrompt) -> Result<usize> {
        let rendered = prompt.render();
        let len = rendered.len();
        self.rendered_prompts.push(rendered);
        Ok(len)
    }
}
