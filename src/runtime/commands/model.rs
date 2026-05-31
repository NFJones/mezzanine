//! Agent model, routing-model, latency, and provider catalog commands.
//!
//! This module owns pane-local model profile selection, provider catalog
//! lookup and caching, generated runtime model profiles, routing-model
//! selection, latency preferences, and picker state derived from those
//! profiles. It keeps provider-catalog mechanics separate from the broader
//! agent-shell command dispatcher.

use super::*;

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
                    .agent_shell_store
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
                    self.provider_registry.profiles(),
                ),
            });
        }
        let requested = args
            .profile
            .as_deref()
            .ok_or_else(|| MezError::invalid_args("model command requires a profile name"))?;
        let profile_name = if args.reasoning_profile.is_none()
            && self.provider_registry.profile(requested).is_some()
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
        let profile = self.provider_registry.resolve_profile(&profile_name)?;
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
                .agent_shell_store
                .get(pane_id)
                .map(|session| session.visibility)
                .unwrap_or(AgentShellVisibility::Hidden),
        })
    }

    /// Runs the execute agent shell model command async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) async fn execute_agent_shell_model_command_async(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("model command must be a slash command"))?;
        let agent_id = format!("agent-{pane_id}");
        let args = runtime_model_command_args(&invocation.args)?;
        if args.routing {
            return self
                .execute_agent_shell_routing_model_command_async(
                    pane_id,
                    &agent_id,
                    RuntimeRoutingModelCommandArgs {
                        profile: args.profile.as_deref(),
                        reasoning_profile: args.reasoning_profile.as_deref(),
                        clear: args.clear,
                        list: args.list,
                        show: args.show,
                    },
                )
                .await;
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
                    .agent_shell_store
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
                    self.provider_registry.profiles(),
                ),
            });
        }
        let requested = args
            .profile
            .as_deref()
            .ok_or_else(|| MezError::invalid_args("model command requires a profile name"))?;
        let profile_name = if args.reasoning_profile.is_none()
            && self.provider_registry.profile(requested).is_some()
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
        let profile = self.provider_registry.resolve_profile(&profile_name)?;
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
                .agent_shell_store
                .get(pane_id)
                .map(|session| session.visibility)
                .unwrap_or(AgentShellVisibility::Hidden),
        })
    }

    /// Executes `/latency` as a pane-local model-profile latency preference override.
    pub(in crate::runtime) fn execute_agent_shell_latency_command(
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
        let profile = self.provider_registry.resolve_profile(&profile_name)?;
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
                .agent_shell_store
                .get(pane_id)
                .map(|session| session.visibility)
                .unwrap_or(AgentShellVisibility::Hidden),
        })
    }

    /// Reports whether the provider behind one model profile supports
    /// provider-visible latency preferences.
    pub(in crate::runtime) fn model_profile_supports_latency_preference(
        &self,
        profile: &ModelProfile,
    ) -> bool {
        self.provider_registry
            .provider(&profile.provider)
            .is_some_and(|provider| {
                ProviderCapabilities::for_provider_config(&provider.kind, provider.api.as_deref())
                    .map(|capabilities| capabilities.supports_service_tier)
                    .unwrap_or(false)
            })
    }

    /// Executes `/thinking` as a pane-local provider thinking-mode override.
    pub(in crate::runtime) fn execute_agent_shell_thinking_command(
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
        let profile = self.provider_registry.resolve_profile(&profile_name)?;
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
                .agent_shell_store
                .get(pane_id)
                .map(|session| session.visibility)
                .unwrap_or(AgentShellVisibility::Hidden),
        })
    }

    /// Reports whether one profile supports and currently enables provider
    /// thinking mode.
    pub(in crate::runtime) fn model_profile_thinking_enabled(
        &self,
        profile: &ModelProfile,
    ) -> Option<bool> {
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
    pub(in crate::runtime) fn model_profile_supports_thinking_toggle(
        &self,
        profile: &ModelProfile,
    ) -> bool {
        self.provider_registry
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
            && self.provider_registry.profile(requested).is_some()
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
    /// Async variant of `/model --routing` for provider catalog lookups.
    async fn execute_agent_shell_routing_model_command_async(
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
            && self.provider_registry.profile(requested).is_some()
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
        let profile_name = self.agent_auto_sizing.router_model_profile.clone();
        let profile = self.provider_registry.resolve_profile(&profile_name)?;
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
        let profile = self.provider_registry.resolve_profile(profile_name)?;
        if profile.provider != active_provider {
            return Err(MezError::config(format!(
                "routing model profile `{profile_name}` uses provider `{}`, but active provider is `{active_provider}`",
                profile.provider
            )));
        }
        self.agent_auto_sizing.router_model_profile = profile_name.to_string();
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
                .agent_shell_store
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
    pub(in crate::runtime) fn configured_model_names_for_pane(
        &mut self,
        pane_id: &str,
    ) -> Result<Vec<String>> {
        let agent_id = format!("agent-{pane_id}");
        let (_active_name, active_profile) =
            self.active_model_profile_for_pane(pane_id, &agent_id, None)?;
        let active_model_label = format!("{}: {}", active_profile.provider, active_profile.model);
        let mut models: Vec<String> = self
            .preset_registry
            .presets
            .keys()
            .map(|preset| format!("preset: {preset}"))
            .collect();
        let mut provider_ids: Vec<String> =
            self.provider_registry.providers().keys().cloned().collect();
        if let Some(auth_store) = self.auth_store.as_ref() {
            let all_metadata = auth_store.read_all_metadata().unwrap_or_default();
            for auth_provider in all_metadata.keys() {
                if !provider_ids.contains(auth_provider) {
                    provider_ids.push(auth_provider.clone());
                }
            }
        }
        for provider_id in &provider_ids {
            let models_for_provider: Vec<String> = if let Some(provider_config) =
                self.provider_registry.provider(provider_id).cloned()
            {
                let catalog = self.runtime_model_catalog_for_provider(provider_id)?;
                let mut items: Vec<String> = catalog
                    .models
                    .iter()
                    .map(|model| format!("{provider_id}: {}", model.id))
                    .collect();
                let configured_items: Vec<String> = if provider_config.models.is_empty() {
                    runtime_provider_default_models(&provider_config)
                } else {
                    provider_config.models.clone()
                }
                .iter()
                .map(|m| format!("{provider_id}: {m}"))
                .filter(|label| !items.iter().any(|i| i == label))
                .collect();
                items.extend(configured_items);
                items
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
    pub(in crate::runtime) fn configured_reasoning_levels_for_pane_model(
        &mut self,
        pane_id: &str,
        model_name: &str,
    ) -> Result<Vec<String>> {
        let agent_id = format!("agent-{pane_id}");
        let (_active_name, active_profile) =
            self.active_model_profile_for_pane(pane_id, &agent_id, None)?;
        let catalog = self.runtime_model_catalog_for_provider(&active_profile.provider)?;
        let provider_config = self.provider_registry.provider(&active_profile.provider);
        let mut levels = catalog
            .models
            .iter()
            .find(|model| model.id == model_name)
            .map(|model| {
                if model.reasoning_levels.is_empty() {
                    catalog.reasoning_levels.clone()
                } else {
                    model.reasoning_levels.clone()
                }
            })
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
        Ok(dedupe_runtime_strings(levels))
    }

    /// Applies a model selected from the pane-frame model picker.
    pub(in crate::runtime) fn apply_pane_model_picker_selection(
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
            .filter(|reasoning| {
                catalog
                    .models
                    .iter()
                    .find(|model| model.id == model_name)
                    .map(|model| {
                        let levels = if model.reasoning_levels.is_empty() {
                            catalog.reasoning_levels.as_slice()
                        } else {
                            model.reasoning_levels.as_slice()
                        };
                        levels.is_empty() || levels.iter().any(|level| level == reasoning)
                    })
                    .unwrap_or(false)
            });
        let model_name = model_name.to_string();
        let requested_reasoning = requested_reasoning.as_deref();
        self.apply_pane_model_picker_profile(pane_id, &model_name, requested_reasoning, &catalog)
    }

    /// Applies a reasoning level selected from the pane-frame reasoning picker.
    pub(in crate::runtime) fn apply_pane_reasoning_picker_selection(
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
        let Some(preset) = self.preset_registry.resolve(preset_name).cloned() else {
            return Err(MezError::invalid_args(format!(
                "model preset `{preset_name}` is not configured"
            )));
        };
        let agent_id = format!("agent-{pane_id}");
        let (_active_name, _active_profile) =
            self.active_model_profile_for_pane(pane_id, &agent_id, None)?;
        let new_profile = self
            .provider_registry
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
        self.agent_auto_sizing_overrides
            .insert(pane_id.to_string(), auto_sizing);
        let resolved = self.provider_registry.resolve_profile(&profile_name)?;
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
                .agent_shell_store
                .get(pane_id)
                .map(|session| session.visibility)
                .unwrap_or(AgentShellVisibility::Hidden),
        })
    }

    /// Returns the auto-sizing configuration currently active for one pane.
    pub(in crate::runtime) fn runtime_auto_sizing_config_for_pane(
        &self,
        pane_id: &str,
    ) -> &RuntimeAutoSizingConfig {
        self.agent_auto_sizing_overrides
            .get(pane_id)
            .unwrap_or(&self.agent_auto_sizing)
    }

    /// Returns the preset label to render for one pane, when presets exist.
    pub(in crate::runtime) fn agent_preset_display_value_for_pane(
        &self,
        pane_id: &str,
    ) -> Option<String> {
        if !self.preset_registry.has_presets() {
            return None;
        }
        Some(
            self.active_model_preset_name_for_pane(pane_id)
                .unwrap_or_else(|| "custom".to_string()),
        )
    }

    /// Returns the active model preset name when the pane state matches one.
    pub(in crate::runtime) fn active_model_preset_name_for_pane(
        &self,
        pane_id: &str,
    ) -> Option<String> {
        if !self.preset_registry.has_presets() {
            return None;
        }
        let agent_id = format!("agent-{pane_id}");
        let (_active_name, active_profile) = self
            .active_model_profile_for_pane(pane_id, &agent_id, None)
            .ok()?;
        let auto_sizing = self.runtime_auto_sizing_config_for_pane(pane_id);
        self.preset_registry
            .presets
            .iter()
            .find_map(|(preset_name, preset)| {
                let preset_profile = self
                    .provider_registry
                    .resolve_profile(&preset.default_model_profile)
                    .ok()?;
                (runtime_model_profile_matches_preset_profile(&active_profile, &preset_profile)
                    && runtime_auto_sizing_matches_preset(auto_sizing, preset))
                .then(|| preset_name.clone())
            })
    }

    /// Applies a latency preference selected from the pane-frame latency picker.
    pub(in crate::runtime) fn apply_pane_latency_picker_selection(
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
        let profile = self.provider_registry.resolve_profile(&profile_name)?;
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
                .agent_shell_store
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
                    .provider_registry
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
        let profile = self.provider_registry.resolve_profile(&profile_name)?;
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
                .agent_shell_store
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
    fn runtime_model_catalog_for_provider(
        &mut self,
        provider_id: &str,
    ) -> Result<RuntimeModelCatalog> {
        if let Some(catalog) = self.provider_model_catalog_cache.get(provider_id) {
            return Ok(catalog.clone());
        }
        let provider_config = self
            .provider_registry
            .provider(provider_id)
            .cloned()
            .ok_or_else(|| {
                MezError::config(format!("provider `{provider_id}` is not configured"))
            })?;
        let fallback = runtime_configured_model_catalog(
            provider_id,
            &provider_config,
            &self.provider_registry,
        );
        if let Some(provider_error) =
            self.runtime_cached_model_catalog_miss_reason(provider_id, &provider_config)
        {
            return Ok(RuntimeModelCatalog {
                provider: fallback.provider,
                source: fallback.source,
                provider_error: Some(provider_error),
                models: fallback.models,
                reasoning_levels: fallback.reasoning_levels,
                quota_usage: fallback.quota_usage,
            });
        }
        if matches!(
            effective_provider_api(&provider_config.kind, provider_config.api.as_deref()),
            Ok(ProviderApiCompatibility::OpenAiResponses)
        ) && fallback.models.is_empty()
        {
            return Err(MezError::invalid_state(
                "OpenAI Responses model listing requires cached provider information or configured fallback models",
            ));
        }
        Ok(fallback)
    }

    /// Runs the runtime model catalog for provider async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    async fn runtime_model_catalog_for_provider_async(
        &mut self,
        provider_id: &str,
    ) -> Result<RuntimeModelCatalog> {
        if let Some(catalog) = self.provider_model_catalog_cache.get(provider_id) {
            return Ok(catalog.clone());
        }
        let provider_config = self
            .provider_registry
            .provider(provider_id)
            .cloned()
            .ok_or_else(|| {
                MezError::config(format!("provider `{provider_id}` is not configured"))
            })?;
        let fallback = runtime_configured_model_catalog(
            provider_id,
            &provider_config,
            &self.provider_registry,
        );
        match effective_provider_api(&provider_config.kind, provider_config.api.as_deref())? {
            ProviderApiCompatibility::OpenAiResponses
            | ProviderApiCompatibility::OpenAiChatCompletions
            | ProviderApiCompatibility::DeepSeekChatCompletions => match self
                .runtime_api_model_catalog_async(provider_id, &provider_config)
                .await
            {
                Ok(catalog) => {
                    let catalog = RuntimeModelCatalog::from_provider(catalog);
                    self.provider_model_catalog_cache
                        .insert(provider_id.to_string(), catalog.clone());
                    Ok(catalog)
                }
                Err(error) if fallback.models.is_empty() => Err(error),
                Err(error) => Ok(RuntimeModelCatalog {
                    provider: fallback.provider,
                    source: "config".to_string(),
                    provider_error: Some(error.message().to_string()),
                    models: fallback.models,
                    reasoning_levels: fallback.reasoning_levels,
                    quota_usage: Vec::new(),
                }),
            },
        }
    }

    /// Explains why a cached provider catalog is not currently available
    /// without attempting network provider discovery.
    fn runtime_cached_model_catalog_miss_reason(
        &self,
        provider_id: &str,
        provider_config: &crate::runtime::RuntimeProviderConfig,
    ) -> Option<String> {
        let Ok(api) = effective_provider_api(&provider_config.kind, provider_config.api.as_deref())
        else {
            return None;
        };
        if !matches!(api, ProviderApiCompatibility::OpenAiResponses) {
            return None;
        }
        let Some(auth_store) = self.auth_store.as_ref() else {
            return Some(
                "OpenAI Responses model listing requires an attached auth store".to_string(),
            );
        };
        let metadata = match auth_store.read_metadata_for_provider(provider_id) {
            Ok(metadata) => metadata,
            Err(error) => return Some(error.message().to_string()),
        };
        let Some(metadata) = metadata else {
            return Some(format!(
                "OpenAI Responses provider `{provider_id}` requires an authenticated provider"
            ));
        };
        if metadata.credential_kind == AuthCredentialKind::ChatGpt {
            return Some(
                "ChatGPT browser credentials do not expose an OpenAI-compatible model catalog"
                    .to_string(),
            );
        }
        Some("OpenAI model catalog has not been refreshed; run :refresh-provider-info".to_string())
    }

    /// Refreshes cached provider information for every configured provider.
    pub(crate) async fn refresh_provider_info_async(&mut self) -> Result<String> {
        let provider_ids = self
            .provider_registry
            .providers()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut refreshed = 0usize;
        let mut failed = 0usize;
        let mut lines = Vec::new();
        for provider_id in &provider_ids {
            self.provider_model_catalog_cache.remove(provider_id);
            match self
                .runtime_model_catalog_for_provider_async(provider_id)
                .await
            {
                Ok(catalog) => {
                    refreshed = refreshed.saturating_add(1);
                    self.provider_model_catalog_cache
                        .insert(provider_id.clone(), catalog.clone());
                    let provider_error = catalog
                        .provider_error
                        .as_deref()
                        .map(runtime_model_catalog_unavailable_reason)
                        .unwrap_or_else(|| "none".to_string());
                    lines.push(format!(
                        "{} source={} models={} reasoning_levels={} quota_entries={} provider_error={}",
                        json_escape(provider_id),
                        json_escape(&catalog.source),
                        catalog.models.len(),
                        catalog.reasoning_levels.len(),
                        catalog.quota_usage.len(),
                        provider_error
                    ));
                }
                Err(error) => {
                    failed = failed.saturating_add(1);
                    lines.push(format!(
                        "{} refresh=failed error={}",
                        json_escape(provider_id),
                        json_escape(error.message())
                    ));
                }
            }
        }
        let mut body = format!(
            "providers={} refreshed={} failed={}",
            provider_ids.len(),
            refreshed,
            failed
        );
        if !lines.is_empty() {
            body.push('\n');
            body.push_str(&lines.join("\n"));
        }
        Ok(body)
    }

    /// Seeds the live model catalog cache for focused runtime tests.
    #[cfg(test)]
    pub(in crate::runtime) fn cache_provider_model_catalog_for_tests(
        &mut self,
        provider_id: &str,
        models: Vec<ProviderModelInfo>,
        reasoning_levels: Vec<String>,
    ) {
        self.provider_model_catalog_cache.insert(
            provider_id.to_string(),
            RuntimeModelCatalog {
                provider: provider_id.to_string(),
                source: "provider".to_string(),
                provider_error: None,
                models,
                reasoning_levels,
                quota_usage: Vec::new(),
            },
        );
    }

    /// Runs the runtime API model catalog async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    async fn runtime_api_model_catalog_async(
        &mut self,
        provider_id: &str,
        provider_config: &crate::runtime::RuntimeProviderConfig,
    ) -> Result<ProviderModelCatalog> {
        let api = effective_provider_api(&provider_config.kind, provider_config.api.as_deref())?;
        self.append_credential_access_audit(
            provider_id,
            &provider_config.auth_profile,
            "provider_model_list",
            "requested",
        )?;
        let Some(auth_store) = self.auth_store.as_ref() else {
            self.append_credential_access_audit(
                provider_id,
                &provider_config.auth_profile,
                "provider_model_list",
                "denied",
            )?;
            return Err(MezError::invalid_state(
                "provider model listing requires an attached auth store",
            ));
        };
        let metadata = auth_store
            .read_metadata_for_provider(provider_id)?
            .ok_or_else(|| {
                MezError::invalid_state(format!(
                    "provider `{provider_id}` model listing requires an authenticated provider"
                ))
            })?;
        if metadata.credential_kind == AuthCredentialKind::ChatGpt {
            self.append_credential_access_audit(
                provider_id,
                &provider_config.auth_profile,
                "provider_model_list",
                "unsupported",
            )?;
            return Err(MezError::invalid_state(
                "ChatGPT browser credentials do not expose an OpenAI-compatible model catalog",
            ));
        }
        let endpoint_override = provider_config
            .base_url
            .as_deref()
            .filter(|endpoint| !endpoint.is_empty());
        let provider_result: Result<Box<dyn AsyncModelProvider>> = match api {
            ProviderApiCompatibility::OpenAiResponses => {
                openai_responses_provider_from_auth_store_with_provider_options(
                    auth_store,
                    provider_id,
                    endpoint_override,
                    &provider_config.options,
                    DEFAULT_PROVIDER_TIMEOUT_MS,
                    ReqwestProviderHttpTransport,
                )
                .map(|provider| Box::new(provider) as Box<dyn AsyncModelProvider>)
            }
            ProviderApiCompatibility::OpenAiChatCompletions => {
                openai_compatible_provider_from_auth_store_with_provider_options(
                    auth_store,
                    provider_id,
                    endpoint_override,
                    DEFAULT_PROVIDER_TIMEOUT_MS,
                    ReqwestProviderHttpTransport,
                )
                .map(|provider| Box::new(provider) as Box<dyn AsyncModelProvider>)
            }
            ProviderApiCompatibility::DeepSeekChatCompletions => {
                deepseek_chat_completions_provider_from_auth_store_with_provider_options(
                    auth_store,
                    provider_id,
                    endpoint_override,
                    DEFAULT_PROVIDER_TIMEOUT_MS,
                    ReqwestProviderHttpTransport,
                )
                .map(|provider| Box::new(provider) as Box<dyn AsyncModelProvider>)
            }
        };
        let provider = match provider_result {
            Ok(provider) => {
                self.append_credential_access_audit(
                    provider_id,
                    &provider_config.auth_profile,
                    "provider_model_list",
                    "granted",
                )?;
                provider
            }
            Err(error) => {
                self.append_credential_access_audit(
                    provider_id,
                    &provider_config.auth_profile,
                    "provider_model_list",
                    "denied",
                )?;
                return Err(error);
            }
        };
        provider.list_models_async().await
    }

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
        if model_name.trim().is_empty() {
            return Err(MezError::invalid_args("model name must not be empty"));
        }
        let model = catalog
            .models
            .iter()
            .find(|model| model.id == model_name)
            .ok_or_else(|| {
                MezError::invalid_args(format!(
                    "model `{model_name}` is not available from provider `{provider_id}`; run `/model list`"
                ))
            })?;
        if let Some(reasoning) = reasoning_profile {
            if reasoning.trim().is_empty() {
                return Err(MezError::invalid_args("reasoning level must not be empty"));
            }
            let levels = if model.reasoning_levels.is_empty() {
                catalog.reasoning_levels.as_slice()
            } else {
                model.reasoning_levels.as_slice()
            };
            if !levels.is_empty() && !levels.iter().any(|level| level == reasoning) {
                return Err(MezError::invalid_args(format!(
                    "reasoning level `{reasoning}` is not available for model `{model_name}`; available={}",
                    levels.join(",")
                )));
            }
        }

        let mut provider_options = std::collections::BTreeMap::new();
        if let Some(reasoning) = reasoning_profile {
            provider_options.insert("reasoning_effort".to_string(), reasoning.to_string());
        }
        if let Some(context_window_tokens) = model.context_window_tokens {
            provider_options.insert(
                "context_window_tokens".to_string(),
                context_window_tokens.to_string(),
            );
        }
        let latency_preference = latency_preference
            .map(runtime_validate_latency_preference)
            .transpose()?
            .map(str::to_string);
        let profile = ModelProfile {
            provider: provider_id.to_string(),
            model: model_name.to_string(),
            reasoning_profile: reasoning_profile.map(str::to_string),
            latency_preference,
            multimodal_required: false,
            provider_options,
            safety_tier: None,
        };
        let profile_name = runtime_generated_model_profile_name(
            &self.provider_registry,
            provider_id,
            model_name,
            reasoning_profile,
            &profile,
        );
        self.provider_registry
            .profiles
            .entry(profile_name.clone())
            .or_insert(profile);
        Ok(profile_name)
    }

    /// Inserts a runtime-generated profile while preserving all provider
    /// options carried by the supplied profile.
    fn insert_runtime_generated_model_profile(&mut self, profile: ModelProfile) -> String {
        let profile_name = runtime_generated_model_profile_name(
            &self.provider_registry,
            &profile.provider,
            &profile.model,
            profile.reasoning_profile.as_deref(),
            &profile,
        );
        self.provider_registry
            .profiles
            .entry(profile_name.clone())
            .or_insert(profile);
        profile_name
    }

    pub(in crate::runtime) fn active_model_profile_for_pane(
        &self,
        pane_id: &str,
        agent_id: &str,
        subagent_id: Option<&str>,
    ) -> Result<(String, ModelProfile)> {
        let default_profile = self
            .provider_registry
            .default_profile_name()
            .ok_or_else(|| MezError::config("default model profile is not configured"))?;
        let window_id = self
            .find_pane_descriptor(pane_id)
            .map(|descriptor| descriptor.window_id.to_string());
        let overrides = ModelProfileOverrides {
            default_profile: self
                .agent_selected_personality_profile(pane_id)
                .and_then(|profile| profile.model_profile.clone()),
            session_profile: self.model_profile_overrides.session_profile.clone(),
            window_profile: window_id.as_deref().and_then(|id| {
                self.model_profile_overrides
                    .window_profiles
                    .get(id)
                    .cloned()
            }),
            pane_profile: self
                .model_profile_overrides
                .pane_profiles
                .get(pane_id)
                .cloned(),
            agent_profile: self
                .model_profile_overrides
                .agent_profiles
                .get(agent_id)
                .cloned(),
            subagent_profile: subagent_id.and_then(|id| {
                self.model_profile_overrides
                    .subagent_profiles
                    .get(id)
                    .cloned()
            }),
        };
        let selection = select_model_profile(&overrides, default_profile)?;
        let profile = self.provider_registry.resolve_profile(&selection.profile)?;
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
        self.provider_registry.resolve_profile(profile_name)?;
        match scope {
            RuntimeModelProfileOverrideScope::Session => {
                self.model_profile_overrides.session_profile = Some(profile_name.to_string());
            }
            RuntimeModelProfileOverrideScope::Window(window_id) => {
                self.model_profile_overrides
                    .window_profiles
                    .insert(window_id, profile_name.to_string());
            }
            RuntimeModelProfileOverrideScope::Pane(pane_id) => {
                self.model_profile_overrides
                    .pane_profiles
                    .insert(pane_id, profile_name.to_string());
            }
            RuntimeModelProfileOverrideScope::Agent(agent_id) => {
                self.model_profile_overrides
                    .agent_profiles
                    .insert(agent_id, profile_name.to_string());
            }
            RuntimeModelProfileOverrideScope::Subagent(agent_id) => {
                self.model_profile_overrides
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
        match scope {
            RuntimeModelProfileOverrideScope::Session => {
                self.model_profile_overrides.session_profile = None;
            }
            RuntimeModelProfileOverrideScope::Window(window_id) => {
                self.model_profile_overrides
                    .window_profiles
                    .remove(&window_id);
            }
            RuntimeModelProfileOverrideScope::Pane(pane_id) => {
                self.model_profile_overrides.pane_profiles.remove(&pane_id);
            }
            RuntimeModelProfileOverrideScope::Agent(agent_id) => {
                self.model_profile_overrides
                    .agent_profiles
                    .remove(&agent_id);
            }
            RuntimeModelProfileOverrideScope::Subagent(agent_id) => {
                self.model_profile_overrides
                    .subagent_profiles
                    .remove(&agent_id);
            }
        }
    }

    /// Runs the inherited model profile for child agent operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn inherited_model_profile_for_child_agent(
        &self,
        parent_agent_id: &str,
    ) -> Option<String> {
        if let Some(profile) = self
            .model_profile_overrides
            .agent_profiles
            .get(parent_agent_id)
        {
            return Some(profile.clone());
        }
        let parent_pane = parent_agent_id.strip_prefix("agent-")?;
        let default_profile = self.provider_registry.default_profile_name()?;
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

/// Carries Runtime Model Catalog state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime) struct RuntimeModelCatalog {
    /// Stores the provider value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    provider: String,
    /// Stores the source value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    source: String,
    /// Stores the provider error value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    provider_error: Option<String>,
    /// Stores the models value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    models: Vec<ProviderModelInfo>,
    /// Stores the reasoning levels value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    reasoning_levels: Vec<String>,
    /// Provider-reported quota usage percentages from the catalog request.
    quota_usage: Vec<ProviderQuotaUsage>,
}

impl RuntimeModelCatalog {
    /// Runs the from provider operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn from_provider(catalog: ProviderModelCatalog) -> Self {
        Self {
            provider: catalog.provider,
            source: catalog.source,
            provider_error: None,
            models: catalog.models,
            reasoning_levels: dedupe_runtime_strings(catalog.reasoning_levels),
            quota_usage: catalog.quota_usage,
        }
    }
}

/// Runs the runtime configured model catalog operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_configured_model_catalog(
    provider_id: &str,
    provider_config: &crate::runtime::RuntimeProviderConfig,
    registry: &crate::runtime::RuntimeProviderRegistry,
) -> RuntimeModelCatalog {
    let mut models = BTreeMap::<String, ProviderModelInfo>::new();
    if let Some(default_model) = provider_config.default_model.as_deref()
        && !default_model.is_empty()
    {
        runtime_insert_catalog_model(
            &mut models,
            default_model,
            runtime_configured_reasoning_levels_for_model(provider_config, default_model),
        );
    }
    let configured_models = provider_config
        .models
        .iter()
        .map(String::as_str)
        .filter(|model| !model.is_empty())
        .collect::<Vec<_>>();
    let default_models = if configured_models.is_empty() {
        runtime_provider_default_models(provider_config)
    } else {
        Vec::new()
    };
    let provider_models = if configured_models.is_empty() {
        default_models
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
    } else {
        configured_models
    };
    for model in provider_models {
        runtime_insert_catalog_model(
            &mut models,
            model,
            runtime_configured_reasoning_levels_for_model(provider_config, model),
        );
    }
    for profile in registry
        .profiles()
        .values()
        .filter(|profile| profile.provider == provider_id)
    {
        let mut reasoning_levels =
            runtime_configured_reasoning_levels_for_model(provider_config, &profile.model);
        if let Some(reasoning) = profile.reasoning_profile.as_deref() {
            reasoning_levels.push(reasoning.to_string());
        }
        runtime_insert_catalog_model(&mut models, &profile.model, reasoning_levels);
    }
    if models.is_empty()
        && let Some(recommended_model) = runtime_provider_recommended_model(provider_config)
    {
        runtime_insert_catalog_model(
            &mut models,
            recommended_model,
            runtime_configured_reasoning_levels_for_model(provider_config, recommended_model),
        );
    }
    let models = models.into_values().collect::<Vec<_>>();
    let reasoning_levels = dedupe_runtime_strings(
        models
            .iter()
            .flat_map(|model| model.reasoning_levels.iter().cloned())
            .collect(),
    );
    RuntimeModelCatalog {
        provider: provider_id.to_string(),
        source: "config".to_string(),
        provider_error: None,
        models,
        reasoning_levels,
        quota_usage: Vec::new(),
    }
}

/// Runs the runtime insert catalog model operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_insert_catalog_model(
    models: &mut BTreeMap<String, ProviderModelInfo>,
    model: &str,
    reasoning_levels: Vec<String>,
) {
    let entry = models
        .entry(model.to_string())
        .or_insert_with(|| ProviderModelInfo {
            id: model.to_string(),
            display_name: None,
            reasoning_levels: Vec::new(),
            context_window_tokens: crate::agent::known_model_context_window_tokens(model),
        });
    entry.reasoning_levels.extend(
        reasoning_levels
            .into_iter()
            .filter(|level| !level.is_empty()),
    );
    entry.reasoning_levels = dedupe_runtime_strings(std::mem::take(&mut entry.reasoning_levels));
    if entry.context_window_tokens.is_none() {
        entry.context_window_tokens = crate::agent::known_model_context_window_tokens(model);
    }
}

/// Runs the runtime configured reasoning levels for model operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_configured_reasoning_levels_for_model(
    provider_config: &crate::runtime::RuntimeProviderConfig,
    model: &str,
) -> Vec<String> {
    let mut levels = provider_config
        .options
        .get("reasoning_effort")
        .or_else(|| provider_config.options.get("reasoning_profile"))
        .into_iter()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if let Ok(provider_api) =
        effective_provider_api(&provider_config.kind, provider_config.api.as_deref())
    {
        match provider_api {
            ProviderApiCompatibility::OpenAiResponses => {
                levels.extend(openai_default_reasoning_levels_for_model(model));
            }
            ProviderApiCompatibility::DeepSeekChatCompletions => {
                levels.extend(deepseek_default_reasoning_effort_levels());
            }
            ProviderApiCompatibility::OpenAiChatCompletions => {}
        }
    }
    dedupe_runtime_strings(levels)
}

/// Returns built-in default models only when the provider's selected API keeps
/// the provider's built-in model catalog semantics.
fn runtime_provider_default_models(
    provider_config: &crate::runtime::RuntimeProviderConfig,
) -> Vec<String> {
    match effective_provider_api(&provider_config.kind, provider_config.api.as_deref()) {
        Ok(ProviderApiCompatibility::OpenAiResponses) if provider_config.kind == "openai" => {
            runtime_default_models_for_provider(&provider_config.kind)
                .map(|models| models.iter().map(|model| (*model).to_string()).collect())
                .unwrap_or_default()
        }
        Ok(ProviderApiCompatibility::DeepSeekChatCompletions)
            if provider_config.kind == "deepseek" =>
        {
            runtime_default_models_for_provider(&provider_config.kind)
                .map(|models| models.iter().map(|model| (*model).to_string()).collect())
                .unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

/// Returns a built-in recommended model only when the provider and API share
/// the built-in provider's catalog contract.
fn runtime_provider_recommended_model(
    provider_config: &crate::runtime::RuntimeProviderConfig,
) -> Option<&'static str> {
    match effective_provider_api(&provider_config.kind, provider_config.api.as_deref()) {
        Ok(ProviderApiCompatibility::OpenAiResponses) if provider_config.kind == "openai" => {
            runtime_recommended_model_for_provider(&provider_config.kind).ok()
        }
        Ok(ProviderApiCompatibility::DeepSeekChatCompletions)
            if provider_config.kind == "deepseek" =>
        {
            runtime_recommended_model_for_provider(&provider_config.kind).ok()
        }
        _ => None,
    }
}

/// Returns the reasoning effort levels supported by DeepSeek providers.
fn deepseek_default_reasoning_effort_levels() -> Vec<String> {
    vec!["high".to_string(), "max".to_string()]
}

/// Formats the current routing auto-sizing model profile.
fn runtime_routing_model_profile_display(
    routing_name: &str,
    routing_profile: &ModelProfile,
    active_profile: &ModelProfile,
) -> String {
    format!(
        "scope=routing profile={} provider={} model={} reasoning_profile={} active_provider={} active_model={} source=runtime-routing-model",
        json_escape(routing_name),
        json_escape(&routing_profile.provider),
        json_escape(&routing_profile.model),
        routing_profile
            .reasoning_profile
            .as_deref()
            .unwrap_or("none"),
        json_escape(&active_profile.provider),
        json_escape(&active_profile.model)
    )
}

/// Runs the runtime model catalog display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_model_catalog_display(
    active_name: &str,
    active_profile: &ModelProfile,
    catalog: &RuntimeModelCatalog,
) -> String {
    let context_limit = format!("{} tokens", active_profile.context_window_tokens());
    let mut lines = vec!["## Model Catalog".to_string(), String::new()];
    if let Some(error) = catalog.provider_error.as_deref() {
        lines.push(format!(
            "**Provider catalog unavailable:** `{}`",
            runtime_model_catalog_unavailable_reason(error)
        ));
        lines.push(String::new());
    }
    if !catalog.reasoning_levels.is_empty() {
        lines.push(format!(
            "**Reasoning levels:** `{}`",
            catalog.reasoning_levels.join(", ")
        ));
        lines.push(String::new());
    }
    let model_rows = catalog
        .models
        .iter()
        .map(|model| {
            let model_name = runtime_model_catalog_model_name(model);
            let active_model =
                catalog.provider == active_profile.provider && model.id == active_profile.model;
            let model_name = if active_model {
                format!("★ {model_name}")
            } else {
                model_name
            };
            vec![
                catalog.provider.clone(),
                model_name,
                runtime_model_catalog_reasoning_display(
                    &model.reasoning_levels,
                    active_model.then_some(active_profile.reasoning_profile.as_deref()),
                ),
                context_limit.clone(),
                catalog.source.clone(),
                if active_model {
                    active_name.to_string()
                } else {
                    String::new()
                },
            ]
        })
        .collect::<Vec<_>>();
    if !model_rows.is_empty() {
        lines.extend(runtime_markdown_table(
            &[
                "Provider",
                "Model",
                "Reasoning levels",
                "Context limit",
                "Source",
                "Active profile",
            ],
            &model_rows,
        ));
    }
    lines.join("\n")
}

/// Formats a provider model name with optional display metadata.
fn runtime_model_catalog_model_name(model: &ProviderModelInfo) -> String {
    match model.display_name.as_deref() {
        Some(display_name) if !display_name.is_empty() => {
            format!("{} ({display_name})", model.id)
        }
        _ => model.id.clone(),
    }
}

/// Formats reasoning choices and marks the active reasoning level.
fn runtime_model_catalog_reasoning_display(
    levels: &[String],
    active_reasoning: Option<Option<&str>>,
) -> String {
    let mut values = if levels.is_empty() {
        vec!["default".to_string()]
    } else {
        levels.to_vec()
    };
    let active = active_reasoning.flatten().unwrap_or("default");
    if !values.iter().any(|level| level == active) {
        values.insert(0, active.to_string());
    }
    if active_reasoning.is_some() {
        values
            .into_iter()
            .map(|level| {
                if level == active {
                    format!("★ {level}")
                } else {
                    level
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        values.join(", ")
    }
}

/// Builds a plain markdown table from already formatted cell values.
pub(super) fn runtime_markdown_table(headers: &[&str], rows: &[Vec<String>]) -> Vec<String> {
    let header = headers
        .iter()
        .map(|cell| runtime_markdown_table_cell(cell))
        .collect::<Vec<_>>()
        .join(" | ");
    let separator = headers
        .iter()
        .map(|_| "---")
        .collect::<Vec<_>>()
        .join(" | ");
    let mut lines = vec![format!("| {header} |"), format!("| {separator} |")];
    lines.extend(rows.iter().map(|row| {
        let cells = row
            .iter()
            .map(|cell| runtime_markdown_table_cell(cell))
            .collect::<Vec<_>>()
            .join(" | ");
        format!("| {cells} |")
    }));
    lines
}

/// Escapes markdown table separators without changing the copyable value.
fn runtime_markdown_table_cell(value: &str) -> String {
    value.replace('|', r"\|").replace('\n', "<br>")
}

/// Runs the runtime model catalog unavailable reason operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_model_catalog_unavailable_reason(error: &str) -> String {
    if error.contains("ChatGPT browser credentials") {
        "browser-auth-catalog-unsupported".to_string()
    } else if error.contains("api.model.read") || error.contains("Missing scopes") {
        "missing-model-read-scope".to_string()
    } else if error.contains("attached auth store") || error.contains("authenticated provider") {
        "auth-unavailable".to_string()
    } else if error.contains("has not been refreshed") {
        "provider-info-not-refreshed".to_string()
    } else if error.contains("Models API returned status")
        || error.contains("model catalog")
        || error.contains("provider HTTP request failed")
    {
        "live-provider-catalog-unavailable".to_string()
    } else {
        error.replace(char::is_whitespace, "-")
    }
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

/// Runs the dedupe runtime strings operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn dedupe_runtime_strings(values: Vec<String>) -> Vec<String> {
    let mut deduped = Vec::new();
    for value in values {
        if !deduped.iter().any(|existing| existing == &value) {
            deduped.push(value);
        }
    }
    deduped
}
