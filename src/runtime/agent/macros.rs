//! Runtime agent macro discovery and managed-step helpers.
//!
//! This module keeps pane-scoped macro catalog discovery beside the skill
//! discovery helpers. It also owns the narrow bridge that lets macro-managed
//! `send_message` traffic become ordinary agent-shell turns in a persistent
//! child subagent session.

use super::*;
use crate::macros::{MacroCatalog, MacroDefinition, discover_macro_catalog, load_macro_definition};
use crate::project::TrustDecision;
use crate::runtime::RuntimeAgentPromptTurnStart;
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

    /// Removes a subagent from the macro-managed set.
    ///
    /// Must be called whenever a macro-managed child pane closes, fails to
    /// spawn, or is torn down with its parent. This prevents stale entries
    /// from accumulating and prevents recycled pane ids from hijacking
    /// macro bridge routing.
    ///
    /// # Parameters
    /// - `child_agent_id`: Runtime child agent id, such as `agent-%2`.
    pub fn deregister_macro_managed_subagent(&mut self, child_agent_id: &str) {
        self.macro_managed_subagent_agents.remove(child_agent_id);
    }

    /// Starts the parent orchestration turn for an explicit `#macro` prompt.
    ///
    /// The runtime loads the configured macro, creates one persistent child
    /// subagent, marks that child as macro-managed for later step messages, and
    /// starts the parent model turn with a runtime hint that describes the
    /// macro sequence and required child recipient. The parent model remains
    /// responsible for adapting each step prompt and judging each child result.
    ///
    /// # Parameters
    /// - `pane_id`: Parent pane where the user invoked the macro.
    /// - `prompt`: Original user prompt beginning with `#<macro-name>`.
    pub(in crate::runtime) fn start_agent_macro_prompt_turn(
        &mut self,
        pane_id: &str,
        prompt: &str,
    ) -> Result<RuntimeAgentPromptTurnStart> {
        let invocation = crate::macros::parse_macro_prompt_invocation(prompt)
            .ok_or_else(|| MezError::invalid_args("macro prompt must start with #<macro-name>"))?;
        let catalog = self.effective_macro_catalog_for_pane(pane_id);
        let summary = catalog.get(&invocation.name).cloned().ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                format!("agent macro is not available: #{}", invocation.name),
            )
        })?;
        let definition = load_macro_definition(&summary)?;
        let controller =
            self.session.primary_client_id().cloned().ok_or_else(|| {
                MezError::invalid_state("agent macro requires an attached primary")
            })?;
        let parent_agent_id = format!("agent-{pane_id}");
        let params = serde_json::json!({
            "parent_agent": { "agent_id": parent_agent_id },
            "placement": "new-window",
            "role": "worker",
            "cooperation_mode": "owned-write",
           "prompt": "",
           "skip_initial_turn": true,
        })
        .to_string();
        let spawn = runtime_subagent_spawn_request(&params, false)?;
        let placement = runtime_subagent_placement_mode(&params)?;
        let spawn_json = self.spawn_runtime_subagent(&controller, spawn, placement)?;
        let (child_agent_id, _child_display_name, _child_turn_id) =
            runtime_spawn_json_agent_and_turn(&spawn_json)?;
        // idle spawn: child_turn_id is None, which is expected for macro session
        let _ = _child_turn_id;
        self.register_macro_managed_subagent(&child_agent_id);
        self.append_agent_trace_turn_event(
            pane_id,
            "",
            &format!("macro child spawned idle child_agent_id={}", child_agent_id),
        )?;
        let orchestration_prompt = runtime_macro_parent_orchestration_prompt(
            &definition,
            invocation.additional_context.as_deref(),
            &child_agent_id,
        );
        let started = self.start_agent_prompt_turn_with_cooperation(
            pane_id,
            &orchestration_prompt,
            Some("macro-orchestration".to_string()),
        )?;
        self.append_agent_trace_turn_event(
            pane_id,
            &started.turn_id,
            &format!(
                "macro orchestration started name={} child_agent_id={}",
                definition.summary.name, child_agent_id
            ),
        )?;
        Ok(started)
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
        let Some(child_lineage) = self.subagent_lineage.get(child_agent_id.as_str()) else {
            return Ok(Some(ActionResult::failed(
                parent_turn,
                action,
                ActionStatus::Failed,
                "macro_bridge_error",
                "macro-managed subagent lineage is missing",
            )?));
        };
        let child_parent_agent_id = child_lineage.parent_agent_id.clone();
        let child_display_name = child_lineage.display_name.clone();
        if child_parent_agent_id != parent_turn.agent_id {
            return Ok(Some(ActionResult::failed(
                parent_turn,
                action,
                ActionStatus::Failed,
                "macro_bridge_error",
                "macro-managed subagent step recipient does not belong to the parent turn",
            )?));
        }
        let child_pane_id = child_agent_id
            .strip_prefix("agent-")
            .ok_or_else(|| MezError::invalid_state("macro-managed child agent id is invalid"))?;
        runtime_pane_by_id(&self.session, child_pane_id)?;
        // --- Ordering guard: reject if a macro step is already in-flight for
        // this parent turn + child agent pair. ---
        let macro_step_in_flight = self
            .joined_subagent_dependencies
            .values()
            .any(|dep| {
                dep.parent_turn_id == parent_turn.turn_id
                    && dep.child_agent_id == child_agent_id
                    && self.joined_subagent_dependency_has_live_child(dep)
            });
        if macro_step_in_flight {
            return Ok(Some(ActionResult::failed(
                parent_turn,
                action,
                ActionStatus::Failed,
                "macro_step_ordering",
                "a macro step is already in flight for this subagent; wait for it to complete before sending the next step",
            )?));
        }
        // --- Idempotency guard: retried parent actions reuse the original
        // step result instead of creating another child turn. ---
        if let Some(existing) = self
            .joined_subagent_dependencies
            .values()
            .find(|dep| {
                dep.parent_turn_id == parent_turn.turn_id
                    && dep.parent_action_id == action.id
            })
        {
            if self.joined_subagent_dependency_has_live_child(existing) {
                // Still in progress — return the same running result.
                return Ok(Some(ActionResult::running(
                    parent_turn,
                    action,
                    vec![format!(
                        "macro step already in progress for {child_agent_id}; waiting for subagent result"
                    )],
                    Some(format!(
                        r#"{{"recipient":"{}","delivery_status":"accepted","join_policy":"macro_step","join_state":"waiting","child_agent_id":"{}","child_turn_id":"{}","idempotent":true,"error":null}}"#,
                        json_escape(recipient),
                        json_escape(&child_agent_id),
                        json_escape(&existing.child_turn_id)
                    )),
                )));
            }
            // Child turn already reached a terminal state — return idempotent
            // terminal result.
            let child_state = self
                .agent_turn_ledger
                .turns()
                .iter()
                .find(|t| t.turn_id == existing.child_turn_id)
                .map(|t| t.state);
            match child_state {
                Some(AgentTurnState::Completed) => {
                    return Ok(Some(ActionResult::succeeded(
                        parent_turn,
                        action,
                        vec![format!(
                            "macro step already completed by {child_agent_id} (idempotent)"
                        )],
                        Some(format!(
                            r#"{{"recipient":"{}","delivery_status":"completed","join_policy":"macro_step","child_agent_id":"{}","child_turn_id":"{}","idempotent":true,"error":null}}"#,
                            json_escape(recipient),
                            json_escape(&child_agent_id),
                            json_escape(&existing.child_turn_id)
                        )),
                    )));
                }
                Some(AgentTurnState::Failed) | Some(AgentTurnState::Interrupted) => {
                    return Ok(Some(ActionResult::failed(
                        parent_turn,
                        action,
                        ActionStatus::Failed,
                        "macro_step_failed",
                        "macro step previously failed; cannot retry",
                    )?));
                }
                _ => {
                    // Other terminal state — treat as resolved.
                    return Ok(Some(ActionResult::succeeded(
                        parent_turn,
                        action,
                        vec![format!(
                            "macro step already resolved by {child_agent_id} (idempotent)"
                        )],
                        Some(format!(
                            r#"{{"recipient":"{}","delivery_status":"resolved","join_policy":"macro_step","child_agent_id":"{}","child_turn_id":"{}","idempotent":true,"error":null}}"#,
                            json_escape(recipient),
                            json_escape(&child_agent_id),
                            json_escape(&existing.child_turn_id)
                        )),
                    )));
                }
            }
        }
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
        .map(|id| id.trim().to_owned())
        .or_else(|| {
            recipient
                .starts_with("agent-%")
                .then(|| recipient.to_string())
        })
}

/// Builds the parent model prompt that orchestrates one active macro run.
fn runtime_macro_parent_orchestration_prompt(
    definition: &MacroDefinition,
    additional_context: Option<&str>,
    child_agent_id: &str,
) -> String {
    let mut lines = vec![
        format!("Agent macro invocation: #{}", definition.summary.name),
        format!("Description: {}", definition.summary.description),
        format!("Persistent subagent recipient: agent:{child_agent_id}"),
        "".to_string(),
        "Macro execution rules:".to_string(),
        "- Use the same persistent subagent recipient for every step.".to_string(),
        "- Send exactly one step prompt at a time with send_message.".to_string(),
        "- Each step is interpreted as a normal agent-shell prompt in the subagent, so slash commands such as /loop remain valid.".to_string(),
        "- You may adapt a scripted step to the user's stated intent, but preserve the macro purpose and step order.".to_string(),
        "- After each subagent result, judge success against the step intent, user context, and remaining sequence.".to_string(),
        "- On success, send the next step to the same recipient. On failure, stop and explain the failure.".to_string(),
        "- Finish successfully only after all required steps complete in order.".to_string(),
        "".to_string(),
    ];
    if let Some(context) = additional_context.filter(|context| !context.trim().is_empty()) {
        lines.push("User additional context:".to_string());
        lines.push(context.trim().to_string());
        lines.push(String::new());
    }
    lines.push("Scripted steps:".to_string());
    lines.extend(
        definition
            .steps
            .iter()
            .map(|step| format!("{}. {}", step.index, step.prompt)),
    );
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies that `macro_message_recipient_agent_id` trims whitespace
    /// from the extracted agent id after the `agent:` prefix, so that
    /// recipients like `"agent: agent-%3"` or `"agent:agent-%3 "` are
    /// correctly routed through the macro bridge instead of silently
    /// falling back to plain MMP delivery.
    #[test]
    fn macro_recipient_trims_whitespace_after_agent_prefix() {
        // Leading whitespace after `agent:`
        assert_eq!(
            macro_message_recipient_agent_id("agent: agent-%5"),
            Some("agent-%5".to_string())
        );
        // Trailing whitespace
        assert_eq!(
            macro_message_recipient_agent_id("agent:agent-%7 "),
            Some("agent-%7".to_string())
        );
        // Both leading and trailing whitespace
        assert_eq!(
            macro_message_recipient_agent_id("agent:  agent-%9  "),
            Some("agent-%9".to_string())
        );
        // Only whitespace after agent: should still be filtered (empty after trim)
        assert_eq!(macro_message_recipient_agent_id("agent:   "), None);
        // Normal untrimmed case still works
        assert_eq!(
            macro_message_recipient_agent_id("agent:agent-%3"),
            Some("agent-%3".to_string())
        );
        // Bare agent-% pattern (no agent: prefix) still works
        assert_eq!(
            macro_message_recipient_agent_id("agent-%12"),
            Some("agent-%12".to_string())
        );
    }

    /// Verifies that `deregister_macro_managed_subagent` removes an agent
    /// from the macro-managed set, preventing stale entries from accumulating
    /// and preventing recycled pane ids from hijacking macro bridge routing.
    #[test]
    fn deregister_macro_managed_removes_agent_from_set() {
        let fixture = crate::test_support::runtime::RuntimeServiceFixture::new();
        let mut service = fixture.build();
        let agent_id = "agent-%99";

        // Initially empty
        assert!(!service.macro_managed_subagent_agents.contains(agent_id));

        // Register
        service.register_macro_managed_subagent(agent_id);
        assert!(service.macro_managed_subagent_agents.contains(agent_id));

        // Deregister
        service.deregister_macro_managed_subagent(agent_id);
        assert!(!service.macro_managed_subagent_agents.contains(agent_id));

        // Deregistering an already-absent id is a no-op
        service.deregister_macro_managed_subagent(agent_id);
        assert!(!service.macro_managed_subagent_agents.contains(agent_id));
    }
}
