//! Runtime control helpers for subagent spawning and bucket management.
//!
//! This module owns the subagent-specific control flow that creates child pane
//! processes, chooses subagent bucket placement, applies inherited agent state,
//! queues initial MMP task-status messages, and rolls back partial spawns. The
//! parent `control` module keeps the public dispatch surface while this child
//! module keeps subagent lifecycle details out of the control facade.

use rand::RngExt;

use super::*;
use crate::runtime::RuntimeAgentPromptTurnStart;

/// Minimum useful width for adding another pane to an existing subagent bucket.
///
/// This is deliberately larger than the layout engine's hard pane minimum so a
/// subagent bucket opens another window instead of creating panes that are too
/// narrow to show command output intelligibly.
const SUBAGENT_BUCKET_MIN_COLUMNS: u16 = 24;

/// Minimum useful height for adding another pane to an existing subagent bucket.
///
/// A small row floor keeps stacked subagents readable while still allowing
/// compact terminals to use top/bottom or grid layouts before opening another
/// subagent bucket window.
const SUBAGENT_BUCKET_MIN_ROWS: u16 = 4;

/// Layout choice for adding one pane to a dedicated subagent bucket window.
#[derive(Debug, Clone, Copy)]
struct RuntimeSubagentBucketLayout {
    /// Self-rebalancing policy to apply before the pane is added.
    policy: mez_mux::layout::LayoutPolicy,
    /// Split direction used for the immediate pane creation operation.
    split_direction: SplitDirection,
}

impl Default for RuntimeSubagentBucketLayout {
    fn default() -> Self {
        Self {
            policy: mez_mux::layout::LayoutPolicy::EvenVertical,
            split_direction: SplitDirection::Vertical,
        }
    }
}

/// Chooses a self-rebalancing layout for a subagent bucket with one more pane.
///
/// The policy prefers side-by-side panes while they remain comfortably wide.
/// When another vertical split would create narrow panes, it chooses the best
/// horizontal or grid layout that still satisfies the useful subagent pane
/// floor. Returning `None` tells the caller to create another bucket window.
fn runtime_subagent_bucket_layout(
    size: mez_mux::layout::Size,
    pane_count: usize,
) -> Option<RuntimeSubagentBucketLayout> {
    if pane_count == 0 {
        return None;
    }
    if pane_count == 1 {
        return Some(RuntimeSubagentBucketLayout::default());
    }

    let vertical = runtime_subagent_layout_candidate(
        mez_mux::layout::LayoutPolicy::EvenVertical,
        size,
        pane_count,
    );
    if vertical
        .as_ref()
        .is_some_and(|candidate| candidate.meets_preferred_size())
    {
        return vertical.map(|candidate| candidate.layout);
    }

    [
        vertical,
        runtime_subagent_layout_candidate(
            mez_mux::layout::LayoutPolicy::EvenHorizontal,
            size,
            pane_count,
        ),
        runtime_subagent_layout_candidate(
            mez_mux::layout::LayoutPolicy::EvenGrid,
            size,
            pane_count,
        ),
    ]
    .into_iter()
    .flatten()
    .max_by_key(|candidate| candidate.score())
    .map(|candidate| candidate.layout)
}

/// Candidate metadata used while scoring subagent bucket layouts.
#[derive(Debug, Clone, Copy)]
struct RuntimeSubagentLayoutCandidate {
    /// Layout that would be applied when this candidate wins.
    layout: RuntimeSubagentBucketLayout,
    /// Smallest pane this candidate would produce.
    minimum_size: mez_mux::layout::Size,
}

/// Initial MMP status metadata for a freshly spawned subagent.
struct RuntimeSubagentInitialTaskStatus<'a> {
    /// Parent agent that should receive the task-status envelope.
    parent_agent_id: &'a str,
    /// Runtime id assigned to the spawned subagent.
    child_agent_id: &'a str,
    /// Human-readable subagent name shown in pane logs.
    child_display_name: &'a str,
    /// Requested subagent role label.
    role: &'a str,
    /// Cooperation mode label attached to the child identity.
    cooperation_mode: &'a str,
    /// Turn id for the child's initial prompt task.
    turn_id: &'a str,
    /// Prompt text used to build visible task summaries.
    task_prompt: &'a str,
}

impl RuntimeSubagentLayoutCandidate {
    /// Returns whether this candidate reaches the preferred terminal pane size.
    fn meets_preferred_size(self) -> bool {
        self.minimum_size.columns >= mez_mux::layout::EVEN_GRID_TARGET_COLUMNS
            && self.minimum_size.rows >= mez_mux::layout::EVEN_GRID_TARGET_ROWS
    }

    /// Scores candidates by preferred fit, then pane utility and simplicity.
    fn score(self) -> (u8, u32, u32, u8) {
        let preferred_axes =
            u8::from(self.minimum_size.columns >= mez_mux::layout::EVEN_GRID_TARGET_COLUMNS)
                + u8::from(self.minimum_size.rows >= mez_mux::layout::EVEN_GRID_TARGET_ROWS);
        let min_ratio = std::cmp::min(
            runtime_ratio_millis(
                self.minimum_size.columns,
                mez_mux::layout::EVEN_GRID_TARGET_COLUMNS,
            ),
            runtime_ratio_millis(
                self.minimum_size.rows,
                mez_mux::layout::EVEN_GRID_TARGET_ROWS,
            ),
        );
        let area =
            u32::from(self.minimum_size.columns).saturating_mul(u32::from(self.minimum_size.rows));
        let simple_axis = u8::from(!matches!(
            self.layout.policy,
            mez_mux::layout::LayoutPolicy::EvenGrid
        ));
        (preferred_axes, min_ratio, area, simple_axis)
    }
}

/// Builds one candidate and rejects layouts that would make unusable panes.
fn runtime_subagent_layout_candidate(
    policy: mez_mux::layout::LayoutPolicy,
    size: mez_mux::layout::Size,
    pane_count: usize,
) -> Option<RuntimeSubagentLayoutCandidate> {
    let minimum_size = mez_mux::layout::even_layout_minimum_pane_size(policy, size, pane_count);
    if minimum_size.columns < SUBAGENT_BUCKET_MIN_COLUMNS
        || minimum_size.rows < SUBAGENT_BUCKET_MIN_ROWS
    {
        return None;
    }
    Some(RuntimeSubagentLayoutCandidate {
        layout: RuntimeSubagentBucketLayout {
            policy,
            split_direction: match policy {
                mez_mux::layout::LayoutPolicy::EvenHorizontal => SplitDirection::Horizontal,
                mez_mux::layout::LayoutPolicy::Tiled
                | mez_mux::layout::LayoutPolicy::EvenVertical
                | mez_mux::layout::LayoutPolicy::EvenGrid => SplitDirection::Vertical,
            },
        },
        minimum_size,
    })
}

/// Returns a fixed-point ratio suitable for integer layout scoring.
fn runtime_ratio_millis(value: u16, target: u16) -> u32 {
    u32::from(value).saturating_mul(1000) / u32::from(target.max(1))
}

impl RuntimeSessionService {
    /// Runs the dispatch runtime agent spawn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_agent_spawn(
        &mut self,
        caller_client_id: &mez_core::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        self.require_live()?;
        let caller = self
            .session
            .clients()
            .iter()
            .find(|client| client.id == *caller_client_id)
            .ok_or_else(|| MezError::forbidden("unknown control client"))?;
        if !matches!(caller.role, ClientRole::Primary | ClientRole::Agent) {
            return Err(MezError::forbidden(
                "agent/spawn requires a primary or agent client",
            ));
        }
        let controller =
            self.session.primary_client_id().cloned().ok_or_else(|| {
                MezError::invalid_state("agent/spawn requires an attached primary")
            })?;
        let spawn = runtime_subagent_spawn_request(params, caller.role == ClientRole::Primary)?;
        let placement = runtime_subagent_placement_mode(params)?;
        self.spawn_runtime_subagent(&controller, spawn, placement)
    }

    /// Returns lineage metadata for an agent id, treating untracked pane agents
    /// as delegation roots.
    fn subagent_lineage_for_agent(&self, agent_id: &str) -> RuntimeSubagentLineage {
        self.subagent_lineage
            .get(agent_id)
            .cloned()
            .unwrap_or_else(|| RuntimeSubagentLineage {
                parent_agent_id: String::new(),
                root_agent_id: agent_id.to_string(),
                depth: 0,
                display_name: String::new(),
            })
    }

    /// Counts active direct child subagents owned by the given parent agent.
    fn active_direct_subagent_count(&self, parent_agent_id: &str) -> usize {
        self.subagent_lineage
            .values()
            .filter(|lineage| lineage.parent_agent_id == parent_agent_id)
            .count()
    }

    /// Validates configured subagent width/depth limits for the next spawn.
    fn validate_subagent_spawn_capacity(
        &self,
        parent_agent_id: &str,
    ) -> Result<RuntimeSubagentLineage> {
        let parent_lineage = self.subagent_lineage_for_agent(parent_agent_id);
        if parent_lineage.depth >= self.max_subagent_depth {
            return Err(MezError::forbidden(format!(
                "subagent depth limit reached for {parent_agent_id}: depth {} of {}",
                parent_lineage.depth, self.max_subagent_depth
            )));
        }
        let (limit_name, limit) = if parent_lineage.depth == 0 {
            ("agents.max_root_subagents", self.max_root_subagents)
        } else {
            (
                "agents.max_subagents_per_subagent",
                self.max_subagents_per_subagent,
            )
        };
        let active = self.active_direct_subagent_count(parent_agent_id);
        if active >= limit {
            return Err(MezError::forbidden(format!(
                "subagent spawn limit reached for {parent_agent_id}: active direct children {active}, {limit_name} {limit}"
            )));
        }
        Ok(RuntimeSubagentLineage {
            parent_agent_id: parent_agent_id.to_string(),
            root_agent_id: parent_lineage.root_agent_id,
            depth: parent_lineage.depth + 1,
            display_name: String::new(),
        })
    }

    /// Allocates a compact random display name that is unique among active subagents.
    fn allocate_subagent_display_name(&self) -> String {
        self.allocate_subagent_display_name_with_rng(&mut rand::rng())
    }

    /// Allocates a compact display name using the provided random source.
    fn allocate_subagent_display_name_with_rng<R: rand::Rng + ?Sized>(
        &self,
        rng: &mut R,
    ) -> String {
        let active_names = self
            .subagent_lineage
            .values()
            .filter_map(|lineage| {
                (!lineage.display_name.trim().is_empty()).then_some(lineage.display_name.as_str())
            })
            .collect::<std::collections::BTreeSet<_>>();
        let available_names = SUBAGENT_FRIENDLY_NAMES
            .iter()
            .copied()
            .filter(|name| !active_names.contains(name))
            .collect::<Vec<_>>();
        if !available_names.is_empty() {
            return available_names[rng.random_range(0..available_names.len())].to_string();
        }
        let mut index = SUBAGENT_FRIENDLY_NAMES.len() + 1;
        loop {
            let candidate = format!("Agent {index}");
            if !active_names.contains(candidate.as_str()) {
                return candidate;
            }
            index += 1;
        }
    }

    /// Creates a child agent pane and prompt turn after caller authorization and
    /// control/MAAP parameter parsing have already succeeded.
    ///
    /// The helper owns the stateful spawn sequence so control requests and MAAP
    /// `spawn_agent` actions share scope inheritance, pane rollback, MMP task
    /// status delivery, lifecycle events, and audit behavior.
    pub(in crate::runtime) fn spawn_runtime_subagent(
        &mut self,
        controller: &mez_core::ids::ClientId,
        spawn: SubagentSpawnRequest,
        placement: RuntimeSubagentPlacement,
    ) -> Result<String> {
        self.spawn_runtime_subagent_internal(Some(controller), spawn, placement)
    }

    /// Creates a child subagent for already-authorized session-owned orchestration.
    pub(in crate::runtime) fn spawn_runtime_subagent_session_owned(
        &mut self,
        spawn: SubagentSpawnRequest,
        placement: RuntimeSubagentPlacement,
    ) -> Result<String> {
        self.spawn_runtime_subagent_internal(None, spawn, placement)
    }

    /// Implements client-authenticated and session-owned subagent creation.
    fn spawn_runtime_subagent_internal(
        &mut self,
        controller: Option<&mez_core::ids::ClientId>,
        mut spawn: SubagentSpawnRequest,
        placement: RuntimeSubagentPlacement,
    ) -> Result<String> {
        let profile = self
            .subagent_profiles
            .get(&spawn.requested_role)
            .cloned()
            .ok_or_else(|| MezError::invalid_args("unsupported subagent role"))?;
        if spawn.cooperation_mode_defaulted
            && let Some(mode) = profile.default_cooperation_mode
        {
            spawn.cooperation_mode = mode;
        }
        if spawn.read_scopes_defaulted || spawn.read_scopes.is_empty() {
            spawn.read_scopes = profile.default_read_scopes.clone();
        }
        if spawn.write_scopes_defaulted || spawn.write_scopes.is_empty() {
            spawn.write_scopes = profile.default_write_scopes.clone();
        }
        if let Some(preset) = profile.permission_preset
            && compare_permission_preset_authority(self.permission_policy.preset, preset)
                == mez_agent::permissions::PermissionAuthorityChange::Broadening
        {
            return Err(MezError::forbidden(
                "subagent profile permission override cannot broaden parent policy",
            ));
        }
        if let Some(instructions) = profile.developer_instructions.as_deref() {
            spawn.task_prompt = format!(
                "{}\n\n[profile developer instructions]\n{}",
                spawn.task_prompt, instructions
            );
        }
        let inherited_scope = self
            .subagent_scope_declarations
            .get(&spawn.parent_agent_id)
            .cloned();
        if let Some(parent_scope) = inherited_scope.as_ref() {
            spawn.cooperation_mode = parent_scope.cooperation_mode;
            spawn.read_scopes = parent_scope.read_scopes.clone();
            spawn.write_scopes = parent_scope.write_scopes.clone();
            if parent_scope.cooperation_mode == mez_agent::CooperationMode::Unrestricted {
                spawn.explicit_user_approval = true;
            }
        } else {
            spawn.read_scopes.clear();
            spawn.write_scopes.clear();
        }
        spawn.validate()?;
        let mut child_lineage = self.validate_subagent_spawn_capacity(&spawn.parent_agent_id)?;
        let child_display_name = self.allocate_subagent_display_name();
        child_lineage.display_name = child_display_name.clone();

        let requested_window_name = match &placement {
            RuntimeSubagentPlacement::NewWindow { name, .. } => Some(name.as_str()),
            RuntimeSubagentPlacement::NewPane { .. } => None,
        };
        spawn.placement = "new-window".to_string();
        let child_start_directory = self.subagent_parent_working_directory(&spawn.parent_agent_id);
        let started = self.spawn_subagent_pane_in_parent_group(
            controller,
            &spawn,
            requested_window_name,
            child_start_directory.as_deref(),
        )?;
        let child_agent_id = format!("agent-{}", started.pane_id);
        if let Err(error) = self.apply_subagent_display_titles(
            controller,
            &started.window_id,
            &started.pane_id,
            &child_display_name,
        ) {
            self.cleanup_failed_subagent_spawn(controller, &started.pane_id, &child_agent_id, None);
            return Err(error);
        }
        self.subagent_lineage
            .insert(child_agent_id.clone(), child_lineage);
        let current_directory = self
            .pane_current_working_directory(&started.pane_id)
            .or_else(|| child_start_directory.clone())
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|| ".".to_string());
        let mut child_scope = inherited_scope.map(|mut declaration| {
            declaration.current_directory = current_directory.clone();
            if profile.permission_preset.is_some() {
                declaration.permission_preset = profile.permission_preset;
            }
            declaration
        });
        if child_scope.is_none()
            && let Some(permission_preset) = profile.permission_preset
        {
            child_scope = Some(SubagentScopeDeclaration {
                cooperation_mode: mez_agent::CooperationMode::Unrestricted,
                current_directory: current_directory.clone(),
                read_scopes: Vec::new(),
                write_scopes: Vec::new(),
                permission_preset: Some(permission_preset),
            });
        }
        if let Some(declaration) = child_scope {
            self.subagent_scope_declarations
                .insert(child_agent_id.clone(), declaration);
        }
        if let Err(error) = self.enter_agent_mode_for_pane(&started.pane_id) {
            self.cleanup_failed_subagent_spawn(controller, &started.pane_id, &child_agent_id, None);
            return Err(error);
        }
        if spawn.skip_initial_turn {
            let (window, pane) = match runtime_pane_by_id(&self.session, started.pane_id.as_str()) {
                Ok(result) => result,
                Err(error) => {
                    self.cleanup_failed_subagent_spawn(
                        controller,
                        &started.pane_id,
                        &child_agent_id,
                        None,
                    );
                    return Err(error);
                }
            };
            return Ok(format!(
                r#"{{"agent":{},"pane":{},"turn":null}}"#,
                runtime_subagent_state_json(
                    &self.session,
                    pane,
                    &child_agent_id,
                    &child_display_name,
                    &spawn,
                    None as Option<&RuntimeAgentPromptTurnStart>,
                    self.model_profile_overrides
                        .agent_profiles
                        .get(child_agent_id.as_str())
                        .map(String::as_str),
                ),
                self.runtime_control_pane_state_json(window, pane),
            ));
        }
        if let Err(error) =
            self.append_agent_parent_prompt_to_terminal_buffer(&started.pane_id, &spawn.task_prompt)
        {
            self.cleanup_failed_subagent_spawn(controller, &started.pane_id, &child_agent_id, None);
            return Err(error);
        }
        if let Some(enabled) = self.inherited_routing_for_child_agent(&spawn.parent_agent_id) {
            self.set_agent_routing_override(&started.pane_id, Some(enabled));
        }
        if let Some(auto_sizing) =
            self.inherited_auto_sizing_for_child_agent(&spawn.parent_agent_id)
        {
            self.agent_auto_sizing_overrides
                .insert(started.pane_id.clone(), auto_sizing);
        }
        if let Some(profile_name) = profile.model_profile.as_deref() {
            self.provider_registry.resolve_profile(profile_name)?;
            self.model_profile_overrides
                .agent_profiles
                .insert(child_agent_id.clone(), profile_name.to_string());
        } else if let Some(parent_profile) =
            self.inherited_model_profile_for_child_agent(&spawn.parent_agent_id)
        {
            self.model_profile_overrides
                .agent_profiles
                .insert(child_agent_id.clone(), parent_profile);
        }
        let turn = match self.start_agent_prompt_turn_with_cooperation(
            &started.pane_id,
            &spawn.task_prompt,
            Some(runtime_cooperation_mode_name(spawn.cooperation_mode).to_string()),
            None,
        ) {
            Ok(turn) => turn,
            Err(error) => {
                self.cleanup_failed_subagent_spawn(
                    controller,
                    &started.pane_id,
                    &child_agent_id,
                    None,
                );
                return Err(error);
            }
        };
        if let Err(error) =
            self.enqueue_subagent_initial_task_status(RuntimeSubagentInitialTaskStatus {
                parent_agent_id: &spawn.parent_agent_id,
                child_agent_id: &child_agent_id,
                child_display_name: &child_display_name,
                role: &spawn.requested_role,
                cooperation_mode: runtime_cooperation_mode_name(spawn.cooperation_mode),
                turn_id: &turn.turn_id,
                task_prompt: &spawn.task_prompt,
            })
        {
            self.cleanup_failed_subagent_spawn(
                controller,
                &started.pane_id,
                &child_agent_id,
                Some(&turn.turn_id),
            );
            return Err(error);
        }
        self.subagent_task_routes
            .insert(turn.turn_id.clone(), spawn.parent_agent_id.clone());
        if let Err(error) = self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"parent_agent_id":"{}","child_agent_id":"{}","child_display_name":"{}","pane_id":"{}","role":"{}","cooperation_mode":"{}","turn_id":"{}"}}"#,
                json_escape(&spawn.parent_agent_id),
                json_escape(&child_agent_id),
                json_escape(&child_display_name),
                json_escape(&started.pane_id),
                json_escape(&spawn.requested_role),
                runtime_cooperation_mode_name(spawn.cooperation_mode),
                json_escape(&turn.turn_id)
            ),
        ) {
            self.cleanup_failed_subagent_spawn(
                controller,
                &started.pane_id,
                &child_agent_id,
                Some(&turn.turn_id),
            );
            return Err(error);
        }
        if let Err(error) =
            self.append_subagent_spawn_audit(&spawn, &child_agent_id, &started.pane_id)
        {
            self.cleanup_failed_subagent_spawn(
                controller,
                &started.pane_id,
                &child_agent_id,
                Some(&turn.turn_id),
            );
            return Err(error);
        }
        let (window, pane) = match runtime_pane_by_id(&self.session, started.pane_id.as_str()) {
            Ok(result) => result,
            Err(error) => {
                self.cleanup_failed_subagent_spawn(
                    controller,
                    &started.pane_id,
                    &child_agent_id,
                    Some(&turn.turn_id),
                );
                return Err(error);
            }
        };
        Ok(format!(
            r#"{{"agent":{},"pane":{},"turn":{}}}"#,
            runtime_subagent_state_json(
                &self.session,
                pane,
                &child_agent_id,
                &child_display_name,
                &spawn,
                Some(&turn),
                self.model_profile_overrides
                    .agent_profiles
                    .get(child_agent_id.as_str())
                    .map(String::as_str),
            ),
            self.runtime_control_pane_state_json(window, pane),
            runtime_agent_turn_state_json(&turn)
        ))
    }

    /// Creates or reuses an unfocused subagent window in the parent's group.
    ///
    /// Subagents are isolated from the controller pane by always running in
    /// windows marked as subagent buckets. Each bucket uses a self-rebalancing
    /// even layout chosen from its geometry and accepts panes until the
    /// configured per-window limit or useful pane-size floor is reached, at
    /// which point a new bucket window is created in the same group.
    fn spawn_subagent_pane_in_parent_group(
        &mut self,
        controller: Option<&mez_core::ids::ClientId>,
        spawn: &SubagentSpawnRequest,
        requested_window_name: Option<&str>,
        start_directory: Option<&Path>,
    ) -> Result<PaneProcessStart> {
        self.prune_subagent_window_ids();
        let group_id = self.subagent_parent_group_id(&spawn.parent_agent_id)?;
        if let Some((window_id, layout)) = self.available_subagent_window_in_group(&group_id) {
            if let Some(controller) = controller {
                self.session
                    .set_window_layout_policy(controller, &window_id, layout.policy)?;
                return self.split_pane_in_window_with_process(
                    controller,
                    &window_id,
                    layout.split_direction,
                    true,
                    None,
                    start_directory,
                );
            }
            self.session
                .set_window_layout_policy_session_owned(&window_id, layout.policy)?;
            return self.split_pane_in_window_with_process_session_owned(
                &window_id,
                layout.split_direction,
                true,
                None,
                start_directory,
            );
        }

        let layout =
            runtime_subagent_bucket_layout(self.session.authoritative_size, 1).unwrap_or_default();
        let generated_window_name = requested_window_name
            .filter(|name| !name.trim().is_empty() && *name != "agent")
            .map(ToOwned::to_owned)
            .is_none();
        let name = requested_window_name
            .filter(|name| !name.trim().is_empty() && *name != "agent")
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| self.next_subagent_window_name(&group_id));
        let started = if let Some(controller) = controller {
            self.create_unfocused_window_in_group_with_pane_process(
                controller,
                &group_id,
                name,
                layout.policy,
                start_directory,
            )?
        } else {
            self.create_unfocused_window_in_group_with_pane_process_session_owned(
                &group_id,
                name,
                layout.policy,
                start_directory,
            )?
        };
        if generated_window_name {
            if let Some(controller) = controller {
                self.session
                    .mark_window_name_generated(controller, &started.window_id)?;
            } else {
                self.session
                    .mark_window_name_generated_session_owned(&started.window_id)?;
            }
        }
        self.subagent_window_ids
            .insert(started.window_id.to_string());
        Ok(started)
    }

    /// Applies human-readable display titles to the spawned pane and bucket.
    fn apply_subagent_display_titles(
        &mut self,
        controller: Option<&mez_core::ids::ClientId>,
        window_id: &str,
        pane_id: &str,
        display_name: &str,
    ) -> Result<()> {
        self.session
            .set_pane_title_explicit(pane_id, display_name)?;
        self.subagent_window_ids.insert(window_id.to_string());
        self.refresh_subagent_window_names_internal(controller)
    }

    /// Refreshes generated names for all live subagent bucket windows.
    pub(in crate::runtime) fn refresh_subagent_window_names(
        &mut self,
        controller: &mez_core::ids::ClientId,
    ) -> Result<()> {
        self.refresh_subagent_window_names_internal(Some(controller))
    }

    /// Refreshes generated subagent window names for session-owned orchestration.
    fn refresh_subagent_window_names_internal(
        &mut self,
        controller: Option<&mez_core::ids::ClientId>,
    ) -> Result<()> {
        self.prune_subagent_window_ids();
        let window_ids = self.subagent_window_ids.iter().cloned().collect::<Vec<_>>();
        for window_id in window_ids {
            let Some(name) = self.subagent_bucket_window_display_name(&window_id) else {
                continue;
            };
            if let Some(controller) = controller {
                self.session
                    .rename_window_generated(controller, window_id.as_str(), name)?;
            } else {
                self.session
                    .rename_window_generated_session_owned(window_id.as_str(), name)?;
            }
        }
        Ok(())
    }

    /// Builds a compact window name from the display names of its subagent panes.
    fn subagent_bucket_window_display_name(&self, window_id: &str) -> Option<String> {
        let window = self
            .session
            .windows()
            .iter()
            .find(|window| window.id.as_str() == window_id)?;
        let pane_names = window
            .panes()
            .iter()
            .filter_map(|pane| {
                let agent_id = format!("agent-{}", pane.id);
                self.subagent_lineage
                    .get(&agent_id)
                    .map(|lineage| lineage.display_name.trim())
                    .filter(|display_name| !display_name.is_empty())
                    .or_else(|| {
                        let title = pane.title.trim();
                        (!title.is_empty() && title != "shell").then_some(title)
                    })
            })
            .collect::<Vec<_>>();
        if pane_names.is_empty() {
            return Some("subagents".to_string());
        }
        let visible = pane_names.iter().take(3).copied().collect::<Vec<_>>();
        let mut name = visible.join("/");
        if pane_names.len() > visible.len() {
            name.push_str(&format!("+{}", pane_names.len() - visible.len()));
        }
        Some(name)
    }

    /// Returns the best known working directory for the parent agent pane.
    fn subagent_parent_working_directory(&self, parent_agent_id: &str) -> Option<PathBuf> {
        let parent_pane_id = pane_id_from_runtime_agent_id(parent_agent_id)?;
        self.pane_current_working_directory(parent_pane_id.as_str())
    }

    /// Returns the routing preference a child agent should inherit.
    pub(in crate::runtime) fn inherited_routing_for_child_agent(
        &self,
        parent_agent_id: &str,
    ) -> Option<bool> {
        let parent_pane_id = pane_id_from_runtime_agent_id(parent_agent_id)?;
        Some(self.agent_routing_enabled_for_pane(parent_pane_id.as_str()))
    }

    /// Returns the auto-sizing configuration a child agent should inherit.
    pub(in crate::runtime) fn inherited_auto_sizing_for_child_agent(
        &self,
        parent_agent_id: &str,
    ) -> Option<RuntimeAutoSizingConfig> {
        let parent_pane_id = pane_id_from_runtime_agent_id(parent_agent_id)?;
        Some(
            self.runtime_auto_sizing_config_for_pane(parent_pane_id.as_str())
                .clone(),
        )
    }

    /// Returns the effective local action executor a child agent should inherit.
    /// Removes subagent bucket ids whose windows no longer exist.
    fn prune_subagent_window_ids(&mut self) {
        let live_windows = self
            .session
            .windows()
            .iter()
            .map(|window| window.id.to_string())
            .collect::<std::collections::BTreeSet<_>>();
        self.subagent_window_ids
            .retain(|window_id| live_windows.contains(window_id));
    }

    /// Resolves the window group that should own a child subagent window.
    fn subagent_parent_group_id(
        &self,
        parent_agent_id: &str,
    ) -> Result<mez_core::ids::WindowGroupId> {
        if let Some(parent_pane_id) = pane_id_from_runtime_agent_id(parent_agent_id)
            && let Ok((parent_window, _)) =
                runtime_pane_by_id(&self.session, parent_pane_id.as_str())
            && let Some(group) = self.session.window_groups().iter().find(|group| {
                group
                    .window_ids
                    .iter()
                    .any(|window_id| window_id == &parent_window.id)
            })
        {
            return Ok(group.id.clone());
        }
        self.session
            .active_group()
            .map(|group| group.id.clone())
            .ok_or_else(|| MezError::invalid_state("session has no active window group"))
    }

    /// Finds a subagent bucket window in a group that still has usable capacity.
    fn available_subagent_window_in_group(
        &self,
        group_id: &mez_core::ids::WindowGroupId,
    ) -> Option<(mez_core::ids::WindowId, RuntimeSubagentBucketLayout)> {
        let group = self
            .session
            .window_groups()
            .iter()
            .find(|group| &group.id == group_id)?;
        group.window_ids.iter().find_map(|window_id| {
            self.subagent_window_ids
                .contains(window_id.as_str())
                .then(|| {
                    self.session
                        .windows()
                        .iter()
                        .find(|window| &window.id == window_id)
                })
                .flatten()
                .and_then(|window| {
                    let next_pane_count = window.panes().len().saturating_add(1);
                    (next_pane_count <= self.max_subagent_panes_per_window)
                        .then(|| runtime_subagent_bucket_layout(window.size, next_pane_count))
                        .flatten()
                        .map(|layout| (window.id.clone(), layout))
                })
        })
    }

    /// Builds a generated name for the next subagent bucket in a group.
    fn next_subagent_window_name(&self, group_id: &mez_core::ids::WindowGroupId) -> String {
        let count = self
            .session
            .window_groups()
            .iter()
            .find(|group| &group.id == group_id)
            .map(|group| {
                group
                    .window_ids
                    .iter()
                    .filter(|window_id| self.subagent_window_ids.contains(window_id.as_str()))
                    .count()
            })
            .unwrap_or(0);
        if count == 0 {
            "subagents".to_string()
        } else {
            format!("subagents {}", count + 1)
        }
    }

    /// Rolls back the pane, model override, turn state, and write scope created
    /// before a subagent spawn setup step failed.
    fn cleanup_failed_subagent_spawn(
        &mut self,
        controller: Option<&mez_core::ids::ClientId>,
        pane_id: &str,
        child_agent_id: &str,
        turn_id: Option<&str>,
    ) {
        let pane_ids = vec![pane_id.to_string()];
        let _ = self.fail_agent_turns_for_pane_shutdown(&pane_ids, "subagent spawn setup failed");
        if let Some(turn_id) = turn_id {
            self.subagent_task_routes.remove(turn_id);
            self.clear_joined_subagent_dependencies_for_turn(turn_id);
        }
        self.subagent_scopes.unregister(child_agent_id);
        self.subagent_scope_declarations.remove(child_agent_id);
        self.subagent_lineage.remove(child_agent_id);
        self.deregister_macro_managed_subagent(child_agent_id);
        self.model_profile_overrides
            .agent_profiles
            .remove(child_agent_id);
        if let Some(controller) = controller {
            let _ = self.dispatch_runtime_pane_close(
                controller,
                &format!(r#"{{"pane_id":"{}","force":true}}"#, json_escape(pane_id)),
            );
        } else if let Ok(transition) = self.session.kill_pane_session_owned(Some(pane_id), true) {
            let Some(pane) = transition.pane else {
                return;
            };
            let removed_pane_id = pane.id.to_string();
            let _ = self.stop_active_pane_pipe(&removed_pane_id);
            let _ = self.terminate_runtime_pane_process(&removed_pane_id, true);
            self.cleanup_removed_pane_runtime_state(&removed_pane_id);
            let _ = self.sync_pane_resize_effects(&transition.effects);
        }
    }

    /// Queues the first MMP task-status update for a newly spawned subagent.
    ///
    /// The parent subscription is created before the status envelope is accepted
    /// so a later `mmp.receive` call can observe this initial running state
    /// instead of starting after it.
    fn enqueue_subagent_initial_task_status(
        &mut self,
        initial_status: RuntimeSubagentInitialTaskStatus<'_>,
    ) -> Result<()> {
        let now_ms = current_unix_seconds().saturating_mul(1000);
        let parent_identity = self.ensure_runtime_message_identity(
            initial_status.parent_agent_id,
            None,
            "agent",
            &[],
            now_ms,
        )?;
        if self
            .message_service
            .subscription(&parent_identity.agent_id)
            .is_none()
        {
            self.message_service.subscribe(&parent_identity.agent_id)?;
        }
        let child_pane_id = pane_id_from_runtime_agent_id(initial_status.child_agent_id)
            .ok_or_else(|| MezError::invalid_args("subagent pane id is invalid for MMP"))?;
        let child_pane_label = child_pane_id.to_string();
        let child_identity = self.ensure_runtime_message_identity(
            initial_status.child_agent_id,
            Some(child_pane_id),
            initial_status.role,
            &["agent-harness", "subagent", initial_status.cooperation_mode],
            now_ms,
        )?;
        let task_status = TaskStatusPayload {
            task_id: initial_status.turn_id.to_string(),
            state: TaskState::Running,
            progress_percent: Some(0),
            summary: "subagent task started".to_string(),
        };
        let envelope = Envelope {
            protocol: "mmp/1",
            id: format!("{}:task_status:started", initial_status.turn_id),
            message_type: "task_status".to_string(),
            time: format!("runtime:{now_ms}"),
            sender: child_identity.clone(),
            recipient: Recipient::Agent(parent_identity.agent_id),
            correlation_id: Some(initial_status.turn_id.to_string()),
            ttl_ms: None,
            content_type: "application/json".to_string(),
            payload: task_status.to_json(),
            extension_fields: vec![(
                "subagent_display_name".to_string(),
                format!(r#""{}""#, json_escape(initial_status.child_display_name)),
            )],
        };
        self.message_service
            .accept_at(&child_identity.agent_id, envelope, now_ms)?;
        self.append_subagent_parent_status_line(
            initial_status.parent_agent_id,
            &format!(
                "subagent {} ({}) started in pane {} ({}, {}): {}",
                initial_status.child_display_name,
                initial_status.child_agent_id,
                child_pane_label,
                initial_status.role,
                initial_status.cooperation_mode,
                Self::runtime_subagent_task_summary(initial_status.task_prompt)
            ),
        )?;
        Ok(())
    }

    /// Produces a short single-line summary for visible subagent spawn logs.
    fn runtime_subagent_task_summary(task_prompt: &str) -> String {
        /// Maximum characters retained in parent-pane spawn summaries.
        const MAX_SUBAGENT_TASK_SUMMARY_CHARS: usize = 120;
        let collapsed = task_prompt.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.is_empty() {
            return "task not specified".to_string();
        }
        let mut summary = String::new();
        let mut chars = collapsed.chars();
        for _ in 0..MAX_SUBAGENT_TASK_SUMMARY_CHARS {
            let Some(ch) = chars.next() else {
                return summary;
            };
            summary.push(ch);
        }
        if chars.next().is_some() {
            summary.push_str("...");
        }
        summary
    }
}
