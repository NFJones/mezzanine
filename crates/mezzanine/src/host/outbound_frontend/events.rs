//! Caller-owned bounded version-one/two event decoding, not frontend negotiation.
//!
//! Accepts only the established event preface on a retained pinned connection.
//! Framing/compression use the existing codecs; payloads are discarded after
//! classification. Version-two clipboard assembly requires an explicit caller
//! gate; no worker, host clipboard write or session negotiation occurs here.
//! This reader cannot authorize input or acknowledge output.
//! Partial reads stay in the owner across cancellation. Invalid framing poisons
//! the owner rather than allowing reuse of ambiguous codec history. The caller
//! must retain the connection lease and dispose it on terminal reader failure.

use tokio::io::{AsyncRead, AsyncReadExt};

use crate::error::{MezError, Result};
use crate::host::terminal::wire_events::{AttachRenderAction, strict_event_action};
use crate::runtime::{IrohCompressionPolicy, IrohStreamDecoder, RuntimeIrohCompressionCodec};

const BODY_LIMIT: usize = 1024 * 1024;
const DECODED_LIMIT: usize = BODY_LIMIT + 1024;

/// One decoded connection-owned item. Clipboard content is deliberately not
/// Debug-formatted; callers must retain negotiated authority and bound delivery.
pub(crate) enum OutboundEventItem {
    /// Content-free redraw facts from an ordinary event notification.
    Redraw(AttachRenderAction, Option<u64>),
    /// Complete UTF-8 clipboard effect, not permission to write a host clipboard.
    Clipboard(String),
}

/// One direction-local reader retaining bounded framing and codec history.
/// The generic I/O seam supports deterministic fragmentation/cancellation tests.
pub(crate) struct OutboundEventReader<R> {
    stream: R,
    compression: IrohCompressionPolicy,
    decoder: Option<IrohStreamDecoder>,
    pending: Vec<u8>,
    ended: bool,
    failed: bool,
    clipboard: Option<crate::host::terminal::iroh_clipboard::IrohClipboardAssembler>,
}

impl OutboundEventReader<iroh::endpoint::RecvStream> {
    /// Accepts an explicitly negotiated version-two clipboard stream. Callers
    /// must validate primary role and the returned clipboard capability first.
    /// Construction grants no authority and starts no clipboard worker.
    pub(crate) async fn accept_clipboard(
        connection: &iroh::endpoint::Connection,
        compression: IrohCompressionPolicy,
        budget: std::time::Duration,
    ) -> Result<Self> {
        Self::accept_version(connection, compression, budget, true).await
    }

    /// Accepts one version-one stream and exact preface within a setup deadline.
    /// Cancellation owns no detached work; this method never closes siblings.
    pub(crate) async fn accept(
        connection: &iroh::endpoint::Connection,
        compression: IrohCompressionPolicy,
        budget: std::time::Duration,
    ) -> Result<Self> {
        Self::accept_version(connection, compression, budget, false).await
    }

    /// Bounds stream acceptance and exact negotiated preface checking together.
    async fn accept_version(
        connection: &iroh::endpoint::Connection,
        compression: IrohCompressionPolicy,
        budget: std::time::Duration,
        clipboard: bool,
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
            Self::from_stream_version(stream, compression, clipboard).await
        }).await.map_err(|_| MezError::invalid_state("outbound event setup timed out"))?
    }
}

impl<R: AsyncRead + Unpin> OutboundEventReader<R> {
    /// Checks the exact preface before constructing reusable reader state.
    /// Setup callers own the timeout; failed/cancelled setup disposes the stream.
    #[cfg(test)]
    async fn from_stream(stream: R, compression: IrohCompressionPolicy) -> Result<Self> {
        Self::from_stream_version(stream, compression, false).await
    }

    /// Checks the exact caller-selected preface. The clipboard gate must come
    /// from independently validated primary capability, never remote frames.
    async fn from_stream_version(
        mut stream: R,
        compression: IrohCompressionPolicy,
        clipboard: bool,
    ) -> Result<Self> {
        let expected = if clipboard {
            crate::runtime::MEZZANINE_IROH_EVENT_STREAM_V2_PREFACE
        } else {
            crate::runtime::MEZZANINE_IROH_EVENT_STREAM_PREFACE
        };
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
            clipboard: clipboard.then(Default::default),
        })
    }

    /// Returns one classified event, or None only after clean EOF. Cancellation
    /// preserves complete/partial pending frames; malformed/truncated data makes
    /// the reader permanently unavailable without leaking peer payloads.
    pub(crate) async fn next(&mut self) -> Result<Option<(AttachRenderAction, Option<u64>)>> {
        if self.clipboard.is_some() {
            return Err(MezError::invalid_state(
                "clipboard reader requires item-aware consumption",
            ));
        }
        match self.next_item().await? {
            Some(OutboundEventItem::Redraw(action, id)) => Ok(Some((action, id))),
            Some(OutboundEventItem::Clipboard(_)) => {
                Err(MezError::invalid_state("unexpected clipboard item"))
            }
            None => Ok(None),
        }
    }

    /// Returns exactly one bounded decoded item. Partial or rejected clipboard
    /// effects produce a neutral redraw fact, preserving bounded burst handling.
    /// Clipboard validation errors discard partial content without killing the
    /// event stream, matching existing attach behavior. Framing errors poison it.
    pub(crate) async fn next_item(&mut self) -> Result<Option<OutboundEventItem>> {
        if self.failed {
            return Err(MezError::invalid_state("outbound event reader unavailable"));
        }
        let result = self.next_inner().await;
        if result.is_err() {
            self.failed = true;
            if let Some(assembler) = &mut self.clipboard {
                assembler.discard_partial();
            }
        }
        result
    }

    /// Reads at most the remaining wire budget, keeping read-ahead in this owner.
    async fn next_inner(&mut self) -> Result<Option<OutboundEventItem>> {
        if self.ended {
            return Ok(None);
        }
        loop {
            if let Some((body, consumed)) = self.decode_pending()? {
                let event = self.classify_item(&body)?;
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
            let expiration = self
                .clipboard
                .as_ref()
                .and_then(|assembler| assembler.expiration_deadline());
            let count = if let Some(deadline) = expiration {
                tokio::select! {
                    read = self.stream.read(&mut bytes[..available]) => read,
                    _ = tokio::time::sleep_until(deadline) => {
                        if let Some(assembler) = &mut self.clipboard {
                            assembler.discard_expired();
                        }
                        continue;
                    }
                }
            } else {
                self.stream.read(&mut bytes[..available]).await
            }
            .map_err(|_| MezError::invalid_state("outbound event read unavailable"))?;
            if count == 0 {
                if !self.pending.is_empty() {
                    return Err(MezError::invalid_state(
                        "outbound event stream ended with incomplete frame",
                    ));
                }
                self.ended = true;
                if let Some(assembler) = &mut self.clipboard {
                    assembler.discard_partial();
                }
                return Ok(None);
            }
            self.pending.extend_from_slice(&bytes[..count]);
        }
    }

    /// Classifies a complete notification without exposing unknown payloads.
    /// Clipboard frames are inert unless the caller selected the v2 gate.
    fn classify_item(&mut self, body: &str) -> Result<OutboundEventItem> {
        let value: serde_json::Value = serde_json::from_str(body)
            .map_err(|_| MezError::invalid_state("outbound event JSON invalid"))?;
        if value
            .get("method")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|method| method.starts_with("client/clipboard."))
        {
            let assembler = self.clipboard.as_mut().ok_or_else(|| {
                MezError::invalid_state("outbound clipboard effect was not negotiated")
            })?;
            if value.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0") {
                assembler.discard_partial();
                return Ok(OutboundEventItem::Redraw(AttachRenderAction::None, None));
            }
            return Ok(match assembler.apply(body) {
                Ok(Some(content)) => OutboundEventItem::Clipboard(content),
                Ok(None) => OutboundEventItem::Redraw(AttachRenderAction::None, None),
                Err(_) => {
                    assembler.discard_partial();
                    OutboundEventItem::Redraw(AttachRenderAction::None, None)
                }
            });
        }
        let (action, id) = strict_event_action(body)?;
        Ok(OutboundEventItem::Redraw(action, id))
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
