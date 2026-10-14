//! Synchronous attached-prompt adoption of one typed human agent snapshot.
//!
//! A capture lives on the readline submission stack, not in presentation state
//! or a control response. Both adapter and direct attached rendering paths use
//! the actual pane passed by ingress. Only a successfully parsed matching pager
//! can stage its browser into the existing pending handoff, and staging and
//! consumption are adjacent actor-owned operations without an await. Every
//! rejection drops local capability; every post-registration exit clears all
//! pending browser/source/stack entries. Other command families retain their
//! established deferred-claim and textual behavior.

use super::RuntimeAgentShellDisplayOutput;
use crate::error::{MezError, Result};
use crate::runtime::RuntimeSessionService;
use crate::runtime::commands::list_agents::AgentPromptCommandCapture;
use mez_core::ids::ClientId;

impl RuntimeSessionService {
    /// Captures exact interaction ownership before synchronous command work.
    /// This does not resolve project trust or read a provider/store. Only the
    /// browser command observes process ownership; unrelated prompts are inert.
    pub(crate) fn prepare_agent_prompt_command_capture(
        &self,
        client: &ClientId,
        pane: &str,
        input: &str,
        queue_external_effects: bool,
    ) -> AgentPromptCommandCapture {
        let browser_requested = mez_agent::slash::parse_slash_command(input)
            .ok()
            .flatten()
            .is_some_and(|invocation| invocation.name == "list-agents");
        AgentPromptCommandCapture {
            queue_external_effects,
            browser_requested,
            client: client.clone(),
            pane: pane.into(),
            generation: self
                .presentation
                .overlay_action_registry
                .current_generation(),
            process: browser_requested
                .then(|| self.pane_process_identity(pane).ok())
                .flatten(),
            conversation: browser_requested
                .then(|| {
                    self.agent_shell_store()
                        .get(pane)
                        .map(|session| session.session_id.clone())
                })
                .flatten(),
            browser: None,
        }
    }

    /// Adopts one matching pager capability or renders an ordinary command.
    /// Error/non-pager/stale/client/pane mismatches never register a browser.
    /// Native action identities and the captured source survive mounting intact.
    pub(crate) fn present_captured_agent_prompt_display_output(
        &mut self,
        client: &ClientId,
        pane: &str,
        capture: AgentPromptCommandCapture,
        output: RuntimeAgentShellDisplayOutput,
    ) -> Result<()> {
        let Some(state) = capture.browser else {
            return self.set_agent_prompt_display_output(pane, output);
        };
        // Move the one owned snapshot only in this focused adoption frame.
        // The box never escapes into global pending state or control output.
        let state = *state;
        let process_is_current = match capture.process.as_ref() {
            Some(process) => self
                .pane_process_identity(pane)
                .is_ok_and(|current| process.same_incarnation(&current)),
            None => self.pane_process_identity(pane).is_err(),
        };
        let pager_matches = matches!(&output, RuntimeAgentShellDisplayOutput::Overlay(content) if content.command.as_deref() == Some(state.command.as_str()));
        self.require_live()?;
        if capture.client != *client
            || !capture.browser_requested
            || capture.pane != pane
            || state.pane_id != pane
            || !self.session.is_attached_primary(client)
            || self.presentation.projected_client_id.as_ref() != Some(client)
            || capture.generation
                != self
                    .presentation
                    .overlay_action_registry
                    .current_generation()
            || !process_is_current
            || self
                .agent_shell_store()
                .get(pane)
                .map(|session| session.session_id.as_str())
                != capture.conversation.as_deref()
            || !pager_matches
        {
            return Err(MezError::conflict("agent browser prompt handoff changed"));
        }
        self.register_pending_record_browser_overlay(
            pane,
            &state.command,
            state.browser,
            state.source,
        );
        let result = self.set_agent_prompt_display_output(pane, output);
        self.discard_pending_record_browser_overlay(pane, &state.command);
        result
    }
}
