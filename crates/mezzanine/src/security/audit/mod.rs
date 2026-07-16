//! Structured security audit records.
//!
//! This module provides the append-only JSON Lines foundation used by
//! permission, agent, MCP, hook, and authentication paths. It intentionally
//! keeps execution-specific policy outside the writer; callers classify actions
//! and pass already structured records.

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
/// Exposes the record module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod record;
/// Exposes the redaction module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod redaction;
/// Exposes the retention module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod retention;
/// Exposes the time module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod time;
/// Exposes the types module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod types;

pub use types::{
    AuditActor, AuditConfig, AuditDeferredWrite, AuditLog, AuditRecord, AuditRetentionPolicy,
};

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
