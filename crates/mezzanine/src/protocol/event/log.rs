//! Retained event log implementation.
//!
//! The log enforces payload limits, assigns monotonic ids, trims old events,
//! and delegates audience filtering to the visibility module.

use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{MezError, Result};

use super::types::{
    EventAudience, EventKind, EventLog, EventVisibility, MezzanineEvent, VisibleEvent,
};
use super::visibility::visible_event;

impl EventLog {
    /// Creates an empty event log with retention and payload limits.
    ///
    /// Returns invalid-arguments errors when either limit is zero.
    pub fn new(max_events: usize, max_payload_bytes: usize) -> Result<Self> {
        if max_events == 0 {
            return Err(MezError::invalid_args(
                "event log must retain at least one event",
            ));
        }
        if max_payload_bytes == 0 {
            return Err(MezError::invalid_args(
                "event payload limit must be greater than zero",
            ));
        }
        Ok(Self {
            max_events,
            max_payload_bytes,
            next_id: 1,
            events: VecDeque::new(),
        })
    }

    /// Appends an event and returns its assigned id.
    ///
    /// Returns an invalid-arguments error when the payload exceeds the
    /// configured byte limit.
    pub fn append(
        &mut self,
        kind: EventKind,
        session_id: Option<String>,
        visibility: EventVisibility,
        payload: impl Into<String>,
    ) -> Result<u64> {
        let payload = payload.into();
        if payload.len() > self.max_payload_bytes {
            return Err(MezError::invalid_args(
                "event payload exceeds configured limit",
            ));
        }

        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.events.push_back(MezzanineEvent {
            id,
            time: current_rfc3339_seconds(),
            kind,
            session_id,
            visibility,
            payload,
        });
        while self.events.len() > self.max_events {
            self.events.pop_front();
        }
        Ok(id)
    }

    /// Replays all retained events visible to an audience.
    pub fn replay_for(&self, audience: &EventAudience) -> Vec<VisibleEvent> {
        self.events
            .iter()
            .filter_map(|event| visible_event(event, audience))
            .collect()
    }

    /// Replays visible events after the provided cursor, up to `limit` events.
    pub fn replay_after_for(
        &self,
        audience: &EventAudience,
        after_event_id: u64,
        limit: usize,
    ) -> Vec<VisibleEvent> {
        if limit == 0 {
            return Vec::new();
        }
        self.events
            .iter()
            .filter(|event| event.id > after_event_id)
            .filter_map(|event| visible_event(event, audience))
            .take(limit)
            .collect()
    }

    /// Returns the latest assigned event id, or zero before the first append.
    pub fn latest_event_id(&self) -> u64 {
        self.next_id.saturating_sub(1)
    }

    /// Returns the oldest retained event id.
    pub fn first_retained_event_id(&self) -> Option<u64> {
        self.events.front().map(|event| event.id)
    }

    /// Returns the configured retained event count.
    pub fn retention_limit(&self) -> usize {
        self.max_events
    }

    /// Returns the number of retained events.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Returns true when no events are retained.
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "test-only adapter retained for focused boundary coverage"
    )]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

/// Runs the current rfc3339 seconds operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn current_rfc3339_seconds() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    unix_seconds_to_rfc3339(seconds)
}

/// Runs the unix seconds to rfc3339 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn unix_seconds_to_rfc3339(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let seconds_of_day = seconds % 86_400;
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Runs the civil from days operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn civil_from_days(days_since_unix_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_unix_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    (year, month, day)
}
