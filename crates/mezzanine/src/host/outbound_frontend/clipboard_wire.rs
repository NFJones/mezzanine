//! Bounded clipboard transfer framing for an exact retained local session.
//!
//! Clipboard content can exceed a snapshot frame, so encoding is lazy and each
//! chunk remains independently bounded. The receiver exposes UTF-8 only after
//! exact ordered completion. Every frame binds the local handle, whole validated
//! session and transfer occurrence. Content is never Debug-formatted or included
//! in errors. Callers own capability negotiation, total deadlines, framing,
//! cancellation and host clipboard policy; this module performs no I/O or replay.

use super::client::SessionSummary;
use super::*;
use base64::Engine as _;

const MAX_BYTES: usize = 8 * 1024 * 1024;
const CHUNK_BYTES: usize = 256 * 1024;
const FRAME_BYTES: usize = 1024 * 1024;

/// Closed transfer records. No arbitrary metadata or target selector is accepted.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum Record {
    #[serde(rename = "clipboard_begin")]
    Begin {
        handle: FrontendHandle,
        session: SessionSummary,
        transfer: u64,
        total_bytes: usize,
        chunks: usize,
    },
    #[serde(rename = "clipboard_chunk")]
    Chunk {
        handle: FrontendHandle,
        session: SessionSummary,
        transfer: u64,
        index: usize,
        data_base64: String,
    },
    #[serde(rename = "clipboard_commit")]
    Commit {
        handle: FrontendHandle,
        session: SessionSummary,
        transfer: u64,
    },
}

/// Lazy producer retaining borrowed content rather than all encoded frames.
pub(super) struct ClipboardFrames<'a> {
    handle: &'a FrontendHandle,
    session: &'a SessionSummary,
    transfer: u64,
    content: &'a str,
    next: usize,
    chunks: usize,
}

impl<'a> ClipboardFrames<'a> {
    /// Validates the finite source budget before producing any transfer bytes.
    /// A transfer ID is a positive occurrence, not independent clipboard authority.
    pub(super) fn new(
        handle: &'a FrontendHandle,
        session: &'a SessionSummary,
        transfer: u64,
        content: &'a str,
    ) -> Result<Self> {
        if transfer == 0 || content.len() > MAX_BYTES {
            return Err(MezError::invalid_args(
                "outbound clipboard transfer exceeds budget",
            ));
        }
        Ok(Self {
            handle,
            session,
            transfer,
            content,
            next: 0,
            chunks: content.len().div_ceil(CHUNK_BYTES).max(1),
        })
    }
}

impl Iterator for ClipboardFrames<'_> {
    type Item = Result<ProtocolFrame>;

    /// Produces one bounded record without collecting encoded copies of content.
    fn next(&mut self) -> Option<Self::Item> {
        let record = if self.next == 0 {
            Record::Begin {
                handle: self.handle.clone(),
                session: self.session.clone(),
                transfer: self.transfer,
                total_bytes: self.content.len(),
                chunks: self.chunks,
            }
        } else if self.next <= self.chunks {
            let index = self.next - 1;
            let start = index * CHUNK_BYTES;
            let end = (start + CHUNK_BYTES).min(self.content.len());
            Record::Chunk {
                handle: self.handle.clone(),
                session: self.session.clone(),
                transfer: self.transfer,
                index,
                data_base64: base64::engine::general_purpose::STANDARD
                    .encode(&self.content.as_bytes()[start..end]),
            }
        } else if self.next == self.chunks + 1 {
            Record::Commit {
                handle: self.handle.clone(),
                session: self.session.clone(),
                transfer: self.transfer,
            }
        } else {
            return None;
        };
        self.next += 1;
        Some(
            serde_json::to_string(&record)
                .map_err(|_| MezError::invalid_state("outbound clipboard encoding unavailable"))
                .and_then(|body| {
                    if body.len() > FRAME_BYTES {
                        return Err(MezError::invalid_state(
                            "outbound clipboard frame exceeds budget",
                        ));
                    }
                    Ok(ProtocolFrame::new(CONTENT_TYPE, body))
                }),
        )
    }
}

/// One receiver's finite in-progress content, never exposed before exact commit.
struct Pending {
    transfer: u64,
    total_bytes: usize,
    chunks: usize,
    next: usize,
    bytes: Vec<u8>,
}

/// Exact-owner decoder. A malformed transfer permanently poisons this exchange;
/// callers must retire its consumed stream rather than attempt resynchronization.
pub(super) struct ClipboardReceiver {
    handle: FrontendHandle,
    session: SessionSummary,
    pending: Option<Pending>,
    last_transfer: u64,
    failed: bool,
}

impl ClipboardReceiver {
    /// Pins inert ownership from an independently validated session settlement.
    pub(super) fn new(handle: FrontendHandle, session: SessionSummary) -> Self {
        Self {
            handle,
            session,
            pending: None,
            last_transfer: 0,
            failed: false,
        }
    }

    /// Applies a single frame. Failure clears partial content and cannot expose
    /// payload in diagnostics; success returns content only on exact UTF-8 commit.
    pub(super) fn apply(&mut self, frame: &ProtocolFrame) -> Result<Option<String>> {
        if self.failed {
            return Err(MezError::invalid_state(
                "outbound clipboard receiver unavailable",
            ));
        }
        let result = self.apply_inner(frame);
        if result.is_err() {
            self.pending = None;
            self.failed = true;
        }
        result
    }

    /// Checks closed ownership, byte/count budgets and ordering before assembly.
    fn apply_inner(&mut self, frame: &ProtocolFrame) -> Result<Option<String>> {
        if frame.content_type != CONTENT_TYPE || frame.body.len() > FRAME_BYTES {
            return Err(MezError::invalid_state("outbound clipboard frame invalid"));
        }
        let record: Record = serde_json::from_str(&frame.body)
            .map_err(|_| MezError::invalid_state("outbound clipboard record invalid"))?;
        let (handle, session, transfer) = match &record {
            Record::Begin {
                handle,
                session,
                transfer,
                ..
            }
            | Record::Chunk {
                handle,
                session,
                transfer,
                ..
            }
            | Record::Commit {
                handle,
                session,
                transfer,
            } => (handle, session, *transfer),
        };
        if *handle != self.handle || *session != self.session || transfer == 0 {
            return Err(MezError::conflict(
                "outbound clipboard transfer owner changed",
            ));
        }
        match record {
            Record::Begin {
                total_bytes,
                chunks,
                ..
            } => {
                if self.pending.is_some()
                    || transfer <= self.last_transfer
                    || total_bytes > MAX_BYTES
                    || chunks != total_bytes.div_ceil(CHUNK_BYTES).max(1)
                {
                    return Err(MezError::invalid_state("outbound clipboard begin invalid"));
                }
                self.pending = Some(Pending {
                    transfer,
                    total_bytes,
                    chunks,
                    next: 0,
                    bytes: Vec::with_capacity(total_bytes),
                });
                Ok(None)
            }
            Record::Chunk {
                index, data_base64, ..
            } => {
                let pending = self.pending.as_mut().ok_or_else(|| {
                    MezError::invalid_state("outbound clipboard chunk has no transfer")
                })?;
                if transfer != pending.transfer
                    || index != pending.next
                    || index >= pending.chunks
                    || data_base64.len() > CHUNK_BYTES.div_ceil(3) * 4
                {
                    return Err(MezError::invalid_state(
                        "outbound clipboard chunk ordering invalid",
                    ));
                }
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data_base64)
                    .map_err(|_| {
                        MezError::invalid_state("outbound clipboard chunk encoding invalid")
                    })?;
                let remaining = pending.total_bytes - pending.bytes.len();
                if bytes.len() != remaining.min(CHUNK_BYTES) {
                    return Err(MezError::invalid_state(
                        "outbound clipboard chunk length invalid",
                    ));
                }
                pending.bytes.extend_from_slice(&bytes);
                pending.next += 1;
                Ok(None)
            }
            Record::Commit { .. } => {
                let pending = self.pending.take().ok_or_else(|| {
                    MezError::invalid_state("outbound clipboard commit has no transfer")
                })?;
                if transfer != pending.transfer
                    || pending.next != pending.chunks
                    || pending.bytes.len() != pending.total_bytes
                {
                    return Err(MezError::invalid_state(
                        "outbound clipboard commit incomplete",
                    ));
                }
                let content = String::from_utf8(pending.bytes).map_err(|_| {
                    MezError::invalid_state("outbound clipboard content is not UTF-8")
                })?;
                self.last_transfer = transfer;
                Ok(Some(content))
            }
        }
    }
}

#[cfg(test)]
mod tests;
