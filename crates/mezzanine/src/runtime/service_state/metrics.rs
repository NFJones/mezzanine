//! Runtime constants, pane-title state, metrics snapshots, and patch records.

use super::{
    AgentTurnState, ModelRequest, ModelResponse, ModelTokenUsage, ModelTokenUsageKey,
    SubagentWaitPolicy,
};
use mez_mux::layout::PaneTitleSource;
use std::collections::BTreeMap;

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
/// Default number of successive shell commands before nudging implementation.
pub const DEFAULT_AGENT_IMPLEMENTATION_PRESSURE_AFTER_SHELL_ACTIONS: usize = 3;
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
    /// Previous OpenAI request snapshot retained only for continuity comparison.
    pub(crate) last_openai_request_continuity_snapshot:
        Option<mez_agent::OpenAiRequestContinuitySnapshot>,
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

    /// Records one provider request shape and prompt-cache diagnostic snapshot.
    pub(crate) fn record_provider_request_shape(
        &mut self,
        request: &ModelRequest,
        diagnostics: Option<&mez_agent::OpenAiPromptCacheDiagnostics>,
        diagnostics_failed: bool,
    ) {
        self.provider_requests_started = self.provider_requests_started.saturating_add(1);
        match request.interaction_kind.as_str() {
            "capability_decision" => {
                self.provider_request_capability_decision =
                    self.provider_request_capability_decision.saturating_add(1);
            }
            "action_execution" => {
                self.provider_request_action_execution =
                    self.provider_request_action_execution.saturating_add(1);
            }
            "repair" => {
                self.provider_request_repair = self.provider_request_repair.saturating_add(1);
            }
            "auto_sizing" => {
                self.provider_request_auto_sizing =
                    self.provider_request_auto_sizing.saturating_add(1);
            }
            _ => {}
        }
        self.provider_request_message_counts
            .record(request.messages.len() as u64);
        let message_bytes = request.messages.iter().fold(0u64, |sum, message| {
            sum.saturating_add(message.content.len() as u64)
        });
        self.provider_request_message_bytes.record(message_bytes);
        self.last_provider = Some(request.provider.clone());
        self.last_model = Some(request.model.clone());
        self.last_interaction_kind = Some(request.interaction_kind.as_str().to_string());
        self.last_allowed_actions = Some(request.allowed_actions.action_type_names().join(","));
        self.last_provider_output_token_budget_tokens = request.max_output_tokens;
        let output_limit_retry_override = provider_request_output_limit_retry_override(request);
        self.last_provider_output_limit_retry_override_tokens = output_limit_retry_override;
        self.last_provider_output_token_budget_source = Some(
            match (request.max_output_tokens, output_limit_retry_override) {
                (Some(_), Some(_)) => "temporary_output_limit_retry_override".to_string(),
                (Some(_), None) => "configured".to_string(),
                (None, _) => "omitted_provider_default".to_string(),
            },
        );
        if let Some(diagnostics) = diagnostics {
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
            self.last_stable_projection_sha256 = Some(diagnostics.stable_projection_sha256.clone());
            self.last_provider_request_shape_sha256 =
                Some(diagnostics.provider_request_shape_sha256.clone());
            self.last_tool_choice_sha256 = Some(diagnostics.tool_choice_sha256.clone());
            self.last_provider_request_sha256 =
                Some(diagnostics.continuity_snapshot.request_sha256.clone());
            self.last_provider_request_bytes = Some(diagnostics.continuity_snapshot.request_bytes);
            let continuity =
                self.last_openai_request_continuity_snapshot
                    .as_ref()
                    .map(|previous| {
                        mez_agent::compare_openai_request_continuity(
                            previous,
                            &diagnostics.continuity_snapshot,
                        )
                    });
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
            self.last_openai_request_continuity_snapshot =
                Some(diagnostics.continuity_snapshot.clone());
        } else if diagnostics_failed {
            self.provider_prompt_cache_diagnostics_failed = self
                .provider_prompt_cache_diagnostics_failed
                .saturating_add(1);
            self.last_openai_request_continuity_snapshot = None;
        } else {
            self.last_openai_request_continuity_snapshot = None;
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
}

/// Returns the output-limit retry override token value when the provider
/// request carries the second-stage request-local recovery mode that raises
/// the provider-visible budget.
pub(super) fn provider_request_output_limit_retry_override(
    request: &ModelRequest,
) -> Option<usize> {
    request.max_output_tokens.filter(|_| {
        request.messages.iter().any(|message| {
            message.placement == mez_agent::ContextPlacement::EphemeralTail
                && message
                    .content
                    .contains("provider_response_mode=compact_output_retry attempt=2")
        })
    })
}
