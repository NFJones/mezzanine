//! Regression tests for terminal input bracketed paste behavior.

use crate::host::terminal::client_loop::{
    route_client_input_actions_with_host_paste_buffer,
    route_client_input_actions_with_host_paste_state,
};
use crate::host::terminal::{MouseAction, TerminalClientLoopAction, TerminalClientLoopConfig};
use mez_mux::attached_client::encode_attached_terminal_output_frame_with_styles;
use mez_mux::host_input::HOST_BRACKETED_PASTE_MAX_BUFFER_BYTES;
use mez_mux::input::{MuxAction, PasteBufferTarget};
use mez_mux::presentation::AttachedTerminalOutputModes;

/// Verifies host bracketed paste payloads are forwarded without interpreting
/// bytes that look like Mezzanine prefix commands or mouse reports. Clipboard
/// paste data belongs to the pane application, and routing it as mux input can
/// turn a large paste into accidental commands.
#[test]
fn client_loop_forwards_host_bracketed_paste_as_opaque_input() {
    let config = TerminalClientLoopConfig::default();
    let mut paste_active = false;
    let input = b"\x1b[200~alpha\x01=beta\x1b[<0;12;5M\x1b[201~";

    let actions =
        route_client_input_actions_with_host_paste_state(input, &config, &mut paste_active)
            .unwrap();

    assert_eq!(
        actions,
        vec![TerminalClientLoopAction::ForwardToPane(input.to_vec())]
    );
    assert!(!paste_active);
}

/// Verifies host bracketed paste state survives across terminal read chunks.
/// Large clipboard pastes are read in bounded chunks; the chunks between the
/// start and end delimiters must remain opaque, while input after the closing
/// delimiter resumes normal mux-prefix parsing.
#[test]
fn client_loop_keeps_host_bracketed_paste_opaque_across_chunks() {
    let config = TerminalClientLoopConfig::default();
    let mut paste_active = false;
    let first = b"\x1b[200~alpha\x01";
    let second = b"=beta\x1b[201~\x01=";

    let first_actions =
        route_client_input_actions_with_host_paste_state(first, &config, &mut paste_active)
            .unwrap();
    assert_eq!(
        first_actions,
        vec![TerminalClientLoopAction::ForwardToPane(first.to_vec())]
    );
    assert!(paste_active);

    let second_actions =
        route_client_input_actions_with_host_paste_state(second, &config, &mut paste_active)
            .unwrap();

    assert_eq!(
        second_actions,
        vec![
            TerminalClientLoopAction::ForwardToPane(b"=beta\x1b[201~".to_vec()),
            TerminalClientLoopAction::ExecuteMux(MuxAction::PasteBuffer(
                PasteBufferTarget::ChooseInteractively,
            )),
        ]
    );
    assert!(!paste_active);
}

/// Verifies a large host bracketed paste stays opaque over many bounded
/// terminal-read chunks. This protects full-screen editor pastes where
/// transcript-sized clipboard contents may contain text that resembles
/// Mezzanine prefix commands or SGR mouse packets.
#[test]
fn client_loop_keeps_large_host_bracketed_paste_opaque_across_many_chunks() {
    let config = TerminalClientLoopConfig::default();
    let mut paste_active = false;
    let mut input = b"\x1b[200~".to_vec();
    input.extend(
        "prompt \u{e0b0} agent trace line\n"
            .repeat(18_000)
            .as_bytes(),
    );
    input.extend_from_slice(b"\x01=not-a-mux-command\n\x1b[<0;12;5Mnot-mouse\n");
    input.extend_from_slice(b"\x1b[201~\x01=");

    let mut forwarded = Vec::new();
    let mut mux_actions = Vec::new();
    for chunk in input.chunks(4096) {
        for action in
            route_client_input_actions_with_host_paste_state(chunk, &config, &mut paste_active)
                .unwrap()
        {
            match action {
                TerminalClientLoopAction::ForwardToPane(bytes) => forwarded.extend(bytes),
                other => mux_actions.push(other),
            }
        }
    }

    assert_eq!(forwarded, input[..input.len().saturating_sub(2)]);
    assert_eq!(
        mux_actions,
        vec![TerminalClientLoopAction::ExecuteMux(
            MuxAction::PasteBuffer(PasteBufferTarget::ChooseInteractively,)
        )]
    );
    assert!(!paste_active);
}

/// Verifies buffered host bracketed paste routing waits for the closing
/// delimiter before forwarding large paste content. This prevents typed input
/// after a slow terminal paste from overtaking an incomplete shell heredoc body.
#[test]
fn client_loop_buffers_incomplete_host_bracketed_paste_until_close() {
    let config = TerminalClientLoopConfig::default();
    let mut paste_active = false;
    let mut paste_buffer = Vec::new();
    let first = b"\x1b[200~cat <<'EOF'\nbody";
    let second = b"\nEOF\n\x1b[201~\x01=";

    let first_actions = route_client_input_actions_with_host_paste_buffer(
        first,
        &config,
        &mut paste_active,
        &mut paste_buffer,
    )
    .unwrap();
    assert!(first_actions.is_empty());
    assert!(paste_active);
    assert_eq!(paste_buffer, first);

    let second_actions = route_client_input_actions_with_host_paste_buffer(
        second,
        &config,
        &mut paste_active,
        &mut paste_buffer,
    )
    .unwrap();
    let mut expected_paste = first.to_vec();
    expected_paste.extend_from_slice(b"\nEOF\n\x1b[201~");
    assert_eq!(
        second_actions,
        vec![
            TerminalClientLoopAction::ForwardToPane(expected_paste),
            TerminalClientLoopAction::ExecuteMux(MuxAction::PasteBuffer(
                PasteBufferTarget::ChooseInteractively,
            )),
        ]
    );
    assert!(!paste_active);
    assert!(paste_buffer.is_empty());
}

/// Verifies malformed host bracketed paste frames cannot consume all later
/// terminal input forever.
///
/// A host terminal can deliver the bracketed-paste start marker without the
/// matching end marker if a paste or terminal helper is interrupted. The
/// buffered production path must bound retained bytes and recover by forwarding
/// the accumulated payload once that bound is exceeded.
#[test]
fn client_loop_recovers_from_oversized_unterminated_host_bracketed_paste() {
    let config = TerminalClientLoopConfig::default();
    let mut paste_active = false;
    let mut paste_buffer = Vec::new();
    let mut input = Vec::from(b"\x1b[200~".as_slice());
    input.extend(vec![b'a'; HOST_BRACKETED_PASTE_MAX_BUFFER_BYTES]);

    let actions = route_client_input_actions_with_host_paste_buffer(
        &input,
        &config,
        &mut paste_active,
        &mut paste_buffer,
    )
    .unwrap();

    assert_eq!(
        actions,
        vec![TerminalClientLoopAction::ForwardToPane(input)]
    );
    assert!(!paste_active);
    assert!(paste_buffer.is_empty());
}

/// Verifies stale malformed host bracketed paste frames release later input.
///
/// A host terminal can emit the paste start delimiter and then never deliver
/// the matching close delimiter. Once the buffered frame is old enough to be
/// considered stale, ordinary input must be routed again instead of being
/// swallowed into the retained paste buffer indefinitely.
#[test]
fn client_loop_recovers_from_stale_unterminated_host_bracketed_paste() {
    let config = TerminalClientLoopConfig {
        host_bracketed_paste_started_at: Some(
            std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(1))
                .unwrap(),
        ),
        ..TerminalClientLoopConfig::default()
    };
    let mut paste_active = true;
    let mut paste_buffer = b"\x1b[200~unterminated".to_vec();

    let actions = route_client_input_actions_with_host_paste_buffer(
        b"echo recovered\n",
        &config,
        &mut paste_active,
        &mut paste_buffer,
    )
    .unwrap();

    assert_eq!(
        actions,
        vec![
            TerminalClientLoopAction::ForwardToPane(b"\x1b[200~unterminated".to_vec()),
            TerminalClientLoopAction::ForwardToPane(b"echo recovered\n".to_vec()),
        ]
    );
    assert!(!paste_active);
    assert!(paste_buffer.is_empty());
}

/// Verifies malformed SGR mouse prefixes do not strand later pane input in the
/// buffered paste router.
/// The production attached-terminal path shares the same batched mouse split
/// logic while carrying paste state, so it must recover the same way and leave
/// the paste buffer untouched when a malformed mouse prefix precedes pane text.
#[test]
fn client_loop_buffered_paste_router_skips_malformed_sgr_mouse_prefix_before_later_pane_input() {
    let config = TerminalClientLoopConfig::default();
    let mut paste_active = false;
    let mut paste_buffer = Vec::new();

    assert_eq!(
        route_client_input_actions_with_host_paste_buffer(
            b"\x1b[<0;12;5q",
            &config,
            &mut paste_active,
            &mut paste_buffer,
        )
        .unwrap(),
        vec![
            TerminalClientLoopAction::HandleMouse(MouseAction::Ignore),
            TerminalClientLoopAction::ForwardToPane(b"q".to_vec()),
        ]
    );
    assert!(!paste_active);
    assert!(paste_buffer.is_empty());
}

/// Verifies attached terminal frames mirror pane bracketed-paste mode into the
/// host terminal. Clipboard paste delimiters are only available when the host
/// terminal has been explicitly placed in bracketed-paste mode.
#[test]
fn attached_output_frame_sets_host_bracketed_paste_mode() {
    let lines = vec!["pane".to_string()];
    let frame = encode_attached_terminal_output_frame_with_styles(
        &lines,
        &[],
        None,
        AttachedTerminalOutputModes {
            bracketed_paste: true,
            ..AttachedTerminalOutputModes::default()
        },
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(rendered.starts_with(
        "\x1b[?25l\x1b[0m\x1b[?6l\x1b[?69l\x1b[r\x1b[?7h\x1b[?1000;1002;1006h\x1b[?2004h\x1b[?1004l\x1b[?1049l\x1b[2J\x1b[H"
    ));
    assert!(
        String::from_utf8(
            mez_mux::attached_client::attached_terminal_restore_presentation_frame().to_vec()
        )
        .unwrap()
        .starts_with("\x1b[?2004l"),
        "restore must always leave host bracketed paste disabled"
    );
}
