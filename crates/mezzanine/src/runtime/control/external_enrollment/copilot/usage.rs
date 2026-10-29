//! Finite content-free source-gap vocabulary beside terminal observations.
//! Gaps confer no source authority, health, presentation or origin inference.

/// Fixed categories exclude arbitrary vendor error text from diagnostics.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(in crate::runtime::control::external_enrollment) enum UsageGap {
    MissingIdentity,
    CountersUnavailable,
    InvalidCounters,
    QueuePressure,
    DeliveryUnavailable,
    ObserverLost,
}

impl UsageGap {
    /// Sticky finite membership; no event-sized collection is retained.
    pub(super) fn bit(self) -> u8 {
        1 << self as u8
    }
}
