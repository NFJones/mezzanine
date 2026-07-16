//! Control Framing implementation.
//!
//! This module owns the control framing boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

#[cfg(test)]
use super::dispatch::{ControlConnectionState, dispatch_control_request_for_connection};
use super::{CONTROL_CONTENT_TYPE, MezError, ProtocolFrame, Result, decode_frame, encode_frame};
#[cfg(test)]
use super::{ControlIdempotencyCache, Session};

// Control content-length framing helpers.

/// Runs the encode control body operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn encode_control_body(body: &str) -> Vec<u8> {
    encode_frame(&ProtocolFrame::new(CONTROL_CONTENT_TYPE, body))
}

/// Runs the decode control frame operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn decode_control_frame(input: &[u8], max_content_length: usize) -> Result<(String, usize)> {
    let (frame, consumed) = decode_frame(input, max_content_length)?;
    if frame.content_type != CONTROL_CONTENT_TYPE {
        return Err(MezError::invalid_args(
            "unexpected control frame content type",
        ));
    }
    Ok((frame.body, consumed))
}

/// Runs the handle control frame operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub fn handle_control_frame(
    input: &[u8],
    max_content_length: usize,
    session: &mut Session,
    connection: &mut ControlConnectionState,
    idempotency: &mut ControlIdempotencyCache,
) -> Result<(Vec<u8>, usize)> {
    let (body, consumed) = decode_control_frame(input, max_content_length)?;
    let response = dispatch_control_request_for_connection(&body, session, connection, idempotency);
    Ok((encode_control_body(&response), consumed))
}

/// Runs the handle control frames operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub fn handle_control_frames(
    input: &[u8],
    max_content_length: usize,
    session: &mut Session,
    connection: &mut ControlConnectionState,
    idempotency: &mut ControlIdempotencyCache,
) -> Result<(Vec<u8>, usize)> {
    let mut offset = 0usize;
    let mut output = Vec::new();
    while offset < input.len() {
        let (body, consumed) = decode_control_frame(&input[offset..], max_content_length)?;
        let response =
            dispatch_control_request_for_connection(&body, session, connection, idempotency);
        output.extend_from_slice(&encode_control_body(&response));
        offset += consumed;
    }
    Ok((output, offset))
}
