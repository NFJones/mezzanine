//! Session data types and in-memory state containers.
//!
//! These types describe clients, observer requests, session state, and the core
//! session container. Behavior is implemented in focused sibling modules.

use crate::layout::{LayoutNode, LayoutPolicy, PaneGeometry, Size, Window};
use mez_core::{ClientId, IdFactory, PaneId, SessionId, WindowGroupId, WindowId};
use std::collections::{BTreeMap, BTreeSet, HashMap};
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
    classification: String,
    version_probe: Option<String>,
}

impl SessionShell {
    /// Creates neutral shell launch metadata from product-resolved values.
    pub fn new(path: PathBuf, source: impl Into<String>, used_fallback: bool) -> Self {
        Self {
            path,
            source: source.into(),
            used_fallback,
            classification: String::new(),
            version_probe: None,
        }
    }

    /// Attaches the product-resolved shell classification and bounded version
    /// probe evidence used to select bootstrap and transaction syntax.
    pub fn with_execution_identity(
        mut self,
        classification: impl Into<String>,
        version_probe: Option<String>,
    ) -> Self {
        self.classification = classification.into();
        self.version_probe = version_probe;
        self
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

    /// Returns the product-resolved shell classification, when retained.
    pub fn classification(&self) -> &str {
        &self.classification
    }

    /// Returns bounded runtime version evidence captured during resolution.
    pub fn version_probe(&self) -> Option<&str> {
        self.version_probe.as_deref()
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
    /// Client-independent landing focus restored for fresh primary clients.
    pub landing_navigation: LandingNavigationState,
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

/// Stable-ID focus state for one caller-local navigation level.
///
/// History is stored oldest-to-newest and is bounded by navigation mutation
/// helpers. Stable identities keep cursors independent of topology indexes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusCursor<T> {
    /// Currently selected identity.
    pub active: Option<T>,
    /// Previously selected identity.
    pub last: Option<T>,
    /// Bounded deduplicated MRU history, oldest to newest.
    pub history: Vec<T>,
}

impl<T> Default for FocusCursor<T> {
    fn default() -> Self {
        Self {
            active: None,
            last: None,
            history: Vec::new(),
        }
    }
}

/// Caller-local group, window, pane, and zoom navigation for one primary.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClientNavigationState {
    /// Session group focus.
    pub groups: FocusCursor<WindowGroupId>,
    /// Window focus retained independently for each group.
    pub windows_by_group: HashMap<WindowGroupId, FocusCursor<WindowId>>,
    /// Pane focus retained independently for each window.
    pub panes_by_window: HashMap<WindowId, FocusCursor<PaneId>>,
    /// Caller-local zoomed pane for each window.
    pub zoomed_panes_by_window: HashMap<WindowId, PaneId>,
    /// Monotonic revision incremented only when this navigation changes.
    pub revision: u64,
}

/// Client-independent landing focus used when no primary view is available.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LandingNavigationState {
    /// Landing group identity.
    pub active_group_id: Option<WindowGroupId>,
    /// Landing window identity.
    pub active_window_id: Option<WindowId>,
    /// Landing pane identity.
    pub active_pane_id: Option<PaneId>,
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
    /// Caller-local navigation for attached primary clients.
    ///
    /// Observer, agent, and automation records do not own independent primary
    /// navigation and therefore retain `None`.
    pub navigation: Option<ClientNavigationState>,
}

/// Maximum number of interactive primary clients attached concurrently.
pub const MAX_ATTACHED_PRIMARY_CLIENTS: usize = 16;

/// Maximum number of unreferenced detached client summaries retained in memory.
pub const MAX_RETAINED_DETACHED_CLIENTS: usize = 64;

/// Session lifecycle edge produced by an exact primary membership change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimaryLifecycleEdge {
    /// Membership changed without entering or leaving the attached state.
    None,
    /// The first primary attached to a session with no attached primaries.
    Attached,
    /// The final attached primary detached from the session.
    Detached,
}

/// Exact primary membership transition returned by attach and detach owners.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrimaryMembershipTransition {
    /// Exact client whose membership changed.
    pub client_id: ClientId,
    /// Number of attached primaries before the transition.
    pub primary_count_before: usize,
    /// Number of attached primaries after the transition.
    pub primary_count_after: usize,
    /// Layout owner before the transition.
    pub layout_owner_before: Option<ClientId>,
    /// Layout owner after the transition.
    pub layout_owner_after: Option<ClientId>,
    /// Canonical terminal size before the transition.
    pub authoritative_size_before: Size,
    /// Canonical terminal size after owner attachment, transfer, or election.
    pub authoritative_size_after: Size,
    /// Pane sizes produced when the canonical terminal geometry changed.
    pub resize_effects: Vec<super::windows::PaneResizeEffect>,
    /// Observer clients revoked because this primary was their exact view source.
    pub revoked_observer_client_ids: Vec<ClientId>,
    /// Lifecycle edge produced by the membership count change.
    pub lifecycle_edge: PrimaryLifecycleEdge,
}

/// Read-only attachment metadata for an observer client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObserverAttachment {
    /// Attached observer client.
    pub client_id: ClientId,
    /// Exact attached primary whose navigation and live pane content this observer follows.
    pub view_source_client_id: ClientId,
    /// Earliest retained event visible to this observer.
    pub visible_from_event_id: u64,
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
    /// Bounded oldest-to-newest stable window identities previously focused
    /// in this group. The history is transient and is never persisted.
    pub(super) window_focus_history: Vec<WindowId>,
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
            window_focus_history: Vec::new(),
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
    /// Bounded oldest-to-newest stable group identities previously focused in
    /// this session. The history is transient and is never persisted.
    pub(super) group_focus_history: Vec<WindowGroupId>,
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
    /// Read-only observer attachments keyed by client identity.
    pub(super) observer_attachments: Vec<ObserverAttachment>,
    /// Client-independent landing navigation used to seed primary views.
    pub(super) landing_navigation: LandingNavigationState,
    /// Attached primary whose terminal size owns canonical layout geometry.
    pub(super) layout_owner_client_id: Option<ClientId>,
    /// Monotonic revision of canonical layout geometry and ownership.
    pub(super) layout_revision: u64,
    /// Stores the next event id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) next_event_id: u64,
}
