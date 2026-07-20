//! Live configuration replacement, reload, application, memory, and trust prompts.

use super::mcp_helpers::{
    runtime_mcp_enabled_server_count, runtime_mcp_pending_discovery_server_ids,
    runtime_mcp_server_has_live_auth_recovery,
};
use super::{
    BTreeMap, BTreeSet, ConfigFormat, ConfigLayer, ConfigScope, EventKind, McpRegistry, MezError,
    ModelProfile, Path, PathBuf, Result, RuntimeConfigApplyReport, RuntimePresentationSettings,
    RuntimeProviderConfig, RuntimeSessionService, SnapshotRepository, TrustDecision,
    compose_effective_config, current_unix_seconds, discover_existing_overlays,
    discover_project_root, fs, json_escape, runtime_agent_action_failure_retry_limit_from_config,
    runtime_agent_auto_sizing_from_config,
    runtime_agent_compaction_raw_retention_percent_from_config,
    runtime_agent_custom_system_prompt_from_config, runtime_agent_loop_limit_from_config,
    runtime_agent_personality_profiles_from_config, runtime_agent_routing_from_config,
    runtime_audit_config_present, runtime_audit_log_from_config,
    runtime_configured_permissions_from_config, runtime_default_agent_personality_from_config,
    runtime_default_models_for_provider, runtime_effective_config_value,
    runtime_history_limit_from_config, runtime_history_rotate_lines_from_config,
    runtime_hook_definitions_from_config, runtime_host_clipboard_from_config,
    runtime_max_concurrent_agents_from_config, runtime_max_root_subagents_from_config,
    runtime_max_subagent_depth_from_config, runtime_max_subagent_panes_per_window_from_config,
    runtime_max_subagents_per_subagent_from_config, runtime_mcp_registry_from_config,
    runtime_preset_registry_from_config, runtime_provider_auth_refresh_leeway_seconds_from_config,
    runtime_provider_registry_from_config, runtime_saved_agent_session_limit_from_config,
    runtime_subagent_profiles_from_config, runtime_subagent_wait_policy_from_config,
    runtime_terminal_emoji_width_from_config,
    runtime_terminal_shell_output_preview_lines_from_config, runtime_terminal_term_from_config,
};

impl RuntimeSessionService {
    /// Runs the config layers operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn config_layers(&self) -> &[ConfigLayer] {
        self.integration.config_layers()
    }

    /// Runs the set config root operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn set_config_root(&mut self, root: PathBuf) {
        let _ = crate::integrations::skills::sync_managed_builtin_skills(&root);
        self.integration.set_config_root(Some(root));
    }

    /// Sets the snapshot repository used by live terminal snapshot commands.
    pub fn set_snapshot_repository(&mut self, snapshots: SnapshotRepository) {
        self.persistence.set_snapshot_repository(snapshots);
    }

    /// Runs the replace config layers operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub fn replace_config_layers(
        &mut self,
        layers: Vec<ConfigLayer>,
    ) -> Result<RuntimeConfigApplyReport> {
        self.integration.replace_config_layers(layers);
        self.apply_runtime_config_layers()
    }

    /// Runs the replace config layers async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn replace_config_layers_async(
        &mut self,
        layers: Vec<ConfigLayer>,
    ) -> Result<RuntimeConfigApplyReport> {
        self.integration.replace_config_layers(layers);
        self.apply_runtime_config_layers_async().await
    }

    /// Refreshes trusted or pending project overlay layers for a pane cwd.
    ///
    /// The daemon can outlive shell-directory changes, so project `.mezzanine`
    /// overlays cannot be discovered only at startup. This refresh keeps the
    /// active layer list aligned with the pane's current repository before
    /// agent work or explicit skill display relies on project-scoped config.
    ///
    /// # Parameters
    /// - `pane_id`: Pane whose current working directory determines the
    ///   effective project root and overlay files.
    pub(crate) fn refresh_project_config_layers_for_pane(
        &mut self,
        pane_id: &str,
    ) -> Result<usize> {
        let Some(current_dir) = self.pane_current_working_directory(pane_id) else {
            return Ok(0);
        };
        let project_root = discover_project_root(&current_dir);
        let overlay_files = discover_existing_overlays(&project_root, &current_dir)?;
        if overlay_files.is_empty() {
            return self.remove_project_config_layers_for_root(&project_root);
        }

        let trusted = self
            .project_trust_store()
            .as_ref()
            .and_then(|store| store.get(&project_root))
            .is_some_and(|record| record.state == TrustDecision::Trusted);
        let selected = overlay_files.iter().cloned().collect::<BTreeSet<_>>();
        let before = self.integration.config_layers().to_vec();
        self.integration.config_layers_mut().retain(|layer| {
            layer.scope != ConfigScope::ProjectOverlay
                || layer
                    .path
                    .as_ref()
                    .is_some_and(|path| selected.contains(path))
        });

        let overlay_count = overlay_files.len();
        for (index, overlay_path) in overlay_files.into_iter().enumerate() {
            let name = if overlay_count == 1 {
                "project".to_string()
            } else {
                format!("project:{}", index + 1)
            };
            let refreshed = ConfigLayer {
                name,
                path: Some(overlay_path.clone()),
                format: ConfigFormat::from_path(&overlay_path)?,
                scope: ConfigScope::ProjectOverlay,
                trusted,
                text: fs::read_to_string(&overlay_path)?,
            };
            if let Some(existing) = self
                .integration
                .config_layers_mut()
                .iter_mut()
                .find(|layer| {
                    layer.scope == ConfigScope::ProjectOverlay
                        && layer.path.as_ref() == Some(&overlay_path)
                })
            {
                *existing = refreshed;
            } else {
                self.integration.config_layers_mut().push(refreshed);
            }
        }

        if self.integration.config_layers() == before {
            return Ok(0);
        }
        let report = self.apply_runtime_config_layers()?;
        Ok(report.applied_layers.len() + report.skipped_layers.len())
    }

    /// Removes stale project overlay layers when the active pane has no
    /// discoverable overlay files.
    ///
    /// # Parameters
    /// - `_project_root`: Current project root, retained for call-site clarity.
    pub(super) fn remove_project_config_layers_for_root(
        &mut self,
        _project_root: &Path,
    ) -> Result<usize> {
        let before_len = self.integration.config_layers().len();
        self.integration
            .config_layers_mut()
            .retain(|layer| layer.scope != ConfigScope::ProjectOverlay);
        let removed = before_len.saturating_sub(self.integration.config_layers().len());
        if removed > 0 {
            self.apply_runtime_config_layers()?;
        }
        Ok(removed)
    }

    /// Runs the reload config layers from disk operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn reload_config_layers_from_disk(&mut self) -> Result<RuntimeConfigApplyReport> {
        for layer in self.integration.config_layers_mut() {
            let Some(path) = layer.path.as_ref() else {
                continue;
            };
            layer.format = ConfigFormat::from_path(path)?;
            layer.text = fs::read_to_string(path)?;
        }
        self.apply_runtime_config_layers()
    }

    /// Runs the reload config layers from disk async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "test-only adapter retained for focused boundary coverage"
    )]
    pub async fn reload_config_layers_from_disk_async(
        &mut self,
    ) -> Result<RuntimeConfigApplyReport> {
        for layer in self.integration.config_layers_mut() {
            let Some(path) = layer.path.as_ref() else {
                continue;
            };
            layer.format = ConfigFormat::from_path(path)?;
            layer.text = fs::read_to_string(path)?;
        }
        self.apply_runtime_config_layers_async().await
    }

    /// Captures live generated model profiles referenced by override state.
    pub(super) fn preserved_model_override_profiles(&self) -> BTreeMap<String, ModelProfile> {
        let mut names = BTreeSet::new();
        if let Some(profile) = self
            .integration
            .model_profile_overrides()
            .session_profile
            .as_ref()
        {
            names.insert(profile.clone());
        }
        names.extend(
            self.integration
                .model_profile_overrides()
                .window_profiles
                .values()
                .cloned(),
        );
        names.extend(
            self.integration
                .model_profile_overrides()
                .pane_profiles
                .values()
                .cloned(),
        );
        names.extend(
            self.integration
                .model_profile_overrides()
                .agent_profiles
                .values()
                .cloned(),
        );
        names.extend(
            self.integration
                .model_profile_overrides()
                .subagent_profiles
                .values()
                .cloned(),
        );
        names
            .into_iter()
            .filter_map(|name| {
                self.provider_registry()
                    .profile(&name)
                    .cloned()
                    .map(|profile| (name, profile))
            })
            .collect()
    }

    /// Runs the apply runtime config layers operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn apply_runtime_config_layers(&mut self) -> Result<RuntimeConfigApplyReport> {
        let effective = compose_effective_config(self.integration.config_layers())?;
        let structured = runtime_effective_config_value(self.integration.config_layers())?;
        let terminal_history_limit = runtime_history_limit_from_config(&structured)?;
        let terminal_history_rotate_lines = runtime_history_rotate_lines_from_config(&structured)?;
        let saved_agent_session_limit = runtime_saved_agent_session_limit_from_config(&structured)?;
        let terminal_term = runtime_terminal_term_from_config(&structured)?;
        let presentation_settings =
            RuntimePresentationSettings::from_config(&structured, &effective)?;
        let terminal_emoji_width = runtime_terminal_emoji_width_from_config(&structured)?;
        let terminal_shell_output_preview_lines =
            runtime_terminal_shell_output_preview_lines_from_config(&structured)?;
        let host_clipboard = runtime_host_clipboard_from_config(&structured)?;
        let audit_log = if runtime_audit_config_present(&structured) {
            Some(runtime_audit_log_from_config(
                &structured,
                self.integration.config_root(),
            )?)
        } else {
            None
        };
        if let Some(store) = self.persistence.transcript_store_mut() {
            store.set_saved_sessions_limit(saved_agent_session_limit)?;
        }
        self.apply_process_terminal_settings(
            terminal_history_limit,
            terminal_history_rotate_lines,
            terminal_term,
            terminal_emoji_width,
            terminal_shell_output_preview_lines,
        )?;
        self.presentation.apply_settings(presentation_settings);
        self.set_host_clipboard(host_clipboard);
        match audit_log {
            Some(Some(audit_log)) => self.set_audit_log(audit_log),
            Some(None) => self.clear_audit_log(),
            None => {}
        }
        let max_concurrent_agents = runtime_max_concurrent_agents_from_config(&structured)?;
        self.configure_subagent_policy(
            runtime_max_subagent_panes_per_window_from_config(&structured)?,
            runtime_max_root_subagents_from_config(&structured)?,
            runtime_max_subagents_per_subagent_from_config(&structured)?,
            runtime_max_subagent_depth_from_config(&structured)?,
            runtime_subagent_wait_policy_from_config(&structured)?,
        );
        self.set_agent_compaction_raw_retention_percent(
            runtime_agent_compaction_raw_retention_percent_from_config(&structured)?,
        );
        self.set_agent_default_routing(runtime_agent_routing_from_config(&structured)?);
        self.set_agent_action_failure_retry_limit(
            runtime_agent_action_failure_retry_limit_from_config(&structured)?,
        );
        self.set_agent_loop_limit(runtime_agent_loop_limit_from_config(&structured)?);
        self.integration.set_provider_auth_refresh_leeway_seconds(
            runtime_provider_auth_refresh_leeway_seconds_from_config(&structured),
        );
        self.set_agent_auto_sizing(runtime_agent_auto_sizing_from_config(&structured)?);
        self.configure_agent_scheduler_limit(max_concurrent_agents)?;
        self.start_ready_agent_turns()?;
        let mut configured_permissions = runtime_configured_permissions_from_config(&structured)?;
        if let Some(approval_policy) = self.integration.live_approval_policy_override() {
            configured_permissions.authorization.approval_policy = approval_policy;
        }
        if let Some(active) = self.integration.live_approval_bypass_override() {
            configured_permissions
                .authorization
                .set_approval_bypass(active);
        }
        self.integration
            .replace_configured_permissions(configured_permissions);
        let preserved_model_profiles = self.preserved_model_override_profiles();
        let mut provider_registry = runtime_provider_registry_from_config(&structured)?;
        for (name, profile) in preserved_model_profiles {
            if provider_registry.provider(&profile.provider).is_some() {
                provider_registry.profiles.entry(name).or_insert(profile);
            }
        }
        let preset_registry =
            runtime_preset_registry_from_config(&structured, &provider_registry.profiles)?;
        // Synthesize provider entries for authenticated providers not in config.
        if let Some(auth_store) = self.integration.auth_store() {
            let all_metadata = auth_store.read_all_metadata().unwrap_or_default();
            for auth_provider in all_metadata.keys() {
                if !provider_registry.providers.contains_key(auth_provider)
                    && let Ok(default_models) = runtime_default_models_for_provider(auth_provider)
                {
                    provider_registry.providers.insert(
                        auth_provider.clone(),
                        RuntimeProviderConfig {
                            provider_id: auth_provider.clone(),
                            kind: auth_provider.clone(),
                            api: None,
                            auth_profile: "default".to_string(),
                            base_url: None,
                            models: default_models.iter().map(|m| (*m).to_string()).collect(),
                            default_model: Some(
                                default_models
                                    .first()
                                    .map(|m| (*m).to_string())
                                    .unwrap_or_default(),
                            ),
                            options: BTreeMap::new(),
                        },
                    );
                }
            }
        }
        self.integration
            .replace_provider_registry(provider_registry);
        *self.integration.preset_registry_mut() = preset_registry;
        self.clear_provider_model_catalog_cache();
        self.integration
            .replace_subagent_profiles(runtime_subagent_profiles_from_config(&structured)?);
        let personality_profiles = runtime_agent_personality_profiles_from_config(&structured)?;
        let default_personality = runtime_default_agent_personality_from_config(&structured)?;
        if let Some(default_personality) = default_personality.as_ref()
            && !personality_profiles.contains_key(default_personality)
        {
            return Err(MezError::config(format!(
                "agents.default_personality `{default_personality}` is not defined in personalities"
            )));
        }
        self.integration
            .replace_agent_personality_profiles(personality_profiles);
        self.integration
            .set_default_agent_personality(default_personality);
        self.integration.set_custom_agent_system_prompt(
            runtime_agent_custom_system_prompt_from_config(&structured)?,
        );
        self.integration
            .replace_hook_definitions(runtime_hook_definitions_from_config(&structured)?);
        let mut registry = runtime_mcp_registry_from_config(&structured)?;
        let environment = std::env::vars().collect::<BTreeMap<_, _>>();
        let blacklisted = registry
            .blacklist_servers_with_missing_environment(&environment, current_unix_seconds())?;
        self.integration.mcp_transports_mut().clear();
        let configured = registry.list_servers().len();
        *self.integration.mcp_registry_mut() = registry;
        let trust_prompts_announced =
            self.append_project_trust_prompt_events_for_pending_layers()?;
        let _ = self.load_persistent_memory_into_session();
        Ok(RuntimeConfigApplyReport {
            applied_layers: effective.applied_layers().to_vec(),
            skipped_layers: effective.skipped_layers().to_vec(),
            terminal_history_limit: self.terminal_history_limit(),
            terminal_history_rotate_lines: self.terminal_history_rotate_lines(),
            terminal_term: self.terminal_term().to_string(),
            window_frames_enabled: self.window_frames_enabled(),
            pane_frames_enabled: self.pane_frames_enabled(),
            max_concurrent_agents,
            permission_policy_applied: true,
            mcp_servers_configured: configured,
            mcp_servers_blacklisted: blacklisted,
            providers_configured: self.provider_registry().providers.len(),
            model_profiles_configured: self.provider_registry().profiles.len(),
            default_model_profile: self.provider_registry().default_profile.clone(),
            hooks_configured: self.integration.hook_definitions().len(),
            project_trust_prompts_announced: trust_prompts_announced,
            ui_theme: self.ui_theme().name.clone(),
        })
    }

    /// Runs the apply runtime config layers async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn apply_runtime_config_layers_async(&mut self) -> Result<RuntimeConfigApplyReport> {
        let mut report = self.apply_runtime_config_layers()?;
        let environment = std::env::vars().collect::<BTreeMap<_, _>>();
        let mut registry = std::mem::take(self.integration.mcp_registry_mut());
        let discovery_blacklisted = self
            .initialize_runtime_mcp_transports_async(
                &mut registry,
                &environment,
                "runtime-config-apply",
                true,
            )
            .await?;
        report.mcp_servers_blacklisted.extend(discovery_blacklisted);
        *self.integration.mcp_registry_mut() = registry;
        Ok(report)
    }

    /// Discovers configured MCP transports that are not already available.
    ///
    /// MCP transports are runtime-owned resources shared across agent turns.
    /// This method is intentionally lazy and preserves existing transports so
    /// an agent prompt or `/list-mcp` does not disconnect working servers.
    pub(crate) async fn ensure_runtime_mcp_transports_discovered_async(
        &mut self,
    ) -> Result<Vec<String>> {
        let environment = std::env::vars().collect::<BTreeMap<_, _>>();
        let mut registry = std::mem::take(self.integration.mcp_registry_mut());
        let blacklisted = self
            .initialize_runtime_mcp_transports_async(
                &mut registry,
                &environment,
                "runtime-mcp-ensure",
                false,
            )
            .await?;
        *self.integration.mcp_registry_mut() = registry;
        let _ = self.persist_registry_update_plan(&self.registry_update_plan());
        Ok(blacklisted)
    }

    /// Initializes configured MCP transports and emits readable lifecycle state.
    ///
    /// The startup path calls this immediately after applying configuration so
    /// configured servers are contacted at session start instead of waiting for
    /// the first agent prompt. Failures still degrade MCP availability for the
    /// session rather than failing session or pane startup.
    ///
    /// # Parameters
    /// - `registry`: In-memory MCP registry being initialized.
    /// - `environment`: Process environment used to resolve server startup plans.
    /// - `source`: Stable source label placed in lifecycle event payloads.
    /// - `emit_empty_completion`: Whether to log a completion summary when
    ///   enabled servers exist but none require startup.
    pub(super) async fn initialize_runtime_mcp_transports_async(
        &mut self,
        registry: &mut McpRegistry,
        environment: &BTreeMap<String, String>,
        source: &str,
        emit_empty_completion: bool,
    ) -> Result<Vec<String>> {
        if emit_empty_completion {
            self.append_runtime_mcp_prechecked_status_events(registry, source)?;
        }
        let pending_server_ids = runtime_mcp_pending_discovery_server_ids(registry, |server| {
            runtime_mcp_server_has_live_auth_recovery(server, self.integration.auth_store())
        });
        if pending_server_ids.is_empty() {
            if emit_empty_completion && runtime_mcp_enabled_server_count(registry) > 0 {
                self.append_runtime_mcp_initialization_completed_event(registry, source, 0)?;
            }
            return Ok(Vec::new());
        }

        self.append_runtime_mcp_initialization_started_event(source, pending_server_ids.len())?;
        let blacklisted = self
            .discover_runtime_mcp_transports_async(registry, environment)
            .await?;
        self.append_runtime_mcp_initialization_completed_event(
            registry,
            source,
            pending_server_ids.len(),
        )?;
        Ok(blacklisted)
    }

    /// Loads global and project-scoped persistent memory records into the
    /// session memory store so agents can benefit from user-stored context
    /// loaded through the CLI.
    pub(crate) fn load_persistent_memory_into_session(&mut self) -> Result<()> {
        let Some(config_root) = self.integration.config_root() else {
            return Ok(());
        };
        let store = crate::storage::memory::PersistentMemoryStore::under_config_root(config_root);
        let Ok(records) = store.list() else {
            return Ok(());
        };
        for record in &records {
            match &record.scope {
                mez_agent::memory::MemoryScope::Global
                | mez_agent::memory::MemoryScope::Project { .. }
                    if record.validate_for_session().is_ok() =>
                {
                    let _ = self.integration.session_memory_mut().upsert(record.clone());
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Persists global and project-scoped session memory records to the
    /// persistent store so they survive beyond this session.
    pub(crate) fn persist_session_memory_to_disk(&mut self) {
        let Some(config_root) = self.integration.config_root() else {
            return;
        };
        let store = crate::storage::memory::PersistentMemoryStore::under_config_root(config_root);
        for record in self.integration.session_memory().export() {
            match &record.scope {
                mez_agent::memory::MemoryScope::Global
                | mez_agent::memory::MemoryScope::Project { .. }
                    if record.validate_for_persistence().is_ok() =>
                {
                    let _ = store.upsert(record);
                }
                _ => {}
            }
        }
    }

    /// Runs the append project trust prompt events for pending layers operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn append_project_trust_prompt_events_for_pending_layers(
        &mut self,
    ) -> Result<usize> {
        let mut overlays_by_root: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
        for layer in self.integration.config_layers() {
            if layer.scope != ConfigScope::ProjectOverlay || layer.trusted {
                continue;
            }
            let Some(path) = layer.path.as_ref() else {
                continue;
            };
            let root = path
                .parent()
                .map(discover_project_root)
                .unwrap_or_else(|| discover_project_root(path));
            let pending = self
                .project_trust_store()
                .as_ref()
                .and_then(|store| store.get(&root))
                .map(|record| record.state == TrustDecision::Pending)
                .unwrap_or(true);
            if pending {
                overlays_by_root.entry(root).or_default().push(path.clone());
            }
        }

        let mut announced = 0usize;
        for (root, overlays) in overlays_by_root {
            if !self
                .integration
                .mark_project_trust_root_announced(root.clone())
            {
                continue;
            }
            let overlay_json = overlays
                .iter()
                .map(|path| format!(r#""{}""#, json_escape(&path.to_string_lossy())))
                .collect::<Vec<_>>()
                .join(",");
            self.append_primary_lifecycle_event(
                EventKind::ConfigChanged,
                format!(
                    r#"{{"project_root":"{}","state":"pending","blocks_until_primary_decision":true,"overlay_files":[{}],"prompt":"project trust decision required","approve_method":"project/trust/decide","reject_method":"project/trust/decide","trust_command":"/trust {}"}}"#,
                    json_escape(&root.to_string_lossy()),
                    overlay_json,
                    json_escape(&root.to_string_lossy())
                ),
            )?;
            announced = announced.saturating_add(1);
        }
        Ok(announced)
    }
}
