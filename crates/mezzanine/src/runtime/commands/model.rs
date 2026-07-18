//! Agent model, routing-model, latency, and provider catalog commands.
//!
//! This module owns pane-local model profile selection, provider catalog
//! lookup and caching, generated runtime model profiles, routing-model
//! selection, latency preferences, and picker state derived from those
//! profiles. It keeps provider-catalog mechanics separate from the broader
//! agent-shell command dispatcher.

use super::model_catalog::{
    RuntimeModelCatalog, runtime_configured_reasoning_levels_for_model,
    runtime_model_catalog_display, runtime_routing_model_profile_display,
};
use super::slash::runtime_single_mode_arg;
use super::{
    AgentShellCommandOutcome, AgentShellVisibility, DEFAULT_AUTO_SIZING_ROUTER_PROFILE, MezError,
    ModelCatalogSelectionErrorKind, ModelProfile, ModelProfileOverrides, ProviderCapabilities,
    RUNTIME_LATENCY_PREFERENCES, Result, RuntimeAutoSizingConfig, RuntimeModelPreset,
    RuntimeModelProfileOverrideScope, RuntimeSessionService, json_escape,
    normalize_model_catalog_values, parse_slash_command, runtime_default_models_for_provider,
    runtime_model_command_args, runtime_model_override_scope_for_args,
    runtime_model_override_scope_name, runtime_model_profile_display,
    runtime_validate_latency_preference, select_model_profile,
};

impl RuntimeSessionService {
    pub(super) fn execute_agent_shell_model_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("model command must be a slash command"))?;
        let agent_id = format!("agent-{pane_id}");
        let args = runtime_model_command_args(&invocation.args)?;
        if args.routing {
            return self.execute_agent_shell_routing_model_command(
                pane_id,
                &agent_id,
                RuntimeRoutingModelCommandArgs {
                    profile: args.profile.as_deref(),
                    reasoning_profile: args.reasoning_profile.as_deref(),
                    clear: args.clear,
                    list: args.list,
                    show: args.show,
                },
            );
        }
        let scope = runtime_model_override_scope_for_args(self, pane_id, &agent_id, &args)?;
        let (active_name, active_profile) =
            self.active_model_profile_for_pane(pane_id, &agent_id, None)?;
        if args.clear {
            self.clear_model_profile_override(scope.clone());
            let (active_name, active_profile) =
                self.active_model_profile_for_pane(pane_id, &agent_id, None)?;
            return Ok(AgentShellCommandOutcome::Mutated {
                command: "model".to_string(),
                body: format!(
                    "scope={} cleared=true active_profile={} provider={} model={}",
                    runtime_model_override_scope_name(&scope),
                    active_name,
                    active_profile.provider,
                    active_profile.model
                ),
                visibility: self
                    .agent_shell_store()
                    .get(pane_id)
                    .map(|session| session.visibility)
                    .unwrap_or(AgentShellVisibility::Hidden),
            });
        }
        if args.list {
            let catalog = self.runtime_model_catalog_for_provider(&active_profile.provider)?;
            self.record_agent_provider_quota_usage(pane_id, &catalog.quota_usage);
            return Ok(AgentShellCommandOutcome::Display {
                command: "model".to_string(),
                body: runtime_model_catalog_display(&active_name, &active_profile, &catalog),
            });
        }
        if args.profile.is_none() || args.show {
            return Ok(AgentShellCommandOutcome::Display {
                command: "model".to_string(),
                body: runtime_model_profile_display(
                    &active_name,
                    &active_profile,
                    self.provider_registry().profiles(),
                ),
            });
        }
        let requested = args
            .profile
            .as_deref()
            .ok_or_else(|| MezError::invalid_args("model command requires a profile name"))?;
        let profile_name = if args.reasoning_profile.is_none()
            && self.provider_registry().profile(requested).is_some()
        {
            requested.to_string()
        } else {
            let catalog = self.runtime_model_catalog_for_provider(&active_profile.provider)?;
            self.runtime_generated_profile_for_provider_model(
                &active_profile.provider,
                requested,
                args.reasoning_profile.as_deref(),
                None,
                &catalog,
            )?
        };
        self.set_model_profile_override(scope.clone(), &profile_name)?;
        let profile = self.provider_registry().resolve_profile(&profile_name)?;
        Ok(AgentShellCommandOutcome::Mutated {
            command: "model".to_string(),
            body: format!(
                "scope={} profile={} provider={} model={} reasoning_profile={} source=runtime-model-selection",
                runtime_model_override_scope_name(&scope),
                profile_name,
                profile.provider,
                profile.model,
                profile.reasoning_profile.as_deref().unwrap_or("none")
            ),
            visibility: self
                .agent_shell_store()
                .get(pane_id)
                .map(|session| session.visibility)
                .unwrap_or(AgentShellVisibility::Hidden),
        })
    }

    /// Executes `/latency` as a pane-local model-profile latency preference override.
    pub(crate) fn execute_agent_shell_latency_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("latency command must be a slash command"))?;
        let agent_id = format!("agent-{pane_id}");
        let (active_name, active_profile) =
            self.active_model_profile_for_pane(pane_id, &agent_id, None)?;
        let args = invocation.args.trim();
        if args.is_empty() || matches!(args, "status" | "show") {
            return Ok(AgentShellCommandOutcome::Display {
                command: "latency".to_string(),
                body: format!(
                    "active_profile={} provider={} model={} reasoning_profile={} latency_preference={} available={}",
                    active_name,
                    active_profile.provider,
                    active_profile.model,
                    active_profile
                        .reasoning_profile
                        .as_deref()
                        .unwrap_or("none"),
                    active_profile
                        .latency_preference
                        .as_deref()
                        .unwrap_or("default"),
                    RUNTIME_LATENCY_PREFERENCES.join(",")
                ),
            });
        }
        if args.split_whitespace().count() != 1 {
            return Err(MezError::invalid_args(
                "latency command accepts at most one preference",
            ));
        }
        let latency = runtime_validate_latency_preference(args)?;
        let catalog = self.runtime_model_catalog_for_provider(&active_profile.provider)?;
        let profile_name = self.runtime_generated_profile_for_provider_model(
            &active_profile.provider,
            &active_profile.model,
            active_profile.reasoning_profile.as_deref(),
            Some(latency),
            &catalog,
        )?;
        let scope = RuntimeModelProfileOverrideScope::Pane(pane_id.to_string());
        self.set_model_profile_override(scope.clone(), &profile_name)?;
        let profile = self.provider_registry().resolve_profile(&profile_name)?;
        Ok(AgentShellCommandOutcome::Mutated {
            command: "latency".to_string(),
            body: format!(
                "scope={} profile={} provider={} model={} reasoning_profile={} latency_preference={} source=runtime-latency-selection",
                runtime_model_override_scope_name(&scope),
                profile_name,
                profile.provider,
                profile.model,
                profile.reasoning_profile.as_deref().unwrap_or("none"),
                profile.latency_preference.as_deref().unwrap_or("default")
            ),
            visibility: self
                .agent_shell_store()
                .get(pane_id)
                .map(|session| session.visibility)
                .unwrap_or(AgentShellVisibility::Hidden),
        })
    }

    /// Reports whether the provider behind one model profile supports
    /// provider-visible latency preferences.
    pub(crate) fn model_profile_supports_latency_preference(&self, profile: &ModelProfile) -> bool {
        self.provider_registry()
            .provider(&profile.provider)
            .is_some_and(|provider| {
                ProviderCapabilities::for_provider_config(&provider.kind, provider.api.as_deref())
                    .map(|capabilities| capabilities.supports_service_tier)
                    .unwrap_or(false)
            })
    }

    /// Executes `/thinking` as a pane-local provider thinking-mode override.
    pub(crate) fn execute_agent_shell_thinking_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("thinking command must be a slash command"))?;
        let agent_id = format!("agent-{pane_id}");
        let (active_name, active_profile) =
            self.active_model_profile_for_pane(pane_id, &agent_id, None)?;
        let Some(enabled_before) = self.model_profile_thinking_enabled(&active_profile) else {
            return Err(MezError::invalid_args(format!(
                "provider `{}` does not support a thinking-mode toggle",
                active_profile.provider
            )));
        };
        let mode = runtime_single_mode_arg(&invocation.args, "thinking", "status")?;
        if matches!(mode.as_str(), "status" | "show") {
            return Ok(AgentShellCommandOutcome::Display {
                command: "thinking".to_string(),
                body: format!(
                    "active_profile={} provider={} model={} enabled={} explicit={} source=runtime-thinking",
                    active_name,
                    active_profile.provider,
                    active_profile.model,
                    enabled_before,
                    active_profile.thinking_enabled().is_some()
                ),
            });
        }
        let enabled = match mode.as_str() {
            "on" => true,
            "off" => false,
            "toggle" => !enabled_before,
            _ => {
                return Err(MezError::invalid_args(
                    "thinking slash command expects on, off, toggle, status, or no argument",
                ));
            }
        };
        let mut profile = active_profile.clone();
        profile.provider_options.insert(
            "thinking".to_string(),
            if enabled { "enabled" } else { "disabled" }.to_string(),
        );
        let profile_name = self.insert_runtime_generated_model_profile(profile);
        let scope = RuntimeModelProfileOverrideScope::Pane(pane_id.to_string());
        self.set_model_profile_override(scope.clone(), &profile_name)?;
        let profile = self.provider_registry().resolve_profile(&profile_name)?;
        Ok(AgentShellCommandOutcome::Mutated {
            command: "thinking".to_string(),
            body: format!(
                "scope={} profile={} provider={} model={} reasoning_profile={} thinking={} changed={} source=runtime-thinking",
                runtime_model_override_scope_name(&scope),
                profile_name,
                profile.provider,
                profile.model,
                profile.reasoning_profile.as_deref().unwrap_or("none"),
                if enabled { "enabled" } else { "disabled" },
                enabled != enabled_before
            ),
            visibility: self
                .agent_shell_store()
                .get(pane_id)
                .map(|session| session.visibility)
                .unwrap_or(AgentShellVisibility::Hidden),
        })
    }

    /// Reports whether one profile supports and currently enables provider
    /// thinking mode.
    pub(crate) fn model_profile_thinking_enabled(&self, profile: &ModelProfile) -> Option<bool> {
        if !self.model_profile_supports_thinking_toggle(profile) {
            return None;
        }
        Some(profile.thinking_enabled().unwrap_or_else(|| {
            profile
                .reasoning_profile
                .as_deref()
                .is_some_and(|reasoning| !reasoning.trim().is_empty())
                || profile
                    .provider_options
                    .get("reasoning_effort")
                    .is_some_and(|effort| !effort.trim().is_empty())
        }))
    }

    /// Reports whether the provider behind one model profile exposes a native
    /// thinking-mode toggle.
    pub(crate) fn model_profile_supports_thinking_toggle(&self, profile: &ModelProfile) -> bool {
        self.provider_registry()
            .provider(&profile.provider)
            .is_some_and(|provider| {
                ProviderCapabilities::for_provider_config(&provider.kind, provider.api.as_deref())
                    .map(|capabilities| capabilities.supports_thinking_toggle)
                    .unwrap_or(false)
            })
    }

    /// Executes `/model --routing` against the auto-sizing router profile.
    ///
    /// The routing model is the provider request used to classify turn size
    /// before the main model request. Keeping it on the `/model` command makes
    /// the model controls discoverable without changing ordinary pane model
    /// selection semantics.
    fn execute_agent_shell_routing_model_command(
        &mut self,
        pane_id: &str,
        agent_id: &str,
        args: RuntimeRoutingModelCommandArgs<'_>,
    ) -> Result<AgentShellCommandOutcome> {
        let (_active_name, active_profile) =
            self.active_model_profile_for_pane(pane_id, agent_id, None)?;
        let (routing_name, routing_profile) = self.active_routing_model_profile()?;
        if args.clear {
            return self.set_routing_model_profile_outcome(
                pane_id,
                DEFAULT_AUTO_SIZING_ROUTER_PROFILE,
                &active_profile.provider,
                "runtime-routing-model-clear",
            );
        }
        if args.list {
            let catalog = self.runtime_model_catalog_for_provider(&active_profile.provider)?;
            self.record_agent_provider_quota_usage(pane_id, &catalog.quota_usage);
            return Ok(AgentShellCommandOutcome::Display {
                command: "model".to_string(),
                body: runtime_model_catalog_display(&routing_name, &routing_profile, &catalog),
            });
        }
        if args.profile.is_none() || args.show {
            return Ok(AgentShellCommandOutcome::Display {
                command: "model".to_string(),
                body: runtime_routing_model_profile_display(
                    &routing_name,
                    &routing_profile,
                    &active_profile,
                ),
            });
        }
        let requested = args
            .profile
            .ok_or_else(|| MezError::invalid_args("model command requires a profile name"))?;
        let profile_name = if args.reasoning_profile.is_none()
            && self.provider_registry().profile(requested).is_some()
        {
            requested.to_string()
        } else {
            let catalog = self.runtime_model_catalog_for_provider(&active_profile.provider)?;
            self.runtime_generated_profile_for_provider_model(
                &active_profile.provider,
                requested,
                args.reasoning_profile,
                None,
                &catalog,
            )?
        };
        self.set_routing_model_profile_outcome(
            pane_id,
            &profile_name,
            &active_profile.provider,
            "runtime-routing-model-selection",
        )
    }

    /// Returns the currently configured routing auto-sizing model profile.
    fn active_routing_model_profile(&self) -> Result<(String, ModelProfile)> {
        let profile_name = self.agent_auto_sizing().router_model_profile.clone();
        let profile = self.provider_registry().resolve_profile(&profile_name)?;
        Ok((profile_name, profile))
    }
    /// Applies a routing auto-sizing model profile after provider validation.
    fn set_routing_model_profile_outcome(
        &mut self,
        pane_id: &str,
        profile_name: &str,
        active_provider: &str,
        source: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let profile = self.provider_registry().resolve_profile(profile_name)?;
        if profile.provider != active_provider {
            return Err(MezError::config(format!(
                "routing model profile `{profile_name}` uses provider `{}`, but active provider is `{active_provider}`",
                profile.provider
            )));
        }
        self.set_agent_router_model_profile(profile_name);
        Ok(AgentShellCommandOutcome::Mutated {
            command: "model".to_string(),
            body: format!(
                "scope=routing profile={} provider={} model={} reasoning_profile={} source={}",
                json_escape(profile_name),
                json_escape(&profile.provider),
                json_escape(&profile.model),
                profile.reasoning_profile.as_deref().unwrap_or("none"),
                json_escape(source)
            ),
            visibility: self
                .agent_shell_store()
                .get(pane_id)
                .map(|session| session.visibility)
                .unwrap_or(AgentShellVisibility::Hidden),
        })
    }

    /// Returns configured provider model names for the pane's active provider.
    ///
    /// The picker path is intentionally synchronous, so it uses configured
    /// provider/profile metadata rather than live provider HTTP. The `/model
    /// list` command remains the richer async path for network-backed catalogs.
    pub(crate) fn configured_model_names_for_pane(&mut self, pane_id: &str) -> Result<Vec<String>> {
        let agent_id = format!("agent-{pane_id}");
        let (_active_name, active_profile) =
            self.active_model_profile_for_pane(pane_id, &agent_id, None)?;
        let active_model_label = format!("{}: {}", active_profile.provider, active_profile.model);
        let mut models: Vec<String> = self
            .integration
            .preset_registry()
            .presets
            .keys()
            .map(|preset| format!("preset: {preset}"))
            .collect();
        let mut provider_ids: Vec<String> = self
            .provider_registry()
            .providers()
            .keys()
            .cloned()
            .collect();
        if let Some(auth_store) = self.integration.auth_store() {
            let all_metadata = auth_store.read_all_metadata().unwrap_or_default();
            for auth_provider in all_metadata.keys() {
                if !provider_ids.contains(auth_provider) {
                    provider_ids.push(auth_provider.clone());
                }
            }
        }
        for provider_id in &provider_ids {
            let models_for_provider: Vec<String> = if let Some(provider_config) =
                self.provider_registry().provider(provider_id).cloned()
            {
                let observed = self.runtime_model_catalog_for_provider(provider_id)?;
                let configured = super::model_catalog::runtime_configured_model_catalog(
                    provider_id,
                    &provider_config,
                    self.provider_registry(),
                );
                let merged = super::ModelCatalog::from_input(super::ModelCatalogInput {
                    candidates: observed
                        .catalog
                        .entries()
                        .iter()
                        .chain(configured.catalog.entries())
                        .map(super::ModelCatalogEntry::to_candidate)
                        .collect(),
                    default_model: provider_config.default_model.clone(),
                    recommended_model: configured.catalog.preferred_model().map(str::to_string),
                    reasoning_levels: observed.catalog.reasoning_levels().to_vec(),
                });
                merged
                    .available_entries()
                    .map(|model| format!("{provider_id}: {}", model.id))
                    .collect()
            } else {
                runtime_default_models_for_provider(provider_id)
                    .map(|models| {
                        models
                            .iter()
                            .map(|m| format!("{provider_id}: {m}"))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            for label in models_for_provider {
                if !models.iter().any(|m: &String| m == &label) {
                    models.push(label);
                }
            }
        }
        if !models.iter().any(|m| m == &active_model_label) {
            models.insert(0, active_model_label);
        }
        Ok(models)
    }
    /// Returns the preset name encoded in one pane-frame model picker entry.
    pub(super) fn pane_model_picker_preset_name<'a>(&self, value: &'a str) -> Option<&'a str> {
        value.strip_prefix("preset: ")
    }

    /// Returns configured reasoning choices for a pane model picker.
    pub(crate) fn configured_reasoning_levels_for_pane_model(
        &mut self,
        pane_id: &str,
        model_name: &str,
    ) -> Result<Vec<String>> {
        let agent_id = format!("agent-{pane_id}");
        let (_active_name, active_profile) =
            self.active_model_profile_for_pane(pane_id, &agent_id, None)?;
        let catalog = self.runtime_model_catalog_for_provider(&active_profile.provider)?;
        let provider_config = self.provider_registry().provider(&active_profile.provider);
        let mut levels = catalog
            .catalog
            .reasoning_levels_for(model_name)
            .map(<[String]>::to_vec)
            .unwrap_or_else(|| {
                provider_config
                    .map(|provider_config| {
                        runtime_configured_reasoning_levels_for_model(provider_config, model_name)
                    })
                    .unwrap_or_default()
            });
        if let Some(reasoning) = active_profile.reasoning_profile
            && !levels.iter().any(|level| level == &reasoning)
        {
            levels.insert(0, reasoning);
        }
        Ok(normalize_model_catalog_values(levels))
    }

    /// Applies a model selected from the pane-frame model picker.
    pub(crate) fn apply_pane_model_picker_selection(
        &mut self,
        pane_id: &str,
        model_label: &str,
    ) -> Result<AgentShellCommandOutcome> {
        if let Some(preset_name) = self.pane_model_picker_preset_name(model_label) {
            return self.apply_preset_selection(pane_id, preset_name);
        }
        let agent_id = format!("agent-{pane_id}");
        let (provider_id, model_name) = parse_picker_model_label(model_label);
        let catalog = self.runtime_model_catalog_for_provider(provider_id)?;
        let requested_reasoning = self
            .active_model_profile_for_pane(pane_id, &agent_id, None)
            .ok()
            .and_then(|(_name, active_profile)| {
                if active_profile.provider == provider_id {
                    active_profile.reasoning_profile
                } else {
                    None
                }
            })
            .filter(|reasoning| catalog.catalog.select(model_name, Some(reasoning)).is_ok());
        let model_name = model_name.to_string();
        let requested_reasoning = requested_reasoning.as_deref();
        self.apply_pane_model_picker_profile(pane_id, &model_name, requested_reasoning, &catalog)
    }

    /// Applies a reasoning level selected from the pane-frame reasoning picker.
    pub(crate) fn apply_pane_reasoning_picker_selection(
        &mut self,
        pane_id: &str,
        reasoning: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let agent_id = format!("agent-{pane_id}");
        let (_active_name, active_profile) =
            self.active_model_profile_for_pane(pane_id, &agent_id, None)?;
        let catalog = self.runtime_model_catalog_for_provider(&active_profile.provider)?;
        let model_name = active_profile.model.clone();
        self.apply_pane_model_picker_profile(pane_id, &model_name, Some(reasoning), &catalog)
    }

    /// Applies a model preset selected from the pane-frame preset picker.
    pub(super) fn apply_preset_selection(
        &mut self,
        pane_id: &str,
        preset_name: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let Some(preset) = self
            .integration
            .preset_registry()
            .resolve(preset_name)
            .cloned()
        else {
            return Err(MezError::invalid_args(format!(
                "model preset `{preset_name}` is not configured"
            )));
        };
        let agent_id = format!("agent-{pane_id}");
        let (_active_name, _active_profile) =
            self.active_model_profile_for_pane(pane_id, &agent_id, None)?;
        let new_profile = self
            .provider_registry()
            .resolve_profile(&preset.default_model_profile)?;
        let provider_id = new_profile.provider.clone();
        let model_name = new_profile.model.clone();
        let new_reasoning = new_profile.reasoning_profile.clone();
        let new_latency = new_profile.latency_preference.clone();
        let catalog = self.runtime_model_catalog_for_provider(&provider_id)?;
        let profile_name = self.runtime_generated_profile_for_provider_model(
            &provider_id,
            &model_name,
            new_reasoning.as_deref(),
            new_latency.as_deref(),
            &catalog,
        )?;
        let router = preset.auto_sizing_router_model_profile.clone();
        let scope = RuntimeModelProfileOverrideScope::Pane(pane_id.to_string());
        self.set_model_profile_override(scope.clone(), &profile_name)?;
        let mut auto_sizing = self.runtime_auto_sizing_config_for_pane(pane_id).clone();
        auto_sizing.router_model_profile = router.clone();
        auto_sizing.small_model_profile = preset.auto_sizing_small_model_profile.clone();
        auto_sizing.medium_model_profile = preset.auto_sizing_medium_model_profile.clone();
        auto_sizing.large_model_profile = preset.auto_sizing_large_model_profile.clone();
        if !preset.allowed_reasoning_efforts.is_empty() {
            auto_sizing.allowed_reasoning_efforts = preset.allowed_reasoning_efforts.clone();
        }
        self.set_agent_auto_sizing_override(pane_id, Some(auto_sizing));
        let resolved = self.provider_registry().resolve_profile(&profile_name)?;
        Ok(AgentShellCommandOutcome::Mutated {
            command: "preset".to_string(),
            body: format!(
                "scope={} preset={} profile={} provider={} model={} reasoning_profile={} latency_preference={} router={} source=runtime-preset-selection",
                runtime_model_override_scope_name(&scope),
                preset_name,
                profile_name,
                resolved.provider,
                resolved.model,
                resolved.reasoning_profile.as_deref().unwrap_or("none"),
                resolved.latency_preference.as_deref().unwrap_or("default"),
                router,
            ),
            visibility: self
                .agent_shell_store()
                .get(pane_id)
                .map(|session| session.visibility)
                .unwrap_or(AgentShellVisibility::Hidden),
        })
    }

    /// Returns the auto-sizing configuration currently active for one pane.
    pub(crate) fn runtime_auto_sizing_config_for_pane(
        &self,
        pane_id: &str,
    ) -> &RuntimeAutoSizingConfig {
        self.agent_auto_sizing_for_pane(pane_id)
    }

    /// Returns the preset label to render for one pane, when presets exist.
    pub(crate) fn agent_preset_display_value_for_pane(&self, pane_id: &str) -> Option<String> {
        if !self.integration.preset_registry().has_presets() {
            return None;
        }
        Some(
            self.active_model_preset_name_for_pane(pane_id)
                .unwrap_or_else(|| "custom".to_string()),
        )
    }

    /// Returns the active model preset name when the pane state matches one.
    pub(crate) fn active_model_preset_name_for_pane(&self, pane_id: &str) -> Option<String> {
        if !self.integration.preset_registry().has_presets() {
            return None;
        }
        let agent_id = format!("agent-{pane_id}");
        let (_active_name, active_profile) = self
            .active_model_profile_for_pane(pane_id, &agent_id, None)
            .ok()?;
        let auto_sizing = self.runtime_auto_sizing_config_for_pane(pane_id);
        self.integration
            .preset_registry()
            .presets
            .iter()
            .find_map(|(preset_name, preset)| {
                let preset_profile = self
                    .provider_registry()
                    .resolve_profile(&preset.default_model_profile)
                    .ok()?;
                (runtime_model_profile_matches_preset_profile(&active_profile, &preset_profile)
                    && runtime_auto_sizing_matches_preset(auto_sizing, preset))
                .then(|| preset_name.clone())
            })
    }

    /// Applies a latency preference selected from the pane-frame latency picker.
    pub(crate) fn apply_pane_latency_picker_selection(
        &mut self,
        pane_id: &str,
        latency: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let agent_id = format!("agent-{pane_id}");
        let (_active_name, active_profile) =
            self.active_model_profile_for_pane(pane_id, &agent_id, None)?;
        let latency = runtime_validate_latency_preference(latency)?;
        let catalog = self.runtime_model_catalog_for_provider(&active_profile.provider)?;
        let profile_name = self.runtime_generated_profile_for_provider_model(
            &active_profile.provider,
            &active_profile.model,
            active_profile.reasoning_profile.as_deref(),
            Some(latency),
            &catalog,
        )?;
        let scope = RuntimeModelProfileOverrideScope::Pane(pane_id.to_string());
        self.set_model_profile_override(scope.clone(), &profile_name)?;
        let profile = self.provider_registry().resolve_profile(&profile_name)?;
        Ok(AgentShellCommandOutcome::Mutated {
            command: "latency".to_string(),
            body: format!(
                "scope={} profile={} provider={} model={} reasoning_profile={} latency_preference={} source=runtime-latency-picker",
                runtime_model_override_scope_name(&scope),
                profile_name,
                profile.provider,
                profile.model,
                profile.reasoning_profile.as_deref().unwrap_or("none"),
                profile.latency_preference.as_deref().unwrap_or("default")
            ),
            visibility: self
                .agent_shell_store()
                .get(pane_id)
                .map(|session| session.visibility)
                .unwrap_or(AgentShellVisibility::Hidden),
        })
    }

    /// Applies a generated pane-scoped model profile for picker selections.
    fn apply_pane_model_picker_profile(
        &mut self,
        pane_id: &str,
        model_name: &str,
        reasoning: Option<&str>,
        catalog: &RuntimeModelCatalog,
    ) -> Result<AgentShellCommandOutcome> {
        let provider_id = catalog.provider.clone();
        let agent_id = format!("agent-{pane_id}");
        let latency = self
            .active_model_profile_for_pane(pane_id, &agent_id, None)
            .ok()
            .and_then(|(_active_name, active_profile)| {
                let new_provider_supports_latency = self
                    .provider_registry()
                    .provider(&provider_id)
                    .is_some_and(|provider| {
                        ProviderCapabilities::for_provider_config(
                            &provider.kind,
                            provider.api.as_deref(),
                        )
                        .map(|capabilities| capabilities.supports_service_tier)
                        .unwrap_or(false)
                    });
                (new_provider_supports_latency
                    && self.model_profile_supports_latency_preference(&active_profile))
                .then(|| {
                    active_profile
                        .latency_preference
                        .unwrap_or_else(|| "default".to_string())
                })
            });
        let profile_name = self.runtime_generated_profile_for_provider_model(
            &provider_id,
            model_name,
            reasoning,
            latency.as_deref(),
            catalog,
        )?;
        let scope = RuntimeModelProfileOverrideScope::Pane(pane_id.to_string());
        self.set_model_profile_override(scope.clone(), &profile_name)?;
        let profile = self.provider_registry().resolve_profile(&profile_name)?;
        Ok(AgentShellCommandOutcome::Mutated {
            command: "model".to_string(),
            body: format!(
                "scope={} profile={} provider={} model={} reasoning_profile={} source=runtime-model-picker",
                runtime_model_override_scope_name(&scope),
                profile_name,
                profile.provider,
                profile.model,
                profile.reasoning_profile.as_deref().unwrap_or("none")
            ),
            visibility: self
                .agent_shell_store()
                .get(pane_id)
                .map(|session| session.visibility)
                .unwrap_or(AgentShellVisibility::Hidden),
        })
    }
}

/// Parses a picker model label in `provider: model` format.
fn parse_picker_model_label(label: &str) -> (&str, &str) {
    match label.split_once(": ") {
        Some((provider, model)) => (provider, model),
        None => ("openai", label),
    }
}

/// Reports whether a live model profile is equivalent to a preset default.
fn runtime_model_profile_matches_preset_profile(
    active: &ModelProfile,
    preset: &ModelProfile,
) -> bool {
    active.provider == preset.provider
        && active.model == preset.model
        && active.reasoning_profile == preset.reasoning_profile
        && active.latency_preference.as_deref().unwrap_or("default")
            == preset.latency_preference.as_deref().unwrap_or("default")
}

/// Reports whether one auto-sizing configuration matches a preset.
fn runtime_auto_sizing_matches_preset(
    config: &RuntimeAutoSizingConfig,
    preset: &RuntimeModelPreset,
) -> bool {
    config.router_model_profile == preset.auto_sizing_router_model_profile
        && config.small_model_profile == preset.auto_sizing_small_model_profile
        && config.medium_model_profile == preset.auto_sizing_medium_model_profile
        && config.large_model_profile == preset.auto_sizing_large_model_profile
        && (preset.allowed_reasoning_efforts.is_empty()
            || config.allowed_reasoning_efforts == preset.allowed_reasoning_efforts)
}

impl RuntimeSessionService {
    /// Runs the runtime generated profile for provider model operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn runtime_generated_profile_for_provider_model(
        &mut self,
        provider_id: &str,
        model_name: &str,
        reasoning_profile: Option<&str>,
        latency_preference: Option<&str>,
        catalog: &RuntimeModelCatalog,
    ) -> Result<String> {
        let selection = catalog
            .catalog
            .select(model_name, reasoning_profile)
            .map_err(|error| match error.kind() {
                ModelCatalogSelectionErrorKind::UnknownModel => MezError::invalid_args(format!(
                    "model `{model_name}` is not available from provider `{provider_id}`; run `/model list`"
                )),
                ModelCatalogSelectionErrorKind::EmptyModel
                | ModelCatalogSelectionErrorKind::UnavailableModel
                | ModelCatalogSelectionErrorKind::EmptyReasoning
                | ModelCatalogSelectionErrorKind::UnknownReasoning => {
                    MezError::invalid_args(error.message())
                }
            })?;
        let model = &selection.model;

        let mut provider_options = std::collections::BTreeMap::new();
        if let Some(context_window_tokens) = model.context_window_tokens {
            provider_options.insert(
                "context_window_tokens".to_string(),
                context_window_tokens.to_string(),
            );
        }
        if !model.capabilities.is_empty() {
            provider_options.insert(
                "model_capabilities".to_string(),
                model.capabilities.join(","),
            );
        }
        let latency_preference = latency_preference
            .map(runtime_validate_latency_preference)
            .transpose()?
            .map(str::to_string);
        let profile = ModelProfile {
            provider: provider_id.to_string(),
            model: model.id.clone(),
            reasoning_profile: selection.reasoning.clone(),
            latency_preference,
            multimodal_required: false,
            provider_options,
            safety_tier: None,
        };
        let profile_name = runtime_generated_model_profile_name(
            self.provider_registry(),
            provider_id,
            &model.id,
            selection.reasoning.as_deref(),
            &profile,
        );
        self.integration
            .provider_registry_mut()
            .profiles
            .entry(profile_name.clone())
            .or_insert(profile);
        Ok(profile_name)
    }

    /// Inserts a runtime-generated profile while preserving all provider
    /// options carried by the supplied profile.
    fn insert_runtime_generated_model_profile(&mut self, profile: ModelProfile) -> String {
        let profile_name = runtime_generated_model_profile_name(
            self.provider_registry(),
            &profile.provider,
            &profile.model,
            profile.reasoning_profile.as_deref(),
            &profile,
        );
        self.integration
            .provider_registry_mut()
            .profiles
            .entry(profile_name.clone())
            .or_insert(profile);
        profile_name
    }

    pub(crate) fn active_model_profile_for_pane(
        &self,
        pane_id: &str,
        agent_id: &str,
        subagent_id: Option<&str>,
    ) -> Result<(String, ModelProfile)> {
        let default_profile = self
            .provider_registry()
            .default_profile_name()
            .ok_or_else(|| MezError::config("default model profile is not configured"))?;
        let window_id = self
            .find_pane_descriptor(pane_id)
            .map(|descriptor| descriptor.window_id.to_string());
        let model_profile_overrides = self.integration.model_profile_overrides();
        let overrides = ModelProfileOverrides {
            default_profile: self
                .agent_selected_personality_profile(pane_id)
                .and_then(|profile| profile.model_profile.clone()),
            session_profile: model_profile_overrides.session_profile.clone(),
            window_profile: window_id
                .as_deref()
                .and_then(|id| model_profile_overrides.window_profiles.get(id).cloned()),
            pane_profile: model_profile_overrides.pane_profiles.get(pane_id).cloned(),
            agent_profile: model_profile_overrides
                .agent_profiles
                .get(agent_id)
                .cloned(),
            subagent_profile: subagent_id
                .and_then(|id| model_profile_overrides.subagent_profiles.get(id).cloned()),
        };
        let selection = select_model_profile(&overrides, default_profile)?;
        let profile = self
            .provider_registry()
            .resolve_profile(&selection.profile)?;
        Ok((selection.profile, profile))
    }

    /// Runs the set model profile override operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn set_model_profile_override(
        &mut self,
        scope: RuntimeModelProfileOverrideScope,
        profile_name: &str,
    ) -> Result<()> {
        self.provider_registry().resolve_profile(profile_name)?;
        let overrides = self.integration.model_profile_overrides_mut();
        match scope {
            RuntimeModelProfileOverrideScope::Session => {
                overrides.session_profile = Some(profile_name.to_string());
            }
            RuntimeModelProfileOverrideScope::Window(window_id) => {
                overrides
                    .window_profiles
                    .insert(window_id, profile_name.to_string());
            }
            RuntimeModelProfileOverrideScope::Pane(pane_id) => {
                overrides
                    .pane_profiles
                    .insert(pane_id, profile_name.to_string());
            }
            RuntimeModelProfileOverrideScope::Agent(agent_id) => {
                overrides
                    .agent_profiles
                    .insert(agent_id, profile_name.to_string());
            }
            RuntimeModelProfileOverrideScope::Subagent(agent_id) => {
                overrides
                    .subagent_profiles
                    .insert(agent_id, profile_name.to_string());
            }
        }
        Ok(())
    }

    /// Runs the clear model profile override operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn clear_model_profile_override(&mut self, scope: RuntimeModelProfileOverrideScope) {
        let overrides = self.integration.model_profile_overrides_mut();
        match scope {
            RuntimeModelProfileOverrideScope::Session => {
                overrides.session_profile = None;
            }
            RuntimeModelProfileOverrideScope::Window(window_id) => {
                overrides.window_profiles.remove(&window_id);
            }
            RuntimeModelProfileOverrideScope::Pane(pane_id) => {
                overrides.pane_profiles.remove(&pane_id);
            }
            RuntimeModelProfileOverrideScope::Agent(agent_id) => {
                overrides.agent_profiles.remove(&agent_id);
            }
            RuntimeModelProfileOverrideScope::Subagent(agent_id) => {
                overrides.subagent_profiles.remove(&agent_id);
            }
        }
    }

    /// Runs the inherited model profile for child agent operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn inherited_model_profile_for_child_agent(
        &self,
        parent_agent_id: &str,
    ) -> Option<String> {
        if let Some(profile) = self
            .integration
            .model_profile_overrides()
            .agent_profiles
            .get(parent_agent_id)
        {
            return Some(profile.clone());
        }
        let parent_pane = parent_agent_id.strip_prefix("agent-")?;
        let default_profile = self.provider_registry().default_profile_name()?;
        self.active_model_profile_for_pane(parent_pane, parent_agent_id, None)
            .ok()
            .map(|(profile, _)| profile)
            .filter(|profile| profile != default_profile)
    }
}

/// Borrowed argument view for `/model --routing`.
///
/// The public parser lives in the runtime config module; this compact local
/// view keeps command execution readable without exposing parser internals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RuntimeRoutingModelCommandArgs<'a> {
    /// Optional requested model profile or provider model name.
    profile: Option<&'a str>,
    /// Optional reasoning profile or effort requested with the model.
    reasoning_profile: Option<&'a str>,
    /// Whether to reset the routing model to the default router profile.
    clear: bool,
    /// Whether to list models for the active provider.
    list: bool,
    /// Whether to show the current routing model.
    show: bool,
}

/// Runs the runtime generated model profile name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_generated_model_profile_name(
    registry: &crate::runtime::RuntimeProviderRegistry,
    provider_id: &str,
    model: &str,
    reasoning_profile: Option<&str>,
    profile: &ModelProfile,
) -> String {
    let base = match reasoning_profile {
        Some(reasoning) => format!("{model}:{reasoning}"),
        None => model.to_string(),
    };
    let base = match profile.thinking_enabled() {
        Some(true) => format!("{base}:thinking-on"),
        Some(false) => format!("{base}:thinking-off"),
        None => base,
    };
    let preferred = match profile
        .latency_preference
        .as_deref()
        .filter(|latency| *latency != "default")
    {
        Some(latency) => format!("{base}:{latency}"),
        None => base,
    };
    if runtime_profile_name_available_or_matching(registry, &preferred, profile) {
        return preferred;
    }
    let mut candidate = format!("{provider_id}:{preferred}");
    if runtime_profile_name_available_or_matching(registry, &candidate, profile) {
        return candidate;
    }
    for index in 2usize.. {
        candidate = format!("{provider_id}:{preferred}:{index}");
        if runtime_profile_name_available_or_matching(registry, &candidate, profile) {
            return candidate;
        }
    }
    unreachable!("usize iteration should find an available generated model profile name")
}

/// Runs the runtime profile name available or matching operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_profile_name_available_or_matching(
    registry: &crate::runtime::RuntimeProviderRegistry,
    name: &str,
    profile: &ModelProfile,
) -> bool {
    registry.profile(name).is_none_or(|existing| {
        existing.provider == profile.provider
            && existing.model == profile.model
            && existing.reasoning_profile == profile.reasoning_profile
            && existing.provider_options == profile.provider_options
            && existing.latency_preference.as_deref().unwrap_or("default")
                == profile.latency_preference.as_deref().unwrap_or("default")
    })
}
