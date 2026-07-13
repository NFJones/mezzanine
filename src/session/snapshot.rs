//! Session restoration from snapshot payloads.
//!
//! Restore validates snapshot structure, reconstructs windows and panes, and
//! advances the id factory beyond restored identifiers.

use crate::error::{MezError, Result};
use crate::ids::{IdFactory, PaneId, SessionId, StableId, WindowGroupId, WindowId};
use crate::layout::{
    LayoutPolicy, Pane, PaneGeometry, PaneTitleSource, RestoredWindowLayout, Size, Window,
};
use crate::snapshot::{
    SessionSnapshotPayload, SnapshotSessionState, WindowGroupSnapshotPayload, WindowSnapshotPayload,
};
use std::collections::{BTreeMap, BTreeSet};

use super::time::current_unix_seconds;
use super::types::{
    PaneStateMetadata, RestoredSessionState, Session, SessionRestoreInput, SessionShell,
    SessionState, WindowGroup,
};

/// Carries freshly allocated layout state rebuilt from a snapshot payload.
type FreshSnapshotLayout = (
    Vec<Window>,
    Vec<WindowGroup>,
    usize,
    usize,
    Option<usize>,
    BTreeMap<String, PaneStateMetadata>,
);

impl Session {
    /// Rebuilds a session from dependency-neutral data decoded by a product adapter.
    pub fn from_restore_input(
        shell: impl Into<SessionShell>,
        input: SessionRestoreInput,
    ) -> Result<Self> {
        let shell = shell.into();
        let restored_at = current_unix_seconds();
        let mut restored_ids = vec![input.session_id.clone()];
        let mut active_window_index = None;
        let mut pane_state_metadata = BTreeMap::new();
        let mut windows = Vec::with_capacity(input.windows.len());

        for (expected_index, restored_window) in input.windows.into_iter().enumerate() {
            if restored_window.index != expected_index {
                return Err(MezError::invalid_args(
                    "restored windows must be stored in contiguous index order",
                ));
            }
            if restored_window.active && active_window_index.replace(expected_index).is_some() {
                return Err(MezError::invalid_args(
                    "restored session contains multiple active windows",
                ));
            }
            restored_ids.push(restored_window.id.clone());

            let mut panes = Vec::with_capacity(restored_window.panes.len());
            let mut pane_geometries = Vec::with_capacity(restored_window.panes.len());
            let mut has_complete_geometry = true;
            for (expected_pane_index, restored_pane) in
                restored_window.panes.into_iter().enumerate()
            {
                if restored_pane.index != expected_pane_index {
                    return Err(MezError::invalid_args(
                        "restored panes must be stored in contiguous index order",
                    ));
                }
                restored_ids.push(restored_pane.id.clone());
                pane_state_metadata.insert(
                    restored_pane.id.to_string(),
                    PaneStateMetadata {
                        current_working_directory: restored_pane.current_working_directory,
                        readiness_state: restored_pane.readiness_state,
                        alternate_screen_active: restored_pane.alternate_screen_active,
                    },
                );
                match restored_pane.geometry {
                    Some(geometry) => pane_geometries.push(geometry),
                    None => has_complete_geometry = false,
                }
                panes.push(Pane {
                    id: restored_pane.id,
                    index: restored_pane.index,
                    title: restored_pane.title,
                    title_source: PaneTitleSource::Explicit,
                    size: restored_pane.size,
                    active: restored_pane.active,
                    live: false,
                });
            }

            let mut window = Window::from_restored_parts_with_layout(
                restored_window.id,
                restored_window.index,
                restored_window.name,
                restored_window.size,
                panes,
                RestoredWindowLayout {
                    pane_geometries: has_complete_geometry.then_some(pane_geometries),
                    layout_root: restored_window.layout_root,
                    layout_policy: restored_window.layout_policy,
                },
            )?;
            window.created_at_unix_seconds = Some(restored_at);
            windows.push(window);
        }

        if windows.is_empty() {
            return Err(MezError::invalid_args(
                "restored session must contain at least one window",
            ));
        }
        let active_window_index = active_window_index.ok_or_else(|| {
            MezError::invalid_args("restored session must contain exactly one active window")
        })?;
        if let Some(active_window_id) = input.active_window_id.as_ref()
            && windows[active_window_index].id != *active_window_id
        {
            return Err(MezError::invalid_args(
                "restored active_window_id does not match the active window",
            ));
        }

        let mut active_group_index = None;
        let mut window_groups = Vec::with_capacity(input.window_groups.len());
        for (expected_index, restored_group) in input.window_groups.into_iter().enumerate() {
            if restored_group.index != expected_index {
                return Err(MezError::invalid_args(
                    "restored window groups must be stored in contiguous index order",
                ));
            }
            if restored_group.active && active_group_index.replace(expected_index).is_some() {
                return Err(MezError::invalid_args(
                    "restored session contains multiple active window groups",
                ));
            }
            let first_window = restored_group.window_ids.first().cloned().ok_or_else(|| {
                MezError::invalid_args("restored window group must contain at least one window")
            })?;
            let mut group = WindowGroup::new(
                restored_group.id.clone(),
                restored_group.index,
                restored_group.name,
                first_window,
                Some(restored_at),
            );
            group.window_ids = restored_group.window_ids;
            group.active_window_id = restored_group.active_window_id;
            group.last_active_window_id = restored_group.last_active_window_id;
            restored_ids.push(restored_group.id);
            window_groups.push(group);
        }
        let active_group_index = active_group_index.ok_or_else(|| {
            MezError::invalid_args("restored session must contain exactly one active window group")
        })?;

        Ok(Self {
            ids: IdFactory::after_existing_ids(restored_ids.iter()),
            id: input.session_id,
            name: input.name,
            state: match input.state {
                RestoredSessionState::Running => SessionState::Running,
                RestoredSessionState::Detached => SessionState::Detached,
                RestoredSessionState::Empty => SessionState::Empty,
                RestoredSessionState::Stopping => SessionState::Stopping,
                RestoredSessionState::Failed => SessionState::Failed,
            },
            created_at_unix_seconds: restored_at,
            updated_at_unix_seconds: restored_at,
            last_attached_at_unix_seconds: None,
            authoritative_size: input.authoritative_size,
            shell,
            config_generation: 0,
            windows,
            window_groups,
            active_group_index,
            last_active_group_index: None,
            active_window_index,
            last_active_window_index: None,
            synchronized_window_ids: BTreeSet::new(),
            pane_state_metadata,
            clients: Vec::new(),
            observers: Vec::new(),
            primary_client_id: None,
            next_event_id: 1,
        })
    }

    /// Replaces user-visible layout state from dependency-neutral restored data.
    ///
    /// Fresh live identifiers are allocated so loading a saved layout behaves
    /// like recreating its groups, windows, and panes in the current session.
    pub fn replace_layout_from_restore_input(&mut self, input: SessionRestoreInput) -> Result<()> {
        let authoritative_size = input.authoritative_size;
        let (
            windows,
            window_groups,
            active_window_index,
            active_group_index,
            last_active_group_index,
            pane_state_metadata,
        ) = self.layout_from_restore_input_with_fresh_ids(input)?;

        self.authoritative_size = authoritative_size;
        self.windows = windows;
        self.window_groups = window_groups;
        self.active_window_index = active_window_index;
        self.last_active_window_index = None;
        self.active_group_index = active_group_index;
        self.last_active_group_index = last_active_group_index;
        self.pane_state_metadata = pane_state_metadata;
        self.record_event();
        Ok(())
    }

    /// Rebuilds neutral restored layout metadata with live-session identifiers.
    fn layout_from_restore_input_with_fresh_ids(
        &mut self,
        input: SessionRestoreInput,
    ) -> Result<FreshSnapshotLayout> {
        let mut restored_to_live_window_ids = BTreeMap::new();
        let mut active_window_index = None;
        let mut pane_state_metadata = BTreeMap::new();
        let mut windows = Vec::with_capacity(input.windows.len());
        let restored_at = current_unix_seconds();

        for (expected_index, restored_window) in input.windows.into_iter().enumerate() {
            if restored_window.index != expected_index {
                return Err(MezError::invalid_args(
                    "restored windows must be stored in contiguous index order",
                ));
            }
            if restored_window.active && active_window_index.replace(expected_index).is_some() {
                return Err(MezError::invalid_args(
                    "restored session contains multiple active windows",
                ));
            }
            let window_id = self.ids.window();
            restored_to_live_window_ids.insert(restored_window.id.to_string(), window_id.clone());

            let mut panes = Vec::with_capacity(restored_window.panes.len());
            let mut pane_geometries = Vec::with_capacity(restored_window.panes.len());
            let mut has_complete_geometry = true;
            for (expected_pane_index, restored_pane) in
                restored_window.panes.into_iter().enumerate()
            {
                if restored_pane.index != expected_pane_index {
                    return Err(MezError::invalid_args(
                        "restored panes must be stored in contiguous index order",
                    ));
                }
                let pane_id = self.ids.pane();
                pane_state_metadata.insert(
                    pane_id.to_string(),
                    PaneStateMetadata {
                        current_working_directory: restored_pane.current_working_directory,
                        readiness_state: restored_pane.readiness_state,
                        alternate_screen_active: restored_pane.alternate_screen_active,
                    },
                );
                match restored_pane.geometry {
                    Some(geometry) => pane_geometries.push(geometry),
                    None => has_complete_geometry = false,
                }
                panes.push(Pane {
                    id: pane_id,
                    index: restored_pane.index,
                    title: restored_pane.title,
                    title_source: PaneTitleSource::Explicit,
                    size: restored_pane.size,
                    active: restored_pane.active,
                    live: false,
                });
            }

            let mut window = Window::from_restored_parts_with_layout(
                window_id,
                restored_window.index,
                restored_window.name,
                restored_window.size,
                panes,
                RestoredWindowLayout {
                    pane_geometries: has_complete_geometry.then_some(pane_geometries),
                    layout_root: restored_window.layout_root,
                    layout_policy: restored_window.layout_policy,
                },
            )?;
            window.created_at_unix_seconds = Some(restored_at);
            windows.push(window);
        }

        if windows.is_empty() {
            return Err(MezError::invalid_args(
                "restored session must contain at least one window",
            ));
        }
        let active_window_index = active_window_index.ok_or_else(|| {
            MezError::invalid_args("restored session must contain exactly one active window")
        })?;

        let mut active_group_index = None;
        let mut window_groups = Vec::with_capacity(input.window_groups.len());
        for (expected_index, restored_group) in input.window_groups.into_iter().enumerate() {
            if restored_group.index != expected_index {
                return Err(MezError::invalid_args(
                    "restored window groups must be stored in contiguous index order",
                ));
            }
            if restored_group.active && active_group_index.replace(expected_index).is_some() {
                return Err(MezError::invalid_args(
                    "restored session contains multiple active window groups",
                ));
            }
            let window_ids = restored_group
                .window_ids
                .iter()
                .map(|id| {
                    restored_to_live_window_ids
                        .get(id.as_str())
                        .cloned()
                        .ok_or_else(|| {
                            MezError::invalid_args(
                                "restored window group references an unknown window",
                            )
                        })
                })
                .collect::<Result<Vec<_>>>()?;
            let first_window = window_ids.first().cloned().ok_or_else(|| {
                MezError::invalid_args("restored window group must contain at least one window")
            })?;
            let mut group = WindowGroup::new(
                self.ids.window_group(),
                restored_group.index,
                restored_group.name,
                first_window,
                Some(restored_at),
            );
            group.window_ids = window_ids;
            group.active_window_id = restored_group
                .active_window_id
                .as_ref()
                .map(|id| {
                    restored_to_live_window_ids
                        .get(id.as_str())
                        .cloned()
                        .ok_or_else(|| {
                            MezError::invalid_args(
                                "restored window group references an unknown active window",
                            )
                        })
                })
                .transpose()?;
            group.last_active_window_id = restored_group
                .last_active_window_id
                .as_ref()
                .map(|id| {
                    restored_to_live_window_ids
                        .get(id.as_str())
                        .cloned()
                        .ok_or_else(|| {
                            MezError::invalid_args(
                                "restored window group references an unknown last active window",
                            )
                        })
                })
                .transpose()?;
            window_groups.push(group);
        }
        let active_group_index = active_group_index.ok_or_else(|| {
            MezError::invalid_args("restored session must contain exactly one active window group")
        })?;

        Ok((
            windows,
            window_groups,
            active_window_index,
            active_group_index,
            None,
            pane_state_metadata,
        ))
    }

    /// Runs the from snapshot payload operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn from_snapshot_payload(
        shell: impl Into<SessionShell>,
        payload: &SessionSnapshotPayload,
    ) -> Result<Self> {
        let shell = shell.into();
        payload.validate()?;
        let session_id = SessionId::parse('$', payload.session_id.clone()).ok_or_else(|| {
            MezError::invalid_args("snapshot payload contains an invalid session id")
        })?;
        let authoritative_size =
            Size::new(payload.authoritative_columns, payload.authoritative_rows)?;
        let mut restored_ids = vec![session_id.clone()];
        let mut active_window_index = None;
        let mut pane_state_metadata = BTreeMap::new();
        let mut windows = Vec::with_capacity(payload.windows.len());
        let restored_at = current_unix_seconds();

        for (expected_index, window_payload) in payload.windows.iter().enumerate() {
            if window_payload.index != expected_index {
                return Err(MezError::invalid_args(
                    "snapshot windows must be stored in contiguous index order",
                ));
            }
            let window_id =
                WindowId::parse('@', window_payload.window_id.clone()).ok_or_else(|| {
                    MezError::invalid_args("snapshot payload contains an invalid window id")
                })?;
            let window_is_active = Some(window_payload.window_id.as_str())
                == payload.active_window_id.as_deref()
                || window_payload.active;
            if window_is_active && active_window_index.replace(expected_index).is_some() {
                return Err(MezError::invalid_args(
                    "snapshot payload contains multiple active windows",
                ));
            }
            restored_ids.push(window_id.clone());

            let mut panes = Vec::with_capacity(window_payload.panes.len());
            for (expected_pane_index, pane_payload) in window_payload.panes.iter().enumerate() {
                if pane_payload.index != expected_pane_index {
                    return Err(MezError::invalid_args(
                        "snapshot panes must be stored in contiguous index order",
                    ));
                }
                let pane_id =
                    PaneId::parse('%', pane_payload.pane_id.clone()).ok_or_else(|| {
                        MezError::invalid_args("snapshot payload contains an invalid pane id")
                    })?;
                restored_ids.push(pane_id.clone());
                pane_state_metadata.insert(
                    pane_id.to_string(),
                    PaneStateMetadata {
                        current_working_directory: pane_payload.current_working_directory.clone(),
                        readiness_state: pane_payload.readiness_state.clone(),
                        alternate_screen_active: pane_payload.alternate_screen_active,
                    },
                );
                panes.push(Pane {
                    id: pane_id,
                    index: pane_payload.index,
                    title: pane_payload.title.clone(),
                    title_source: PaneTitleSource::Explicit,
                    size: Size::new(pane_payload.columns, pane_payload.rows)?,
                    active: pane_payload.active,
                    live: false,
                });
            }

            let pane_geometries = restored_pane_geometries(window_payload);
            let layout_root = restored_layout_root(window_payload)?;
            let layout_policy =
                LayoutPolicy::from_name(&window_payload.layout_policy).ok_or_else(|| {
                    MezError::invalid_args("snapshot window layout policy is invalid")
                })?;
            let mut window = Window::from_restored_parts_with_layout(
                window_id,
                window_payload.index,
                window_payload.name.clone(),
                Size::new(window_payload.columns, window_payload.rows)?,
                panes,
                RestoredWindowLayout {
                    pane_geometries,
                    layout_root,
                    layout_policy,
                },
            )?;
            window.created_at_unix_seconds = Some(restored_at);
            windows.push(window);
        }

        if windows.is_empty() {
            return Err(MezError::invalid_args(
                "snapshot payload must contain at least one window",
            ));
        }
        let active_window_index = active_window_index.ok_or_else(|| {
            MezError::invalid_args("snapshot payload must contain exactly one active window")
        })?;
        if let Some(active_window_id) = payload.active_window_id.as_deref()
            && windows[active_window_index].id.as_str() != active_window_id
        {
            return Err(MezError::invalid_args(
                "snapshot active_window_id does not match the active window",
            ));
        }
        let (window_groups, active_group_index, last_active_group_index) =
            restored_window_groups(payload, &windows, active_window_index, &mut restored_ids)?;

        Ok(Self {
            ids: IdFactory::after_existing_ids(restored_ids.iter()),
            id: session_id,
            name: payload.name.clone(),
            state: session_state_from_snapshot(payload.state),
            created_at_unix_seconds: restored_at,
            updated_at_unix_seconds: restored_at,
            last_attached_at_unix_seconds: None,
            authoritative_size,
            shell,
            config_generation: 0,
            windows,
            window_groups,
            active_group_index,
            last_active_group_index,
            active_window_index,
            last_active_window_index: None,
            synchronized_window_ids: BTreeSet::new(),
            pane_state_metadata,
            clients: Vec::new(),
            observers: Vec::new(),
            primary_client_id: None,
            next_event_id: 1,
        })
    }
}

/// Runs the session state from snapshot operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn session_state_from_snapshot(state: SnapshotSessionState) -> SessionState {
    match state {
        SnapshotSessionState::Running => SessionState::Running,
        SnapshotSessionState::Detached => SessionState::Detached,
        SnapshotSessionState::Empty => SessionState::Empty,
        SnapshotSessionState::Stopping => SessionState::Stopping,
        SnapshotSessionState::Failed => SessionState::Failed,
    }
}

/// Restores saved window groups, falling back to one group for legacy payloads.
fn restored_window_groups(
    payload: &SessionSnapshotPayload,
    windows: &[Window],
    active_window_index: usize,
    restored_ids: &mut Vec<StableId>,
) -> Result<(Vec<WindowGroup>, usize, Option<usize>)> {
    if payload.window_groups.is_empty() {
        let group_id = WindowGroupId::parse('g', "g1").ok_or_else(|| {
            MezError::invalid_args("snapshot restore could not allocate default group id")
        })?;
        restored_ids.push(group_id.clone());
        let mut default_group = WindowGroup::new(
            group_id,
            0,
            "0",
            windows[active_window_index].id.clone(),
            Some(current_unix_seconds()),
        );
        default_group.window_ids = windows.iter().map(|window| window.id.clone()).collect();
        default_group.active_window_id = Some(windows[active_window_index].id.clone());
        return Ok((vec![default_group], 0, None));
    }

    let mut active_group_index = None;
    let mut groups = Vec::with_capacity(payload.window_groups.len());
    for (expected_index, group_payload) in payload.window_groups.iter().enumerate() {
        if group_payload.index != expected_index {
            return Err(MezError::invalid_args(
                "snapshot window groups must be stored in contiguous index order",
            ));
        }
        let group = restored_window_group(group_payload, windows, current_unix_seconds())?;
        if group_payload.active && active_group_index.replace(expected_index).is_some() {
            return Err(MezError::invalid_args(
                "snapshot payload contains multiple active window groups",
            ));
        }
        restored_ids.push(group.id.clone());
        groups.push(group);
    }
    let active_group_index = active_group_index.ok_or_else(|| {
        MezError::invalid_args("snapshot payload must contain exactly one active window group")
    })?;
    Ok((groups, active_group_index, None))
}

/// Converts one validated snapshot window group into a live session group.
fn restored_window_group(
    group_payload: &WindowGroupSnapshotPayload,
    windows: &[Window],
    restored_at: u64,
) -> Result<WindowGroup> {
    let group_id = WindowGroupId::parse('g', group_payload.group_id.clone()).ok_or_else(|| {
        MezError::invalid_args("snapshot payload contains an invalid window group id")
    })?;
    let window_ids = group_payload
        .window_ids
        .iter()
        .map(|window_id| restored_group_window_id(windows, window_id))
        .collect::<Result<Vec<_>>>()?;
    let first_window = window_ids.first().cloned().ok_or_else(|| {
        MezError::invalid_args("snapshot window group must contain at least one window")
    })?;
    let mut group = WindowGroup::new(
        group_id,
        group_payload.index,
        group_payload.name.clone(),
        first_window,
        Some(restored_at),
    );
    group.window_ids = window_ids;
    group.active_window_id = group_payload
        .active_window_id
        .as_deref()
        .map(|window_id| restored_group_window_id(windows, window_id))
        .transpose()?;
    group.last_active_window_id = group_payload
        .last_active_window_id
        .as_deref()
        .map(|window_id| restored_group_window_id(windows, window_id))
        .transpose()?;
    Ok(group)
}

/// Finds the restored window id for one snapshot group reference.
fn restored_group_window_id(windows: &[Window], window_id: &str) -> Result<WindowId> {
    windows
        .iter()
        .find(|window| window.id.as_str() == window_id)
        .map(|window| window.id.clone())
        .ok_or_else(|| MezError::invalid_args("snapshot window group references an unknown window"))
}

/// Runs the restored pane geometries operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn restored_pane_geometries(window_payload: &WindowSnapshotPayload) -> Option<Vec<PaneGeometry>> {
    let mut pane_geometries = Vec::with_capacity(window_payload.panes.len());
    for pane_payload in &window_payload.panes {
        let geometry = pane_payload.geometry.as_ref()?;
        pane_geometries.push(PaneGeometry {
            index: pane_payload.index,
            column: geometry.column,
            row: geometry.row,
            columns: geometry.columns,
            rows: geometry.rows,
        });
    }
    Some(pane_geometries)
}

/// Runs the restored layout root operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn restored_layout_root(
    window_payload: &WindowSnapshotPayload,
) -> Result<Option<crate::layout::LayoutNode>> {
    window_payload
        .layout_root
        .as_ref()
        .map(|layout_root| layout_root.to_layout_node(&window_payload.panes))
        .transpose()
}
