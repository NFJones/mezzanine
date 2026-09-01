//! Negotiated compression records for X11 forwarding over Iroh.
//!
//! The authenticated X11 stream preface remains raw. After that preface this
//! module adapts the connection's immutable compression policy into bounded
//! X11 records: raw chunks for `none`, independent envelopes for version-2
//! codecs, and fresh direction-local contexts for version-3 codecs. Encoder
//! and decoder state is deliberately owned by one X11 stream direction and is
//! never shared with control, event, another X11 stream, or the reverse path.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use zeroize::Zeroizing;

use crate::error::{MezError, Result};
use crate::runtime::config::RuntimeIrohCompressionCodec;
use crate::runtime::{
    IrohCompressionMetrics, IrohCompressionPolicy, IrohFrameCompressionMode, IrohStreamDecoder,
    IrohStreamEncoder,
};

/// Maximum X11 application bytes represented by one compression record.
const X11_RECORD_BYTES: usize = 64 * 1024;

/// One direction-local encoder for a single X11 Iroh stream.
pub(crate) struct X11IrohEncoder {
    policy: IrohCompressionPolicy,
    streaming: Option<IrohStreamEncoder>,
}

impl X11IrohEncoder {
    /// Creates fresh encoding state for one X11 stream direction.
    pub(crate) fn new(policy: IrohCompressionPolicy) -> Result<Self> {
        let policy = policy.with_max_decoded_bytes(X11_RECORD_BYTES)?;
        let streaming = policy
            .is_streaming()
            .then(|| IrohStreamEncoder::new(policy))
            .transpose()?;
        Ok(Self { policy, streaming })
    }

    /// Reports whether the selected transport leaves X11 bytes unframed.
    pub(crate) fn is_raw(&self) -> bool {
        self.policy.codec() == RuntimeIrohCompressionCodec::None
    }

    /// Writes one setup packet as an identity record and flushes it immediately.
    pub(crate) async fn write_setup<W>(
        &mut self,
        writer: &mut W,
        setup: &[u8],
        metrics: Option<&IrohCompressionMetrics>,
    ) -> Result<()>
    where
        W: AsyncWrite + Unpin,
    {
        let encoded = self.encode(setup, IrohFrameCompressionMode::IdentityOnly)?;
        writer.write_all(encoded.as_bytes()).await?;
        writer.flush().await?;
        if let Some(metrics) = metrics {
            metrics.record_frame(
                encoded.as_bytes().len(),
                encoded.decoded_bytes(),
                encoded.compressed(),
            );
        }
        Ok(())
    }

    /// Relays source bytes as immediately flushed bounded eligible records.
    pub(crate) async fn relay<R, W>(
        &mut self,
        source: &mut R,
        writer: &mut W,
        metrics: Option<&IrohCompressionMetrics>,
    ) -> Result<()>
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let mut buffer = vec![0u8; X11_RECORD_BYTES];
        loop {
            let read = source.read(&mut buffer).await?;
            if read == 0 {
                return Ok(());
            }
            let encoded = self.encode(&buffer[..read], IrohFrameCompressionMode::Eligible)?;
            writer.write_all(encoded.as_bytes()).await?;
            writer.flush().await?;
            if let Some(metrics) = metrics {
                metrics.record_frame(
                    encoded.as_bytes().len(),
                    encoded.decoded_bytes(),
                    encoded.compressed(),
                );
            }
        }
    }

    /// Encodes one non-empty bounded X11 record with this direction's state.
    fn encode(
        &mut self,
        bytes: &[u8],
        mode: IrohFrameCompressionMode,
    ) -> Result<crate::runtime::iroh_compression::IrohEncodedFrame> {
        match self.streaming.as_mut() {
            Some(encoder) => encoder.encode_frame(bytes, mode),
            None => self.policy.encode_frame(bytes, mode),
        }
    }
}

/// One direction-local decoder for a single X11 Iroh stream.
pub(crate) struct X11IrohDecoder {
    policy: IrohCompressionPolicy,
    streaming: Option<IrohStreamDecoder>,
    pending: Vec<u8>,
}

/// One decoded X11 record with privacy-safe wire accounting metadata.
struct X11DecodedRecord {
    bytes: Vec<u8>,
    wire_bytes: usize,
    compressed: bool,
}

impl X11IrohDecoder {
    /// Creates fresh decoding state for one X11 stream direction.
    pub(crate) fn new(policy: IrohCompressionPolicy) -> Result<Self> {
        let policy = policy.with_max_decoded_bytes(X11_RECORD_BYTES)?;
        let streaming = policy
            .is_streaming()
            .then(|| IrohStreamDecoder::new(policy))
            .transpose()?;
        Ok(Self {
            policy,
            streaming,
            pending: Vec::new(),
        })
    }

    /// Reports whether the selected transport leaves X11 bytes unframed.
    pub(crate) fn is_raw(&self) -> bool {
        self.policy.codec() == RuntimeIrohCompressionCodec::None
    }

    /// Reads exactly one identity setup record without discarding later records.
    pub(crate) async fn read_setup<R>(&mut self, reader: &mut R) -> Result<Zeroizing<Vec<u8>>>
    where
        R: AsyncRead + Unpin,
    {
        if self.is_raw() {
            return Err(MezError::invalid_state(
                "raw X11 setup must use the bounded X11 setup parser",
            ));
        }
        let record = self.read_record(reader).await?.ok_or_else(|| {
            MezError::invalid_state("X11 Iroh stream ended before its setup record")
        })?;
        if record.compressed {
            return Err(MezError::forbidden(
                "X11 setup record must reset compression history",
            ));
        }
        Ok(Zeroizing::new(record.bytes))
    }

    /// Relays complete decoded records and shuts down the local writer at EOF.
    pub(crate) async fn relay<R, W>(
        &mut self,
        reader: &mut R,
        writer: &mut W,
        metrics: Option<&IrohCompressionMetrics>,
    ) -> Result<()>
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        if self.is_raw() {
            let mut buffer = [0u8; X11_RECORD_BYTES];
            loop {
                let read = reader.read(&mut buffer).await?;
                if read == 0 {
                    writer.shutdown().await?;
                    return Ok(());
                }
                if let Some(metrics) = metrics {
                    metrics.record_frame(read, read, false);
                }
                writer.write_all(&buffer[..read]).await?;
                writer.flush().await?;
            }
        }

        while let Some(record) = self.read_record(reader).await? {
            if let Some(metrics) = metrics {
                metrics.record_frame(record.wire_bytes, record.bytes.len(), record.compressed);
            }
            writer.write_all(&record.bytes).await?;
            writer.flush().await?;
        }
        writer.shutdown().await?;
        Ok(())
    }

    /// Reads one complete independent or streaming record with strict EOF bounds.
    async fn read_record<R>(&mut self, reader: &mut R) -> Result<Option<X11DecodedRecord>>
    where
        R: AsyncRead + Unpin,
    {
        if self.streaming.is_some() {
            return self.read_streaming_record(reader).await;
        }
        self.read_independent_record(reader).await
    }

    /// Reads one exact version-2 envelope without consuming the next record.
    async fn read_independent_record<R>(
        &mut self,
        reader: &mut R,
    ) -> Result<Option<X11DecodedRecord>>
    where
        R: AsyncRead + Unpin,
    {
        let header_length = IrohCompressionPolicy::envelope_header_length();
        let mut header = vec![0u8; header_length];
        let first = reader.read(&mut header[..1]).await?;
        if first == 0 {
            return Ok(None);
        }
        reader.read_exact(&mut header[1..]).await.map_err(|_| {
            MezError::invalid_state("X11 Iroh envelope ended with a truncated header")
        })?;
        let wire_bytes = self.policy.declared_envelope_length(&header)?;
        let mut envelope = header;
        envelope.resize(wire_bytes, 0);
        reader
            .read_exact(&mut envelope[header_length..])
            .await
            .map_err(|_| MezError::invalid_state("X11 Iroh envelope was truncated"))?;
        let compressed = envelope[4] != 0;
        let bytes = self.policy.decode_frame(&envelope)?;
        Ok(Some(X11DecodedRecord {
            bytes,
            wire_bytes,
            compressed,
        }))
    }

    /// Reads one version-3 record while retaining coalesced successor bytes.
    async fn read_streaming_record<R>(&mut self, reader: &mut R) -> Result<Option<X11DecodedRecord>>
    where
        R: AsyncRead + Unpin,
    {
        let decoder = self
            .streaming
            .as_mut()
            .ok_or_else(|| MezError::invalid_state("X11 streaming decoder state is unavailable"))?;
        let mut buffer = vec![0u8; X11_RECORD_BYTES];
        loop {
            if let Some(record) = decoder.decode_record(&self.pending)? {
                let wire_bytes = record.consumed();
                let decoded = X11DecodedRecord {
                    bytes: record.as_bytes().to_vec(),
                    wire_bytes,
                    compressed: record.compressed(),
                };
                self.pending.drain(..wire_bytes);
                return Ok(Some(decoded));
            }
            if self.pending.len() > self.policy.stream_record_wire_limit() {
                return Err(MezError::invalid_args(
                    "X11 Iroh stream record exceeds its configured limit",
                ));
            }
            let remaining = self
                .policy
                .stream_record_wire_limit()
                .saturating_sub(self.pending.len());
            if remaining == 0 {
                return Err(MezError::invalid_args(
                    "X11 Iroh stream record exceeds its configured limit",
                ));
            }
            let read_limit = remaining.min(buffer.len());
            let read = reader.read(&mut buffer[..read_limit]).await?;
            if read == 0 {
                if self.pending.is_empty() {
                    return Ok(None);
                }
                return Err(MezError::invalid_state(
                    "X11 Iroh stream ended with an incomplete record",
                ));
            }
            self.pending.extend_from_slice(&buffer[..read]);
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    /// Every compressed codec must preserve one identity setup record and two
    /// fragmented/coalesced application records without sharing setup history.
    #[tokio::test]
    async fn compressed_codecs_round_trip_fragmented_x11_records() {
        for codec in [
            RuntimeIrohCompressionCodec::Zstd,
            RuntimeIrohCompressionCodec::Lz4,
            RuntimeIrohCompressionCodec::ZstdStream,
            RuntimeIrohCompressionCodec::Lz4Stream,
        ] {
            let policy = test_policy(codec);
            let setup = vec![0x41; 48];
            let first = vec![0x52; 4096];
            let second = vec![0x63; X11_RECORD_BYTES];
            let mut encoder = X11IrohEncoder::new(policy).unwrap();
            let encoded_setup = encoder
                .encode(&setup, IrohFrameCompressionMode::IdentityOnly)
                .unwrap();
            let encoded_first = encoder
                .encode(&first, IrohFrameCompressionMode::Eligible)
                .unwrap();
            let encoded_second = encoder
                .encode(&second, IrohFrameCompressionMode::Eligible)
                .unwrap();
            assert!(!encoded_setup.compressed(), "{codec:?}");
            assert!(encoded_first.compressed(), "{codec:?}");
            assert!(encoded_second.compressed(), "{codec:?}");

            if policy.is_streaming() {
                let mut fresh = X11IrohEncoder::new(policy).unwrap();
                let fresh_first = fresh
                    .encode(&first, IrohFrameCompressionMode::Eligible)
                    .unwrap();
                assert_eq!(
                    encoded_first.as_bytes(),
                    fresh_first.as_bytes(),
                    "identity setup must not seed {codec:?} history"
                );
            }

            let mut wire = Vec::new();
            wire.extend_from_slice(encoded_setup.as_bytes());
            wire.extend_from_slice(encoded_first.as_bytes());
            wire.extend_from_slice(encoded_second.as_bytes());
            let (mut writer, mut reader) = tokio::io::duplex(wire.len() + 1);
            let write_task = tokio::spawn(async move {
                for fragment in wire.chunks(3) {
                    writer.write_all(fragment).await.unwrap();
                }
                writer.shutdown().await.unwrap();
            });

            let mut decoder = X11IrohDecoder::new(policy).unwrap();
            assert_eq!(&*decoder.read_setup(&mut reader).await.unwrap(), &setup);
            let decoded_first = decoder.read_record(&mut reader).await.unwrap().unwrap();
            assert_eq!(decoded_first.bytes, first);
            let decoded_second = decoder.read_record(&mut reader).await.unwrap().unwrap();
            assert_eq!(decoded_second.bytes, second);
            assert!(decoder.read_record(&mut reader).await.unwrap().is_none());
            write_task.await.unwrap();
        }
    }

    /// The `none` codec must remain byte-for-byte raw while still accounting
    /// each successful source read as an identity record.
    #[tokio::test]
    async fn raw_codec_relay_is_byte_equivalent_and_accounted() {
        let policy = test_policy(RuntimeIrohCompressionCodec::None);
        let metrics = IrohCompressionMetrics::new(policy.codec());
        let payload = vec![0x7a; X11_RECORD_BYTES + 17];
        let (mut source_writer, mut source_reader) = tokio::io::duplex(payload.len() + 1);
        let (mut wire_writer, mut wire_reader) = tokio::io::duplex(payload.len() + 1);
        let expected = payload.clone();
        let source_task = tokio::spawn(async move {
            source_writer.write_all(&payload).await.unwrap();
            source_writer.shutdown().await.unwrap();
        });
        let relay_metrics = metrics.clone();
        let relay_task = tokio::spawn(async move {
            X11IrohEncoder::new(policy)
                .unwrap()
                .relay(&mut source_reader, &mut wire_writer, Some(&relay_metrics))
                .await
                .unwrap();
            wire_writer.shutdown().await.unwrap();
        });
        let mut actual = Vec::new();
        wire_reader.read_to_end(&mut actual).await.unwrap();
        source_task.await.unwrap();
        relay_task.await.unwrap();
        assert_eq!(actual, expected);
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.wire_bytes, expected.len() as u64);
        assert_eq!(snapshot.decoded_bytes, expected.len() as u64);
        assert_eq!(snapshot.compressed_frames, 0);
        assert!(snapshot.identity_frames >= 2);
    }

    /// Truncated independent and streaming setup records must fail without
    /// yielding credential bytes or treating EOF as a clean boundary.
    #[tokio::test]
    async fn compressed_setup_rejects_truncated_records() {
        for codec in [
            RuntimeIrohCompressionCodec::Zstd,
            RuntimeIrohCompressionCodec::Lz4,
            RuntimeIrohCompressionCodec::ZstdStream,
            RuntimeIrohCompressionCodec::Lz4Stream,
        ] {
            let policy = test_policy(codec);
            let mut encoder = X11IrohEncoder::new(policy).unwrap();
            let setup = encoder
                .encode(&[0x41; 48], IrohFrameCompressionMode::IdentityOnly)
                .unwrap();
            let truncated = setup.as_bytes()[..setup.as_bytes().len() - 1].to_vec();
            let (mut writer, mut reader) = tokio::io::duplex(truncated.len() + 1);
            writer.write_all(&truncated).await.unwrap();
            writer.shutdown().await.unwrap();
            let error = X11IrohDecoder::new(policy)
                .unwrap()
                .read_setup(&mut reader)
                .await
                .expect_err("truncated X11 setup records must fail");
            assert!(
                error.message().contains("truncated") || error.message().contains("incomplete")
            );
        }
    }

    /// Every compressed codec must reject a peer record whose declared
    /// decoded size exceeds the X11-specific 64 KiB record boundary.
    #[tokio::test]
    async fn compressed_codecs_reject_oversized_x11_records() {
        for codec in [
            RuntimeIrohCompressionCodec::Zstd,
            RuntimeIrohCompressionCodec::Lz4,
            RuntimeIrohCompressionCodec::ZstdStream,
            RuntimeIrohCompressionCodec::Lz4Stream,
        ] {
            let broad_policy =
                IrohCompressionPolicy::new(codec, 1, 3, X11_RECORD_BYTES + 1).unwrap();
            let encoded = if broad_policy.is_streaming() {
                IrohStreamEncoder::new(broad_policy)
                    .unwrap()
                    .encode_frame(
                        &vec![0x41; X11_RECORD_BYTES + 1],
                        IrohFrameCompressionMode::Eligible,
                    )
                    .unwrap()
            } else {
                broad_policy
                    .encode_frame(
                        &vec![0x41; X11_RECORD_BYTES + 1],
                        IrohFrameCompressionMode::Eligible,
                    )
                    .unwrap()
            };
            let (mut writer, mut reader) = tokio::io::duplex(encoded.as_bytes().len() + 1);
            writer.write_all(encoded.as_bytes()).await.unwrap();
            writer.shutdown().await.unwrap();

            let error = match X11IrohDecoder::new(broad_policy)
                .unwrap()
                .read_record(&mut reader)
                .await
            {
                Err(error) => error,
                Ok(_) => panic!("oversized X11 records must fail before delivery"),
            };
            assert!(
                error.message().contains("configured limit"),
                "{codec:?}: {error:?}"
            );
        }
    }

    /// Builds one maximum-X11-record policy for a selected negotiated codec.
    fn test_policy(codec: RuntimeIrohCompressionCodec) -> IrohCompressionPolicy {
        IrohCompressionPolicy::new(codec, 1, 3, X11_RECORD_BYTES).unwrap()
    }
}
