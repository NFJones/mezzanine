//! Runtime and session fixtures for tests.

use std::path::PathBuf;

use crate::host::shell::{ResolvedShell, ShellSource};
use crate::host::terminal::HostClipboard;
use crate::runtime::RuntimeSessionService;
use mez_core::ids::ClientId;
use mez_mux::layout::Size;
use mez_mux::session::Session;

/// Builds sessions with the fallback POSIX shell used by most tests.
#[derive(Debug, Clone)]
pub(crate) struct SessionFixture {
    size: Size,
}

impl SessionFixture {
    /// Creates a default 80x24 session fixture.
    pub(crate) fn new() -> Self {
        Self {
            size: Size::new(80, 24).unwrap(),
        }
    }

    /// Overrides the session size.
    pub(crate) fn size(mut self, size: Size) -> Self {
        self.size = size;
        self
    }

    /// Builds the session.
    pub(crate) fn build(self) -> Session {
        Session::new_default(
            ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
            self.size,
        )
    }

    /// Builds the session and attaches a primary client.
    pub(crate) fn build_with_primary(self) -> (Session, ClientId) {
        let mut session = self.build();
        let primary = session.attach_primary("primary", true).unwrap();
        (session, primary)
    }
}

impl Default for SessionFixture {
    fn default() -> Self {
        Self::new()
    }
}

/// Builds runtime services with stable defaults for unit tests.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeServiceFixture {
    size: Size,
    control_socket: PathBuf,
    created_at_unix_seconds: u64,
    max_events: usize,
    max_payload_bytes: usize,
}

impl RuntimeServiceFixture {
    /// Creates a runtime service fixture with the existing test defaults.
    pub(crate) fn new() -> Self {
        Self {
            size: Size::new(80, 24).unwrap(),
            control_socket: PathBuf::from("/tmp/mez-1000/default.sock"),
            created_at_unix_seconds: 100,
            max_events: 10,
            max_payload_bytes: 1024,
        }
    }

    /// Overrides the terminal size for the initial session.
    pub(crate) fn size(mut self, size: Size) -> Self {
        self.size = size;
        self
    }

    /// Overrides the control socket path.
    pub(crate) fn control_socket(mut self, path: impl Into<PathBuf>) -> Self {
        self.control_socket = path.into();
        self
    }

    /// Builds the runtime service.
    pub(crate) fn build(self) -> RuntimeSessionService {
        let mut service = RuntimeSessionService::with_event_log(
            SessionFixture::new().size(self.size).build(),
            self.control_socket,
            self.created_at_unix_seconds,
            self.max_events,
            self.max_payload_bytes,
        )
        .unwrap();
        service.set_host_clipboard_for_tests(HostClipboard::disabled());
        service
    }
}

impl Default for RuntimeServiceFixture {
    fn default() -> Self {
        Self::new()
    }
}
