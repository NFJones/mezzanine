//! Window mutation and pane-management operations.
//!
//! This module owns active-pane bookkeeping, split/remove behavior, resizing,
//! zoom state, and layout policy application for a single window.

use super::{
    IdFactory, LayoutNode, LayoutPolicy, MIN_PANE_COLUMNS, MIN_PANE_ROWS, MezError, Pane,
    PaneGeometry, PaneId, PaneNavigationDirection, PaneSizeSpec, PaneTitleSource, ResizeDirection,
    RestoredWindowLayout, Result, Size, SplitDirection, Window, WindowId, WindowNameSource,
    even_grid_dimensions, pane_matches_target, percent_size_for_axis, range_overlap_u16,
    split_dimension_evenly, split_size, split_size_with_spec,
};

impl Window {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(ids: &mut IdFactory, index: usize, name: impl Into<String>, size: Size) -> Self {
        let pane = Pane {
            id: ids.pane(),
            index: 0,
            title: "shell".to_string(),
            title_source: PaneTitleSource::Default,
            size,
            active: true,
            live: true,
        };
        let name = name.into();

        Self {
            id: ids.window(),
            index,
            name_source: window_name_source_for_created_window(index, &name),
            name,
            created_at_unix_seconds: None,
            size,
            panes: vec![pane],
            active_pane_index: 0,
            last_active_pane_index: None,
            zoomed_pane_id: None,
            layout_policy: LayoutPolicy::Tiled,
            layout_root: LayoutNode::single_pane(0),
            pane_geometries: vec![PaneGeometry {
                index: 0,
                column: 0,
                row: 0,
                columns: size.columns,
                rows: size.rows,
            }],
        }
    }

    /// Runs the panes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn panes(&self) -> &[Pane] {
        &self.panes
    }

    /// Runs the panes mut operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn panes_mut(&mut self) -> &mut [Pane] {
        &mut self.panes
    }

    /// Runs the active pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn active_pane(&self) -> &Pane {
        &self.panes[self.active_pane_index]
    }

    /// Runs the active pane index operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn active_pane_index(&self) -> usize {
        self.active_pane_index
    }

    /// Runs the last active pane index operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn last_active_pane_index(&self) -> Option<usize> {
        self.last_active_pane_index
    }

    /// Runs the zoomed pane id operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn zoomed_pane_id(&self) -> Option<&PaneId> {
        self.zoomed_pane_id.as_ref()
    }

    /// Runs the layout policy operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn layout_policy(&self) -> LayoutPolicy {
        self.layout_policy
    }

    /// Returns the stored recursive split tree for this window.
    pub fn layout_root(&self) -> &LayoutNode {
        &self.layout_root
    }

    /// Returns the currently stored pane rectangles for this window.
    pub fn pane_geometries(&self) -> Vec<PaneGeometry> {
        self.pane_geometries.clone()
    }

    /// Returns pane rectangles reapportioned to a caller-supplied render area.
    ///
    /// The stored split tree is preserved while pane sizes are cloned and scaled
    /// to the supplied size. This lets terminal rendering reserve rows for
    /// mux-managed window frames without mutating the authoritative session
    /// geometry used by protocol state and layout commands.
    pub fn pane_geometries_for_size(&self, size: Size) -> Vec<PaneGeometry> {
        let mut panes = self.panes.clone();
        self.layout_root.resize_panes(&mut panes, size);
        self.layout_root.pane_geometries(&panes)
    }

    /// Returns the displayed title for this window.
    ///
    /// Numeric default window names are treated as generated identities, so the
    /// visible title follows the active pane title until the user explicitly
    /// renames the window to a non-default value.
    pub fn title(&self) -> String {
        if self.name_source.is_explicit() {
            self.name.clone()
        } else if self.name.trim().is_empty()
            || self.name == self.index.to_string()
            || self.name == "shell"
        {
            self.active_pane().title.clone()
        } else {
            self.name.clone()
        }
    }

    /// Runs the rename operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn rename(&mut self, name: impl Into<String>) {
        self.rename_explicit(name);
    }

    /// Assigns a user- or agent-authored name to this window.
    pub fn rename_explicit(&mut self, name: impl Into<String>) {
        self.name = name.into();
        self.name_source = WindowNameSource::Explicit;
    }

    /// Assigns a runtime-generated name to this window.
    pub fn rename_generated(&mut self, name: impl Into<String>) {
        self.name = name.into();
        self.name_source = WindowNameSource::Generated;
    }

    /// Marks the current name as runtime-generated without changing it.
    pub fn mark_name_generated(&mut self) {
        self.name_source = WindowNameSource::Generated;
    }

    /// Returns whether the current name was explicitly assigned.
    pub fn has_explicit_name(&self) -> bool {
        self.name_source.is_explicit()
    }

    /// Runs the split active operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn split_active(
        &mut self,
        ids: &mut IdFactory,
        direction: SplitDirection,
    ) -> Result<&Pane> {
        self.split_active_select(ids, direction, true)
    }

    /// Splits the active pane and optionally selects the newly created pane.
    ///
    /// Command paths use `select_new = true` for the default behavior that the
    /// new pane receives focus. Detached/no-select creation paths pass `false`
    /// when they need to retain focus on the spawning pane.
    pub fn split_active_select(
        &mut self,
        ids: &mut IdFactory,
        direction: SplitDirection,
        select_new: bool,
    ) -> Result<&Pane> {
        self.split_active_with_requested_size(ids, direction, None, select_new)
    }

    /// Splits the active pane while assigning a requested size to the new pane.
    ///
    /// The requested size must fit within the split axis and preserve the
    /// cross-axis dimension so that the resulting split tree has no gaps or
    /// overlapping pane rectangles.
    pub fn split_active_with_size_spec(
        &mut self,
        ids: &mut IdFactory,
        direction: SplitDirection,
        requested_size: PaneSizeSpec,
    ) -> Result<&Pane> {
        self.split_active_with_size_spec_select(ids, direction, requested_size, true)
    }

    /// Splits the active pane with a requested size and optional new-pane focus.
    pub fn split_active_with_size_spec_select(
        &mut self,
        ids: &mut IdFactory,
        direction: SplitDirection,
        requested_size: PaneSizeSpec,
        select_new: bool,
    ) -> Result<&Pane> {
        self.split_active_with_requested_size(ids, direction, Some(requested_size), select_new)
    }

    /// Runs the split active with requested size operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn split_active_with_requested_size(
        &mut self,
        ids: &mut IdFactory,
        direction: SplitDirection,
        requested_size: Option<PaneSizeSpec>,
        select_new: bool,
    ) -> Result<&Pane> {
        let active_index = self.active_pane_index;
        let original_size = self.panes[active_index].size;
        let (existing, created) = match requested_size {
            Some(requested_size) => split_size_with_spec(original_size, direction, requested_size)?,
            None => split_size(original_size, direction)?,
        };

        self.panes[active_index].size = existing;
        let new_index = active_index + 1;
        let pane = Pane {
            id: ids.pane(),
            index: new_index,
            title: "shell".to_string(),
            title_source: PaneTitleSource::Default,
            size: created,
            active: false,
            live: true,
        };
        self.panes.insert(new_index, pane);
        self.reindex_panes();
        self.layout_root
            .split_pane(active_index, new_index, direction)?;

        if select_new {
            self.set_active_pane_index(new_index);
        } else {
            self.panes[active_index].active = true;
            self.panes[new_index].active = false;
        }
        self.rebalance_after_pane_count_change();

        Ok(&self.panes[new_index])
    }

    /// Runs the select adjacent pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn select_adjacent_pane(&mut self, direction: PaneNavigationDirection) -> Result<()> {
        if self.panes.is_empty() {
            return Err(MezError::invalid_state("window has no panes"));
        }
        let rects = self.pane_rects();
        let Some(active) = rects
            .iter()
            .find(|rect| rect.index == self.active_pane_index)
        else {
            return Ok(());
        };
        let direct_candidates = rects
            .iter()
            .filter(|rect| rect.index != self.active_pane_index)
            .filter_map(|rect| directional_candidate(active, rect, direction))
            .collect::<Vec<_>>();
        let nearest_direct_distance = direct_candidates
            .iter()
            .map(|candidate| candidate.distance)
            .min();
        let backtrack = self
            .last_active_pane_index
            .filter(|last_active_index| *last_active_index != self.active_pane_index)
            .and_then(|last_active_index| {
                direct_candidates
                    .iter()
                    .find(|candidate| candidate.index == last_active_index)
            })
            .filter(|candidate| Some(candidate.distance) == nearest_direct_distance)
            .map(|candidate| candidate.index);
        let next = backtrack
            .or_else(|| {
                direct_candidates
                    .iter()
                    .min()
                    .map(|candidate| candidate.index)
            })
            .or_else(|| {
                rects
                    .iter()
                    .filter(|rect| rect.index != self.active_pane_index)
                    .filter_map(|rect| wrapped_directional_candidate(active, rect, direction))
                    .min()
                    .map(|candidate| candidate.index)
            });
        if let Some(next) = next {
            self.set_active_pane_index(next);
        }
        Ok(())
    }

    /// Runs the select last pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn select_last_pane(&mut self) -> Result<()> {
        let index = self
            .last_active_pane_index
            .filter(|index| *index < self.panes.len())
            .ok_or_else(|| MezError::invalid_state("window has no last active pane"))?;
        self.set_active_pane_index(index);
        Ok(())
    }

    /// Runs the toggle zoom active operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn toggle_zoom_active(&mut self) -> Option<&PaneId> {
        let active_id = self.panes[self.active_pane_index].id.clone();
        if self.zoomed_pane_id.as_ref() == Some(&active_id) {
            self.zoomed_pane_id = None;
        } else {
            self.zoomed_pane_id = Some(active_id);
        }
        self.zoomed_pane_id.as_ref()
    }

    /// Runs the rotate panes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn rotate_panes(&mut self, reverse: bool) {
        if self.panes.len() < 2 {
            return;
        }

        let slots = self.panes.clone();
        let mut rotated = self.panes.clone();
        if reverse {
            rotated.rotate_right(1);
        } else {
            rotated.rotate_left(1);
        }

        for (index, (slot, pane)) in slots.iter().zip(rotated.iter_mut()).enumerate() {
            pane.index = index;
            pane.size = slot.size;
            pane.active = slot.active;
        }
        self.panes = rotated;
        self.active_pane_index = self
            .panes
            .iter()
            .position(|pane| pane.active)
            .unwrap_or(self.active_pane_index.min(self.panes.len() - 1));
        self.retain_valid_zoom();
        self.refresh_pane_geometries();
    }

    /// Runs the cycle layout operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn cycle_layout(&mut self) -> LayoutPolicy {
        self.layout_policy = self.layout_policy.next();
        self.apply_layout_policy();
        self.layout_policy
    }

    /// Runs the set layout policy operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn set_layout_policy(&mut self, policy: LayoutPolicy) -> LayoutPolicy {
        self.layout_policy = policy;
        self.apply_layout_policy();
        self.layout_policy
    }

    /// Resizes the owning window and reapportions pane sizes through the stored split tree.
    pub fn resize_window(&mut self, size: Size) -> Result<()> {
        if size.columns == 0 || size.rows == 0 {
            return Err(MezError::invalid_args(
                "window size must be positive non-zero cells",
            ));
        }
        let minimum = self.layout_root.minimum_size();
        if size.columns < minimum.columns || size.rows < minimum.rows {
            return Err(MezError::invalid_args(format!(
                "window size {}x{} is smaller than layout minimum {}x{}",
                size.columns, size.rows, minimum.columns, minimum.rows
            )));
        }
        self.size = size;
        if self.layout_policy_rebalances() {
            self.apply_layout_policy();
        } else {
            self.layout_root.resize_panes(&mut self.panes, size);
            self.refresh_pane_geometries();
        }
        Ok(())
    }

    /// Runs the select pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn select_pane(&mut self, target: &str) -> Result<()> {
        let index = self.pane_index(Some(target))?;
        self.set_active_pane_index(index);
        Ok(())
    }

    /// Runs the pane index operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn pane_index(&self, target: Option<&str>) -> Result<usize> {
        match target {
            Some(target) => self
                .panes
                .iter()
                .position(|pane| pane_matches_target(pane, target))
                .ok_or_else(|| {
                    MezError::new(crate::error::MezErrorKind::NotFound, "pane not found")
                }),
            None => {
                if self.panes.is_empty() {
                    Err(MezError::invalid_state("window has no active pane"))
                } else {
                    Ok(self.active_pane_index)
                }
            }
        }
    }

    /// Runs the pane index by id operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn pane_index_by_id(&self, pane_id: &str) -> Option<usize> {
        self.panes
            .iter()
            .position(|pane| pane.id.as_str() == pane_id)
    }

    /// Runs the swap panes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn swap_panes(&mut self, first: Option<&str>, second: &str) -> Result<()> {
        let first_index = self.pane_index(first)?;
        let second_index = self.pane_index(Some(second))?;
        self.swap_pane_indices(first_index, second_index);
        Ok(())
    }

    /// Runs the take pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn take_pane(&mut self, target: Option<&str>) -> Result<Pane> {
        let index = self.pane_index(target)?;
        Ok(self.take_pane_at(index))
    }

    /// Runs the take pane at operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn take_pane_at(&mut self, index: usize) -> Pane {
        let removed = self.panes.remove(index);
        if self.panes.is_empty() {
            self.active_pane_index = 0;
            self.last_active_pane_index = None;
            self.zoomed_pane_id = None;
            return removed;
        }

        let next_active = if removed.active {
            index.min(self.panes.len() - 1)
        } else if self.active_pane_index > index {
            self.active_pane_index - 1
        } else {
            self.active_pane_index.min(self.panes.len() - 1)
        };
        self.reindex_panes();
        self.reflow_after_pane_removal(index, removed.size);
        self.set_active_pane_index(next_active);
        self.retain_valid_zoom();
        removed
    }

    /// Runs the insert existing after operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn insert_existing_after(
        &mut self,
        target: Option<&str>,
        mut pane: Pane,
        direction: SplitDirection,
        select_inserted: bool,
    ) -> Result<&Pane> {
        let target_index = self.pane_index(target)?;
        let active_pane_id = self.panes[self.active_pane_index].id.clone();
        let (existing, inserted) = split_size(self.panes[target_index].size, direction)?;

        self.panes[target_index].size = existing;
        let inserted_index = target_index + 1;
        pane.index = inserted_index;
        pane.size = inserted;
        pane.active = false;
        self.panes.insert(inserted_index, pane);
        self.reindex_panes();
        self.layout_root
            .split_pane(target_index, inserted_index, direction)?;

        if select_inserted {
            self.set_active_pane_index(inserted_index);
        } else if let Some(active_index) =
            self.panes.iter().position(|pane| pane.id == active_pane_id)
        {
            self.set_active_pane_index(active_index);
        }
        self.rebalance_after_pane_count_change();

        Ok(&self.panes[inserted_index])
    }

    /// Runs the from existing pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn from_existing_pane(
        ids: &mut IdFactory,
        index: usize,
        name: impl Into<String>,
        size: Size,
        mut pane: Pane,
    ) -> Self {
        pane.index = 0;
        pane.size = size;
        pane.active = true;

        let name = name.into();
        Self {
            id: ids.window(),
            index,
            name_source: window_name_source_for_created_window(index, &name),
            name,
            created_at_unix_seconds: None,
            size,
            panes: vec![pane],
            active_pane_index: 0,
            last_active_pane_index: None,
            zoomed_pane_id: None,
            layout_policy: LayoutPolicy::Tiled,
            layout_root: LayoutNode::single_pane(0),
            pane_geometries: vec![PaneGeometry {
                index: 0,
                column: 0,
                row: 0,
                columns: size.columns,
                rows: size.rows,
            }],
        }
    }

    /// Runs the from restored parts operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn from_restored_parts(
        id: WindowId,
        index: usize,
        name: impl Into<String>,
        size: Size,
        panes: Vec<Pane>,
    ) -> Result<Self> {
        Self::from_restored_parts_with_geometries(
            id,
            index,
            name,
            size,
            panes,
            None,
            LayoutPolicy::Tiled,
        )
    }

    /// Reconstructs a window from snapshot-owned panes and optional pane rectangles.
    ///
    /// Supplied rectangles must align with restored pane order, match pane sizes,
    /// fit inside the restored window, and avoid overlap. Snapshots that do not
    /// contain complete rectangle metadata fall back to the deterministic layout
    /// inference used for legacy payloads. The supplied layout policy is
    /// retained as the active policy for later layout cycling and mutations.
    pub fn from_restored_parts_with_geometries(
        id: WindowId,
        index: usize,
        name: impl Into<String>,
        size: Size,
        panes: Vec<Pane>,
        pane_geometries: Option<Vec<PaneGeometry>>,
        layout_policy: LayoutPolicy,
    ) -> Result<Self> {
        Self::from_restored_parts_with_layout(
            id,
            index,
            name,
            size,
            panes,
            RestoredWindowLayout {
                pane_geometries,
                layout_root: None,
                layout_policy,
            },
        )
    }

    /// Reconstructs a window from snapshot panes, rectangles, and a split tree.
    ///
    /// Complete snapshot rectangles are validated against pane dimensions before
    /// the optional layout tree is applied. When the tree is present it must
    /// reference each restored pane once and reproduce the restored rectangles.
    /// Legacy payloads without a tree build a tree from restored rectangles or,
    /// when those are absent, from the old deterministic rectangle inference.
    pub(crate) fn from_restored_parts_with_layout(
        id: WindowId,
        index: usize,
        name: impl Into<String>,
        size: Size,
        mut panes: Vec<Pane>,
        layout: RestoredWindowLayout,
    ) -> Result<Self> {
        if panes.is_empty() {
            return Err(MezError::invalid_args(
                "restored window must contain at least one pane",
            ));
        }
        let active_indices = panes
            .iter()
            .enumerate()
            .filter_map(|(index, pane)| pane.active.then_some(index))
            .collect::<Vec<_>>();
        if active_indices.len() != 1 {
            return Err(MezError::invalid_args(
                "restored window must contain exactly one active pane",
            ));
        }
        for (pane_index, pane) in panes.iter_mut().enumerate() {
            pane.index = pane_index;
        }
        let active_pane_index = active_indices[0];
        let validated_pane_geometries = layout
            .pane_geometries
            .map(|geometries| validated_pane_geometries(size, &panes, geometries))
            .transpose()?;
        let layout_root = match layout.layout_root {
            Some(layout_root) => {
                layout_root.validate_pane_indices(panes.len())?;
                layout_root
            }
            None => restored_layout_root(size, &panes, validated_pane_geometries.as_deref())?,
        };

        let name = name.into();
        let mut window = Self {
            id,
            index,
            name_source: window_name_source_for_created_window(index, &name),
            name,
            created_at_unix_seconds: None,
            size,
            panes,
            active_pane_index,
            last_active_pane_index: None,
            zoomed_pane_id: None,
            layout_policy: layout.layout_policy,
            layout_root,
            pane_geometries: Vec::new(),
        };
        window.refresh_pane_geometries();
        if let Some(expected_geometries) = validated_pane_geometries
            && window.pane_geometries != expected_geometries
        {
            return Err(MezError::invalid_args(
                "restored layout tree must match restored pane geometries",
            ));
        }
        Ok(window)
    }

    /// Runs the swap panes between operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn swap_panes_between(
        first: &mut Window,
        first_index: usize,
        second: &mut Window,
        second_index: usize,
    ) {
        let first_slot = first.panes[first_index].clone();
        let second_slot = second.panes[second_index].clone();

        std::mem::swap(
            &mut first.panes[first_index],
            &mut second.panes[second_index],
        );

        first.panes[first_index].index = first_slot.index;
        first.panes[first_index].size = first_slot.size;
        first.panes[first_index].active = first_slot.active;
        second.panes[second_index].index = second_slot.index;
        second.panes[second_index].size = second_slot.size;
        second.panes[second_index].active = second_slot.active;

        first.reindex_panes();
        second.reindex_panes();
        first.refresh_pane_geometries();
        second.refresh_pane_geometries();
    }

    /// Runs the kill pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn kill_pane(&mut self, target: Option<&str>) -> Result<Pane> {
        let index = match target {
            Some(target) => self
                .panes
                .iter()
                .position(|pane| pane_matches_target(pane, target))
                .ok_or_else(|| {
                    MezError::new(crate::error::MezErrorKind::NotFound, "pane not found")
                })?,
            None => self.active_pane_index,
        };

        if self.panes.len() == 1 {
            return Err(MezError::invalid_state(
                "cannot remove the final pane through Window::kill_pane",
            ));
        }

        let active_pane_id = self.panes[self.active_pane_index].id.clone();
        let removed = self.panes.remove(index);

        let next_active = if removed.active {
            index.min(self.panes.len() - 1)
        } else {
            self.panes
                .iter()
                .position(|pane| pane.id == active_pane_id)
                .unwrap_or_else(|| index.min(self.panes.len() - 1))
        };
        self.reindex_panes();
        self.reflow_after_pane_removal(index, removed.size);
        self.set_active_pane_index(next_active);
        self.retain_valid_zoom();
        Ok(removed)
    }

    /// Runs the resize active pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn resize_active_pane(&mut self, size: Size) -> Result<()> {
        if size.columns < MIN_PANE_COLUMNS || size.rows < MIN_PANE_ROWS {
            return Err(MezError::invalid_args(
                "pane size is below the minimum pane dimensions",
            ));
        }
        self.panes[self.active_pane_index].size = size;
        self.refresh_pane_geometries();
        Ok(())
    }

    /// Runs the resize pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn resize_pane(&mut self, target: Option<&str>, size: Size) -> Result<&Pane> {
        let index = match target {
            Some(target) => self
                .panes
                .iter()
                .position(|pane| pane_matches_target(pane, target))
                .ok_or_else(|| {
                    MezError::new(crate::error::MezErrorKind::NotFound, "pane not found")
                })?,
            None => self.active_pane_index,
        };
        self.resize_pane_at_index(index, size)
    }

    /// Resizes a pane by resolving a spec-defined size request.
    pub fn resize_pane_with_spec(
        &mut self,
        target: Option<&str>,
        spec: PaneSizeSpec,
    ) -> Result<&Pane> {
        let index = self.pane_index(target)?;
        let size = self.size_from_spec(index, spec)?;
        self.resize_pane_at_index(index, size)
    }

    /// Resolves a spec-defined size request without mutating pane state.
    pub fn resolve_pane_size_spec(&self, target: Option<&str>, spec: PaneSizeSpec) -> Result<Size> {
        let index = self.pane_index(target)?;
        self.size_from_spec(index, spec)
    }

    /// Runs the resize pane at index operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn resize_pane_at_index(&mut self, index: usize, size: Size) -> Result<&Pane> {
        if size.columns < MIN_PANE_COLUMNS || size.rows < MIN_PANE_ROWS {
            return Err(MezError::invalid_args(
                "pane size is below the minimum pane dimensions",
            ));
        }
        self.panes[index].size = size;
        self.refresh_pane_geometries();
        Ok(&self.panes[index])
    }

    /// Replaces pane geometry with a validated rendered layout.
    ///
    /// This is used by pointer-driven border resize, where the user moves one
    /// rendered split edge and expects only the panes attached to that edge to
    /// change. The replacement geometries become the new split-tree weights for
    /// subsequent terminal resizes.
    pub fn replace_pane_geometries(&mut self, geometries: Vec<PaneGeometry>) -> Result<()> {
        validate_replacement_pane_geometries(self.size, self.panes.len(), &geometries)?;
        let previous_sizes = self.panes.iter().map(|pane| pane.size).collect::<Vec<_>>();
        for geometry in &geometries {
            if let Some(pane) = self.panes.get_mut(geometry.index) {
                pane.size = Size {
                    columns: geometry.columns,
                    rows: geometry.rows,
                };
            }
        }
        if self.layout_root.pane_geometries(&self.panes) == geometries {
            self.pane_geometries = geometries;
            return Ok(());
        }
        for (pane, size) in self.panes.iter_mut().zip(previous_sizes) {
            pane.size = size;
        }

        let layout_root = LayoutNode::from_geometries(&geometries)?;
        layout_root.validate_pane_indices(self.panes.len())?;
        for geometry in &geometries {
            if let Some(pane) = self.panes.get_mut(geometry.index) {
                pane.size = Size {
                    columns: geometry.columns,
                    rows: geometry.rows,
                };
            }
        }
        self.layout_root = layout_root;
        self.pane_geometries = geometries;
        Ok(())
    }

    /// Runs the size from spec operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn size_from_spec(&self, index: usize, spec: PaneSizeSpec) -> Result<Size> {
        let current = self
            .panes
            .get(index)
            .ok_or_else(|| MezError::invalid_state("pane index is outside the window"))?
            .size;
        match spec {
            PaneSizeSpec::Cells { columns, rows } => {
                if columns.is_none() && rows.is_none() {
                    return Err(MezError::invalid_args(
                        "cells resize requires columns or rows",
                    ));
                }
                Size::new(
                    columns.unwrap_or(current.columns),
                    rows.unwrap_or(current.rows),
                )
            }
            PaneSizeSpec::Percent { percent, axis } => percent_size_for_axis(
                self.size,
                current,
                percent,
                axis,
                "percent resize requires a positive percent",
                "percent resize",
            ),
            PaneSizeSpec::Delta { direction, amount }
            | PaneSizeSpec::Edge {
                edge: direction,
                amount,
            } => size_from_direction(current, direction, amount),
        }
    }

    /// Runs the set active pane index operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn set_active_pane_index(&mut self, index: usize) {
        if self.active_pane_index != index && self.active_pane_index < self.panes.len() {
            self.last_active_pane_index = Some(self.active_pane_index);
        }
        self.active_pane_index = index;
        for (pane_index, pane) in self.panes.iter_mut().enumerate() {
            pane.active = pane_index == index;
        }
    }

    /// Runs the swap pane indices operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn swap_pane_indices(&mut self, first_index: usize, second_index: usize) {
        if first_index == second_index {
            return;
        }

        let first_slot = self.panes[first_index].clone();
        let second_slot = self.panes[second_index].clone();
        self.panes.swap(first_index, second_index);

        self.panes[first_index].index = first_slot.index;
        self.panes[first_index].size = first_slot.size;
        self.panes[first_index].active = first_slot.active;
        self.panes[second_index].index = second_slot.index;
        self.panes[second_index].size = second_slot.size;
        self.panes[second_index].active = second_slot.active;
        self.active_pane_index = self
            .panes
            .iter()
            .position(|pane| pane.active)
            .unwrap_or(self.active_pane_index.min(self.panes.len() - 1));
        self.retain_valid_zoom();
    }

    /// Runs the reindex panes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn reindex_panes(&mut self) {
        for (index, pane) in self.panes.iter_mut().enumerate() {
            pane.index = index;
        }
    }

    /// Runs the apply layout policy operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn apply_layout_policy(&mut self) {
        if self.panes.is_empty() {
            return;
        }

        match self.layout_policy {
            LayoutPolicy::Tiled | LayoutPolicy::EvenVertical => {
                let widths = split_dimension_evenly(self.size.columns, self.panes.len());
                for (pane, columns) in self.panes.iter_mut().zip(widths) {
                    pane.size = Size {
                        columns,
                        rows: self.size.rows,
                    };
                }
            }
            LayoutPolicy::EvenHorizontal => {
                let heights = split_dimension_evenly(self.size.rows, self.panes.len());
                for (pane, rows) in self.panes.iter_mut().zip(heights) {
                    pane.size = Size {
                        columns: self.size.columns,
                        rows,
                    };
                }
            }
            LayoutPolicy::EvenGrid => {
                self.apply_even_grid_layout();
                return;
            }
        }
        self.layout_root = match self.layout_policy {
            LayoutPolicy::Tiled | LayoutPolicy::EvenVertical => {
                LayoutNode::flat(SplitDirection::Vertical, self.panes.len())
            }
            LayoutPolicy::EvenHorizontal => {
                LayoutNode::flat(SplitDirection::Horizontal, self.panes.len())
            }
            LayoutPolicy::EvenGrid => unreachable!("even-grid layout returns after applying"),
        };
        self.refresh_pane_geometries();
    }

    /// Applies the self-rebalancing grid layout to the current panes.
    ///
    /// The grid is row-major so pane order remains stable for navigation and
    /// snapshots. Rows and columns are both apportioned evenly, with the final
    /// row allowed to contain fewer panes when the pane count is not rectangular.
    fn apply_even_grid_layout(&mut self) {
        let pane_count = self.panes.len();
        let (column_count, row_count) = even_grid_dimensions(self.size, pane_count);
        let row_heights = split_dimension_evenly(self.size.rows, row_count);
        let mut pane_index = 0;
        let mut row_nodes = Vec::with_capacity(row_count);

        for (row_index, rows) in row_heights.into_iter().enumerate() {
            if pane_index >= pane_count {
                break;
            }
            let rows_remaining = row_count.saturating_sub(row_index).max(1);
            let panes_remaining = pane_count.saturating_sub(pane_index);
            let panes_this_row = panes_remaining.div_ceil(rows_remaining).min(column_count);
            let widths = split_dimension_evenly(self.size.columns, panes_this_row);
            let mut children = Vec::with_capacity(panes_this_row);

            for columns in widths {
                self.panes[pane_index].size = Size { columns, rows };
                children.push(LayoutNode::single_pane(pane_index));
                pane_index += 1;
            }

            row_nodes.push(if children.len() == 1 {
                children.remove(0)
            } else {
                LayoutNode::Split {
                    direction: SplitDirection::Vertical,
                    children,
                }
            });
        }

        self.layout_root = if row_nodes.len() == 1 {
            row_nodes.remove(0)
        } else {
            LayoutNode::Split {
                direction: SplitDirection::Horizontal,
                children: row_nodes,
            }
        };
        self.refresh_pane_geometries();
    }

    /// Returns whether pane count changes should immediately rebalance sizes.
    fn layout_policy_rebalances(&self) -> bool {
        matches!(
            self.layout_policy,
            LayoutPolicy::EvenVertical | LayoutPolicy::EvenHorizontal | LayoutPolicy::EvenGrid
        )
    }

    /// Reapplies even layout policies after pane insertion or removal.
    ///
    /// Tiled layout intentionally preserves the user's split tree. Even layout
    /// modes are an explicit request for uniform pane sizing, so pane count
    /// changes rebuild their flat layout instead of inheriting a neighbor's
    /// previous proportion.
    fn rebalance_after_pane_count_change(&mut self) {
        if self.layout_policy_rebalances() {
            self.apply_layout_policy();
        } else {
            self.refresh_pane_geometries();
        }
    }

    /// Runs the retain valid zoom operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn retain_valid_zoom(&mut self) {
        if let Some(zoomed) = &self.zoomed_pane_id
            && !self.panes.iter().any(|pane| &pane.id == zoomed)
        {
            self.zoomed_pane_id = None;
        }
    }

    /// Runs the reflow after pane removal operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn reflow_after_pane_removal(&mut self, removed_index: usize, removed_size: Size) {
        if self.layout_policy_rebalances() {
            self.apply_layout_policy();
            return;
        }
        if !self
            .layout_root
            .remove_pane(removed_index, removed_size, &mut self.panes)
        {
            self.layout_root.resize_panes(&mut self.panes, self.size);
        }
        self.refresh_pane_geometries();
    }

    /// Runs the pane rects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn pane_rects(&self) -> Vec<PaneRect> {
        self.pane_geometries
            .iter()
            .copied()
            .map(PaneRect::from)
            .collect()
    }

    /// Runs the refresh pane geometries operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn refresh_pane_geometries(&mut self) {
        self.pane_geometries = self.layout_root.pane_geometries(&self.panes);
    }
}

/// Carries Pane Rect state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PaneRect {
    /// Stores the index value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    index: usize,
    /// Stores the column value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    column: u16,
    /// Stores the row value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    row: u16,
    /// Stores the columns value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    columns: u16,
    /// Stores the rows value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    rows: u16,
}

impl From<PaneRect> for PaneGeometry {
    /// Runs the from operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn from(rect: PaneRect) -> Self {
        Self {
            index: rect.index,
            column: rect.column,
            row: rect.row,
            columns: rect.columns,
            rows: rect.rows,
        }
    }
}

impl From<PaneGeometry> for PaneRect {
    /// Runs the from operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn from(geometry: PaneGeometry) -> Self {
        Self {
            index: geometry.index,
            column: geometry.column,
            row: geometry.row,
            columns: geometry.columns,
            rows: geometry.rows,
        }
    }
}

impl PaneRect {
    /// Runs the right operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn right(self) -> u16 {
        self.column.saturating_add(self.columns)
    }

    /// Runs the bottom operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn bottom(self) -> u16 {
        self.row.saturating_add(self.rows)
    }

    /// Runs the center column twice operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn center_column_twice(self) -> u32 {
        u32::from(self.column)
            .saturating_mul(2)
            .saturating_add(u32::from(self.columns))
    }

    /// Runs the center row twice operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn center_row_twice(self) -> u32 {
        u32::from(self.row)
            .saturating_mul(2)
            .saturating_add(u32::from(self.rows))
    }
}

/// Carries Directional Candidate state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct DirectionalCandidate {
    /// Stores the distance value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    distance: u16,
    /// Stores the center delta value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    center_delta: u32,
    /// Stores the index value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    index: usize,
}

/// Carries Wrapped Directional Candidate state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct WrappedDirectionalCandidate {
    /// Stores the overlap rank value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    overlap_rank: u8,
    /// Stores the edge distance value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    edge_distance: u16,
    /// Stores the center delta value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    center_delta: u32,
    /// Stores the index value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    index: usize,
}

/// Runs the directional candidate operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn directional_candidate(
    active: &PaneRect,
    candidate: &PaneRect,
    direction: PaneNavigationDirection,
) -> Option<DirectionalCandidate> {
    let (in_direction, overlap, distance, center_delta) = match direction {
        PaneNavigationDirection::Up => (
            candidate.bottom() <= active.row,
            range_overlap_u16(
                candidate.column,
                candidate.right(),
                active.column,
                active.right(),
            ),
            active.row.saturating_sub(candidate.bottom()),
            abs_delta(
                candidate.center_column_twice(),
                active.center_column_twice(),
            ),
        ),
        PaneNavigationDirection::Down => (
            candidate.row >= active.bottom(),
            range_overlap_u16(
                candidate.column,
                candidate.right(),
                active.column,
                active.right(),
            ),
            candidate.row.saturating_sub(active.bottom()),
            abs_delta(
                candidate.center_column_twice(),
                active.center_column_twice(),
            ),
        ),
        PaneNavigationDirection::Left => (
            candidate.right() <= active.column,
            range_overlap_u16(
                candidate.row,
                candidate.bottom(),
                active.row,
                active.bottom(),
            ),
            active.column.saturating_sub(candidate.right()),
            abs_delta(candidate.center_row_twice(), active.center_row_twice()),
        ),
        PaneNavigationDirection::Right => (
            candidate.column >= active.right(),
            range_overlap_u16(
                candidate.row,
                candidate.bottom(),
                active.row,
                active.bottom(),
            ),
            candidate.column.saturating_sub(active.right()),
            abs_delta(candidate.center_row_twice(), active.center_row_twice()),
        ),
    };
    if in_direction && overlap > 0 {
        Some(DirectionalCandidate {
            distance,
            center_delta,
            index: candidate.index,
        })
    } else {
        None
    }
}

/// Runs the wrapped directional candidate operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn wrapped_directional_candidate(
    active: &PaneRect,
    candidate: &PaneRect,
    direction: PaneNavigationDirection,
) -> Option<WrappedDirectionalCandidate> {
    let (overlap, edge_distance, center_delta) = match direction {
        PaneNavigationDirection::Up => (
            range_overlap_u16(
                candidate.column,
                candidate.right(),
                active.column,
                active.right(),
            ),
            u16::MAX.saturating_sub(candidate.bottom()),
            abs_delta(
                candidate.center_column_twice(),
                active.center_column_twice(),
            ),
        ),
        PaneNavigationDirection::Down => (
            range_overlap_u16(
                candidate.column,
                candidate.right(),
                active.column,
                active.right(),
            ),
            candidate.row,
            abs_delta(
                candidate.center_column_twice(),
                active.center_column_twice(),
            ),
        ),
        PaneNavigationDirection::Left => (
            range_overlap_u16(
                candidate.row,
                candidate.bottom(),
                active.row,
                active.bottom(),
            ),
            u16::MAX.saturating_sub(candidate.right()),
            abs_delta(candidate.center_row_twice(), active.center_row_twice()),
        ),
        PaneNavigationDirection::Right => (
            range_overlap_u16(
                candidate.row,
                candidate.bottom(),
                active.row,
                active.bottom(),
            ),
            candidate.column,
            abs_delta(candidate.center_row_twice(), active.center_row_twice()),
        ),
    };
    if overlap == 0 {
        return None;
    }
    Some(WrappedDirectionalCandidate {
        overlap_rank: 0,
        edge_distance,
        center_delta,
        index: candidate.index,
    })
}

/// Runs the abs delta operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn abs_delta(first: u32, second: u32) -> u32 {
    first.max(second).saturating_sub(first.min(second))
}

/// Runs the size from direction operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn size_from_direction(current: Size, direction: ResizeDirection, amount: u16) -> Result<Size> {
    if amount == 0 {
        return Err(MezError::invalid_args(
            "directional resize amount must be positive",
        ));
    }
    match direction {
        ResizeDirection::Left => Size::new(
            current
                .columns
                .checked_sub(amount)
                .ok_or_else(|| MezError::invalid_args("resize would reduce columns below zero"))?,
            current.rows,
        ),
        ResizeDirection::Right => Size::new(
            current
                .columns
                .checked_add(amount)
                .ok_or_else(|| MezError::invalid_args("resize columns are out of range"))?,
            current.rows,
        ),
        ResizeDirection::Up => Size::new(
            current.columns,
            current
                .rows
                .checked_sub(amount)
                .ok_or_else(|| MezError::invalid_args("resize would reduce rows below zero"))?,
        ),
        ResizeDirection::Down => Size::new(
            current.columns,
            current
                .rows
                .checked_add(amount)
                .ok_or_else(|| MezError::invalid_args("resize rows are out of range"))?,
        ),
    }
}

/// Runs the inferred pane rects operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn inferred_pane_rects(window_size: Size, panes: &[Pane]) -> Vec<PaneRect> {
    candidate_layouts(panes)
        .into_iter()
        .find(|layout| layout.size == window_size)
        .map(|layout| layout.rects)
        .unwrap_or_else(|| fallback_pane_rects(window_size, panes))
}

/// Runs the stored pane geometries operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn stored_pane_geometries(window_size: Size, panes: &[Pane]) -> Vec<PaneGeometry> {
    inferred_pane_rects(window_size, panes)
        .into_iter()
        .map(PaneGeometry::from)
        .collect()
}

/// Runs the restored layout root operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn restored_layout_root(
    window_size: Size,
    panes: &[Pane],
    pane_geometries: Option<&[PaneGeometry]>,
) -> Result<LayoutNode> {
    let geometries = pane_geometries
        .map(<[PaneGeometry]>::to_vec)
        .unwrap_or_else(|| stored_pane_geometries(window_size, panes));
    let layout_root = LayoutNode::from_geometries(&geometries)?;
    layout_root.validate_pane_indices(panes.len())?;
    Ok(layout_root)
}

/// Runs the validated pane geometries operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validated_pane_geometries(
    window_size: Size,
    panes: &[Pane],
    geometries: Vec<PaneGeometry>,
) -> Result<Vec<PaneGeometry>> {
    if geometries.len() != panes.len() {
        return Err(MezError::invalid_args(
            "restored pane geometries must align to restored panes",
        ));
    }
    for (index, (pane, geometry)) in panes.iter().zip(geometries.iter()).enumerate() {
        if geometry.index != index {
            return Err(MezError::invalid_args(
                "restored pane geometries must use contiguous pane indices",
            ));
        }
        if geometry.columns == 0 || geometry.rows == 0 {
            return Err(MezError::invalid_args(
                "restored pane geometry dimensions must be non-zero",
            ));
        }
        if geometry.columns != pane.size.columns || geometry.rows != pane.size.rows {
            return Err(MezError::invalid_args(
                "restored pane geometry dimensions must match pane size",
            ));
        }
        if geometry.column.saturating_add(geometry.columns) > window_size.columns
            || geometry.row.saturating_add(geometry.rows) > window_size.rows
        {
            return Err(MezError::invalid_args(
                "restored pane geometry must fit inside the window",
            ));
        }
    }
    for (left_index, left) in geometries.iter().enumerate() {
        for right in geometries.iter().skip(left_index.saturating_add(1)) {
            if pane_geometries_overlap(left, right) {
                return Err(MezError::invalid_args(
                    "restored pane geometries must not overlap",
                ));
            }
        }
    }
    Ok(geometries)
}

/// Runs the validate replacement pane geometries operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validate_replacement_pane_geometries(
    window_size: Size,
    pane_count: usize,
    geometries: &[PaneGeometry],
) -> Result<()> {
    if geometries.len() != pane_count {
        return Err(MezError::invalid_args(
            "replacement pane geometries must align to window panes",
        ));
    }
    let mut seen = vec![false; pane_count];
    for geometry in geometries {
        let Some(slot) = seen.get_mut(geometry.index) else {
            return Err(MezError::invalid_args(
                "replacement pane geometries must use known pane indices",
            ));
        };
        if *slot {
            return Err(MezError::invalid_args(
                "replacement pane geometries must not repeat pane indices",
            ));
        }
        *slot = true;
        if geometry.columns < MIN_PANE_COLUMNS || geometry.rows < MIN_PANE_ROWS {
            return Err(MezError::invalid_args(
                "replacement pane geometry is below the minimum pane dimensions",
            ));
        }
        if geometry.column.saturating_add(geometry.columns) > window_size.columns
            || geometry.row.saturating_add(geometry.rows) > window_size.rows
        {
            return Err(MezError::invalid_args(
                "replacement pane geometry must fit inside the window",
            ));
        }
    }
    if seen.iter().any(|present| !present) {
        return Err(MezError::invalid_args(
            "replacement pane geometries must include every pane",
        ));
    }
    for (left_index, left) in geometries.iter().enumerate() {
        for right in geometries.iter().skip(left_index.saturating_add(1)) {
            if pane_geometries_overlap(left, right) {
                return Err(MezError::invalid_args(
                    "replacement pane geometries must not overlap",
                ));
            }
        }
    }
    Ok(())
}

/// Runs the pane geometries overlap operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn pane_geometries_overlap(left: &PaneGeometry, right: &PaneGeometry) -> bool {
    let left_right = left.column.saturating_add(left.columns);
    let left_bottom = left.row.saturating_add(left.rows);
    let right_right = right.column.saturating_add(right.columns);
    let right_bottom = right.row.saturating_add(right.rows);
    left.column < right_right
        && right.column < left_right
        && left.row < right_bottom
        && right.row < left_bottom
}

/// Carries Candidate Layout state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CandidateLayout {
    /// Stores the size value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    size: Size,
    /// Stores the rects value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    rects: Vec<PaneRect>,
}

/// Runs the candidate layouts operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn candidate_layouts(panes: &[Pane]) -> Vec<CandidateLayout> {
    /// Defines the MAX CANDIDATE LAYOUTS const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    const MAX_CANDIDATE_LAYOUTS: usize = 128;
    candidate_layouts_with_indices(panes, 0, MAX_CANDIDATE_LAYOUTS)
}

/// Runs the candidate layouts with indices operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn candidate_layouts_with_indices(
    panes: &[Pane],
    index_offset: usize,
    limit: usize,
) -> Vec<CandidateLayout> {
    if panes.is_empty() || limit == 0 {
        return Vec::new();
    }
    if panes.len() == 1 {
        return vec![CandidateLayout {
            size: panes[0].size,
            rects: vec![PaneRect {
                index: index_offset,
                column: 0,
                row: 0,
                columns: panes[0].size.columns,
                rows: panes[0].size.rows,
            }],
        }];
    }

    let mut layouts = Vec::new();
    for split in 1..panes.len() {
        let left_layouts = candidate_layouts_with_indices(&panes[..split], index_offset, limit);
        let right_layouts = candidate_layouts_with_indices(
            &panes[split..],
            index_offset + split,
            limit.saturating_sub(layouts.len()),
        );
        for left in &left_layouts {
            for right in &right_layouts {
                if left.size.rows == right.size.rows {
                    layouts.push(join_candidate_layouts(
                        left,
                        right,
                        SplitDirection::Vertical,
                    ));
                    if layouts.len() >= limit {
                        return layouts;
                    }
                }
                if left.size.columns == right.size.columns {
                    layouts.push(join_candidate_layouts(
                        left,
                        right,
                        SplitDirection::Horizontal,
                    ));
                    if layouts.len() >= limit {
                        return layouts;
                    }
                }
            }
        }
    }
    layouts
}

/// Runs the join candidate layouts operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn join_candidate_layouts(
    left: &CandidateLayout,
    right: &CandidateLayout,
    direction: SplitDirection,
) -> CandidateLayout {
    let mut rects = left.rects.clone();
    rects.extend(right.rects.iter().map(|rect| match direction {
        SplitDirection::Vertical => PaneRect {
            column: rect.column.saturating_add(left.size.columns),
            ..*rect
        },
        SplitDirection::Horizontal => PaneRect {
            row: rect.row.saturating_add(left.size.rows),
            ..*rect
        },
    }));
    let size = match direction {
        SplitDirection::Vertical => Size {
            columns: left.size.columns.saturating_add(right.size.columns),
            rows: left.size.rows,
        },
        SplitDirection::Horizontal => Size {
            columns: left.size.columns,
            rows: left.size.rows.saturating_add(right.size.rows),
        },
    };
    CandidateLayout { size, rects }
}

/// Runs the fallback pane rects operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn fallback_pane_rects(window_size: Size, panes: &[Pane]) -> Vec<PaneRect> {
    let side_by_side = panes
        .first()
        .map(|first| {
            panes.iter().all(|pane| pane.size.rows == first.size.rows)
                && panes.iter().map(|pane| pane.size.columns).sum::<u16>() <= window_size.columns
        })
        .unwrap_or(false);
    let mut column = 0u16;
    let mut row = 0u16;
    panes
        .iter()
        .enumerate()
        .map(|(index, pane)| {
            let rect = PaneRect {
                index,
                column,
                row,
                columns: pane.size.columns,
                rows: pane.size.rows,
            };
            if side_by_side {
                column = column.saturating_add(pane.size.columns);
            } else {
                row = row.saturating_add(pane.size.rows);
            }
            rect
        })
        .collect()
}

/// Infers whether a newly created or restored window name is generated.
fn window_name_source_for_created_window(index: usize, name: &str) -> WindowNameSource {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed == index.to_string() || trimmed == "shell" {
        WindowNameSource::Generated
    } else {
        WindowNameSource::Explicit
    }
}
