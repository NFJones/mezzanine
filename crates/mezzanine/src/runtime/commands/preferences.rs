//! Agent routing and personality slash-command helpers.
//!
//! This module owns pane-local agent presentation preferences that are driven
//! through slash commands. It keeps routing toggles, personality profile
//! selection, and visibility lookup helpers together so the command facade does
//! not mix preference state with unrelated command families.

use super::{
    AgentShellCommandOutcome, AgentShellVisibility, ConfigMutation, ConfigMutationOperation,
    ConfigMutationValue, MezError, Result, RuntimeSessionService, json_escape, parse_slash_command,
    runtime_apply_persisted_config_mutation_batch, runtime_primary_config_path,
    runtime_single_mode_arg, validate_agent_personality,
};
use mez_agent::AutoSizingRoutingPolicy;

impl RuntimeSessionService {
    /// Executes `/routing` against pane-scoped auto-sizing state.
    pub(crate) fn execute_agent_shell_routing_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("routing command must be a slash command"))?;
        let arguments = invocation.args.split_ascii_whitespace().collect::<Vec<_>>();
        if arguments.first() == Some(&"policy") {
            return self.execute_agent_shell_root_routing_policy_command(pane_id, &arguments);
        }
        let mode = runtime_single_mode_arg(&invocation.args, "routing", "toggle")?;
        let default_enabled = self.agent_default_routing();
        let enabled_before = self
            .agent_routing_override(pane_id)
            .unwrap_or(default_enabled);
        if matches!(mode.as_str(), "status" | "show") {
            return Ok(AgentShellCommandOutcome::Display {
                command: "routing".to_string(),
                body: format!(
                    "pane={} enabled={} default={} override_present={} source=runtime-routing",
                    json_escape(pane_id),
                    enabled_before,
                    default_enabled,
                    self.agent_routing_override(pane_id).is_some()
                ),
            });
        }
        let visibility = self.agent_shell_visibility_for_pane(pane_id)?;
        let enabled = match mode.as_str() {
            "on" => true,
            "off" => false,
            "toggle" => !enabled_before,
            _ => {
                return Err(MezError::invalid_args(
                    "routing slash command expects on, off, toggle, status, or no argument",
                ));
            }
        };
        self.set_agent_routing_override(pane_id, Some(enabled));
        Ok(AgentShellCommandOutcome::Mutated {
            command: "routing".to_string(),
            body: format!(
                "pane={} enabled={} default={} changed={} source=runtime-routing",
                json_escape(pane_id),
                enabled,
                default_enabled,
                enabled != enabled_before
            ),
            visibility,
        })
    }

    /// Persists the root-turn application policy selected through `/routing policy`.
    fn execute_agent_shell_root_routing_policy_command(
        &mut self,
        pane_id: &str,
        arguments: &[&str],
    ) -> Result<AgentShellCommandOutcome> {
        let ["policy", requested] = arguments else {
            return Err(MezError::invalid_args(
                "routing policy expects exactly one value: subagent or in-place",
            ));
        };
        let policy = AutoSizingRoutingPolicy::parse(requested).ok_or_else(|| {
            MezError::invalid_args("routing policy expects one of: subagent, in-place")
        })?;
        let path = runtime_primary_config_path(self)?.ok_or_else(|| {
            MezError::invalid_state("routing policy requires a configured primary config path")
        })?;
        let report = runtime_apply_persisted_config_mutation_batch(
            self,
            path,
            &[ConfigMutation {
                path: "agents.auto_sizing.root_routing_policy".to_string(),
                operation: ConfigMutationOperation::Set(ConfigMutationValue::String(
                    policy.as_str().to_string(),
                )),
            }],
            "agent/shell/routing-policy",
        )?;
        let visibility = self.agent_shell_visibility_for_pane(pane_id)?;
        Ok(AgentShellCommandOutcome::Mutated {
            command: "routing".to_string(),
            body: format!(
                "pane={} root_policy={} changed={} persisted_path={} source=runtime-routing",
                json_escape(pane_id),
                policy.as_str(),
                report.changed,
                json_escape(&report.path.display().to_string())
            ),
            visibility,
        })
    }

    /// Executes `/personality` against pane-scoped response style state.
    pub(super) fn execute_agent_shell_personality_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("personality command must be a slash command"))?;
        let requested = invocation.args.trim();
        let current = self.agent_response_style(pane_id).map(ToOwned::to_owned);
        let current_profile = self
            .agent_selected_personality_profile_id(pane_id)
            .map(ToOwned::to_owned);
        if requested.is_empty() || matches!(requested, "status" | "show") {
            return Ok(AgentShellCommandOutcome::Display {
                command: "personality".to_string(),
                body: format!(
                    "pane={} profile={} style={} configured_profiles={} source=runtime-personality",
                    json_escape(pane_id),
                    current_profile
                        .as_deref()
                        .map(json_escape)
                        .unwrap_or_else(|| "default".to_string()),
                    current
                        .as_deref()
                        .map(json_escape)
                        .unwrap_or_else(|| "default".to_string()),
                    self.integration.agent_personality_profiles().len()
                ),
            });
        }
        if requested == "list" {
            let profiles = self
                .integration
                .agent_personality_profiles()
                .iter()
                .map(|(id, profile)| {
                    format!(
                        "{}{}",
                        id,
                        profile
                            .name
                            .as_deref()
                            .map(|name| format!(" ({name})"))
                            .unwrap_or_default()
                    )
                })
                .collect::<Vec<_>>();
            return Ok(AgentShellCommandOutcome::Display {
                command: "personality".to_string(),
                body: format!(
                    "profiles=[{}] default={} source=runtime-personality",
                    profiles.join(", "),
                    self.integration
                        .default_agent_personality()
                        .map(json_escape)
                        .unwrap_or_else(|| "none".to_string())
                ),
            });
        }
        validate_agent_personality(requested)?;
        let visibility = self.agent_shell_visibility_for_pane(pane_id)?;
        if matches!(requested, "clear" | "default") {
            let changed = current.is_some()
                || self
                    .integration
                    .agent_personality_selections()
                    .contains_key(pane_id);
            self.integration
                .agent_personality_selections_mut()
                .remove(pane_id);
            self.set_agent_response_style(pane_id, None);
            return Ok(AgentShellCommandOutcome::Mutated {
                command: "personality".to_string(),
                body: format!(
                    "pane={} profile=default style=default changed={} source=runtime-personality",
                    json_escape(pane_id),
                    changed
                ),
                visibility,
            });
        }
        let requested_style = if let Some(profile) = self
            .integration
            .agent_personality_profiles()
            .get(requested)
            .cloned()
        {
            self.integration
                .agent_personality_selections_mut()
                .insert(pane_id.to_string(), requested.to_string());
            self.apply_agent_personality_profile_overrides(pane_id, &profile)?;
            profile.response_style
        } else {
            self.integration
                .agent_personality_selections_mut()
                .remove(pane_id);
            Some(requested.to_string())
        };
        let changed = current != requested_style || current_profile.as_deref() != Some(requested);
        self.set_agent_response_style(pane_id, requested_style);
        let active = self.agent_response_style(pane_id);
        Ok(AgentShellCommandOutcome::Mutated {
            command: "personality".to_string(),
            body: format!(
                "pane={} profile={} style={} changed={} source=runtime-personality",
                json_escape(pane_id),
                self.agent_selected_personality_profile_id(pane_id)
                    .map(json_escape)
                    .unwrap_or_else(|| "custom".to_string()),
                active
                    .map(json_escape)
                    .unwrap_or_else(|| "default".to_string()),
                changed
            ),
            visibility,
        })
    }

    /// Applies runtime overrides supplied by a configured personality profile.
    ///
    /// # Parameters
    /// - `pane_id`: The pane receiving the profile overrides.
    /// - `profile`: The configured profile selected by `/personality`.
    fn apply_agent_personality_profile_overrides(
        &mut self,
        pane_id: &str,
        profile: &crate::runtime::RuntimeAgentPersonalityProfile,
    ) -> Result<()> {
        if let Some(model_profile) = profile.model_profile.as_ref() {
            if self.provider_registry().profile(model_profile).is_none() {
                return Err(MezError::invalid_args(format!(
                    "personality model_profile `{model_profile}` is not configured"
                )));
            }
            self.integration
                .model_profile_overrides_mut()
                .pane_profiles
                .insert(pane_id.to_string(), model_profile.to_string());
        }
        if let Some(planning_enabled) = profile.planning_enabled {
            self.set_agent_planning_enabled(pane_id, planning_enabled);
        }
        if let Some(routing_enabled) = profile.routing_enabled {
            self.set_agent_routing_override(pane_id, Some(routing_enabled));
        }
        Ok(())
    }

    /// Returns the selected or default personality profile id for a pane.
    ///
    /// # Parameters
    /// - `pane_id`: The pane whose selected profile should be resolved.
    pub(super) fn agent_selected_personality_profile_id(&self, pane_id: &str) -> Option<&str> {
        self.integration
            .agent_personality_selections()
            .get(pane_id)
            .map(String::as_str)
            .or(self.integration.default_agent_personality())
            .filter(|profile_id| {
                self.integration
                    .agent_personality_profiles()
                    .contains_key(*profile_id)
            })
    }

    /// Returns the selected or default personality profile for a pane.
    ///
    /// # Parameters
    /// - `pane_id`: The pane whose selected profile should be resolved.
    pub(crate) fn agent_selected_personality_profile(
        &self,
        pane_id: &str,
    ) -> Option<&crate::runtime::RuntimeAgentPersonalityProfile> {
        self.agent_selected_personality_profile_id(pane_id)
            .and_then(|profile_id| {
                self.integration
                    .agent_personality_profiles()
                    .get(profile_id)
            })
    }

    /// Runs the agent shell visibility for pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn agent_shell_visibility_for_pane(
        &self,
        pane_id: &str,
    ) -> Result<AgentShellVisibility> {
        self.agent_shell_store()
            .get(pane_id)
            .map(|session| session.visibility)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "agent shell session not found for pane",
                )
            })
    }
}
