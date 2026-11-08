//! Bounded, content-free observations of the existing model operation. These
//! atomics never admit work, acknowledge delivery, or authorize publication.
//! Actor inspection must not acquire the model session's worker-held mutex.

use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};

#[derive(Debug, Default)]
pub(super) struct Progress {
    phase: AtomicU8,
    calls: AtomicU64,
    depth: AtomicUsize,
}

/// A best-effort concurrent display sample; it grants no execution authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub(crate) phase: &'static str,
    pub(crate) model_calls: u64,
    pub(crate) serial_depth: usize,
}

impl Progress {
    pub(super) fn snapshot(&self) -> Snapshot {
        Snapshot {
            phase: match self.phase.load(Ordering::Relaxed) {
                1 => "packing",
                2 => "inference",
                3 => "validating",
                4 => "queued",
                5 => "publishing",
                _ => "claimed",
            },
            model_calls: self.calls.load(Ordering::Relaxed),
            serial_depth: self.depth.load(Ordering::Relaxed),
        }
    }

    pub(super) fn preparing(&self) {
        self.phase(1);
    }

    /// Counts actual calls into the provider port, including repairs/retries.
    /// Invocation is not evidence of HTTP transmission or provider acceptance.
    pub(super) fn invoking(&self, depth: usize) {
        self.depth.fetch_max(depth, Ordering::Relaxed);
        // Keep saturation with unlimited transport retry and Rust 1.95; newer
        // atomic update conveniences are not available on the minimum toolchain.
        let mut previous = self.calls.load(Ordering::Relaxed);
        while let Err(current) = self.calls.compare_exchange_weak(
            previous,
            previous.saturating_add(1),
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            previous = current;
        }
        self.phase(2);
    }

    pub(super) fn phase(&self, phase: u8) {
        self.phase.store(phase, Ordering::Relaxed);
    }
}
