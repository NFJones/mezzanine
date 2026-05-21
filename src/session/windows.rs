//! Session window and pane operations.
//!
//! This module owns primary-authorized window selection, pane splitting, joins,
//! kills, layout cycling, window reordering, live-state updates, and
//! active-window bookkeeping calls.

use crate::error::{MezError, Result};
use crate::ids::{ClientId, PaneId, WindowGroupId, WindowId};
use crate::layout::{
    LayoutPolicy, Pane, PaneGeometry, PaneNavigationDirection, PaneSizeSpec, PaneTitleSource, Size,
    SplitDirection, Window,
};

use super::targets::JoinDestination;
use super::time::current_unix_seconds;
use super::types::{ClientState, Session, SessionState, WindowGroup};

/// Defines the DEFAULT PANE TITLE const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const DEFAULT_PANE_TITLE: &str = "shell";

impl Session {
    /// Runs the new window operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new_window(
        &mut self,
        primary_client_id: &ClientId,
        name: impl Into<String>,
        select: bool,
    ) -> Result<WindowId> {
        self.require_primary(primary_client_id)?;
        let index = self.windows.len();
        let mut window = Window::new(&mut self.ids, index, name, self.authoritative_size);
        window.created_at_unix_seconds = Some(current_unix_seconds());
        let id = window.id.clone();
        self.windows.push(window);
        if self.window_groups.is_empty() {
            let group = WindowGroup::new(
                self.ids.window_group(),
                0,
                "0",
                id.clone(),
                Some(current_unix_seconds()),
            );
            self.window_groups.push(group);
            self.active_group_index = 0;
        } else {
            let group_index = self
                .active_group_index
                .min(self.window_groups.len().saturating_sub(1));
            if let Some(group) = self.window_groups.get_mut(group_index) {
                group.window_ids.push(id.clone());
                if group.active_window_id.is_none() {
                    group.active_window_id = Some(id.clone());
                }
            }
        }
        if select {
            self.set_active_window_index(index);
        }
        self.record_event();
        Ok(id)
    }

    /// Creates a new window inside a specific existing group.
    ///
    /// This is used by background orchestration paths that must keep the
    /// primary user's active pane unchanged while still placing related windows
    /// in the same visible group as their controller.
    pub fn new_window_in_group(
        &mut self,
        primary_client_id: &ClientId,
        group_id: &WindowGroupId,
        name: impl Into<String>,
        select: bool,
    ) -> Result<WindowId> {
        self.require_primary(primary_client_id)?;
        let group_index = self
            .window_groups
            .iter()
            .position(|group| &group.id == group_id)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "window group not found",
                )
            })?;
        let index = self.windows.len();
        let mut window = Window::new(&mut self.ids, index, name, self.authoritative_size);
        window.created_at_unix_seconds = Some(current_unix_seconds());
        let id = window.id.clone();
        self.windows.push(window);
        if let Some(group) = self.window_groups.get_mut(group_index) {
            group.window_ids.push(id.clone());
            if group.active_window_id.is_none() {
                group.active_window_id = Some(id.clone());
            }
        }
        if select {
            self.set_active_window_index(index);
        }
        self.record_event();
        Ok(id)
    }

    /// Creates a new window group with a single landing window.
    ///
    /// The new group receives a stable `gN` id and the landing window receives
    /// the requested name so a single create command has an immediate visible
    /// title in both the top group bar and the normal window bar.
    pub fn new_group(
        &mut self,
        primary_client_id: &ClientId,
        name: impl Into<String>,
        select: bool,
    ) -> Result<(crate::ids::WindowGroupId, WindowId)> {
        self.require_primary(primary_client_id)?;
        let name = name.into();
        let window_index = self.windows.len();
        let mut window = Window::new(
            &mut self.ids,
            window_index,
            if name.trim().is_empty() {
                window_index.to_string()
            } else {
                name.clone()
            },
            self.authoritative_size,
        );
        let now = current_unix_seconds();
        window.created_at_unix_seconds = Some(now);
        let window_id = window.id.clone();
        let group_id = self.ids.window_group();
        let group_name = if name.trim().is_empty() {
            self.window_groups.len().to_string()
        } else {
            name
        };
        let group = WindowGroup::new(
            group_id.clone(),
            self.window_groups.len(),
            group_name,
            window_id.clone(),
            Some(now),
        );
        self.windows.push(window);
        self.window_groups.push(group);
        if select || self.window_groups.len() == 1 {
            self.set_active_window_index(window_index);
        }
        self.record_event();
        Ok((group_id, window_id))
    }

    /// Runs the rename window operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn rename_window(
        &mut self,
        primary_client_id: &ClientId,
        target: Option<&str>,
        name: impl Into<String>,
    ) -> Result<()> {
        self.require_primary(primary_client_id)?;
        let index = self.window_index_or_active(target)?;
        self.windows[index].rename(name);
        self.record_event();
        Ok(())
    }

    /// Assigns a generated window name unless the target has an explicit name.
    pub fn rename_window_generated(
        &mut self,
        primary_client_id: &ClientId,
        target: &str,
        name: impl Into<String>,
    ) -> Result<bool> {
        self.require_primary(primary_client_id)?;
        let index = self.window_index_or_active(Some(target))?;
        if self.windows[index].has_explicit_name() {
            return Ok(false);
        }
        self.windows[index].rename_generated(name);
        self.record_event();
        Ok(true)
    }

    /// Marks a target window as generated so runtime refreshes may rename it.
    pub fn mark_window_name_generated(
        &mut self,
        primary_client_id: &ClientId,
        target: &str,
    ) -> Result<()> {
        self.require_primary(primary_client_id)?;
        let index = self.window_index_or_active(Some(target))?;
        self.windows[index].mark_name_generated();
        self.record_event();
        Ok(())
    }

    /// Runs the select window operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn select_window(&mut self, primary_client_id: &ClientId, target: &str) -> Result<()> {
        self.require_primary(primary_client_id)?;
        let index = self.window_index_or_active(Some(target))?;
        self.set_active_window_index(index);
        self.record_event();
        Ok(())
    }

    /// Runs the next window operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn next_window(&mut self, primary_client_id: &ClientId) -> Result<()> {
        self.require_primary(primary_client_id)?;
        if self.windows.is_empty() {
            return Err(MezError::invalid_state("session has no windows"));
        }
        let group_indexes = self.active_group_window_indexes();
        if group_indexes.is_empty() {
            return Err(MezError::invalid_state("active group has no windows"));
        }
        let position = group_indexes
            .iter()
            .position(|index| *index == self.active_window_index)
            .unwrap_or(0);
        let index = group_indexes[(position + 1) % group_indexes.len()];
        self.set_active_window_index(index);
        self.record_event();
        Ok(())
    }

    /// Runs the previous window operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn previous_window(&mut self, primary_client_id: &ClientId) -> Result<()> {
        self.require_primary(primary_client_id)?;
        if self.windows.is_empty() {
            return Err(MezError::invalid_state("session has no windows"));
        }
        let group_indexes = self.active_group_window_indexes();
        if group_indexes.is_empty() {
            return Err(MezError::invalid_state("active group has no windows"));
        }
        let position = group_indexes
            .iter()
            .position(|index| *index == self.active_window_index)
            .unwrap_or(0);
        let index = if position == 0 {
            group_indexes[group_indexes.len() - 1]
        } else {
            group_indexes[position - 1]
        };
        self.set_active_window_index(index);
        self.record_event();
        Ok(())
    }

    /// Runs the last window operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn last_window(&mut self, primary_client_id: &ClientId) -> Result<()> {
        self.require_primary(primary_client_id)?;
        let index = self
            .active_group()
            .and_then(|group| group.last_active_window_id.as_ref())
            .and_then(|window_id| self.window_index_by_id(window_id.as_str()))
            .or_else(|| {
                self.last_active_window_index
                    .filter(|index| *index < self.windows.len())
            })
            .ok_or_else(|| MezError::invalid_state("session has no last active window"))?;
        self.set_active_window_index(index);
        self.record_event();
        Ok(())
    }

    /// Selects a window group by id, index, name, or navigation alias.
    pub fn select_group(&mut self, primary_client_id: &ClientId, target: &str) -> Result<()> {
        self.require_primary(primary_client_id)?;
        let group_index = self.group_index_or_active(Some(target))?;
        self.set_active_group_index(group_index)?;
        self.record_event();
        Ok(())
    }

    /// Selects the next window group in displayed order.
    pub fn next_group(&mut self, primary_client_id: &ClientId) -> Result<()> {
        self.require_primary(primary_client_id)?;
        if self.window_groups.is_empty() {
            return Err(MezError::invalid_state("session has no window groups"));
        }
        let index = (self.active_group_index + 1) % self.window_groups.len();
        self.set_active_group_index(index)?;
        self.record_event();
        Ok(())
    }

    /// Selects the previous window group in displayed order.
    pub fn previous_group(&mut self, primary_client_id: &ClientId) -> Result<()> {
        self.require_primary(primary_client_id)?;
        if self.window_groups.is_empty() {
            return Err(MezError::invalid_state("session has no window groups"));
        }
        let index = if self.active_group_index == 0 {
            self.window_groups.len() - 1
        } else {
            self.active_group_index - 1
        };
        self.set_active_group_index(index)?;
        self.record_event();
        Ok(())
    }

    /// Selects the previously active window group.
    pub fn last_group(&mut self, primary_client_id: &ClientId) -> Result<()> {
        self.require_primary(primary_client_id)?;
        let index = self
            .last_active_group_index
            .filter(|index| *index < self.window_groups.len())
            .ok_or_else(|| MezError::invalid_state("session has no last active group"))?;
        self.set_active_group_index(index)?;
        self.record_event();
        Ok(())
    }

    /// Renames a target window group.
    pub fn rename_group(
        &mut self,
        primary_client_id: &ClientId,
        target: Option<&str>,
        name: impl Into<String>,
    ) -> Result<()> {
        self.require_primary(primary_client_id)?;
        let name = name.into();
        if name.trim().is_empty() {
            return Err(MezError::invalid_args(
                "window group name must not be empty",
            ));
        }
        let group_index = self.group_index_or_active(target)?;
        self.window_groups[group_index].name = name;
        self.record_event();
        Ok(())
    }

    /// Moves the source or active window to a new window index.
    ///
    /// Window ids, panes, pane processes, buffers, and agent identity are
    /// preserved because the operation only reorders the session window vector.
    /// The active and last-active window pointers follow their original window
    /// identities after reindexing. A target outside the current window range
    /// is rejected before any mutation occurs.
    pub fn move_window(
        &mut self,
        primary_client_id: &ClientId,
        source: Option<&str>,
        target_index: usize,
    ) -> Result<()> {
        self.require_primary(primary_client_id)?;
        if self.windows.is_empty() {
            return Err(MezError::invalid_state("session has no windows"));
        }
        if target_index >= self.windows.len() {
            return Err(MezError::invalid_args(
                "move-window target index is outside the window range",
            ));
        }
        let source_index = self.window_index_or_active(source)?;
        if source_index == target_index {
            return Ok(());
        }

        let active_window_id = self.windows[self.active_window_index].id.clone();
        let last_active_window_id = self
            .last_active_window_index
            .and_then(|index| self.windows.get(index))
            .map(|window| window.id.clone());

        let window = self.windows.remove(source_index);
        self.windows.insert(target_index, window);
        self.reindex_windows();
        self.sync_group_window_order_with_window_order();
        self.active_window_index = self
            .window_index_by_id(active_window_id.as_str())
            .unwrap_or(0);
        self.last_active_window_index =
            last_active_window_id.and_then(|window_id| self.window_index_by_id(window_id.as_str()));
        self.record_event();
        Ok(())
    }

    /// Runs the split active pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn split_active_pane(
        &mut self,
        primary_client_id: &ClientId,
        direction: SplitDirection,
    ) -> Result<PaneId> {
        self.split_active_pane_select(primary_client_id, direction, true)
    }

    /// Splits the active pane and optionally selects the newly created pane.
    ///
    /// The default muxxer behavior follows the default mux behavior by moving focus to the newly
    /// spawned pane. Explicit detached/no-select command and control requests
    /// use `select_new = false`.
    pub fn split_active_pane_select(
        &mut self,
        primary_client_id: &ClientId,
        direction: SplitDirection,
        select_new: bool,
    ) -> Result<PaneId> {
        self.require_primary(primary_client_id)?;
        let window = self
            .windows
            .get_mut(self.active_window_index)
            .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
        let pane_id = window
            .split_active_select(&mut self.ids, direction, select_new)?
            .id
            .clone();
        self.record_event();
        Ok(pane_id)
    }

    /// Splits the active pane inside a target window without focusing that
    /// window at the session level.
    ///
    /// The target window still records its own active pane so background windows
    /// can manage their panes naturally, but the primary user's active window
    /// and active group remain unchanged unless `window_id` is already focused.
    pub fn split_pane_in_window_select(
        &mut self,
        primary_client_id: &ClientId,
        window_id: &WindowId,
        direction: SplitDirection,
        select_new: bool,
    ) -> Result<PaneId> {
        self.require_primary(primary_client_id)?;
        let window = self
            .windows
            .iter_mut()
            .find(|window| &window.id == window_id)
            .ok_or_else(|| {
                MezError::new(crate::error::MezErrorKind::NotFound, "window not found")
            })?;
        let pane_id = window
            .split_active_select(&mut self.ids, direction, select_new)?
            .id
            .clone();
        self.record_event();
        Ok(pane_id)
    }

    /// Splits the active pane and assigns a spec-derived size to the new pane.
    ///
    /// The split is rejected before mutation if the requested pane size cannot
    /// be represented without leaving unused space or overlapping the existing
    /// pane in the active window's split tree.
    pub fn split_active_pane_with_size_spec(
        &mut self,
        primary_client_id: &ClientId,
        direction: SplitDirection,
        requested_size: PaneSizeSpec,
    ) -> Result<PaneId> {
        self.split_active_pane_with_size_spec_select(
            primary_client_id,
            direction,
            requested_size,
            true,
        )
    }

    /// Splits the active pane with a requested size and optional new-pane focus.
    pub fn split_active_pane_with_size_spec_select(
        &mut self,
        primary_client_id: &ClientId,
        direction: SplitDirection,
        requested_size: PaneSizeSpec,
        select_new: bool,
    ) -> Result<PaneId> {
        self.require_primary(primary_client_id)?;
        let window = self
            .windows
            .get_mut(self.active_window_index)
            .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
        let pane_id = window
            .split_active_with_size_spec_select(
                &mut self.ids,
                direction,
                requested_size,
                select_new,
            )?
            .id
            .clone();
        self.record_event();
        Ok(pane_id)
    }

    /// Runs the select pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn select_pane(&mut self, primary_client_id: &ClientId, target: &str) -> Result<()> {
        self.require_primary(primary_client_id)?;
        let window = self
            .windows
            .get_mut(self.active_window_index)
            .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
        window.select_pane(target)?;
        self.record_event();
        Ok(())
    }

    /// Runs the select pane global operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn select_pane_global(&mut self, primary_client_id: &ClientId, target: &str) -> Result<()> {
        self.require_primary(primary_client_id)?;
        let (window_index, pane_index) = self.pane_location(Some(target))?;
        let pane_id = self.windows[window_index].panes()[pane_index].id.clone();
        self.set_active_window_index(window_index);
        let window = self
            .windows
            .get_mut(self.active_window_index)
            .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
        window.select_pane(pane_id.as_str())?;
        self.record_event();
        Ok(())
    }

    /// Runs the select adjacent pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn select_adjacent_pane(
        &mut self,
        primary_client_id: &ClientId,
        direction: PaneNavigationDirection,
    ) -> Result<PaneId> {
        self.require_primary(primary_client_id)?;
        let window = self
            .windows
            .get_mut(self.active_window_index)
            .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
        window.select_adjacent_pane(direction)?;
        let pane_id = window.active_pane().id.clone();
        self.record_event();
        Ok(pane_id)
    }

    /// Runs the select last pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn select_last_pane(&mut self, primary_client_id: &ClientId) -> Result<PaneId> {
        self.require_primary(primary_client_id)?;
        let window = self
            .windows
            .get_mut(self.active_window_index)
            .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
        window.select_last_pane()?;
        let pane_id = window.active_pane().id.clone();
        self.record_event();
        Ok(pane_id)
    }

    /// Runs the toggle active pane zoom operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn toggle_active_pane_zoom(
        &mut self,
        primary_client_id: &ClientId,
    ) -> Result<Option<PaneId>> {
        self.require_primary(primary_client_id)?;
        let window = self
            .windows
            .get_mut(self.active_window_index)
            .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
        let zoomed = window.toggle_zoom_active().cloned();
        self.record_event();
        Ok(zoomed)
    }

    /// Runs the rotate panes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn rotate_panes(&mut self, primary_client_id: &ClientId, reverse: bool) -> Result<()> {
        self.require_primary(primary_client_id)?;
        let window = self
            .windows
            .get_mut(self.active_window_index)
            .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
        window.rotate_panes(reverse);
        self.record_event();
        Ok(())
    }

    /// Runs the cycle layout operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn cycle_layout(&mut self, primary_client_id: &ClientId) -> Result<LayoutPolicy> {
        self.require_primary(primary_client_id)?;
        let window = self
            .windows
            .get_mut(self.active_window_index)
            .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
        let policy = window.cycle_layout();
        self.record_event();
        Ok(policy)
    }

    /// Runs the select layout operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn select_layout(
        &mut self,
        primary_client_id: &ClientId,
        layout_name: &str,
    ) -> Result<LayoutPolicy> {
        self.require_primary(primary_client_id)?;
        let policy = LayoutPolicy::from_name(layout_name)
            .ok_or_else(|| MezError::invalid_args("select-layout layout is invalid"))?;
        let window = self
            .windows
            .get_mut(self.active_window_index)
            .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
        let policy = window.set_layout_policy(policy);
        self.record_event();
        Ok(policy)
    }

    /// Reapplies the active window's current layout policy.
    ///
    /// This command is useful after direct pane resizing or manual layout
    /// changes have disturbed the active policy's balanced geometry. It keeps
    /// the selected policy unchanged while forcing the window to recompute its
    /// pane rectangles and sizes.
    pub fn rebalance_window(&mut self, primary_client_id: &ClientId) -> Result<LayoutPolicy> {
        self.require_primary(primary_client_id)?;
        let window = self
            .windows
            .get_mut(self.active_window_index)
            .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
        let policy = window.set_layout_policy(window.layout_policy());
        self.record_event();
        Ok(policy)
    }

    /// Applies a layout policy to a specific window without changing focus.
    pub fn set_window_layout_policy(
        &mut self,
        primary_client_id: &ClientId,
        window_id: &WindowId,
        policy: LayoutPolicy,
    ) -> Result<LayoutPolicy> {
        self.require_primary(primary_client_id)?;
        let window = self
            .windows
            .iter_mut()
            .find(|window| &window.id == window_id)
            .ok_or_else(|| {
                MezError::new(crate::error::MezErrorKind::NotFound, "window not found")
            })?;
        let policy = window.set_layout_policy(policy);
        self.record_event();
        Ok(policy)
    }

    /// Runs the resize pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn resize_pane(
        &mut self,
        primary_client_id: &ClientId,
        target: Option<&str>,
        size: Size,
    ) -> Result<Pane> {
        self.require_primary(primary_client_id)?;
        let window = self
            .windows
            .get_mut(self.active_window_index)
            .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
        let pane = window.resize_pane(target, size)?.clone();
        self.record_event();
        Ok(pane)
    }

    /// Resizes a pane from a spec-defined size request.
    pub fn resize_pane_with_spec(
        &mut self,
        primary_client_id: &ClientId,
        target: Option<&str>,
        spec: PaneSizeSpec,
    ) -> Result<Pane> {
        self.require_primary(primary_client_id)?;
        let window = self
            .windows
            .get_mut(self.active_window_index)
            .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
        let pane = window.resize_pane_with_spec(target, spec)?.clone();
        self.record_event();
        Ok(pane)
    }

    /// Replaces the active window's pane geometry after a rendered border drag.
    pub fn replace_active_window_pane_geometries(
        &mut self,
        primary_client_id: &ClientId,
        geometries: Vec<PaneGeometry>,
    ) -> Result<()> {
        self.require_primary(primary_client_id)?;
        let window = self
            .windows
            .get_mut(self.active_window_index)
            .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
        window.replace_pane_geometries(geometries)?;
        self.record_event();
        Ok(())
    }

    /// Resizes a pane in a specific window without changing the active window.
    ///
    /// This is used by creation paths that may create an unselected window but
    /// still need to apply a requested pane size before the pane process starts.
    pub fn resize_pane_in_window(
        &mut self,
        primary_client_id: &ClientId,
        window_id: &WindowId,
        pane_id: &PaneId,
        size: Size,
    ) -> Result<Pane> {
        self.require_primary(primary_client_id)?;
        let index = self.window_index_or_active(Some(window_id.as_str()))?;
        let pane = self.windows[index]
            .resize_pane(Some(pane_id.as_str()), size)?
            .clone();
        self.record_event();
        Ok(pane)
    }

    /// Resizes a pane in a specific window from a spec without changing focus.
    pub fn resize_pane_in_window_with_spec(
        &mut self,
        primary_client_id: &ClientId,
        window_id: &WindowId,
        pane_id: &PaneId,
        spec: PaneSizeSpec,
    ) -> Result<Pane> {
        self.require_primary(primary_client_id)?;
        let index = self.window_index_or_active(Some(window_id.as_str()))?;
        let pane = self.windows[index]
            .resize_pane_with_spec(Some(pane_id.as_str()), spec)?
            .clone();
        self.record_event();
        Ok(pane)
    }

    /// Updates the primary terminal size and reapportions every window to match it.
    pub fn resize_authoritative_terminal(
        &mut self,
        primary_client_id: &ClientId,
        size: Size,
    ) -> Result<()> {
        self.require_primary(primary_client_id)?;
        self.authoritative_size = size;
        for window in &mut self.windows {
            window.resize_window(size)?;
        }
        self.record_event();
        Ok(())
    }

    /// Runs the set pane live state operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn set_pane_live_state(&mut self, pane_id: &str, live: bool) -> Result<()> {
        let (window_index, pane_index) = self.pane_location(Some(pane_id))?;
        self.windows[window_index].panes_mut()[pane_index].live = live;
        self.record_event();
        Ok(())
    }

    /// Runs the set pane title from terminal operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn set_pane_title_from_terminal(
        &mut self,
        pane_id: &str,
        title: impl Into<String>,
    ) -> Result<bool> {
        let title = terminal_pane_title_or_default(&title.into());
        let (window_index, pane_index) = self.pane_location(Some(pane_id))?;
        let pane = &mut self.windows[window_index].panes_mut()[pane_index];
        if pane.title == title {
            return Ok(false);
        }
        if pane.title_source.is_explicit() {
            return Ok(false);
        }
        pane.title = title;
        pane.title_source = PaneTitleSource::Automatic;
        self.record_event();
        Ok(true)
    }

    /// Explicitly assigns a pane title from a user or agent command.
    pub fn set_pane_title_explicit(
        &mut self,
        pane_id: &str,
        title: impl Into<String>,
    ) -> Result<bool> {
        let title = terminal_pane_title_or_default(&title.into());
        let (window_index, pane_index) = self.pane_location(Some(pane_id))?;
        let pane = &mut self.windows[window_index].panes_mut()[pane_index];
        let changed = pane.title != title || !pane.title_source.is_explicit();
        if !changed {
            return Ok(false);
        }
        pane.title = title;
        pane.title_source = PaneTitleSource::Explicit;
        self.record_event();
        Ok(true)
    }

    /// Runs the swap panes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn swap_panes(
        &mut self,
        primary_client_id: &ClientId,
        source: Option<&str>,
        target: &str,
    ) -> Result<()> {
        self.require_primary(primary_client_id)?;
        let (source_window_index, source_pane_index) = self.pane_location(source)?;
        let (target_window_index, target_pane_index) = self.pane_location(Some(target))?;

        if source_window_index == target_window_index {
            self.windows[source_window_index].swap_panes(source, target)?;
        } else if source_window_index < target_window_index {
            let (left, right) = self.windows.split_at_mut(target_window_index);
            Window::swap_panes_between(
                &mut left[source_window_index],
                source_pane_index,
                &mut right[0],
                target_pane_index,
            );
        } else {
            let (left, right) = self.windows.split_at_mut(source_window_index);
            Window::swap_panes_between(
                &mut right[0],
                source_pane_index,
                &mut left[target_window_index],
                target_pane_index,
            );
        }

        self.record_event();
        Ok(())
    }

    /// Runs the break pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn break_pane(
        &mut self,
        primary_client_id: &ClientId,
        target: Option<&str>,
        name: Option<String>,
        select_new_window: bool,
    ) -> Result<WindowId> {
        self.require_primary(primary_client_id)?;
        let (source_window_index, source_pane_index) = self.pane_location(target)?;
        let source_window_id = self.windows[source_window_index].id.clone();
        let source_group_index = self.group_index_containing_window_id(&source_window_id);
        let source_group_window_position = source_group_index.and_then(|group_index| {
            self.window_groups[group_index]
                .window_ids
                .iter()
                .position(|window_id| window_id == &source_window_id)
        });
        let pane = self.windows[source_window_index].take_pane_at(source_pane_index);

        let removed_source_window = self.windows[source_window_index].panes().is_empty();
        if self.windows[source_window_index].panes().is_empty() {
            self.windows.remove(source_window_index);
            self.reindex_windows();
        }

        let new_window_index = self.windows.len();
        let window_name = name.unwrap_or_else(|| new_window_index.to_string());
        let mut window = Window::from_existing_pane(
            &mut self.ids,
            new_window_index,
            window_name,
            self.authoritative_size,
            pane,
        );
        window.created_at_unix_seconds = Some(current_unix_seconds());
        let window_id = window.id.clone();
        self.windows.push(window);
        if let Some(group_index) = source_group_index
            && let Some(group) = self.window_groups.get_mut(group_index)
        {
            if removed_source_window {
                if let Some(position) = source_group_window_position
                    && let Some(slot) = group.window_ids.get_mut(position)
                {
                    *slot = window_id.clone();
                }
            } else if let Some(position) = source_group_window_position {
                let insert_at = position.saturating_add(1).min(group.window_ids.len());
                group.window_ids.insert(insert_at, window_id.clone());
            } else {
                group.window_ids.push(window_id.clone());
            }
            if group.active_window_id.as_ref() == Some(&source_window_id) && removed_source_window {
                group.active_window_id = Some(window_id.clone());
            }
        } else if self.window_groups.is_empty() {
            self.window_groups.push(WindowGroup::new(
                self.ids.window_group(),
                0,
                "0",
                window_id.clone(),
                Some(current_unix_seconds()),
            ));
        } else if let Some(group) = self.window_groups.get_mut(self.active_group_index) {
            group.window_ids.push(window_id.clone());
        }
        self.reindex_windows();
        self.reconcile_window_groups_after_window_removal();
        if select_new_window || self.windows.len() == 1 {
            self.set_active_window_index(new_window_index);
        } else if removed_source_window {
            self.active_window_index = self.active_window_index.min(self.windows.len() - 1);
            self.sync_active_group_to_active_window();
        }
        self.record_event();
        Ok(window_id)
    }

    /// Runs the join pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn join_pane(
        &mut self,
        primary_client_id: &ClientId,
        source: Option<&str>,
        target: &str,
        direction: SplitDirection,
        select_joined_pane: bool,
    ) -> Result<PaneId> {
        self.require_primary(primary_client_id)?;
        let (source_window_index, source_pane_index) = self.pane_location(source)?;
        let destination = self.join_destination(target)?;

        if let JoinDestination::Pane {
            window_index,
            pane_id,
        } = &destination
            && *window_index == source_window_index
            && self.windows[source_window_index].panes()[source_pane_index]
                .id
                .as_str()
                == pane_id.as_str()
        {
            return Err(MezError::invalid_args(
                "join-pane source and destination pane must differ",
            ));
        }
        if matches!(destination, JoinDestination::Window(window_index) if window_index == source_window_index)
            && self.windows[source_window_index].panes().len() == 1
        {
            return Err(MezError::invalid_args(
                "join-pane cannot move the final pane into its own window",
            ));
        }

        let pane = self.windows[source_window_index].take_pane_at(source_pane_index);
        let joined_pane_id = pane.id.clone();
        let removed_source_window = self.windows[source_window_index].panes().is_empty();
        if removed_source_window {
            self.windows.remove(source_window_index);
            self.after_window_removed(source_window_index);
        }

        let (destination_window_index, destination_pane_target) =
            self.adjust_join_destination(destination, source_window_index, removed_source_window)?;
        self.windows[destination_window_index].insert_existing_after(
            destination_pane_target.as_deref(),
            pane,
            direction,
            select_joined_pane,
        )?;
        if select_joined_pane {
            self.set_active_window_index(destination_window_index);
        }
        self.record_event();
        Ok(joined_pane_id)
    }

    /// Runs the kill pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn kill_pane(
        &mut self,
        primary_client_id: &ClientId,
        target: Option<&str>,
        force: bool,
    ) -> Result<Option<Pane>> {
        self.require_primary(primary_client_id)?;
        let (window_index, pane_index) = match target {
            Some(target) => self.pane_location(Some(target))?,
            None => {
                let window = self
                    .windows
                    .get(self.active_window_index)
                    .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
                (self.active_window_index, window.active_pane_index())
            }
        };
        let window = self
            .windows
            .get(window_index)
            .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
        let target_pane = window
            .panes()
            .get(pane_index)
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "pane not found"))?;

        if target_pane.live && !force {
            return Err(MezError::forbidden(
                "killing a live pane requires an explicit force flag",
            ));
        }

        let removed = if window.panes().len() == 1 {
            let window = self.windows.remove(window_index);
            self.after_window_removed(window_index);
            window.panes().first().cloned()
        } else {
            let target_id = target_pane.id.to_string();
            Some(self.windows[window_index].kill_pane(Some(&target_id))?)
        };

        self.record_event();
        Ok(removed)
    }

    /// Runs the close exited pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn close_exited_pane(&mut self, pane_id: &str) -> Result<Option<Pane>> {
        let (window_index, _pane_index) = self.pane_location(Some(pane_id))?;
        let removed = if self.windows[window_index].panes().len() == 1 {
            let window = self.windows.remove(window_index);
            self.after_window_removed(window_index);
            window.panes().first().cloned()
        } else {
            Some(self.windows[window_index].kill_pane(Some(pane_id))?)
        };
        self.record_event();
        Ok(removed)
    }

    /// Runs the kill session operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn kill_session(&mut self, primary_client_id: &ClientId, force: bool) -> Result<()> {
        self.require_primary(primary_client_id)?;
        if self
            .windows
            .iter()
            .flat_map(|window| window.panes())
            .any(|pane| pane.live)
            && !force
        {
            return Err(MezError::forbidden(
                "killing a session with live panes requires an explicit force flag",
            ));
        }
        self.windows.clear();
        self.window_groups.clear();
        self.active_group_index = 0;
        self.last_active_group_index = None;
        self.active_window_index = 0;
        self.last_active_window_index = None;
        self.state = SessionState::Empty;
        self.record_event();
        Ok(())
    }

    /// Forces session shutdown from the runtime supervisor without requiring an
    /// attached primary client.
    pub(crate) fn force_supervisor_shutdown(&mut self) {
        let now = current_unix_seconds();
        for client in &mut self.clients {
            if client.state == ClientState::Attached {
                client.state = ClientState::Detached;
                client.last_seen_at_unix_seconds = Some(now);
            }
        }
        self.primary_client_id = None;
        self.windows.clear();
        self.window_groups.clear();
        self.active_group_index = 0;
        self.last_active_group_index = None;
        self.active_window_index = 0;
        self.last_active_window_index = None;
        self.state = SessionState::Empty;
        self.record_event();
    }

    /// Runs the kill window operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn kill_window(
        &mut self,
        primary_client_id: &ClientId,
        target: Option<&str>,
        force: bool,
    ) -> Result<Window> {
        self.require_primary(primary_client_id)?;
        let index = self.window_index_or_active(target)?;
        if self.windows[index].panes().iter().any(|pane| pane.live) && !force {
            return Err(MezError::forbidden(
                "killing a window with live panes requires an explicit force flag",
            ));
        }
        let removed = self.windows.remove(index);
        self.after_window_removed(index);
        self.record_event();
        Ok(removed)
    }

    /// Closes an entire window group and returns the removed windows.
    pub fn kill_group(
        &mut self,
        primary_client_id: &ClientId,
        target: Option<&str>,
        force: bool,
    ) -> Result<Vec<Window>> {
        self.require_primary(primary_client_id)?;
        if self.window_groups.len() <= 1 {
            return Err(MezError::forbidden(
                "killing the final window group requires kill-session",
            ));
        }
        let group_index = self.group_index_or_active(target)?;
        let window_ids = self.window_groups[group_index].window_ids.clone();
        let live = self
            .windows
            .iter()
            .filter(|window| window_ids.iter().any(|id| id == &window.id))
            .flat_map(|window| window.panes())
            .any(|pane| pane.live);
        if live && !force {
            return Err(MezError::forbidden(
                "killing a group with live panes requires an explicit force flag",
            ));
        }

        let mut removed = Vec::new();
        let mut index = 0usize;
        while index < self.windows.len() {
            if window_ids.iter().any(|id| id == &self.windows[index].id) {
                removed.push(self.windows.remove(index));
            } else {
                index = index.saturating_add(1);
            }
        }
        self.reindex_windows();
        self.reconcile_window_groups_after_window_removal();
        if !self.windows.is_empty() {
            self.active_window_index = self.active_window_index.min(self.windows.len() - 1);
            self.sync_active_group_to_active_window();
        }
        self.record_event();
        Ok(removed)
    }
}

/// Runs the terminal pane title or default operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn terminal_pane_title_or_default(title: &str) -> String {
    let sanitized = title
        .chars()
        .filter(|ch| !ch.is_control())
        .collect::<String>();
    let trimmed = sanitized.trim();
    if trimmed.is_empty() {
        DEFAULT_PANE_TITLE.to_string()
    } else {
        trimmed.to_string()
    }
}
