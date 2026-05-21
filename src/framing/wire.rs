//! Wire encoding and decoding for content-length protocol frames.
//!
//! The wire layer is deliberately strict: headers must be UTF-8, body bytes
//! must match the advertised content length, and configured maximum lengths are
//! enforced before body allocation by the streaming codec.

use std::str;

use crate::error::{MezError, Result};

use super::types::ProtocolFrame;

/// Encodes a protocol frame with content-length and content-type headers.
pub fn encode_frame(frame: &ProtocolFrame) -> Vec<u8> {
    let body = frame.body.as_bytes();
    let header = format!(
        "Content-Length: {}\r\nContent-Type: {}\r\n\r\n",
        body.len(),
        frame.content_type
    );
    let mut encoded = header.into_bytes();
    encoded.extend_from_slice(body);
    encoded
}

/// Decodes one complete protocol frame from a byte slice.
///
/// Returns the decoded frame and the number of consumed bytes, or an error when
/// headers are malformed, the body is incomplete, UTF-8 decoding fails, or the
/// declared content length exceeds the supplied limit.
pub fn decode_frame(input: &[u8], max_content_length: usize) -> Result<(ProtocolFrame, usize)> {
    let header_end = find_header_end(input)
        .ok_or_else(|| MezError::invalid_args("protocol frame is missing header terminator"))?;
    let header = str::from_utf8(&input[..header_end])
        .map_err(|_| MezError::invalid_args("protocol frame headers must be UTF-8"))?;

    let mut content_length = None;
    let mut content_type = "application/json".to_string();

    for line in header.split("\r\n") {
        let Some((name, value)) = line.split_once(':') else {
            return Err(MezError::invalid_args("malformed protocol frame header"));
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        match name.as_str() {
            "content-length" => {
                let parsed = value
                    .parse::<usize>()
                    .map_err(|_| MezError::invalid_args("invalid Content-Length header"))?;
                content_length = Some(parsed);
            }
            "content-type" => {
                content_type = value.to_string();
            }
            _ => {}
        }
    }

    let content_length =
        content_length.ok_or_else(|| MezError::invalid_args("missing Content-Length header"))?;
    if content_length > max_content_length {
        return Err(MezError::invalid_args(
            "Content-Length exceeds configured limit",
        ));
    }

    let body_start = header_end + 4;
    let body_end = body_start
        .checked_add(content_length)
        .ok_or_else(|| MezError::invalid_args("Content-Length overflow"))?;
    if input.len() < body_end {
        return Err(MezError::invalid_args("incomplete protocol frame body"));
    }

    let body = str::from_utf8(&input[body_start..body_end])
        .map_err(|_| MezError::invalid_args("protocol frame body must be UTF-8"))?
        .to_string();

    Ok((ProtocolFrame { content_type, body }, body_end))
}

/// Runs the find header end operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn find_header_end(input: &[u8]) -> Option<usize> {
    input.windows(4).position(|window| window == b"\r\n\r\n")
}

/// Runs the frame content length from header operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn frame_content_length_from_header(header: &[u8]) -> Result<usize> {
    let header = str::from_utf8(header)
        .map_err(|_| MezError::invalid_args("protocol frame headers must be UTF-8"))?;
    for line in header.split("\r\n") {
        let Some((name, value)) = line.split_once(':') else {
            return Err(MezError::invalid_args("malformed protocol frame header"));
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            return value
                .trim()
                .parse::<usize>()
                .map_err(|_| MezError::invalid_args("invalid Content-Length header"));
        }
    }
    Err(MezError::invalid_args("missing Content-Length header"))
}
