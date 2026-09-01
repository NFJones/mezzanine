//! Runtime status, message-log, and metrics display helpers.
//!
//! This module owns command-support formatting for runtime event messages,
//! pending approval summaries, hook failures, and runtime/async
//! metrics so the command-support parent can remain focused on dispatch.

use super::{
    BTreeMap, EventAudience, HookExecutionStatus, ModelTokenUsage, ModelTokenUsageKey,
    RuntimeSessionService, event_type_name, runtime_hook_event_name,
    runtime_hook_execution_status_name,
};

/// Runs the runtime show messages display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_show_messages_display(service: &RuntimeSessionService) -> String {
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
    let mut rows = Vec::new();
    for approval in &pending_approvals {
        rows.push(show_messages_table_row(
            "pending approval",
            &approval.id,
            "—",
            &format!(
                "agent={}; pane={}; action={}",
                approval.requesting_agent_id, approval.pane_id, approval.action_summary
            ),
        ));
    }
    for result in &hook_failures {
        rows.push(show_messages_table_row(
            "hook failure",
            &result.hook_id,
            "—",
            &format!(
                "event={}; status={}; exit_code={}",
                runtime_hook_event_name(result.event),
                runtime_hook_execution_status_name(result.status),
                result
                    .exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "none".to_string())
            ),
        ));
    }
    let Some(event_log) = service.event_log() else {
        rows.push(show_messages_table_row(
            "unavailable",
            "—",
            "—",
            "runtime event log unavailable",
        ));
        return runtime_show_messages_table(rows);
    };
    let events = event_log.replay_for(&EventAudience::AllPrimaries);
    if events.is_empty() && rows.is_empty() {
        rows.push(show_messages_table_row(
            "empty",
            "—",
            "—",
            "no visible runtime messages",
        ));
    }
    rows.extend(
        events
            .iter()
            .rev()
            .map(|event| {
                show_messages_table_row(
                    event_type_name(event.kind),
                    &event.id.to_string(),
                    &event.time,
                    &format!(
                        "session={}; payload={}",
                        event.session_id.as_deref().unwrap_or("none"),
                        event.payload
                    ),
                )
            })
            .collect::<Vec<_>>(),
    );
    runtime_show_messages_table(rows)
}

/// Formats one runtime histogram summary and bucket listing for pager output.
fn runtime_metrics_histogram_lines(
    name: &str,
    histogram: &crate::host::async_runtime::RuntimeHistogram,
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
        "input={} cached_input={} output={} reasoning={} cumulative_cache_hit={} total={}",
        usage.billed_input_tokens(),
        usage.cached_input_tokens_display(),
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
    let iroh_metrics = service.integration.remote_iroh_diagnostics();
    let x11_metrics = service.runtime_x11_proxy_diagnostics();
    let mut lines = vec![
        "metrics source=runtime-service status=available".to_string(),
        "".to_string(),
        "[iroh transport]".to_string(),
        format!("listener_active = {}", iroh_metrics.listener_active),
        format!("active_connections = {}", iroh_metrics.active_connections),
        format!(
            "connections_accepted = {}",
            iroh_metrics.connections_accepted
        ),
        format!(
            "connections_rejected = {}",
            iroh_metrics.connections_rejected
        ),
        format!("setup_successes = {}", iroh_metrics.setup_successes),
        format!("setup_failures = {}", iroh_metrics.setup_failures),
        format!(
            "setup_latency_average_millis = {}",
            iroh_metrics.average_setup_latency_millis()
        ),
        format!(
            "setup_latency_max_millis = {}",
            iroh_metrics.setup_latency_max_millis
        ),
        format!(
            "connections_completed = {}",
            iroh_metrics.connections_completed
        ),
        format!("connections_failed = {}", iroh_metrics.connections_failed),
        format!("shutdown_aborts = {}", iroh_metrics.shutdown_aborts),
        format!("last_connection_path = {}", iroh_metrics.last_path_name()),
        format!("direct_connections = {}", iroh_metrics.direct_connections),
        format!("relay_connections = {}", iroh_metrics.relay_connections),
        format!("custom_connections = {}", iroh_metrics.custom_connections),
        format!("unknown_connections = {}", iroh_metrics.unknown_connections),
        "".to_string(),
        "[x11 forwarding]".to_string(),
        format!("route_active = {}", x11_metrics.route_active),
        format!(
            "authority_repair_pending = {}",
            x11_metrics.authority_repair_pending
        ),
        format!("active_streams = {}", x11_metrics.active_streams),
        format!("route_activations = {}", x11_metrics.route_activations),
        format!("route_deactivations = {}", x11_metrics.route_deactivations),
        format!("route_takeovers = {}", x11_metrics.route_takeovers),
        format!(
            "authority_publication_failures = {}",
            x11_metrics.authority_publication_failures
        ),
        format!("sockets_accepted = {}", x11_metrics.sockets_accepted),
        format!(
            "sockets_rejected_no_route = {}",
            x11_metrics.sockets_rejected_no_route
        ),
        format!(
            "sockets_rejected_capacity = {}",
            x11_metrics.sockets_rejected_capacity
        ),
        format!("streams_started = {}", x11_metrics.streams_started),
        format!("streams_completed = {}", x11_metrics.streams_completed),
        format!("streams_cancelled = {}", x11_metrics.streams_cancelled),
        format!("streams_failed = {}", x11_metrics.streams_failed),
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
        format!(
            "agent_streaming_projection_results = {}",
            runtime_metrics.agent_streaming_projection_results
        ),
        format!(
            "agent_streaming_projection_installs = {}",
            runtime_metrics.agent_streaming_projection_installs
        ),
        format!(
            "agent_streaming_projection_rejections = {}",
            runtime_metrics.agent_streaming_projection_rejections
        ),
        format!(
            "agent_streaming_projection_lineage_rejections = {}",
            runtime_metrics.agent_streaming_projection_lineage_rejections
        ),
        format!(
            "agent_presentation_decoded_cache_hits = {}",
            runtime_metrics.agent_presentation_decoded_cache_hits
        ),
        format!(
            "agent_presentation_decoded_cache_misses = {}",
            runtime_metrics.agent_presentation_decoded_cache_misses
        ),
        format!(
            "agent_presentation_snapshot_cache_hits = {}",
            runtime_metrics.agent_presentation_snapshot_cache_hits
        ),
        format!(
            "agent_presentation_snapshot_cache_misses = {}",
            runtime_metrics.agent_presentation_snapshot_cache_misses
        ),
        format!(
            "agent_presentation_replayed_entries = {}",
            runtime_metrics.agent_presentation_replayed_entries
        ),
        format!(
            "agent_presentation_cache_evictions = {}",
            runtime_metrics.agent_presentation_cache_evictions
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
            "last_stable_projection_sha256 = {}",
            runtime_metrics
                .last_stable_projection_sha256
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
            "last_provider_request_sha256 = {}",
            runtime_metrics
                .last_provider_request_sha256
                .as_deref()
                .unwrap_or("none")
        ),
        format!(
            "last_provider_request_bytes = {}",
            runtime_metrics
                .last_provider_request_bytes
                .map_or_else(|| "none".to_string(), |value| value.to_string())
        ),
        format!(
            "last_provider_request_continuity_category = {}",
            runtime_metrics
                .last_provider_request_continuity_category
                .as_deref()
                .unwrap_or("none")
        ),
        format!(
            "last_provider_request_continuity_message_index = {}",
            runtime_metrics
                .last_provider_request_continuity_message_index
                .map_or_else(|| "none".to_string(), |value| value.to_string())
        ),
        format!(
            "last_provider_request_common_message_prefix = {}",
            runtime_metrics
                .last_provider_request_common_message_prefix
                .map_or_else(|| "none".to_string(), |value| value.to_string())
        ),
        format!(
            "last_provider_request_common_component_prefix = {}",
            runtime_metrics
                .last_provider_request_common_component_prefix
                .map_or_else(|| "none".to_string(), |value| value.to_string())
        ),
        format!(
            "last_provider_request_messages_append_only = {}",
            runtime_metrics
                .last_provider_request_messages_append_only
                .map_or_else(|| "none".to_string(), |value| value.to_string())
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
            "provider_prompt_stable_projection_bytes",
            &runtime_metrics.provider_prompt_stable_projection_bytes,
        ),
        (
            "provider_request_shape_bytes",
            &runtime_metrics.provider_request_shape_bytes,
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
    for family in crate::host::async_runtime::AsyncRuntimeRequestFamily::ALL {
        let latency = metrics.request_latency(family);
        lines.extend(runtime_metrics_histogram_lines(
            &format!("actor_{}_queue_wait_ms", family.name()),
            &latency.queue_wait_ms,
        ));
        lines.extend(runtime_metrics_histogram_lines(
            &format!("actor_{}_handler_duration_ms", family.name()),
            &latency.handler_duration_ms,
        ));
    }
    for phase in crate::host::async_runtime::AsyncRuntimeLatencyPhase::ALL {
        lines.extend(runtime_metrics_histogram_lines(
            phase.name(),
            metrics.phase_latency(phase),
        ));
    }
    lines.join("\n")
}

/// Builds a privacy-safe live Iroh status table for the invoking client.
pub(crate) fn runtime_show_iroh_status_display(
    service: &RuntimeSessionService,
    client_id: &mez_core::ids::ClientId,
) -> String {
    let Some(status) = service
        .integration
        .remote_iroh_connection_quality(client_id)
    else {
        return [
            "# Iroh connection",
            "",
            "| Metric | Value | Detail |",
            "| --- | --- | --- |",
            "| State | unavailable | this client has no correlated live Iroh connection |",
            "| Codec | unavailable | no negotiated connection-local codec |",
            "| Compression | unavailable | no connection-local frame samples |",
            "| Render updates | unavailable | no connection-local render samples |",
            "| Quality | unknown | insufficient transport samples |",
        ]
        .join("\n");
    };

    let sample_age = status.sample_age();
    let (quality, reason) = iroh_connection_quality(status, sample_age);
    let compression = iroh_compression_effectiveness(status);
    [
        "# Iroh connection".to_string(),
        String::new(),
        "| Metric | Value | Detail |".to_string(),
        "| --- | --- | --- |".to_string(),
        format!(
            "| State | connected | connected for {}; sample {} old |",
            format_duration_millis(status.connected_millis),
            format_duration_millis(u64::try_from(sample_age.as_millis()).unwrap_or(u64::MAX))
        ),
        format!(
            "| Path | {} | selected transport path |",
            status.path_name()
        ),
        format!(
            "| RTT | {} | average {}; jitter {} |",
            format_micros(status.rtt_micros),
            format_micros(status.average_rtt_micros),
            format_micros(status.jitter_micros)
        ),
        format!(
            "| Traffic | ↓ {}/s · ↑ {}/s | totals ↓ {} · ↑ {} |",
            format_bytes(status.rx_bytes_per_second),
            format_bytes(status.tx_bytes_per_second),
            format_bytes(status.rx_bytes),
            format_bytes(status.tx_bytes)
        ),
        format!(
            "| Codec | {} | negotiated application-frame codec |",
            status.compression_codec.as_str()
        ),
        compression,
        format!(
            "| Render updates | snapshots {} · deltas {} · changed rows {} | selected wire {} · decoded {} · snapshot candidates {}; coalesced {} · suppressed {} · snapshot fallbacks {}; max ready depth {}; write wait {} total · {} max |",
            status.render_snapshot_frames,
            status.render_delta_frames,
            status.render_changed_rows,
            format_bytes(status.render_selected_wire_bytes),
            format_bytes(status.render_selected_decoded_bytes),
            format_bytes(status.render_snapshot_candidate_bytes),
            status.render_triggers_coalesced,
            status.render_updates_suppressed,
            status.render_snapshot_fallbacks,
            status.render_ready_depth_max,
            format_micros(status.render_write_wait_micros),
            format_micros(status.render_write_wait_max_micros),
        ),
        format!(
            "| Loss | {} packets | since the previous selected-path sample |",
            status.lost_packets
        ),
        format!(
            "| Congestion | {} events | cwnd {}; MTU {} |",
            status.congestion_events,
            format_bytes(status.cwnd_bytes),
            status.mtu
        ),
        format!("| Quality | {quality} | {reason} |"),
    ]
    .join("\n")
}

fn iroh_compression_effectiveness(
    status: crate::runtime::RuntimeIrohConnectionQualitySnapshot,
) -> String {
    let frames = status
        .compression_compressed_frames
        .saturating_add(status.compression_identity_frames);
    if frames == 0 || status.compression_decoded_bytes == 0 {
        return "| Compression | insufficient sample | no complete frames on this connection |"
            .to_string();
    }
    let ratio =
        status.compression_decoded_bytes as f64 / status.compression_wire_bytes.max(1) as f64;
    let saved = i128::from(status.compression_decoded_bytes)
        .saturating_sub(i128::from(status.compression_wire_bytes));
    let saved = if saved >= 0 {
        format!(
            "{} saved",
            format_bytes(u64::try_from(saved).unwrap_or(u64::MAX))
        )
    } else {
        format!(
            "{} expansion",
            format_bytes(u64::try_from(saved.saturating_abs()).unwrap_or(u64::MAX))
        )
    };
    format!(
        "| Compression | {ratio:.2}× · {saved} | connection wire {}; decoded {}; frames compressed {} · identity {} |",
        format_bytes(status.compression_wire_bytes),
        format_bytes(status.compression_decoded_bytes),
        status.compression_compressed_frames,
        status.compression_identity_frames
    )
}

fn iroh_connection_quality(
    status: crate::runtime::RuntimeIrohConnectionQualitySnapshot,
    sample_age: std::time::Duration,
) -> (&'static str, &'static str) {
    match crate::runtime::classify_runtime_iroh_connection_quality(
        status.rtt_micros,
        status.jitter_micros,
        status.lost_packets,
        status.congestion_events,
        sample_age,
    ) {
        crate::host::terminal::TerminalIrohStatusQuality::Good => {
            ("good", "stable RTT with no recent loss or congestion")
        }
        crate::host::terminal::TerminalIrohStatusQuality::Degraded => {
            ("degraded", "elevated RTT, jitter, loss, or congestion")
        }
        crate::host::terminal::TerminalIrohStatusQuality::Poor => {
            ("poor", "high RTT or sustained recent transport trouble")
        }
        crate::host::terminal::TerminalIrohStatusQuality::Unknown => {
            ("unknown", "transport sample is stale")
        }
    }
}

fn format_micros(micros: u64) -> String {
    if micros < 1_000 {
        format!("{micros} µs")
    } else {
        format!("{:.1} ms", micros as f64 / 1_000.0)
    }
}

fn format_duration_millis(millis: u64) -> String {
    if millis < 1_000 {
        format!("{millis} ms")
    } else if millis < 60_000 {
        format!("{:.1} s", millis as f64 / 1_000.0)
    } else {
        let seconds = millis / 1_000;
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    }
}

fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1_024;
    const MIB: u64 = KIB * 1_024;
    const GIB: u64 = MIB * 1_024;
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// Renders message rows through the shared Markdown table pager path.
fn runtime_show_messages_table(rows: Vec<String>) -> String {
    let mut lines = vec![
        "| kind | id | time | details |".to_string(),
        "| --- | --- | --- | --- |".to_string(),
    ];
    lines.extend(rows);
    lines.join("\n")
}

/// Renders one escaped message row.
fn show_messages_table_row(kind: &str, id: &str, time: &str, details: &str) -> String {
    format!(
        "| {} | {} | {} | {} |",
        show_messages_table_cell(kind),
        show_messages_table_cell(id),
        show_messages_table_cell(time),
        show_messages_table_cell(details)
    )
}

/// Escapes one message field for a Markdown table cell.
fn show_messages_table_cell(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace(['\r', '\n'], " ")
}
