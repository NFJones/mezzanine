//! Session restoration from snapshot payloads.
//!
//! Restore validates snapshot structure, reconstructs windows and panes, and
//! advances the id factory beyond restored identifiers.

use crate::layout::{Pane, PaneTitleSource, RestoredWindowLayout, Window};
use crate::{MuxError as MezError, Result};
use mez_core::IdFactory;
use std::collections::{BTreeMap, BTreeSet};

use super::time::current_unix_seconds;
use super::types::{
    LandingNavigationState, PaneStateMetadata, RestoredSessionState, Session, SessionRestoreInput,
    SessionShell, SessionState, WindowGroup,
};

/// Carries freshly allocated layout state rebuilt from a snapshot payload.
type FreshSnapshotLayout = (
    Vec<Window>,
    Vec<WindowGroup>,
    usize,
    usize,
    Option<usize>,
    BTreeMap<String, PaneStateMetadata>,
    LandingNavigationState,
);

impl Session {
    /// Rebuilds a session from dependency-neutral data decoded by a product adapter.
    pub fn from_restore_input(
        shell: impl Into<SessionShell>,
        input: SessionRestoreInput,
    ) -> Result<Self> {
        let shell = shell.into();
        let restored_at = current_unix_seconds();
        let landing_navigation = input.landing_navigation.clone();
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
            group_focus_history: Vec::new(),
            active_window_index,
            last_active_window_index: None,
            synchronized_window_ids: BTreeSet::new(),
            pane_state_metadata,
            clients: Vec::new(),
            observer_attachments: Vec::new(),
            landing_navigation,
            layout_owner_client_id: None,
            layout_revision: 0,
            next_event_id: 1,
        })
    }

    /// Replaces user-visible layout state from dependency-neutral restored data.
    ///
    /// Fresh live identifiers are allocated so loading a saved layout behaves
    /// like recreating its groups, windows, and panes in the current session.
    pub fn replace_layout_from_restore_input(&mut self, input: SessionRestoreInput) -> Result<()> {
        let restored_authoritative_size = input.authoritative_size;
        let authoritative_size = self
            .layout_owner_client_id
            .as_ref()
            .and_then(|owner_id| self.clients.iter().find(|client| client.id == *owner_id))
            .and_then(|client| client.terminal.as_ref())
            .map(|terminal| crate::layout::Size::new(terminal.columns, terminal.rows))
            .transpose()?
            .unwrap_or(restored_authoritative_size);
        let (
            windows,
            window_groups,
            active_window_index,
            active_group_index,
            last_active_group_index,
            pane_state_metadata,
            landing_navigation,
        ) = self.layout_from_restore_input_with_fresh_ids(input)?;

        self.authoritative_size = restored_authoritative_size;
        self.windows = windows;
        self.window_groups = window_groups;
        self.active_window_index = active_window_index;
        self.last_active_window_index = None;
        self.active_group_index = active_group_index;
        self.last_active_group_index = last_active_group_index;
        self.group_focus_history.clear();
        self.pane_state_metadata = pane_state_metadata;
        self.landing_navigation = landing_navigation;
        let _ = self.apply_authoritative_layout_size(authoritative_size)?;
        let _ = self.reconcile_client_navigation();
        self.record_event();
        Ok(())
    }

    /// Rebuilds neutral restored layout metadata with live-session identifiers.
    fn layout_from_restore_input_with_fresh_ids(
        &mut self,
        input: SessionRestoreInput,
    ) -> Result<FreshSnapshotLayout> {
        let restored_landing_navigation = input.landing_navigation.clone();
        let mut restored_to_live_window_ids = BTreeMap::new();
        let mut restored_to_live_pane_ids = BTreeMap::new();
        let mut restored_to_live_group_ids = BTreeMap::new();
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
                let restored_pane_id = restored_pane.id.to_string();
                let pane_id = self.ids.pane();
                restored_to_live_pane_ids.insert(restored_pane_id, pane_id.clone());
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
            let restored_group_id = restored_group.id.to_string();
            let group_id = self.ids.window_group();
            restored_to_live_group_ids.insert(restored_group_id, group_id.clone());
            let mut group = WindowGroup::new(
                group_id,
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
        let landing_navigation = LandingNavigationState {
            active_group_id: restored_landing_navigation
                .active_group_id
                .as_ref()
                .and_then(|id| restored_to_live_group_ids.get(id.as_str()))
                .cloned(),
            active_window_id: restored_landing_navigation
                .active_window_id
                .as_ref()
                .and_then(|id| restored_to_live_window_ids.get(id.as_str()))
                .cloned(),
            active_pane_id: restored_landing_navigation
                .active_pane_id
                .as_ref()
                .and_then(|id| restored_to_live_pane_ids.get(id.as_str()))
                .cloned(),
        };

        Ok((
            windows,
            window_groups,
            active_window_index,
            active_group_index,
            None,
            pane_state_metadata,
            landing_navigation,
        ))
    }
}
