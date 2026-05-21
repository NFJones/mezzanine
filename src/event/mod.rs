//! Session event log and observer-safe replay policy.
//!
//! Runtime clients and agents need a retained stream for state changes without
//! leaking data across observer approval boundaries. This module models event
//! visibility independently from the socket transport so the eventual control
//! endpoint can deliver the same events over Unix or TCP sockets. Events use a
//! deterministic monotonic `event:<id>` timestamp until a runtime clock is
//! wired into the live daemon.

/// Exposes the json module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod json;
/// Exposes the log module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod log;
/// Exposes the types module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod types;
/// Exposes the visibility module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod visibility;

pub use json::{encode_event_notification, event_method_name, event_type_name};
pub use types::{
    EventAudience, EventKind, EventLog, EventVisibility, MezzanineEvent, VisibleEvent,
};

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
