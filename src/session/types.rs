//! Session data types and in-memory state containers.
//!
//! These types describe clients, observer requests, session state, and the core
//! session container. Behavior is implemented in focused sibling modules.

use mez_core::{ClientId, IdFactory, ObserverRequestId, SessionId, WindowGroupId, WindowId};
use mez_mux::layout::{LayoutNode, LayoutPolicy, PaneGeometry, Size, Window};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Shell launch metadata retained by the session domain.
///
/// Shell discovery and validation remain product responsibilities. The session
/// stores only the resolved launch path and descriptive snapshot metadata that
/// process adapters need after construction or restoration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionShell {
    path: PathBuf,
    source: String,
    used_fallback: bool,
}

impl SessionShell {
    /// Creates neutral shell launch metadata from product-resolved values.
    pub fn new(path: PathBuf, source: impl Into<String>, used_fallback: bool) -> Self {
        Self {
            path,
            source: source.into(),
            used_fallback,
        }
    }

    /// Returns the resolved executable path used for pane processes.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the stable descriptive source name used by snapshots.
    pub fn source_name(&self) -> &str {
        &self.source
    }

    /// Returns whether shell resolution selected the fallback executable.
    pub fn used_fallback(&self) -> bool {
        self.used_fallback
    }
}

/// Dependency-neutral session data decoded by a product persistence adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRestoreInput {
    /// Stable session identity to restore.
    pub session_id: SessionId,
    /// User-visible session name.
    pub name: String,
    /// Restored lifecycle state.
    pub state: RestoredSessionState,
    /// Authoritative attached-terminal dimensions.
    pub authoritative_size: Size,
    /// Stable active-window identity, when recorded.
    pub active_window_id: Option<WindowId>,
    /// Restored window topology in index order.
    pub windows: Vec<RestoredWindow>,
    /// Restored window-group topology in index order.
    pub window_groups: Vec<RestoredWindowGroup>,
}

/// Lifecycle state accepted by session restoration without persistence coupling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoredSessionState {
    /// Running session state.
    Running,
    /// Detached session state.
    Detached,
    /// Empty session state.
    Empty,
    /// Stopping session state.
    Stopping,
    /// Failed session state.
    Failed,
}

/// One decoded window accepted by session restoration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoredWindow {
    /// Stable window identity.
    pub id: WindowId,
    /// Contiguous window index.
    pub index: usize,
    /// User-visible window name.
    pub name: String,
    /// Whether the window was active.
    pub active: bool,
    /// Window dimensions.
    pub size: Size,
    /// Decoded layout policy.
    pub layout_policy: LayoutPolicy,
    /// Decoded layout tree, when recorded.
    pub layout_root: Option<LayoutNode>,
    /// Restored panes in index order.
    pub panes: Vec<RestoredPane>,
}

/// One decoded pane accepted by session restoration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoredPane {
    /// Stable pane identity.
    pub id: mez_core::PaneId,
    /// Contiguous pane index.
    pub index: usize,
    /// User-visible pane title.
    pub title: String,
    /// Whether the pane was active.
    pub active: bool,
    /// Pane dimensions.
    pub size: Size,
    /// Stored pane rectangle, when available.
    pub geometry: Option<PaneGeometry>,
    /// Last observed working directory.
    pub current_working_directory: Option<String>,
    /// Last observed agent readiness state.
    pub readiness_state: String,
    /// Whether the alternate screen was active.
    pub alternate_screen_active: bool,
}

/// One decoded window group accepted by session restoration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoredWindowGroup {
    /// Stable group identity.
    pub id: WindowGroupId,
    /// Contiguous group index.
    pub index: usize,
    /// User-visible group name.
    pub name: String,
    /// Ordered member windows.
    pub window_ids: Vec<WindowId>,
    /// Active member window, when recorded.
    pub active_window_id: Option<WindowId>,
    /// Previously active member window, when recorded.
    pub last_active_window_id: Option<WindowId>,
    /// Whether the group was active.
    pub active: bool,
}

/// Carries Client Role state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientRole {
    /// Represents the Primary case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Primary,
    /// Represents the Pending Observer case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    PendingObserver,
    /// Represents the Observer case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Observer,
    /// Represents the Agent case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Agent,
    /// Represents the Automation case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Automation,
}

/// Carries Client State state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientState {
    /// Represents the Attached case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Attached,
    /// Represents the Pending case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Pending,
    /// Represents the Detached case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Detached,
    /// Represents the Revoked case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Revoked,
    /// Represents the Failed case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Failed,
}

/// Terminal descriptor supplied by a client when it attaches to a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientTerminalDescriptor {
    /// Number of terminal columns reported by the client.
    pub columns: u16,
    /// Number of terminal rows reported by the client.
    pub rows: u16,
    /// Terminal profile name reported by the client.
    pub term: String,
    /// Optional terminal feature names reported by the client.
    pub features: Vec<String>,
}

/// Carries Client state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    /// Stores the id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub id: ClientId,
    /// Stores the name value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub name: String,
    /// Stores the role value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub role: ClientRole,
    /// Stores the state value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub state: ClientState,
    /// Stores the interactive value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub interactive: bool,
    /// Stores the terminal value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub terminal: Option<ClientTerminalDescriptor>,
    /// Stores the attached at unix seconds value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub attached_at_unix_seconds: Option<u64>,
    /// Stores the last seen at unix seconds value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub last_seen_at_unix_seconds: Option<u64>,
}

/// Carries Observer Decision State state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObserverDecisionState {
    /// Represents the Pending case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Pending,
    /// Represents the Approved case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Approved,
    /// Represents the Rejected case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Rejected,
    /// Represents the Revoked case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Revoked,
}

/// Carries Observer Request state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObserverRequest {
    /// Stores the id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub id: ObserverRequestId,
    /// Stores the client id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub client_id: ClientId,
    /// Stores the state value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub state: ObserverDecisionState,
    /// Stores the descriptor name value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub descriptor_name: String,
    /// Stores the descriptor interactive value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub descriptor_interactive: bool,
    /// Stores the descriptor terminal value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub descriptor_terminal: Option<ClientTerminalDescriptor>,
    /// Stores the requested at unix seconds value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub requested_at_unix_seconds: Option<u64>,
    /// Stores the decided at unix seconds value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub decided_at_unix_seconds: Option<u64>,
    /// Stores the decided by client id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub decided_by_client_id: Option<String>,
    /// Stores the visible from event id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub visible_from_event_id: Option<u64>,
    /// Stores the visible from unix seconds value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub visible_from_unix_seconds: Option<u64>,
    /// Stores the reason value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub reason: Option<String>,
}

/// Carries Session State state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// Represents the Running case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Running,
    /// Represents the Detached case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Detached,
    /// Represents the Empty case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Empty,
    /// Represents the Stopping case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Stopping,
    /// Represents the Failed case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Failed,
}

/// Pane metadata retained by the session when it is known outside a live
/// runtime process manager.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneStateMetadata {
    /// Last known shell current working directory for the pane.
    pub current_working_directory: Option<String>,
    /// Last known agent-harness readiness state for the pane.
    pub readiness_state: String,
    /// Whether the pane was last observed in the alternate screen.
    pub alternate_screen_active: bool,
}

/// A user-facing group of windows inside a session.
///
/// The live runtime still owns pane processes through the session's flat window
/// list, while this grouping layer records which ordered windows are presented
/// together in the UI. Every live window must belong to exactly one group, and
/// the active session window must belong to the active group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowGroup {
    /// Stable window-group identity used by commands and control state.
    pub id: WindowGroupId,
    /// Display index within the session group bar.
    pub index: usize,
    /// User-provided or generated group name.
    pub name: String,
    /// Ordered stable window identities owned by this group.
    pub window_ids: Vec<WindowId>,
    /// Stable identity of the active window inside this group.
    pub active_window_id: Option<WindowId>,
    /// Stable identity of the previous active window inside this group.
    pub last_active_window_id: Option<WindowId>,
    /// Unix timestamp for group creation when known.
    pub created_at_unix_seconds: Option<u64>,
}

impl WindowGroup {
    /// Creates a new group around an initial landing window.
    pub fn new(
        id: WindowGroupId,
        index: usize,
        name: impl Into<String>,
        window_id: WindowId,
        created_at_unix_seconds: Option<u64>,
    ) -> Self {
        Self {
            id,
            index,
            name: name.into(),
            window_ids: vec![window_id.clone()],
            active_window_id: Some(window_id),
            last_active_window_id: None,
            created_at_unix_seconds,
        }
    }
}

/// Carries Session state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone)]
pub struct Session {
    /// Stores the ids value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) ids: IdFactory,
    /// Stores the id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub id: SessionId,
    /// Stores the name value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub name: String,
    /// Stores the state value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub state: SessionState,
    /// Stores the created at unix seconds value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub created_at_unix_seconds: u64,
    /// Stores the updated at unix seconds value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub updated_at_unix_seconds: u64,
    /// Stores the last attached at unix seconds value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub last_attached_at_unix_seconds: Option<u64>,
    /// Stores the authoritative size value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub authoritative_size: Size,
    /// Stores the shell value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub shell: SessionShell,
    /// Stores the config generation value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub config_generation: u64,
    /// Stores the windows value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) windows: Vec<Window>,
    /// Stores the window groups value for this data structure.
    ///
    /// Each group owns an ordered set of stable window ids. The flat `windows`
    /// list remains the authoritative process/layout collection, while groups
    /// provide the user-facing organization and active group state.
    pub(super) window_groups: Vec<WindowGroup>,
    /// Stores the active window group index value for this data structure.
    ///
    /// The index points into `window_groups` and must reference the group that
    /// owns the current `active_window_index` whenever windows are present.
    pub(super) active_group_index: usize,
    /// Stores the last active window group index value for this data structure.
    ///
    /// The value is used by `last-group` and is cleared when the referenced
    /// group is removed.
    pub(super) last_active_group_index: Option<usize>,
    /// Stores the active window index value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) active_window_index: usize,
    /// Stores the last active window index value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) last_active_window_index: Option<usize>,
    /// Stores window ids whose panes receive synchronized primary input.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) synchronized_window_ids: BTreeSet<String>,
    /// Stores the pane state metadata value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pane_state_metadata: BTreeMap<String, PaneStateMetadata>,
    /// Stores the clients value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) clients: Vec<Client>,
    /// Stores the observers value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) observers: Vec<ObserverRequest>,
    /// Stores the primary client id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) primary_client_id: Option<ClientId>,
    /// Stores the next event id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) next_event_id: u64,
}
