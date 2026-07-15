//! Intrinsic regression tests for mux-owned readline buffer behavior.

use crate::readline::{ReadlineBuffer, ReadlineEdit, ReadlineOutcome};

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

/// Verifies shell-style word deletion stops at punctuation boundaries so
/// Alt+Backspace mirrors readline behavior for paths and flags.
#[test]
fn readline_word_deletion_stops_at_punctuation_boundaries() {
    let mut buffer = ReadlineBuffer::new();
    buffer.insert_text("alpha/beta-gamma");

    assert!(buffer.kill_word_left());
    assert_eq!(buffer.line(), "alpha/beta-");
    assert_eq!(buffer.cursor(), "alpha/beta-".len());
    assert!(buffer.kill_word_left());
    assert_eq!(buffer.line(), "alpha/beta");
    assert_eq!(buffer.cursor(), "alpha/beta".len());
    assert!(buffer.kill_word_left());
    assert_eq!(buffer.line(), "alpha/");
    assert_eq!(buffer.cursor(), "alpha/".len());
    assert!(buffer.kill_word_left());
    assert_eq!(buffer.line(), "alpha");
    assert_eq!(buffer.cursor(), "alpha".len());
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

/// Verifies multiline row navigation keeps the original target column after a
/// shorter row clamps one vertical step.
///
/// Without sticky vertical-column tracking, moving Up from a long row into a
/// short middle row collapses the stored column to that row end. A later Up or
/// Down then stays pinned to the shortened column instead of returning to the
/// original horizontal position.
#[test]
fn readline_multiline_row_navigation_restores_preferred_column_after_short_row() {
    let mut buffer = ReadlineBuffer::new();
    buffer.insert_text("abcdefg\nx\nabcdefg");

    assert!(buffer.move_row_up_or_history_previous());
    assert_eq!(buffer.cursor(), "abcdefg\nx".len());
    assert!(buffer.move_row_up_or_history_previous());
    assert_eq!(buffer.cursor(), "abcdefg".len());

    assert!(buffer.move_row_down_or_history_next());
    assert_eq!(buffer.cursor(), "abcdefg\nx".len());
    assert!(buffer.move_row_down_or_history_next());
    assert_eq!(buffer.cursor(), "abcdefg\nx\nabcdefg".len());
}

/// Verifies visual-row navigation keeps wrap-boundary spaces addressable on the
/// next wrapped row.
///
/// The prompt renderer drops only the single whitespace cell chosen as the wrap
/// seam. Additional spaces after that seam remain visible at the start of the
/// next row, so Up and Down must preserve columns through them instead of
/// skipping directly to the next non-whitespace byte.
#[test]
fn readline_visual_row_navigation_preserves_wrap_boundary_spaces() {
    let mut buffer = ReadlineBuffer::new();
    buffer.insert_text("alpha beta   gamma");

    assert!(buffer.move_buffer_start());
    assert!(buffer.move_visual_row_down_or_history_next(12));
    assert_eq!(buffer.cursor(), "alpha beta  ".len());

    assert!(buffer.move_buffer_start());
    for _ in 0..5 {
        assert!(buffer.move_right());
    }
    assert!(buffer.move_visual_row_down_or_history_next(12));
    assert_eq!(buffer.cursor(), "alpha beta   gamm".len());
    assert!(buffer.move_visual_row_up_or_history_previous(12));
    assert_eq!(buffer.cursor(), "alpha".len());
}

/// Verifies recalled multiline history entries treat a hard wrap boundary as
/// the start of the lower visual row during upward navigation.
///
/// Without this boundary disambiguation, a cursor placed at the first column of
/// a wrapped tail row is attributed to the previous full-width row instead of
/// the lower row. Pressing Up then falls through to the previous logical line
/// or history entry before subsequent vertical moves recover.
#[test]
fn readline_history_visual_row_navigation_uses_wrapped_tail_row_start() {
    let mut buffer = ReadlineBuffer::new();
    buffer.insert_text("header\nabcdefghijklmno");
    assert_eq!(buffer.submit(), "header\nabcdefghijklmno");

    assert!(buffer.history_previous());
    for _ in 0..5 {
        assert!(buffer.move_left());
    }

    assert!(buffer.move_visual_row_up_or_history_previous(10));
    assert_eq!(buffer.cursor(), "header\n".len());
    assert!(buffer.move_visual_row_up_or_history_previous(10));
    assert_eq!(buffer.cursor(), 0);
}

/// Verifies upward visual-row navigation preserves columns that land on the
/// previous row's wrap-breaking whitespace.
///
/// Without keeping the chosen break space inside the previous visual row,
/// moving Up from a later wrapped row can only target the previous row's last
/// non-space character. That clamps the cursor left to the apparent line end
/// before subsequent navigation behaves normally.
#[test]
fn readline_history_visual_row_navigation_preserves_wrap_break_space_column() {
    let mut buffer = ReadlineBuffer::new();
    buffer.insert_text("header\nalpha beta  abcdefghijklmn");
    assert_eq!(buffer.submit(), "header\nalpha beta  abcdefghijklmn");

    assert!(buffer.history_previous());
    assert!(buffer.move_left());
    assert!(buffer.move_left());
    let wrapped_tail_cursor = buffer.cursor();

    assert!(buffer.move_visual_row_up_or_history_previous(12));
    assert_eq!(buffer.cursor(), "header\nalpha beta  ".len());
    assert!(buffer.move_visual_row_down_or_history_next(12));
    assert_eq!(buffer.cursor(), wrapped_tail_cursor);
}

/// Verifies fitting logical rows are not split at their last whitespace during
/// visual-row navigation through recalled multiline history.
///
/// Without checking that a wrap actually overflowed the available columns,
/// whitespace-preferred wrapping turns the last word of a short final logical
/// line into a phantom tail row. Pressing Up from inside that word then stays
/// inside the same logical line at the same in-word offset instead of moving to
/// the same display column on the line above.
#[test]
fn readline_history_visual_row_navigation_skips_phantom_tail_rows() {
    let mut buffer = ReadlineBuffer::new();
    buffer.insert_text("abcdefghijklmno\ndelta word");
    assert_eq!(buffer.submit(), "abcdefghijklmno\ndelta word");

    assert!(buffer.history_previous());
    assert!(buffer.move_left());
    assert_eq!(buffer.cursor(), "abcdefghijklmno\ndelta wor".len());

    assert!(buffer.move_visual_row_up_or_history_previous(20));
    assert_eq!(buffer.cursor(), "abcdefghi".len());
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
    assert!(buffer.move_row_down_or_history_next());
    assert_eq!(buffer.line(), "first line\nsecond line");
    assert!(buffer.cursor() > "first line\n".len());
    assert!(buffer.move_row_down_or_history_next());
    assert_eq!(buffer.line(), "draft");
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

/// Verifies reverse search rejects entries that only satisfy an ordered-character
/// fuzzy match and lack the typed substring.
#[test]
fn readline_history_search_backward_requires_contiguous_substrings() {
    let mut buffer = ReadlineBuffer::new();
    buffer.insert_text("show /loop status");
    assert_eq!(buffer.submit(), "show /loop status");
    buffer.insert_text("show /long/output");
    assert_eq!(buffer.submit(), "show /long/output");
    buffer.insert_text("/loop");

    assert!(buffer.history_search_backward());

    assert_eq!(buffer.line(), "show /loop status");
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
