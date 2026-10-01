//! Tokio codec implementation for protocol frames.
//!
//! The codec performs incremental decoding without consuming partial input and
//! enforces independent finite header and configured body limits before waiting
//! for oversized peer input. Each header scan examines at most 8 KiB.

use tokio_util::bytes::{BufMut, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

use crate::error::MezError;

use super::types::{ProtocolFrame, ProtocolFrameCodec};
use super::wire::{decode_frame_incremental, encode_frame};

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
        let Some((frame, consumed)) = decode_frame_incremental(src, self.max_content_length)?
        else {
            return Ok(None);
        };
        let _ = src.split_to(consumed);
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
