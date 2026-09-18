//! Deferred execution for runtime slash commands that read stores or files.
//!
//! Prompt slash commands are applied inside the attached-client actor request, so
//! a command that walks a store or the filesystem parks the serialized actor for
//! the whole read while every other client keystroke, pane output chunk, and
//! provider event waits behind it. This module owns the off-actor half of that
//! split: the actor captures owned inputs while it still owns the pane, a worker
//! renders the outcome, and the actor applies it through the same display path
//! the inline lane used, so presentation stays byte-identical.
//!
//! Families move here one at a time. A command that is not listed in
//! [`RUNTIME_AGENT_OFF_ACTOR_COMMANDS`] keeps executing inline even when the
//! disposition classifier calls it deferred, so the classification stays the
//! contract this executor consumes instead of a switch for every deferred name
//! at once.

use super::{
    AgentShellCommandOutcome, AgentShellVisibility, MezError, Result, RuntimeSessionService,
    lists::{runtime_agent_macro_catalog_body, runtime_agent_skill_catalog_body},
    runtime_agent_shell_command_response_json,
    shell::agent_shell_invalid_command_response_json,
};
use crate::runtime::{
    RuntimeAgentCommandAsyncOutcome, RuntimeAgentCommandAsyncWork, RuntimeAgentCommandDispatch,
    RuntimeAgentCommandPrepared, runtime_agent_shell_deferred_command_response_json,
};

/// Slash commands whose store or filesystem reads already run off the actor.
///
/// Every entry must also be classified
/// [`super::disposition::RuntimeAgentSlashCommandDisposition::Deferred`], which
/// the guard test below pins.
pub(crate) const RUNTIME_AGENT_OFF_ACTOR_COMMANDS: &[&str] =
    &["list-skills", "list-macros", "auth-status"];

impl RuntimeSessionService {
    /// Starts one actor-owned claim for a deferred slash command.
    ///
    /// The generation is stamped by the actor that dispatches the work and
    /// compared when the outcome settles, so resubmitting the same command while
    /// an earlier attempt is still in flight cannot apply a stale display.
    pub(crate) fn begin_agent_command_claim(&mut self, pane_id: &str) -> u64 {
        self.agent.begin_agent_command_claim(pane_id)
    }

    /// Returns the current deferred slash-command claim generation for a pane.
    pub(crate) fn agent_command_claim_generation(&self, pane_id: &str) -> u64 {
        self.agent.agent_command_claim_generation(pane_id)
    }

    /// Queues one deferred slash command for off-actor execution.
    ///
    /// The inline lane refreshed project config layers and trust state before it
    /// read the catalog, so that refresh stays on the actor: the catalog walk -
    /// the read that scales with the pane's skill and macro directories - is what
    /// moves off it, and the refresh itself can follow once its layer computation
    /// is separable from installing the layers.
    pub(crate) fn dispatch_deferred_agent_shell_command(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        command: &str,
        input: &str,
    ) -> Result<String> {
        self.refresh_project_config_layers_for_pane(pane_id)?;
        let claim_generation = self.begin_agent_command_claim(pane_id);
        self.presentation
            .push_pending_deferred_agent_command(RuntimeAgentCommandDispatch {
                primary_client_id: primary_client_id.clone(),
                pane_id: pane_id.to_string(),
                command: command.to_string(),
                input: input.to_string(),
                claim_generation,
            });
        Ok(runtime_agent_shell_deferred_command_response_json(
            pane_id, input, command,
        ))
    }

    /// Captures the owned inputs one deferred slash command may read.
    ///
    /// The claim re-checks the same guards the inline path applied inside the
    /// actor request (attached primary, visible agent prompt, current claim
    /// generation) and then copies exactly what the off-actor execution needs,
    /// so the worker never reaches into live service state. A claim that no
    /// longer matches answers `None` and the worker drops the dispatch.
    pub(crate) fn claim_agent_command_work(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        command: &str,
        input: &str,
        claim_generation: u64,
    ) -> Result<Option<RuntimeAgentCommandAsyncWork>> {
        if !self.session.is_attached_primary(primary_client_id) {
            return Ok(None);
        }
        if self.agent_command_claim_generation(pane_id) != claim_generation {
            return Ok(None);
        }
        let visible = self
            .agent_shell_store()
            .get(pane_id)
            .is_some_and(|session| session.visibility == AgentShellVisibility::Visible);
        if !visible {
            return Ok(None);
        }
        // Each moved family names exactly what the worker may read; a command
        // with no prepared-input variant has no off-actor executor yet and keeps
        // executing inline.
        let prepared = match command {
            "list-skills" | "list-macros" => RuntimeAgentCommandPrepared::Catalog {
                config_root: self
                    .integration
                    .config_root()
                    .map(std::path::Path::to_path_buf),
                project_root: self.trusted_skill_project_root_for_pane(pane_id),
            },
            "auth-status" => RuntimeAgentCommandPrepared::AuthStatus {
                providers: self
                    .provider_registry()
                    .providers()
                    .keys()
                    .cloned()
                    .collect(),
                auth_store: self.auth_store().cloned(),
            },
            _ => return Ok(None),
        };
        Ok(Some(RuntimeAgentCommandAsyncWork {
            pane_id: pane_id.to_string(),
            primary_client_id: primary_client_id.clone(),
            command: command.to_string(),
            input: input.to_string(),
            claim_generation,
            prepared,
        }))
    }

    /// Executes one claimed deferred slash command off actor ownership.
    ///
    /// The function is deliberately static: the worker must not be able to reach
    /// live service state, so everything it may read arrives in `work`. It
    /// returns the same response body the inline lane produced for the same
    /// catalog, which is what makes the completion presentation byte-identical.
    pub(crate) fn execute_deferred_agent_command(
        work: &RuntimeAgentCommandAsyncWork,
    ) -> RuntimeAgentCommandAsyncOutcome {
        let body = match &work.prepared {
            RuntimeAgentCommandPrepared::Catalog {
                config_root,
                project_root,
            } => match work.command.as_str() {
                "list-skills" => runtime_agent_skill_catalog_body(
                    &crate::integrations::skills::discover_skill_catalog(
                        config_root.as_deref(),
                        project_root.as_deref(),
                    ),
                ),
                "list-macros" => runtime_agent_macro_catalog_body(
                    &crate::integrations::macros::discover_macro_catalog(
                        config_root.as_deref(),
                        project_root.as_deref(),
                    ),
                ),
                other => {
                    return RuntimeAgentCommandAsyncOutcome::Failed {
                        message: format!(
                            "deferred catalog command `{other}` has no off-actor executor"
                        ),
                    };
                }
            },
            RuntimeAgentCommandPrepared::AuthStatus {
                providers,
                auth_store,
            } => {
                match super::status::runtime_agent_auth_status_body(providers, auth_store.as_ref())
                {
                    Ok(body) => body,
                    Err(error) => {
                        return RuntimeAgentCommandAsyncOutcome::Failed {
                            message: error.message().to_string(),
                        };
                    }
                }
            }
        };
        let outcome = AgentShellCommandOutcome::Display {
            command: work.command.clone(),
            body,
        };
        RuntimeAgentCommandAsyncOutcome::Response {
            body: runtime_agent_shell_command_response_json(
                &work.pane_id,
                &work.input,
                Some(&outcome),
            ),
        }
    }

    /// Applies one settled deferred slash command outcome on the actor.
    ///
    /// Stale outcomes are dropped: the pane's current claim generation must still
    /// match the dispatched one, so an earlier attempt cannot overwrite a display
    /// the user asked for again. A failure is reported through the same
    /// invalid-command response path the inline lane used.
    ///
    /// A pane that was hidden or closed while the worker ran also drops its
    /// display: the prompt that asked for it no longer exists, and the claim
    /// refuses work for a pane whose agent prompt is not visible, matching the
    /// submit-time guard the inline lane applied.
    pub(crate) fn complete_agent_command_work(
        &mut self,
        work: &RuntimeAgentCommandAsyncWork,
        outcome: RuntimeAgentCommandAsyncOutcome,
    ) -> Result<bool> {
        if self.agent_command_claim_generation(&work.pane_id) != work.claim_generation {
            return Ok(false);
        }
        // The submitting primary can detach while the worker runs; its display
        // must not be painted for a client that is no longer attached.
        if !self.session.is_attached_primary(&work.primary_client_id) {
            return Ok(false);
        }
        let body = match outcome {
            RuntimeAgentCommandAsyncOutcome::Response { body } => body,
            RuntimeAgentCommandAsyncOutcome::Failed { message } => {
                agent_shell_invalid_command_response_json(
                    &work.pane_id,
                    &work.input,
                    &MezError::invalid_state(message),
                )
            }
        };
        self.apply_deferred_agent_shell_response_body(&work.pane_id, &body)?;
        Ok(true)
    }

    /// Runs every queued deferred slash command through the worker lane.
    ///
    /// The production worker claims, executes, and settles through actor
    /// requests; focused tests drive the same three steps in-process so they can
    /// assert the applied body without standing up the async runtime client.
    #[cfg(test)]
    pub(crate) fn run_pending_deferred_agent_command_for_tests(
        &mut self,
    ) -> Result<Option<String>> {
        let mut last_body = None;
        for dispatch in self.take_pending_deferred_agent_commands() {
            let Some(work) = self.claim_agent_command_work(
                &dispatch.primary_client_id,
                &dispatch.pane_id,
                &dispatch.command,
                &dispatch.input,
                dispatch.claim_generation,
            )?
            else {
                continue;
            };
            let outcome = RuntimeSessionService::execute_deferred_agent_command(&work);
            let body = match &outcome {
                RuntimeAgentCommandAsyncOutcome::Response { body } => body.clone(),
                RuntimeAgentCommandAsyncOutcome::Failed { .. } => String::new(),
            };
            if !self.complete_agent_command_work(&work, outcome)? {
                continue;
            }
            last_body = Some(body);
        }
        Ok(last_body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::commands::disposition::{
        RuntimeAgentSlashCommandDisposition, runtime_agent_slash_command_disposition,
    };

    /// Verifies every command the executor runs off the actor is also classified
    /// deferred, so the two lists cannot drift apart.
    #[test]
    fn runtime_agent_off_actor_commands_are_classified_deferred() {
        for command in RUNTIME_AGENT_OFF_ACTOR_COMMANDS {
            assert_eq!(
                runtime_agent_slash_command_disposition(command),
                RuntimeAgentSlashCommandDisposition::Deferred,
                "`{command}` runs off the actor, so the disposition classifier must defer it"
            );
        }
    }

    /// Verifies the catalog executor renders the same body the inline lane
    /// returned, which is what keeps the deferred presentation byte-identical.
    #[test]
    fn runtime_agent_deferred_catalog_matches_the_inline_body() {
        let catalog = crate::integrations::skills::discover_skill_catalog(None, None);
        let work = RuntimeAgentCommandAsyncWork {
            pane_id: "%1".to_string(),
            primary_client_id: mez_core::ids::ClientId::parse('c', "c1".to_string()).unwrap(),
            command: "list-skills".to_string(),
            input: "/list-skills".to_string(),
            claim_generation: 1,
            prepared: RuntimeAgentCommandPrepared::Catalog {
                config_root: None,
                project_root: None,
            },
        };
        let expected = runtime_agent_shell_command_response_json(
            &work.pane_id,
            &work.input,
            Some(&AgentShellCommandOutcome::Display {
                command: "list-skills".to_string(),
                body: runtime_agent_skill_catalog_body(&catalog),
            }),
        );
        let RuntimeAgentCommandAsyncOutcome::Response { body } =
            RuntimeSessionService::execute_deferred_agent_command(&work)
        else {
            panic!("the catalog executor must produce a response body");
        };
        assert_eq!(
            body, expected,
            "the deferred catalog body must match the inline body byte for byte"
        );
    }
}
