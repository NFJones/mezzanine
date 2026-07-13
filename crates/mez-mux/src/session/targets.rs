//! Session target resolution and shared mutation helpers.
//!
//! Targeting centralizes window and pane lookup by id, index, and name, plus
//! shared helper state updates used by client and window operations.

use crate::{MuxError as MezError, MuxErrorKind, Result};
use mez_core::{PaneId, WindowId};

use super::time::current_unix_seconds;
use super::types::{Session, SessionState};

impl Session {
    /// Runs the record event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn record_event(&mut self) -> u64 {
        let id = self.next_event_id;
        self.next_event_id += 1;
        self.updated_at_unix_seconds = current_unix_seconds();
        id
    }

    /// Runs the window index or active operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn window_index_or_active(&self, target: Option<&str>) -> Result<usize> {
        match target {
            None => {
                if self.windows.is_empty() {
                    Err(MezError::invalid_state("session has no active window"))
                } else {
                    Ok(self.active_window_index)
                }
            }
            Some(target) => self
                .window_index_by_target(target)
                .ok_or_else(|| MezError::new(MuxErrorKind::NotFound, "window not found")),
        }
    }

    /// Runs the window index by target operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn window_index_by_target(&self, target: &str) -> Option<usize> {
        self.window_index_by_id(target)
            .or_else(|| self.window_index_by_index(target))
            .or_else(|| self.window_index_by_name(target))
    }

    /// Runs the window index by id operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn window_index_by_id(&self, target: &str) -> Option<usize> {
        self.windows
            .iter()
            .position(|window| window.id.as_str() == target)
    }

    /// Runs the window index by index operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn window_index_by_index(&self, target: &str) -> Option<usize> {
        let group_index = target.parse::<usize>().ok()?;
        self.active_group_window_indexes()
            .get(group_index)
            .copied()
            .or_else(|| {
                self.windows
                    .iter()
                    .position(|window| window.index.to_string() == target)
            })
    }

    /// Runs the window index by name operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn window_index_by_name(&self, target: &str) -> Option<usize> {
        self.active_group_window_indexes()
            .into_iter()
            .find(|index| self.windows[*index].name == target)
            .or_else(|| self.windows.iter().position(|window| window.name == target))
    }

    /// Returns windows in the active group in user-facing order.
    pub fn active_group_windows(&self) -> Vec<&crate::layout::Window> {
        self.active_group_window_indexes()
            .iter()
            .filter_map(|index| self.windows.get(*index))
            .collect()
    }

    /// Returns the active group's displayed index for a stable window id.
    pub fn active_group_window_display_index(&self, window_id: &WindowId) -> Option<usize> {
        self.active_group()?
            .window_ids
            .iter()
            .position(|candidate| candidate == window_id)
    }

    /// Resolves a window group target by id, display index, name, or alias.
    pub(super) fn group_index_or_active(&self, target: Option<&str>) -> Result<usize> {
        match target {
            None | Some("current") => {
                if self.window_groups.is_empty() {
                    Err(MezError::invalid_state("session has no window groups"))
                } else {
                    Ok(self.active_group_index)
                }
            }
            Some(target) => self
                .group_index_by_target(target)
                .ok_or_else(|| MezError::new(MuxErrorKind::NotFound, "window group not found")),
        }
    }

    /// Resolves a window group target without applying command authorization.
    pub(super) fn group_index_by_target(&self, target: &str) -> Option<usize> {
        match target {
            "next" => (!self.window_groups.is_empty())
                .then_some((self.active_group_index + 1) % self.window_groups.len()),
            "previous" | "prev" => {
                if self.window_groups.is_empty() {
                    None
                } else if self.active_group_index == 0 {
                    Some(self.window_groups.len() - 1)
                } else {
                    Some(self.active_group_index - 1)
                }
            }
            "last" => self
                .last_active_group_index
                .filter(|index| *index < self.window_groups.len()),
            _ => self
                .window_groups
                .iter()
                .position(|group| group.id.as_str() == target)
                .or_else(|| {
                    target.parse::<usize>().ok().and_then(|index| {
                        self.window_groups
                            .iter()
                            .position(|group| group.index == index)
                    })
                })
                .or_else(|| {
                    self.window_groups
                        .iter()
                        .position(|group| group.name == target)
                }),
        }
    }

    /// Returns flat window indexes for the active group.
    pub(super) fn active_group_window_indexes(&self) -> Vec<usize> {
        let Some(group) = self.window_groups.get(self.active_group_index) else {
            return (0..self.windows.len()).collect();
        };
        group
            .window_ids
            .iter()
            .filter_map(|window_id| self.window_index_by_id(window_id.as_str()))
            .collect()
    }

    /// Returns the group index that owns a stable window id.
    pub(super) fn group_index_containing_window_id(&self, window_id: &WindowId) -> Option<usize> {
        self.window_groups.iter().position(|group| {
            group
                .window_ids
                .iter()
                .any(|candidate| candidate == window_id)
        })
    }

    /// Runs the pane location operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn pane_location(&self, target: Option<&str>) -> Result<(usize, usize)> {
        match target {
            None => {
                let window = self
                    .windows
                    .get(self.active_window_index)
                    .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
                Ok((self.active_window_index, window.active_pane_index()))
            }
            Some(target) => {
                if let Some(window) = self.windows.get(self.active_window_index)
                    && let Ok(pane_index) = window.pane_index(Some(target))
                {
                    return Ok((self.active_window_index, pane_index));
                }

                self.windows
                    .iter()
                    .enumerate()
                    .find_map(|(window_index, window)| {
                        window
                            .pane_index_by_id(target)
                            .map(|pane_index| (window_index, pane_index))
                    })
                    .ok_or_else(|| MezError::new(MuxErrorKind::NotFound, "pane not found"))
            }
        }
    }

    /// Runs the join destination operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn join_destination(&self, target: &str) -> Result<JoinDestination> {
        if let Some(window_index) = self.window_index_by_target(target) {
            return Ok(JoinDestination::Window(window_index));
        }

        let (window_index, pane_index) = self.pane_location(Some(target))?;
        Ok(JoinDestination::Pane {
            window_index,
            pane_id: self.windows[window_index].panes()[pane_index].id.clone(),
        })
    }

    /// Runs the adjust join destination operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn adjust_join_destination(
        &self,
        destination: JoinDestination,
        removed_window_index: usize,
        removed_source_window: bool,
    ) -> Result<(usize, Option<String>)> {
        match destination {
            JoinDestination::Window(window_index) => {
                let window_index =
                    adjust_window_index(window_index, removed_window_index, removed_source_window)?;
                Ok((window_index, None))
            }
            JoinDestination::Pane {
                window_index,
                pane_id,
            } => {
                let window_index =
                    adjust_window_index(window_index, removed_window_index, removed_source_window)?;
                Ok((window_index, Some(pane_id.to_string())))
            }
        }
    }

    /// Runs the after window removed operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn after_window_removed(&mut self, removed_index: usize) {
        self.reindex_windows();
        self.reconcile_window_groups_after_window_removal();
        self.last_active_window_index = self
            .last_active_window_index
            .filter(|index| *index < self.windows.len() && *index != removed_index);
        if self.windows.is_empty() {
            self.active_window_index = 0;
            self.window_groups.clear();
            self.active_group_index = 0;
            self.last_active_group_index = None;
            self.state = SessionState::Empty;
        } else if self.active_window_index >= self.windows.len() {
            self.active_window_index = self.windows.len() - 1;
        } else if removed_index <= self.active_window_index && self.active_window_index > 0 {
            self.active_window_index -= 1;
        }
        self.sync_active_group_to_active_window();
    }

    /// Runs the reindex windows operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn reindex_windows(&mut self) {
        for (index, window) in self.windows.iter_mut().enumerate() {
            window.index = index;
        }
    }

    /// Reindexes window groups in displayed order.
    pub(super) fn reindex_window_groups(&mut self) {
        for (index, group) in self.window_groups.iter_mut().enumerate() {
            group.index = index;
        }
    }

    /// Reorders each group's window ids to match the session's flat window
    /// order after a window move. Group membership remains unchanged.
    pub(super) fn sync_group_window_order_with_window_order(&mut self) {
        let ordered_window_ids = self
            .windows
            .iter()
            .map(|window| window.id.clone())
            .collect::<Vec<_>>();
        for group in &mut self.window_groups {
            group.window_ids.sort_by_key(|window_id| {
                ordered_window_ids
                    .iter()
                    .position(|candidate| candidate == window_id)
                    .unwrap_or(usize::MAX)
            });
        }
    }

    /// Runs the set active window index operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn set_active_window_index(&mut self, index: usize) {
        let previous_window_id = self
            .windows
            .get(self.active_window_index)
            .map(|window| window.id.clone());
        let next_window_id = self.windows.get(index).map(|window| window.id.clone());
        if self.active_window_index != index {
            self.last_active_window_index = Some(self.active_window_index);
            self.active_window_index = index;
        }
        if let Some(next_window_id) = next_window_id {
            self.sync_active_group_for_window(next_window_id, previous_window_id);
        }
    }

    /// Selects an active group and focuses that group's active or first window.
    pub(super) fn set_active_group_index(&mut self, index: usize) -> Result<()> {
        if index >= self.window_groups.len() {
            return Err(MezError::new(
                MuxErrorKind::NotFound,
                "window group not found",
            ));
        }
        let window_id = self.window_groups[index]
            .active_window_id
            .clone()
            .or_else(|| self.window_groups[index].window_ids.first().cloned())
            .ok_or_else(|| MezError::invalid_state("window group has no windows"))?;
        let window_index = self
            .window_index_by_id(window_id.as_str())
            .ok_or_else(|| MezError::invalid_state("window group active window is missing"))?;
        if self.active_group_index != index {
            self.last_active_group_index = Some(self.active_group_index);
            self.active_group_index = index;
        }
        self.set_active_window_index(window_index);
        Ok(())
    }

    /// Updates active group state to match the current active window.
    pub(super) fn sync_active_group_to_active_window(&mut self) {
        let Some(window) = self.windows.get(self.active_window_index) else {
            return;
        };
        self.sync_active_group_for_window(window.id.clone(), None);
    }

    /// Updates active group state for a selected window id.
    pub(super) fn sync_active_group_for_window(
        &mut self,
        window_id: WindowId,
        previous_window_id: Option<WindowId>,
    ) {
        let Some(group_index) = self.group_index_containing_window_id(&window_id) else {
            return;
        };
        if self.active_group_index != group_index {
            self.last_active_group_index = Some(self.active_group_index);
            self.active_group_index = group_index;
        }
        if let Some(group) = self.window_groups.get_mut(group_index)
            && group.active_window_id.as_ref() != Some(&window_id)
        {
            group.last_active_window_id = group.active_window_id.clone().or(previous_window_id);
            group.active_window_id = Some(window_id);
        }
    }

    /// Removes stale window references and empty groups after flat window removal.
    pub(super) fn reconcile_window_groups_after_window_removal(&mut self) {
        let live_window_ids = self
            .windows
            .iter()
            .map(|window| window.id.clone())
            .collect::<Vec<_>>();
        for group in &mut self.window_groups {
            group
                .window_ids
                .retain(|window_id| live_window_ids.iter().any(|live| live == window_id));
            if !group
                .active_window_id
                .as_ref()
                .is_some_and(|window_id| group.window_ids.iter().any(|id| id == window_id))
            {
                group.active_window_id = group.window_ids.first().cloned();
            }
            if !group
                .last_active_window_id
                .as_ref()
                .is_some_and(|window_id| group.window_ids.iter().any(|id| id == window_id))
            {
                group.last_active_window_id = None;
            }
        }
        self.window_groups
            .retain(|group| !group.window_ids.is_empty());
        self.reindex_window_groups();
        if self.active_group_index >= self.window_groups.len() {
            self.active_group_index = self.window_groups.len().saturating_sub(1);
        }
        self.last_active_group_index = self
            .last_active_group_index
            .filter(|index| *index < self.window_groups.len());
    }
}

/// Carries Join Destination state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum JoinDestination {
    /// Represents the Window case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Window(usize),
    /// Represents the Pane case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Pane {
        /// Stores the window index value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        window_index: usize,
        /// Stores the pane id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        pane_id: PaneId,
    },
}

/// Runs the adjust window index operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn adjust_window_index(
    window_index: usize,
    removed_window_index: usize,
    removed_source_window: bool,
) -> Result<usize> {
    if !removed_source_window {
        return Ok(window_index);
    }
    if window_index == removed_window_index {
        return Err(MezError::invalid_state("destination window was removed"));
    }
    if window_index > removed_window_index {
        Ok(window_index - 1)
    } else {
        Ok(window_index)
    }
}
