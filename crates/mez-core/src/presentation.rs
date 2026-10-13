//! Immutable presentation occurrence provenance, independent of chronology.
//!
//! Producers capture a local wall-clock value once at a semantic boundary.
//! Consumers retain it without sampling clocks, deriving remote generation
//! times, or using it for identity, deduplication, admission or ordering. Unix
//! milliseconds are signed; unknown/unrepresentable values remain explicit.
//! Legacy record time is coarse persistence evidence, not occurrence proof.

use serde::{Deserialize, Serialize};

/// Evidence behind a local presentation timestamp, never execution evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PresentationOccurrenceBasis {
    /// No trustworthy representable timestamp is available.
    #[default]
    Unknown,
    /// Original legacy persistence time with seconds precision, not occurrence.
    LegacyRecord,
    /// First locally observed component; not remote model generation time.
    LocalObservation,
    /// Local accepted input/result/message; not remote delivery/execution time.
    LocalAcceptance,
}

/// Immutable wall-clock evidence carried separately from authored source bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresentationOccurrence {
    /// Signed Unix milliseconds, or unknown/unrepresentable clock value.
    pub unix_millis: Option<i64>,
    /// The local boundary that supplied the timestamp.
    pub basis: PresentationOccurrenceBasis,
}

impl PresentationOccurrence {
    /// Constructs explicit evidence; missing clock values cannot claim a basis.
    pub fn new(unix_millis: Option<i64>, basis: PresentationOccurrenceBasis) -> Self {
        Self {
            unix_millis: if basis == PresentationOccurrenceBasis::Unknown {
                None
            } else {
                unix_millis
            },
            basis: if unix_millis.is_some() {
                basis
            } else {
                PresentationOccurrenceBasis::Unknown
            },
        }
    }

    /// Preserves legacy seconds only when millisecond conversion is representable.
    pub fn legacy_record(seconds: u64) -> Self {
        Self::new(
            seconds
                .checked_mul(1000)
                .and_then(|value| i64::try_from(value).ok()),
            PresentationOccurrenceBasis::LegacyRecord,
        )
    }

    /// Checks coherent known-versus-unknown provenance without range guessing.
    pub fn is_valid(self) -> bool {
        self.unix_millis.is_some() != (self.basis == PresentationOccurrenceBasis::Unknown)
    }
}

/// Typed annotation on one physical row of an existing presentation timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresentationEntry {
    /// Stable producer-owned identity, independent of time or repeated source.
    pub id: String,
    /// Original occurrence evidence retained identically on all component rows.
    pub occurrence: PresentationOccurrence,
    /// Original first row; pruning never promotes a continuation to a boundary.
    pub first_row: bool,
}

impl PresentationEntry {
    /// Rejects unbounded/control-bearing identity and incoherent provenance.
    pub fn is_valid(&self) -> bool {
        !self.id.is_empty()
            && self.id.len() <= 1024
            && !self.id.chars().any(char::is_control)
            && self.occurrence.is_valid()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Numeric edges retain signed milliseconds without date-format guessing;
    /// overflow of coarse legacy seconds and unknown basis stay explicit.
    #[test]
    fn occurrence_range_and_basis_are_explicit() {
        for millis in [i64::MIN, -1, 0, i64::MAX] {
            let stamp = PresentationOccurrence::new(
                Some(millis),
                PresentationOccurrenceBasis::LocalObservation,
            );
            assert_eq!(stamp.unix_millis, Some(millis));
            assert!(stamp.is_valid());
        }
        assert_eq!(
            PresentationOccurrence::new(Some(1), PresentationOccurrenceBasis::Unknown),
            PresentationOccurrence::default()
        );
        assert_eq!(
            PresentationOccurrence::legacy_record(u64::MAX),
            PresentationOccurrence::default()
        );
        assert_eq!(
            PresentationOccurrence::legacy_record(42).unix_millis,
            Some(42000)
        );
        assert_eq!(
            PresentationOccurrence::legacy_record(42).basis,
            PresentationOccurrenceBasis::LegacyRecord
        );
    }
}
