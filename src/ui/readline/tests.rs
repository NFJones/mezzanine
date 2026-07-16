//! Product prompt, selector, and decoder integration tests.

use super::{
    ReadlineEdit, ReadlineInputDecoder, ReadlineOutcome, ReadlinePrompt, ReadlinePromptKind,
};

/// Verifies large pasted text renders as one compact editable block while
/// submission still recovers the exact original payload through the product
/// command-prompt prefix adapter.
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

/// Verifies visible-row navigation crosses explicit newlines by adjacent visual
/// rows instead of jumping to a character column on the previous logical line.
///
/// When the prior logical line wraps, Up from a later logical line should land
/// on that wrapped tail so the cursor does not jump horizontally before later
/// Up and Down navigation stabilizes.
#[test]
fn readline_agent_prompt_preserves_visible_column_across_wrapped_logical_lines() {
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Agent);
    prompt.set_prompt_body_columns(12);

    assert_eq!(
        prompt_outcome(&mut prompt, b"alpha beta gamma"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt_outcome(&mut prompt, b"\n"), ReadlineOutcome::Edited);
    assert_eq!(
        prompt_outcome(&mut prompt, b"delta"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.cursor(), "alpha beta gamma\ndelta".len());

    assert_eq!(
        prompt_outcome(&mut prompt, b"\x1b[A"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.cursor(), "alpha beta gamma".len());

    assert_eq!(
        prompt_outcome(&mut prompt, b"\x1b[B"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.cursor(), "alpha beta gamma\ndelta".len());
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
        prompt_outcome(&mut prompt, b"set-theme to"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt_outcome(&mut prompt, b"\t"), ReadlineOutcome::Edited);

    assert_eq!(prompt.buffer.line(), "set-theme tokyo_night ");
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
        prompt_outcome(&mut prompt, b"save-layout "),
        ReadlineOutcome::Edited
    );

    assert_eq!(prompt.buffer.line(), "save-layout ");
    assert_eq!(
        prompt.render_with_shadow_hint(),
        ":save-layout  [--name name]"
    );
    assert_eq!(
        prompt.rendered_shadow_hint_columns(),
        Some((":save-layout ".len(), 14))
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
    assert_eq!(prompt.rendered_shadow_hint_columns(), Some((9, 6)));
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
/// same reverse history search as the legacy ASCII control byte.
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

/// Applies one terminal-input batch through the product prompt adapter.
fn prompt_outcome(prompt: &mut ReadlinePrompt, input: &[u8]) -> ReadlineOutcome {
    match prompt.apply_terminal_input(input) {
        Ok(outcome) => outcome,
        Err(error) => panic!("unexpected readline error: {error}"),
    }
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
