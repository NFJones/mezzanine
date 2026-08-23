//! Core session construction, accessors, and session-level metadata changes.
//!
//! Lifecycle methods initialize the default in-memory model and expose immutable
//! views while delegating clients, windows, and restore behavior to siblings.

use crate::layout::{Size, Window};
use crate::{MuxError as MezError, Result};
use mez_core::{ClientId, IdFactory};
use std::collections::{BTreeMap, BTreeSet};

use super::time::current_unix_seconds;
use super::types::{
    Client, ClientNavigationState, FocusCursor, LandingNavigationState, ObserverRequest,
    PaneStateMetadata, Session, SessionShell, SessionState, WindowGroup,
};

impl Session {
    /// Runs the new default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new_default(shell: impl Into<SessionShell>, size: Size) -> Self {
        let shell = shell.into();
        let mut ids = IdFactory::default();
        let id = ids.session();
        let now = current_unix_seconds();
        let mut window = Window::new(&mut ids, 0, "0", size);
        window.created_at_unix_seconds = Some(now);
        let group = WindowGroup::new(ids.window_group(), 0, "0", window.id.clone(), Some(now));
        let landing_navigation = LandingNavigationState {
            active_group_id: Some(group.id.clone()),
            active_window_id: Some(window.id.clone()),
            active_pane_id: Some(window.active_pane().id.clone()),
        };

        Self {
            ids,
            id,
            name: "default".to_string(),
            state: SessionState::Running,
            created_at_unix_seconds: now,
            updated_at_unix_seconds: now,
            last_attached_at_unix_seconds: None,
            authoritative_size: size,
            shell,
            config_generation: 0,
            windows: vec![window],
            window_groups: vec![group],
            active_group_index: 0,
            last_active_group_index: None,
            group_focus_history: Vec::new(),
            active_window_index: 0,
            last_active_window_index: None,
            synchronized_window_ids: BTreeSet::new(),
            pane_state_metadata: BTreeMap::new(),
            clients: Vec::new(),
            observers: Vec::new(),
            landing_navigation,
            primary_client_id: None,
            next_event_id: 1,
        }
    }

    /// Runs the windows operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn windows(&self) -> &[Window] {
        &self.windows
    }

    /// Returns the ordered window groups in this session.
    pub fn window_groups(&self) -> &[WindowGroup] {
        &self.window_groups
    }

    /// Returns the active window group when the session has one.
    pub fn active_group(&self) -> Option<&WindowGroup> {
        self.window_groups.get(self.active_group_index)
    }

    /// Runs the active window operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn active_window(&self) -> Option<&Window> {
        self.windows.get(self.active_window_index)
    }

    /// Returns the monotonic session mutation revision used to fence rendered input.
    pub fn mutation_revision(&self) -> u64 {
        self.next_event_id.saturating_sub(1)
    }

    /// Returns caller-relative active group for an attached primary.
    pub fn active_group_for(&self, client_id: &ClientId) -> Result<&WindowGroup> {
        let navigation = self.navigation(client_id)?;
        let group_id = navigation
            .groups
            .active
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("client has no active window group"))?;
        self.window_groups
            .iter()
            .find(|group| &group.id == group_id)
            .ok_or_else(|| MezError::invalid_state("client active window group is stale"))
    }

    /// Returns caller-relative active window for an attached primary.
    pub fn active_window_for(&self, client_id: &ClientId) -> Result<&Window> {
        let navigation = self.navigation(client_id)?;
        let group_id = navigation
            .groups
            .active
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("client has no active window group"))?;
        let window_id = navigation
            .windows_by_group
            .get(group_id)
            .and_then(|cursor| cursor.active.as_ref())
            .ok_or_else(|| MezError::invalid_state("client has no active window"))?;
        self.windows
            .iter()
            .find(|window| &window.id == window_id)
            .ok_or_else(|| MezError::invalid_state("client active window is stale"))
    }

    /// Returns caller-relative active pane for an attached primary.
    pub fn active_pane_for(&self, client_id: &ClientId) -> Result<&crate::layout::Pane> {
        let navigation = self.navigation(client_id)?;
        let window = self.active_window_for(client_id)?;
        let pane_id = navigation
            .panes_by_window
            .get(&window.id)
            .and_then(|cursor| cursor.active.as_ref())
            .ok_or_else(|| MezError::invalid_state("client has no active pane"))?;
        window
            .panes()
            .iter()
            .find(|pane| &pane.id == pane_id)
            .ok_or_else(|| MezError::invalid_state("client active pane is stale"))
    }

    /// Builds a fresh primary navigation state from client-independent landing focus.
    pub(super) fn navigation_from_landing(&self) -> ClientNavigationState {
        let mut navigation = ClientNavigationState::default();
        navigation.groups.active = self.landing_navigation.active_group_id.clone();
        if let (Some(group_id), Some(window_id)) = (
            self.landing_navigation.active_group_id.clone(),
            self.landing_navigation.active_window_id.clone(),
        ) {
            navigation.windows_by_group.insert(
                group_id,
                FocusCursor {
                    active: Some(window_id.clone()),
                    ..FocusCursor::default()
                },
            );
            if let Some(pane_id) = self.landing_navigation.active_pane_id.clone() {
                navigation.panes_by_window.insert(
                    window_id,
                    FocusCursor {
                        active: Some(pane_id),
                        ..FocusCursor::default()
                    },
                );
            }
        }
        navigation
    }

    /// Returns whether the session layout currently contains a pane.
    ///
    /// Process supervision can temporarily retain a pane process after its
    /// layout entry has been removed, so callers must not infer layout
    /// membership from process ownership alone.
    pub fn contains_pane(&self, pane_id: &str) -> bool {
        self.windows.iter().any(|window| {
            window
                .panes()
                .iter()
                .any(|pane| pane.id.as_str() == pane_id)
        })
    }

    /// Returns whether pane input synchronization is active for the active window.
    pub fn active_window_panes_synchronized(&self) -> bool {
        self.active_window()
            .is_some_and(|window| self.synchronized_window_ids.contains(window.id.as_str()))
    }

    /// Returns whether pane input synchronization is enabled for a target window.
    pub fn window_panes_synchronized(&self, target: Option<&str>) -> Result<bool> {
        let index = self.window_index_or_active(target)?;
        Ok(self
            .windows
            .get(index)
            .is_some_and(|window| self.synchronized_window_ids.contains(window.id.as_str())))
    }

    /// Runs the clients operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn clients(&self) -> &[Client] {
        &self.clients
    }

    /// Runs the observers operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn observers(&self) -> &[ObserverRequest] {
        &self.observers
    }

    /// Runs the pane state metadata operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn pane_state_metadata(&self, pane_id: &str) -> Option<&PaneStateMetadata> {
        self.pane_state_metadata.get(pane_id)
    }

    /// Runs the primary client id operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn primary_client_id(&self) -> Option<&ClientId> {
        self.primary_client_id.as_ref()
    }

    /// Runs the advance config generation operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn advance_config_generation(&mut self) -> u64 {
        self.config_generation = self.config_generation.saturating_add(1);
        self.record_event();
        self.config_generation
    }

    /// Runs the rename session operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn rename_session(
        &mut self,
        primary_client_id: &ClientId,
        name: impl Into<String>,
    ) -> Result<()> {
        self.require_primary(primary_client_id)?;
        let name = name.into();
        if name.is_empty() {
            return Err(MezError::invalid_args("session name must not be empty"));
        }
        self.name = name;
        self.record_event();
        Ok(())
    }
}
