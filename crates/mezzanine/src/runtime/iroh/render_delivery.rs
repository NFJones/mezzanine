//! Bounded render fragmentation, encoding, and stream flush accounting.
//!
//! Delivery never commits the caller's render base or presentation receipts.
//! Successful return means every fragment and the final flush completed;
//! initialization and update failures retain distinct payload-free context.

use super::*;

/// Splits an oversized framed v3 render update into bounded v4 envelopes.
/// The original frame bytes remain unchanged through atomic reconstruction.
pub(super) fn encode_iroh_render_delivery_frames(
    frame: Vec<u8>,
    revision: u64,
    version: u32,
) -> Result<Vec<Vec<u8>>> {
    if version < 4 || frame.len() <= IROH_RENDER_FRAGMENT_BYTES {
        return Ok(vec![frame]);
    }
    if frame.len() > IROH_RENDER_FRAGMENT_MAX_TOTAL_BYTES {
        return Err(MezError::invalid_state(
            "Iroh rendered view exceeds the bounded v4 fragment transfer limit",
        ));
    }
    let total_bytes = frame.len();
    let chunks = frame.chunks(IROH_RENDER_FRAGMENT_BYTES).collect::<Vec<_>>();
    let chunk_count = chunks.len();
    debug_assert!(chunk_count <= IROH_RENDER_FRAGMENT_MAX_CHUNKS);
    Ok(chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            encode_control_body(
                &serde_json::json!({
                    "jsonrpc": "2.0", "method": "render/chunk",
                    "params": {"revision": revision, "index": index, "chunks": chunk_count,
                        "total_bytes": total_bytes,
                        "data_base64": base64::engine::general_purpose::STANDARD.encode(chunk)}
                })
                .to_string(),
            )
        })
        .collect())
}

/// Error context differs for the first snapshot and subsequent updates.
#[derive(Clone, Copy)]
pub(super) enum IrohRenderDeliveryPhase {
    InitialSnapshot,
    Update,
}

impl IrohRenderDeliveryPhase {
    /// Returns the phase-specific write timeout diagnostic.
    fn write_timeout(self) -> &'static str {
        match self {
            Self::InitialSnapshot => "Iroh render snapshot write timed out",
            Self::Update => "Iroh render update write timed out",
        }
    }
    /// Returns the phase-specific write failure diagnostic.
    pub(super) fn write_failed(self) -> &'static str {
        match self {
            Self::InitialSnapshot => "Iroh render snapshot write failed",
            Self::Update => "Iroh render update write failed",
        }
    }
    /// Returns the phase-specific flush timeout diagnostic.
    fn flush_timeout(self) -> &'static str {
        match self {
            Self::InitialSnapshot => "Iroh render snapshot flush timed out",
            Self::Update => "Iroh render update flush timed out",
        }
    }
    /// Returns the phase-specific flush failure diagnostic.
    pub(super) fn flush_failed(self) -> &'static str {
        match self {
            Self::InitialSnapshot => "Iroh render snapshot flush failed",
            Self::Update => "Iroh render update flush failed",
        }
    }
}

/// Encodes and writes every fragment, then flushes before reporting delivery.
#[allow(
    clippy::too_many_arguments,
    reason = "stream, render identity, codec state, accounting, timeout, and error phase are independent delivery inputs"
)]
pub(super) async fn write_iroh_render_delivery<W: tokio::io::AsyncWrite + Unpin>(
    send: &mut W,
    frame: Vec<u8>,
    revision: u64,
    version: u32,
    compression: IrohCompressionPolicy,
    stream_encoder: &mut Option<IrohStreamEncoder>,
    compression_metrics: &IrohCompressionMetrics,
    idle_timeout: Duration,
    phase: IrohRenderDeliveryPhase,
) -> Result<(usize, usize, Duration)> {
    let frames = encode_iroh_render_delivery_frames(frame, revision, version)?;
    let started = Instant::now();
    let mut wire_bytes = 0usize;
    let mut decoded_bytes = 0usize;
    for frame in frames {
        let frame = match stream_encoder.as_mut() {
            Some(encoder) => encoder.encode_frame(&frame, IrohFrameCompressionMode::Eligible)?,
            None => compression.encode_frame(&frame, IrohFrameCompressionMode::Eligible)?,
        };
        wire_bytes = wire_bytes.saturating_add(frame.as_bytes().len());
        decoded_bytes = decoded_bytes.saturating_add(frame.decoded_bytes());
        compression_metrics.record_frame(
            frame.as_bytes().len(),
            frame.decoded_bytes(),
            frame.compressed(),
        );
        tokio::time::timeout(idle_timeout, send.write_all(frame.as_bytes()))
            .await
            .map_err(|_| MezError::invalid_state(phase.write_timeout()))?
            .map_err(|_| MezError::invalid_state(phase.write_failed()))?;
    }
    tokio::time::timeout(idle_timeout, send.flush())
        .await
        .map_err(|_| MezError::invalid_state(phase.flush_timeout()))?
        .map_err(|_| MezError::invalid_state(phase.flush_failed()))?;
    Ok((wire_bytes, decoded_bytes, started.elapsed()))
}
