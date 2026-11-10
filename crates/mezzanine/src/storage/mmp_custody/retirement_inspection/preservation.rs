//! Quarantined bodyless records funded by one original lower reservation.
//!
//! Encoded prefixes never escape a failed/refused snapshot. The reservation
//! covers all retained buffers and bounded container overhead independently of
//! the source decoder and slot accumulator. These bytes confer no cleanup or
//! canonical authority and are not yet published to storage.

use super::*;
use mez_agent::messaging::{MessageAcceptanceMetadata, MessageSourcePreservationReservation};
use std::io::{self, Write};

pub(crate) struct PreservedSourceEvidence {
    pub sequence: u64,
    encoded: Vec<u8>,
}

impl PreservedSourceEvidence {
    /// Borrows the original encoding without detaching its reservation.
    pub(crate) fn encoded(&self) -> &[u8] {
        &self.encoded
    }
}

pub(super) struct PreservationBuffer {
    records: Vec<PreservedSourceEvidence>,
    reservation: Arc<MessageSourcePreservationReservation>,
    bytes: usize,
    pub(super) refused: bool,
}

impl PreservationBuffer {
    pub(super) fn new(reservation: Arc<MessageSourcePreservationReservation>) -> Result<Self> {
        let mut records = Vec::new();
        records
            .try_reserve_exact(MAX_CANDIDATE_SOURCES)
            .map_err(|_| unavailable())?;
        Ok(Self {
            records,
            reservation,
            bytes: 0,
            refused: false,
        })
    }

    pub(super) fn records(&self) -> &[PreservedSourceEvidence] {
        &self.records
    }

    pub(super) fn sort(&mut self) {
        self.records.sort_unstable_by_key(|record| record.sequence);
    }

    /// Uses the verified original and counted exact size. Insufficient reserved
    /// space is expected refusal, not corruption; the caller must still finish
    /// the member's strict slot/inventory validation before discarding output.
    pub(super) fn capture(
        &mut self,
        source: &MessageAcceptanceMetadata,
        source_sha256: &[u8; 32],
        original_bytes: usize,
        expected: &SourceEvidenceCommitment,
    ) -> Result<()> {
        let bytes = usize::try_from(expected.encoded_bytes).map_err(|_| unavailable())?;
        let total = self.bytes.checked_add(bytes).ok_or_else(unavailable)?;
        if self.refused || total > self.reservation.encoded_bytes_limit() {
            self.refused = true;
            return Ok(());
        }
        if self.records.len() == MAX_CANDIDATE_SOURCES {
            return Err(unavailable());
        }
        struct Bounded {
            bytes: Vec<u8>,
            limit: usize,
        }
        impl Write for Bounded {
            fn write(&mut self, input: &[u8]) -> io::Result<usize> {
                if input.len() > self.limit - self.bytes.len() {
                    return Err(io::Error::other(
                        "source preservation encoding exceeded reservation",
                    ));
                }
                self.bytes.extend_from_slice(input);
                Ok(input.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut encoded = Vec::new();
        encoded
            .try_reserve_exact(bytes)
            .map_err(|_| unavailable())?;
        // Do not silently retain excess Vec capacity outside the funded bound.
        if encoded.capacity() > bytes {
            return Err(unavailable());
        }
        let mut sink = Bounded {
            bytes: encoded,
            limit: bytes,
        };
        source
            .write_metadata_evidence(&mut sink, source_sha256, original_bytes)
            .map_err(|_| unavailable())?;
        if sink.bytes.len() != bytes
            || <[u8; 32]>::from(Sha256::digest(&sink.bytes)) != expected.sha256
        {
            return Err(unavailable());
        }
        self.records.push(PreservedSourceEvidence {
            sequence: source.sequence(),
            encoded: sink.bytes,
        });
        self.bytes = total;
        Ok(())
    }
}
