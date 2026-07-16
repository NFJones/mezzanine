//! Runtime process helpers for pane layout creation and resize synchronization.
//!
//! This module owns runtime-backed window, group, pane-split, pane-resize, and
//! primary-terminal resize operations that mutate session layout while keeping
//! tracked PTY sizes synchronized. The parent processes module keeps lower-level
//! pane I/O, output, lifecycle, and transaction coordination while this child
//! module keeps layout rollback and size validation rules together.

use super::*;

impl RuntimeSessionService {
    /// Runs the create window with pane process operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn create_window_with_pane_process(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        name: impl Into<String>,
        select: bool,
        explicit_command: Option<&str>,
    ) -> Result<PaneProcessStart> {
        self.create_window_with_pane_process_with_options(
            primary_client_id,
            name,
            select,
            explicit_command,
            None,
            None,
        )
    }

    /// Creates a window with one pane and starts the pane process with creation options.
    ///
    /// The caller must be the active primary client. `start_directory`, when
    /// present, is applied to the spawned shell. `requested_size`, when present,
    /// resizes the created pane before the PTY is opened.
    pub fn create_window_with_pane_process_with_options(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        name: impl Into<String>,
        select: bool,
        explicit_command: Option<&str>,
        start_directory: Option<&Path>,
        requested_size: Option<PaneSizeSpec>,
    ) -> Result<PaneProcessStart> {
        self.require_live()?;
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        validate_runtime_start_directory(start_directory)?;
        if let Some(spec) = requested_size {
            validate_new_window_requested_pane_size(self.session.authoritative_size, spec)?;
        }
        let previous_session = self.session.clone();
        let previous_window_created_at_unix_seconds = self.window_created_at_unix_seconds.clone();
        let window_id = self.session.new_window(primary_client_id, name, select)?;
        self.window_created_at_unix_seconds
            .insert(window_id.to_string(), current_unix_seconds());
        if let Some(spec) = requested_size {
            let pane_id = self
                .session
                .windows()
                .iter()
                .find(|window| window.id == window_id)
                .and_then(|window| window.panes().first())
                .map(|pane| pane.id.clone())
                .ok_or_else(|| {
                    MezError::new(
                        crate::error::MezErrorKind::NotFound,
                        "created pane not found",
                    )
                })?;
            let pane = self.session.resize_pane_in_window_with_spec(
                primary_client_id,
                &window_id,
                &pane_id,
                spec,
            )?;
            validate_pane_size_for_resize(pane.size)?;
        }
        let window = self
            .session
            .windows()
            .iter()
            .find(|window| window.id == window_id)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "created window not found",
                )
            })?;
        let pane = window.active_pane();
        let size = self
            .pane_process_size_for(window, pane.id.as_str())
            .unwrap_or(pane.size);
        let descriptor = PaneDescriptor {
            window_id: window.id.clone(),
            pane_id: pane.id.clone(),
            size,
        };
        let started = match self.start_pane_process_with_start_directory(
            descriptor,
            explicit_command,
            start_directory,
        ) {
            Ok(started) => started,
            Err(error) => {
                self.session = previous_session;
                self.window_created_at_unix_seconds = previous_window_created_at_unix_seconds;
                return Err(error);
            }
        };
        self.append_lifecycle_event(
            EventKind::WindowChanged,
            format!(
                r#"{{"window_id":"{}","state":"created","pane_id":"{}"}}"#,
                json_escape(&started.window_id),
                json_escape(&started.pane_id)
            ),
        )?;
        Ok(started)
    }

    /// Creates a window in a specific group and starts its initial pane process.
    ///
    /// Unlike the normal window creation path, this helper does not require the
    /// target group to be active and never focuses the created window. It is
    /// used for subagent windows that should belong beside their controller
    /// without stealing user focus.
    pub fn create_unfocused_window_in_group_with_pane_process(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        group_id: &mez_core::ids::WindowGroupId,
        name: impl Into<String>,
        layout_policy: mez_mux::layout::LayoutPolicy,
        start_directory: Option<&Path>,
    ) -> Result<PaneProcessStart> {
        self.require_live()?;
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        self.create_unfocused_window_in_group_with_pane_process_internal(
            Some(primary_client_id),
            group_id,
            name,
            layout_policy,
            start_directory,
        )
    }

    /// Creates an unfocused window and pane process for session-owned orchestration.
    pub(in crate::runtime) fn create_unfocused_window_in_group_with_pane_process_session_owned(
        &mut self,
        group_id: &mez_core::ids::WindowGroupId,
        name: impl Into<String>,
        layout_policy: mez_mux::layout::LayoutPolicy,
        start_directory: Option<&Path>,
    ) -> Result<PaneProcessStart> {
        self.require_live()?;
        self.create_unfocused_window_in_group_with_pane_process_internal(
            None,
            group_id,
            name,
            layout_policy,
            start_directory,
        )
    }

    /// Implements authenticated and session-owned unfocused window creation.
    fn create_unfocused_window_in_group_with_pane_process_internal(
        &mut self,
        primary_client_id: Option<&mez_core::ids::ClientId>,
        group_id: &mez_core::ids::WindowGroupId,
        name: impl Into<String>,
        layout_policy: mez_mux::layout::LayoutPolicy,
        start_directory: Option<&Path>,
    ) -> Result<PaneProcessStart> {
        validate_runtime_start_directory(start_directory)?;
        let previous_session = self.session.clone();
        let previous_window_created_at_unix_seconds = self.window_created_at_unix_seconds.clone();
        let window_id = if let Some(primary_client_id) = primary_client_id {
            self.session
                .new_window_in_group(primary_client_id, group_id, name, false)?
        } else {
            self.session
                .new_window_in_group_session_owned(group_id, name, false)?
        };
        self.window_created_at_unix_seconds
            .insert(window_id.to_string(), current_unix_seconds());
        if let Some(primary_client_id) = primary_client_id {
            self.session
                .set_window_layout_policy(primary_client_id, &window_id, layout_policy)?;
        } else {
            self.session
                .set_window_layout_policy_session_owned(&window_id, layout_policy)?;
        }
        let window = self
            .session
            .windows()
            .iter()
            .find(|window| window.id == window_id)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "created window not found",
                )
            })?;
        let pane = window.active_pane();
        let size = self
            .pane_process_size_for(window, pane.id.as_str())
            .unwrap_or(pane.size);
        let descriptor = PaneDescriptor {
            window_id: window.id.clone(),
            pane_id: pane.id.clone(),
            size,
        };
        let started =
            match self.start_pane_process_with_start_directory(descriptor, None, start_directory) {
                Ok(started) => started,
                Err(error) => {
                    self.session = previous_session;
                    self.window_created_at_unix_seconds = previous_window_created_at_unix_seconds;
                    return Err(error);
                }
            };
        self.append_lifecycle_event(
            EventKind::WindowChanged,
            format!(
                r#"{{"window_id":"{}","group_id":"{}","state":"created","pane_id":"{}","layout_policy":"{}"}}"#,
                json_escape(&started.window_id),
                json_escape(group_id.as_str()),
                json_escape(&started.pane_id),
                layout_policy.name()
            ),
        )?;
        Ok(started)
    }

    /// Creates a new window group with one landing pane and starts its process.
    ///
    /// This follows the same runtime path as `window/create`: the in-memory
    /// session mutation is rolled back if the pane process cannot be spawned.
    pub fn create_group_with_pane_process(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        name: impl Into<String>,
        select: bool,
        explicit_command: Option<&str>,
        start_directory: Option<&Path>,
    ) -> Result<PaneProcessStart> {
        self.require_live()?;
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        validate_runtime_start_directory(start_directory)?;
        let previous_session = self.session.clone();
        let previous_window_created_at_unix_seconds = self.window_created_at_unix_seconds.clone();
        let (group_id, window_id) = self.session.new_group(primary_client_id, name, select)?;
        self.window_created_at_unix_seconds
            .insert(window_id.to_string(), current_unix_seconds());
        let window = self
            .session
            .windows()
            .iter()
            .find(|window| window.id == window_id)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "created group window not found",
                )
            })?;
        let pane = window.active_pane();
        let size = self
            .pane_process_size_for(window, pane.id.as_str())
            .unwrap_or(pane.size);
        let descriptor = PaneDescriptor {
            window_id: window.id.clone(),
            pane_id: pane.id.clone(),
            size,
        };
        let started = match self.start_pane_process_with_start_directory(
            descriptor,
            explicit_command,
            start_directory,
        ) {
            Ok(started) => started,
            Err(error) => {
                self.session = previous_session;
                self.window_created_at_unix_seconds = previous_window_created_at_unix_seconds;
                return Err(error);
            }
        };
        self.append_lifecycle_event(
            EventKind::WindowChanged,
            format!(
                r#"{{"group_id":"{}","window_id":"{}","state":"created","pane_id":"{}"}}"#,
                json_escape(group_id.as_str()),
                json_escape(&started.window_id),
                json_escape(&started.pane_id)
            ),
        )?;
        Ok(started)
    }

    /// Runs the split pane with process operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn split_pane_with_process(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        direction: SplitDirection,
        explicit_command: Option<&str>,
    ) -> Result<PaneProcessStart> {
        self.split_pane_with_process_with_options(
            primary_client_id,
            direction,
            true,
            explicit_command,
            None,
            None,
        )
    }

    /// Splits the active pane and starts the new pane process with creation options.
    ///
    /// The caller must be the active primary client. The new pane inherits the
    /// normal split geometry unless `requested_size` is provided, in which case
    /// the pane and PTY are resized before process spawn.
    pub fn split_pane_with_process_with_options(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        direction: SplitDirection,
        select_new: bool,
        explicit_command: Option<&str>,
        start_directory: Option<&Path>,
        requested_size: Option<PaneSizeSpec>,
    ) -> Result<PaneProcessStart> {
        self.require_live()?;
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        validate_runtime_start_directory(start_directory)?;
        let previous_session = self.session.clone();
        let previous_window_created_at_unix_seconds = self.window_created_at_unix_seconds.clone();
        let pane_id = match requested_size {
            Some(spec) => self.session.split_active_pane_with_size_spec_select(
                primary_client_id,
                direction,
                spec,
                select_new,
            )?,
            None => {
                self.session
                    .split_active_pane_select(primary_client_id, direction, select_new)?
            }
        };
        if let Err(error) = self.sync_tracked_pty_sizes() {
            self.session = previous_session;
            self.window_created_at_unix_seconds = previous_window_created_at_unix_seconds;
            let _ = self.sync_tracked_pty_sizes();
            return Err(error);
        }
        let descriptor = match self.find_pane_descriptor(pane_id.as_str()) {
            Some(descriptor) => descriptor,
            None => {
                self.session = previous_session;
                self.window_created_at_unix_seconds = previous_window_created_at_unix_seconds;
                let _ = self.sync_tracked_pty_sizes();
                return Err(MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "created pane not found",
                ));
            }
        };
        match self.start_pane_process_with_start_directory(
            descriptor,
            explicit_command,
            start_directory,
        ) {
            Ok(started) => Ok(started),
            Err(error) => {
                self.session = previous_session;
                self.window_created_at_unix_seconds = previous_window_created_at_unix_seconds;
                let _ = self.sync_tracked_pty_sizes();
                Err(error)
            }
        }
    }

    /// Splits a target window and starts a process in the created pane.
    ///
    /// The session-level focused window is left untouched. This lets background
    /// orchestration append panes to a hidden or non-focused window while still
    /// reusing the normal process, PTY-size synchronization, and rollback path.
    pub fn split_pane_in_window_with_process(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        window_id: &WindowId,
        direction: SplitDirection,
        select_new: bool,
        explicit_command: Option<&str>,
        start_directory: Option<&Path>,
    ) -> Result<PaneProcessStart> {
        self.require_live()?;
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        self.split_pane_in_window_with_process_internal(
            Some(primary_client_id),
            window_id,
            direction,
            select_new,
            explicit_command,
            start_directory,
        )
    }

    /// Splits a background window and starts its pane process for session-owned orchestration.
    pub(in crate::runtime) fn split_pane_in_window_with_process_session_owned(
        &mut self,
        window_id: &WindowId,
        direction: SplitDirection,
        select_new: bool,
        explicit_command: Option<&str>,
        start_directory: Option<&Path>,
    ) -> Result<PaneProcessStart> {
        self.require_live()?;
        self.split_pane_in_window_with_process_internal(
            None,
            window_id,
            direction,
            select_new,
            explicit_command,
            start_directory,
        )
    }

    /// Implements authenticated and session-owned background pane creation.
    fn split_pane_in_window_with_process_internal(
        &mut self,
        primary_client_id: Option<&mez_core::ids::ClientId>,
        window_id: &WindowId,
        direction: SplitDirection,
        select_new: bool,
        explicit_command: Option<&str>,
        start_directory: Option<&Path>,
    ) -> Result<PaneProcessStart> {
        validate_runtime_start_directory(start_directory)?;
        let previous_session = self.session.clone();
        let previous_window_created_at_unix_seconds = self.window_created_at_unix_seconds.clone();
        let pane_id = if let Some(primary_client_id) = primary_client_id {
            self.session.split_pane_in_window_select(
                primary_client_id,
                window_id,
                direction,
                select_new,
            )?
        } else {
            self.session
                .split_pane_in_window_select_session_owned(window_id, direction, select_new)?
        };
        if let Err(error) = self.sync_tracked_pty_sizes() {
            self.session = previous_session;
            self.window_created_at_unix_seconds = previous_window_created_at_unix_seconds;
            let _ = self.sync_tracked_pty_sizes();
            return Err(error);
        }
        let descriptor = match self.find_pane_descriptor(pane_id.as_str()) {
            Some(descriptor) => descriptor,
            None => {
                self.session = previous_session;
                self.window_created_at_unix_seconds = previous_window_created_at_unix_seconds;
                let _ = self.sync_tracked_pty_sizes();
                return Err(MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "created pane not found",
                ));
            }
        };
        match self.start_pane_process_with_start_directory(
            descriptor,
            explicit_command,
            start_directory,
        ) {
            Ok(started) => Ok(started),
            Err(error) => {
                self.session = previous_session;
                self.window_created_at_unix_seconds = previous_window_created_at_unix_seconds;
                let _ = self.sync_tracked_pty_sizes();
                Err(error)
            }
        }
    }

    /// Runs the resize pane pty operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn resize_pane_pty(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        target: Option<&str>,
        size: Size,
    ) -> Result<PaneResizeUpdate> {
        self.require_live()?;
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        validate_pane_size_for_resize(size)?;
        let descriptor = self.active_window_pane_descriptor(target)?;
        let target_pane_id = descriptor.pane_id.to_string();
        if self
            .primary_pid_for_live_pane_process(descriptor.pane_id.as_str())
            .is_none()
        {
            return Err(MezError::new(
                crate::error::MezErrorKind::NotFound,
                "pane process not found",
            ));
        }

        let mut next_session = self.session.clone();
        let transition = next_session.resize_pane_transition(primary_client_id, target, size)?;
        self.session = next_session;
        self.sync_pane_resize_effects(&transition.effects)?
            .into_iter()
            .find(|update| update.pane_id == target_pane_id)
            .ok_or_else(|| MezError::invalid_state("resized pane process was not synchronized"))
    }

    /// Resolves a size spec, resizes the pane PTY, and updates session state.
    pub fn resize_pane_pty_with_spec(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        target: Option<&str>,
        spec: PaneSizeSpec,
    ) -> Result<PaneResizeUpdate> {
        self.require_live()?;
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        let descriptor = self.active_window_pane_descriptor(target)?;
        let size = self
            .session
            .windows()
            .iter()
            .find(|window| window.id == descriptor.window_id)
            .ok_or_else(|| MezError::invalid_state("pane window not found"))?
            .resolve_pane_size_spec(Some(descriptor.pane_id.as_str()), spec)?;
        self.resize_pane_pty(primary_client_id, target, size)
    }

    /// Runs the swap panes and sync pty sizes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn swap_panes_and_sync_pty_sizes(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        source: Option<&str>,
        destination: &str,
    ) -> Result<Vec<PaneResizeUpdate>> {
        self.require_live()?;
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        let effects = self
            .session
            .swap_panes_transition(primary_client_id, source, destination)?;
        self.sync_pane_resize_effects(&effects)
    }

    /// Runs the break pane and sync pty sizes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn break_pane_and_sync_pty_sizes(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        target: Option<&str>,
        name: Option<String>,
        select_new_window: bool,
    ) -> Result<(WindowId, Vec<PaneResizeUpdate>)> {
        self.require_live()?;
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        let transition = self.session.break_pane_transition(
            primary_client_id,
            target,
            name,
            select_new_window,
        )?;
        let window_id = transition.window_id;
        self.window_created_at_unix_seconds
            .insert(window_id.to_string(), current_unix_seconds());
        let updates = self.sync_pane_resize_effects(&transition.effects)?;
        Ok((window_id, updates))
    }

    /// Runs the join pane and sync pty sizes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn join_pane_and_sync_pty_sizes(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        source: Option<&str>,
        destination: &str,
        direction: SplitDirection,
        select_joined_pane: bool,
    ) -> Result<(PaneId, Vec<PaneResizeUpdate>)> {
        self.require_live()?;
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        let transition = self.session.join_pane_transition(
            primary_client_id,
            source,
            destination,
            direction,
            select_joined_pane,
        )?;
        let pane_id = transition.pane_id;
        let updates = self.sync_pane_resize_effects(&transition.effects)?;
        Ok((pane_id, updates))
    }

    /// Runs the sync tracked pty sizes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn sync_tracked_pty_sizes(&mut self) -> Result<Vec<PaneResizeUpdate>> {
        self.require_live()?;
        let effects = self
            .tracked_pane_descriptors()
            .into_iter()
            .map(|descriptor| mez_mux::session::PaneResizeEffect {
                pane_id: descriptor.pane_id,
                size: descriptor.size,
            })
            .collect::<Vec<_>>();
        self.sync_pane_resize_effects(&effects)
    }

    /// Applies process-neutral session resize effects to product-owned PTYs and screens.
    pub(crate) fn sync_pane_resize_effects(
        &mut self,
        effects: &[mez_mux::session::PaneResizeEffect],
    ) -> Result<Vec<PaneResizeUpdate>> {
        let mut updates = Vec::new();

        for effect in effects {
            let descriptor = self
                .find_pane_descriptor(effect.pane_id.as_str())
                .ok_or_else(|| MezError::invalid_state("resized pane descriptor was not found"))?;
            let pane_id = descriptor.pane_id.as_str();
            let Some(primary_pid) = self.primary_pid_for_live_pane_process(pane_id) else {
                continue;
            };
            let process_size = self
                .session
                .windows()
                .iter()
                .find(|window| window.id == descriptor.window_id)
                .and_then(|window| self.pane_process_size_for(window, pane_id))
                .unwrap_or(effect.size);
            if self.pane_processes.contains_pane(pane_id) {
                self.pane_processes.resize_pane(pane_id, process_size)?;
            } else if self.pane_process_is_adapter_owned(pane_id) {
                self.queued_pane_resize_effects.insert(
                    pane_id.to_string(),
                    RuntimeSideEffect::ResizePane {
                        pane_id: pane_id.to_string(),
                        size: process_size,
                    },
                );
            }
            if let Some(screen) = self.pane_screens.get_mut(descriptor.pane_id.as_str()) {
                screen.resize(process_size);
            }
            if let Some(screen) = self
                .pane_transaction_osc_screens
                .get_mut(descriptor.pane_id.as_str())
            {
                screen.resize(process_size);
            }
            let update = PaneResizeUpdate {
                session_id: self.session.id.to_string(),
                window_id: descriptor.window_id.to_string(),
                pane_id: descriptor.pane_id.to_string(),
                primary_pid,
                size: process_size,
                registry_update: self.registry_update_plan(),
            };
            self.append_pane_resize_event(&update)?;
            updates.push(update);
        }

        Ok(updates)
    }

    /// Applies a primary terminal resize to session geometry and tracked pane PTYs.
    pub fn resize_attached_primary_terminal(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        size: Size,
    ) -> Result<Vec<PaneResizeUpdate>> {
        self.require_live()?;
        validate_pane_size_for_resize(size)?;
        let effects = self
            .session
            .resize_authoritative_terminal_transition(primary_client_id, size)?;
        self.presentation.clear_mouse_resize_drag_state();
        self.refresh_active_copy_mode_viewports()?;
        let updates = self.sync_pane_resize_effects(&effects)?;
        self.append_lifecycle_event(
            EventKind::PaneChanged,
            format!(
                r#"{{"terminal_resize":"primary","columns":{},"rows":{},"resized_panes":{}}}"#,
                size.columns,
                size.rows,
                updates.len()
            ),
        )?;
        Ok(updates)
    }

    /// Refreshes retained copy-mode viewport heights after pane geometry changes.
    fn refresh_active_copy_mode_viewports(&mut self) -> Result<()> {
        let pane_ids = self.active_copy_modes.keys().cloned().collect::<Vec<_>>();
        for pane_id in pane_ids {
            let viewport_rows = self.copy_mode_viewport_rows_for_pane(&pane_id);
            if let Some(copy_mode) = self.active_copy_modes.get_mut(&pane_id) {
                copy_mode.resize_viewport_rows(viewport_rows)?;
            }
        }
        Ok(())
    }
}

fn validate_runtime_start_directory(start_directory: Option<&Path>) -> Result<()> {
    let Some(start_directory) = start_directory else {
        return Ok(());
    };
    let metadata = std::fs::metadata(start_directory).map_err(|error| {
        MezError::invalid_args(format!(
            "start_directory `{}` is not accessible: {error}",
            start_directory.display()
        ))
    })?;
    if !metadata.is_dir() {
        return Err(MezError::invalid_args(format!(
            "start_directory `{}` is not a directory",
            start_directory.display()
        )));
    }
    Ok(())
}

/// Runs the validate new window requested pane size operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validate_new_window_requested_pane_size(window_size: Size, spec: PaneSizeSpec) -> Result<()> {
    let size = requested_new_window_pane_size(window_size, spec)?;
    validate_pane_size_for_resize(size)?;
    if size.columns > window_size.columns || size.rows > window_size.rows {
        return Err(MezError::invalid_args(
            "pane creation size must fit inside the new window",
        ));
    }
    Ok(())
}

/// Runs the requested new window pane size operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn requested_new_window_pane_size(window_size: Size, spec: PaneSizeSpec) -> Result<Size> {
    match spec {
        PaneSizeSpec::Cells { columns, rows } => Size::new(
            columns.unwrap_or(window_size.columns),
            rows.unwrap_or(window_size.rows),
        )
        .map_err(MezError::from),
        PaneSizeSpec::Percent { percent, axis } => {
            if percent == 0 {
                return Err(MezError::invalid_args(
                    "percent pane creation size requires a positive percent",
                ));
            }
            let columns = if matches!(axis, ResizeAxis::Columns | ResizeAxis::Both) {
                requested_percent_dimension(window_size.columns, percent, "columns")?
            } else {
                window_size.columns
            };
            let rows = if matches!(axis, ResizeAxis::Rows | ResizeAxis::Both) {
                requested_percent_dimension(window_size.rows, percent, "rows")?
            } else {
                window_size.rows
            };
            Size::new(columns, rows).map_err(MezError::from)
        }
        PaneSizeSpec::Delta { direction, amount }
        | PaneSizeSpec::Edge {
            edge: direction,
            amount,
        } => requested_new_window_pane_size_from_direction(window_size, direction, amount),
    }
}

/// Runs the requested new window pane size from direction operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn requested_new_window_pane_size_from_direction(
    current: Size,
    direction: ResizeDirection,
    amount: u16,
) -> Result<Size> {
    if amount == 0 {
        return Err(MezError::invalid_args(
            "directional pane creation size amount must be positive",
        ));
    }
    match direction {
        ResizeDirection::Left => Size::new(
            current.columns.checked_sub(amount).ok_or_else(|| {
                MezError::invalid_args("pane creation size would reduce columns below zero")
            })?,
            current.rows,
        )
        .map_err(MezError::from),
        ResizeDirection::Right => Size::new(
            current.columns.checked_add(amount).ok_or_else(|| {
                MezError::invalid_args("pane creation size columns are out of range")
            })?,
            current.rows,
        )
        .map_err(MezError::from),
        ResizeDirection::Up => Size::new(
            current.columns,
            current.rows.checked_sub(amount).ok_or_else(|| {
                MezError::invalid_args("pane creation size would reduce rows below zero")
            })?,
        )
        .map_err(MezError::from),
        ResizeDirection::Down => Size::new(
            current.columns,
            current.rows.checked_add(amount).ok_or_else(|| {
                MezError::invalid_args("pane creation size rows are out of range")
            })?,
        )
        .map_err(MezError::from),
    }
}

/// Runs the requested percent dimension operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn requested_percent_dimension(total: u16, percent: u16, axis: &'static str) -> Result<u16> {
    let scaled = u32::from(total)
        .saturating_mul(u32::from(percent))
        .saturating_add(99)
        / 100;
    u16::try_from(scaled.max(1)).map_err(|_| {
        MezError::invalid_args(format!("percent pane creation size {axis} is out of range"))
    })
}

/// Runs the terminal clipboard policy accepts osc52 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn terminal_clipboard_policy_accepts_osc52(policy: &str) -> bool {
    matches!(policy, "external" | "host" | "internal")
}
