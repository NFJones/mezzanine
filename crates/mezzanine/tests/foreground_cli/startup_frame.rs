//! Render-aware startup evidence for the foreground pane-status fixture.
//!
//! Serving acknowledgement precedes physical terminal presentation. A raw PTY
//! transcript is not a rendered row: styles and differential updates can split
//! the pane label, and erased/body text cannot establish visible window chrome.
//! Tests use the production mux encoder, not guessed ANSI repaint sequences.

use super::*;
use mez_mux::attached_client::{
    AttachedTerminalOutputFrameState, encode_attached_terminal_output_frame_with_styles,
    encode_attached_terminal_output_update_frame_with_styles,
};
use mez_mux::presentation::AttachedTerminalOutputModes;
use mez_terminal::{GraphicRendition, TerminalColor, TerminalStyleSpan};

/// Recognizes serving acknowledgement and default shell pane presentation.
/// The acknowledgement must precede the first full presentation, and the shell
/// pane label and bottom window actions must exist in the current screen. Replay
/// accumulated original bytes through TerminalScreen so SGR and differential
/// updates do not create false negatives or preserve erased/body labels.
pub(super) fn contains_serving_shell_frame(text: &str) -> bool {
    let Some(marker) = text.find("serving: true") else {
        return false;
    };
    let Some(presentation) = text.find("\x1b[2J\x1b[H") else {
        return false;
    };
    if marker + "serving: true".len() > presentation {
        return false;
    }
    let mut screen =
        mez_terminal::TerminalScreen::new(mez_terminal::TerminalSize::new(80, 24).unwrap(), 100)
            .unwrap();
    screen.feed(text.as_bytes());
    let rows = screen.visible_lines();
    rows.first()
        .is_some_and(|row| is_default_shell_pane_row(row))
        && rows.last().is_some_and(|row| {
            ["□", "⊕", "λ"]
                .into_iter()
                .all(|action| row.contains(action))
        })
}

/// Matches the first pane's exact title token in a rendered chrome row. The
/// screen trims blank cells, so a trailing raw space is not a label boundary.
/// Requiring index zero and an exact shell token rejects generic shell echoes.
fn is_default_shell_pane_row(row: &str) -> bool {
    let mut tokens = row.split_whitespace();
    tokens.next() == Some("0")
        && tokens.next().is_some_and(|title| {
            ["shell", "sh", "bash", "zsh", "fish", "dash", "ksh"].contains(&title)
        })
}

/// Builds the source-verified default layout: top pane label and bottom window
/// actions, separate from body text. Full width permits production span diffs.
fn frame_rows(title: &str) -> Vec<String> {
    let mut rows = vec![" ".repeat(80); 24];
    rows[0] = format!("{:<80}", format!(" 0 {title} "));
    rows[1] = format!("{:<80}", "$ ");
    let window = " 0 main                      □ ⊕ λ";
    let width = mez_terminal::terminal_text_width(window, mez_terminal::terminal_emoji_width());
    rows[23] = format!("{window}{}", " ".repeat(80 - width));
    rows
}

/// Prepends the plain serving status emitted and flushed by cli/serve.rs.
/// Byte chunks are accumulated exactly like ForegroundProcess::read_until.
fn startup_bytes(frame: Vec<u8>) -> Vec<u8> {
    let mut bytes = b"serving: true\r\n".to_vec();
    bytes.extend(frame);
    bytes
}

/// A styled shell label remains one visible label even when the production
/// encoder inserts SGR boundaries inside it. The raw contiguous-label check
/// must fail here while the reconstructed first-row predicate must succeed.
#[test]
fn serving_frame_predicate_accepts_production_styled_shell_label() {
    let rows = frame_rows("sh");
    let spans = vec![vec![TerminalStyleSpan {
        start: 3,
        length: 2,
        rendition: GraphicRendition {
            foreground: Some(TerminalColor::Indexed(120)),
            ..Default::default()
        },
    }]];
    let bytes = startup_bytes(encode_attached_terminal_output_frame_with_styles(
        &rows,
        &spans,
        None,
        AttachedTerminalOutputModes::default(),
    ));
    let text = String::from_utf8(bytes).unwrap();
    assert!(!contains_default_shell_pane_frame(&text));
    assert!(contains_serving_shell_frame(&text));
}

/// A process-title change may repaint only title cells, retaining the unchanged
/// window prefix. Accumulated real encoder bytes must identify the current
/// shell frame without requiring a second full redraw or a longer wait.
#[test]
fn serving_frame_predicate_accepts_production_title_diff() {
    let modes = AttachedTerminalOutputModes::default();
    let previous_rows = frame_rows("cat");
    let rows = frame_rows("sh");
    let previous = AttachedTerminalOutputFrameState::new_with_modes(&previous_rows, &[], modes);
    let mut bytes = startup_bytes(encode_attached_terminal_output_frame_with_styles(
        &previous_rows,
        &[],
        None,
        modes,
    ));
    let diff = encode_attached_terminal_output_update_frame_with_styles(
        &rows,
        &[],
        None,
        modes,
        Some(&previous),
    );
    assert!(!String::from_utf8_lossy(&diff).contains("\x1b[2J"));
    bytes.extend(diff);
    let text = String::from_utf8(bytes).unwrap();
    assert!(!contains_default_shell_pane_frame(&text));
    assert!(contains_serving_shell_frame(&text));
}

/// Startup acknowledgement without current shell chrome, or a shell-looking
/// body echo, cannot establish a frame. A visible shell frame without the
/// acknowledgement is also insufficient: both independent facts are required.
#[test]
fn serving_frame_predicate_rejects_body_echo_and_absent_marker() {
    let modes = AttachedTerminalOutputModes::default();
    let rows = frame_rows("sh");
    let frame = encode_attached_terminal_output_frame_with_styles(&rows, &[], None, modes);
    assert!(!contains_serving_shell_frame(
        &String::from_utf8(frame).unwrap()
    ));
    assert!(!contains_serving_shell_frame("serving: true\r\n"));
    let mut late_marker = frame_rows("sh");
    late_marker[5] = "body echo: serving: true".to_string();
    let late_marker =
        encode_attached_terminal_output_frame_with_styles(&late_marker, &[], None, modes);
    assert!(!contains_serving_shell_frame(
        &String::from_utf8(late_marker).unwrap()
    ));
    let mut echoed = frame_rows("cat");
    echoed[5] = "body echo: 0 sh □ ⊕ λ".to_string();
    let bytes = startup_bytes(encode_attached_terminal_output_frame_with_styles(
        &echoed,
        &[],
        None,
        modes,
    ));
    assert!(!contains_serving_shell_frame(
        &String::from_utf8(bytes).unwrap()
    ));
}

/// Erasing a once-valid shell row must invalidate startup frame evidence, even
/// though its raw transcript still contains the label and serving marker. The
/// replacement full redraw models the clear used for a resized presentation.
#[test]
fn serving_frame_predicate_rejects_erased_label_after_full_redraw() {
    let modes = AttachedTerminalOutputModes::default();
    let mut bytes = startup_bytes(encode_attached_terminal_output_frame_with_styles(
        &frame_rows("sh"),
        &[],
        None,
        modes,
    ));
    assert!(contains_serving_shell_frame(&String::from_utf8_lossy(
        &bytes
    )));
    bytes.extend(encode_attached_terminal_output_frame_with_styles(
        &frame_rows("cat"),
        &[],
        None,
        modes,
    ));
    assert!(!contains_serving_shell_frame(
        &String::from_utf8(bytes).unwrap()
    ));
}

/// Reader boundaries can split SGR, cursor commands, and multibyte chrome. The
/// fixture accumulates original bytes before decoding, so every split must
/// converge to the same startup verdict without a transport chunk assumption.
#[test]
fn serving_frame_predicate_accepts_accumulated_arbitrary_chunks() {
    let bytes = startup_bytes(encode_attached_terminal_output_frame_with_styles(
        &frame_rows("sh"),
        &[],
        None,
        AttachedTerminalOutputModes::default(),
    ));
    for chunk_size in [1, 2, 3, 7, 4096] {
        let mut accumulated = Vec::new();
        for chunk in bytes.chunks(chunk_size) {
            accumulated.extend_from_slice(chunk);
            let _ = contains_serving_shell_frame(&String::from_utf8_lossy(&accumulated));
        }
        assert!(contains_serving_shell_frame(&String::from_utf8_lossy(
            &accumulated
        )));
    }
}
