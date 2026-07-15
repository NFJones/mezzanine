//! Host terminal input framing independent of product routing policy.
//!
//! This module owns bracketed-paste delimiter recognition, bounded buffering
//! across reads, and stale malformed-frame recovery. It intentionally does not
//! interpret ordinary bytes as mux bindings, mouse events, or product actions;
//! callers route `HostInputSegment::Ordinary` through their own policy while
//! forwarding `HostInputSegment::BracketedPaste` opaquely to the active pane.

use std::time::{Duration, Instant};

/// Maximum retained bytes for one incomplete host bracketed-paste frame.
pub const HOST_BRACKETED_PASTE_MAX_BUFFER_BYTES: usize = 1024 * 1024;

/// Maximum age of an incomplete host bracketed-paste frame.
pub const HOST_BRACKETED_PASTE_STALE_AFTER: Duration = Duration::from_millis(500);

const HOST_BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const HOST_BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

/// One framed host-input segment ready for product routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostInputSegment {
    /// Bytes outside a bracketed-paste frame.
    Ordinary(Vec<u8>),
    /// A complete or forcibly released paste frame that must remain opaque.
    BracketedPaste(Vec<u8>),
}

/// Stateful bounded decoder for host bracketed-paste frames.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostBracketedPasteDecoder {
    active: bool,
    buffer: Vec<u8>,
    started_at: Option<Instant>,
}

impl HostBracketedPasteDecoder {
    /// Restores decoder state carried by a product client loop.
    pub fn from_parts(active: bool, buffer: Vec<u8>, started_at: Option<Instant>) -> Self {
        Self {
            active,
            buffer,
            started_at,
        }
    }

    /// Returns whether an incomplete paste frame is retained.
    pub const fn active(&self) -> bool {
        self.active
    }

    /// Returns bytes retained for an incomplete paste frame.
    pub fn buffer(&self) -> &[u8] {
        &self.buffer
    }

    /// Returns when the retained paste frame began.
    pub const fn started_at(&self) -> Option<Instant> {
        self.started_at
    }

    /// Decodes one host read using `now` for deterministic stale-frame policy.
    pub fn decode_at(&mut self, input: &[u8], now: Instant) -> Vec<HostInputSegment> {
        let mut segments = Vec::new();
        self.decode_into(input, now, &mut segments);
        segments
    }

    /// Decodes input into caller-owned output while carrying frame state.
    fn decode_into(&mut self, input: &[u8], now: Instant, segments: &mut Vec<HostInputSegment>) {
        if self.active
            && self.started_at.is_some_and(|started| {
                now.saturating_duration_since(started) >= HOST_BRACKETED_PASTE_STALE_AFTER
            })
        {
            self.release_buffer(segments);
        }

        if self.active {
            self.buffer.extend_from_slice(input);
            if self.buffer.len() > HOST_BRACKETED_PASTE_MAX_BUFFER_BYTES {
                self.release_buffer(segments);
                return;
            }
            let Some(end_start) = find_bytes(&self.buffer, HOST_BRACKETED_PASTE_END) else {
                return;
            };
            let consumed = end_start.saturating_add(HOST_BRACKETED_PASTE_END.len());
            let suffix = self.buffer[consumed..].to_vec();
            let frame = self.buffer[..consumed].to_vec();
            self.reset();
            segments.push(HostInputSegment::BracketedPaste(frame));
            if !suffix.is_empty() {
                self.decode_into(&suffix, now, segments);
            }
            return;
        }

        let Some(start) = find_bytes(input, HOST_BRACKETED_PASTE_START) else {
            if !input.is_empty() {
                segments.push(HostInputSegment::Ordinary(input.to_vec()));
            }
            return;
        };
        if start > 0 {
            segments.push(HostInputSegment::Ordinary(input[..start].to_vec()));
        }
        let framed = &input[start..];
        if let Some(end_start) = find_bytes(framed, HOST_BRACKETED_PASTE_END) {
            let consumed = end_start.saturating_add(HOST_BRACKETED_PASTE_END.len());
            segments.push(HostInputSegment::BracketedPaste(
                framed[..consumed].to_vec(),
            ));
            self.decode_into(&framed[consumed..], now, segments);
            return;
        }

        self.active = true;
        self.started_at = Some(now);
        self.buffer.extend_from_slice(framed);
        if self.buffer.len() > HOST_BRACKETED_PASTE_MAX_BUFFER_BYTES {
            self.release_buffer(segments);
        }
    }

    /// Releases a malformed retained frame as opaque pane input.
    fn release_buffer(&mut self, segments: &mut Vec<HostInputSegment>) {
        let buffer = std::mem::take(&mut self.buffer);
        self.active = false;
        self.started_at = None;
        if !buffer.is_empty() {
            segments.push(HostInputSegment::BracketedPaste(buffer));
        }
    }

    /// Clears retained frame state after completion.
    fn reset(&mut self) {
        self.active = false;
        self.buffer.clear();
        self.started_at = None;
    }
}

/// Finds the first complete byte sequence within one host read.
fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies split paste frames remain buffered until complete and preserve
    /// ordinary suffix input as a separately routable segment.
    #[test]
    fn host_bracketed_paste_decoder_assembles_split_frames() {
        let now = Instant::now();
        let mut decoder = HostBracketedPasteDecoder::default();

        assert!(decoder.decode_at(b"\x1b[200~body", now).is_empty());
        assert!(decoder.active());
        assert_eq!(
            decoder.decode_at(b" tail\x1b[201~next", now),
            vec![
                HostInputSegment::BracketedPaste(b"\x1b[200~body tail\x1b[201~".to_vec()),
                HostInputSegment::Ordinary(b"next".to_vec()),
            ]
        );
        assert!(!decoder.active());
    }

    /// Verifies an oversized malformed paste frame is released opaquely and
    /// leaves the decoder ready for ordinary input.
    #[test]
    fn host_bracketed_paste_decoder_bounds_incomplete_frames() {
        let now = Instant::now();
        let mut input = b"\x1b[200~".to_vec();
        input.extend(vec![b'x'; HOST_BRACKETED_PASTE_MAX_BUFFER_BYTES]);
        let mut decoder = HostBracketedPasteDecoder::default();

        assert_eq!(
            decoder.decode_at(&input, now),
            vec![HostInputSegment::BracketedPaste(input)]
        );
        assert!(!decoder.active());
        assert!(decoder.buffer().is_empty());
    }

    /// Verifies stale malformed frames are released before current ordinary
    /// input so later keystrokes cannot remain trapped in paste state.
    #[test]
    fn host_bracketed_paste_decoder_releases_stale_frames() {
        let now = Instant::now();
        let started = now.checked_sub(Duration::from_secs(1)).unwrap();
        let mut decoder = HostBracketedPasteDecoder::from_parts(
            true,
            b"\x1b[200~unterminated".to_vec(),
            Some(started),
        );

        assert_eq!(
            decoder.decode_at(b"next", now),
            vec![
                HostInputSegment::BracketedPaste(b"\x1b[200~unterminated".to_vec()),
                HostInputSegment::Ordinary(b"next".to_vec()),
            ]
        );
        assert!(!decoder.active());
    }
}
