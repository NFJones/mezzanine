//! Bounded application-layer compression for versioned Iroh frames and streams.
//!
//! Iroh and QUIC do not transparently compress application payloads. This
//! module therefore wraps one complete existing Mezzanine control or event
//! frame in one independently decodable version 2 envelope or one compact
//! version 3 record backed by direction-local compression history. The
//! unchanged version 1 ALPN remains raw. All compressed variants validate
//! declared sizes before allocation or decompression, and version 3 reset
//! records keep sensitive traffic outside reusable compression history.

#![allow(
    dead_code,
    reason = "the framing foundation is consumed by the dependent Iroh runtime integration task"
)]

use futures_util::StreamExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncWriteExt, DuplexStream};
use tokio_util::codec::FramedRead;
use zstd::stream::raw::{
    CParameter, DParameter, Decoder as ZstdStreamDecoder, Encoder as ZstdStreamEncoder, InBuffer,
    Operation, OutBuffer,
};

use crate::error::{MezError, Result};
use crate::protocol::framing::{ProtocolFrameCodec, decode_frame, encode_frame};

use super::RuntimeIrohCompressionCodec;

/// ALPN identity for version 2 Iroh frames using Zstandard compression.
pub(crate) const MEZZANINE_IROH_ZSTD_ALPN: &[u8] = b"mezzanine/transport/2/zstd";
/// ALPN identity for version 2 Iroh frames using LZ4 block compression.
pub(crate) const MEZZANINE_IROH_LZ4_ALPN: &[u8] = b"mezzanine/transport/2/lz4";
/// ALPN identity for version 3 Iroh streams using stateful Zstandard compression.
pub(crate) const MEZZANINE_IROH_ZSTD_STREAM_ALPN: &[u8] = b"mezzanine/transport/3/zstd-stream";
/// ALPN identity for version 3 Iroh streams using stateful linked-history LZ4.
pub(crate) const MEZZANINE_IROH_LZ4_STREAM_ALPN: &[u8] = b"mezzanine/transport/3/lz4-stream";

const ENVELOPE_MAGIC: &[u8; 4] = b"MZC2";
const ENVELOPE_HEADER_LENGTH: usize = 16;
const FLAG_COMPRESSED: u8 = 0b0000_0001;
const KNOWN_FLAGS: u8 = FLAG_COMPRESSED;
const STREAM_FLAG_COMPRESSED: u8 = 0b0000_0001;
const STREAM_FLAG_RESET: u8 = 0b0000_0010;
const STREAM_KNOWN_FLAGS: u8 = STREAM_FLAG_COMPRESSED | STREAM_FLAG_RESET;
const STREAM_HISTORY_BYTES: usize = 64 * 1024;
const STREAM_ZSTD_WINDOW_LOG: u32 = 16;
const STREAM_CODEC_SCRATCH_BYTES: usize = 16 * 1024;

impl RuntimeIrohCompressionCodec {
    /// Returns the deterministic ALPN associated with this configured codec.
    pub(crate) const fn alpn(self) -> &'static [u8] {
        match self {
            Self::ZstdStream => MEZZANINE_IROH_ZSTD_STREAM_ALPN,
            Self::Lz4Stream => MEZZANINE_IROH_LZ4_STREAM_ALPN,
            Self::Zstd => MEZZANINE_IROH_ZSTD_ALPN,
            Self::Lz4 => MEZZANINE_IROH_LZ4_ALPN,
            Self::None => super::MEZZANINE_IROH_ALPN,
        }
    }

    /// Maps a negotiated ALPN to its closed codec value.
    pub(crate) fn from_alpn(alpn: &[u8]) -> Result<Self> {
        match alpn {
            MEZZANINE_IROH_ZSTD_STREAM_ALPN => Ok(Self::ZstdStream),
            MEZZANINE_IROH_LZ4_STREAM_ALPN => Ok(Self::Lz4Stream),
            MEZZANINE_IROH_ZSTD_ALPN => Ok(Self::Zstd),
            MEZZANINE_IROH_LZ4_ALPN => Ok(Self::Lz4),
            super::MEZZANINE_IROH_ALPN => Ok(Self::None),
            _ => Err(MezError::invalid_state(
                "Iroh connection negotiated an unsupported ALPN",
            )),
        }
    }
}

/// Per-frame policy controlling whether compression is permitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IrohFrameCompressionMode {
    /// Apply the negotiated codec when the threshold and size benefit permit.
    Eligible,
    /// Emit an identity envelope even on a compressed version 2 ALPN.
    IdentityOnly,
}

/// Immutable bounded framing policy for one negotiated Iroh connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IrohCompressionPolicy {
    codec: RuntimeIrohCompressionCodec,
    min_bytes: usize,
    zstd_level: i32,
    max_decoded_bytes: usize,
}

/// Privacy-safe aggregate application-frame counters for one Iroh connection.
#[derive(Debug, Clone)]
pub(crate) struct IrohCompressionMetrics {
    codec: RuntimeIrohCompressionCodec,
    inner: Arc<IrohCompressionMetricsInner>,
}

#[derive(Debug, Default)]
struct IrohCompressionMetricsInner {
    wire_bytes: AtomicU64,
    decoded_bytes: AtomicU64,
    compressed_frames: AtomicU64,
    identity_frames: AtomicU64,
    render_snapshot_frames: AtomicU64,
    render_delta_frames: AtomicU64,
    render_changed_rows: AtomicU64,
    render_selected_wire_bytes: AtomicU64,
    render_selected_decoded_bytes: AtomicU64,
    render_snapshot_candidate_bytes: AtomicU64,
    render_triggers_coalesced: AtomicU64,
    render_updates_suppressed: AtomicU64,
    render_snapshot_fallbacks: AtomicU64,
    render_ready_depth_max: AtomicU64,
    render_write_wait_micros: AtomicU64,
    render_write_wait_max_micros: AtomicU64,
}

/// Copyable compression counter snapshot containing no payload or topology data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IrohCompressionMetricsSnapshot {
    pub(crate) codec: RuntimeIrohCompressionCodec,
    pub(crate) wire_bytes: u64,
    pub(crate) decoded_bytes: u64,
    pub(crate) compressed_frames: u64,
    pub(crate) identity_frames: u64,
    pub(crate) render_snapshot_frames: u64,
    pub(crate) render_delta_frames: u64,
    pub(crate) render_changed_rows: u64,
    pub(crate) render_selected_wire_bytes: u64,
    pub(crate) render_selected_decoded_bytes: u64,
    pub(crate) render_snapshot_candidate_bytes: u64,
    pub(crate) render_triggers_coalesced: u64,
    pub(crate) render_updates_suppressed: u64,
    pub(crate) render_snapshot_fallbacks: u64,
    pub(crate) render_ready_depth_max: u64,
    pub(crate) render_write_wait_micros: u64,
    pub(crate) render_write_wait_max_micros: u64,
}

impl IrohCompressionMetrics {
    /// Creates counters scoped to one immutable negotiated codec.
    pub(crate) fn new(codec: RuntimeIrohCompressionCodec) -> Self {
        Self {
            codec,
            inner: Arc::new(IrohCompressionMetricsInner::default()),
        }
    }

    /// Returns the current privacy-safe cumulative counters.
    pub(crate) fn snapshot(&self) -> IrohCompressionMetricsSnapshot {
        IrohCompressionMetricsSnapshot {
            codec: self.codec,
            wire_bytes: self.inner.wire_bytes.load(Ordering::Relaxed),
            decoded_bytes: self.inner.decoded_bytes.load(Ordering::Relaxed),
            compressed_frames: self.inner.compressed_frames.load(Ordering::Relaxed),
            identity_frames: self.inner.identity_frames.load(Ordering::Relaxed),
            render_snapshot_frames: self.inner.render_snapshot_frames.load(Ordering::Relaxed),
            render_delta_frames: self.inner.render_delta_frames.load(Ordering::Relaxed),
            render_changed_rows: self.inner.render_changed_rows.load(Ordering::Relaxed),
            render_selected_wire_bytes: self
                .inner
                .render_selected_wire_bytes
                .load(Ordering::Relaxed),
            render_selected_decoded_bytes: self
                .inner
                .render_selected_decoded_bytes
                .load(Ordering::Relaxed),
            render_snapshot_candidate_bytes: self
                .inner
                .render_snapshot_candidate_bytes
                .load(Ordering::Relaxed),
            render_triggers_coalesced: self.inner.render_triggers_coalesced.load(Ordering::Relaxed),
            render_updates_suppressed: self.inner.render_updates_suppressed.load(Ordering::Relaxed),
            render_snapshot_fallbacks: self.inner.render_snapshot_fallbacks.load(Ordering::Relaxed),
            render_ready_depth_max: self.inner.render_ready_depth_max.load(Ordering::Relaxed),
            render_write_wait_micros: self.inner.render_write_wait_micros.load(Ordering::Relaxed),
            render_write_wait_max_micros: self
                .inner
                .render_write_wait_max_micros
                .load(Ordering::Relaxed),
        }
    }

    /// Records one encoded or decoded application frame without retaining data.
    pub(crate) fn record_frame(&self, wire_bytes: usize, decoded_bytes: usize, compressed: bool) {
        self.inner.wire_bytes.fetch_add(
            u64::try_from(wire_bytes).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        self.inner.decoded_bytes.fetch_add(
            u64::try_from(decoded_bytes).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        if compressed {
            self.inner.compressed_frames.fetch_add(1, Ordering::Relaxed);
        } else {
            self.inner.identity_frames.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Records one successfully flushed render update without retaining rows.
    pub(crate) fn record_render_update(
        &self,
        delta: bool,
        changed_rows: usize,
        wire_bytes: usize,
        decoded_bytes: usize,
        snapshot_candidate_bytes: usize,
    ) {
        if delta {
            self.inner
                .render_delta_frames
                .fetch_add(1, Ordering::Relaxed);
        } else {
            self.inner
                .render_snapshot_frames
                .fetch_add(1, Ordering::Relaxed);
        }
        self.inner.render_changed_rows.fetch_add(
            u64::try_from(changed_rows).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        self.inner.render_selected_wire_bytes.fetch_add(
            u64::try_from(wire_bytes).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        self.inner.render_selected_decoded_bytes.fetch_add(
            u64::try_from(decoded_bytes).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        self.inner.render_snapshot_candidate_bytes.fetch_add(
            u64::try_from(snapshot_candidate_bytes).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    /// Records bounded latest-state render coalescing without terminal content.
    pub(crate) fn record_render_coalescing(
        &self,
        ready_depth: usize,
        suppressed: bool,
        snapshot_fallback: bool,
    ) {
        let ready_depth = u64::try_from(ready_depth).unwrap_or(u64::MAX);
        self.inner
            .render_triggers_coalesced
            .fetch_add(ready_depth.saturating_sub(1), Ordering::Relaxed);
        self.inner
            .render_ready_depth_max
            .fetch_max(ready_depth, Ordering::Relaxed);
        if suppressed {
            self.inner
                .render_updates_suppressed
                .fetch_add(1, Ordering::Relaxed);
        }
        if snapshot_fallback {
            self.inner
                .render_snapshot_fallbacks
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Records time spent awaiting one complete render update write and flush.
    pub(crate) fn record_render_write_wait(&self, elapsed: std::time::Duration) {
        let micros = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
        self.inner
            .render_write_wait_micros
            .fetch_add(micros, Ordering::Relaxed);
        self.inner
            .render_write_wait_max_micros
            .fetch_max(micros, Ordering::Relaxed);
    }
}

/// Encoded frame plus non-sensitive accounting metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IrohEncodedFrame {
    bytes: Vec<u8>,
    compressed: bool,
    decoded_bytes: usize,
}

impl IrohEncodedFrame {
    /// Returns the bytes to write to the negotiated Iroh stream.
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Reports whether this individual version 2 envelope used compression.
    pub(crate) const fn compressed(&self) -> bool {
        self.compressed
    }

    /// Returns the complete existing frame size before envelope compression.
    pub(crate) const fn decoded_bytes(&self) -> usize {
        self.decoded_bytes
    }
}

impl IrohCompressionPolicy {
    /// Builds a connection-local policy after configuration validation.
    ///
    /// The decoded limit must be non-zero, the threshold cannot exceed it, and
    /// the Zstandard level remains constrained to the documented safe range.
    pub(crate) fn new(
        codec: RuntimeIrohCompressionCodec,
        min_bytes: usize,
        zstd_level: i32,
        max_decoded_bytes: usize,
    ) -> Result<Self> {
        if max_decoded_bytes == 0 {
            return Err(MezError::invalid_args(
                "Iroh compression decoded frame limit must be greater than zero",
            ));
        }
        if min_bytes > max_decoded_bytes {
            return Err(MezError::invalid_args(
                "Iroh compression threshold exceeds the decoded frame limit",
            ));
        }
        if !(-5..=22).contains(&zstd_level) {
            return Err(MezError::invalid_args(
                "Iroh compression Zstandard level must be from -5 to 22",
            ));
        }
        Ok(Self {
            codec,
            min_bytes,
            zstd_level,
            max_decoded_bytes,
        })
    }

    /// Returns configured codecs with stateful variants before frame codecs.
    ///
    /// This preserves configured order within each class while ensuring a
    /// mutually supported version 3 codec is attempted before version 2.
    pub(crate) fn negotiation_codecs(
        codecs: &[RuntimeIrohCompressionCodec],
    ) -> impl Iterator<Item = RuntimeIrohCompressionCodec> + '_ {
        codecs
            .iter()
            .copied()
            .filter(|codec| {
                matches!(
                    codec,
                    RuntimeIrohCompressionCodec::ZstdStream
                        | RuntimeIrohCompressionCodec::Lz4Stream
                )
            })
            .chain(codecs.iter().copied().filter(|codec| {
                !matches!(
                    codec,
                    RuntimeIrohCompressionCodec::ZstdStream
                        | RuntimeIrohCompressionCodec::Lz4Stream
                )
            }))
    }

    /// Returns ALPNs in deterministic streaming-first negotiation order.
    pub(crate) fn ordered_alpns(codecs: &[RuntimeIrohCompressionCodec]) -> Vec<Vec<u8>> {
        Self::negotiation_codecs(codecs)
            .map(|codec| codec.alpn().to_vec())
            .collect()
    }

    /// Returns the fixed number of bytes required to inspect one v2 header.
    pub(crate) const fn envelope_header_length() -> usize {
        ENVELOPE_HEADER_LENGTH
    }

    /// Returns the immutable codec selected for this connection.
    pub(crate) const fn codec(self) -> RuntimeIrohCompressionCodec {
        self.codec
    }

    /// Reports whether this policy selected a stateful version 3 codec.
    pub(crate) const fn is_streaming(self) -> bool {
        matches!(
            self.codec,
            RuntimeIrohCompressionCodec::ZstdStream | RuntimeIrohCompressionCodec::Lz4Stream
        )
    }

    /// Returns the maximum buffered wire bytes for one version 3 record.
    pub(crate) fn stream_record_wire_limit(self) -> usize {
        self.stream_record_payload_limit().saturating_add(11)
    }

    fn stream_record_payload_limit(self) -> usize {
        match self.codec {
            RuntimeIrohCompressionCodec::ZstdStream => {
                zstd::zstd_safe::compress_bound(self.max_decoded_bytes).saturating_add(64)
            }
            RuntimeIrohCompressionCodec::Lz4Stream => {
                lz4_flex::block::get_maximum_output_size(self.max_decoded_bytes)
            }
            _ => self.max_decoded_bytes,
        }
    }

    /// Encodes one complete existing frame according to the negotiated codec.
    ///
    /// Version 1 `none` framing remains byte-for-byte unchanged. Version 2
    /// codecs use an identity envelope below the threshold, when explicitly
    /// required for sensitive initialization traffic, or when compression
    /// would not reduce the payload size.
    pub(crate) fn encode_frame(
        self,
        frame: &[u8],
        mode: IrohFrameCompressionMode,
    ) -> Result<IrohEncodedFrame> {
        self.validate_decoded_length(frame.len())?;
        if self.is_streaming() {
            return Err(MezError::invalid_state(
                "stateful Iroh codecs require a direction-local stream encoder",
            ));
        }
        if self.codec == RuntimeIrohCompressionCodec::None {
            return Ok(IrohEncodedFrame {
                bytes: frame.to_vec(),
                compressed: false,
                decoded_bytes: frame.len(),
            });
        }

        let compressed =
            if mode == IrohFrameCompressionMode::Eligible && frame.len() >= self.min_bytes {
                let candidate = match self.codec {
                    RuntimeIrohCompressionCodec::ZstdStream
                    | RuntimeIrohCompressionCodec::Lz4Stream => unreachable!(
                        "stateful Iroh framing returns before version 2 envelope encoding"
                    ),
                    RuntimeIrohCompressionCodec::Zstd => {
                        zstd::stream::encode_all(frame, self.zstd_level).map_err(|_| {
                            MezError::invalid_state("failed to encode bounded Iroh Zstandard frame")
                        })?
                    }
                    RuntimeIrohCompressionCodec::Lz4 => lz4_flex::block::compress(frame),
                    RuntimeIrohCompressionCodec::None => unreachable!(
                        "uncompressed Iroh framing returns before version 2 envelope encoding"
                    ),
                };
                (candidate.len() < frame.len()).then_some(candidate)
            } else {
                None
            };
        let (payload, flags) = match compressed {
            Some(payload) => (payload, FLAG_COMPRESSED),
            None => (frame.to_vec(), 0),
        };
        let encoded_length = u32::try_from(payload.len()).map_err(|_| {
            MezError::invalid_args("Iroh compression encoded frame length exceeds wire range")
        })?;
        let decoded_length = u32::try_from(frame.len()).map_err(|_| {
            MezError::invalid_args("Iroh compression decoded frame length exceeds wire range")
        })?;
        let total_length = ENVELOPE_HEADER_LENGTH
            .checked_add(payload.len())
            .ok_or_else(|| MezError::invalid_args("Iroh compression envelope length overflow"))?;
        let mut bytes = Vec::with_capacity(total_length);
        bytes.extend_from_slice(ENVELOPE_MAGIC);
        bytes.push(flags);
        bytes.extend_from_slice(&[0; 3]);
        bytes.extend_from_slice(&encoded_length.to_be_bytes());
        bytes.extend_from_slice(&decoded_length.to_be_bytes());
        bytes.extend_from_slice(&payload);
        Ok(IrohEncodedFrame {
            bytes,
            compressed: flags == FLAG_COMPRESSED,
            decoded_bytes: frame.len(),
        })
    }

    /// Returns the exact complete envelope length declared by a v2 header.
    ///
    /// Callers can use this before reading a payload. Declared encoded and
    /// decoded lengths are bounded before allocation or decompression.
    pub(crate) fn declared_envelope_length(self, header: &[u8]) -> Result<usize> {
        if self.codec == RuntimeIrohCompressionCodec::None {
            return Err(MezError::invalid_state(
                "version 1 Iroh framing does not use compression envelopes",
            ));
        }
        let parsed = self.parse_header(header)?;
        ENVELOPE_HEADER_LENGTH
            .checked_add(parsed.encoded_length)
            .ok_or_else(|| MezError::invalid_args("Iroh compression envelope length overflow"))
    }

    /// Decodes one exact negotiated frame and enforces all declared limits.
    pub(crate) fn decode_frame(self, encoded: &[u8]) -> Result<Vec<u8>> {
        if self.codec == RuntimeIrohCompressionCodec::None {
            self.validate_decoded_length(encoded.len())?;
            return Ok(encoded.to_vec());
        }
        let header = self.parse_header(encoded)?;
        let expected_length = ENVELOPE_HEADER_LENGTH
            .checked_add(header.encoded_length)
            .ok_or_else(|| MezError::invalid_args("Iroh compression envelope length overflow"))?;
        if encoded.len() != expected_length {
            return Err(MezError::invalid_args(if encoded.len() < expected_length {
                "truncated Iroh compression envelope"
            } else {
                "Iroh compression envelope contains trailing bytes"
            }));
        }
        let payload = &encoded[ENVELOPE_HEADER_LENGTH..];
        let decoded = if header.compressed {
            match self.codec {
                RuntimeIrohCompressionCodec::ZstdStream
                | RuntimeIrohCompressionCodec::Lz4Stream => {
                    unreachable!("stateful Iroh framing uses a direction-local stream decoder")
                }
                RuntimeIrohCompressionCodec::Zstd => {
                    zstd::bulk::decompress(payload, header.decoded_length).map_err(|_| {
                        MezError::invalid_args("invalid bounded Iroh Zstandard frame")
                    })?
                }
                RuntimeIrohCompressionCodec::Lz4 => {
                    lz4_flex::block::decompress(payload, header.decoded_length)
                        .map_err(|_| MezError::invalid_args("invalid bounded Iroh LZ4 frame"))?
                }
                RuntimeIrohCompressionCodec::None => unreachable!(
                    "uncompressed Iroh framing returns before version 2 envelope decoding"
                ),
            }
        } else {
            if header.encoded_length != header.decoded_length {
                return Err(MezError::invalid_args(
                    "identity Iroh envelope lengths do not match",
                ));
            }
            payload.to_vec()
        };
        if decoded.len() != header.decoded_length {
            return Err(MezError::invalid_args(
                "decoded Iroh frame length does not match its declaration",
            ));
        }
        Ok(decoded)
    }

    fn validate_decoded_length(self, length: usize) -> Result<()> {
        if length == 0 {
            return Err(MezError::invalid_args(
                "Iroh compression frame must not be empty",
            ));
        }
        if length > self.max_decoded_bytes {
            return Err(MezError::invalid_args(
                "Iroh compression decoded frame exceeds its configured limit",
            ));
        }
        if length > u32::MAX as usize {
            return Err(MezError::invalid_args(
                "Iroh compression decoded frame length exceeds wire range",
            ));
        }
        Ok(())
    }

    fn parse_header(self, encoded: &[u8]) -> Result<ParsedEnvelopeHeader> {
        if encoded.len() < ENVELOPE_HEADER_LENGTH {
            return Err(MezError::invalid_args(
                "truncated Iroh compression envelope header",
            ));
        }
        if &encoded[..4] != ENVELOPE_MAGIC {
            return Err(MezError::invalid_args(
                "invalid Iroh compression envelope magic",
            ));
        }
        let flags = encoded[4];
        if flags & !KNOWN_FLAGS != 0 {
            return Err(MezError::invalid_args(
                "unsupported Iroh compression envelope flags",
            ));
        }
        if encoded[5..8] != [0; 3] {
            return Err(MezError::invalid_args(
                "Iroh compression envelope reserved bytes are non-zero",
            ));
        }
        let encoded_length =
            usize::try_from(u32::from_be_bytes(encoded[8..12].try_into().map_err(
                |_| MezError::invalid_args("invalid Iroh encoded length field"),
            )?))
            .map_err(|_| MezError::invalid_args("Iroh encoded length exceeds platform range"))?;
        let decoded_length =
            usize::try_from(u32::from_be_bytes(encoded[12..16].try_into().map_err(
                |_| MezError::invalid_args("invalid Iroh decoded length field"),
            )?))
            .map_err(|_| MezError::invalid_args("Iroh decoded length exceeds platform range"))?;
        if encoded_length == 0 || decoded_length == 0 {
            return Err(MezError::invalid_args(
                "Iroh compression envelope lengths must be non-zero",
            ));
        }
        if encoded_length > self.max_decoded_bytes {
            return Err(MezError::invalid_args(
                "Iroh compression encoded frame exceeds its configured limit",
            ));
        }
        self.validate_decoded_length(decoded_length)?;
        Ok(ParsedEnvelopeHeader {
            compressed: flags == FLAG_COMPRESSED,
            encoded_length,
            decoded_length,
        })
    }
}

/// Iroh-only bridge between raw Mezzanine frames and negotiated wire frames.
///
/// Existing control and attach code continues to read and write the unchanged
/// content-length frame stream through `stream_mut`. The bridge owns the Iroh
/// stream pair and translates each complete frame independently. Its first
/// outbound frame is always identity-only so initialization credentials and
/// initialization responses cannot be compressed.
pub(crate) struct IrohCompressionBridge {
    stream: DuplexStream,
    task: tokio::task::JoinHandle<Result<()>>,
}

impl IrohCompressionBridge {
    /// Starts one connection-local frame bridge over an accepted Iroh stream.
    pub(crate) fn spawn(
        recv: iroh::endpoint::RecvStream,
        send: iroh::endpoint::SendStream,
        policy: IrohCompressionPolicy,
        max_content_length: usize,
    ) -> Result<Self> {
        Self::spawn_with_metrics(
            recv,
            send,
            policy,
            IrohCompressionMetrics::new(policy.codec()),
            max_content_length,
        )
    }

    /// Starts a bridge that records counters in a shared connection-local handle.
    pub(crate) fn spawn_with_metrics(
        recv: iroh::endpoint::RecvStream,
        send: iroh::endpoint::SendStream,
        policy: IrohCompressionPolicy,
        metrics: IrohCompressionMetrics,
        max_content_length: usize,
    ) -> Result<Self> {
        let codec = ProtocolFrameCodec::new(max_content_length)?;
        let (stream, bridge_stream) = tokio::io::duplex(64 * 1024);
        let (bridge_read, bridge_write) = tokio::io::split(bridge_stream);
        let task = tokio::spawn(async move {
            let activation = policy
                .is_streaming()
                .then(|| Arc::new(tokio::sync::Barrier::new(2)));
            let outbound = pump_raw_frames_to_iroh_with_activation(
                bridge_read,
                send,
                policy,
                metrics.clone(),
                codec,
                activation.clone(),
            );
            let inbound = pump_iroh_frames_to_raw_with_activation(
                recv,
                bridge_write,
                policy,
                metrics,
                max_content_length,
                activation,
            );
            tokio::try_join!(outbound, inbound)?;
            Ok(())
        });
        Ok(Self { stream, task })
    }

    /// Returns the unchanged raw frame stream consumed by existing adapters.
    pub(crate) fn stream_mut(&mut self) -> &mut DuplexStream {
        &mut self.stream
    }

    /// Stops the local stream and waits boundedly for the bridge task.
    pub(crate) async fn shutdown(mut self, timeout: std::time::Duration) -> Result<()> {
        let shutdown = tokio::time::timeout(timeout, async {
            self.stream.shutdown().await?;
            (&mut self.task).await.map_err(|error| {
                MezError::invalid_state(format!("Iroh compression bridge task failed: {error}"))
            })?
        })
        .await;
        match shutdown {
            Ok(result) => result,
            Err(_) => {
                self.task.abort();
                let _ = (&mut self.task).await;
                Err(MezError::invalid_state(
                    "Iroh compression bridge shutdown timed out",
                ))
            }
        }
    }
}

impl Drop for IrohCompressionBridge {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn pump_raw_frames_to_iroh<R>(
    raw: R,
    send: iroh::endpoint::SendStream,
    policy: IrohCompressionPolicy,
    metrics: IrohCompressionMetrics,
    codec: ProtocolFrameCodec,
) -> Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
{
    pump_raw_frames_to_iroh_with_activation(raw, send, policy, metrics, codec, None).await
}

async fn pump_raw_frames_to_iroh_with_activation<R>(
    raw: R,
    mut send: iroh::endpoint::SendStream,
    policy: IrohCompressionPolicy,
    metrics: IrohCompressionMetrics,
    codec: ProtocolFrameCodec,
    activation: Option<Arc<tokio::sync::Barrier>>,
) -> Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut framed = FramedRead::new(raw, codec);
    if policy.is_streaming() {
        if let Some(frame) = framed.next().await {
            let frame = encode_frame(&frame?);
            policy.validate_decoded_length(frame.len())?;
            metrics.record_frame(frame.len(), frame.len(), false);
            send.write_all(&frame).await.map_err(|_| {
                MezError::invalid_state("failed to write raw Iroh initialization frame")
            })?;
            send.flush().await.map_err(|_| {
                MezError::invalid_state("failed to flush raw Iroh initialization frame")
            })?;
            if let Some(activation) = &activation {
                activation.wait().await;
            }
        }
        let mut encoder = IrohStreamEncoder::new(policy)?;
        while let Some(frame) = framed.next().await {
            let frame = encode_frame(&frame?);
            let mode = control_stream_frame_mode(&frame, policy.max_decoded_bytes);
            let encoded = encoder.encode_frame(&frame, mode)?;
            metrics.record_frame(
                encoded.as_bytes().len(),
                encoded.decoded_bytes(),
                encoded.compressed(),
            );
            send.write_all(encoded.as_bytes()).await.map_err(|_| {
                MezError::invalid_state("failed to write stateful Iroh control frame")
            })?;
            send.flush().await.map_err(|_| {
                MezError::invalid_state("failed to flush stateful Iroh control frame")
            })?;
        }
        return finish_iroh_control_send(send).await;
    }
    let mut first_frame = true;
    while let Some(frame) = framed.next().await {
        let frame = encode_frame(&frame?);
        let mode = if first_frame {
            IrohFrameCompressionMode::IdentityOnly
        } else {
            IrohFrameCompressionMode::Eligible
        };
        let encoded = policy.encode_frame(&frame, mode)?;
        metrics.record_frame(
            encoded.as_bytes().len(),
            encoded.decoded_bytes(),
            encoded.compressed(),
        );
        send.write_all(encoded.as_bytes()).await.map_err(|_| {
            MezError::invalid_state("failed to write negotiated Iroh control frame")
        })?;
        send.flush().await.map_err(|_| {
            MezError::invalid_state("failed to flush negotiated Iroh control frame")
        })?;
        first_frame = false;
    }
    finish_iroh_control_send(send).await
}

async fn finish_iroh_control_send(mut send: iroh::endpoint::SendStream) -> Result<()> {
    send.finish()
        .map_err(|_| MezError::invalid_state("failed to finish negotiated Iroh control stream"))?;
    match send.stopped().await {
        Ok(None) => Ok(()),
        Ok(Some(_)) => Err(MezError::invalid_state(
            "peer reset negotiated Iroh control stream before acknowledgement",
        )),
        Err(_) => Err(MezError::invalid_state(
            "negotiated Iroh control stream acknowledgement failed",
        )),
    }
}

fn control_stream_frame_mode(frame: &[u8], max_content_length: usize) -> IrohFrameCompressionMode {
    let Ok((decoded, consumed)) = decode_frame(frame, max_content_length) else {
        return IrohFrameCompressionMode::IdentityOnly;
    };
    if consumed != frame.len() {
        return IrohFrameCompressionMode::IdentityOnly;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&decoded.body) else {
        return IrohFrameCompressionMode::IdentityOnly;
    };
    if value
        .get("method")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|method| method.contains("clipboard"))
        || json_contains_sensitive_transport_key(&value)
    {
        IrohFrameCompressionMode::IdentityOnly
    } else {
        IrohFrameCompressionMode::Eligible
    }
}

fn json_contains_sensitive_transport_key(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(object) => object.iter().any(|(key, value)| {
            matches!(
                key.as_str(),
                "credential" | "device_credential" | "invitation" | "password" | "secret" | "token"
            ) || json_contains_sensitive_transport_key(value)
        }),
        serde_json::Value::Array(values) => {
            values.iter().any(json_contains_sensitive_transport_key)
        }
        _ => false,
    }
}

async fn pump_iroh_frames_to_raw<W>(
    recv: iroh::endpoint::RecvStream,
    raw: W,
    policy: IrohCompressionPolicy,
    metrics: IrohCompressionMetrics,
    max_content_length: usize,
) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    pump_iroh_frames_to_raw_with_activation(recv, raw, policy, metrics, max_content_length, None)
        .await
}

async fn pump_iroh_frames_to_raw_with_activation<W>(
    recv: iroh::endpoint::RecvStream,
    mut raw: W,
    policy: IrohCompressionPolicy,
    metrics: IrohCompressionMetrics,
    max_content_length: usize,
    activation: Option<Arc<tokio::sync::Barrier>>,
) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    if policy.is_streaming() {
        return pump_streaming_iroh_frames_to_raw(
            recv,
            raw,
            policy,
            metrics,
            max_content_length,
            activation,
        )
        .await;
    }
    if policy.codec() == RuntimeIrohCompressionCodec::None {
        let codec = ProtocolFrameCodec::new(max_content_length)?;
        let mut framed = FramedRead::new(recv, codec);
        while let Some(frame) = framed.next().await {
            let frame = encode_frame(&frame?);
            metrics.record_frame(frame.len(), frame.len(), false);
            raw.write_all(&frame).await?;
            raw.flush().await?;
        }
        raw.shutdown().await?;
        return Ok(());
    }

    let mut recv = recv;
    loop {
        let mut header = [0u8; ENVELOPE_HEADER_LENGTH];
        match tokio::io::AsyncReadExt::read(&mut recv, &mut header[..1]).await {
            Ok(0) => {
                raw.shutdown().await?;
                return Ok(());
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::UnexpectedEof
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::ConnectionReset
                ) =>
            {
                raw.shutdown().await?;
                return Ok(());
            }
            Err(_) => {
                return Err(MezError::invalid_state(
                    "failed to read negotiated Iroh frame header",
                ));
            }
        }
        tokio::io::AsyncReadExt::read_exact(&mut recv, &mut header[1..])
            .await
            .map_err(|_| MezError::invalid_state("negotiated Iroh frame header was truncated"))?;
        let envelope_length = policy.declared_envelope_length(&header)?;
        let mut envelope = Vec::with_capacity(envelope_length);
        envelope.extend_from_slice(&header);
        envelope.resize(envelope_length, 0);
        tokio::io::AsyncReadExt::read_exact(&mut recv, &mut envelope[ENVELOPE_HEADER_LENGTH..])
            .await
            .map_err(|_| MezError::invalid_state("negotiated Iroh frame was truncated"))?;
        let decoded = policy.decode_frame(&envelope)?;
        metrics.record_frame(
            envelope.len(),
            decoded.len(),
            envelope[4] == FLAG_COMPRESSED,
        );
        let (_, consumed) = decode_frame(&decoded, max_content_length)?;
        if consumed != decoded.len() {
            return Err(MezError::invalid_args(
                "negotiated Iroh envelope must contain exactly one control frame",
            ));
        }
        raw.write_all(&decoded).await?;
        raw.flush().await?;
    }
}

async fn pump_streaming_iroh_frames_to_raw<W>(
    recv: iroh::endpoint::RecvStream,
    mut raw: W,
    policy: IrohCompressionPolicy,
    metrics: IrohCompressionMetrics,
    max_content_length: usize,
    activation: Option<Arc<tokio::sync::Barrier>>,
) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let codec = ProtocolFrameCodec::new(max_content_length)?;
    let mut framed = FramedRead::new(recv, codec);
    let Some(initialization) = framed.next().await else {
        raw.shutdown().await?;
        return Ok(());
    };
    let initialization = encode_frame(&initialization?);
    policy.validate_decoded_length(initialization.len())?;
    metrics.record_frame(initialization.len(), initialization.len(), false);
    raw.write_all(&initialization).await?;
    raw.flush().await?;
    if let Some(activation) = activation {
        activation.wait().await;
    }

    let parts = framed.into_parts();
    let mut recv = parts.io;
    let mut pending = parts.read_buf.to_vec();
    let mut decoder = IrohStreamDecoder::new(policy)?;
    let mut buffer = [0u8; STREAM_CODEC_SCRATCH_BYTES];
    loop {
        while let Some(record) = decoder.decode_record(&pending)? {
            let (_, consumed) = decode_frame(record.as_bytes(), max_content_length)?;
            if consumed != record.as_bytes().len() {
                return Err(MezError::invalid_args(
                    "stateful Iroh record must contain exactly one control frame",
                ));
            }
            metrics.record_frame(
                record.consumed(),
                record.as_bytes().len(),
                record.compressed(),
            );
            raw.write_all(record.as_bytes()).await?;
            raw.flush().await?;
            pending.drain(..record.consumed());
        }
        if pending.len() > policy.stream_record_wire_limit() {
            return Err(MezError::invalid_args(
                "stateful Iroh control record exceeds its configured limit",
            ));
        }
        let read = tokio::io::AsyncReadExt::read(&mut recv, &mut buffer)
            .await
            .map_err(|_| MezError::invalid_state("failed to read stateful Iroh control stream"))?;
        if read == 0 {
            if !pending.is_empty() {
                return Err(MezError::invalid_state(
                    "stateful Iroh control stream ended with an incomplete record",
                ));
            }
            raw.shutdown().await?;
            return Ok(());
        }
        pending.extend_from_slice(&buffer[..read]);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParsedEnvelopeHeader {
    compressed: bool,
    encoded_length: usize,
    decoded_length: usize,
}

/// Direction-local encoder for one version 3 Iroh application stream.
pub(crate) struct IrohStreamEncoder {
    policy: IrohCompressionPolicy,
    state: IrohStreamEncoderState,
}

enum IrohStreamEncoderState {
    Zstd(ZstdStreamEncoder<'static>),
    Lz4 { history: Vec<u8> },
}

/// Direction-local decoder for one version 3 Iroh application stream.
pub(crate) struct IrohStreamDecoder {
    policy: IrohCompressionPolicy,
    state: IrohStreamDecoderState,
}

enum IrohStreamDecoderState {
    Zstd(ZstdStreamDecoder<'static>),
    Lz4 { history: Vec<u8> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParsedStreamRecordHeader {
    flags: u8,
    header_length: usize,
    encoded_length: usize,
    decoded_length: usize,
}

impl ParsedStreamRecordHeader {
    fn record_length(self) -> Result<usize> {
        self.header_length
            .checked_add(self.encoded_length)
            .ok_or_else(|| MezError::invalid_args("Iroh stream record length overflow"))
    }

    const fn compressed(self) -> bool {
        self.flags & STREAM_FLAG_COMPRESSED != 0
    }

    const fn reset(self) -> bool {
        self.flags & STREAM_FLAG_RESET != 0
    }
}

impl IrohStreamEncoder {
    /// Creates an encoder with fresh history for exactly one stream direction.
    pub(crate) fn new(policy: IrohCompressionPolicy) -> Result<Self> {
        let state = match policy.codec() {
            RuntimeIrohCompressionCodec::ZstdStream => {
                let mut encoder = ZstdStreamEncoder::new(policy.zstd_level).map_err(|_| {
                    MezError::invalid_state("failed to initialize Iroh Zstandard stream encoder")
                })?;
                encoder
                    .set_parameter(CParameter::WindowLog(STREAM_ZSTD_WINDOW_LOG))
                    .map_err(|_| {
                        MezError::invalid_state("failed to bound Iroh Zstandard stream window")
                    })?;
                IrohStreamEncoderState::Zstd(encoder)
            }
            RuntimeIrohCompressionCodec::Lz4Stream => IrohStreamEncoderState::Lz4 {
                history: Vec::with_capacity(STREAM_HISTORY_BYTES),
            },
            _ => {
                return Err(MezError::invalid_state(
                    "Iroh stream encoder requires a version 3 codec",
                ));
            }
        };
        Ok(Self { policy, state })
    }

    /// Encodes and codec-flushes one complete inner Content-Length frame.
    ///
    /// Identity-only records reset history before and after their payload so
    /// sensitive bytes neither reference nor seed reusable compression state.
    pub(crate) fn encode_frame(
        &mut self,
        frame: &[u8],
        mode: IrohFrameCompressionMode,
    ) -> Result<IrohEncodedFrame> {
        self.policy.validate_decoded_length(frame.len())?;
        let (flags, payload, compressed) = if mode == IrohFrameCompressionMode::IdentityOnly {
            self.reset()?;
            let payload = frame.to_vec();
            self.reset()?;
            (STREAM_FLAG_RESET, payload, false)
        } else {
            let payload = match &mut self.state {
                IrohStreamEncoderState::Zstd(encoder) => encode_zstd_stream_record(encoder, frame)?,
                IrohStreamEncoderState::Lz4 { history } => {
                    let payload = lz4_flex::block::compress_with_dict(frame, history);
                    update_stream_history(history, frame);
                    payload
                }
            };
            (STREAM_FLAG_COMPRESSED, payload, true)
        };
        if payload.len() > self.policy.stream_record_payload_limit() {
            return Err(MezError::invalid_args(
                "Iroh stream encoded record exceeds its configured limit",
            ));
        }
        let mut bytes = Vec::with_capacity(payload.len() + 11);
        bytes.push(flags);
        append_stream_varint(&mut bytes, payload.len())?;
        append_stream_varint(&mut bytes, frame.len())?;
        bytes.extend_from_slice(&payload);
        Ok(IrohEncodedFrame {
            bytes,
            compressed,
            decoded_bytes: frame.len(),
        })
    }

    fn reset(&mut self) -> Result<()> {
        match &mut self.state {
            IrohStreamEncoderState::Zstd(encoder) => encoder.reinit().map_err(|_| {
                MezError::invalid_state("failed to reset Iroh Zstandard stream encoder")
            }),
            IrohStreamEncoderState::Lz4 { history } => {
                history.clear();
                Ok(())
            }
        }
    }
}

fn encode_zstd_stream_record(
    encoder: &mut ZstdStreamEncoder<'static>,
    frame: &[u8],
) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    let mut input = InBuffer::around(frame);
    let mut scratch = [0u8; STREAM_CODEC_SCRATCH_BYTES];
    while input.pos() < frame.len() {
        let before = input.pos();
        let written = {
            let mut output = OutBuffer::around(&mut scratch[..]);
            encoder.run(&mut input, &mut output).map_err(|_| {
                MezError::invalid_state("failed to encode Iroh Zstandard stream record")
            })?;
            output.pos()
        };
        payload.extend_from_slice(&scratch[..written]);
        if input.pos() == before && written == 0 {
            return Err(MezError::invalid_state(
                "Iroh Zstandard stream encoder made no progress",
            ));
        }
    }
    loop {
        let (remaining, written) = {
            let mut output = OutBuffer::around(&mut scratch[..]);
            let remaining = encoder.flush(&mut output).map_err(|_| {
                MezError::invalid_state("failed to flush Iroh Zstandard stream record")
            })?;
            (remaining, output.pos())
        };
        payload.extend_from_slice(&scratch[..written]);
        if remaining == 0 {
            return Ok(payload);
        }
        if written == 0 {
            return Err(MezError::invalid_state(
                "Iroh Zstandard stream flush made no progress",
            ));
        }
    }
}

fn append_stream_varint(output: &mut Vec<u8>, value: usize) -> Result<()> {
    let mut value = u32::try_from(value)
        .map_err(|_| MezError::invalid_args("Iroh stream record length exceeds wire range"))?;
    loop {
        let byte = u8::try_from(value & 0x7f)
            .map_err(|_| MezError::invalid_state("invalid Iroh stream varint byte"))?;
        value >>= 7;
        output.push(if value == 0 { byte } else { byte | 0x80 });
        if value == 0 {
            return Ok(());
        }
    }
}

fn update_stream_history(history: &mut Vec<u8>, decoded: &[u8]) {
    if decoded.len() >= STREAM_HISTORY_BYTES {
        history.clear();
        history.extend_from_slice(&decoded[decoded.len() - STREAM_HISTORY_BYTES..]);
        return;
    }
    let excess = history
        .len()
        .saturating_add(decoded.len())
        .saturating_sub(STREAM_HISTORY_BYTES);
    if excess > 0 {
        history.drain(..excess);
    }
    history.extend_from_slice(decoded);
}

impl IrohStreamDecoder {
    /// Creates a decoder with fresh history for exactly one stream direction.
    pub(crate) fn new(policy: IrohCompressionPolicy) -> Result<Self> {
        let state = match policy.codec() {
            RuntimeIrohCompressionCodec::ZstdStream => {
                let mut decoder = ZstdStreamDecoder::new().map_err(|_| {
                    MezError::invalid_state("failed to initialize Iroh Zstandard stream decoder")
                })?;
                decoder
                    .set_parameter(DParameter::WindowLogMax(STREAM_ZSTD_WINDOW_LOG))
                    .map_err(|_| {
                        MezError::invalid_state("failed to bound Iroh Zstandard decode window")
                    })?;
                IrohStreamDecoderState::Zstd(decoder)
            }
            RuntimeIrohCompressionCodec::Lz4Stream => IrohStreamDecoderState::Lz4 {
                history: Vec::with_capacity(STREAM_HISTORY_BYTES),
            },
            _ => {
                return Err(MezError::invalid_state(
                    "Iroh stream decoder requires a version 3 codec",
                ));
            }
        };
        Ok(Self { policy, state })
    }

    /// Decodes one complete record when enough bytes are available.
    pub(crate) fn decode_record(&mut self, input: &[u8]) -> Result<Option<IrohDecodedRecord>> {
        let Some(header) = parse_stream_record_header(input, self.policy)? else {
            return Ok(None);
        };
        let record_length = header.record_length()?;
        if input.len() < record_length {
            return Ok(None);
        }
        let payload = &input[header.header_length..record_length];
        if header.reset() {
            self.reset()?;
        }
        let decoded = if header.compressed() {
            match &mut self.state {
                IrohStreamDecoderState::Zstd(decoder) => {
                    decode_zstd_stream_record(decoder, payload, header.decoded_length)?
                }
                IrohStreamDecoderState::Lz4 { history } => {
                    let decoded = lz4_flex::block::decompress_with_dict(
                        payload,
                        header.decoded_length,
                        history,
                    )
                    .map_err(|_| {
                        MezError::invalid_args("invalid bounded Iroh LZ4 stream record")
                    })?;
                    update_stream_history(history, &decoded);
                    decoded
                }
            }
        } else {
            payload.to_vec()
        };
        if decoded.len() != header.decoded_length {
            return Err(MezError::invalid_args(
                "decoded Iroh stream record length does not match its declaration",
            ));
        }
        if header.reset() {
            self.reset()?;
        }
        Ok(Some(IrohDecodedRecord {
            bytes: decoded,
            consumed: record_length,
            compressed: header.compressed(),
        }))
    }

    fn reset(&mut self) -> Result<()> {
        match &mut self.state {
            IrohStreamDecoderState::Zstd(decoder) => decoder.reinit().map_err(|_| {
                MezError::invalid_state("failed to reset Iroh Zstandard stream decoder")
            }),
            IrohStreamDecoderState::Lz4 { history } => {
                history.clear();
                Ok(())
            }
        }
    }
}

/// One decoded v3 record plus wire accounting metadata.
pub(crate) struct IrohDecodedRecord {
    bytes: Vec<u8>,
    consumed: usize,
    compressed: bool,
}

impl IrohDecodedRecord {
    /// Returns the complete decoded inner Content-Length frame.
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the number of v3 wire bytes consumed by this record.
    pub(crate) const fn consumed(&self) -> usize {
        self.consumed
    }

    /// Reports whether the record payload used the negotiated codec.
    pub(crate) const fn compressed(&self) -> bool {
        self.compressed
    }
}

fn parse_stream_record_header(
    input: &[u8],
    policy: IrohCompressionPolicy,
) -> Result<Option<ParsedStreamRecordHeader>> {
    let Some(&flags) = input.first() else {
        return Ok(None);
    };
    if flags & !STREAM_KNOWN_FLAGS != 0 {
        return Err(MezError::invalid_args(
            "unsupported Iroh stream record flags",
        ));
    }
    if flags & STREAM_FLAG_COMPRESSED == 0 && flags & STREAM_FLAG_RESET == 0 {
        return Err(MezError::invalid_args(
            "Iroh stream identity record must reset history",
        ));
    }
    if flags & STREAM_FLAG_COMPRESSED != 0 && flags & STREAM_FLAG_RESET != 0 {
        return Err(MezError::invalid_args(
            "Iroh stream record cannot be compressed and reset history",
        ));
    }
    let Some((encoded_length, encoded_bytes)) = parse_stream_varint(&input[1..])? else {
        return Ok(None);
    };
    let decoded_offset = 1 + encoded_bytes;
    let Some((decoded_length, decoded_bytes)) = parse_stream_varint(&input[decoded_offset..])?
    else {
        return Ok(None);
    };
    policy.validate_decoded_length(decoded_length)?;
    if encoded_length == 0 {
        return Err(MezError::invalid_args(
            "Iroh stream encoded record length must be non-zero",
        ));
    }
    if encoded_length > policy.stream_record_payload_limit() {
        return Err(MezError::invalid_args(
            "Iroh stream encoded record exceeds its configured limit",
        ));
    }
    if flags & STREAM_FLAG_COMPRESSED == 0 && encoded_length != decoded_length {
        return Err(MezError::invalid_args(
            "Iroh stream identity record lengths do not match",
        ));
    }
    Ok(Some(ParsedStreamRecordHeader {
        flags,
        header_length: decoded_offset + decoded_bytes,
        encoded_length,
        decoded_length,
    }))
}

fn parse_stream_varint(input: &[u8]) -> Result<Option<(usize, usize)>> {
    let mut value = 0u32;
    for (index, byte) in input.iter().copied().take(5).enumerate() {
        if index == 4 && byte & 0xf0 != 0 {
            return Err(MezError::invalid_args(
                "Iroh stream record varint exceeds wire range",
            ));
        }
        value |= u32::from(byte & 0x7f) << (index * 7);
        if byte & 0x80 == 0 {
            return Ok(Some((value as usize, index + 1)));
        }
    }
    if input.len() < 5 {
        Ok(None)
    } else {
        Err(MezError::invalid_args(
            "Iroh stream record varint is unterminated",
        ))
    }
}

fn decode_zstd_stream_record(
    decoder: &mut ZstdStreamDecoder<'static>,
    payload: &[u8],
    decoded_length: usize,
) -> Result<Vec<u8>> {
    let mut decoded = vec![0u8; decoded_length];
    let mut input = InBuffer::around(payload);
    let produced = {
        let mut output = OutBuffer::around(&mut decoded[..]);
        while input.pos() < payload.len() {
            let input_before = input.pos();
            let output_before = output.pos();
            decoder.run(&mut input, &mut output).map_err(|_| {
                MezError::invalid_args("invalid bounded Iroh Zstandard stream record")
            })?;
            if input.pos() == input_before && output.pos() == output_before {
                return Err(MezError::invalid_args(
                    "Iroh Zstandard stream decoder made no progress",
                ));
            }
            if output.pos() == decoded_length && input.pos() < payload.len() {
                return Err(MezError::invalid_args(
                    "Iroh Zstandard stream record exceeds its decoded declaration",
                ));
            }
        }
        output.pos()
    };
    if produced != decoded_length {
        return Err(MezError::invalid_args(
            "Iroh Zstandard stream record did not produce its decoded declaration",
        ));
    }
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicBool, AtomicU64};

    use iroh::endpoint::{PortmapperConfig, presets};
    use iroh::{Endpoint, RelayMode};
    use tokio::io::AsyncReadExt;

    struct CountingAllocator;

    static COUNT_ALLOCATIONS: AtomicBool = AtomicBool::new(false);
    static ALLOCATION_COUNT: AtomicU64 = AtomicU64::new(0);
    static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

    #[global_allocator]
    static TEST_ALLOCATOR: CountingAllocator = CountingAllocator;

    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            if COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
                ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
                ALLOCATED_BYTES.fetch_add(
                    u64::try_from(layout.size()).unwrap_or(u64::MAX),
                    Ordering::Relaxed,
                );
            }
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            unsafe { System.dealloc(pointer, layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            if COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
                ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
                ALLOCATED_BYTES.fetch_add(
                    u64::try_from(layout.size()).unwrap_or(u64::MAX),
                    Ordering::Relaxed,
                );
            }
            unsafe { System.alloc_zeroed(layout) }
        }

        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            if COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
                ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
                ALLOCATED_BYTES.fetch_add(
                    u64::try_from(new_size).unwrap_or(u64::MAX),
                    Ordering::Relaxed,
                );
            }
            unsafe { System.realloc(pointer, layout, new_size) }
        }
    }

    fn begin_allocation_count() {
        ALLOCATION_COUNT.store(0, Ordering::Relaxed);
        ALLOCATED_BYTES.store(0, Ordering::Relaxed);
        COUNT_ALLOCATIONS.store(true, Ordering::SeqCst);
    }

    fn finish_allocation_count() -> (u64, u64) {
        COUNT_ALLOCATIONS.store(false, Ordering::SeqCst);
        (
            ALLOCATION_COUNT.load(Ordering::Relaxed),
            ALLOCATED_BYTES.load(Ordering::Relaxed),
        )
    }

    fn policy(codec: RuntimeIrohCompressionCodec, min_bytes: usize) -> IrohCompressionPolicy {
        IrohCompressionPolicy::new(codec, min_bytes, 3, 4096).unwrap()
    }

    async fn test_iroh_stream_pair() -> (
        Endpoint,
        Endpoint,
        iroh::endpoint::Connection,
        iroh::endpoint::Connection,
        iroh::endpoint::SendStream,
        iroh::endpoint::RecvStream,
        iroh::endpoint::SendStream,
        iroh::endpoint::RecvStream,
    ) {
        const TEST_ALPN: &[u8] = b"mezzanine/compression-test/1";
        let server = Endpoint::builder(presets::Minimal)
            .alpns(vec![TEST_ALPN.to_vec()])
            .relay_mode(RelayMode::Disabled)
            .clear_address_lookup()
            .portmapper_config(PortmapperConfig::Disabled)
            .bind()
            .await
            .unwrap();
        let client = Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .clear_address_lookup()
            .portmapper_config(PortmapperConfig::Disabled)
            .bind()
            .await
            .unwrap();
        let server_addr = server.addr();
        let client_side = async {
            let connection = client.connect(server_addr, TEST_ALPN).await.unwrap();
            let (mut send, recv) = connection.open_bi().await.unwrap();
            send.write_all(&[0]).await.unwrap();
            send.flush().await.unwrap();
            (connection, send, recv)
        };
        let server_side = async {
            let incoming = server.accept().await.unwrap();
            let connection = incoming.accept().unwrap().await.unwrap();
            let (send, mut recv) = connection.accept_bi().await.unwrap();
            let mut marker = [0u8; 1];
            recv.read_exact(&mut marker).await.unwrap();
            (connection, send, recv)
        };
        let (
            (client_connection, client_send, client_recv),
            (server_connection, server_send, server_recv),
        ) = tokio::join!(client_side, server_side);
        (
            server,
            client,
            server_connection,
            client_connection,
            server_send,
            server_recv,
            client_send,
            client_recv,
        )
    }

    /// Verifies a version 3 bridge leaves both initialization frames raw and
    /// switches both directions to fresh stateful contexts only afterward.
    ///
    /// The follow-up request and response exercise the activation barrier and
    /// prove that existing bridge consumers continue to see unchanged inner
    /// Content-Length frames across the negotiated transition.
    #[tokio::test(flavor = "current_thread")]
    async fn streaming_bridge_preserves_raw_initialization_then_transitions_both_directions() {
        let (
            server,
            client,
            server_connection,
            client_connection,
            server_send,
            server_recv,
            mut client_send,
            mut client_recv,
        ) = test_iroh_stream_pair().await;
        let policy = policy(RuntimeIrohCompressionCodec::Lz4Stream, 1);
        let mut bridge =
            IrohCompressionBridge::spawn(server_recv, server_send, policy, 4096).unwrap();
        let initialize_request = crate::control::encode_control_body(
            r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{}}"#,
        );
        let initialize_response = crate::control::encode_control_body(
            r#"{"jsonrpc":"2.0","id":"init","result":{"ok":true}}"#,
        );
        let follow_up_request = crate::control::encode_control_body(
            r#"{"jsonrpc":"2.0","id":"next","method":"pane/list","params":{}}"#,
        );
        let follow_up_response = crate::control::encode_control_body(
            r#"{"jsonrpc":"2.0","id":"next","result":{"panes":[]}}"#,
        );

        client_send.write_all(&initialize_request).await.unwrap();
        client_send.flush().await.unwrap();
        let mut received_initialize_request = vec![0u8; initialize_request.len()];
        bridge
            .stream_mut()
            .read_exact(&mut received_initialize_request)
            .await
            .unwrap();
        assert_eq!(received_initialize_request, initialize_request);

        bridge
            .stream_mut()
            .write_all(&initialize_response)
            .await
            .unwrap();
        bridge.stream_mut().flush().await.unwrap();
        let mut received_initialize_response = vec![0u8; initialize_response.len()];
        client_recv
            .read_exact(&mut received_initialize_response)
            .await
            .unwrap();
        assert_eq!(received_initialize_response, initialize_response);

        let mut client_encoder = IrohStreamEncoder::new(policy).unwrap();
        let encoded_request = client_encoder
            .encode_frame(&follow_up_request, IrohFrameCompressionMode::Eligible)
            .unwrap();
        client_send
            .write_all(encoded_request.as_bytes())
            .await
            .unwrap();
        client_send.flush().await.unwrap();
        let mut received_follow_up_request = vec![0u8; follow_up_request.len()];
        bridge
            .stream_mut()
            .read_exact(&mut received_follow_up_request)
            .await
            .unwrap();
        assert_eq!(received_follow_up_request, follow_up_request);

        bridge
            .stream_mut()
            .write_all(&follow_up_response)
            .await
            .unwrap();
        bridge.stream_mut().flush().await.unwrap();
        let mut client_decoder = IrohStreamDecoder::new(policy).unwrap();
        let mut pending = Vec::new();
        let decoded_response = loop {
            let mut byte = [0u8; 1];
            client_recv.read_exact(&mut byte).await.unwrap();
            pending.push(byte[0]);
            if let Some(record) = client_decoder.decode_record(&pending).unwrap() {
                break record.as_bytes().to_vec();
            }
        };
        assert_eq!(decoded_response, follow_up_response);

        drop(bridge);
        server_connection.close(0u32.into(), b"test complete");
        client_connection.close(0u32.into(), b"test complete");
        server.close().await;
        client.close().await;
    }

    /// Dropping bridge ownership on an early connection error must cancel its
    /// bidirectional pump task instead of detaching work past the connection.
    #[tokio::test(flavor = "current_thread")]
    async fn dropped_bridge_aborts_pending_pump_task() {
        let (
            server,
            client,
            server_connection,
            client_connection,
            server_send,
            server_recv,
            mut client_send,
            _client_recv,
        ) = test_iroh_stream_pair().await;
        client_send.finish().unwrap();
        let bridge = IrohCompressionBridge::spawn(
            server_recv,
            server_send,
            policy(RuntimeIrohCompressionCodec::None, 1),
            4096,
        )
        .unwrap();
        let task = bridge.task.abort_handle();

        drop(bridge);

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !task.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("dropped bridge pump task should be cancelled");
        assert!(task.is_finished());

        server_connection.close(0u32.into(), b"test complete");
        client_connection.close(0u32.into(), b"test complete");
        server.close().await;
        client.close().await;
    }

    /// Finishing the outbound pump succeeds only after the peer has received
    /// the complete final frame and acknowledged the finished QUIC stream.
    #[tokio::test(flavor = "current_thread")]
    async fn outbound_pump_waits_for_clean_final_frame_acknowledgement() {
        let (
            server,
            client,
            server_connection,
            client_connection,
            server_send,
            _server_recv,
            mut client_send,
            mut client_recv,
        ) = test_iroh_stream_pair().await;
        client_send.finish().unwrap();
        let (mut raw_writer, raw_reader) = tokio::io::duplex(4096);
        let frame = crate::control::encode_control_body(r#"{"final":true}"#);
        let outbound = tokio::spawn(pump_raw_frames_to_iroh(
            raw_reader,
            server_send,
            policy(RuntimeIrohCompressionCodec::None, 1),
            IrohCompressionMetrics::new(RuntimeIrohCompressionCodec::None),
            ProtocolFrameCodec::new(4096).unwrap(),
        ));

        raw_writer.write_all(&frame).await.unwrap();
        raw_writer.shutdown().await.unwrap();
        let received = client_recv.read_to_end(4096).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), outbound)
            .await
            .unwrap()
            .unwrap()
            .unwrap();

        assert_eq!(received, frame);
        server_connection.close(0u32.into(), b"test complete");
        client_connection.close(0u32.into(), b"test complete");
        server.close().await;
        client.close().await;
    }

    /// A peer STOP received after the final frame bytes but before local FIN
    /// must be reported instead of being mistaken for clean acknowledgement.
    #[tokio::test(flavor = "current_thread")]
    async fn outbound_pump_reports_peer_reset_before_acknowledgement() {
        let (
            server,
            client,
            server_connection,
            client_connection,
            server_send,
            _server_recv,
            mut client_send,
            mut client_recv,
        ) = test_iroh_stream_pair().await;
        client_send.finish().unwrap();
        let (mut raw_writer, raw_reader) = tokio::io::duplex(4096);
        let frame = crate::control::encode_control_body(r#"{"final":true}"#);
        let outbound = tokio::spawn(pump_raw_frames_to_iroh(
            raw_reader,
            server_send,
            policy(RuntimeIrohCompressionCodec::None, 1),
            IrohCompressionMetrics::new(RuntimeIrohCompressionCodec::None),
            ProtocolFrameCodec::new(4096).unwrap(),
        ));

        raw_writer.write_all(&frame).await.unwrap();
        let mut received = vec![0u8; frame.len()];
        client_recv.read_exact(&mut received).await.unwrap();
        client_recv.stop(42u32.into()).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        raw_writer.shutdown().await.unwrap();
        let error = tokio::time::timeout(std::time::Duration::from_secs(2), outbound)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();

        assert_eq!(received, frame);
        assert!(error.message().contains("peer reset"), "{error:?}");
        server_connection.close(0u32.into(), b"test complete");
        client_connection.close(0u32.into(), b"test complete");
        server.close().await;
        client.close().await;
    }

    /// Verifies connection-local counters distinguish compressed and identity
    /// frames without retaining payloads and start empty for a new connection.
    #[test]
    fn compression_metrics_count_wire_decoded_and_bypass_frames() {
        let metrics = IrohCompressionMetrics::new(RuntimeIrohCompressionCodec::Zstd);
        assert_eq!(
            metrics.snapshot(),
            IrohCompressionMetricsSnapshot {
                codec: RuntimeIrohCompressionCodec::Zstd,
                wire_bytes: 0,
                decoded_bytes: 0,
                compressed_frames: 0,
                identity_frames: 0,
                render_snapshot_frames: 0,
                render_delta_frames: 0,
                render_changed_rows: 0,
                render_selected_wire_bytes: 0,
                render_selected_decoded_bytes: 0,
                render_snapshot_candidate_bytes: 0,
                render_triggers_coalesced: 0,
                render_updates_suppressed: 0,
                render_snapshot_fallbacks: 0,
                render_ready_depth_max: 0,
                render_write_wait_micros: 0,
                render_write_wait_max_micros: 0,
            }
        );

        metrics.record_frame(128, 512, true);
        metrics.record_frame(48, 32, false);
        metrics.record_render_update(false, 24, 128, 512, 512);
        metrics.record_render_update(true, 1, 48, 96, 512);
        metrics.record_render_coalescing(5, true, true);
        metrics.record_render_write_wait(std::time::Duration::from_micros(250));

        assert_eq!(
            metrics.snapshot(),
            IrohCompressionMetricsSnapshot {
                codec: RuntimeIrohCompressionCodec::Zstd,
                wire_bytes: 176,
                decoded_bytes: 544,
                compressed_frames: 1,
                identity_frames: 1,
                render_snapshot_frames: 1,
                render_delta_frames: 1,
                render_changed_rows: 25,
                render_selected_wire_bytes: 176,
                render_selected_decoded_bytes: 608,
                render_snapshot_candidate_bytes: 1_024,
                render_triggers_coalesced: 4,
                render_updates_suppressed: 1,
                render_snapshot_fallbacks: 1,
                render_ready_depth_max: 5,
                render_write_wait_micros: 250,
                render_write_wait_max_micros: 250,
            }
        );
    }

    /// Reports reproducible release-mode compression measurements for the
    /// representative control, terminal, config, incompressible, and
    /// bidirectional frame classes used by rollout review.
    #[test]
    #[ignore = "release-mode report; run with `just iroh-compression-bench`"]
    fn iroh_compression_release_benchmark() {
        const ITERATIONS: usize = 2_000;
        let small_control = crate::control::encode_control_body(
            r#"{"jsonrpc":"2.0","id":1,"method":"session/get","params":{}}"#,
        );
        let repetitive_terminal = crate::control::encode_control_body(
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": { "lines": vec!["prompt> repeated terminal output"; 256] }
            })
            .to_string(),
        );
        let json_config = crate::control::encode_control_body(
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": 3,
                "result": {
                    "providers": (0..64).map(|index| serde_json::json!({
                        "name": format!("provider-{index}"),
                        "enabled": true,
                        "models": ["small", "medium", "large"]
                    })).collect::<Vec<_>>()
                }
            })
            .to_string(),
        );
        let mut random_state = 0x1234_5678_9abc_def0u64;
        let incompressible_body = (0..8_192)
            .map(|_| {
                random_state ^= random_state << 13;
                random_state ^= random_state >> 7;
                random_state ^= random_state << 17;
                char::from(b'!' + u8::try_from(random_state % 90).unwrap())
            })
            .collect::<String>();
        let incompressible = crate::control::encode_control_body(&incompressible_body);
        let bidirectional = vec![
            small_control.clone(),
            repetitive_terminal.clone(),
            crate::control::encode_control_body(
                r#"{"jsonrpc":"2.0","method":"event/pane_changed","params":{"event_type":"pane_changed"}}"#,
            ),
        ];
        let classes = [
            ("small_control", vec![small_control]),
            ("repetitive_terminal", vec![repetitive_terminal]),
            ("json_config", vec![json_config]),
            ("incompressible", vec![incompressible]),
            ("bidirectional_attach_event", bidirectional),
        ];
        let mut results = Vec::new();
        for codec in [
            RuntimeIrohCompressionCodec::None,
            RuntimeIrohCompressionCodec::Zstd,
            RuntimeIrohCompressionCodec::Lz4,
        ] {
            for (class, frames) in &classes {
                let max_decoded_bytes = (frames.iter().map(Vec::len).max().unwrap() + 1).max(512);
                let policy = IrohCompressionPolicy::new(codec, 512, 3, max_decoded_bytes).unwrap();
                begin_allocation_count();
                let started = std::time::Instant::now();
                let mut wire_bytes = 0u64;
                let mut decoded_bytes = 0u64;
                for _ in 0..ITERATIONS {
                    for frame in frames {
                        let encoded = policy
                            .encode_frame(frame, IrohFrameCompressionMode::Eligible)
                            .unwrap();
                        wire_bytes = wire_bytes.saturating_add(encoded.as_bytes().len() as u64);
                        decoded_bytes = decoded_bytes.saturating_add(frame.len() as u64);
                        let decoded = policy.decode_frame(encoded.as_bytes()).unwrap();
                        assert_eq!(decoded, *frame);
                        std::hint::black_box(decoded);
                    }
                }
                let elapsed = started.elapsed();
                let (allocations, allocated_bytes) = finish_allocation_count();
                let operations = u64::try_from(ITERATIONS * frames.len()).unwrap();
                let elapsed_nanos = u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX).max(1);
                results.push(serde_json::json!({
                    "codec": codec.as_str(),
                    "class": class,
                    "iterations": ITERATIONS,
                    "operations": operations,
                    "decoded_bytes": decoded_bytes,
                    "wire_bytes": wire_bytes,
                    "wire_ratio": wire_bytes as f64 / decoded_bytes.max(1) as f64,
                    "elapsed_nanos": elapsed_nanos,
                    "nanoseconds_per_operation": elapsed_nanos as f64 / operations as f64,
                    "decoded_mebibytes_per_second": decoded_bytes as f64 * 1_000_000_000.0
                        / elapsed_nanos as f64 / (1024.0 * 1024.0),
                    "allocations": allocations,
                    "allocations_per_operation": allocations as f64 / operations as f64,
                    "allocated_bytes": allocated_bytes,
                }));
            }
        }
        let report = serde_json::to_string_pretty(&serde_json::json!({
            "format_version": 1,
            "iterations_per_fixture": ITERATIONS,
            "compression_min_bytes": 512,
            "compression_zstd_level": 3,
            "results": results,
        }))
        .unwrap();
        if let Ok(path) = std::env::var("MEZ_IROH_COMPRESSION_BENCH_REPORT") {
            std::fs::write(path, format!("{report}\n")).unwrap();
        }
        println!("{report}");
    }

    /// Verifies streaming codecs precede their frame-based alternatives during
    /// negotiation and unknown negotiated values are rejected rather than guessed.
    #[test]
    fn codec_alpns_are_closed_and_ordered() {
        let codecs = [
            RuntimeIrohCompressionCodec::Zstd,
            RuntimeIrohCompressionCodec::Lz4,
            RuntimeIrohCompressionCodec::Lz4Stream,
            RuntimeIrohCompressionCodec::ZstdStream,
            RuntimeIrohCompressionCodec::None,
        ];
        assert_eq!(
            IrohCompressionPolicy::ordered_alpns(&codecs),
            vec![
                MEZZANINE_IROH_LZ4_STREAM_ALPN.to_vec(),
                MEZZANINE_IROH_ZSTD_STREAM_ALPN.to_vec(),
                MEZZANINE_IROH_ZSTD_ALPN.to_vec(),
                MEZZANINE_IROH_LZ4_ALPN.to_vec(),
                super::super::MEZZANINE_IROH_ALPN.to_vec(),
            ]
        );
        for codec in codecs {
            assert_eq!(
                RuntimeIrohCompressionCodec::from_alpn(codec.alpn()).unwrap(),
                codec
            );
        }
        assert!(RuntimeIrohCompressionCodec::from_alpn(b"unknown").is_err());
    }

    /// Verifies both version 3 codecs reuse direction-local history across
    /// records, decode incrementally, and preserve exact inner frame bytes.
    ///
    /// The second repeated record should be smaller than the first because it
    /// can reference established stream history. A truncated record must stay
    /// buffered rather than advancing decoder state or exposing a partial
    /// application frame.
    #[test]
    fn streaming_codecs_reuse_history_and_decode_incrementally() {
        let frame = crate::control::encode_control_body(
            r#"{"jsonrpc":"2.0","id":1,"method":"session/get","params":{"padding":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}"#,
        );
        for codec in [
            RuntimeIrohCompressionCodec::ZstdStream,
            RuntimeIrohCompressionCodec::Lz4Stream,
        ] {
            let policy = policy(codec, 1);
            let mut encoder = IrohStreamEncoder::new(policy).unwrap();
            let mut decoder = IrohStreamDecoder::new(policy).unwrap();
            let first = encoder
                .encode_frame(&frame, IrohFrameCompressionMode::Eligible)
                .unwrap();
            let second = encoder
                .encode_frame(&frame, IrohFrameCompressionMode::Eligible)
                .unwrap();

            assert!(first.compressed(), "{codec:?}");
            assert!(second.compressed(), "{codec:?}");
            assert!(
                second.as_bytes().len() < first.as_bytes().len(),
                "{codec:?}: first={} second={}",
                first.as_bytes().len(),
                second.as_bytes().len()
            );
            assert!(
                decoder
                    .decode_record(&first.as_bytes()[..first.as_bytes().len() - 1])
                    .unwrap()
                    .is_none()
            );
            let decoded_first = decoder.decode_record(first.as_bytes()).unwrap().unwrap();
            assert_eq!(decoded_first.as_bytes(), frame);
            assert_eq!(decoded_first.consumed(), first.as_bytes().len());
            let decoded_second = decoder.decode_record(second.as_bytes()).unwrap().unwrap();
            assert_eq!(decoded_second.as_bytes(), frame);
            assert_eq!(decoded_second.consumed(), second.as_bytes().len());
        }
    }

    /// Verifies identity-only version 3 records reset codec history on both
    /// sides so sensitive bytes neither use prior context nor seed later data.
    ///
    /// A normal record after the reset must still round-trip through a fresh
    /// epoch, while malformed flag combinations fail before payload decoding.
    #[test]
    fn streaming_identity_records_reset_history_and_reject_invalid_flags() {
        let ordinary = crate::control::encode_control_body(
            r#"{"jsonrpc":"2.0","id":1,"method":"pane/list","params":{}}"#,
        );
        let sensitive = crate::control::encode_control_body(
            r#"{"jsonrpc":"2.0","method":"client/clipboard.chunk","params":{"data_base64":"c2VjcmV0"}}"#,
        );
        for codec in [
            RuntimeIrohCompressionCodec::ZstdStream,
            RuntimeIrohCompressionCodec::Lz4Stream,
        ] {
            let policy = policy(codec, 1);
            let mut encoder = IrohStreamEncoder::new(policy).unwrap();
            let mut decoder = IrohStreamDecoder::new(policy).unwrap();
            let before = encoder
                .encode_frame(&ordinary, IrohFrameCompressionMode::Eligible)
                .unwrap();
            let reset = encoder
                .encode_frame(&sensitive, IrohFrameCompressionMode::IdentityOnly)
                .unwrap();
            let after = encoder
                .encode_frame(&ordinary, IrohFrameCompressionMode::Eligible)
                .unwrap();

            assert_eq!(
                decoder
                    .decode_record(before.as_bytes())
                    .unwrap()
                    .unwrap()
                    .as_bytes(),
                ordinary
            );
            let decoded_reset = decoder.decode_record(reset.as_bytes()).unwrap().unwrap();
            assert_eq!(decoded_reset.as_bytes(), sensitive);
            assert!(!decoded_reset.compressed());
            assert_eq!(
                decoder
                    .decode_record(after.as_bytes())
                    .unwrap()
                    .unwrap()
                    .as_bytes(),
                ordinary
            );

            let mut malformed = reset.as_bytes().to_vec();
            malformed[0] = STREAM_FLAG_COMPRESSED | STREAM_FLAG_RESET;
            assert!(
                IrohStreamDecoder::new(policy)
                    .unwrap()
                    .decode_record(&malformed)
                    .is_err()
            );
        }
    }

    /// Verifies Zstandard and LZ4 independently round-trip one complete frame
    /// and actually select compression for representative repetitive content.
    #[test]
    fn compressed_codecs_round_trip_repetitive_frames() {
        let frame = b"Content-Length: 2048\r\n\r\nAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".repeat(32);
        for codec in [
            RuntimeIrohCompressionCodec::Zstd,
            RuntimeIrohCompressionCodec::Lz4,
        ] {
            let policy = policy(codec, 1);
            let encoded = policy
                .encode_frame(&frame, IrohFrameCompressionMode::Eligible)
                .unwrap();
            assert!(encoded.compressed(), "{codec:?}");
            assert_eq!(encoded.decoded_bytes(), frame.len());
            assert_eq!(policy.decode_frame(encoded.as_bytes()).unwrap(), frame);
        }
    }

    /// Verifies below-threshold, expansion-prone, and initialization frames use
    /// identity envelopes while retaining the negotiated compressed ALPN.
    #[test]
    fn version_two_identity_fallback_covers_threshold_expansion_and_initialization() {
        let zstd_policy = policy(RuntimeIrohCompressionCodec::Zstd, 64);
        let short = b"Content-Length: 2\r\n\r\n{}";
        let below_threshold = zstd_policy
            .encode_frame(short, IrohFrameCompressionMode::Eligible)
            .unwrap();
        assert!(!below_threshold.compressed());
        assert_eq!(
            zstd_policy
                .decode_frame(below_threshold.as_bytes())
                .unwrap(),
            short
        );

        let initialization = vec![b'A'; 512];
        let sensitive = zstd_policy
            .encode_frame(&initialization, IrohFrameCompressionMode::IdentityOnly)
            .unwrap();
        assert!(!sensitive.compressed());
        assert_eq!(
            zstd_policy.decode_frame(sensitive.as_bytes()).unwrap(),
            initialization
        );

        let tiny_policy = policy(RuntimeIrohCompressionCodec::Lz4, 1);
        let expansion = tiny_policy
            .encode_frame(&[1], IrohFrameCompressionMode::Eligible)
            .unwrap();
        assert!(!expansion.compressed());
    }

    /// Verifies the `none` codec preserves the existing version 1 frame bytes
    /// exactly and never introduces a version 2 envelope.
    #[test]
    fn none_codec_preserves_version_one_framing() {
        let policy = policy(RuntimeIrohCompressionCodec::None, 1);
        let frame = b"Content-Length: 2\r\n\r\n{}";
        let encoded = policy
            .encode_frame(frame, IrohFrameCompressionMode::Eligible)
            .unwrap();
        assert_eq!(encoded.as_bytes(), frame);
        assert_eq!(policy.decode_frame(frame).unwrap(), frame);
    }

    /// Verifies malformed magic, flags, reserved bytes, truncation, and trailing
    /// data are rejected before any decoded frame reaches the control layer.
    #[test]
    fn malformed_envelope_structure_is_rejected() {
        let policy = policy(RuntimeIrohCompressionCodec::Zstd, 1024);
        let encoded = policy
            .encode_frame(b"frame", IrohFrameCompressionMode::Eligible)
            .unwrap()
            .bytes;
        for mutation in [0usize, 4, 5] {
            let mut malformed = encoded.clone();
            malformed[mutation] = 0xff;
            assert!(policy.decode_frame(&malformed).is_err(), "byte {mutation}");
        }
        assert!(policy.decode_frame(&encoded[..encoded.len() - 1]).is_err());
        let mut trailing = encoded.clone();
        trailing.push(0);
        assert!(policy.decode_frame(&trailing).is_err());
    }

    /// Verifies declared encoded and decoded limits are checked from the fixed
    /// header before payload allocation or decompression can occur.
    #[test]
    fn declared_lengths_are_bounded_before_decode() {
        let policy =
            IrohCompressionPolicy::new(RuntimeIrohCompressionCodec::Zstd, 1, 3, 32).unwrap();
        let mut header = [0u8; ENVELOPE_HEADER_LENGTH];
        header[..4].copy_from_slice(ENVELOPE_MAGIC);
        header[4] = FLAG_COMPRESSED;
        header[8..12].copy_from_slice(&33u32.to_be_bytes());
        header[12..16].copy_from_slice(&32u32.to_be_bytes());
        assert!(policy.declared_envelope_length(&header).is_err());

        header[8..12].copy_from_slice(&1u32.to_be_bytes());
        header[12..16].copy_from_slice(&33u32.to_be_bytes());
        assert!(policy.declared_envelope_length(&header).is_err());
    }

    /// Verifies identity envelopes require equal lengths and compressed output
    /// must match the exact decoded length declared by the sender.
    #[test]
    fn envelope_requires_exact_decoded_lengths() {
        let identity_policy = policy(RuntimeIrohCompressionCodec::Zstd, 1024);
        let mut identity = identity_policy
            .encode_frame(b"frame", IrohFrameCompressionMode::Eligible)
            .unwrap()
            .bytes;
        identity[12..16].copy_from_slice(&4u32.to_be_bytes());
        assert!(identity_policy.decode_frame(&identity).is_err());

        let compressed_policy = policy(RuntimeIrohCompressionCodec::Zstd, 1);
        let mut compressed = compressed_policy
            .encode_frame(&vec![b'Z'; 256], IrohFrameCompressionMode::Eligible)
            .unwrap()
            .bytes;
        compressed[12..16].copy_from_slice(&255u32.to_be_bytes());
        assert!(compressed_policy.decode_frame(&compressed).is_err());
    }
}
