//! In-memory multiplexer session domain.
//!
//! This module owns dependency-neutral client, observer, window, group, layout
//! restoration, and resize-effect transitions. Product persistence, process
//! supervision, and snapshot decoding remain outside this crate.

mod clients;
mod lifecycle;
mod reconciliation;
mod snapshot;
mod targets;
#[cfg(test)]
mod tests;
mod time;
mod types;
mod windows;

pub use reconciliation::TopologyReconciliation;
pub use types::{
    Client, ClientNavigationState, ClientRole, ClientState, ClientTerminalDescriptor, FocusCursor,
    LandingNavigationState, MAX_ATTACHED_PRIMARY_CLIENTS, MAX_RETAINED_DETACHED_CLIENTS,
    ObserverDecisionState, ObserverRequest, PrimaryLifecycleEdge, PrimaryMembershipTransition,
    RestoredPane, RestoredSessionState, RestoredWindow, RestoredWindowGroup, Session,
    SessionRestoreInput, SessionShell, SessionState, WindowGroup,
};
pub use windows::{
    BreakPaneTransition, JoinPaneTransition, KillGroupTransition, KillWindowTransition,
    PaneResizeEffect, PaneResizeTransition, RemovePaneTransition,
};
