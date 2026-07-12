//! In-memory session lifecycle model.
//!
//! The live daemon and persistence layer are not implemented yet. This module
//! models default session creation, primary-client exclusivity, observer
//! requests, detach behavior, and window ownership.

/// Exposes the clients module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod clients;
/// Exposes the lifecycle module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod lifecycle;
/// Exposes the snapshot module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod snapshot;
/// Exposes the targets module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod targets;
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
/// Exposes the windows module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod windows;

pub use types::{
    Client, ClientRole, ClientState, ClientTerminalDescriptor, ObserverDecisionState,
    ObserverRequest, Session, SessionShell, SessionState, WindowGroup,
};
pub use windows::{
    BreakPaneTransition, JoinPaneTransition, KillGroupTransition, KillWindowTransition,
    PaneResizeEffect, PaneResizeTransition, RemovePaneTransition,
};

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
