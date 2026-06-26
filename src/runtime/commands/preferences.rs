//! Agent routing and personality slash-command helpers.
//!
//! This module owns pane-local agent presentation preferences that are driven
//! through slash commands. It keeps routing toggles, personality profile
//! selection, and visibility lookup helpers together so the command facade does
//! not mix preference state with unrelated command families.

use super::*;

impl RuntimeSessionService {
    /// Returns the pane-effective local action executor selected by config or slash command.
    pub(in crate::runtime) fn agent_local_action_executor_for_pane(
        &self,
        pane_id: &str,
    ) -> RuntimeLocalActionExecutor {
        self.agent_local_action_executor_overrides
            .get(pane_id)
            .copied()
            .unwrap_or(self.agent_local_action_executor)
    }

    /// Executes `/shell-mode` against pane-scoped local action executor state.
    pub(in crate::runtime) fn execute_agent_shell_shell_mode_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("shell-mode command must be a slash command"))?;
        let args = parse_shell_mode_args(&invocation.args)?;
        if matches!(args.mode, ShellModeCommandMode::Status) {
            return Ok(AgentShellCommandOutcome::Display {
                command: "shell-mode".to_string(),
                body: self.agent_shell_mode_status_display(pane_id),
            });
        }

        let requested = args
            .mode
            .executor()
            .ok_or_else(|| MezError::invalid_args("shell-mode expects native, pane, or status"))?;
        let visibility = self.agent_shell_visibility_for_pane(pane_id)?;
        match args.scope {
            ShellModeCommandScope::Session => {
                let before = self.agent_local_action_executor_for_pane(pane_id);
                self.agent_local_action_executor_overrides
                    .insert(pane_id.to_string(), requested);
                Ok(AgentShellCommandOutcome::Mutated {
                    command: "shell-mode".to_string(),
                    body: self.agent_shell_mode_mutation_display(
                        pane_id,
                        requested,
                        before != requested,
                        "session",
                    ),
                    visibility,
                })
            }
            ShellModeCommandScope::Config => {
                let before = self.agent_local_action_executor_for_pane(pane_id);
                let path = self.runtime_primary_config_path_for_agent_command()?;
                let report = runtime_apply_persisted_config_mutation_batch(
                    self,
                    path,
                    &[ConfigMutation {
                        path: "agents.local_action_executor".to_string(),
                        operation: ConfigMutationOperation::Set(ConfigMutationValue::String(
                            runtime_local_action_executor_config_name(requested).to_string(),
                        )),
                    }],
                    "agent/slash:shell-mode",
                )?;
                self.agent_local_action_executor_overrides.remove(pane_id);
                Ok(AgentShellCommandOutcome::Mutated {
                    command: "shell-mode".to_string(),
                    body: format!(
                        "{} config_changed={} config_path={} reload_required={} deferred={}",
                        self.agent_shell_mode_mutation_display(
                            pane_id,
                            requested,
                            before != requested || report.changed,
                            "config"
                        ),
                        report.changed,
                        json_escape(&report.path.display().to_string()),
                        report.reload_required,
                        report.deferred
                    ),
                    visibility,
                })
            }
        }
    }

    /// Executes `/routing` against pane-scoped auto-sizing state.
    pub(in crate::runtime) fn execute_agent_shell_routing_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("routing command must be a slash command"))?;
        let mode = runtime_single_mode_arg(&invocation.args, "routing", "toggle")?;
        let default_enabled = self.agent_routing;
        let enabled_before = self
            .agent_routing_overrides
            .get(pane_id)
            .copied()
            .unwrap_or(default_enabled);
        if matches!(mode.as_str(), "status" | "show") {
            return Ok(AgentShellCommandOutcome::Display {
                command: "routing".to_string(),
                body: format!(
                    "pane={} enabled={} default={} override_present={} source=runtime-routing",
                    json_escape(pane_id),
                    enabled_before,
                    default_enabled,
                    self.agent_routing_overrides.contains_key(pane_id)
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
        self.agent_routing_overrides
            .insert(pane_id.to_string(), enabled);
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

    /// Executes `/personality` against pane-scoped response style state.
    pub(super) fn execute_agent_shell_personality_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("personality command must be a slash command"))?;
        let requested = invocation.args.trim();
        let current = self.agent_response_styles.get(pane_id).cloned();
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
                    self.agent_personality_profiles.len()
                ),
            });
        }
        if requested == "list" {
            let profiles = self
                .agent_personality_profiles
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
                    self.default_agent_personality
                        .as_deref()
                        .map(json_escape)
                        .unwrap_or_else(|| "none".to_string())
                ),
            });
        }
        validate_agent_personality(requested)?;
        let visibility = self.agent_shell_visibility_for_pane(pane_id)?;
        if matches!(requested, "clear" | "default") {
            let changed =
                current.is_some() || self.agent_personality_selections.contains_key(pane_id);
            self.agent_personality_selections.remove(pane_id);
            self.agent_response_styles.remove(pane_id);
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
        let requested_style =
            if let Some(profile) = self.agent_personality_profiles.get(requested).cloned() {
                self.agent_personality_selections
                    .insert(pane_id.to_string(), requested.to_string());
                self.apply_agent_personality_profile_overrides(pane_id, &profile)?;
                profile.response_style
            } else {
                self.agent_personality_selections.remove(pane_id);
                Some(requested.to_string())
            };
        let changed = current != requested_style || current_profile.as_deref() != Some(requested);
        if let Some(style) = requested_style {
            self.agent_response_styles
                .insert(pane_id.to_string(), style);
        } else {
            self.agent_response_styles.remove(pane_id);
        }
        let active = self.agent_response_styles.get(pane_id);
        Ok(AgentShellCommandOutcome::Mutated {
            command: "personality".to_string(),
            body: format!(
                "pane={} profile={} style={} changed={} source=runtime-personality",
                json_escape(pane_id),
                self.agent_selected_personality_profile_id(pane_id)
                    .map(json_escape)
                    .unwrap_or_else(|| "custom".to_string()),
                active
                    .map(|style| json_escape(style))
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
            if self.provider_registry.profile(model_profile).is_none() {
                return Err(MezError::invalid_args(format!(
                    "personality model_profile `{model_profile}` is not configured"
                )));
            }
            self.model_profile_overrides
                .pane_profiles
                .insert(pane_id.to_string(), model_profile.to_string());
        }
        if let Some(planning_enabled) = profile.planning_enabled {
            if planning_enabled {
                self.agent_planning_modes.insert(pane_id.to_string());
            } else {
                self.agent_planning_modes.remove(pane_id);
            }
        }
        if let Some(routing_enabled) = profile.routing_enabled {
            self.agent_routing_overrides
                .insert(pane_id.to_string(), routing_enabled);
        }
        Ok(())
    }

    /// Returns the selected or default personality profile id for a pane.
    ///
    /// # Parameters
    /// - `pane_id`: The pane whose selected profile should be resolved.
    pub(super) fn agent_selected_personality_profile_id(&self, pane_id: &str) -> Option<&str> {
        self.agent_personality_selections
            .get(pane_id)
            .map(String::as_str)
            .or(self.default_agent_personality.as_deref())
            .filter(|profile_id| self.agent_personality_profiles.contains_key(*profile_id))
    }

    /// Returns the selected or default personality profile for a pane.
    ///
    /// # Parameters
    /// - `pane_id`: The pane whose selected profile should be resolved.
    pub(in crate::runtime) fn agent_selected_personality_profile(
        &self,
        pane_id: &str,
    ) -> Option<&crate::runtime::RuntimeAgentPersonalityProfile> {
        self.agent_selected_personality_profile_id(pane_id)
            .and_then(|profile_id| self.agent_personality_profiles.get(profile_id))
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
        self.agent_shell_store
            .get(pane_id)
            .map(|session| session.visibility)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "agent shell session not found for pane",
                )
            })
    }

    /// Formats the current local action executor status for `/shell-mode`.
    fn agent_shell_mode_status_display(&self, pane_id: &str) -> String {
        let effective = self.agent_local_action_executor_for_pane(pane_id);
        let override_present = self
            .agent_local_action_executor_overrides
            .contains_key(pane_id);
        format!(
            "pane={} mode={} configured={} source={}{}",
            json_escape(pane_id),
            runtime_local_action_executor_display_name(effective),
            runtime_local_action_executor_display_name(self.agent_local_action_executor),
            if override_present {
                "session"
            } else {
                "config"
            },
            self.agent_shell_mode_warning_suffix(pane_id, effective)
        )
    }

    /// Formats a successful local action executor mutation for `/shell-mode`.
    fn agent_shell_mode_mutation_display(
        &self,
        pane_id: &str,
        executor: RuntimeLocalActionExecutor,
        changed: bool,
        source: &str,
    ) -> String {
        format!(
            "pane={} mode={} changed={} source={}{}",
            json_escape(pane_id),
            runtime_local_action_executor_display_name(executor),
            changed,
            source,
            self.agent_shell_mode_warning_suffix(pane_id, executor)
        )
    }

    /// Formats native-mode environment diagnostics for command output.
    fn agent_shell_mode_warning_suffix(
        &self,
        pane_id: &str,
        executor: RuntimeLocalActionExecutor,
    ) -> String {
        if executor != RuntimeLocalActionExecutor::Native {
            return String::new();
        }
        let Some(working_directory) = self.pane_current_working_directory(pane_id) else {
            return " warning=working-directory-unknown".to_string();
        };
        let probe = EnvironmentEquivalenceProbe::compare(
            self.pane_environment_signatures.get(pane_id),
            &working_directory,
        );
        if probe.equivalence == EnvironmentEquivalence::Equivalent {
            return " equivalence=equivalent".to_string();
        }
        format!(
            " warning=native-host-equivalence-not-proven equivalence={} diagnostics={}",
            probe.equivalence.as_str(),
            json_escape(&probe.diagnostics.join("; "))
        )
    }
}

/// Parsed `/shell-mode` scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellModeCommandScope {
    /// Apply the requested shell mode to the active agent shell session only.
    Session,
    /// Persist the requested shell mode into the primary user configuration.
    Config,
}

/// Parsed `/shell-mode` operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellModeCommandMode {
    /// Show the active shell mode.
    Status,
    /// Select a specific executor.
    Executor(RuntimeLocalActionExecutor),
}

impl ShellModeCommandMode {
    /// Returns the executor for mutation modes.
    fn executor(self) -> Option<RuntimeLocalActionExecutor> {
        match self {
            Self::Status => None,
            Self::Executor(executor) => Some(executor),
        }
    }
}

/// Parsed `/shell-mode` arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ShellModeCommandArgs {
    /// Requested mode or status operation.
    mode: ShellModeCommandMode,
    /// Requested mutation scope.
    scope: ShellModeCommandScope,
}

/// Parses `/shell-mode` arguments.
fn parse_shell_mode_args(args: &str) -> Result<ShellModeCommandArgs> {
    let mut mode = None;
    let mut scope = ShellModeCommandScope::Session;
    let mut words = args.split_whitespace();
    while let Some(word) = words.next() {
        match word {
            "--scope" => {
                let value = words.next().ok_or_else(|| {
                    MezError::invalid_args("/shell-mode --scope requires session or config")
                })?;
                scope = parse_shell_mode_scope(value)?;
            }
            value if value.starts_with("--scope=") => {
                let value = value.trim_start_matches("--scope=");
                scope = parse_shell_mode_scope(value)?;
            }
            "status" | "show" => set_shell_mode_once(&mut mode, ShellModeCommandMode::Status)?,
            "native" => set_shell_mode_once(
                &mut mode,
                ShellModeCommandMode::Executor(RuntimeLocalActionExecutor::Native),
            )?,
            "pane" => set_shell_mode_once(
                &mut mode,
                ShellModeCommandMode::Executor(RuntimeLocalActionExecutor::PaneShell),
            )?,
            _ => {
                return Err(MezError::invalid_args(
                    "/shell-mode expects native, pane, or status",
                ));
            }
        }
    }
    Ok(ShellModeCommandArgs {
        mode: mode.unwrap_or(ShellModeCommandMode::Status),
        scope,
    })
}

/// Parses a `/shell-mode --scope` value.
fn parse_shell_mode_scope(value: &str) -> Result<ShellModeCommandScope> {
    match value {
        "session" => Ok(ShellModeCommandScope::Session),
        "config" => Ok(ShellModeCommandScope::Config),
        _ => Err(MezError::invalid_args(
            "/shell-mode --scope must be session or config",
        )),
    }
}

/// Sets the shell mode parser result while rejecting duplicates.
fn set_shell_mode_once(
    mode: &mut Option<ShellModeCommandMode>,
    next: ShellModeCommandMode,
) -> Result<()> {
    if mode.replace(next).is_some() {
        return Err(MezError::invalid_args(
            "/shell-mode accepts at most one mode argument",
        ));
    }
    Ok(())
}

/// Returns the config value for a local action executor.
fn runtime_local_action_executor_config_name(executor: RuntimeLocalActionExecutor) -> &'static str {
    match executor {
        RuntimeLocalActionExecutor::PaneShell => "pane_shell",
        RuntimeLocalActionExecutor::Native => "native",
    }
}

/// Returns the display value for a local action executor.
fn runtime_local_action_executor_display_name(
    executor: RuntimeLocalActionExecutor,
) -> &'static str {
    match executor {
        RuntimeLocalActionExecutor::PaneShell => "pane",
        RuntimeLocalActionExecutor::Native => "native",
    }
}
