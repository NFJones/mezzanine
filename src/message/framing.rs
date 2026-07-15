//! MMP frame encoding, decoding, and single-frame request handling.
//!
//! Framing adapts the generic protocol framing module to the MMP content type and
//! delegates decoded bodies to the dispatch layer.

use crate::error::{MezError, Result};
use crate::framing::{ProtocolFrame, decode_frame, encode_frame};

use mez_agent::messaging::{
    MMP_CONTENT_TYPE, MessageConnection, MessageService, dispatch_mmp_body,
};

/// Runs the encode mmp body operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn encode_mmp_body(body: &str) -> Vec<u8> {
    encode_frame(&ProtocolFrame::new(MMP_CONTENT_TYPE, body))
}

/// Runs the decode mmp frame operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn decode_mmp_frame(input: &[u8], max_content_length: usize) -> Result<(String, usize)> {
    let (frame, consumed) = decode_frame(input, max_content_length)?;
    if frame.content_type != MMP_CONTENT_TYPE {
        return Err(MezError::invalid_args("unexpected MMP frame content type"));
    }
    Ok((frame.body, consumed))
}

/// Runs the handle mmp frame operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn handle_mmp_frame(
    input: &[u8],
    max_content_length: usize,
    service: &mut MessageService,
    connection: &mut MessageConnection,
    now_ms: u64,
) -> Result<(Vec<u8>, usize)> {
    let (body, consumed) = decode_mmp_frame(input, max_content_length)?;
    let response = dispatch_mmp_body(&body, service, connection, now_ms);
    Ok((encode_mmp_body(&response), consumed))
}
