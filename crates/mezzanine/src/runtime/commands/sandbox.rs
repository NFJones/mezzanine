//! Pane-local sandbox command parsing, execution, and presentation.
//!
//! This module owns the narrow `/sandbox` runtime hierarchy. Status and the
//! default enable/disable operations act on only the invoking pane. Explicit
//! `--global` mutations reuse the atomic persisted-config transaction, while
//! trust operations delegate to their established runtime owner.

use super::shell::AgentShellCommandOrigin;
use super::{
    AgentShellCommandOutcome, AgentShellVisibility, ConfigMutation, ConfigMutationOperation,
    ConfigMutationValue, MezError, Result, RuntimeSessionService, parse_slash_command,
    runtime_apply_persisted_config_mutation_batch, runtime_primary_config_path,
};
use crate::integrations::agent::slash::AgentShellPresentation;
use crate::runtime::{SandboxBackend, SandboxConfig};
use crate::security::audit::{AuditActor, AuditRecord};
use crate::security::sandbox::SandboxPlatformAvailability;

/// Scope selected for an enable, disable, or status operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SandboxScope {
    /// Exact invoking pane only.
    Pane,
    /// Persisted default configuration.
    Global,
}

/// Strict operations accepted by `/sandbox`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SandboxCommand {
    /// Displays effective pane or persisted global state.
    Status(SandboxScope),
    /// Enables the available native confinement backend in the selected scope.
    Enable(SandboxScope),
    /// Selects policy-only execution in the selected scope.
    Disable(SandboxScope),
    /// Delegates to project trust handling.
    Trust,
}

impl RuntimeSessionService {
    /// Executes the canonical `/sandbox` hierarchy.
    ///
    /// # Errors
    /// Returns an argument error for unsupported grammar, a forbidden error
    /// for non-primary mutation, and a config error when global persistence or
    /// live application cannot complete atomically.
    pub(super) fn execute_agent_shell_sandbox_command(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        input: &str,
        origin: AgentShellCommandOrigin,
    ) -> Result<AgentShellCommandOutcome> {
        if !self.session.is_attached_primary(primary_client_id) {
            return Err(MezError::forbidden(
                "sandbox commands require an attached primary client",
            ));
        }
        let operation = parse_sandbox_command(input)?;
        match operation {
            SandboxCommand::Trust => {
                self.execute_agent_shell_trust_command(primary_client_id, pane_id, input)
            }
            SandboxCommand::Status(scope) => Ok(AgentShellCommandOutcome::Presented {
                command: "sandbox".to_string(),
                body: self.render_sandbox_status(pane_id, scope),
                presentation: AgentShellPresentation::Pager,
            }),
            SandboxCommand::Enable(scope) => {
                if !origin.is_authenticated_primary_input() {
                    return Err(MezError::forbidden(
                        "sandbox mutations require authenticated primary-client input",
                    ));
                }
                self.apply_sandbox_backend(primary_client_id, pane_id, scope, true)
            }
            SandboxCommand::Disable(scope) => {
                if !origin.is_authenticated_primary_input() {
                    return Err(MezError::forbidden(
                        "sandbox mutations require authenticated primary-client input",
                    ));
                }
                self.apply_sandbox_backend(primary_client_id, pane_id, scope, false)
            }
        }
    }

    /// Renders pane-effective state with explicit override provenance, or only
    /// the persisted global default when `--global` was selected.
    fn render_sandbox_status(&self, pane_id: &str, scope: SandboxScope) -> String {
        let global = &self.configured_permissions().sandbox;
        if scope == SandboxScope::Global {
            return format!(
                "# Sandbox Status\n\n| Field | Value |\n| --- | --- |\n| Scope | global |\n| Backend | `{}` |\n| Source | persisted configuration |\n| Generation | {} |\n",
                global.as_str(),
                self.session.config_generation,
            );
        }
        let effective = self.sandbox_config_for_pane(pane_id);
        let overridden = self.pane_has_sandbox_override(pane_id);
        format!(
            "# Sandbox Status\n\n| Field | Value |\n| --- | --- |\n| Scope | pane |\n| Pane | `{pane_id}` |\n| Effective backend | `{}` |\n| Global backend | `{}` |\n| Source | {} |\n| Local override | {} |\n| Generation | {} |\n",
            effective.as_str(),
            global.as_str(),
            if overridden {
                "pane override"
            } else {
                "global default"
            },
            if overridden { "yes" } else { "no" },
            self.session.config_generation,
        )
    }

    /// Applies one local override or persisted global backend mutation.
    fn apply_sandbox_backend(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        scope: SandboxScope,
        enable: bool,
    ) -> Result<AgentShellCommandOutcome> {
        let visibility = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| session.visibility)
            .unwrap_or(AgentShellVisibility::Visible);
        let enabled_backend = if enable {
            Some(SandboxPlatformAvailability::current().setup_backend().ok_or_else(|| {
                MezError::invalid_state(
                    "sandbox enable is unavailable because this platform has no native sandbox backend",
                )
            })?)
        } else {
            None
        };
        if let Some((backend, available)) = enabled_backend
            && !available
        {
            return Err(MezError::invalid_state(format!(
                "sandbox enable is unavailable because the fixed {} executable is unavailable",
                backend.as_str()
            )));
        }
        let requested = enabled_backend
            .map(|(backend, _)| backend.as_str())
            .unwrap_or("policy-only");
        let changed = match scope {
            SandboxScope::Pane => {
                let current = self.sandbox_config_for_pane(pane_id);
                let next = if enable {
                    match &self.configured_permissions().sandbox {
                        SandboxConfig::Bubblewrap(config) => {
                            SandboxConfig::Bubblewrap(config.clone())
                        }
                        SandboxConfig::Seatbelt(config) => SandboxConfig::Seatbelt(config.clone()),
                        SandboxConfig::PolicyOnly => match enabled_backend
                            .map(|(backend, _)| backend)
                        {
                            Some(SandboxBackend::Bubblewrap) => SandboxConfig::default_bubblewrap(),
                            Some(SandboxBackend::Seatbelt) => SandboxConfig::default_seatbelt(),
                            None => SandboxConfig::PolicyOnly,
                        },
                    }
                } else {
                    SandboxConfig::PolicyOnly
                };
                let changed = current != next || !self.pane_has_sandbox_override(pane_id);
                self.integration
                    .set_pane_sandbox_override(pane_id, Some(next));
                changed
            }
            SandboxScope::Global => {
                let path = runtime_primary_config_path(self)?.ok_or_else(|| {
                    MezError::invalid_state(
                        "global sandbox mutation requires a primary configuration file",
                    )
                })?;
                runtime_apply_persisted_config_mutation_batch(
                    self,
                    path,
                    &[ConfigMutation {
                        path: "permissions.sandbox".to_string(),
                        operation: ConfigMutationOperation::Set(ConfigMutationValue::String(
                            requested.to_string(),
                        )),
                    }],
                    "agent-shell-sandbox",
                )?
                .changed
            }
        };
        self.append_sandbox_command_audit(
            primary_client_id,
            pane_id,
            if enable { "enable" } else { "disable" },
            scope,
            changed,
        )?;
        Ok(AgentShellCommandOutcome::Mutated {
            command: "sandbox".to_string(),
            body: format!(
                "Sandbox {requested} scope={} changed={changed}; effective={} global={}",
                sandbox_scope_name(scope),
                self.sandbox_config_for_pane(pane_id).as_str(),
                self.configured_permissions().sandbox.as_str(),
            ),
            visibility,
        })
    }

    /// Appends redacted sandbox command metadata.
    fn append_sandbox_command_audit(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        operation: &str,
        scope: SandboxScope,
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
            "sandbox",
            operation,
        )
        .with_pane_id(pane_id.to_string())
        .with_metadata("scope", sandbox_scope_name(scope))
        .with_metadata("changed", changed.to_string())
        .with_metadata("config_generation", generation.to_string());
        record.outcome = if changed { "changed" } else { "no_op" }.to_string();
        audit_log.append(record.sanitized())?;
        Ok(())
    }
}

/// Parses the intentionally narrow `/sandbox` grammar.
fn parse_sandbox_command(input: &str) -> Result<SandboxCommand> {
    let invocation = parse_slash_command(input)?
        .ok_or_else(|| MezError::invalid_args("sandbox command must be a slash command"))?;
    if invocation.name != "sandbox" {
        return Err(MezError::invalid_args(
            "sandbox executor received another slash command",
        ));
    }
    let words = shlex::split(&invocation.args)
        .ok_or_else(|| MezError::invalid_args("sandbox arguments contain invalid quoting"))?;
    let words = words.iter().map(String::as_str).collect::<Vec<_>>();
    match words.as_slice() {
        [] | ["status"] => Ok(SandboxCommand::Status(SandboxScope::Pane)),
        ["status", "--global"] => Ok(SandboxCommand::Status(SandboxScope::Global)),
        ["enable", "--yes"] | ["enable", "--yes", "--global"] => {
            Ok(SandboxCommand::Enable(if words.contains(&"--global") {
                SandboxScope::Global
            } else {
                SandboxScope::Pane
            }))
        }
        ["enable", "--global", "--yes"] => Ok(SandboxCommand::Enable(SandboxScope::Global)),
        ["disable", "--yes"] | ["disable", "--yes", "--global"] => {
            Ok(SandboxCommand::Disable(if words.contains(&"--global") {
                SandboxScope::Global
            } else {
                SandboxScope::Pane
            }))
        }
        ["disable", "--global", "--yes"] => Ok(SandboxCommand::Disable(SandboxScope::Global)),
        ["trust", ..] => Ok(SandboxCommand::Trust),
        ["enable", ..] | ["disable", ..] => Err(MezError::invalid_args(
            "sandbox enable and disable require exactly --yes and optionally --global",
        )),
        _ => Err(MezError::invalid_args(
            "sandbox expects status, enable, disable, or trust",
        )),
    }
}

/// Returns the stable command spelling for one scope.
const fn sandbox_scope_name(scope: SandboxScope) -> &'static str {
    match scope {
        SandboxScope::Pane => "pane",
        SandboxScope::Global => "global",
    }
}

#[cfg(test)]
mod tests {
    use super::{SandboxCommand, SandboxScope, parse_sandbox_command};

    /// Verifies the narrow hierarchy defaults status and mutations to the pane
    /// while requiring confirmation and explicit global selection.
    #[test]
    fn parses_narrow_sandbox_grammar_and_scope() {
        assert_eq!(
            parse_sandbox_command("/sandbox").unwrap(),
            SandboxCommand::Status(SandboxScope::Pane)
        );
        assert_eq!(
            parse_sandbox_command("/sandbox enable --yes").unwrap(),
            SandboxCommand::Enable(SandboxScope::Pane)
        );
        assert_eq!(
            parse_sandbox_command("/sandbox disable --global --yes").unwrap(),
            SandboxCommand::Disable(SandboxScope::Global)
        );
        assert!(parse_sandbox_command("/sandbox enable").is_err());
        assert!(parse_sandbox_command("/sandbox profile export").is_err());
        assert!(parse_sandbox_command("/sandbox toolchains").is_err());
    }
}
