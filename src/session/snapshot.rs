//! Session restoration from snapshot payloads.
//!
//! Restore validates snapshot structure, reconstructs windows and panes, and
//! advances the id factory beyond restored identifiers.

use crate::error::{MezError, Result};
use crate::ids::{IdFactory, PaneId, SessionId, WindowGroupId, WindowId};
use crate::layout::{
    LayoutPolicy, Pane, PaneGeometry, PaneTitleSource, RestoredWindowLayout, Size, Window,
};
use crate::shell::ResolvedShell;
use crate::snapshot::{SessionSnapshotPayload, SnapshotSessionState, WindowSnapshotPayload};
use std::collections::BTreeMap;

use super::time::current_unix_seconds;
use super::types::{PaneStateMetadata, Session, SessionState, WindowGroup};

impl Session {
    /// Runs the from snapshot payload operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn from_snapshot_payload(
        shell: ResolvedShell,
        payload: &SessionSnapshotPayload,
    ) -> Result<Self> {
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
        let group_id = WindowGroupId::parse('g', "g1").ok_or_else(|| {
            MezError::invalid_args("snapshot restore could not allocate default group id")
        })?;
        restored_ids.push(group_id.clone());
        let mut default_group = WindowGroup::new(
            group_id,
            0,
            "0",
            windows[active_window_index].id.clone(),
            Some(restored_at),
        );
        default_group.window_ids = windows.iter().map(|window| window.id.clone()).collect();
        default_group.active_window_id = Some(windows[active_window_index].id.clone());

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
            window_groups: vec![default_group],
            active_group_index: 0,
            last_active_group_index: None,
            active_window_index,
            last_active_window_index: None,
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
