//! Bounded connection-local clipboard transfer assembly for terminal adapters.
//!
//! Preserves established attach sequencing, byte/chunk budgets, UTF-8 validation
//! and partial-transfer expiry. Callers must independently negotiate clipboard
//! authority and enforce enclosing frame budgets. This owner assembles content
//! only: it does not write a host clipboard, spawn workers, authenticate peers,
//! retain content in diagnostics, or grant input/presentation authority.

use crate::error::{MezError, Result};
use base64::Engine as _;

const IROH_CLIENT_CLIPBOARD_MAX_BYTES: usize = 8 * 1024 * 1024;
const IROH_CLIENT_CLIPBOARD_MAX_CHUNK_BYTES: usize = 256 * 1024;
const IROH_CLIENT_CLIPBOARD_MAX_CHUNKS: usize =
    IROH_CLIENT_CLIPBOARD_MAX_BYTES / IROH_CLIENT_CLIPBOARD_MAX_CHUNK_BYTES;

/// One bounded in-progress client clipboard transfer.
struct IrohClipboardTransfer {
    sequence: u64,
    total_bytes: usize,
    chunk_count: usize,
    next_index: usize,
    bytes: Vec<u8>,
    started_at: tokio::time::Instant,
}

/// Connection-local assembler for independently negotiated clipboard effects.
#[derive(Default)]
pub(crate) struct IrohClipboardAssembler {
    last_sequence: u64,
    transfer: Option<IrohClipboardTransfer>,
}

impl IrohClipboardAssembler {
    /// Applies one clipboard notification and returns completed UTF-8 content.
    /// Invalid sequencing or bounds reject with diagnostics excluding payloads.
    pub(crate) fn apply(&mut self, body: &str) -> Result<Option<String>> {
        self.discard_expired();
        let value: serde_json::Value = serde_json::from_str(body)
            .map_err(|_| MezError::invalid_args("invalid Iroh clipboard effect JSON"))?;
        let method = value
            .get("method")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| MezError::invalid_args("Iroh clipboard effect omitted method"))?;
        let params = value
            .get("params")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| MezError::invalid_args("Iroh clipboard effect omitted params"))?;
        let sequence = params
            .get("sequence")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| MezError::invalid_args("Iroh clipboard effect omitted sequence"))?;

        match method {
            "client/clipboard.begin" => {
                self.transfer = None;
                let total_bytes = params
                    .get("total_bytes")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok())
                    .ok_or_else(|| {
                        MezError::invalid_args("Iroh clipboard begin omitted total byte count")
                    })?;
                let chunk_count = params
                    .get("chunks")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok())
                    .ok_or_else(|| {
                        MezError::invalid_args("Iroh clipboard begin omitted chunk count")
                    })?;
                if sequence <= self.last_sequence
                    || total_bytes > IROH_CLIENT_CLIPBOARD_MAX_BYTES
                    || chunk_count == 0
                    || chunk_count > IROH_CLIENT_CLIPBOARD_MAX_CHUNKS
                {
                    return Err(MezError::invalid_args(
                        "Iroh clipboard begin exceeds sequence or size bounds",
                    ));
                }
                self.transfer = Some(IrohClipboardTransfer {
                    sequence,
                    total_bytes,
                    chunk_count,
                    next_index: 0,
                    bytes: Vec::with_capacity(total_bytes),
                    started_at: tokio::time::Instant::now(),
                });
                Ok(None)
            }
            "client/clipboard.chunk" => {
                let index = params
                    .get("index")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok())
                    .ok_or_else(|| MezError::invalid_args("Iroh clipboard chunk omitted index"))?;
                let encoded = params
                    .get("data_base64")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        MezError::invalid_args("Iroh clipboard chunk omitted encoded data")
                    })?;
                let transfer = self.transfer.as_mut().ok_or_else(|| {
                    MezError::invalid_args("Iroh clipboard chunk has no active transfer")
                })?;
                if sequence != transfer.sequence
                    || index != transfer.next_index
                    || index >= transfer.chunk_count
                {
                    self.transfer = None;
                    return Err(MezError::invalid_args(
                        "Iroh clipboard chunk ordering is invalid",
                    ));
                }
                let chunk = base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .map_err(|_| MezError::invalid_args("Iroh clipboard chunk is not base64"))?;
                if chunk.len() > IROH_CLIENT_CLIPBOARD_MAX_CHUNK_BYTES
                    || transfer.bytes.len().saturating_add(chunk.len()) > transfer.total_bytes
                {
                    self.transfer = None;
                    return Err(MezError::invalid_args(
                        "Iroh clipboard chunk exceeds declared bounds",
                    ));
                }
                transfer.bytes.extend_from_slice(&chunk);
                transfer.next_index += 1;
                Ok(None)
            }
            "client/clipboard.commit" => {
                let transfer = self.transfer.take().ok_or_else(|| {
                    MezError::invalid_args("Iroh clipboard commit has no active transfer")
                })?;
                if sequence != transfer.sequence
                    || transfer.next_index != transfer.chunk_count
                    || transfer.bytes.len() != transfer.total_bytes
                {
                    return Err(MezError::invalid_args(
                        "Iroh clipboard commit does not match the declared transfer",
                    ));
                }
                let content = String::from_utf8(transfer.bytes).map_err(|_| {
                    MezError::invalid_args("Iroh clipboard transfer is not valid UTF-8")
                })?;
                self.last_sequence = sequence;
                Ok(Some(content))
            }
            _ => Err(MezError::invalid_args(
                "unsupported Iroh clipboard effect method",
            )),
        }
    }

    /// Discards partial content after a caller-observed malformed transfer,
    /// preserving the completed-sequence watermark and connection ownership.
    pub(crate) fn discard_partial(&mut self) {
        self.transfer = None;
    }

    /// Discards a partial transfer after the bounded completion deadline.
    pub(crate) fn discard_expired(&mut self) {
        const TRANSFER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
        if self
            .transfer
            .as_ref()
            .is_some_and(|transfer| transfer.started_at.elapsed() >= TRANSFER_TIMEOUT)
        {
            self.transfer = None;
        }
    }

    /// Returns the deadline for the current partial transfer, when present.
    pub(crate) fn expiration_deadline(&self) -> Option<tokio::time::Instant> {
        const TRANSFER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
        self.transfer
            .as_ref()
            .map(|transfer| transfer.started_at + TRANSFER_TIMEOUT)
    }
}

#[cfg(test)]
mod tests;
