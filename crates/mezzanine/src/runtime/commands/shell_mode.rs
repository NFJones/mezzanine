//! Pane-local shell execution mode command parsing, execution, and
//! presentation.
//!
//! This module owns the narrow `/shell-mode` runtime hierarchy. Selecting a
//! mode without `--global` acts on only the invoking pane. Explicit global
//! selections reuse the atomic persisted-config
//! transaction, while pane-local mutations mirror the agent routing override
//! store.

use super::shell::AgentShellCommandOrigin;
use super::{
    AgentShellCommandOutcome, AgentShellVisibility, ConfigMutation, ConfigMutationOperation,
    ConfigMutationValue, MezError, Result, RuntimeSessionService, parse_slash_command,
    runtime_apply_persisted_config_mutation_batch, runtime_primary_config_path,
};
use crate::integrations::agent::slash::AgentShellPresentation;
use crate::runtime::config::ShellMode;
use crate::security::audit::{AuditActor, AuditRecord};

/// Scope selected for a mode operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellModeScope {
    /// Exact invoking pane only.
    Pane,
    /// Persisted default configuration.
    Global,
}

/// Strict operations accepted by `/shell-mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellModeCommand {
    /// Displays the effective shell execution mode for the invoking pane.
    Status,
    /// Selects one shell execution mode in the requested scope.
    Set(ShellModeScope, ShellMode),
}

impl RuntimeSessionService {
    /// Executes the canonical `/shell-mode` hierarchy.
    ///
    /// # Errors
    /// Returns an argument error for unsupported grammar, a forbidden error
    /// for non-primary mutation, and a config error when global persistence
    /// or live application cannot complete atomically.
    pub(super) fn execute_agent_shell_shell_mode_command(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        input: &str,
        origin: AgentShellCommandOrigin,
    ) -> Result<AgentShellCommandOutcome> {
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden(
                "shell mode commands require the primary client",
            ));
        }
        let operation = parse_shell_mode_command(input)?;
        match operation {
            ShellModeCommand::Status => Ok(AgentShellCommandOutcome::Presented {
                command: "shell-mode".to_string(),
                body: self.render_shell_mode_status(pane_id),
                presentation: AgentShellPresentation::Pager,
            }),
            ShellModeCommand::Set(scope, mode) => {
                if !origin.is_authenticated_primary_input() {
                    return Err(MezError::forbidden(
                        "shell mode mutations require authenticated primary-client input",
                    ));
                }
                self.apply_shell_mode(primary_client_id, pane_id, scope, mode)
            }
        }
    }

    /// Renders the effective pane mode and the configured global default.
    fn render_shell_mode_status(&self, pane_id: &str) -> String {
        let global = self.agent_default_shell_mode();
        let override_mode = self.agent_shell_mode_override(pane_id);
        let effective = self.effective_agent_shell_mode_for_pane(pane_id);
        format!(
            "# Shell Mode Status\n\n| Field | Value |\n| --- | --- |\n| Pane | `{pane_id}` |\n| Effective mode | `{}` |\n| Global mode | `{}` |\n| Source | {} |\n| Local override | {} |\n",
            effective.name(),
            global.name(),
            if override_mode.is_some() {
                "pane override"
            } else {
                "global default"
            },
            if override_mode.is_some() { "yes" } else { "no" },
        )
    }

    /// Applies one local override or persisted global mode mutation.
    fn apply_shell_mode(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        scope: ShellModeScope,
        mode: ShellMode,
    ) -> Result<AgentShellCommandOutcome> {
        let visibility = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| session.visibility)
            .unwrap_or(AgentShellVisibility::Visible);
        let changed = match scope {
            ShellModeScope::Pane => {
                let current = self.effective_agent_shell_mode_for_pane(pane_id);
                let overridden = self.agent_shell_mode_override(pane_id).is_some();
                let changed = current != mode || !overridden;
                self.set_agent_shell_mode_override(pane_id, Some(mode));
                changed
            }
            ShellModeScope::Global => {
                let path = runtime_primary_config_path(self)?.ok_or_else(|| {
                    MezError::invalid_state(
                        "global shell mode mutation requires a primary configuration file",
                    )
                })?;
                runtime_apply_persisted_config_mutation_batch(
                    self,
                    path,
                    &[ConfigMutation {
                        path: "agents.shell_mode".to_string(),
                        operation: ConfigMutationOperation::Set(ConfigMutationValue::String(
                            mode.name().to_string(),
                        )),
                    }],
                    "agent-shell-shell-mode",
                )?
                .changed
            }
        };
        self.append_shell_mode_command_audit(primary_client_id, pane_id, mode, scope, changed)?;
        Ok(AgentShellCommandOutcome::Mutated {
            command: "shell-mode".to_string(),
            body: format!(
                "Shell mode {} scope={} changed={changed}; effective={} global={}",
                mode.name(),
                shell_mode_scope_name(scope),
                self.effective_agent_shell_mode_for_pane(pane_id).name(),
                self.agent_default_shell_mode().name(),
            ),
            visibility,
        })
    }

    /// Appends redacted shell-mode command metadata.
    fn append_shell_mode_command_audit(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        mode: ShellMode,
        scope: ShellModeScope,
        changed: bool,
    ) -> Result<()> {
        let generation = self.session.config_generation;
        let Some(audit_log) = self.persistence.audit_log_mut() else {
            return Ok(());
        };
        let mut record = AuditRecord::new(
            self.session.id.to_string(),
            AuditActor {
                kind: "client".to_string(),
                id: primary_client_id.as_str().to_string(),
            },
            "shell-mode",
            mode.name(),
        )
        .with_pane_id(pane_id.to_string())
        .with_metadata("scope", shell_mode_scope_name(scope))
        .with_metadata("changed", changed.to_string())
        .with_metadata("config_generation", generation.to_string());
        record.outcome = if changed { "changed" } else { "no_op" }.to_string();
        audit_log.append(record.sanitized())?;
        Ok(())
    }
}

/// Parses the intentionally narrow `/shell-mode` grammar.
fn parse_shell_mode_command(input: &str) -> Result<ShellModeCommand> {
    let invocation = parse_slash_command(input)?
        .ok_or_else(|| MezError::invalid_args("shell-mode command must be a slash command"))?;
    if invocation.name != "shell-mode" {
        return Err(MezError::invalid_args(
            "shell-mode executor received another slash command",
        ));
    }
    let words = shlex::split(&invocation.args)
        .ok_or_else(|| MezError::invalid_args("shell-mode arguments contain invalid quoting"))?;
    let words = words.iter().map(String::as_str).collect::<Vec<_>>();
    match words.as_slice() {
        ["status"] => Ok(ShellModeCommand::Status),
        ["pane"] => Ok(ShellModeCommand::Set(ShellModeScope::Pane, ShellMode::Pane)),
        ["native"] => Ok(ShellModeCommand::Set(
            ShellModeScope::Pane,
            ShellMode::Native,
        )),
        ["pane", "--global"] => Ok(ShellModeCommand::Set(
            ShellModeScope::Global,
            ShellMode::Pane,
        )),
        ["native", "--global"] => Ok(ShellModeCommand::Set(
            ShellModeScope::Global,
            ShellMode::Native,
        )),
        _ => Err(MezError::invalid_args(
            "shell-mode expects status, pane, or native; pane and native optionally accept --global",
        )),
    }
}

/// Returns the stable command spelling for one scope.
const fn shell_mode_scope_name(scope: ShellModeScope) -> &'static str {
    match scope {
        ShellModeScope::Pane => "pane",
        ShellModeScope::Global => "global",
    }
}

#[cfg(test)]
mod tests {
    use super::{ShellModeCommand, ShellModeScope, parse_shell_mode_command};
    use crate::runtime::config::ShellMode;

    /// Verifies shell-mode accepts the read-only status view and mode selection
    /// defaults to the pane with only the optional global scope flag.
    #[test]
    fn parses_narrow_shell_mode_grammar_and_scope() {
        assert_eq!(
            parse_shell_mode_command("/shell-mode status").unwrap(),
            ShellModeCommand::Status
        );
        assert_eq!(
            parse_shell_mode_command("/shell-mode pane").unwrap(),
            ShellModeCommand::Set(ShellModeScope::Pane, ShellMode::Pane)
        );
        assert_eq!(
            parse_shell_mode_command("/shell-mode native").unwrap(),
            ShellModeCommand::Set(ShellModeScope::Pane, ShellMode::Native)
        );
        assert_eq!(
            parse_shell_mode_command("/shell-mode pane --global").unwrap(),
            ShellModeCommand::Set(ShellModeScope::Global, ShellMode::Pane)
        );
        assert_eq!(
            parse_shell_mode_command("/shell-mode native --global").unwrap(),
            ShellModeCommand::Set(ShellModeScope::Global, ShellMode::Native)
        );
        assert!(parse_shell_mode_command("/shell-mode").is_err());
        assert!(parse_shell_mode_command("/shell-mode status --global").is_err());
        assert!(parse_shell_mode_command("/shell-mode native --yes").is_err());
        assert!(parse_shell_mode_command("/shell-mode --global native").is_err());
    }
}
