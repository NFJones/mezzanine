//! Privacy-safe selected-path status projection and deterministic classification.
//!
//! Samples contain measurements and a path class, never addresses, relay URLs,
//! credentials, or payload bytes. Classification is advisory, not authority.

use super::{Instant, RuntimeIrohCompressionCodec};

/// Privacy-safe selected-path measurements for one initialized Iroh client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RuntimeIrohConnectionQualitySnapshot {
    pub(crate) connected_millis: u64,
    pub(crate) sampled_at: Instant,
    pub(crate) rtt_micros: u64,
    pub(crate) average_rtt_micros: u64,
    pub(crate) jitter_micros: u64,
    pub(crate) tx_bytes: u64,
    pub(crate) rx_bytes: u64,
    pub(crate) tx_bytes_per_second: u64,
    pub(crate) rx_bytes_per_second: u64,
    pub(crate) lost_packets: u64,
    pub(crate) congestion_events: u64,
    pub(crate) cwnd_bytes: u64,
    pub(crate) mtu: u16,
    pub(crate) compression_codec: RuntimeIrohCompressionCodec,
    pub(crate) compression_wire_bytes: u64,
    pub(crate) compression_decoded_bytes: u64,
    pub(crate) compression_compressed_frames: u64,
    pub(crate) compression_identity_frames: u64,
    pub(crate) render_triggers_coalesced: u64,
    pub(crate) render_updates_suppressed: u64,
    pub(crate) render_snapshot_fallbacks: u64,
    pub(crate) render_ready_depth_max: u64,
    pub(crate) render_write_wait_micros: u64,
    pub(crate) render_write_wait_max_micros: u64,
    pub(crate) render_snapshot_frames: u64,
    pub(crate) render_delta_frames: u64,
    pub(crate) render_changed_rows: u64,
    pub(crate) render_selected_wire_bytes: u64,
    pub(crate) render_selected_decoded_bytes: u64,
    pub(crate) render_snapshot_candidate_bytes: u64,
    pub(super) path: u8,
}

impl RuntimeIrohConnectionQualitySnapshot {
    /// Returns the selected path class without exposing an address or relay URL.
    pub(crate) const fn path_name(self) -> &'static str {
        match self.path {
            1 => "direct",
            2 => "relay",
            3 => "custom",
            _ => "unknown",
        }
    }

    /// Returns the elapsed time since the transport sample was collected.
    pub(crate) fn sample_age(self) -> std::time::Duration {
        self.sampled_at.elapsed()
    }

    /// Builds a deterministic snapshot for focused command-rendering tests.
    #[cfg(test)]
    pub(crate) fn test_fixture(path: &str) -> Self {
        Self {
            connected_millis: 12_000,
            sampled_at: Instant::now(),
            rtt_micros: 42_000,
            average_rtt_micros: 45_000,
            jitter_micros: 6_000,
            tx_bytes: 524_288,
            rx_bytes: 8_388_608,
            tx_bytes_per_second: 1_126,
            rx_bytes_per_second: 3_277,
            lost_packets: 0,
            congestion_events: 0,
            cwnd_bytes: 65_536,
            mtu: 1_200,
            compression_codec: RuntimeIrohCompressionCodec::Zstd,
            compression_wire_bytes: 512,
            compression_decoded_bytes: 1_024,
            compression_compressed_frames: 2,
            compression_identity_frames: 1,
            render_triggers_coalesced: 4,
            render_updates_suppressed: 1,
            render_snapshot_fallbacks: 1,
            render_ready_depth_max: 5,
            render_write_wait_micros: 250,
            render_write_wait_max_micros: 250,
            render_snapshot_frames: 1,
            render_delta_frames: 1,
            render_changed_rows: 25,
            render_selected_wire_bytes: 176,
            render_selected_decoded_bytes: 608,
            render_snapshot_candidate_bytes: 1_024,
            path: match path {
                "direct" => 1,
                "relay" => 2,
                "custom" => 3,
                _ => 0,
            },
        }
    }
}

/// Classifies one privacy-safe Iroh transport sample for diagnostics and UI.
pub(crate) fn classify_runtime_iroh_connection_quality(
    rtt_micros: u64,
    jitter_micros: u64,
    lost_packets: u64,
    congestion_events: u64,
    sample_age: std::time::Duration,
) -> crate::host::terminal::TerminalIrohStatusQuality {
    use crate::host::terminal::TerminalIrohStatusQuality;
    if sample_age > std::time::Duration::from_secs(5) {
        TerminalIrohStatusQuality::Unknown
    } else if rtt_micros >= 500_000 || lost_packets >= 4 || congestion_events >= 4 {
        TerminalIrohStatusQuality::Poor
    } else if rtt_micros >= 200_000
        || jitter_micros >= 75_000
        || lost_packets > 0
        || congestion_events > 0
    {
        TerminalIrohStatusQuality::Degraded
    } else {
        TerminalIrohStatusQuality::Good
    }
}
