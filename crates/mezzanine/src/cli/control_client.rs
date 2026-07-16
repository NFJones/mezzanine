//! Cli Control Client implementation.
//!
//! This module owns the cli control client boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    CliOutputFormat, MezError, Read, Result, SocketSelection, UnixStream, Write,
    decode_control_frame, encode_control_body, json_escape, selected_socket_path,
    write_control_response,
};

// Direct control request framing and response handling.

/// Runs the run control request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn run_control_request<W: Write>(
    socket_selection: &SocketSelection,
    method: &str,
    params: &str,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    let socket_path = selected_socket_path(socket_selection);
    let mut stream = UnixStream::connect(socket_path)?;
    let initialize = r#"{"jsonrpc":"2.0","id":"cli-init","method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}}}}"#;
    let request = format!(
        r#"{{"jsonrpc":"2.0","id":"cli","method":"{}","params":{}}}"#,
        json_escape(method),
        params
    );
    let mut request_frames = encode_control_body(initialize);
    request_frames.extend_from_slice(&encode_control_body(&request));
    stream.write_all(&request_frames)?;
    stream.flush()?;
    let response = read_control_response_frames(&mut stream, 1024 * 1024, 2)?;
    let (_, consumed) = decode_control_frame(&response, 1024 * 1024)?;
    let (body, _) = decode_control_frame(&response[consumed..], 1024 * 1024)?;
    write_control_response(stdout, output_format, &body)?;
    Ok(())
}

/// Runs the read control response frames operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn read_control_response_frames(
    stream: &mut UnixStream,
    max_content_length: usize,
    expected_frames: usize,
) -> Result<Vec<u8>> {
    let mut response = Vec::new();
    let mut buffer = vec![0; 8192];
    loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        response.extend_from_slice(&buffer[..read]);
        if response.len() > max_content_length {
            return Err(MezError::invalid_state("control response exceeds limit"));
        }
        if count_complete_control_frames(&response, max_content_length) >= expected_frames {
            return Ok(response);
        }
    }
    Err(incomplete_control_response_error(
        &response,
        max_content_length,
        expected_frames,
    ))
}

/// Runs the count complete control frames operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn count_complete_control_frames(input: &[u8], max_content_length: usize) -> usize {
    let mut count = 0;
    let mut consumed = 0;
    while consumed < input.len() {
        let Ok((_, next)) = decode_control_frame(&input[consumed..], max_content_length) else {
            break;
        };
        if next == 0 {
            break;
        }
        count += 1;
        consumed += next;
    }
    count
}

/// Returns a diagnostic for a closed control socket with missing response frames.
///
/// # Parameters
/// - `input`: The bytes received before the socket closed.
/// - `max_content_length`: The maximum control frame body length.
/// - `expected_frames`: The number of frames the caller was waiting for.
pub(super) fn incomplete_control_response_error(
    input: &[u8],
    max_content_length: usize,
    expected_frames: usize,
) -> MezError {
    let complete_frames = count_complete_control_frames(input, max_content_length);
    MezError::invalid_state(format!(
        "control socket closed before complete response frame ({complete_frames}/{expected_frames})"
    ))
}
