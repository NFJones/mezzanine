//! Product persistence adapters over canonical lower-domain records.
//!
//! The modules below own SQLite and filesystem layouts, compatibility data,
//! private-file posture, repositories, and cross-crate snapshot persistence.

pub(crate) mod context_documents;
pub(crate) mod issues;
#[allow(
    dead_code,
    reason = "host routing, administration, and recovery consume the completed durable lease repository in subsequent architecture phases"
)]
pub(crate) mod lease;
pub(crate) mod local_assignment;
pub(crate) mod memory;
pub(crate) mod registry;
mod row_diff;
#[allow(
    dead_code,
    reason = "shared groundwork for the session-state conversions in 3b8c6774, f6382169, 6f30066a, 5e099d41, and bddc716e; the first conversion consumes it"
)]
pub(crate) mod shared_sqlite;
pub(crate) mod snapshot;
pub(crate) mod token_usage;
pub(crate) mod transcript;
