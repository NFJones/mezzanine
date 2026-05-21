//! Tokio codec implementation for protocol frames.
//!
//! The codec performs incremental decoding without consuming partial input and
//! enforces configured maximum body length before waiting for oversized bodies.

use tokio_util::bytes::{BufMut, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

use crate::error::MezError;

use super::types::{ProtocolFrame, ProtocolFrameCodec};
use super::wire::{decode_frame, encode_frame, find_header_end, frame_content_length_from_header};

impl Decoder for ProtocolFrameCodec {
    /// Defines the Item type used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    type Item = ProtocolFrame;
    /// Defines the Error type used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    type Error = MezError;

    /// Runs the decode operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn decode(
        &mut self,
        src: &mut BytesMut,
    ) -> std::result::Result<Option<Self::Item>, Self::Error> {
        let Some(header_end) = find_header_end(src) else {
            return Ok(None);
        };
        let content_length = frame_content_length_from_header(&src[..header_end])?;
        if content_length > self.max_content_length {
            return Err(MezError::invalid_args(
                "Content-Length exceeds configured limit",
            ));
        }
        let body_start = header_end + 4;
        let body_end = body_start
            .checked_add(content_length)
            .ok_or_else(|| MezError::invalid_args("Content-Length overflow"))?;
        if src.len() < body_end {
            return Ok(None);
        }
        let frame_bytes = src.split_to(body_end);
        let (frame, consumed) = decode_frame(&frame_bytes, self.max_content_length)?;
        if consumed != body_end {
            return Err(MezError::invalid_state(
                "protocol frame codec consumed an unexpected frame length",
            ));
        }
        Ok(Some(frame))
    }
}

impl Encoder<ProtocolFrame> for ProtocolFrameCodec {
    /// Defines the Error type used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    type Error = MezError;

    /// Runs the encode operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn encode(
        &mut self,
        item: ProtocolFrame,
        dst: &mut BytesMut,
    ) -> std::result::Result<(), Self::Error> {
        if item.body.len() > self.max_content_length {
            return Err(MezError::invalid_args(
                "protocol frame body exceeds configured limit",
            ));
        }
        dst.put_slice(&encode_frame(&item));
        Ok(())
    }
}
