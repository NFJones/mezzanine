//! Human-authenticated compaction selection. This module changes only future
//! admissions; it never starts compaction or edits accepted replay history.

use super::shell::AgentShellCommandOrigin;
use super::{
    AgentShellCommandOutcome, ConfigMutation, ConfigMutationOperation, ConfigMutationValue,
    ConfigScope, MezError, Result, RuntimeSessionService, parse_slash_command,
    runtime_apply_persisted_config_mutation_batch, runtime_effective_config_value,
    runtime_primary_config_path,
};
use crate::integrations::agent::slash::AgentShellPresentation;
use crate::runtime::config::CompactionMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operation {
    Status,
    Select {
        global: bool,
        mode: Option<CompactionMode>,
    },
}

fn parse(input: &str) -> Result<Operation> {
    let invocation = parse_slash_command(input)?
        .filter(|invocation| invocation.name == "compaction-mode")
        .ok_or_else(|| MezError::invalid_args("expected /compaction-mode"))?;
    let words = shlex::split(&invocation.args).ok_or_else(|| {
        MezError::invalid_args("compaction-mode arguments contain invalid quoting")
    })?;
    let words = words.iter().map(String::as_str).collect::<Vec<_>>();
    let (word, global) = match words.as_slice() {
        [] | ["status"] => return Ok(Operation::Status),
        [word] => (*word, false),
        [word, "--global"] => (*word, true),
        _ => return Err(grammar_error()),
    };
    let mode = match word {
        "summary" => Some(CompactionMode::Summary),
        "raw_tail" => Some(CompactionMode::RawTail),
        "reset" => None,
        _ => return Err(grammar_error()),
    };
    Ok(Operation::Select { global, mode })
}

fn grammar_error() -> MezError {
    MezError::invalid_args(
        "compaction-mode expects status, summary, raw_tail, or reset; only selections/reset accept a trailing --global",
    )
}

/// Narrow exception for read-only guards; invalid/mutating forms stay fenced.
pub(super) fn is_status(input: &str) -> bool {
    matches!(parse(input), Ok(Operation::Status))
}

impl RuntimeSessionService {
    pub(super) fn execute_compaction_mode_command(
        &mut self,
        primary: &mez_core::ids::ClientId,
        pane: &str,
        input: &str,
        origin: AgentShellCommandOrigin,
    ) -> Result<AgentShellCommandOutcome> {
        if !self.session.is_attached_primary(primary) {
            return Err(MezError::forbidden(
                "compaction-mode requires an attached primary client",
            ));
        }
        let operation = parse(input)?;
        let visibility = self
            .agent_shell_store()
            .get(pane)
            .ok_or_else(|| MezError::invalid_state("compaction-mode requires a live agent pane"))?
            .visibility;
        let Operation::Select { global, mode } = operation else {
            return Ok(AgentShellCommandOutcome::Presented {
                command: "compaction-mode".into(),
                body: self.render_compaction_mode_status(pane)?,
                presentation: AgentShellPresentation::Pager,
            });
        };
        if !origin.is_authenticated_primary_input() {
            return Err(MezError::forbidden(
                "compaction-mode mutations require authenticated primary-client input",
            ));
        }
        if (global && self.any_agent_is_compacting()) || (!global && self.agent_is_compacting(pane))
        {
            return Err(MezError::conflict(
                "compaction is active; wait for it to settle or use /stop before changing selection",
            ));
        }
        let changed = if global {
            // All checks precede even config-root creation.
            let path = runtime_primary_config_path(self)?.ok_or_else(|| {
                MezError::invalid_state(
                    "global compaction-mode requires a writable primary configuration target",
                )
            })?;
            runtime_apply_persisted_config_mutation_batch(
                self,
                path,
                &[ConfigMutation {
                    path: "agents.compaction_mode".into(),
                    operation: mode.map_or(ConfigMutationOperation::Unset, |mode| {
                        ConfigMutationOperation::Set(ConfigMutationValue::String(
                            mode.name().into(),
                        ))
                    }),
                }],
                "agent-shell-compaction-mode",
            )?
            .changed
        } else {
            self.set_agent_compaction_mode_override(pane, mode)
        };
        Ok(AgentShellCommandOutcome::Mutated {
            command: "compaction-mode".into(),
            body: format!(
                "Compaction mode scope={} changed={changed}; effective={} override_masks_global={}\n\n{}",
                if global { "global" } else { "pane" },
                self.effective_agent_compaction_mode(pane).name(),
                self.agent_compaction_mode_override(pane).is_some(),
                self.render_compaction_mode_status(pane)?
            ),
            visibility,
        })
    }

    fn render_compaction_mode_status(&self, pane: &str) -> Result<String> {
        let local = self.agent_compaction_mode_override(pane);
        let primary = self
            .integration
            .config_layers()
            .iter()
            .find(|layer| layer.scope == ConfigScope::Primary);
        let primary_value = primary
            .map(|layer| runtime_effective_config_value(std::slice::from_ref(layer)))
            .transpose()?
            .and_then(|value| {
                value
                    .get("agents")?
                    .get("compaction_mode")?
                    .as_str()
                    .map(String::from)
            });
        let provenance = primary
            .map(|layer| {
                layer
                    .path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| format!("{} (no disk target)", layer.name))
            })
            .unwrap_or_else(|| "none".into());
        let captured = self
            .runtime_compaction_status(pane)
            .and_then(|status| status.captured_mode);
        Ok(format!(
            "# Compaction Mode Status\n\n| Field | Value |\n| --- | --- |\n| Pane | `{pane}` |\n| Effective mode | `{}` |\n| Source | {} |\n| Explicit override | {} |\n| Configured default | `{}` |\n| Primary-config value | {} |\n| Primary-config provenance | {} |\n| Persistence | {} |\n| Active captured mode | {} |\n",
            self.effective_agent_compaction_mode(pane).name(),
            if local.is_some() {
                "pane override"
            } else {
                "configured default"
            },
            local.map_or("none", |mode| mode.name()),
            self.agent_compaction_mode().name(),
            primary_value.as_deref().unwrap_or("unset"),
            provenance.replace('|', "\\|"),
            self.persistence
                .primary_config_persistence_status(primary, self.session.config_generation),
            captured.map_or("none", |mode| mode.name())
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compaction_mode_strict_grammar() {
        for input in ["/compaction-mode", "/compaction-mode status"] {
            assert_eq!(parse(input).unwrap(), Operation::Status);
        }
        for (name, mode) in [
            ("summary", Some(CompactionMode::Summary)),
            ("raw_tail", Some(CompactionMode::RawTail)),
            ("reset", None),
        ] {
            for global in [false, true] {
                assert_eq!(
                    parse(&format!(
                        "/compaction-mode {name}{}",
                        if global { " --global" } else { "" }
                    ))
                    .unwrap(),
                    Operation::Select { global, mode }
                );
            }
        }
        for args in [
            "status --global",
            "--global summary",
            "raw-tail",
            "toggle",
            "summary extra",
            "summary --global --global",
            "reset --pane",
            "'summary",
            "summary --yes",
            "30",
        ] {
            assert!(
                parse(&format!("/compaction-mode {args}")).is_err(),
                "{args}"
            );
            assert!(!is_status(&format!("/compaction-mode {args}")));
        }
    }
}
