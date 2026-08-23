//! Bounded application-layer compression for versioned Iroh frames.
//!
//! Iroh and QUIC do not transparently compress application payloads. This
//! module therefore wraps one complete existing Mezzanine control or event
//! frame in one independently decodable version 2 envelope. The unchanged
//! version 1 ALPN remains raw, while compressed ALPNs use strict declared-size
//! checks before allocation or decompression. Initialization and other
//! credential-bearing traffic can be forced into identity envelopes so secrets
//! are never compressed with attacker-influenced data.

#![allow(
    dead_code,
    reason = "the framing foundation is consumed by the dependent Iroh runtime integration task"
)]

use crate::error::{MezError, Result};

use super::RuntimeIrohCompressionCodec;

/// ALPN identity for version 2 Iroh frames using Zstandard compression.
pub(crate) const MEZZANINE_IROH_ZSTD_ALPN: &[u8] = b"mezzanine/transport/2/zstd";
/// ALPN identity for version 2 Iroh frames using LZ4 block compression.
pub(crate) const MEZZANINE_IROH_LZ4_ALPN: &[u8] = b"mezzanine/transport/2/lz4";

const ENVELOPE_MAGIC: &[u8; 4] = b"MZC2";
const ENVELOPE_HEADER_LENGTH: usize = 16;
const FLAG_COMPRESSED: u8 = 0b0000_0001;
const KNOWN_FLAGS: u8 = FLAG_COMPRESSED;

impl RuntimeIrohCompressionCodec {
    /// Returns the deterministic ALPN associated with this configured codec.
    pub(crate) const fn alpn(self) -> &'static [u8] {
        match self {
            Self::Zstd => MEZZANINE_IROH_ZSTD_ALPN,
            Self::Lz4 => MEZZANINE_IROH_LZ4_ALPN,
            Self::None => super::MEZZANINE_IROH_ALPN,
        }
    }

    /// Maps a negotiated ALPN to its closed codec value.
    pub(crate) fn from_alpn(alpn: &[u8]) -> Result<Self> {
        match alpn {
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

    /// Returns ALPNs for configured codecs in exact preference order.
    pub(crate) fn ordered_alpns(codecs: &[RuntimeIrohCompressionCodec]) -> Vec<Vec<u8>> {
        codecs.iter().map(|codec| codec.alpn().to_vec()).collect()
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParsedEnvelopeHeader {
    compressed: bool,
    encoded_length: usize,
    decoded_length: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(codec: RuntimeIrohCompressionCodec, min_bytes: usize) -> IrohCompressionPolicy {
        IrohCompressionPolicy::new(codec, min_bytes, 3, 4096).unwrap()
    }

    /// Verifies codec preference maps directly to deterministic ALPN order and
    /// unknown negotiated values are rejected rather than guessed.
    #[test]
    fn codec_alpns_are_closed_and_ordered() {
        let codecs = [
            RuntimeIrohCompressionCodec::Lz4,
            RuntimeIrohCompressionCodec::Zstd,
            RuntimeIrohCompressionCodec::None,
        ];
        assert_eq!(
            IrohCompressionPolicy::ordered_alpns(&codecs),
            vec![
                MEZZANINE_IROH_LZ4_ALPN.to_vec(),
                MEZZANINE_IROH_ZSTD_ALPN.to_vec(),
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
