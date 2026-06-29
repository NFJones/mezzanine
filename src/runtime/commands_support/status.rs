//! Runtime status, message-log, and metrics display helpers.
//!
//! This module owns command-support formatting for runtime event messages,
//! pending observer/approval summaries, hook failures, and runtime/async
//! metrics so the command-support parent can remain focused on dispatch.

use super::*;

/// Runs the runtime show messages display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_show_messages_display(service: &RuntimeSessionService) -> String {
    let terminal_width = service.session.authoritative_size.columns;
    let pending_observers = service
        .session
        .observers()
        .iter()
        .filter(|observer| observer.state == ObserverDecisionState::Pending)
        .collect::<Vec<_>>();
    let pending_approvals = service.blocked_approvals().pending();
    let hook_failures = service
        .focused_shell_hook_results()
        .iter()
        .filter(|result| {
            matches!(
                result.status,
                HookExecutionStatus::Failed | HookExecutionStatus::TimedOut
            ) || result.failure.is_some()
        })
        .collect::<Vec<_>>();
    let mut status_lines = Vec::new();
    for observer in &pending_observers {
        status_lines.push(format!(
            "pending_observer={}:client={}:state=pending",
            observer.id, observer.client_id
        ));
    }
    for approval in &pending_approvals {
        status_lines.push(format!(
            "pending_approval={}:agent={}:pane={}:action={}",
            json_escape(&approval.id),
            json_escape(&approval.requesting_agent_id),
            json_escape(&approval.pane_id),
            json_escape(&approval.action_summary)
        ));
    }
    for result in &hook_failures {
        status_lines.push(format!(
            "hook_failure={}:event={}:status={}:exit_code={}",
            json_escape(&result.hook_id),
            runtime_hook_event_name(result.event),
            runtime_hook_execution_status_name(result.status),
            result
                .exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "none".to_string())
        ));
    }
    let summary = format!(
        "pending_observers={} pending_approvals={} hook_failures={}",
        pending_observers.len(),
        pending_approvals.len(),
        hook_failures.len()
    );
    let Some(event_log) = service.event_log() else {
        return runtime_show_messages_body(
            0,
            "source=runtime-event-log status=unavailable",
            &summary,
            terminal_width,
            status_lines,
        );
    };
    let events = event_log.replay_for(&EventAudience::Primary);
    if events.is_empty() {
        return runtime_show_messages_body(
            0,
            "source=runtime-event-log status=empty",
            &summary,
            terminal_width,
            status_lines,
        );
    }
    let mut lines = status_lines;
    lines.extend(
        events
            .iter()
            .rev()
            .map(|event| {
                format!(
                    "event_id={}:time={}:type={}:session={}:payload={}",
                    event.id,
                    json_escape(&event.time),
                    event_type_name(event.kind),
                    event
                        .session_id
                        .as_deref()
                        .map(json_escape)
                        .unwrap_or_else(|| "none".to_string()),
                    json_escape(&event.payload)
                )
            })
            .collect::<Vec<_>>(),
    );
    runtime_show_messages_body(
        events.len(),
        "source=runtime-event-log",
        &summary,
        terminal_width,
        lines,
    )
}

/// Formats one runtime histogram summary and bucket listing for pager output.
fn runtime_metrics_histogram_lines(
    name: &str,
    histogram: &crate::async_runtime::RuntimeHistogram,
) -> Vec<String> {
    let average = if histogram.observations == 0 {
        0.0
    } else {
        histogram.sum as f64 / histogram.observations as f64
    };
    let mut lines = vec![format!(
        "{name}: observations={} min={} max={} average={average:.2}",
        histogram.observations,
        histogram
            .min
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_string()),
        histogram
            .max
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_string()),
    )];
    lines.extend(histogram.buckets.iter().map(|bucket| {
        let upper_bound = if bucket.upper_bound == u64::MAX {
            "+inf".to_string()
        } else {
            bucket.upper_bound.to_string()
        };
        format!("  <= {upper_bound}: {}", bucket.count)
    }));
    lines
}

/// Formats provider token usage for the runtime metrics command.
fn runtime_provider_token_usage_metrics(usage: ModelTokenUsage) -> String {
    format!(
        "input={} cached_input={} cache_write_input={} output={} reasoning={} cache_hit={} total={}",
        usage.billed_input_tokens(),
        usage.cached_input_tokens_display(),
        usage.cache_write_input_tokens_display(),
        usage.output_tokens,
        usage.reasoning_tokens,
        usage.cached_input_hit_ratio_display(),
        usage.total_tokens()
    )
}

/// Builds stable per-model provider token metrics lines.
fn runtime_provider_token_usage_by_model_lines(
    usage_by_model: &BTreeMap<ModelTokenUsageKey, ModelTokenUsage>,
) -> Vec<String> {
    let mut lines = Vec::new();
    if usage_by_model.is_empty() {
        lines.push("provider_model_tokens = none".to_string());
        return lines;
    }
    for (key, usage) in usage_by_model {
        lines.push(format!(
            "provider_model_tokens[{}] = provider={} model={} {}",
            key.display_name(),
            key.provider,
            key.model,
            runtime_provider_token_usage_metrics(*usage)
        ));
    }
    lines
}

/// Runs the runtime show metrics display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_show_metrics_display(service: &RuntimeSessionService) -> String {
    let runtime_metrics = service.runtime_metrics();
    let mut lines = vec![
        "metrics source=runtime-service status=available".to_string(),
        "".to_string(),
        "[runtime counts]".to_string(),
        format!(
            "agent_turns_started = {}",
            runtime_metrics.agent_turns_started
        ),
        format!(
            "agent_turns_completed = {}",
            runtime_metrics.agent_turns_completed
        ),
        format!(
            "agent_turns_failed = {}",
            runtime_metrics.agent_turns_failed
        ),
        format!(
            "agent_turns_interrupted = {}",
            runtime_metrics.agent_turns_interrupted
        ),
        format!(
            "agent_turns_blocked = {}",
            runtime_metrics.agent_turns_blocked
        ),
        format!(
            "provider_requests_started = {}",
            runtime_metrics.provider_requests_started
        ),
        format!(
            "provider_request_capability_decision = {}",
            runtime_metrics.provider_request_capability_decision
        ),
        format!(
            "provider_request_action_execution = {}",
            runtime_metrics.provider_request_action_execution
        ),
        format!(
            "provider_request_repair = {}",
            runtime_metrics.provider_request_repair
        ),
        format!(
            "provider_request_auto_sizing = {}",
            runtime_metrics.provider_request_auto_sizing
        ),
        format!(
            "provider_responses_succeeded = {}",
            runtime_metrics.provider_responses_succeeded
        ),
        format!(
            "provider_responses_failed = {}",
            runtime_metrics.provider_responses_failed
        ),
        format!(
            "provider_prompt_cache_diagnostics_available = {}",
            runtime_metrics.provider_prompt_cache_diagnostics_available
        ),
        format!(
            "provider_prompt_cache_diagnostics_failed = {}",
            runtime_metrics.provider_prompt_cache_diagnostics_failed
        ),
        format!(
            "provider_cached_input_reports = {}",
            runtime_metrics.provider_cached_input_reports
        ),
        format!(
            "provider_cached_input_unknown = {}",
            runtime_metrics.provider_cached_input_unknown
        ),
        format!(
            "provider_cached_input_zero_hits = {}",
            runtime_metrics.provider_cached_input_zero_hits
        ),
        format!(
            "provider_input_tokens = {}",
            runtime_metrics.provider_input_tokens
        ),
        format!(
            "provider_output_tokens = {}",
            runtime_metrics.provider_output_tokens
        ),
        format!(
            "provider_reasoning_tokens = {}",
            runtime_metrics.provider_reasoning_tokens
        ),
        format!(
            "provider_cached_input_tokens = {}",
            runtime_metrics.provider_cached_input_tokens
        ),
        format!(
            "provider_cache_write_input_tokens = {}",
            runtime_metrics.provider_cache_write_input_tokens
        ),
        format!(
            "provider_billed_input_tokens = {}",
            runtime_metrics.provider_billed_input_tokens
        ),
        format!(
            "shell_action_batches = {}",
            runtime_metrics.shell_action_batches
        ),
        format!(
            "shell_actions_dispatched = {}",
            runtime_metrics.shell_actions_dispatched
        ),
        format!(
            "shell_transactions_observed = {}",
            runtime_metrics.shell_transactions_observed
        ),
        format!(
            "shell_transactions_succeeded = {}",
            runtime_metrics.shell_transactions_succeeded
        ),
        format!(
            "shell_transactions_failed = {}",
            runtime_metrics.shell_transactions_failed
        ),
        format!(
            "shell_transaction_protocol_violations = {}",
            runtime_metrics.shell_transaction_protocol_violations
        ),
        "".to_string(),
        "[runtime latest]".to_string(),
        format!(
            "last_provider = {}",
            runtime_metrics.last_provider.as_deref().unwrap_or("none")
        ),
        format!(
            "last_model = {}",
            runtime_metrics.last_model.as_deref().unwrap_or("none")
        ),
        format!(
            "last_interaction_kind = {}",
            runtime_metrics
                .last_interaction_kind
                .as_deref()
                .unwrap_or("none")
        ),
        format!(
            "last_allowed_actions = {}",
            runtime_metrics
                .last_allowed_actions
                .as_deref()
                .unwrap_or("none")
        ),
        format!(
            "last_prompt_cache_key = {}",
            runtime_metrics
                .last_prompt_cache_key
                .as_deref()
                .unwrap_or("none")
        ),
        format!(
            "last_stable_prompt_prefix_sha256 = {}",
            runtime_metrics
                .last_stable_prompt_prefix_sha256
                .as_deref()
                .unwrap_or("none")
        ),
        format!(
            "last_provider_request_shape_sha256 = {}",
            runtime_metrics
                .last_provider_request_shape_sha256
                .as_deref()
                .unwrap_or("none")
        ),
        format!(
            "last_tool_choice_sha256 = {}",
            runtime_metrics
                .last_tool_choice_sha256
                .as_deref()
                .unwrap_or("none")
        ),
        format!(
            "last_provider_output_token_budget_source = {}",
            runtime_metrics
                .last_provider_output_token_budget_source
                .as_deref()
                .unwrap_or("none")
        ),
        format!(
            "last_provider_output_token_budget_tokens = {}",
            runtime_metrics
                .last_provider_output_token_budget_tokens
                .map(|tokens| tokens.to_string())
                .unwrap_or_else(|| "none".to_string())
        ),
        format!(
            "last_provider_output_limit_retry_override_tokens = {}",
            runtime_metrics
                .last_provider_output_limit_retry_override_tokens
                .map(|tokens| tokens.to_string())
                .unwrap_or_else(|| "none".to_string())
        ),
        format!(
            "last_provider_input_tokens = {}",
            runtime_metrics
                .last_provider_input_tokens
                .map(|tokens| tokens.to_string())
                .unwrap_or_else(|| "none".to_string())
        ),
        format!(
            "last_provider_cached_input_tokens = {}",
            runtime_metrics
                .last_provider_cached_input_tokens
                .map(|tokens| tokens.to_string())
                .unwrap_or_else(|| "none".to_string())
        ),
        format!(
            "last_provider_cache_write_input_tokens = {}",
            runtime_metrics
                .last_provider_cache_write_input_tokens
                .map(|tokens| tokens.to_string())
                .unwrap_or_else(|| "none".to_string())
        ),
        format!(
            "last_provider_cached_input_hit_ratio = {}",
            runtime_metrics
                .last_provider_cached_input_hit_ratio_basis_points
                .map(|basis_points| format!("{}.{:02}%", basis_points / 100, basis_points % 100))
                .unwrap_or_else(|| "none".to_string())
        ),
        "".to_string(),
        "[runtime histograms]".to_string(),
    ];
    for (name, histogram) in [
        (
            "provider_request_message_counts",
            &runtime_metrics.provider_request_message_counts,
        ),
        (
            "provider_request_message_bytes",
            &runtime_metrics.provider_request_message_bytes,
        ),
        (
            "provider_prompt_instructions_bytes",
            &runtime_metrics.provider_prompt_instructions_bytes,
        ),
        (
            "provider_prompt_response_format_bytes",
            &runtime_metrics.provider_prompt_response_format_bytes,
        ),
        (
            "provider_prompt_tools_bytes",
            &runtime_metrics.provider_prompt_tools_bytes,
        ),
        (
            "provider_prompt_tool_choice_bytes",
            &runtime_metrics.provider_prompt_tool_choice_bytes,
        ),
        (
            "provider_prompt_stable_input_bytes",
            &runtime_metrics.provider_prompt_stable_input_bytes,
        ),
        (
            "provider_prompt_volatile_input_bytes",
            &runtime_metrics.provider_prompt_volatile_input_bytes,
        ),
        (
            "provider_prompt_stable_prefix_bytes",
            &runtime_metrics.provider_prompt_stable_prefix_bytes,
        ),
        (
            "provider_request_shape_bytes",
            &runtime_metrics.provider_request_shape_bytes,
        ),
        (
            "provider_prompt_cacheable_prefix_bytes",
            &runtime_metrics.provider_prompt_cacheable_prefix_bytes,
        ),
        (
            "provider_input_tokens_per_response",
            &runtime_metrics.provider_input_tokens_per_response,
        ),
        (
            "provider_output_tokens_per_response",
            &runtime_metrics.provider_output_tokens_per_response,
        ),
        (
            "provider_cached_input_tokens_per_response",
            &runtime_metrics.provider_cached_input_tokens_per_response,
        ),
        (
            "provider_cache_write_input_tokens_per_response",
            &runtime_metrics.provider_cache_write_input_tokens_per_response,
        ),
        (
            "provider_cached_input_hit_ratio_basis_points",
            &runtime_metrics.provider_cached_input_hit_ratio_basis_points,
        ),
        (
            "provider_response_action_counts",
            &runtime_metrics.provider_response_action_counts,
        ),
        (
            "shell_actions_dispatched_per_batch",
            &runtime_metrics.shell_actions_dispatched_per_batch,
        ),
        (
            "shell_transaction_duration_ms",
            &runtime_metrics.shell_transaction_duration_ms,
        ),
        (
            "shell_transaction_output_bytes",
            &runtime_metrics.shell_transaction_output_bytes,
        ),
    ] {
        lines.extend(runtime_metrics_histogram_lines(name, histogram));
    }
    lines.push("".to_string());
    lines.push("[runtime provider tokens by model]".to_string());
    lines.extend(runtime_provider_token_usage_by_model_lines(
        &runtime_metrics.provider_token_usage_by_model,
    ));
    lines.push("".to_string());
    let Some(metrics) = service.async_runtime_metrics() else {
        lines.push("metrics source=async-runtime status=unavailable".to_string());
        return lines.join("\n");
    };
    lines.extend([
        "metrics source=async-runtime status=available".to_string(),
        "".to_string(),
        "[async runtime counts]".to_string(),
        format!("commands_processed = {}", metrics.commands_processed),
        format!(
            "render_client_view_requests = {}",
            metrics.render_client_view_requests
        ),
        format!(
            "render_client_frame_requests = {}",
            metrics.render_client_frame_requests
        ),
        format!(
            "terminal_step_control_requests = {}",
            metrics.terminal_step_control_requests
        ),
        format!(
            "terminal_view_control_requests = {}",
            metrics.terminal_view_control_requests
        ),
        format!("runtime_event_batches = {}", metrics.runtime_event_batches),
        format!(
            "runtime_events_accepted = {}",
            metrics.runtime_events_accepted
        ),
        format!(
            "runtime_events_applied = {}",
            metrics.runtime_events_applied
        ),
        format!(
            "runtime_side_effects_queued = {}",
            metrics.runtime_side_effects_queued
        ),
        format!(
            "runtime_side_effects_drained = {}",
            metrics.runtime_side_effects_drained
        ),
        format!("pane_output_chunks = {}", metrics.pane_output_chunks),
        format!("pane_output_bytes = {}", metrics.pane_output_bytes),
        format!(
            "render_invalidations_coalesced = {}",
            metrics.render_invalidations_coalesced
        ),
        format!(
            "runtime_timer_schedules_queued = {}",
            metrics.runtime_timer_schedules_queued
        ),
        format!(
            "runtime_timer_cancellations_queued = {}",
            metrics.runtime_timer_cancellations_queued
        ),
        format!(
            "runtime_timer_events_ignored = {}",
            metrics.runtime_timer_events_ignored
        ),
        format!(
            "side_effect_queue_depth = {}",
            metrics.side_effect_queue_depth
        ),
        format!(
            "side_effect_queue_high_water = {}",
            metrics.side_effect_queue_high_water
        ),
        format!(
            "message_delivery_notifications = {}",
            metrics.message_delivery_notifications
        ),
        format!(
            "event_delivery_notifications = {}",
            metrics.event_delivery_notifications
        ),
        format!(
            "side_effect_delivery_notifications = {}",
            metrics.side_effect_delivery_notifications
        ),
        format!(
            "lifecycle_state_notifications = {}",
            metrics.lifecycle_state_notifications
        ),
        "".to_string(),
        "[async runtime histograms]".to_string(),
    ]);
    for (name, histogram) in [
        (
            "runtime_event_batch_sizes",
            &metrics.runtime_event_batch_sizes,
        ),
        (
            "runtime_side_effect_enqueue_sizes",
            &metrics.runtime_side_effect_enqueue_sizes,
        ),
        (
            "runtime_side_effect_drain_sizes",
            &metrics.runtime_side_effect_drain_sizes,
        ),
        ("pane_output_chunk_bytes", &metrics.pane_output_chunk_bytes),
        (
            "side_effect_queue_depth_samples",
            &metrics.side_effect_queue_depth_samples,
        ),
    ] {
        lines.extend(runtime_metrics_histogram_lines(name, histogram));
    }
    lines.join("\n")
}

/// Runs the runtime show messages body operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_show_messages_body(
    messages: usize,
    status: &str,
    summary: &str,
    terminal_width: u16,
    lines: Vec<String>,
) -> String {
    let header = format!("messages={messages} {status} {summary}");
    if lines.is_empty() {
        header
    } else {
        let wrapped = wrap_show_messages_lines(&lines, terminal_width);
        format!("{header}\n{}", wrapped.join("\n"))
    }
}

/// Wraps message-log detail rows to the configured terminal width.
///
/// The first physical row keeps the normal message text. Continuation rows are
/// indented by four spaces so long log entries remain readable without losing
/// their association with the preceding row.
fn wrap_show_messages_lines(lines: &[String], terminal_width: u16) -> Vec<String> {
    lines
        .iter()
        .flat_map(|line| {
            let continuation_width = terminal_width.saturating_sub(4).max(1);
            wrap_agent_log_lines(std::slice::from_ref(line), terminal_width)
                .into_iter()
                .enumerate()
                .flat_map(move |(index, wrapped)| {
                    if index == 0 {
                        vec![wrapped]
                    } else {
                        wrap_agent_log_lines(std::slice::from_ref(&wrapped), continuation_width)
                            .into_iter()
                            .map(|continued| format!("    {continued}"))
                            .collect::<Vec<_>>()
                    }
                })
        })
        .collect()
}
