//! Inline human slash-browser snapshot and prompt-local presentation capture.
//!
//! Opening is diagnostic: the existing actor-owned browser builder qualifies
//! current trusted roots and reconciles exact targets once. It grants no new
//! controls, MMP scope, provider work or conversation. The generic text/control
//! response is ordinary Presented/Pager Markdown. Only the actual prompt
//! renderer supplies a local capture, which is dropped on error and adopted
//! once under exact client/pane/interaction-generation checks; it is never
//! stored in session/global pending state while command execution is running.

use super::shell::agent_shell_invalid_command_response_json;
use super::{EventKind, json_escape, runtime_agent_shell_command_response_json};
use crate::error::{MezError, Result};
use crate::integrations::agent::slash::{AgentShellCommandOutcome, AgentShellPresentation};
use crate::runtime::RuntimeSessionService;
use crate::runtime::control::agent_browser::AgentBrowserQuery;
use crate::runtime::service_state::{
    RuntimeRecordBrowserOverlaySource, RuntimeRecordBrowserOverlayState,
};
use mez_agent::slash::ListAgentsOptions;
use mez_core::ids::ClientId;

/// One synchronous prompt interaction, not a wire capability or deferred claim.
/// Its browser remains local until validated immediate pager registration.
pub(crate) struct AgentPromptCommandCapture {
    /// Whether this renderer uses adapter-owned ordinary worker dispatch.
    pub(crate) queue_external_effects: bool,
    /// Whether this interaction submitted the canonical human browser command.
    pub(crate) browser_requested: bool,
    /// Authenticated primary that owns the original prompt interaction.
    pub(crate) client: ClientId,
    /// Actual prompt pane, independent of global focus.
    pub(crate) pane: String,
    /// Existing native action generation before inline command execution.
    pub(crate) generation: u32,
    /// Preferred process incarnation before command work, including absence.
    pub(crate) process: Option<crate::runtime::processes::RuntimePaneProcessIdentity>,
    /// Exact conversation before work; never rebound to a replacement prompt.
    pub(crate) conversation: Option<String>,
    /// Exactly the shared builder's snapshot; populated only by list-agents.
    /// Out-of-line local ownership keeps the synchronous prompt frame bounded
    /// without enlarging service/actor state or granting a global UI capability.
    pub(crate) browser: Option<Box<RuntimeRecordBrowserOverlayState>>,
}

impl RuntimeSessionService {
    /// Settles only an already-admitted runtime browser command. Parser/build
    /// errors use the existing pane-local response; one lifecycle event then
    /// checkpoint follow exactly the generic read-only command tail. Failure of
    /// either publication step propagates Err, so the renderer drops its local
    /// capture rather than presenting an unsettled result. Non-browser ingress,
    /// steering, new/clear/exit and provider/MCP execution never enter this owner.
    pub(super) fn execute_agent_shell_list_agents_response(
        &mut self,
        client: &ClientId,
        pane: &str,
        input: &str,
        args: Option<&str>,
        capture: Option<&mut AgentPromptCommandCapture>,
    ) -> Result<String> {
        let outcome = args
            .ok_or_else(|| MezError::invalid_args(mez_agent::slash::LIST_AGENTS_USAGE))
            .and_then(|args| {
                mez_agent::slash::parse_list_agents_options(args).map_err(MezError::invalid_args)
            })
            .and_then(|options| {
                self.execute_agent_shell_list_agents_command(client, pane, options, capture)
            });
        let response = match outcome {
            Ok(outcome) => runtime_agent_shell_command_response_json(pane, input, Some(&outcome)),
            Err(error) => agent_shell_invalid_command_response_json(pane, input, &error),
        };
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","agent_shell_command":"{}"}}"#,
                json_escape(pane),
                json_escape(input)
            ),
        )?;
        self.checkpoint_agent_session_metadata()?;
        Ok(response)
    }

    /// Builds one scoped snapshot and its existing display outcome. Textual
    /// callers omit capture, so they cannot leave typed targets for a later UI.
    pub(super) fn execute_agent_shell_list_agents_command(
        &mut self,
        client: &ClientId,
        pane: &str,
        options: ListAgentsOptions,
        capture: Option<&mut AgentPromptCommandCapture>,
    ) -> Result<AgentShellCommandOutcome> {
        let query = if options.all {
            AgentBrowserQuery::All
        } else {
            self.current_project_agent_browser_query(client, pane)?
        };
        let (browser, targets) = self.agent_management_browser_for_query(client, &query)?;
        // Text/control callers need the same nonsensitive error chrome as the
        // human pager. Raw copy/save exports intentionally omit that chrome.
        let body = browser.render_page().markdown;
        if let Some(capture) = capture {
            if capture.client != *client || capture.pane != pane {
                return Err(MezError::conflict("agent browser prompt owner changed"));
            }
            capture.browser = Some(Box::new(RuntimeRecordBrowserOverlayState {
                pane_id: pane.into(),
                command: "list-agents".into(),
                source: Some(RuntimeRecordBrowserOverlaySource::Agents {
                    client_id: client.as_str().into(),
                    query,
                    targets,
                    confirmation: None,
                }),
                browser,
                stack: Vec::new(),
            }));
        }
        Ok(AgentShellCommandOutcome::Presented {
            command: "list-agents".into(),
            body,
            presentation: AgentShellPresentation::Pager,
        })
    }
}
