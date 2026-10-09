//! Runtime agent skill discovery and invocation helpers.
//!
//! This module owns the runtime side of non-effecting MAAP skill actions. It
//! keeps catalog lookup, duplicate-call suppression, and skill-context
//! materialization together so the main runtime agent facade can focus on turn
//! orchestration.

use super::{
    ActionResult, ActionStatus, AgentAction, AgentTurnExecution, AgentTurnRecord, AgentTurnState,
    MezError, PathBuf, Result, RuntimeSessionService, runtime_agent_action_summary,
    runtime_agent_turn_state_from_action_results,
};
use crate::integrations::skills::{
    discover_skill_catalog, load_model_skill_document, project_skill_root_for_summary,
};
use mez_agent::{
    SkillActionContext, SkillCatalog, SkillSource, SkillSummary, skill_action_context_from_blocks,
};

/// Exact winning metadata and bounded document identity selected in this turn.
#[derive(Debug, Clone)]
pub(super) struct SkillSelectionReceipt {
    /// Winning source and metadata, never projected as a filesystem capability.
    summary: SkillSummary,
    /// Digest of the bounded bytes; changed bodies require fresh selection.
    digest: String,
}

/// Hashes already bounded skill bytes without exposing document contents.
fn document_digest(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

impl RuntimeSessionService {
    /// Resolves live operator policy; composition errors fail closed.
    fn model_skill_policy(&self) -> Result<mez_agent::skill_discovery::SkillDiscoveryPolicy> {
        let config = crate::config::compose_effective_config(self.integration.config_layers())?;
        Ok(crate::config::skill_discovery_policy(&config))
    }

    /// Reports optional discovery eligibility before conversation surface capture.
    pub(super) fn model_skill_discovery_available(&self, pane: &str) -> Result<bool> {
        let policy = self.model_skill_policy()?;
        if policy.global == Some(false) {
            return Ok(false);
        }
        Ok(!policy
            .project(&self.effective_skill_catalog_for_pane(pane))
            .is_empty())
    }

    /// Resolves only the currently authorized source root, never a payload path.
    fn model_skill_root(&self, pane: &str, summary: &SkillSummary) -> Option<PathBuf> {
        match summary.source {
            SkillSource::Builtin => None,
            SkillSource::User => self
                .integration
                .config_root()
                .map(|root| root.join("skills")),
            SkillSource::Project => self
                .trusted_skill_project_root_for_pane(pane)
                .and_then(|root| project_skill_root_for_summary(&root, summary)),
        }
    }

    /// Builds the effective skill catalog for one pane.
    ///
    /// User skills are always read from the configured user root. Project
    /// skills are included only when the pane is inside a trusted project root.
    ///
    /// # Parameters
    /// - `pane_id`: Pane whose current working directory scopes project skills.
    pub(crate) fn effective_skill_catalog_for_pane(&self, pane_id: &str) -> SkillCatalog {
        let project_root = self.trusted_skill_project_root_for_pane(pane_id);
        let mut catalog =
            discover_skill_catalog(self.integration.config_root(), project_root.as_deref());
        if let Ok(config) =
            crate::config::compose_effective_config(self.integration.config_layers())
        {
            let policy = crate::config::skill_discovery_policy(&config);
            let unknown = policy
                .overrides
                .keys()
                .filter(|name| catalog.get(name).is_none())
                .cloned()
                .collect::<Vec<_>>();
            for name in unknown {
                catalog.diagnostics.push(mez_agent::SkillDiagnostic {
                    path: PathBuf::from(format!("skills.overrides.{name}.discovery")),
                    message: "configured skill name is not in the effective human catalog; policy is inert".to_string(),
                });
            }
        }
        catalog
    }

    /// Returns the trusted project root whose skills may apply to one pane.
    ///
    /// A deeper rejected or revoked decision withholds project skills even when
    /// a broader ancestor is trusted, because implicit authority comes from the
    /// deepest stored project-trust decision.
    ///
    /// # Parameters
    /// - `pane_id`: Pane whose working directory determines project scope.
    pub(crate) fn trusted_skill_project_root_for_pane(&self, pane_id: &str) -> Option<PathBuf> {
        self.trusted_project_root_for_pane(pane_id)
    }

    /// Builds the currently loaded skill context state for one active turn.
    ///
    /// Explicit `$skill` prompt expansion and successful `call_skill` results
    /// both place full skill text in the model context. The runtime uses that
    /// context as the source of truth for suppressing redundant non-effecting
    /// skill actions before they become unbounded provider continuations.
    fn runtime_skill_action_context_for_turn(&self, turn_id: &str) -> Result<SkillActionContext> {
        let context = self
            .agent_turn_contexts()
            .get(turn_id)
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        Ok(skill_action_context_from_blocks(context.blocks()))
    }

    /// Executes a runtime-owned skill lookup or skill-load action.
    ///
    /// # Parameters
    /// - `turn`: Active turn receiving the action result.
    /// - `action`: `request_skills` or `call_skill` action to execute.
    fn execute_skill_action_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        skill_context: &mut SkillActionContext,
    ) -> Result<ActionResult> {
        let denied = || {
            ActionResult::failed(
                turn,
                action,
                ActionStatus::Failed,
                "skill_selection_unavailable",
                "Skill discovery/loading is unavailable, changed, or lacks a current-turn selection; use only the captured actions.",
            )
        };
        self.refresh_project_trust_store_from_disk_if_changed()?;
        let surface = self
            .agent_shell_store()
            .get(&turn.pane_id)
            .and_then(|session| session.allowed_actions.as_ref());
        if surface.is_none_or(|set| {
            !set.contains(mez_agent::AllowedAction::RequestSkills)
                || !set.contains(mez_agent::AllowedAction::CallSkill)
        }) || !self
            .agent_enabled_actions()
            .contains(mez_agent::AllowedAction::RequestSkills)
            || !self
                .agent_enabled_actions()
                .contains(mez_agent::AllowedAction::CallSkill)
        {
            return Ok(denied()?);
        }
        let policy = self.model_skill_policy()?;
        if policy.global == Some(false) {
            return Ok(denied()?);
        }
        let catalog = self.effective_skill_catalog_for_pane(&turn.pane_id);
        match &action.payload {
            mez_agent::AgentActionPayload::RequestSkills => {
                if self
                    .agent
                    .skill_discovery_receipts
                    .contains_key(&turn.turn_id)
                    || !skill_context.loaded_skills.is_empty()
                {
                    return Ok(denied()?);
                }
                let mut receipts = Vec::new();
                for summary in catalog
                    .skills
                    .iter()
                    .filter(|summary| policy.eligible(&summary.name, summary.discovery))
                    .take(256)
                {
                    let root = self.model_skill_root(&turn.pane_id, summary);
                    if let Ok(document) = load_model_skill_document(summary, root.as_deref()) {
                        receipts.push(SkillSelectionReceipt {
                            summary: summary.clone(),
                            digest: document_digest(&document.text),
                        });
                    }
                }
                let metadata = receipts.iter().map(|receipt| serde_json::json!({
                    "name": receipt.summary.name, "description": receipt.summary.description,
                    "source": receipt.summary.source.as_str(),
                })).collect::<Vec<_>>();
                let text = serde_json::json!({"skills": metadata}).to_string();
                self.agent
                    .skill_discovery_receipts
                    .insert(turn.turn_id.clone(), receipts);
                skill_context.catalog_requested = true;
                Ok(ActionResult::succeeded(
                    turn,
                    action,
                    vec![text.clone()],
                    Some(text),
                ))
            }
            mez_agent::AgentActionPayload::CallSkill {
                name,
                additional_context,
            } => {
                let Some(receipt) = self
                    .agent
                    .skill_discovery_receipts
                    .get(&turn.turn_id)
                    .and_then(|rows| rows.iter().find(|row| row.summary.name == *name))
                    .cloned()
                else {
                    return Ok(denied()?);
                };
                if skill_context.loaded_skills.contains(name)
                    || self
                        .agent
                        .model_loaded_skills
                        .get(&turn.turn_id)
                        .is_some_and(|names| names.contains(name))
                    || !policy.eligible(name, receipt.summary.discovery)
                    || catalog.get(name) != Some(&receipt.summary)
                {
                    return Ok(denied()?);
                }
                let root = self.model_skill_root(&turn.pane_id, &receipt.summary);
                let Ok(document) = load_model_skill_document(&receipt.summary, root.as_deref())
                else {
                    return Ok(denied()?);
                };
                if document_digest(&document.text) != receipt.digest {
                    return Ok(denied()?);
                }
                let content =
                    self.runtime_skill_context_text(document, additional_context.as_deref())?;
                self.agent
                    .model_loaded_skills
                    .entry(turn.turn_id.clone())
                    .or_default()
                    .insert(name.clone());
                skill_context.loaded_skills.insert(name.clone());
                Ok(ActionResult::succeeded(
                    turn,
                    action,
                    vec![content],
                    Some(
                        serde_json::json!({
                            "name":name, "source":receipt.summary.source.as_str()
                        })
                        .to_string(),
                    ),
                ))
            }
            _ => Err(MezError::invalid_args("not a skill action")),
        }
    }

    /// Executes any provider-produced non-effecting skill actions and appends
    /// their results to running turn context for provider continuation.
    ///
    /// # Parameters
    /// - `turn`: Active turn containing the running action results.
    /// - `execution`: Provider execution whose pending skill results are updated.
    pub(super) fn execute_running_skill_actions_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &mut AgentTurnExecution,
    ) -> Result<usize> {
        if execution.terminal_state != AgentTurnState::Running {
            return Ok(0);
        }
        let Some(batch) = execution.response.action_batch.clone() else {
            return Ok(0);
        };
        let mut executed = 0usize;
        let mut skill_context = self.runtime_skill_action_context_for_turn(&turn.turn_id)?;
        for index in 0..execution.action_results.len() {
            if execution.action_results[index].status != ActionStatus::Running
                || !matches!(
                    execution.action_results[index].action_type,
                    "request_skills" | "call_skill"
                )
            {
                continue;
            }
            let action = batch
                .actions
                .iter()
                .find(|action| action.id == execution.action_results[index].action_id)
                .cloned()
                .ok_or_else(|| {
                    MezError::invalid_state("running skill result does not match an action")
                })?;
            if !self.queue_ordered_provider_header(&turn.pane_id, execution, &action)? {
                self.append_agent_status_text_to_terminal_buffer(
                    &turn.pane_id,
                    &format!(
                        "agent: {}",
                        runtime_agent_action_summary(&action)
                            .unwrap_or_else(|| "skill action".to_string())
                    ),
                )?;
            }
            execution.action_results[index] =
                self.execute_skill_action_for_turn(turn, &action, &mut skill_context)?;
            self.flush_ordered_provider_headers(&turn.pane_id, execution)?;
            executed = executed.saturating_add(1);
        }
        execution.terminal_state = runtime_agent_turn_state_from_action_results(
            &execution.action_results,
            execution.final_turn,
        );
        Ok(executed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod shared;

    /// Creates eligible user metadata before capturing a new conversation surface.
    fn fixture(global: bool) -> (RuntimeSessionService, AgentTurnRecord, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "mez-model-skills-{}-{}",
            std::process::id(),
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        std::fs::create_dir_all(root.join("skills/review")).unwrap();
        std::fs::write(
            root.join("skills/review/SKILL.md"),
            "---\nname: review\ndescription: Review safely\ndiscovery: true\n---\nBODY_SENTINEL\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("skills/hidden")).unwrap();
        std::fs::write(root.join("skills/hidden/SKILL.md"),
            "---\nname: hidden\ndescription: HIDDEN_DESCRIPTION\ndiscovery: false\n---\nHIDDEN_BODY\n").unwrap();
        let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
        service.set_config_root(root.clone());
        configure(&mut service, global);
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        let started = service
            .start_agent_prompt_turn("%1", "inspect skills")
            .unwrap();
        let turn = service
            .agent_turn_ledger()
            .turn(&started.turn_id)
            .unwrap()
            .clone();
        (service, turn, root)
    }

    /// Replaces live operator policy without changing the captured conversation schema.
    fn configure(service: &mut RuntimeSessionService, global: bool) {
        service.replace_config_layers(vec![crate::config::ConfigLayer {
            name: "primary".into(), path: None, format: crate::config::ConfigFormat::Toml,
            scope: crate::config::ConfigScope::Primary, trusted: true,
            text: format!("[agents]\nenabled_actions = [\"say\", \"request_skills\", \"call_skill\"]\n[skills]\ndiscovery = {global}\n"),
        }]).unwrap();
    }

    /// Executes the same semantic reducer used for running provider skill results.
    fn action(
        service: &mut RuntimeSessionService,
        turn: &AgentTurnRecord,
        payload: mez_agent::AgentActionPayload,
        context: &mut SkillActionContext,
    ) -> ActionResult {
        service
            .execute_skill_action_for_turn(
                turn,
                &AgentAction {
                    id: "fixture-action".into(),
                    payload,
                },
                context,
            )
            .unwrap()
    }

    /// Discovery reveals only eligible metadata. Loading requires the current
    /// turn's receipt; duplicate loads, changed bytes and live revocation fail
    /// without suggesting excluded names or widening action authority.
    #[test]
    fn model_skill_discovery_requires_live_current_turn_selection() {
        let (mut service, turn, root) = fixture(true);
        let mut context = SkillActionContext::default();
        let call = || mez_agent::AgentActionPayload::CallSkill {
            name: "review".into(),
            additional_context: None,
        };
        assert!(action(&mut service, &turn, call(), &mut context).is_error);
        let catalog = action(
            &mut service,
            &turn,
            mez_agent::AgentActionPayload::RequestSkills,
            &mut context,
        );
        assert!(!catalog.is_error);
        let json = catalog.structured_content_json.unwrap();
        assert!(json.contains("Review safely"));
        for forbidden in ["hidden", "HIDDEN", "path", "BODY_SENTINEL", "diagnostics"] {
            assert!(!json.contains(forbidden), "{json}");
        }
        let mut other = turn.clone();
        other.turn_id = "another-turn".into();
        assert!(
            action(
                &mut service,
                &other,
                call(),
                &mut SkillActionContext::default()
            )
            .is_error
        );
        let document = action(&mut service, &turn, call(), &mut context);
        assert!(!document.is_error);
        assert!(action(&mut service, &turn, call(), &mut context).is_error);
        configure(&mut service, false);
        assert!(
            action(
                &mut service,
                &turn,
                mez_agent::AgentActionPayload::RequestSkills,
                &mut SkillActionContext::default()
            )
            .is_error
        );
        std::fs::remove_dir_all(root).unwrap();

        let (mut service, turn, root) = fixture(true);
        let mut context = SkillActionContext::default();
        assert!(
            !action(
                &mut service,
                &turn,
                mez_agent::AgentActionPayload::RequestSkills,
                &mut context
            )
            .is_error
        );
        std::fs::write(
            root.join("skills/review/SKILL.md"),
            "---\nname: review\ndescription: Review safely\ndiscovery: true\n---\nCHANGED_BODY\n",
        )
        .unwrap();
        assert!(action(&mut service, &turn, call(), &mut context).is_error);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A newly trusted shadow and subsequent trust revocation both invalidate
    /// selection. A lower-priority user document cannot replace the selected
    /// project winner even when it has the same name and eligible metadata.
    #[test]
    fn model_skill_selection_rejects_shadow_and_trust_changes() {
        let (mut service, turn, root) = fixture(true);
        let project = root.join("project");
        std::fs::create_dir_all(project.join(".mezzanine/skills/review")).unwrap();
        let project_text =
            "---\nname: review\ndescription: Project review\ndiscovery: true\n---\nPROJECT_BODY\n";
        std::fs::write(
            project.join(".mezzanine/skills/review/SKILL.md"),
            project_text,
        )
        .unwrap();
        service.set_pane_current_working_directory("%1", project.clone());
        let mut context = SkillActionContext::default();
        assert!(
            !action(
                &mut service,
                &turn,
                mez_agent::AgentActionPayload::RequestSkills,
                &mut context
            )
            .is_error
        );
        let mut trust = crate::security::project::ProjectTrustStore::default();
        trust
            .decide(
                project.clone(),
                crate::security::project::TrustDecision::Trusted,
                None,
            )
            .unwrap();
        service.set_project_trust_store(trust.clone(), None);
        let call = || mez_agent::AgentActionPayload::CallSkill {
            name: "review".into(),
            additional_context: None,
        };
        assert!(action(&mut service, &turn, call(), &mut context).is_error);
        let mut next = turn.clone();
        next.turn_id = "project-selection-turn".into();
        let mut context = SkillActionContext::default();
        assert!(
            !action(
                &mut service,
                &next,
                mez_agent::AgentActionPayload::RequestSkills,
                &mut context
            )
            .is_error
        );
        trust
            .decide(
                project,
                crate::security::project::TrustDecision::Revoked,
                None,
            )
            .unwrap();
        service.set_project_trust_store(trust, None);
        assert!(action(&mut service, &next, call(), &mut context).is_error);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Default-off capture remains immutable after policy opt-in, requiring a
    /// new conversation instead of silently adding actions to an existing one.
    #[test]
    fn model_skill_policy_opt_in_does_not_widen_captured_surface() {
        let (mut service, turn, root) = fixture(false);
        let before = service
            .capture_agent_session_allowed_actions_for_pane("%1")
            .unwrap();
        assert!(!before.contains(mez_agent::AllowedAction::RequestSkills));
        configure(&mut service, true);
        assert_eq!(
            service
                .capture_agent_session_allowed_actions_for_pane("%1")
                .unwrap(),
            before
        );
        assert!(
            action(
                &mut service,
                &turn,
                mez_agent::AgentActionPayload::RequestSkills,
                &mut SkillActionContext::default()
            )
            .is_error
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
