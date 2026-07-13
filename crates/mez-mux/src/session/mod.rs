//! In-memory multiplexer session domain.
//!
//! This module owns dependency-neutral client, observer, window, group, layout
//! restoration, and resize-effect transitions. Product persistence, process
//! supervision, and snapshot decoding remain outside this crate.

mod clients;
mod lifecycle;
mod snapshot;
mod targets;
mod time;
mod types;
mod windows;

pub use types::{
    Client, ClientRole, ClientState, ClientTerminalDescriptor, ObserverDecisionState,
    ObserverRequest, RestoredPane, RestoredSessionState, RestoredWindow, RestoredWindowGroup,
    Session, SessionRestoreInput, SessionShell, SessionState, WindowGroup,
};
pub use windows::{
    BreakPaneTransition, JoinPaneTransition, KillGroupTransition, KillWindowTransition,
    PaneResizeEffect, PaneResizeTransition, RemovePaneTransition,
};
