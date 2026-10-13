//! Stable contracts shared by independent Mezzanine subsystems.
//!
//! This crate is the workspace dependency root and owns canonical identifiers
//! plus their validation invariants. Product policy, I/O, persistence, runtime
//! orchestration, and a general-purpose utility layer do not belong here.

/// Stable opaque identifiers shared across Mezzanine subsystems.
pub mod ids;

/// Execution-inert provenance shared by presentation rows and recovery receipts.
pub mod presentation;

pub use ids::{AgentId, ClientId, IdFactory, PaneId, SessionId, StableId, WindowGroupId, WindowId};
