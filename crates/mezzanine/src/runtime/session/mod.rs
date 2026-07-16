//! Runtime mux-session and lifecycle metadata ownership.
//!
//! This component is the application boundary around the mux domain session.
//! It owns the canonical [`Session`] together with the timestamps and socket
//! identity that describe that session to runtime clients. Mux operations
//! remain available through `Deref`; application-only metadata is private and
//! can be changed only through the typed accessors below.

use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};

use mez_mux::session::Session;

use super::RuntimeLifecycleState;

/// Owns the mux session and its application lifecycle metadata.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeSessionComponent {
    session: Session,
    window_created_at_unix_seconds: BTreeMap<String, u64>,
    lifecycle_state: RuntimeLifecycleState,
    socket_path: PathBuf,
    created_at_unix_seconds: u64,
    last_attach_at_unix_seconds: Option<u64>,
}

impl RuntimeSessionComponent {
    /// Builds session ownership from the validated runtime constructor state.
    pub(crate) fn new(
        session: Session,
        window_created_at_unix_seconds: BTreeMap<String, u64>,
        lifecycle_state: RuntimeLifecycleState,
        socket_path: PathBuf,
        created_at_unix_seconds: u64,
    ) -> Self {
        Self {
            session,
            window_created_at_unix_seconds,
            lifecycle_state,
            socket_path,
            created_at_unix_seconds,
            last_attach_at_unix_seconds: None,
        }
    }

    /// Returns window creation timestamps keyed by stable window id.
    pub(crate) fn window_created_at_unix_seconds(&self) -> &BTreeMap<String, u64> {
        &self.window_created_at_unix_seconds
    }

    /// Returns window creation timestamps for transactional mutation.
    pub(crate) fn window_created_at_unix_seconds_mut(&mut self) -> &mut BTreeMap<String, u64> {
        &mut self.window_created_at_unix_seconds
    }

    /// Replaces all window creation timestamps during rollback or restoration.
    pub(crate) fn replace_window_created_at_unix_seconds(&mut self, values: BTreeMap<String, u64>) {
        self.window_created_at_unix_seconds = values;
    }

    /// Returns the application lifecycle state.
    pub(crate) fn lifecycle_state(&self) -> RuntimeLifecycleState {
        self.lifecycle_state
    }

    /// Replaces the application lifecycle state after a validated transition.
    pub(crate) fn set_lifecycle_state(&mut self, state: RuntimeLifecycleState) {
        self.lifecycle_state = state;
    }

    /// Returns the canonical control socket path.
    pub(crate) fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Returns the session creation timestamp.
    pub(crate) fn created_at_unix_seconds(&self) -> u64 {
        self.created_at_unix_seconds
    }

    /// Returns the most recent primary-client attachment timestamp.
    pub(crate) fn last_attach_at_unix_seconds(&self) -> Option<u64> {
        self.last_attach_at_unix_seconds
    }

    /// Records the most recent primary-client attachment timestamp.
    pub(crate) fn set_last_attach_at_unix_seconds(&mut self, value: Option<u64>) {
        self.last_attach_at_unix_seconds = value;
    }
}

impl Deref for RuntimeSessionComponent {
    type Target = Session;

    fn deref(&self) -> &Self::Target {
        &self.session
    }
}

impl DerefMut for RuntimeSessionComponent {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.session
    }
}
