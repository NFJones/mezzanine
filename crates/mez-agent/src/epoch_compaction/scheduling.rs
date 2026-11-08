//! Cheap deterministic schedule estimates, never runtime model discovery.

use super::*;

/// Scheduling choices share source, acceptance and result contracts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionSchedule {
    /// Favor a ready chronological carry plus not-yet-summarized source.
    Rolling,
    /// Favor concurrent source batches followed by necessary adjacent merges.
    Parallel,
}

/// Advisory comparison. Group packing, actual outputs and real input rejection
/// determine actual calls; these values are not promises or output ceilings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionScheduleEstimate {
    /// Cached selected replay cost divided by available auxiliary source capacity.
    pub initial_source_batches: usize,
    /// Expected serial model waves for a small rolling carry.
    pub rolling_depth: usize,
    /// Expected model waves with bounded parallel batches and necessary merges.
    pub parallel_depth: usize,
    /// Estimated rolling logical calls, used only as a latency/cost tie breaker.
    pub rolling_calls: usize,
    /// Estimated parallel logical calls, including necessary merge work.
    pub parallel_calls: usize,
    /// Lowest expected latency, then lowest call count; no optimization calls.
    pub selected: CompactionSchedule,
}

impl CompactionScheduleEstimate {
    pub(super) fn new(tokens: usize, limits: EpochCompactionLimits) -> Self {
        let capacity = limits.input_tokens - limits.framing_tokens;
        let initial = tokens.div_ceil(capacity).max(1);
        let carry_capacity = capacity
            .saturating_sub(limits.expected_summary_tokens)
            .max(1);
        let rolling_calls = if initial == 1 {
            1
        } else {
            1usize.saturating_add(tokens.saturating_sub(capacity).div_ceil(carry_capacity))
        };
        let fan_in = (capacity / limits.expected_summary_tokens).max(2);
        let mut remaining = initial;
        let mut parallel_calls = initial;
        let mut parallel_depth = initial.div_ceil(limits.in_flight);
        while remaining > 1 {
            remaining = remaining.div_ceil(fan_in);
            parallel_calls = parallel_calls.saturating_add(remaining);
            parallel_depth = parallel_depth.saturating_add(remaining.div_ceil(limits.in_flight));
        }
        let selected = if (parallel_depth, parallel_calls) < (rolling_calls, rolling_calls) {
            CompactionSchedule::Parallel
        } else {
            CompactionSchedule::Rolling
        };
        Self {
            initial_source_batches: initial,
            rolling_depth: rolling_calls,
            parallel_depth,
            rolling_calls,
            parallel_calls,
            selected,
        }
    }
}
