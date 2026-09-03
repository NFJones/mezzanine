//! Runtime constants, pane-title state, metrics snapshots, and patch records.

use super::{
    AgentTurnState, ModelResponse, ModelTokenUsage, ModelTokenUsageKey, SubagentWaitPolicy,
};
use mez_mux::layout::PaneTitleSource;
use std::collections::BTreeMap;

/// Maximum cache-routing scopes and conversation statuses retained in metrics.
const PROVIDER_WIRE_DIAGNOSTIC_SCOPE_LIMIT: usize = 4096;

/// Cache namespace used to compare only provider requests that can actually
/// share one backend prefix cache.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ProviderWireContinuityKey {
    conversation_id: String,
    cache_namespace: String,
    provider: String,
    model: String,
    prompt_cache_lineage_id: Option<String>,
}

/// Latest concrete execution request and matching cache diagnostics for one
/// conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeProviderWireRequestStatus {
    pub(crate) request_id: String,
    pub(crate) provider: String,
    pub(crate) model: String,
    pub(crate) interaction_kind: String,
    pub(crate) purpose: String,
    pub(crate) usage: Option<ModelTokenUsage>,
    pub(crate) stable_input_bytes: Option<usize>,
    pub(crate) volatile_input_bytes: Option<usize>,
    pub(crate) mcp_live_state_bytes: usize,
    pub(crate) mcp_catalog_bytes: usize,
    pub(crate) action_result_bytes: usize,
    pub(crate) continuity: Option<mez_agent::OpenAiRequestContinuity>,
}

/// Prior pane title state for a title emitted by a foreground program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProgramOwnedPaneTitle {
    /// Foreground process group that owned the program title.
    pub(crate) foreground_process_group_id: u32,
    /// Title to restore when the foreground program exits or changes.
    pub(crate) previous_title: String,
    /// Title provenance to restore when the foreground program exits or changes.
    pub(crate) previous_source: PaneTitleSource,
}

/// Defines the DEFAULT PTY READ LIMIT BYTES const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const DEFAULT_PTY_READ_LIMIT_BYTES: usize = 64 * 1024;
/// Default number of subagent panes that may share one subagent window.
pub const DEFAULT_MAX_SUBAGENT_PANES_PER_WINDOW: usize = 4;
/// Default number of direct subagents a root pane agent may spawn.
pub const DEFAULT_MAX_ROOT_SUBAGENTS: usize = 4;
/// Default number of direct subagents a child subagent may spawn.
pub const DEFAULT_MAX_SUBAGENTS_PER_SUBAGENT: usize = 2;
/// Default maximum delegation depth for nested subagents.
pub const DEFAULT_MAX_SUBAGENT_DEPTH: usize = 2;
/// Default policy for parent turns after spawning child subagents.
pub const DEFAULT_SUBAGENT_WAIT_POLICY: SubagentWaitPolicy = SubagentWaitPolicy::Join;
/// Default percent of the active model context retained as uncompacted raw tail.
pub const DEFAULT_AGENT_COMPACTION_RAW_RETENTION_PERCENT: usize = 10;
/// Whether agent turns use automatic model and reasoning sizing by default.
pub const DEFAULT_AGENT_ROUTING: bool = false;
/// Default bounded retry budget for model-correctable action failures.
pub const DEFAULT_AGENT_ACTION_FAILURE_RETRY_LIMIT: usize = 5;
/// Default maximum number of work iterations a `/loop` command may run.
pub const DEFAULT_AGENT_LOOP_LIMIT: usize = 8;
/// Runtime-owned diagnostics for provider, prompt-cache, turn, and shell work.
///
/// The async runtime actor records serialized actor activity separately. This
/// snapshot covers the higher-level runtime service path so inspection commands
/// can debug agent/provider behavior without parsing trace logs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RuntimeMetricsSnapshot {
    /// Number of agent turns started by the runtime service.
    pub(crate) agent_turns_started: u64,
    /// Number of agent turns that ended completed.
    pub(crate) agent_turns_completed: u64,
    /// Number of agent turns that ended failed.
    pub(crate) agent_turns_failed: u64,
    /// Number of agent turns that ended interrupted.
    pub(crate) agent_turns_interrupted: u64,
    /// Number of agent turns that ended blocked waiting for approval or child work.
    pub(crate) agent_turns_blocked: u64,
    /// Number of provider request shapes recorded from runtime executions.
    pub(crate) provider_requests_started: u64,
    /// Number of recorded provider requests in capability-decision mode.
    pub(crate) provider_request_capability_decision: u64,
    /// Number of recorded provider requests in action-execution mode.
    pub(crate) provider_request_action_execution: u64,
    /// Number of recorded provider requests in repair mode.
    pub(crate) provider_request_repair: u64,
    /// Number of recorded provider requests in auto-sizing mode.
    pub(crate) provider_request_auto_sizing: u64,
    /// Number of provider executions that returned a usable response.
    pub(crate) provider_responses_succeeded: u64,
    /// Number of provider executions that failed before a usable response.
    pub(crate) provider_responses_failed: u64,
    /// Number of request shapes with available prompt-cache diagnostics.
    pub(crate) provider_prompt_cache_diagnostics_available: u64,
    /// Number of request shapes whose prompt-cache diagnostics could not be built.
    pub(crate) provider_prompt_cache_diagnostics_failed: u64,
    /// Number of provider responses that reported cached input tokens.
    pub(crate) provider_cached_input_reports: u64,
    /// Number of provider responses that did not report cached input tokens.
    pub(crate) provider_cached_input_unknown: u64,
    /// Number of provider responses that reported zero cached input tokens.
    pub(crate) provider_cached_input_zero_hits: u64,
    /// Accumulated provider input tokens.
    pub(crate) provider_input_tokens: u64,
    /// Accumulated provider output tokens.
    pub(crate) provider_output_tokens: u64,
    /// Accumulated provider reasoning tokens.
    pub(crate) provider_reasoning_tokens: u64,
    /// Accumulated provider cached input tokens when reported.
    pub(crate) provider_cached_input_tokens: u64,
    /// Accumulated provider cache-write input tokens when reported.
    pub(crate) provider_cache_write_input_tokens: u64,
    /// Accumulated provider input tokens not reported as cache hits.
    pub(crate) provider_billed_input_tokens: u64,
    /// Accumulated provider token usage grouped by provider/model.
    pub(crate) provider_token_usage_by_model: BTreeMap<ModelTokenUsageKey, ModelTokenUsage>,
    /// Number of shell action dispatch attempts that reached dispatch accounting.
    pub(crate) shell_action_batches: u64,
    /// Number of shell-backed agent actions dispatched to panes.
    pub(crate) shell_actions_dispatched: u64,
    /// Number of shell transactions observed to completion.
    pub(crate) shell_transactions_observed: u64,
    /// Number of shell transactions that exited successfully.
    pub(crate) shell_transactions_succeeded: u64,
    /// Number of shell transactions that exited non-zero.
    pub(crate) shell_transactions_failed: u64,
    /// Number of shell transaction marker protocol violations.
    pub(crate) shell_transaction_protocol_violations: u64,
    /// Number of completed streaming projection results received by the actor.
    pub(crate) agent_streaming_projection_results: u64,
    /// Number of current streaming projection results installed atomically.
    pub(crate) agent_streaming_projection_installs: u64,
    /// Number of streaming projection results rejected as stale.
    pub(crate) agent_streaming_projection_rejections: u64,
    /// Number of rejected streaming projections whose pane lineage changed.
    pub(crate) agent_streaming_projection_lineage_rejections: u64,
    /// Number of resize workers that reused actor-cached decoded entries.
    pub(crate) agent_presentation_decoded_cache_hits: u64,
    /// Number of resize workers that decoded durable presentation storage.
    pub(crate) agent_presentation_decoded_cache_misses: u64,
    /// Number of resize workers that reused an exact canonical width snapshot.
    pub(crate) agent_presentation_snapshot_cache_hits: u64,
    /// Number of resize workers that semantically rebuilt a canonical snapshot.
    pub(crate) agent_presentation_snapshot_cache_misses: u64,
    /// Number of durable entries semantically replayed by resize workers.
    pub(crate) agent_presentation_replayed_entries: u64,
    /// Number of decoded or snapshot entries evicted by cache bounds.
    pub(crate) agent_presentation_cache_evictions: u64,
    /// Histogram of provider request message counts.
    pub(crate) provider_request_message_counts: crate::host::async_runtime::RuntimeHistogram,
    /// Histogram of total provider request message bytes.
    pub(crate) provider_request_message_bytes: crate::host::async_runtime::RuntimeHistogram,
    /// Histogram of OpenAI instruction bytes in cache diagnostics.
    pub(crate) provider_prompt_instructions_bytes: crate::host::async_runtime::RuntimeHistogram,
    /// Histogram of OpenAI response-format bytes in cache diagnostics.
    pub(crate) provider_prompt_response_format_bytes: crate::host::async_runtime::RuntimeHistogram,
    /// Histogram of OpenAI tool schema bytes in cache diagnostics.
    pub(crate) provider_prompt_tools_bytes: crate::host::async_runtime::RuntimeHistogram,
    /// Histogram of OpenAI tool-choice bytes in cache diagnostics.
    pub(crate) provider_prompt_tool_choice_bytes: crate::host::async_runtime::RuntimeHistogram,
    /// Histogram of stable input bytes in cache diagnostics.
    pub(crate) provider_prompt_stable_input_bytes: crate::host::async_runtime::RuntimeHistogram,
    /// Histogram of volatile input bytes in cache diagnostics.
    pub(crate) provider_prompt_volatile_input_bytes: crate::host::async_runtime::RuntimeHistogram,
    /// Histogram of local instructions-and-stable-input projection bytes.
    pub(crate) provider_prompt_stable_projection_bytes:
        crate::host::async_runtime::RuntimeHistogram,
    /// Histogram of provider request-shape bytes tracked outside the prompt prefix.
    pub(crate) provider_request_shape_bytes: crate::host::async_runtime::RuntimeHistogram,
    /// Histogram of latest response input tokens.
    pub(crate) provider_input_tokens_per_response: crate::host::async_runtime::RuntimeHistogram,
    /// Histogram of latest response output tokens.
    pub(crate) provider_output_tokens_per_response: crate::host::async_runtime::RuntimeHistogram,
    /// Histogram of latest response cached input tokens.
    pub(crate) provider_cached_input_tokens_per_response:
        crate::host::async_runtime::RuntimeHistogram,
    /// Histogram of latest response cache-write input tokens.
    pub(crate) provider_cache_write_input_tokens_per_response:
        crate::host::async_runtime::RuntimeHistogram,
    /// Histogram of latest response cache-hit ratios in basis points.
    pub(crate) provider_cached_input_hit_ratio_basis_points:
        crate::host::async_runtime::RuntimeHistogram,
    /// Histogram of MAAP action counts per provider response.
    pub(crate) provider_response_action_counts: crate::host::async_runtime::RuntimeHistogram,
    /// Histogram of shell actions dispatched per dispatch pass.
    pub(crate) shell_actions_dispatched_per_batch: crate::host::async_runtime::RuntimeHistogram,
    /// Histogram of shell transaction elapsed milliseconds.
    pub(crate) shell_transaction_duration_ms: crate::host::async_runtime::RuntimeHistogram,
    /// Histogram of shell transaction model-visible output bytes.
    pub(crate) shell_transaction_output_bytes: crate::host::async_runtime::RuntimeHistogram,
    /// Most recent provider identifier observed by runtime metrics.
    pub(crate) last_provider: Option<String>,
    /// Most recent provider model observed by runtime metrics.
    pub(crate) last_model: Option<String>,
    /// Most recent provider interaction kind observed by runtime metrics.
    pub(crate) last_interaction_kind: Option<String>,
    /// Most recent allowed action surface observed by runtime metrics.
    pub(crate) last_allowed_actions: Option<String>,
    /// Most recent prompt-cache key observed by runtime metrics.
    pub(crate) last_prompt_cache_key: Option<String>,
    /// Most recent local instructions-and-stable-input projection digest.
    pub(crate) last_stable_projection_sha256: Option<String>,
    /// Most recent provider request-shape digest observed by runtime metrics.
    pub(crate) last_provider_request_shape_sha256: Option<String>,
    /// Most recent complete provider-visible request digest.
    pub(crate) last_provider_request_sha256: Option<String>,
    /// Most recent complete provider-visible request byte count.
    pub(crate) last_provider_request_bytes: Option<usize>,
    /// First divergence category from the preceding comparable request.
    pub(crate) last_provider_request_continuity_category: Option<String>,
    /// First divergent provider input message index, when applicable.
    pub(crate) last_provider_request_continuity_message_index: Option<usize>,
    /// Number of unchanged provider input messages at the request front.
    pub(crate) last_provider_request_common_message_prefix: Option<usize>,
    /// Number of unchanged request components before the first divergence.
    pub(crate) last_provider_request_common_component_prefix: Option<usize>,
    /// Whether provider input messages only appended after the previous request.
    pub(crate) last_provider_request_messages_append_only: Option<bool>,
    /// Number of unchanged cache-eligible input messages at the request front.
    pub(crate) last_provider_request_common_stable_message_prefix: Option<usize>,
    /// Whether cache-eligible input only appended after the previous prefix.
    pub(crate) last_provider_request_stable_messages_append_only: Option<bool>,
    /// Previous OpenAI request snapshot retained only for continuity comparison.
    pub(crate) last_openai_request_continuity_snapshot:
        Option<mez_agent::OpenAiRequestContinuitySnapshot>,
    /// OpenAI request baselines isolated by actual cache-routing scope.
    provider_wire_continuity_by_scope:
        BTreeMap<ProviderWireContinuityKey, mez_agent::OpenAiRequestContinuitySnapshot>,
    /// Latest execution request and matching usage per conversation.
    provider_wire_status_by_conversation: BTreeMap<String, RuntimeProviderWireRequestStatus>,
    /// Most recent tool-choice digest observed by runtime metrics.
    pub(crate) last_tool_choice_sha256: Option<String>,
    /// Most recent provider output-token budget source observed by runtime metrics.
    pub(crate) last_provider_output_token_budget_source: Option<String>,
    /// Most recent provider output-token budget value observed by runtime metrics.
    pub(crate) last_provider_output_token_budget_tokens: Option<usize>,
    /// Most recent temporary output-limit retry override observed by runtime metrics.
    pub(crate) last_provider_output_limit_retry_override_tokens: Option<usize>,
    /// Most recent provider response input tokens observed by runtime metrics.
    pub(crate) last_provider_input_tokens: Option<u64>,
    /// Most recent provider response cached input tokens, when reported.
    pub(crate) last_provider_cached_input_tokens: Option<u64>,
    /// Most recent provider response cache-write input tokens, when reported.
    pub(crate) last_provider_cache_write_input_tokens: Option<u64>,
    /// Most recent provider response cache-hit ratio in basis points.
    pub(crate) last_provider_cached_input_hit_ratio_basis_points: Option<u32>,
}

impl RuntimeMetricsSnapshot {
    /// Returns the latest concrete execution request diagnostics for a conversation.
    pub(crate) fn provider_wire_status(
        &self,
        conversation_id: &str,
    ) -> Option<&RuntimeProviderWireRequestStatus> {
        self.provider_wire_status_by_conversation
            .get(conversation_id)
    }

    /// Records one concrete provider send with its matching response usage.
    ///
    /// OpenAI continuity baselines are isolated by conversation, endpoint
    /// namespace, provider, model, and prompt-cache lineage. Auxiliary calls
    /// participate in aggregate request metrics but do not replace the latest
    /// user-visible execution request shown by `/status`.
    pub(crate) fn record_provider_wire_request_observation(
        &mut self,
        observation: &crate::integrations::agent::provider::ProviderWireRequestObservation,
    ) -> RuntimeProviderWireRequestStatus {
        self.provider_requests_started = self.provider_requests_started.saturating_add(1);
        match observation.interaction_kind.as_str() {
            "capability_decision" => {
                self.provider_request_capability_decision =
                    self.provider_request_capability_decision.saturating_add(1);
            }
            "action_execution" | "capability_continuation" => {
                self.provider_request_action_execution =
                    self.provider_request_action_execution.saturating_add(1);
            }
            "maap_repair" => {
                self.provider_request_repair = self.provider_request_repair.saturating_add(1);
            }
            "auto_sizing" => {
                self.provider_request_auto_sizing =
                    self.provider_request_auto_sizing.saturating_add(1);
            }
            _ => {}
        }
        self.provider_request_message_counts
            .record(observation.message_count as u64);
        self.provider_request_message_bytes
            .record(observation.message_bytes as u64);
        self.last_provider = Some(observation.provider.clone());
        self.last_model = Some(observation.model.clone());
        self.last_interaction_kind = Some(observation.interaction_kind.clone());
        self.last_allowed_actions = Some(observation.allowed_actions.clone());
        self.last_provider_output_token_budget_tokens = observation.max_output_tokens;
        self.last_provider_output_limit_retry_override_tokens =
            observation.output_limit_retry_override_tokens;
        self.last_provider_output_token_budget_source = Some(
            match (
                observation.max_output_tokens,
                observation.output_limit_retry_override_tokens,
            ) {
                (Some(_), Some(_)) => "temporary_output_limit_retry_override".to_string(),
                (Some(_), None) => "configured".to_string(),
                (None, _) => "omitted_provider_default".to_string(),
            },
        );

        let continuity = observation
            .openai_diagnostics
            .as_ref()
            .and_then(|diagnostics| {
                self.provider_prompt_cache_diagnostics_available = self
                    .provider_prompt_cache_diagnostics_available
                    .saturating_add(1);
                self.provider_prompt_instructions_bytes
                    .record(diagnostics.instructions_bytes as u64);
                self.provider_prompt_response_format_bytes
                    .record(diagnostics.response_format_bytes as u64);
                self.provider_prompt_tools_bytes
                    .record(diagnostics.tools_bytes as u64);
                self.provider_prompt_tool_choice_bytes
                    .record(diagnostics.tool_choice_bytes as u64);
                self.provider_prompt_stable_input_bytes
                    .record(diagnostics.stable_input_bytes as u64);
                self.provider_prompt_volatile_input_bytes
                    .record(diagnostics.volatile_input_bytes as u64);
                self.provider_prompt_stable_projection_bytes
                    .record(diagnostics.stable_projection_bytes as u64);
                self.provider_request_shape_bytes
                    .record(diagnostics.provider_request_shape_bytes as u64);
                self.last_prompt_cache_key = Some(diagnostics.prompt_cache_key.clone());
                self.last_stable_projection_sha256 =
                    Some(diagnostics.stable_projection_sha256.clone());
                self.last_provider_request_shape_sha256 =
                    Some(diagnostics.provider_request_shape_sha256.clone());
                self.last_tool_choice_sha256 = Some(diagnostics.tool_choice_sha256.clone());
                self.last_provider_request_sha256 =
                    Some(diagnostics.continuity_snapshot.request_sha256.clone());
                self.last_provider_request_bytes =
                    Some(diagnostics.continuity_snapshot.request_bytes);

                let key = ProviderWireContinuityKey {
                    conversation_id: observation.conversation_id.clone(),
                    cache_namespace: observation.cache_namespace.clone(),
                    provider: observation.provider.clone(),
                    model: observation.model.clone(),
                    prompt_cache_lineage_id: observation.prompt_cache_lineage_id.clone(),
                };
                let continuity = self
                    .provider_wire_continuity_by_scope
                    .get(&key)
                    .map(|previous| {
                        mez_agent::compare_openai_request_continuity(
                            previous,
                            &diagnostics.continuity_snapshot,
                        )
                    });
                self.provider_wire_continuity_by_scope
                    .insert(key, diagnostics.continuity_snapshot.clone());
                while self.provider_wire_continuity_by_scope.len()
                    > PROVIDER_WIRE_DIAGNOSTIC_SCOPE_LIMIT
                {
                    let _ = self.provider_wire_continuity_by_scope.pop_first();
                }
                continuity
            });
        if observation.diagnostics_failed {
            self.provider_prompt_cache_diagnostics_failed = self
                .provider_prompt_cache_diagnostics_failed
                .saturating_add(1);
        }

        self.last_provider_request_continuity_category = Some(
            continuity
                .as_ref()
                .map_or_else(|| "initial".to_string(), |value| value.category.clone()),
        );
        self.last_provider_request_continuity_message_index =
            continuity.as_ref().and_then(|value| value.message_index);
        self.last_provider_request_common_message_prefix =
            continuity.as_ref().map(|value| value.common_message_prefix);
        self.last_provider_request_common_component_prefix = continuity
            .as_ref()
            .map(|value| value.common_component_prefix);
        self.last_provider_request_messages_append_only =
            continuity.as_ref().map(|value| value.messages_append_only);
        self.last_provider_request_common_stable_message_prefix = continuity
            .as_ref()
            .map(|value| value.common_stable_message_prefix);
        self.last_provider_request_stable_messages_append_only = continuity
            .as_ref()
            .map(|value| value.stable_messages_append_only);

        let status = RuntimeProviderWireRequestStatus {
            request_id: observation.request_id.clone(),
            provider: observation.provider.clone(),
            model: observation.model.clone(),
            interaction_kind: observation.interaction_kind.clone(),
            purpose: observation.purpose.as_str().to_string(),
            usage: observation.usage,
            stable_input_bytes: observation
                .openai_diagnostics
                .as_ref()
                .map(|diagnostics| diagnostics.stable_input_bytes),
            volatile_input_bytes: observation
                .openai_diagnostics
                .as_ref()
                .map(|diagnostics| diagnostics.volatile_input_bytes),
            mcp_live_state_bytes: observation.mcp_live_state_bytes,
            mcp_catalog_bytes: observation.mcp_catalog_bytes,
            action_result_bytes: observation.action_result_bytes,
            continuity,
        };
        if observation.purpose
            == crate::integrations::agent::provider::ProviderRequestPurpose::Execution
        {
            self.provider_wire_status_by_conversation
                .insert(observation.conversation_id.clone(), status.clone());
            while self.provider_wire_status_by_conversation.len()
                > PROVIDER_WIRE_DIAGNOSTIC_SCOPE_LIMIT
            {
                let _ = self.provider_wire_status_by_conversation.pop_first();
            }
        }
        status
    }

    /// Records that one runtime-owned agent turn started execution.
    #[cfg(test)]
    pub(crate) fn record_agent_turn_started(&mut self) {
        self.agent_turns_started = self.agent_turns_started.saturating_add(1);
    }

    /// Records one terminal or blocked turn outcome.
    pub(crate) fn record_agent_turn_finished(&mut self, state: AgentTurnState) {
        match state {
            AgentTurnState::Completed => {
                self.agent_turns_completed = self.agent_turns_completed.saturating_add(1);
            }
            AgentTurnState::Failed => {
                self.agent_turns_failed = self.agent_turns_failed.saturating_add(1);
            }
            AgentTurnState::Interrupted => {
                self.agent_turns_interrupted = self.agent_turns_interrupted.saturating_add(1);
            }
            AgentTurnState::Blocked => {
                self.agent_turns_blocked = self.agent_turns_blocked.saturating_add(1);
            }
            AgentTurnState::Queued | AgentTurnState::Running => {}
        }
    }

    /// Records one successful provider execution and its response shape.
    pub(crate) fn record_provider_response(
        &mut self,
        response: &ModelResponse,
        latest_usage: ModelTokenUsage,
        model_key: &ModelTokenUsageKey,
    ) {
        self.provider_responses_succeeded = self.provider_responses_succeeded.saturating_add(1);
        self.provider_response_action_counts.record(
            response
                .action_batch
                .as_ref()
                .map(|batch| batch.actions.len() as u64)
                .unwrap_or(0),
        );
        self.record_provider_token_usage(response.usage, latest_usage, model_key);
    }

    /// Records one provider request that failed before yielding a usable response.
    pub(crate) fn record_provider_failure(&mut self) {
        self.provider_responses_failed = self.provider_responses_failed.saturating_add(1);
    }

    /// Records provider token counters and per-response token histograms.
    pub(crate) fn record_provider_token_usage(
        &mut self,
        usage: ModelTokenUsage,
        latest_usage: ModelTokenUsage,
        model_key: &ModelTokenUsageKey,
    ) {
        self.provider_input_tokens = self
            .provider_input_tokens
            .saturating_add(usage.input_tokens);
        self.provider_output_tokens = self
            .provider_output_tokens
            .saturating_add(usage.output_tokens);
        self.provider_reasoning_tokens = self
            .provider_reasoning_tokens
            .saturating_add(usage.reasoning_tokens);
        self.provider_cached_input_tokens = self
            .provider_cached_input_tokens
            .saturating_add(usage.cached_input_tokens.unwrap_or(0));
        self.provider_cache_write_input_tokens = self
            .provider_cache_write_input_tokens
            .saturating_add(usage.cache_write_input_tokens.unwrap_or(0));
        self.provider_billed_input_tokens = self
            .provider_billed_input_tokens
            .saturating_add(usage.billed_input_tokens());
        if !usage.is_zero() {
            self.provider_token_usage_by_model
                .entry(model_key.clone())
                .or_default()
                .add_assign(usage);
        }
        self.provider_input_tokens_per_response
            .record(latest_usage.input_tokens);
        self.provider_output_tokens_per_response
            .record(latest_usage.output_tokens);
        self.last_provider_input_tokens = Some(latest_usage.input_tokens);
        self.last_provider_cached_input_tokens = latest_usage.cached_input_tokens;
        self.last_provider_cache_write_input_tokens = latest_usage.cache_write_input_tokens;
        self.last_provider_cached_input_hit_ratio_basis_points =
            latest_usage.cached_input_hit_ratio_basis_points();
        if let Some(cache_write) = latest_usage.cache_write_input_tokens {
            self.provider_cache_write_input_tokens_per_response
                .record(cache_write);
        }
        if let Some(cached) = latest_usage.cached_input_tokens {
            self.provider_cached_input_reports =
                self.provider_cached_input_reports.saturating_add(1);
            if cached == 0 {
                self.provider_cached_input_zero_hits =
                    self.provider_cached_input_zero_hits.saturating_add(1);
            }
            self.provider_cached_input_tokens_per_response
                .record(cached);
            let denominator = self
                .provider_billed_input_tokens
                .saturating_add(self.provider_cached_input_tokens);
            let ratio = self
                .provider_cached_input_tokens
                .saturating_mul(10_000)
                .saturating_add(denominator / 2)
                .checked_div(denominator)
                .unwrap_or(0);
            self.provider_cached_input_hit_ratio_basis_points
                .record(ratio.min(10_000));
        } else {
            self.provider_cached_input_unknown =
                self.provider_cached_input_unknown.saturating_add(1);
        }
    }

    /// Records the number of shell-backed actions dispatched in one pass.
    pub(crate) fn record_shell_action_batch(&mut self, dispatched: usize) {
        self.shell_action_batches = self.shell_action_batches.saturating_add(1);
        self.shell_actions_dispatched = self
            .shell_actions_dispatched
            .saturating_add(dispatched as u64);
        self.shell_actions_dispatched_per_batch
            .record(dispatched as u64);
    }

    /// Records one completed shell transaction and its result payload size.
    pub(crate) fn record_shell_transaction_completion(
        &mut self,
        started_at_unix_ms: u64,
        finished_at_unix_ms: u64,
        output_bytes: usize,
        exit_code: i32,
    ) {
        self.shell_transactions_observed = self.shell_transactions_observed.saturating_add(1);
        if exit_code == 0 {
            self.shell_transactions_succeeded = self.shell_transactions_succeeded.saturating_add(1);
        } else {
            self.shell_transactions_failed = self.shell_transactions_failed.saturating_add(1);
        }
        self.shell_transaction_duration_ms
            .record(finished_at_unix_ms.saturating_sub(started_at_unix_ms));
        self.shell_transaction_output_bytes
            .record(output_bytes as u64);
    }

    /// Records one shell wrapper marker protocol violation.
    pub(crate) fn record_shell_transaction_protocol_violation(&mut self) {
        self.shell_transaction_protocol_violations =
            self.shell_transaction_protocol_violations.saturating_add(1);
    }

    /// Records one content-free streaming projection completion outcome.
    pub(crate) fn record_agent_streaming_projection_result(
        &mut self,
        installed: bool,
        lineage_rejected: bool,
    ) {
        self.agent_streaming_projection_results =
            self.agent_streaming_projection_results.saturating_add(1);
        if installed {
            self.agent_streaming_projection_installs =
                self.agent_streaming_projection_installs.saturating_add(1);
        } else {
            self.agent_streaming_projection_rejections =
                self.agent_streaming_projection_rejections.saturating_add(1);
            if lineage_rejected {
                self.agent_streaming_projection_lineage_rejections = self
                    .agent_streaming_projection_lineage_rejections
                    .saturating_add(1);
            }
        }
    }

    /// Records one completed background presentation resize cache outcome.
    pub(crate) fn record_agent_presentation_resize_cache(
        &mut self,
        decoded_hit: bool,
        snapshot_hit: bool,
        replayed_entries: usize,
        evictions: u64,
    ) {
        if decoded_hit {
            self.agent_presentation_decoded_cache_hits =
                self.agent_presentation_decoded_cache_hits.saturating_add(1);
        } else {
            self.agent_presentation_decoded_cache_misses = self
                .agent_presentation_decoded_cache_misses
                .saturating_add(1);
        }
        if snapshot_hit {
            self.agent_presentation_snapshot_cache_hits = self
                .agent_presentation_snapshot_cache_hits
                .saturating_add(1);
        } else {
            self.agent_presentation_snapshot_cache_misses = self
                .agent_presentation_snapshot_cache_misses
                .saturating_add(1);
        }
        self.agent_presentation_replayed_entries = self
            .agent_presentation_replayed_entries
            .saturating_add(u64::try_from(replayed_entries).unwrap_or(u64::MAX));
        self.agent_presentation_cache_evictions = self
            .agent_presentation_cache_evictions
            .saturating_add(evictions);
    }
}

#[cfg(test)]
mod provider_wire_tests {
    use super::*;
    use crate::integrations::agent::provider::{
        ProviderRequestPurpose, ProviderWireRequestObservation,
    };

    fn request(model: &str, lineage: &str) -> mez_agent::ModelRequest {
        mez_agent::ModelRequest {
            provider: "openai".to_string(),
            model: model.to_string(),
            reasoning_effort: None,
            thinking_enabled: None,
            latency_preference: None,
            prompt_cache_retention: None,
            max_output_tokens: None,
            temperature: None,
            stop: None,
            prompt_cache_session_id: Some("cache-session".to_string()),
            prompt_cache_lineage_id: Some(lineage.to_string()),
            turn_id: "turn-wire".to_string(),
            agent_id: "agent-wire".to_string(),
            available_mcp_tools: Vec::new(),
            memory_actions_enabled: false,
            issue_actions_enabled: false,
            interaction_kind: mez_agent::ModelInteractionKind::ActionExecution,
            allowed_actions: mez_agent::AllowedActionSet::say_only(),
            recovery_input: None,
            messages: vec![
                mez_agent::ModelMessage {
                    role: mez_agent::ModelMessageRole::System,
                    source: mez_agent::ContextSourceKind::System,
                    placement: mez_agent::ContextPlacement::StablePrefix,
                    content: "stable instructions".to_string(),
                },
                mez_agent::ModelMessage {
                    role: mez_agent::ModelMessageRole::User,
                    source: mez_agent::ContextSourceKind::UserInstruction,
                    placement: mez_agent::ContextPlacement::ConversationAppend,
                    content: "user request".to_string(),
                },
            ]
            .into(),
        }
    }

    fn observation(
        request_id: &str,
        conversation_id: &str,
        namespace: &str,
        provider: &str,
        request: &mez_agent::ModelRequest,
        purpose: ProviderRequestPurpose,
        usage: Option<ModelTokenUsage>,
    ) -> ProviderWireRequestObservation {
        ProviderWireRequestObservation {
            request_id: request_id.to_string(),
            attempt_index: 1,
            retry_reason: None,
            conversation_id: conversation_id.to_string(),
            turn_id: request.turn_id.clone(),
            agent_id: request.agent_id.clone(),
            pane_id: "%wire".to_string(),
            provider: provider.to_string(),
            cache_namespace: namespace.to_string(),
            model: request.model.clone(),
            prompt_cache_lineage_id: request.prompt_cache_lineage_id.clone(),
            interaction_kind: request.interaction_kind.as_str().to_string(),
            allowed_actions: request.allowed_actions.action_type_names().join(","),
            max_output_tokens: request.max_output_tokens,
            output_limit_retry_override_tokens: None,
            purpose,
            message_count: request.messages.len(),
            message_bytes: request
                .messages
                .iter()
                .map(|message| message.content.len())
                .sum(),
            mcp_live_state_bytes: 0,
            mcp_catalog_bytes: 0,
            action_result_bytes: 0,
            openai_diagnostics: Some(
                mez_agent::openai_prompt_cache_diagnostics_for_request(request).unwrap(),
            ),
            diagnostics_failed: false,
            usage,
            succeeded: true,
            failure_kind: None,
        }
    }

    /// Verifies continuity comparisons never cross an actual provider-cache
    /// routing boundary.
    #[test]
    fn continuity_is_isolated_by_every_cache_routing_key() {
        let mut metrics = RuntimeMetricsSnapshot::default();
        let base_request = request("gpt-wire", "lineage-a");
        let base = observation(
            "wire-base",
            "conversation-a",
            "namespace-a",
            "openai",
            &base_request,
            ProviderRequestPurpose::Execution,
            None,
        );
        assert_eq!(
            metrics
                .record_provider_wire_request_observation(&base)
                .continuity,
            None
        );
        let mut repeated = base.clone();
        repeated.request_id = "wire-repeated".to_string();
        let repeated = metrics.record_provider_wire_request_observation(&repeated);
        assert_eq!(
            repeated
                .continuity
                .as_ref()
                .map(|value| value.category.as_str()),
            Some("identical")
        );

        let mut variants = Vec::new();
        let mut different_conversation = base.clone();
        different_conversation.request_id = "wire-conversation".to_string();
        different_conversation.conversation_id = "conversation-b".to_string();
        variants.push(different_conversation);
        let mut different_namespace = base.clone();
        different_namespace.request_id = "wire-namespace".to_string();
        different_namespace.cache_namespace = "namespace-b".to_string();
        variants.push(different_namespace);
        let mut different_provider = base.clone();
        different_provider.request_id = "wire-provider".to_string();
        different_provider.provider = "openai-compatible".to_string();
        variants.push(different_provider);
        let different_model_request = request("gpt-wire-b", "lineage-a");
        variants.push(observation(
            "wire-model",
            "conversation-a",
            "namespace-a",
            "openai",
            &different_model_request,
            ProviderRequestPurpose::Execution,
            None,
        ));
        let different_lineage_request = request("gpt-wire", "lineage-b");
        variants.push(observation(
            "wire-lineage",
            "conversation-a",
            "namespace-a",
            "openai",
            &different_lineage_request,
            ProviderRequestPurpose::Execution,
            None,
        ));

        for variant in variants {
            assert_eq!(
                metrics
                    .record_provider_wire_request_observation(&variant)
                    .continuity,
                None,
                "scope leaked for {}",
                variant.request_id
            );
        }
    }

    /// Verifies execution status retains the exact request's usage and is not
    /// replaced by auxiliary traffic.
    #[test]
    fn execution_status_pairs_exact_usage_and_ignores_auxiliary_calls() {
        let mut metrics = RuntimeMetricsSnapshot::default();
        let request = request("gpt-wire", "lineage-a");
        let warm_usage = ModelTokenUsage {
            input_tokens: 100,
            cached_input_tokens: Some(80),
            ..ModelTokenUsage::default()
        };
        let execution = observation(
            "wire-execution",
            "conversation-a",
            "namespace-a",
            "openai",
            &request,
            ProviderRequestPurpose::Execution,
            Some(warm_usage),
        );
        metrics.record_provider_wire_request_observation(&execution);

        let mut auxiliary = execution.clone();
        auxiliary.request_id = "wire-auxiliary".to_string();
        auxiliary.purpose = ProviderRequestPurpose::Auxiliary;
        auxiliary.usage = Some(ModelTokenUsage {
            input_tokens: 20,
            cached_input_tokens: Some(0),
            ..ModelTokenUsage::default()
        });
        metrics.record_provider_wire_request_observation(&auxiliary);
        let status = metrics.provider_wire_status("conversation-a").unwrap();
        assert_eq!(status.request_id, "wire-execution");
        assert_eq!(status.usage, Some(warm_usage));

        let mut omitted = execution.clone();
        omitted.request_id = "wire-omitted".to_string();
        omitted.usage = None;
        metrics.record_provider_wire_request_observation(&omitted);
        let status = metrics.provider_wire_status("conversation-a").unwrap();
        assert_eq!(status.request_id, "wire-omitted");
        assert_eq!(status.usage, None);

        let explicit_zero = ModelTokenUsage {
            cached_input_tokens: Some(0),
            ..ModelTokenUsage::default()
        };
        let mut zero = execution;
        zero.request_id = "wire-zero".to_string();
        zero.usage = Some(explicit_zero);
        metrics.record_provider_wire_request_observation(&zero);
        assert_eq!(
            metrics
                .provider_wire_status("conversation-a")
                .unwrap()
                .usage,
            Some(explicit_zero)
        );
    }

    /// Verifies long-running hosts retain bounded continuity and status maps
    /// with deterministic oldest-key eviction.
    #[test]
    fn provider_wire_diagnostic_maps_are_bounded() {
        let mut metrics = RuntimeMetricsSnapshot::default();
        let request = request("gpt-wire", "lineage-a");
        let mut status = observation(
            "wire-status",
            "conversation-00000",
            "namespace-status",
            "openai",
            &request,
            ProviderRequestPurpose::Execution,
            None,
        );
        status.openai_diagnostics = None;
        let continuity = observation(
            "wire-continuity",
            "conversation-continuity",
            "namespace-00000",
            "openai",
            &request,
            ProviderRequestPurpose::Auxiliary,
            None,
        );
        for index in 0..=PROVIDER_WIRE_DIAGNOSTIC_SCOPE_LIMIT {
            status.request_id = format!("wire-status-{index}");
            status.conversation_id = format!("conversation-{index:05}");
            metrics.record_provider_wire_request_observation(&status);

            let mut continuity = continuity.clone();
            continuity.request_id = format!("wire-continuity-{index}");
            continuity.cache_namespace = format!("namespace-{index:05}");
            metrics.record_provider_wire_request_observation(&continuity);
        }

        assert_eq!(
            metrics.provider_wire_status_by_conversation.len(),
            PROVIDER_WIRE_DIAGNOSTIC_SCOPE_LIMIT
        );
        assert!(
            !metrics
                .provider_wire_status_by_conversation
                .contains_key("conversation-00000")
        );
        assert!(
            metrics
                .provider_wire_status_by_conversation
                .contains_key(&format!(
                    "conversation-{:05}",
                    PROVIDER_WIRE_DIAGNOSTIC_SCOPE_LIMIT
                ))
        );
        assert_eq!(
            metrics.provider_wire_continuity_by_scope.len(),
            PROVIDER_WIRE_DIAGNOSTIC_SCOPE_LIMIT
        );
        assert!(!metrics.provider_wire_continuity_by_scope.contains_key(
            &ProviderWireContinuityKey {
                conversation_id: "conversation-continuity".to_string(),
                cache_namespace: "namespace-00000".to_string(),
                provider: "openai".to_string(),
                model: "gpt-wire".to_string(),
                prompt_cache_lineage_id: Some("lineage-a".to_string()),
            }
        ));
    }
}
