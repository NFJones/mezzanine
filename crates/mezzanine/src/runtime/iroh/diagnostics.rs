//! Privacy-safe aggregate projections of the existing Iroh diagnostics registry.
//!
//! Counters remain owned by the connection/listener lifecycle. This projection
//! retains no peer identifiers, addresses, or payloads and grants no authority.

use super::{Ordering, RuntimeIrohDiagnostics};

/// Copyable status projection that contains no endpoint or peer identifiers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RuntimeIrohDiagnosticsSnapshot {
    pub(crate) listener_active: bool,
    pub(crate) active_connections: usize,
    pub(crate) connections_accepted: u64,
    pub(crate) connections_rejected: u64,
    pub(crate) setup_successes: u64,
    pub(crate) setup_failures: u64,
    pub(crate) setup_latency_total_millis: u64,
    pub(crate) setup_latency_max_millis: u64,
    pub(crate) connections_completed: u64,
    pub(crate) connections_failed: u64,
    pub(crate) direct_connections: u64,
    pub(crate) relay_connections: u64,
    pub(crate) custom_connections: u64,
    pub(crate) unknown_connections: u64,
    pub(crate) shutdown_aborts: u64,
    last_path: u8,
}

impl RuntimeIrohDiagnosticsSnapshot {
    /// Returns the mean setup latency, or zero before any setup attempt.
    pub(crate) fn average_setup_latency_millis(self) -> u64 {
        let attempts = self.setup_successes.saturating_add(self.setup_failures);
        self.setup_latency_total_millis
            .checked_div(attempts)
            .unwrap_or(0)
    }

    /// Returns the selected path class without exposing network addresses.
    pub(crate) const fn last_path_name(self) -> &'static str {
        match self.last_path {
            1 => "direct",
            2 => "relay",
            3 => "custom",
            _ => "unknown",
        }
    }
}

impl RuntimeIrohDiagnostics {
    /// Samples aggregate counters without retaining transport-private data.
    pub(crate) fn snapshot(&self) -> RuntimeIrohDiagnosticsSnapshot {
        RuntimeIrohDiagnosticsSnapshot {
            listener_active: self.inner.listener_active.load(Ordering::Relaxed),
            active_connections: self.inner.active_connections.load(Ordering::Relaxed),
            connections_accepted: self.inner.connections_accepted.load(Ordering::Relaxed),
            connections_rejected: self.inner.connections_rejected.load(Ordering::Relaxed),
            setup_successes: self.inner.setup_successes.load(Ordering::Relaxed),
            setup_failures: self.inner.setup_failures.load(Ordering::Relaxed),
            setup_latency_total_millis: self
                .inner
                .setup_latency_total_millis
                .load(Ordering::Relaxed),
            setup_latency_max_millis: self.inner.setup_latency_max_millis.load(Ordering::Relaxed),
            connections_completed: self.inner.connections_completed.load(Ordering::Relaxed),
            connections_failed: self.inner.connections_failed.load(Ordering::Relaxed),
            direct_connections: self.inner.direct_connections.load(Ordering::Relaxed),
            relay_connections: self.inner.relay_connections.load(Ordering::Relaxed),
            custom_connections: self.inner.custom_connections.load(Ordering::Relaxed),
            unknown_connections: self.inner.unknown_connections.load(Ordering::Relaxed),
            shutdown_aborts: self.inner.shutdown_aborts.load(Ordering::Relaxed),
            last_path: self.inner.last_path.load(Ordering::Relaxed),
        }
    }
}
