//! Caller-owned bounded version-one event stream, not frontend negotiation.
//!
//! Accepts only the established event preface on a retained pinned connection.
//! Framing/compression use the existing codecs; payloads are discarded after
//! classification. No worker is spawned, no clipboard/render extension is
//! negotiated, and this reader cannot authorize input or acknowledge output.
//! Partial reads stay in the owner across cancellation. Invalid framing poisons
//! the owner rather than allowing reuse of ambiguous codec history. The caller
//! must retain the connection lease and dispose it on terminal reader failure.

use tokio::io::{AsyncRead, AsyncReadExt};

use crate::error::{MezError, Result};
use crate::host::terminal::wire_events::{AttachRenderAction, strict_event_action};
use crate::runtime::{IrohCompressionPolicy, IrohStreamDecoder, RuntimeIrohCompressionCodec};

const BODY_LIMIT: usize = 1024 * 1024;
const DECODED_LIMIT: usize = BODY_LIMIT + 1024;

/// One direction-local reader retaining bounded framing and codec history.
/// The generic I/O seam supports deterministic fragmentation/cancellation tests.
pub(crate) struct OutboundEventReader<R> {
    stream: R,
    compression: IrohCompressionPolicy,
    decoder: Option<IrohStreamDecoder>,
    pending: Vec<u8>,
    ended: bool,
    failed: bool,
}

impl OutboundEventReader<iroh::endpoint::RecvStream> {
    /// Accepts one version-one stream and exact preface within a setup deadline.
    /// Cancellation owns no detached work; this method never closes siblings.
    pub(crate) async fn accept(
        connection: &iroh::endpoint::Connection,
        compression: IrohCompressionPolicy,
        budget: std::time::Duration,
    ) -> Result<Self> {
        if !(std::time::Duration::from_millis(100)..=std::time::Duration::from_secs(120))
            .contains(&budget)
        {
            return Err(MezError::invalid_args(
                "outbound event setup budget unavailable",
            ));
        }
        tokio::time::timeout(budget, async {
            let stream = tokio::select! {
                result = connection.accept_uni() => result.map_err(|_| MezError::invalid_state("outbound event stream unavailable"))?,
                _ = connection.closed() => return Err(MezError::invalid_state("outbound connection closed before event setup")),
            };
            Self::from_stream(stream, compression).await
        }).await.map_err(|_| MezError::invalid_state("outbound event setup timed out"))?
    }
}

impl<R: AsyncRead + Unpin> OutboundEventReader<R> {
    /// Checks the exact preface before constructing reusable reader state.
    /// Setup callers own the timeout; failed/cancelled setup disposes the stream.
    async fn from_stream(mut stream: R, compression: IrohCompressionPolicy) -> Result<Self> {
        let expected = crate::runtime::MEZZANINE_IROH_EVENT_STREAM_PREFACE;
        let mut preface = vec![0; expected.len()];
        stream
            .read_exact(&mut preface)
            .await
            .map_err(|_| MezError::invalid_state("outbound event preface incomplete"))?;
        if preface != expected {
            return Err(MezError::invalid_state(
                "outbound event preface unsupported",
            ));
        }
        let compression = compression.with_max_decoded_bytes(DECODED_LIMIT)?;
        let decoder = compression
            .is_streaming()
            .then(|| IrohStreamDecoder::new(compression))
            .transpose()?;
        Ok(Self {
            stream,
            compression,
            decoder,
            pending: Vec::new(),
            ended: false,
            failed: false,
        })
    }

    /// Returns one classified event, or None only after clean EOF. Cancellation
    /// preserves complete/partial pending frames; malformed/truncated data makes
    /// the reader permanently unavailable without leaking peer payloads.
    pub(crate) async fn next(&mut self) -> Result<Option<(AttachRenderAction, Option<u64>)>> {
        if self.failed {
            return Err(MezError::invalid_state("outbound event reader unavailable"));
        }
        let result = self.next_inner().await;
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    /// Reads at most the remaining wire budget, keeping read-ahead in this owner.
    async fn next_inner(&mut self) -> Result<Option<(AttachRenderAction, Option<u64>)>> {
        if self.ended {
            return Ok(None);
        }
        loop {
            if let Some((body, consumed)) = self.decode_pending()? {
                let event = strict_event_action(&body)?;
                self.pending.drain(..consumed);
                return Ok(Some(event));
            }
            let wire_limit = if self.compression.is_streaming() {
                self.compression.stream_record_wire_limit()
            } else if self.compression.codec() == RuntimeIrohCompressionCodec::None {
                DECODED_LIMIT
            } else {
                // Existing v2 header validation independently bounds encoded
                // and decoded lengths before allocation/decompression.
                DECODED_LIMIT * 2 + IrohCompressionPolicy::envelope_header_length()
            };
            let available = wire_limit.saturating_sub(self.pending.len()).min(8192);
            if available == 0 {
                return Err(MezError::invalid_state(
                    "outbound event frame exceeds limit",
                ));
            }
            let mut bytes = [0_u8; 8192];
            let count = self
                .stream
                .read(&mut bytes[..available])
                .await
                .map_err(|_| MezError::invalid_state("outbound event read unavailable"))?;
            if count == 0 {
                if !self.pending.is_empty() {
                    return Err(MezError::invalid_state(
                        "outbound event stream ended with incomplete frame",
                    ));
                }
                self.ended = true;
                return Ok(None);
            }
            self.pending.extend_from_slice(&bytes[..count]);
        }
    }

    /// Decodes exactly one bounded record with the shared transport codecs.
    fn decode_pending(&mut self) -> Result<Option<(String, usize)>> {
        if self.compression.codec() == RuntimeIrohCompressionCodec::None {
            return crate::protocol::framing::decode_frame_incremental(&self.pending, BODY_LIMIT)
                .map_err(|_| MezError::invalid_state("outbound event framing invalid"))?
                .map(|(frame, consumed)| {
                    if frame.content_type != crate::control::CONTROL_CONTENT_TYPE {
                        return Err(MezError::invalid_state(
                            "outbound event content type unsupported",
                        ));
                    }
                    Ok((frame.body, consumed))
                })
                .transpose();
        }
        let (bytes, consumed) = if let Some(decoder) = self.decoder.as_mut() {
            let Some(record) = decoder
                .decode_record(&self.pending)
                .map_err(|_| MezError::invalid_state("outbound event record invalid"))?
            else {
                return Ok(None);
            };
            (record.as_bytes().to_vec(), record.consumed())
        } else {
            if self.pending.len() < IrohCompressionPolicy::envelope_header_length() {
                return Ok(None);
            }
            let length = self
                .compression
                .declared_envelope_length(&self.pending)
                .map_err(|_| MezError::invalid_state("outbound event envelope invalid"))?;
            if self.pending.len() < length {
                return Ok(None);
            }
            (
                self.compression
                    .decode_frame(&self.pending[..length])
                    .map_err(|_| MezError::invalid_state("outbound event envelope invalid"))?,
                length,
            )
        };
        let (body, inner_consumed) = crate::control::decode_control_frame(&bytes, BODY_LIMIT)
            .map_err(|_| MezError::invalid_state("outbound event framing invalid"))?;
        if inner_consumed != bytes.len() {
            return Err(MezError::invalid_state(
                "outbound event record has trailing frames",
            ));
        }
        Ok(Some((body, consumed)))
    }
}

#[cfg(test)]
mod tests;
