//! Runtime agent macro discovery and managed-step helpers.
//!
//! This module keeps pane-scoped macro catalog discovery beside the skill
//! discovery helpers. It also owns the narrow bridge that lets macro-managed
//! `send_message` traffic become ordinary agent-shell turns in a persistent
//! child subagent session.

use super::*;
use crate::macros::{MacroCatalog, discover_macro_catalog};
use crate::project::TrustDecision;
use crate::scheduler::{ScheduledWork, ScheduledWorkKind};

impl RuntimeSessionService {
    /// Builds the effective macro catalog for one pane.
    ///
    /// User macros are read from the configured user root. Project macros are
    /// included only when the pane is inside a trusted project root.
    ///
    /// # Parameters
    /// - `pane_id`: Pane whose current working directory scopes project macros.
    pub(in crate::runtime) fn effective_macro_catalog_for_pane(
        &self,
        pane_id: &str,
    ) -> MacroCatalog {
        let project_root = self.trusted_macro_project_root_for_pane(pane_id);
        discover_macro_catalog(self.config_root.as_deref(), project_root.as_deref())
    }

    /// Returns the trusted project root whose macros may apply to one pane.
    ///
    /// # Parameters
    /// - `pane_id`: Pane whose working directory determines project scope.
    fn trusted_macro_project_root_for_pane(&self, pane_id: &str) -> Option<PathBuf> {
        let working_directory = self.pane_current_working_directory(pane_id)?;
        let store = self.project_trust_store.as_ref()?;
        store
            .records()
            .filter(|record| record.state == TrustDecision::Trusted)
            .find(|record| {
                runtime_path_under_project_root(&working_directory, &record.project_root)
            })
            .map(|record| record.project_root.clone())
    }

    /// Registers one spawned subagent as macro-managed.
    ///
    /// Future macro orchestration uses this marker after creating the one
    /// persistent child session for a macro run. Only marked children receive
    /// the send-message-to-agent-shell bridge, which preserves ordinary MMP
    /// behavior for unrelated ad hoc subagent messages.
    ///
    /// # Parameters
    /// - `child_agent_id`: Runtime child agent id, such as `agent-%2`.
    pub fn register_macro_managed_subagent(&mut self, child_agent_id: &str) {
        self.macro_managed_subagent_agents
            .insert(child_agent_id.to_string());
    }

    /// Starts a normal child agent-shell turn for a macro step message.
    ///
    /// The bridge is intentionally limited to macro-managed child agents and
    /// text payloads. When it applies, the message payload is queued through the
    /// same scheduler and provider path as an ordinary prompt submitted in the
    /// child subagent shell, which preserves slash-command behavior such as
    /// `/loop` while keeping the parent action result tied to the child task
    /// result route.
    ///
    /// # Parameters
    /// - `parent_turn`: Parent turn that emitted the `send_message` action.
    /// - `action`: Parent action whose result should wait for the child step.
    /// - `recipient`: Model-supplied recipient string from the action.
    /// - `content_type`: Canonical MMP content type for the payload.
    /// - `payload`: Text prompt to queue in the child agent shell.
    pub(in crate::runtime) fn queue_macro_managed_message_step(
        &mut self,
        parent_turn: &AgentTurnRecord,
        action: &AgentAction,
        recipient: &str,
        content_type: &str,
        payload: &str,
    ) -> Result<Option<ActionResult>> {
        if content_type != "text/plain; charset=utf-8" {
            return Ok(None);
        }
        let Some(child_agent_id) = macro_message_recipient_agent_id(recipient) else {
            return Ok(None);
        };
        if !self
            .macro_managed_subagent_agents
            .contains(child_agent_id.as_str())
        {
            return Ok(None);
        }
        let child_lineage = self
            .subagent_lineage
            .get(child_agent_id.as_str())
            .ok_or_else(|| MezError::invalid_state("macro-managed subagent lineage is missing"))?;
        let child_parent_agent_id = child_lineage.parent_agent_id.clone();
        let child_display_name = child_lineage.display_name.clone();
        if child_parent_agent_id != parent_turn.agent_id {
            return Err(MezError::forbidden(
                "macro-managed subagent step recipient does not belong to the parent turn",
            ));
        }
        let child_pane_id = child_agent_id
            .strip_prefix("agent-")
            .ok_or_else(|| MezError::invalid_state("macro-managed child agent id is invalid"))?;
        runtime_pane_by_id(&self.session, child_pane_id)?;
        let context = self.agent_context_for_pane_prompt(child_pane_id, payload, 100)?;
        let context = self.apply_agent_shell_preference_context(child_pane_id, context)?;
        let turn_id = self.next_agent_turn_id();
        let created_at_unix_seconds = current_unix_seconds();
        let (model_profile_name, model_profile) =
            self.active_model_profile_for_pane(child_pane_id, &child_agent_id, None)?;
        let turn = AgentTurnRecord {
            turn_id: turn_id.clone(),
            agent_id: child_agent_id.to_string(),
            pane_id: child_pane_id.to_string(),
            trigger: crate::agent::AgentTurnTrigger::LocalMessage,
            started_at_unix_seconds: created_at_unix_seconds,
            policy_profile: "runtime".to_string(),
            model_profile: model_profile_name.clone(),
            parent_turn_id: Some(parent_turn.turn_id.clone()),
            cooperation_mode: Some("macro-step".to_string()),
            state: AgentTurnState::Queued,
        };
        self.agent_turn_ledger.queue_turn(turn.clone())?;
        self.agent_turn_contexts.insert(turn_id.clone(), context);
        self.agent_turn_model_profiles
            .insert(turn_id.clone(), model_profile);
        self.subagent_task_routes
            .insert(turn_id.clone(), parent_turn.agent_id.clone());
        self.joined_subagent_dependencies.insert(
            turn_id.clone(),
            JoinedSubagentDependency {
                parent_turn_id: parent_turn.turn_id.clone(),
                parent_action_id: action.id.clone(),
                child_turn_id: turn_id.clone(),
                child_agent_id: child_agent_id.to_string(),
                child_display_name: Some(child_display_name.clone()),
            },
        );
        self.agent_scheduler.enqueue(ScheduledWork {
            turn_id: turn_id.clone(),
            agent_id: child_agent_id.to_string(),
            pane_id: Some(child_pane_id.to_string()),
            kind: ScheduledWorkKind::ShellCapable,
        })?;
        self.append_agent_trace_turn_event(
            child_pane_id,
            &turn_id,
            "created state=queued reason=macro_message_step",
        )?;
        self.append_agent_trace_turn_event(
            child_pane_id,
            &turn_id,
            &format!(
                "context prepared blocks={} model_profile={}",
                self.agent_turn_contexts
                    .get(&turn_id)
                    .map(|context| context.blocks.len())
                    .unwrap_or_default(),
                model_profile_name
            ),
        )?;
        self.append_agent_trace_turn_event(
            child_pane_id,
            &turn_id,
            "scheduler enqueue kind=shell_capable reason=macro_message_step",
        )?;
        self.start_ready_agent_turns()?;
        Ok(Some(ActionResult::running(
            parent_turn,
            action,
            vec![format!(
                "macro step delivered to {child_agent_id}; waiting for subagent result"
            )],
            Some(format!(
                r#"{{"recipient":"{}","delivery_status":"accepted","join_policy":"macro_step","join_state":"waiting","child_agent_id":"{}","child_turn_id":"{}","error":null}}"#,
                json_escape(recipient),
                json_escape(&child_agent_id),
                json_escape(&turn_id)
            )),
        )))
    }
}

/// Returns the target agent id for a direct agent-recipient string.
fn macro_message_recipient_agent_id(recipient: &str) -> Option<String> {
    recipient
        .strip_prefix("agent:")
        .filter(|agent_id| !agent_id.trim().is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            recipient
                .starts_with("agent-%")
                .then(|| recipient.to_string())
        })
}
