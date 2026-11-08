//! Runtime agent provider-context preparation and recovery helpers.
//!
//! This module owns active-turn project-guidance refresh, request-local context
//! preparation, provider context/output-limit recovery, and auto-sizing
//! dispatch selection. Steering and other chronology commit at their actor
//! receipt boundaries before this module observes a snapshot. The provider
//! execution loop remains in the facade while these helper decisions stay
//! grouped by their context-shaping responsibility. Guidance remains chronological;
//! cache lineage fingerprints its actual snapshot rather than a removed stable slot.

use super::{
    AgentId, AgentTurnRecord, AgentTurnState, ContextSourceKind, MezError, ModelProfile, Result,
    RuntimeAutoSizingDispatch, RuntimeAutoSizingTargetProfile, RuntimeSessionService,
    invoked_mcp_tools_for_context_with_configured, runtime_cooperation_mode_name,
    runtime_mezzanine_error_code,
};
#[cfg(test)]
use crate::runtime::RuntimeAutoSizingDecision;
use mez_agent::set_project_guidance_context;
use sha2::{Digest, Sha256};

/// Hashes the latest chronological instruction snapshot for cache identity.
/// A length-prefixed label and exact content distinguish alternative discoveries
/// without exposing file bodies in diagnostics or reclassifying them as system
/// policy. Missing prepared guidance is an invariant error, not an absent slot.
fn project_guidance_source_fingerprint(context: &mez_agent::AgentContext) -> Result<String> {
    let block = context
        .chronology()
        .iter()
        .rev()
        .find(|event| event.block().source == ContextSourceKind::ProjectGuidance)
        .map(|event| event.block())
        .ok_or_else(|| MezError::invalid_state("prepared project guidance snapshot is missing"))?;
    let mut digest = Sha256::new();
    digest.update(b"project-guidance\0");
    digest.update(format!("{}\0", block.label.len()).as_bytes());
    digest.update(block.label.as_bytes());
    digest.update(block.content.as_bytes());
    Ok(digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Renders the bounded safe state that follows an output-token cutoff.
///
/// Native argument fragments are deliberately absent. A cutoff containing an
/// unfinished native call requires re-emission of one complete atomic call.
fn output_limit_continuation_input(state: &mez_agent::ProviderOutputLimitState) -> String {
    let instruction = match state.continuation_disposition {
        mez_agent::ProviderOutputLimitContinuationDisposition::ContinueVisibleText => {
            "Treat the safe assistant text below as already emitted. Continue immediately after it; do not repeat or replace it."
        }
        mez_agent::ProviderOutputLimitContinuationDisposition::ReemitAtomicNativeCall => {
            "No truncated action ran. Re-emit exactly one complete atomic action call; do not continue or rely on truncated arguments."
        }
    };
    format!(
        "[output-limit continuation]
provider={}
api={}
stop_reason={}
response_id={}
complete_native_items={}
incomplete_native_items={}
{}
[safe partial assistant text]
{}",
        state.provider,
        state.api,
        state.stop_reason,
        state.response_id.as_deref().unwrap_or("unknown"),
        state.complete_native_items,
        state.incomplete_native_items,
        instruction,
        state.safe_partial_text,
    )
}

impl RuntimeSessionService {
    /// Appends a changed configured MCP catalog to immutable turn chronology.
    ///
    /// Provider preparation may run repeatedly while one turn is active. The
    /// exact configured catalog is appended only when its canonical projection
    /// changes; explicit-only server metadata remains request-local.
    pub(crate) fn refresh_agent_turn_mcp_catalog_context(
        &mut self,
        turn: &AgentTurnRecord,
    ) -> Result<()> {
        let configured_server_names = self.integration.always_exposed_mcp_servers().to_vec();
        let current = mez_agent::configured_mcp_catalog_snapshot_content(
            &self.mcp_registry().prompt_summary(),
            &configured_server_names,
        );
        let context = self
            .agent_turn_contexts_mut()
            .get_mut(&turn.turn_id)
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        let previous = context
            .chronology()
            .iter()
            .rev()
            .find(|event| event.block().source == ContextSourceKind::McpCatalogSnapshot)
            .map(|event| event.block().content.as_str());
        let current = current
            .or_else(|| previous.map(|_| mez_agent::MCP_CATALOG_REMOVED_CONTEXT.to_string()));
        if let Some(content) = current.filter(|content| previous != Some(content.as_str())) {
            context
                .append_reference_event(
                    ContextSourceKind::McpCatalogSnapshot,
                    mez_agent::MCP_CATALOG_SNAPSHOT_CONTEXT_LABEL,
                    content,
                )
                .map_err(|error| MezError::invalid_state(error.to_string()))?;
        }
        Ok(())
    }

    /// Prepares the next provider request from durable stable and chronological
    /// context without adding a request-local model-visible suffix.
    pub(crate) fn prepare_agent_turn_model_context(
        &self,
        turn: &AgentTurnRecord,
        durable: super::AgentContext,
        mcp_summary: &mez_agent::McpPromptSummary,
        _model_profile: &ModelProfile,
    ) -> Result<(super::PreparedModelContext, Vec<mez_agent::McpPromptTool>)> {
        durable.validate_durable()?;
        let available_mcp_tools = invoked_mcp_tools_for_context_with_configured(
            &durable,
            mcp_summary,
            self.integration.always_exposed_mcp_servers(),
        );
        Ok((
            super::PreparedModelContext::new(durable)?.with_previous_request(
                self.agent
                    .agent_turn_provider_request_chains
                    .get(&turn.turn_id)
                    .cloned()
                    .or_else(|| {
                        self.agent_turn_executions()
                            .get(&turn.turn_id)
                            .map(|execution| execution.request.clone())
                    })
                    .or_else(|| {
                        self.agent
                            .agent_conversation_provider_request_chains
                            .get(&turn.conversation_id)
                            .cloned()
                    }),
            ),
            available_mcp_tools,
        ))
    }

    /// Refreshes project guidance blocks on a stored turn context.
    ///
    /// Before the first accepted provider response, discovery may append a
    /// changed snapshot and establish a new cache lineage. Once a provider request
    /// has been accepted, the turn's front-loaded instructions are frozen;
    /// changed guidance is deferred to the next turn rather than rewriting the
    /// active OpenAI request chain.
    pub(crate) fn refresh_agent_turn_project_guidance_context(
        &mut self,
        turn: &AgentTurnRecord,
    ) -> Result<()> {
        let Some(instruction_files) = self
            .pane_agent_instruction_files(&turn.pane_id)
            .map(<[_]>::to_vec)
        else {
            return Ok(());
        };
        let previous = self
            .agent_turn_contexts()
            .get(&turn.turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        let mut context = set_project_guidance_context(
            previous.clone(),
            &instruction_files,
            self.agent_instruction_context_max_bytes(),
        )?;
        if context == previous {
            return Ok(());
        }

        if self
            .agent
            .agent_turn_provider_request_chains
            .contains_key(&turn.turn_id)
            || self.agent_turn_executions().contains_key(&turn.turn_id)
        {
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                "project_guidance refresh_deferred reason=active_provider_cache_epoch",
            )?;
            return Ok(());
        }

        let prior_lineage = previous
            .metadata()
            .prompt_cache_lineage_id
            .as_deref()
            .unwrap_or("none");
        let current_fingerprint_text = project_guidance_source_fingerprint(&context)?;
        let lineage_material =
            format!("{prior_lineage}\0project-guidance\0{current_fingerprint_text}");
        let lineage_digest = Sha256::digest(lineage_material.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let mut metadata = context.metadata().clone();
        metadata.prompt_cache_lineage_id = Some(format!("guidance-{lineage_digest}"));
        context.set_metadata(metadata);
        context.validate_durable()?;
        self.agent_turn_contexts_mut()
            .insert(turn.turn_id.clone(), context);
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "context_lineage changed reason=project_guidance source_fingerprint={current_fingerprint_text}"
            ),
        )?;
        Ok(())
    }

    /// Queues model-backed active-turn compaction after a provider rejects the request
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
        let recovery_attempt = attempt.max(1);
        if !self.queue_agent_context_limit_recovery_compaction(
            turn_id,
            turn.model_profile.clone(),
            model_profile.clone(),
            recovery_attempt,
        )? {
            return Ok(false);
        }
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            turn_id,
            &format!(
                "context_limit_recovery queued trigger=provider_context_limit provider={} model={} context_window_tokens={} max_input_tokens={} attempt={} retained_tail_percent={} error_kind={}",
                model_profile.provider,
                model_profile.model,
                model_profile.context_window_tokens().unwrap_or(0),
                model_profile.max_input_tokens().unwrap_or(0),
                recovery_attempt,
                self.agent_compaction_raw_retention_percent(),
                runtime_mezzanine_error_code(error.kind())
            ),
        )?;
        Ok(true)
    }

    /// Queues one compaction at a safe continuation boundary after an ordinary
    /// execution response reports input at or above its configured threshold.
    ///
    /// The reported sample alone selects this trigger. Common canonical source
    /// admission owns eligibility and retention; no ordinary request estimate
    /// selects another history algorithm or shortens its retention policy.
    pub(crate) fn defer_agent_provider_for_observed_input_limit(
        &mut self,
        turn: &AgentTurnRecord,
        model_profile: &ModelProfile,
        observed_usage: mez_agent::ModelTokenUsage,
        _accepted_request: &mez_agent::ModelRequest,
    ) -> Result<bool> {
        let Some(max_input_tokens) = model_profile.max_input_tokens() else {
            return Ok(false);
        };
        if observed_usage.input_tokens < max_input_tokens as u64
            || !self
                .agent
                .agent_turn_observed_input_compaction_turns
                .insert(turn.turn_id.clone())
        {
            return Ok(false);
        }
        if !self.queue_agent_active_turn_compaction(
            &turn.turn_id,
            turn.model_profile.clone(),
            model_profile.clone(),
            crate::runtime::agent_state::RuntimeActiveTurnCompactionTrigger::ObservedInputLimit {
                observed_input_tokens: observed_usage.input_tokens,
                max_input_tokens,
            },
        )? {
            self.agent
                .agent_turn_observed_input_compaction_turns
                .remove(&turn.turn_id);
            return Ok(false);
        }
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "observed_input_limit queued trigger=observed_input_limit provider={} model={} observed_input_tokens={} max_input_tokens={} context_window_tokens={} displayed_usage_sample=prior_successful_request",
                model_profile.provider,
                model_profile.model,
                observed_usage.input_tokens,
                max_input_tokens,
                model_profile.context_window_tokens().unwrap_or(0),
            ),
        )?;
        Ok(true)
    }

    /// Records compact-output retry controller state after a provider cuts
    /// generation off at its output-token limit.
    ///
    /// This recovery path deliberately does not compact context: the provider
    /// accepted the input, but the model spent too much output budget. The
    /// durable chronology remains unchanged. Provider preparation exposes a
    /// request-local mode flag whose behavior is defined by stable format
    /// policy, then escalates the `max_output_tokens` provider option on a
    /// later retry.
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
        let Some(output_limit_state) = error.provider_output_limit_state().cloned() else {
            return Ok(false);
        };
        if !self.agent.agent_turn_model_profiles.contains_key(turn_id) {
            self.agent.pending_agent_provider_tasks.remove(turn_id);
            return Err(MezError::invalid_state(
                "runtime agent turn has no model profile",
            ));
        }

        if !self.agent_turn_contexts().contains_key(turn_id) {
            return Err(MezError::invalid_state(
                "runtime agent turn context is unavailable",
            ));
        }
        self.agent
            .agent_turn_output_limit_recovery_attempts
            .insert(turn_id.to_string(), attempt.max(1));
        if attempt <= 1 {
            let context = self
                .agent_turn_contexts_mut()
                .get_mut(turn_id)
                .ok_or_else(|| {
                    MezError::invalid_state("runtime agent turn context is unavailable")
                })?;
            context
                .append_reference_event(
                    ContextSourceKind::RuntimeHint,
                    "output-limit continuation",
                    output_limit_continuation_input(&output_limit_state),
                )
                .map_err(|error| MezError::invalid_state(error.to_string()))?;
        }
        self.agent.agent_turn_interaction_kinds.insert(
            turn_id.to_string(),
            mez_agent::ModelInteractionKind::OutputLimitRetry,
        );
        let status_text = if attempt <= 1 {
            "agent: provider response hit output limit; continuing from safe partial output"
                .to_string()
        } else {
            "agent: provider response hit output limit again; starting one fresh compact recovery request".to_string()
        };
        self.append_agent_status_text_to_terminal_buffer(&turn.pane_id, &status_text)?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            turn_id,
            &format!(
                "provider_request recovery_applied reason=provider_output_limit stage={} partial_state={} error_kind={}",
                attempt.max(1),
                if attempt <= 1 { "continuation" } else { "fresh_compact" },
                runtime_mezzanine_error_code(error.kind())
            ),
        )?;
        Ok(true)
    }

    /// Returns the pane-local routing preference, falling back to
    /// the configured default when the pane has no explicit override.
    pub(crate) fn agent_routing_enabled_for_pane(&self, pane_id: &str) -> bool {
        self.agent_routing_override(pane_id)
            .or_else(|| {
                self.agent_selected_personality_profile(pane_id)
                    .and_then(|profile| profile.routing_enabled)
            })
            .unwrap_or(self.agent_default_routing())
    }

    /// Builds an automatic sizing dispatch for the first provider request of a
    /// turn.
    pub(crate) fn runtime_auto_sizing_dispatch_for_turn(
        &self,
        turn: &AgentTurnRecord,
        default_profile: &ModelProfile,
    ) -> Result<Option<RuntimeAutoSizingDispatch>> {
        if !self.agent_routing_enabled_for_pane(&turn.pane_id)
            || self.agent_turn_routing_applied(&turn.turn_id)
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
            supported_reasoning_efforts: mez_agent::auto_sizing_reasoning_levels_for_profile(
                provider_config,
                &profile,
            ),
            profile,
        })
    }

    /// Resolves one explicit child-turn selection through the pane's effective
    /// auto-sizing policy without issuing a router request.
    #[cfg(test)]
    pub(crate) fn runtime_explicit_auto_sizing_selection_for_pane(
        &self,
        pane_id: &str,
        size: &str,
        reasoning_effort: &str,
    ) -> Result<mez_agent::AutoSizingSelection> {
        let config = self.runtime_auto_sizing_config_for_pane(pane_id).clone();
        let profile_name = match size {
            "small" => &config.small_model_profile,
            "medium" => &config.medium_model_profile,
            "large" => &config.large_model_profile,
            _ => return Err(MezError::invalid_args("unsupported subagent model size")),
        };
        let target = self.runtime_auto_sizing_target_profile(size, profile_name)?;
        mez_agent::auto_sizing_selection_for_explicit_pair(
            &config.allowed_reasoning_efforts,
            &target,
            reasoning_effort,
        )
        .map_err(|error| MezError::invalid_args(error.message()))
    }

    /// Resolves an explicit spawn selection from the frozen provider-visible
    /// catalog that advertised it rather than current routing configuration.
    pub(crate) fn runtime_explicit_auto_sizing_selection_from_catalog(
        &self,
        allowed_actions: &mez_agent::AllowedActionSet,
        size: &str,
        reasoning_effort: &str,
    ) -> Result<mez_agent::AutoSizingSelection> {
        let sizing = allowed_actions.spawn_agent_sizing().ok_or_else(|| {
            MezError::invalid_args("spawn agent sizing is unavailable in the frozen action catalog")
        })?;
        let option = sizing
            .sizes
            .iter()
            .find(|option| option.size == size)
            .ok_or_else(|| {
                MezError::invalid_args(
                    "unsupported subagent model size in the frozen action catalog",
                )
            })?;
        if !option
            .allowed_reasoning_efforts
            .iter()
            .any(|effort| effort == reasoning_effort)
        {
            return Err(MezError::invalid_args(
                "subagent reasoning effort is not allowed for the frozen model size",
            ));
        }
        let profile = option.execution_profile.clone().ok_or_else(|| {
            MezError::invalid_state(
                "frozen spawn sizing entry lacks the execution profile required for explicit selection",
            )
        })?;
        let target = mez_agent::AutoSizingTargetProfile {
            size: option.size.clone(),
            profile_name: option.profile_name.clone(),
            profile,
            supported_reasoning_efforts: Vec::new(),
        };
        mez_agent::auto_sizing_selection_for_explicit_pair(
            &option.allowed_reasoning_efforts,
            &target,
            reasoning_effort,
        )
        .map_err(|error| MezError::invalid_args(error.message()))
    }

    /// Builds the routed-size reasoning contract for `spawn_agent` schemas.
    ///
    /// Each size reports its configured profile and exactly the reasoning
    /// efforts an explicit pair may use, so the provider-facing schema
    /// advertises only selections this pane's effective policy resolves. The
    /// manifest is descriptive: acceptance and rejection remain owned by
    /// `runtime_explicit_auto_sizing_selection_for_pane`.
    pub(crate) fn runtime_spawn_agent_sizing_for_pane(
        &self,
        pane_id: &str,
    ) -> Result<mez_agent::SpawnAgentSizing> {
        let config = self.runtime_auto_sizing_config_for_pane(pane_id).clone();
        let mut sizes = Vec::with_capacity(3);
        for (size, profile_name) in [
            ("small", &config.small_model_profile),
            ("medium", &config.medium_model_profile),
            ("large", &config.large_model_profile),
        ] {
            let target = self.runtime_auto_sizing_target_profile(size, profile_name)?;
            sizes.push(mez_agent::SpawnAgentSizeOption {
                size: size.to_string(),
                profile_name: profile_name.clone(),
                execution_profile: Some(target.profile.clone()),
                allowed_reasoning_efforts:
                    mez_agent::auto_sizing_allowed_reasoning_efforts_for_target(
                        &config.allowed_reasoning_efforts,
                        &target,
                    ),
            });
        }
        Ok(mez_agent::SpawnAgentSizing { sizes })
    }

    /// Logs a bounded auto-sizing decision without placing router
    /// correspondence into model context or transcript content.
    #[cfg(test)]
    pub(crate) fn record_auto_sizing_outcome(
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

#[cfg(test)]
mod tests {
    use super::output_limit_continuation_input;

    /// Verifies a credential-shaped provider stop reason never reaches the
    /// output-limit continuation prompt.
    ///
    /// The continuation prompt is provider-visible request text, so a stop
    /// reason carrying a bearer-token or API-key shape must be replaced at the
    /// shared diagnostics boundary before this injection point, while the
    /// remaining structured continuation fields stay available.
    #[test]
    fn output_limit_continuation_prompt_excludes_credential_shaped_stop_reason() {
        const SENTINEL: &str = "sk-ant-api03-CONTINUATIONSENTINEL000";
        let state = mez_agent::ProviderOutputLimitState::new(
            "anthropic",
            "messages",
            format!("stop_sequence Bearer {SENTINEL}"),
            Some("resp_safe".to_string()),
            "safe partial text",
            1,
            0,
            mez_agent::ModelTokenUsage::default(),
            mez_agent::ProviderOutputLimitContinuationDisposition::ContinueVisibleText,
        );

        let prompt = output_limit_continuation_input(&state);

        assert!(!prompt.contains(SENTINEL), "{prompt}");
        assert!(prompt.contains("stop_reason=[REDACTED]"), "{prompt}");
        assert!(prompt.contains("response_id=resp_safe"), "{prompt}");
        assert!(prompt.contains("safe partial text"), "{prompt}");
        assert!(!format!("{state:?}").contains(SENTINEL));
    }
}
