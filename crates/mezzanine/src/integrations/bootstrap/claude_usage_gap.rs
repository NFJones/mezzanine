//! Fixed content-free Claude request coverage diagnostics shared by adapters.
//!
//! These reasons describe missing telemetry, never expense, vendor outcomes,
//! observer freshness or source authority. The native helper owner separately
//! qualifies the exact creator and epoch before recording one bounded reason.

/// Closed diagnostic grammar; arbitrary vendor text cannot cross this boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum UsageGapReason {
    /// Original native request readiness was unavailable.
    RequestReadiness,
    /// A delegated request finished without attributable valid final counters.
    FinalizedCounters,
    /// The exact final report has no known durable acknowledgment.
    DurableAcknowledgment,
    /// Finite active/pending metadata capacity refused new telemetry admission.
    Capacity,
}

impl UsageGapReason {
    /// Returns a fixed status label without reading or formatting vendor data.
    pub(crate) fn coverage(self) -> &'static str {
        match self {
            Self::RequestReadiness => "unavailable-claude-usage-request-readiness",
            Self::FinalizedCounters => "unavailable-claude-usage-finalized-counters",
            Self::DurableAcknowledgment => "unavailable-claude-usage-durable-acknowledgment",
            Self::Capacity => "unavailable-claude-usage-capacity",
        }
    }
}
