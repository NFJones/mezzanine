//! Connection-local selected-path health evidence for terminal adapters.
//!
//! Preserves the existing attach sampler and quality classification: one retained
//! connection owns its previous selected-path counters, jitter and refresh timer.
//! Path changes restart deltas; absent measurements yield unknown rather than
//! guessed good health. This owner samples no sibling connection and grants no
//! routing, input or presentation authority. Callers own scheduling and lifetime.

use super::TerminalIrohStatusQuality;

/// Previous selected-path counters retained for connection-local deltas.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AttachIrohPathSample {
    path_id: String,
    rtt_micros: u64,
    jitter_micros: u64,
    lost_packets: u64,
    congestion_events: u64,
}

/// Samples and classifies one retained Iroh connection with bounded state.
#[derive(Debug)]
pub(crate) struct AttachIrohHealthTracker {
    previous: Option<AttachIrohPathSample>,
    quality: TerminalIrohStatusQuality,
    deadline: tokio::time::Instant,
}

impl Default for AttachIrohHealthTracker {
    fn default() -> Self {
        Self {
            previous: None,
            quality: TerminalIrohStatusQuality::Unknown,
            deadline: tokio::time::Instant::now(),
        }
    }
}

impl AttachIrohHealthTracker {
    /// Established client-local sampling cadence, not a transport idle deadline.
    const REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

    /// Returns the next client-local sampling deadline.
    pub(crate) fn deadline(&self) -> tokio::time::Instant {
        self.deadline
    }

    /// Returns the last classified quality, initially unknown.
    pub(crate) fn quality(&self) -> TerminalIrohStatusQuality {
        self.quality
    }

    /// Samples the selected path and reports whether visible quality changed.
    /// No selected path leaves quality unknown and reschedules the next sample.
    pub(crate) fn sample(&mut self, connection: &iroh::endpoint::Connection) -> bool {
        let previous_quality = self.quality;
        let paths = connection.paths();
        let Some(path) = paths.iter().find(|path| path.is_selected()) else {
            self.quality = TerminalIrohStatusQuality::Unknown;
            self.deadline = tokio::time::Instant::now() + Self::REFRESH_INTERVAL;
            return self.quality != previous_quality;
        };
        let stats = path.stats();
        let path_id = format!("{:?}", path.id());
        let rtt_micros = u64::try_from(stats.rtt.as_micros()).unwrap_or(u64::MAX);
        let same_path = self
            .previous
            .as_ref()
            .is_some_and(|previous| previous.path_id == path_id);
        let jitter_micros = self
            .previous
            .as_ref()
            .filter(|_| same_path)
            .map(|previous| {
                previous
                    .jitter_micros
                    .saturating_mul(3)
                    .saturating_add(rtt_micros.abs_diff(previous.rtt_micros))
                    / 4
            })
            .unwrap_or(0);
        let delta = |current: u64, previous: fn(&AttachIrohPathSample) -> u64| {
            self.previous
                .as_ref()
                .filter(|_| same_path)
                .map(|sample| current.saturating_sub(previous(sample)))
                .unwrap_or(0)
        };
        let lost_packets = delta(stats.lost_packets, |sample| sample.lost_packets);
        let congestion_events = delta(stats.congestion_events, |sample| sample.congestion_events);
        self.quality = crate::runtime::classify_runtime_iroh_connection_quality(
            rtt_micros,
            jitter_micros,
            lost_packets,
            congestion_events,
            std::time::Duration::ZERO,
        );
        self.previous = Some(AttachIrohPathSample {
            path_id,
            rtt_micros,
            jitter_micros,
            lost_packets: stats.lost_packets,
            congestion_events: stats.congestion_events,
        });
        self.deadline = tokio::time::Instant::now() + Self::REFRESH_INTERVAL;
        self.quality != previous_quality
    }
}

#[cfg(test)]
mod tests;
