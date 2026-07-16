//! Product persistence adapters over canonical lower-domain records.
//!
//! The modules below own SQLite and filesystem layouts, compatibility data,
//! private-file posture, repositories, and cross-crate snapshot persistence.

pub(crate) mod issues;
pub(crate) mod memory;
pub(crate) mod registry;
pub(crate) mod snapshot;
pub(crate) mod transcript;
