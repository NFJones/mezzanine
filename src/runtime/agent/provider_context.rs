//! Runtime agent provider-context preparation and recovery helpers.
//!
//! This module owns active-turn project-guidance refresh, mid-turn steering
//! insertion, provider context/output-limit recovery, and auto-sizing dispatch
//! selection. The provider execution loop remains in the facade while these
//! helper decisions stay grouped by their context-shaping responsibility.

use super::{
    AgentId, AgentTurnExecution, AgentTurnRecord, AgentTurnState, ContextBlock, ContextSourceKind,
    MezError, ModelProfile, RUNTIME_PROVIDER_OUTPUT_LIMIT_RETRY_LABEL, Result,
    RuntimeAutoSizingDispatch, RuntimeAutoSizingTargetProfile, RuntimeSessionService,
    compact_model_context_for_budget_with_retained_tail_percent,
    runtime_auto_sizing_reasoning_levels_for_profile, runtime_cooperation_mode_name,
    runtime_mezzanine_error_code, set_project_guidance_context,
};
#[cfg(test)]
use crate::runtime::RuntimeAutoSizingDecision;

impl RuntimeSessionService {
    /// Refreshes project guidance blocks on a stored turn context.
    ///
    /// Provider continuations can happen after file mutations and shell output
    /// observations. This keeps discovered repository instruction content in
    /// every provider-bound context without duplicating stale guidance blocks.
    pub(in crate::runtime) fn refresh_agent_turn_project_guidance_context(
        &mut self,
        turn: &AgentTurnRecord,
    ) -> Result<()> {
        let Some(instruction_files) = self
            .pane_agent_instruction_files(&turn.pane_id)
            .map(<[_]>::to_vec)
            .filter(|files| !files.is_empty())
        else {
            return Ok(());
        };
        let context = self
            .agent_turn_contexts()
            .get(&turn.turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        let context = set_project_guidance_context(context, &instruction_files, 2)?;
        self.agent_turn_contexts_mut()
            .insert(turn.turn_id.clone(), context);
        Ok(())
    }

    /// Drains user steering prompts into the next provider-bound context.
    ///
    /// Mid-turn input is intentionally not treated as a new conversation turn.
    /// The block template tells the model that the text was submitted while
    /// the current turn was already active and should be incorporated from the
    /// next action boundary forward.
    pub(in crate::runtime) fn drain_pending_agent_turn_steering_context(
        &mut self,
        turn: &AgentTurnRecord,
    ) -> Result<usize> {
        let Some(steering) = self.take_agent_turn_steering(&turn.turn_id) else {
            return Ok(0);
        };
        let count = steering.len();
        let context = self
            .agent_turn_contexts_mut()
            .get_mut(&turn.turn_id)
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        for (index, steering) in steering.into_iter().enumerate() {
            context.blocks.push(ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: format!(
                    "user steering input {} for active turn {}",
                    index + 1,
                    turn.turn_id
                ),
                content: mez_agent::agent_turn_steering_context_content(&steering),
            });
        }
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!("user_steering applied count={count} reason=provider_context_preparation"),
        )?;
        Ok(count)
    }

    /// Locally compacts active-turn context after a provider rejects the request
    /// as too large.
    ///
    /// Once the provider has rejected the exact request, the recoverable
    /// continuation is to reduce model-visible active-turn context and retry
    /// with the same durable turn.
    pub(crate) fn recover_agent_provider_context_limit_failure(
        &mut self,
        agent_id: &AgentId,
        turn_id: &str,
        error: &MezError,
        attempt: u32,
    ) -> Result<bool> {
        let Some(turn) = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
        else {
            self.agent.pending_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        };
        if turn.agent_id != agent_id.as_str() {
            return Err(MezError::invalid_args(
                "agent provider recovery agent id does not match turn",
            ));
        }
        if turn.state != AgentTurnState::Running {
            self.agent.pending_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        }
        let Some(stored_model_profile) = self.agent.agent_turn_model_profiles.get(turn_id).cloned()
        else {
            self.agent.pending_agent_provider_tasks.remove(turn_id);
            return Err(MezError::invalid_state(
                "runtime agent turn has no model profile",
            ));
        };
        let auto_sizing =
            self.runtime_auto_sizing_dispatch_for_turn(&turn, &stored_model_profile)?;
        let model_profile = mez_agent::auto_sizing_minimum_context_profile(
            &stored_model_profile,
            auto_sizing.as_ref(),
        );
        let context = self
            .agent_turn_contexts()
            .get(turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        let profile_budget_words = model_profile.context_window_budget_words();
        let recovery_attempt = attempt.max(1);
        let recovery_budget_words = match recovery_attempt {
            1 => profile_budget_words,
            2 => profile_budget_words.saturating_mul(3).saturating_div(4),
            3 => profile_budget_words.saturating_div(2),
            _ => profile_budget_words.saturating_div(4),
        }
        .max(1);
        let retained_tail_percent = self.agent_compaction_raw_retention_percent();
        let (compacted_context, report) =
            compact_model_context_for_budget_with_retained_tail_percent(
                context,
                recovery_budget_words,
                retained_tail_percent,
            )?;
        if !report.changed() {
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                turn_id,
                &format!(
                    "context_limit_recovery skipped attempt={} profile_budget_words={} recovery_budget_words={} retained_tail_percent={} error_kind={} no_compactable_blocks=true",
                    recovery_attempt,
                    profile_budget_words,
                    recovery_budget_words,
                    retained_tail_percent,
                    runtime_mezzanine_error_code(error.kind())
                ),
            )?;
            self.append_agent_status_text_to_terminal_buffer(
                &turn.pane_id,
                &format!(
                    "agent: provider rejected context as too large; no compactable active turn context remains profile_budget_words={} recovery_budget_words={}",
                    profile_budget_words,
                    recovery_budget_words
                ),
            )?;
            return Ok(false);
        }
        self.agent_turn_contexts_mut()
            .insert(turn_id.to_string(), compacted_context);
        self.append_agent_status_text_to_terminal_buffer(
            &turn.pane_id,
            &format!(
                "agent: provider rejected context as too large; compacted active turn context profile_budget_words={} recovery_budget_words={} retained_tail_percent={} compacted_blocks={} omitted_blocks={}",
                profile_budget_words,
                recovery_budget_words,
                retained_tail_percent,
                report.compacted_blocks,
                report.omitted_blocks
            ),
        )?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            turn_id,
            &format!(
                "context_limit_recovery applied attempt={} profile_budget_words={} recovery_budget_words={} retained_tail_percent={} compacted_blocks={} omitted_blocks={} omitted_original_words={} error_kind={}",
                recovery_attempt,
                profile_budget_words,
                recovery_budget_words,
                retained_tail_percent,
                report.compacted_blocks,
                report.omitted_blocks,
                report.omitted_original_words,
                runtime_mezzanine_error_code(error.kind())
            ),
        )?;
        Ok(true)
    }

    /// Adds compact-output retry guidance after a provider cuts generation off
    /// at its output-token limit.
    ///
    /// This recovery path deliberately does not compact context: the provider
    /// accepted the input, but the model spent too much output budget. The
    /// durable active turn first retries with compact-output guidance only,
    /// then escalates the `max_output_tokens` provider option on a later retry.
    pub(crate) fn recover_agent_provider_output_limit_failure(
        &mut self,
        agent_id: &AgentId,
        turn_id: &str,
        error: &MezError,
        attempt: u32,
    ) -> Result<bool> {
        let Some(turn) = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
        else {
            self.agent.pending_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        };
        if turn.agent_id != agent_id.as_str() {
            return Err(MezError::invalid_args(
                "agent provider recovery agent id does not match turn",
            ));
        }
        if turn.state != AgentTurnState::Running {
            self.agent.pending_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        }
        let Some(mut model_profile) = self.agent.agent_turn_model_profiles.get(turn_id).cloned()
        else {
            self.agent.pending_agent_provider_tasks.remove(turn_id);
            return Err(MezError::invalid_state(
                "runtime agent turn has no model profile",
            ));
        };
        let retry_tokens = (attempt > 1).then(|| model_profile.output_limit_retry_tokens());
        if let Some(retry_tokens) = retry_tokens {
            model_profile
                .provider_options
                .insert("max_output_tokens".to_string(), retry_tokens.to_string());
            self.agent
                .agent_turn_model_profiles
                .insert(turn_id.to_string(), model_profile);
        }

        let context = self
            .agent_turn_contexts_mut()
            .get_mut(turn_id)
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        context.blocks.retain(|block| {
            block.source != ContextSourceKind::Configuration
                || block.label != RUNTIME_PROVIDER_OUTPUT_LIMIT_RETRY_LABEL
        });
        let guidance = if let Some(retry_tokens) = retry_tokens {
            format!(
                "[ephemeral provider output-limit retry]\n\
                 The previous provider response was still incomplete because generation hit max_output_tokens. \
                 Return one complete compact MAAP batch or one short final say. \
                 Do not include progress prose, plans, evidence lists, command logs, or explanations. \
                 Prefer the next focused executable action when work remains. \
                 This retry instruction is not durable transcript or future-turn context.\n\
                 attempt={} max_output_tokens={} error_kind={} error_message={}",
                attempt.max(1),
                retry_tokens,
                runtime_mezzanine_error_code(error.kind()),
                error.message()
            )
        } else {
            format!(
                "[ephemeral provider output-limit retry]\n\
                 The previous provider response was incomplete because generation hit max_output_tokens. \
                 Retry once with a much shorter complete response before Mezzanine escalates recovery. \
                 Return one minimal complete MAAP batch or one short final say. \
                 Do not include progress prose, plans, evidence lists, command logs, or explanations. \
                 Prefer the next focused executable action when work remains. \
                 This retry instruction is not durable transcript or future-turn context.\n\
                 attempt={} error_kind={} error_message={}",
                attempt.max(1),
                runtime_mezzanine_error_code(error.kind()),
                error.message()
            )
        };
        context.blocks.push(ContextBlock {
            source: ContextSourceKind::Configuration,
            label: RUNTIME_PROVIDER_OUTPUT_LIMIT_RETRY_LABEL.to_string(),
            content: guidance,
        });
        let status_text = if let Some(retry_tokens) = retry_tokens {
            format!(
                "agent: provider response hit output limit again; retrying compactly attempt={} max_output_tokens={}",
                attempt.max(1),
                retry_tokens
            )
        } else {
            format!(
                "agent: provider response hit output limit; retrying with shorter-response guidance attempt={}",
                attempt.max(1)
            )
        };
        self.append_agent_status_text_to_terminal_buffer(&turn.pane_id, &status_text)?;
        let trace_retry_tokens = retry_tokens
            .map(|tokens| tokens.to_string())
            .unwrap_or_else(|| "unchanged".to_string());
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            turn_id,
            &format!(
                "provider_request recovery_applied reason=provider_output_limit attempt={} max_output_tokens={} error_kind={}",
                attempt.max(1),
                trace_retry_tokens,
                runtime_mezzanine_error_code(error.kind())
            ),
        )?;
        Ok(true)
    }

    /// Keeps an active turn alive when user steering arrived mid-request.
    ///
    /// If the model completed while a newer user prompt was waiting to be
    /// incorporated, finishing the turn would silently discard the user's
    /// steering. Instead, the runtime converts that completion into one more
    /// provider continuation so the pending input can be drained into context.
    pub(in crate::runtime) fn continue_completed_turn_for_pending_steering(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &mut AgentTurnExecution,
    ) -> Result<bool> {
        if execution.terminal_state != AgentTurnState::Completed
            || !self.agent_turn_has_pending_steering(&turn.turn_id)
        {
            return Ok(false);
        }
        execution.terminal_state = AgentTurnState::Running;
        execution.final_turn = false;
        self.append_agent_status_text_to_terminal_buffer(
            &turn.pane_id,
            "agent: steering input arrived during provider work; continuing current turn",
        )?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            "user_steering forced_continuation reason=provider_completed_before_context_applied",
        )?;
        Ok(true)
    }

    /// Returns the pane-local routing preference, falling back to
    /// the configured default when the pane has no explicit override.
    pub(in crate::runtime) fn agent_routing_enabled_for_pane(&self, pane_id: &str) -> bool {
        self.agent_routing_override(pane_id)
            .or_else(|| {
                self.agent_selected_personality_profile(pane_id)
                    .and_then(|profile| profile.routing_enabled)
            })
            .unwrap_or(self.agent_default_routing())
    }

    /// Builds an automatic sizing dispatch for the first provider request of a
    /// turn.
    pub(in crate::runtime) fn runtime_auto_sizing_dispatch_for_turn(
        &self,
        turn: &AgentTurnRecord,
        default_profile: &ModelProfile,
    ) -> Result<Option<RuntimeAutoSizingDispatch>> {
        if !self.agent_routing_enabled_for_pane(&turn.pane_id)
            || self.agent_turn_executions().contains_key(&turn.turn_id)
        {
            return Ok(None);
        }
        let config = self.runtime_auto_sizing_config_for_pane(&turn.pane_id);
        let router_profile = self
            .provider_registry()
            .resolve_profile(&config.router_model_profile)?;
        let small =
            self.runtime_auto_sizing_target_profile("small", &config.small_model_profile)?;
        let medium =
            self.runtime_auto_sizing_target_profile("medium", &config.medium_model_profile)?;
        let large =
            self.runtime_auto_sizing_target_profile("large", &config.large_model_profile)?;
        Ok(Some(RuntimeAutoSizingDispatch {
            router_profile_name: config.router_model_profile.clone(),
            router_profile,
            default_profile_name: turn.model_profile.clone(),
            default_profile: default_profile.clone(),
            small,
            medium,
            large,
            turn_metadata: self.runtime_auto_sizing_turn_metadata(turn),
            allowed_reasoning_efforts: config.allowed_reasoning_efforts.clone(),
            fallback_policy: config.fallback_policy,
        }))
    }

    /// Builds bounded non-conversation metadata for the internal router.
    fn runtime_auto_sizing_turn_metadata(&self, turn: &AgentTurnRecord) -> Option<String> {
        let mut lines = Vec::new();
        if let Some(parent_turn_id) = turn.parent_turn_id.as_deref() {
            lines.push(format!("parent_turn_id={parent_turn_id}"));
        }
        if let Some(lineage) = self.subagent_lineage(&turn.agent_id) {
            lines.push(format!("parent_agent_id={}", lineage.parent_agent_id));
            lines.push(format!("root_agent_id={}", lineage.root_agent_id));
            lines.push(format!("subagent_display_name={}", lineage.display_name));
        }
        if let Some(scope) = self.subagent_scope_declaration_for_turn(turn) {
            lines.push(format!(
                "subagent_cooperation_mode={}",
                runtime_cooperation_mode_name(scope.cooperation_mode)
            ));
            lines.push(format!(
                "subagent_current_directory={}",
                scope.current_directory
            ));
            lines.push(format!(
                "subagent_read_scopes={}",
                scope.read_scopes.join(",")
            ));
            lines.push(format!(
                "subagent_write_scopes={}",
                scope.write_scopes.join(",")
            ));
        }
        if lines.is_empty() {
            None
        } else {
            Some(lines.join("\n"))
        }
    }

    /// Resolves one configured auto-sizing target profile.
    fn runtime_auto_sizing_target_profile(
        &self,
        size: &str,
        profile_name: &str,
    ) -> Result<RuntimeAutoSizingTargetProfile> {
        let profile = self.provider_registry().resolve_profile(profile_name)?;
        let provider_config = self.provider_registry().provider(&profile.provider);
        Ok(RuntimeAutoSizingTargetProfile {
            size: size.to_string(),
            profile_name: profile_name.to_string(),
            supported_reasoning_efforts: runtime_auto_sizing_reasoning_levels_for_profile(
                provider_config,
                &profile,
            ),
            profile,
        })
    }

    /// Logs a bounded auto-sizing decision without placing router
    /// correspondence into model context or transcript content.
    #[cfg(test)]
    pub(in crate::runtime) fn record_auto_sizing_outcome(
        &mut self,
        turn: &AgentTurnRecord,
        profile: &ModelProfile,
        decision: Option<&RuntimeAutoSizingDecision>,
        fallback: Option<&str>,
    ) -> Result<()> {
        if let Some(decision) = decision {
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                &format!(
                    "auto_sizing selected size={} model={} reasoning={} confidence={:.2}",
                    decision.size, profile.model, decision.reasoning_effort, decision.confidence
                ),
            )?;
            self.append_agent_status_text_to_terminal_buffer(
                &turn.pane_id,
                &format!(
                    "agent: routing selected {} reasoning on {}",
                    decision.reasoning_effort, profile.model
                ),
            )?;
        } else if let Some(fallback) = fallback {
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                &format!(
                    "auto_sizing fallback model={} reasoning={} error={}",
                    profile.model,
                    profile.reasoning_profile.as_deref().unwrap_or("none"),
                    fallback
                ),
            )?;
            self.append_agent_status_text_to_terminal_buffer(
                &turn.pane_id,
                &format!("agent: routing fallback to {}: {}", profile.model, fallback),
            )?;
        }
        Ok(())
    }
}
