//! Agent-provider worker scheduling, dispatch execution, recovery, and accounting.

use super::{
    ActionStatus, AgentActionPayload, AgentCompactionEvent, AgentId, AgentProviderEvent,
    AgentRememberEvent, AgentTurnExecution, AgentTurnLedger, AgentTurnRecord, AgentTurnRunner,
    AgentTurnState, AsyncAgentProviderPollReport, AsyncAgentProviderServiceConfig,
    AsyncModelProvider, AsyncRuntimeSessionHandle, ContextSourceKind, JoinSet, MezError,
    MezErrorKind, ModelMessage, ModelMessageRole, ModelProfile, ModelRequest, ModelResponse,
    ProviderErrorRetryClass, ReqwestProviderHttpTransport, Result, RuntimeAgentCompactionDispatch,
    RuntimeAgentProviderDispatch, RuntimeAgentProviderDispatchProvider,
    RuntimeAgentRememberDispatch, RuntimeEvent, RuntimeEventBatch, RuntimeLifecycleState,
    RuntimeSideEffect, execute_network_action_with_transport_async,
    is_terminal_runtime_lifecycle_state, provider_error_retry_class,
    runtime_execute_auto_sizing_with_async_provider, sleep, watch,
};
use crate::runtime::RuntimeAgentProviderWorkerOutcome;
use std::time::Duration;

/// Runs the run async agent provider service operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn run_async_agent_provider_service<F>(
    handle: &AsyncRuntimeSessionHandle,
    config: AsyncAgentProviderServiceConfig,
    mut should_stop: F,
) -> Result<AsyncAgentProviderPollReport>
where
    F: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    config.validate()?;
    let mut lifecycle = handle.lifecycle_state_watcher();
    let mut side_effect_watcher = handle.side_effect_delivery_watcher();
    let mut workers = JoinSet::new();
    let mut report = AsyncAgentProviderPollReport {
        polls: 0,
        executions: 0,
        idle_polls: 0,
        terminal_state: *lifecycle.borrow(),
    };

    loop {
        let state = *lifecycle.borrow();
        report.terminal_state = state;
        drain_completed_agent_provider_workers(&mut workers, handle, &mut report).await?;
        if should_stop(report.polls, state) {
            abort_agent_provider_workers(&mut workers).await;
            return Ok(report);
        }

        let dispatches = handle
            .drain_agent_provider_dispatch_side_effects(config.max_tasks_per_poll)
            .await?;
        if dispatches.is_empty() {
            handle
                .queue_provider_poll_timer_if_needed(
                    report.polls.saturating_add(1),
                    config.provider_poll_fallback_delay_ms(),
                )
                .await?;
        }
        if dispatches.is_empty() {
            report.idle_polls = report.idle_polls.saturating_add(1);
            report.polls = report.polls.saturating_add(1);
            if should_stop(report.polls, state) {
                abort_agent_provider_workers(&mut workers).await;
                return Ok(report);
            }
            if let Some(joined) = wait_for_agent_provider_worker_wakeup(
                handle,
                &mut workers,
                &mut lifecycle,
                &mut side_effect_watcher,
                config.idle_interval,
            )
            .await
            {
                record_joined_agent_provider_worker(joined, handle, &mut report).await?;
            }
        } else {
            dispatch_agent_provider_side_effects(handle, dispatches, &mut workers).await?;
            drain_completed_agent_provider_workers(&mut workers, handle, &mut report).await?;
            report.polls = report.polls.saturating_add(1);
        }
    }
}

type AsyncAgentProviderWorkerResult = Option<(RuntimeEvent, bool)>;

/// Drains provider workers that completed without blocking new dispatch claims.
async fn drain_completed_agent_provider_workers(
    workers: &mut JoinSet<Result<AsyncAgentProviderWorkerResult>>,
    handle: &AsyncRuntimeSessionHandle,
    report: &mut AsyncAgentProviderPollReport,
) -> Result<()> {
    while let Some(joined) = workers.try_join_next() {
        record_joined_agent_provider_worker(joined, handle, report).await?;
    }
    Ok(())
}

/// Records one completed provider worker and applies its runtime event.
async fn record_joined_agent_provider_worker(
    joined: std::result::Result<Result<AsyncAgentProviderWorkerResult>, tokio::task::JoinError>,
    handle: &AsyncRuntimeSessionHandle,
    report: &mut AsyncAgentProviderPollReport,
) -> Result<()> {
    match joined {
        Ok(Ok(Some((event, completed)))) => {
            if completed {
                report.executions = report.executions.saturating_add(1);
            }
            let mut batch = RuntimeEventBatch::new();
            batch.push(event);
            handle.submit_runtime_events(batch).await?;
            Ok(())
        }
        Ok(Ok(None)) => Ok(()),
        Ok(Err(error)) => Err(error),
        Err(error) if error.is_cancelled() => Ok(()),
        Err(error) => Err(MezError::invalid_state(format!(
            "async agent provider worker task failed: {error}"
        ))),
    }
}

/// Waits until provider work, actor events, lifecycle changes, or idle probing
/// should return control to the provider service loop.
async fn wait_for_agent_provider_worker_wakeup(
    handle: &AsyncRuntimeSessionHandle,
    workers: &mut JoinSet<Result<AsyncAgentProviderWorkerResult>>,
    lifecycle_watcher: &mut watch::Receiver<RuntimeLifecycleState>,
    side_effect_watcher: &mut watch::Receiver<u64>,
    idle_interval: Duration,
) -> Option<std::result::Result<Result<AsyncAgentProviderWorkerResult>, tokio::task::JoinError>> {
    if workers.is_empty() {
        tokio::select! {
            _ = handle.wait_for_event_delivery() => None,
            changed = side_effect_watcher.changed() => {
                let _ = changed;
                None
            }
            changed = lifecycle_watcher.changed() => {
                let _ = changed;
                None
            }
            _ = sleep(idle_interval) => None,
        }
    } else {
        tokio::select! {
            biased;
            joined = workers.join_next() => joined,
            _ = handle.wait_for_event_delivery() => None,
            changed = side_effect_watcher.changed() => {
                let _ = changed;
                None
            }
            changed = lifecycle_watcher.changed() => {
                let _ = changed;
                None
            }
            _ = sleep(idle_interval) => None,
        }
    }
}

/// Aborts provider workers before the provider service exits.
async fn abort_agent_provider_workers(
    workers: &mut JoinSet<Result<AsyncAgentProviderWorkerResult>>,
) {
    workers.abort_all();
    while workers.join_next().await.is_some() {}
}

/// Runs the dispatch agent provider side effects operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn dispatch_agent_provider_side_effects(
    handle: &AsyncRuntimeSessionHandle,
    dispatches: Vec<RuntimeSideEffect>,
    workers: &mut JoinSet<Result<AsyncAgentProviderWorkerResult>>,
) -> Result<()> {
    for dispatch in dispatches {
        match dispatch {
            RuntimeSideEffect::DispatchAgentProvider { agent_id, turn_id } => {
                let Some(dispatch) = handle
                    .claim_configured_agent_provider_task(agent_id.clone(), turn_id.clone())
                    .await?
                else {
                    continue;
                };
                workers.spawn(monitor_runtime_agent_provider_dispatch(
                    handle.clone(),
                    agent_id,
                    turn_id,
                    dispatch,
                ));
            }
            RuntimeSideEffect::DispatchAgentCompaction { pane_id } => {
                let dispatch = match handle.claim_agent_compaction_task(pane_id.clone()).await {
                    Ok(Some(dispatch)) => dispatch,
                    Ok(None) => continue,
                    Err(error) => {
                        let mut batch = RuntimeEventBatch::new();
                        batch.push(RuntimeEvent::AgentCompaction(
                            AgentCompactionEvent::Failed {
                                pane_id,
                                kind: provider_worker_error_kind(&error).to_string(),
                                message: error.message().to_string(),
                                provider_failure_json: error
                                    .provider_failure_json()
                                    .map(str::to_string),
                                provider_raw_text: error.provider_raw_text().map(str::to_string),
                            },
                        ));
                        handle.submit_runtime_events(batch).await?;
                        continue;
                    }
                };
                workers.spawn(monitor_runtime_agent_compaction_dispatch(
                    handle.clone(),
                    pane_id,
                    dispatch,
                ));
            }
            RuntimeSideEffect::DispatchAgentRemember { pane_id } => {
                let dispatch = match handle.claim_agent_remember_task(pane_id.clone()).await {
                    Ok(Some(dispatch)) => dispatch,
                    Ok(None) => continue,
                    Err(error) => {
                        let mut batch = RuntimeEventBatch::new();
                        batch.push(RuntimeEvent::AgentRemember(AgentRememberEvent::Failed {
                            pane_id,
                            kind: provider_worker_error_kind(&error).to_string(),
                            message: error.message().to_string(),
                            provider_failure_json: error
                                .provider_failure_json()
                                .map(str::to_string),
                            provider_raw_text: error.provider_raw_text().map(str::to_string),
                        }));
                        handle.submit_runtime_events(batch).await?;
                        continue;
                    }
                };
                workers.spawn(monitor_runtime_agent_remember_dispatch(
                    handle.clone(),
                    pane_id,
                    dispatch,
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Runs one provider request while honoring turn cancellation.
///
/// A provider task is claimed before it begins request serialization and
/// network work. Once claimed, `/stop` removes the turn from runtime state,
/// so the async side must drop the provider future instead of waiting for it
/// to finish and continue allocating memory for a cancelled turn.
async fn monitor_runtime_agent_provider_dispatch(
    handle: AsyncRuntimeSessionHandle,
    agent_id: AgentId,
    turn_id: String,
    dispatch: RuntimeAgentProviderDispatch,
) -> Result<AsyncAgentProviderWorkerResult> {
    let mut lifecycle = handle.lifecycle_state_watcher();
    let mut side_effect_watcher = handle.side_effect_delivery_watcher();
    if !handle.agent_turn_is_running(&turn_id).await? {
        return Ok(None);
    }
    let worker = execute_runtime_agent_provider_dispatch(dispatch, None);
    tokio::pin!(worker);
    loop {
        tokio::select! {
            result = &mut worker => {
                return Ok(Some(provider_worker_event(agent_id, turn_id, Ok(result))));
            }
            _ = handle.wait_for_event_delivery() => {}
            changed = side_effect_watcher.changed() => {
                let _ = changed;
            }
            changed = lifecycle.changed() => {
                if changed.is_err() {
                    return Ok(None);
                }
            }
        }
        let lifecycle_state = *lifecycle.borrow();
        if is_terminal_runtime_lifecycle_state(lifecycle_state)
            || !handle.agent_turn_is_running(&turn_id).await?
        {
            return Ok(None);
        }
    }
}

/// Runs one model-backed compaction worker while honoring shutdown.
async fn monitor_runtime_agent_compaction_dispatch(
    handle: AsyncRuntimeSessionHandle,
    pane_id: String,
    dispatch: RuntimeAgentCompactionDispatch,
) -> Result<AsyncAgentProviderWorkerResult> {
    let mut lifecycle = handle.lifecycle_state_watcher();
    let mut side_effect_watcher = handle.side_effect_delivery_watcher();
    let worker = execute_runtime_agent_compaction_dispatch(dispatch);
    tokio::pin!(worker);
    loop {
        tokio::select! {
            result = &mut worker => {
                return Ok(Some(compaction_worker_event(pane_id, Ok(result))));
            }
            _ = handle.wait_for_event_delivery() => {}
            changed = side_effect_watcher.changed() => {
                let _ = changed;
            }
            changed = lifecycle.changed() => {
                if changed.is_err() {
                    return Ok(None);
                }
            }
        }
        if is_terminal_runtime_lifecycle_state(*lifecycle.borrow()) {
            return Ok(None);
        }
    }
}

/// Runs one model-backed durable memory worker while honoring shutdown.
async fn monitor_runtime_agent_remember_dispatch(
    handle: AsyncRuntimeSessionHandle,
    pane_id: String,
    dispatch: RuntimeAgentRememberDispatch,
) -> Result<AsyncAgentProviderWorkerResult> {
    let mut lifecycle = handle.lifecycle_state_watcher();
    let mut side_effect_watcher = handle.side_effect_delivery_watcher();
    let worker = execute_runtime_agent_remember_dispatch(dispatch);
    tokio::pin!(worker);
    loop {
        tokio::select! {
            result = &mut worker => {
                return Ok(Some(remember_worker_event(pane_id, Ok(result))));
            }
            _ = handle.wait_for_event_delivery() => {}
            changed = side_effect_watcher.changed() => {
                let _ = changed;
            }
            changed = lifecycle.changed() => {
                if changed.is_err() {
                    return Ok(None);
                }
            }
        }
        if is_terminal_runtime_lifecycle_state(*lifecycle.borrow()) {
            return Ok(None);
        }
    }
}

/// Runs the provider worker event operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn provider_worker_event(
    agent_id: AgentId,
    turn_id: String,
    result: std::result::Result<Result<RuntimeAgentProviderWorkerOutcome>, tokio::task::JoinError>,
) -> (RuntimeEvent, bool) {
    match result {
        Ok(Ok(RuntimeAgentProviderWorkerOutcome::Execution(execution))) => (
            RuntimeEvent::AgentProvider(AgentProviderEvent::Completed {
                agent_id,
                turn_id,
                execution,
            }),
            true,
        ),
        Ok(Ok(RuntimeAgentProviderWorkerOutcome::RoutingSelected(selection))) => (
            RuntimeEvent::AgentProvider(AgentProviderEvent::RoutingSelected {
                agent_id,
                turn_id,
                selection,
            }),
            false,
        ),
        Ok(Err(error)) => (
            RuntimeEvent::AgentProvider(AgentProviderEvent::Failed {
                agent_id,
                turn_id,
                kind: provider_worker_error_kind(&error).to_string(),
                message: error.message().to_string(),
                provider_failure_json: error.provider_failure_json().map(str::to_string),
                provider_raw_text: error.provider_raw_text().map(str::to_string),
            }),
            false,
        ),
        Err(error) => (
            RuntimeEvent::AgentProvider(AgentProviderEvent::Failed {
                agent_id,
                turn_id,
                kind: "invalid_state".to_string(),
                message: format!("provider worker join failed: {error}"),
                provider_failure_json: None,
                provider_raw_text: None,
            }),
            false,
        ),
    }
}

/// Converts a compaction worker result into a runtime event.
fn compaction_worker_event(
    pane_id: String,
    result: std::result::Result<Result<mez_agent::ModelResponse>, tokio::task::JoinError>,
) -> (RuntimeEvent, bool) {
    match result {
        Ok(Ok(response)) => (
            RuntimeEvent::AgentCompaction(AgentCompactionEvent::Completed {
                pane_id,
                response: Box::new(response),
            }),
            true,
        ),
        Ok(Err(error)) => (
            RuntimeEvent::AgentCompaction(AgentCompactionEvent::Failed {
                pane_id,
                kind: provider_worker_error_kind(&error).to_string(),
                message: error.message().to_string(),
                provider_failure_json: error.provider_failure_json().map(str::to_string),
                provider_raw_text: error.provider_raw_text().map(str::to_string),
            }),
            false,
        ),
        Err(error) => (
            RuntimeEvent::AgentCompaction(AgentCompactionEvent::Failed {
                pane_id,
                kind: "invalid_state".to_string(),
                message: format!("provider worker join failed: {error}"),
                provider_failure_json: None,
                provider_raw_text: None,
            }),
            false,
        ),
    }
}

/// Converts a durable memory worker result into a runtime event.
fn remember_worker_event(
    pane_id: String,
    result: std::result::Result<Result<mez_agent::ModelResponse>, tokio::task::JoinError>,
) -> (RuntimeEvent, bool) {
    match result {
        Ok(Ok(response)) => (
            RuntimeEvent::AgentRemember(AgentRememberEvent::Completed {
                pane_id,
                response: Box::new(response),
            }),
            true,
        ),
        Ok(Err(error)) => (
            RuntimeEvent::AgentRemember(AgentRememberEvent::Failed {
                pane_id,
                kind: provider_worker_error_kind(&error).to_string(),
                message: error.message().to_string(),
                provider_failure_json: error.provider_failure_json().map(str::to_string),
                provider_raw_text: error.provider_raw_text().map(str::to_string),
            }),
            false,
        ),
        Err(error) => (
            RuntimeEvent::AgentRemember(AgentRememberEvent::Failed {
                pane_id,
                kind: "invalid_state".to_string(),
                message: format!("provider worker join failed: {error}"),
                provider_failure_json: None,
                provider_raw_text: None,
            }),
            false,
        ),
    }
}

/// Runs the execute runtime agent provider dispatch operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn execute_runtime_agent_provider_dispatch(
    dispatch: RuntimeAgentProviderDispatch,
    _output_progress_sender: Option<tokio::sync::mpsc::UnboundedSender<AgentProviderEvent>>,
) -> Result<RuntimeAgentProviderWorkerOutcome> {
    let RuntimeAgentProviderDispatch {
        turn,
        context,
        allowed_actions,
        interaction_kind,
        model_profile,
        macro_judge_request,
        sandbox_failure_assessment_request,
        auto_sizing,
        auto_sizing_provider,
        provider,
        permission_policy,
        sandbox_first_local_prompts,
        session_approvals,
        path_scopes,
        subagent_scope,
        available_mcp_servers,
        available_mcp_tools,
        memory_actions_enabled,
        issue_actions_enabled,
        loop_turn: _,
    } = dispatch;
    let context = context.into_agent_context();
    let routing_token_usage_by_model = std::collections::BTreeMap::new();
    if let Some(request) = sandbox_failure_assessment_request.or(macro_judge_request) {
        let response = match provider {
            RuntimeAgentProviderDispatchProvider::OpenAi(provider) => {
                provider.send_request_async(&request).await?
            }
            RuntimeAgentProviderDispatchProvider::Anthropic(provider) => {
                provider.send_request_async(&request).await?
            }
            RuntimeAgentProviderDispatchProvider::DeepSeek(provider) => {
                provider.send_request_async(&request).await?
            }
            RuntimeAgentProviderDispatchProvider::OpenAiCompatible(provider) => {
                provider.send_request_async(&request).await?
            }
        };
        return Ok(RuntimeAgentProviderWorkerOutcome::Execution(Box::new(
            super::AgentTurnExecution {
                request,
                response,
                latest_response_usage: Default::default(),
                routing_token_usage_by_model,
                action_results: Vec::new(),
                final_turn: false,
                terminal_state: AgentTurnState::Running,
            },
        )));
    }
    if let Some(auto_sizing) = auto_sizing.as_ref() {
        let auto_sizing_execution = if let Some(router_provider) = auto_sizing_provider.as_ref() {
            match router_provider {
                RuntimeAgentProviderDispatchProvider::OpenAi(provider) => {
                    runtime_execute_auto_sizing_with_async_provider(
                        provider,
                        auto_sizing,
                        &turn,
                        &context,
                    )
                    .await?
                }
                RuntimeAgentProviderDispatchProvider::DeepSeek(provider) => {
                    runtime_execute_auto_sizing_with_async_provider(
                        provider,
                        auto_sizing,
                        &turn,
                        &context,
                    )
                    .await?
                }
                RuntimeAgentProviderDispatchProvider::Anthropic(provider) => {
                    runtime_execute_auto_sizing_with_async_provider(
                        provider,
                        auto_sizing,
                        &turn,
                        &context,
                    )
                    .await?
                }
                RuntimeAgentProviderDispatchProvider::OpenAiCompatible(provider) => {
                    runtime_execute_auto_sizing_with_async_provider(
                        provider,
                        auto_sizing,
                        &turn,
                        &context,
                    )
                    .await?
                }
            }
        } else if auto_sizing.router_profile.provider == provider.provider_id() {
            match &provider {
                RuntimeAgentProviderDispatchProvider::OpenAi(provider) => {
                    runtime_execute_auto_sizing_with_async_provider(
                        provider,
                        auto_sizing,
                        &turn,
                        &context,
                    )
                    .await?
                }
                RuntimeAgentProviderDispatchProvider::DeepSeek(provider) => {
                    runtime_execute_auto_sizing_with_async_provider(
                        provider,
                        auto_sizing,
                        &turn,
                        &context,
                    )
                    .await?
                }
                RuntimeAgentProviderDispatchProvider::Anthropic(provider) => {
                    runtime_execute_auto_sizing_with_async_provider(
                        provider,
                        auto_sizing,
                        &turn,
                        &context,
                    )
                    .await?
                }
                RuntimeAgentProviderDispatchProvider::OpenAiCompatible(provider) => {
                    runtime_execute_auto_sizing_with_async_provider(
                        provider,
                        auto_sizing,
                        &turn,
                        &context,
                    )
                    .await?
                }
            }
        } else {
            return Err(MezError::invalid_state(format!(
                "auto-sizing router provider `{}` is unavailable",
                auto_sizing.router_profile.provider
            )));
        };
        return Ok(RuntimeAgentProviderWorkerOutcome::RoutingSelected(
            Box::new(auto_sizing_execution.into_routing_selection()),
        ));
    }
    match provider {
        RuntimeAgentProviderDispatchProvider::OpenAi(provider) => {
            let mut ledger = AgentTurnLedger::new(false);
            let runner = AgentTurnRunner {
                provider: &provider,
                model_profile,
                permissions: &crate::security::permissions::ProductPermissionPlanning::new(
                    &permission_policy,
                    &session_approvals,
                    path_scopes.as_ref(),
                )
                .with_sandbox_first_local_prompts(sandbox_first_local_prompts),
                subagent_scope: subagent_scope.as_ref(),
                subagent_scope_enforcement: &mez_agent::DEFAULT_SUBAGENT_SCOPE_ENFORCEMENT,
                available_mcp_servers,
                available_mcp_tools: &available_mcp_tools,
                memory_actions_enabled,
                issue_actions_enabled,
            };
            let execution = runner
                .run_turn_async_ref_with_allowed_actions(
                    &mut ledger,
                    turn.clone(),
                    &context,
                    allowed_actions.clone(),
                    interaction_kind,
                )
                .await?;
            let mut execution = execute_provider_worker_network_actions(&turn, execution).await?;
            execution.routing_token_usage_by_model = routing_token_usage_by_model;
            Ok(RuntimeAgentProviderWorkerOutcome::Execution(Box::new(
                execution,
            )))
        }
        RuntimeAgentProviderDispatchProvider::Anthropic(provider) => {
            let mut ledger = AgentTurnLedger::new(false);
            let runner = AgentTurnRunner {
                provider: &provider,
                model_profile,
                permissions: &crate::security::permissions::ProductPermissionPlanning::new(
                    &permission_policy,
                    &session_approvals,
                    path_scopes.as_ref(),
                )
                .with_sandbox_first_local_prompts(sandbox_first_local_prompts),
                subagent_scope: subagent_scope.as_ref(),
                subagent_scope_enforcement: &mez_agent::DEFAULT_SUBAGENT_SCOPE_ENFORCEMENT,
                available_mcp_servers,
                available_mcp_tools: &available_mcp_tools,
                memory_actions_enabled,
                issue_actions_enabled,
            };
            let execution = runner
                .run_turn_async_ref_with_allowed_actions(
                    &mut ledger,
                    turn.clone(),
                    &context,
                    allowed_actions.clone(),
                    interaction_kind,
                )
                .await?;
            let mut execution = execute_provider_worker_network_actions(&turn, execution).await?;
            execution.routing_token_usage_by_model = routing_token_usage_by_model;
            Ok(RuntimeAgentProviderWorkerOutcome::Execution(Box::new(
                execution,
            )))
        }
        RuntimeAgentProviderDispatchProvider::DeepSeek(provider) => {
            let mut ledger = AgentTurnLedger::new(false);
            let runner = AgentTurnRunner {
                provider: &provider,
                model_profile,
                permissions: &crate::security::permissions::ProductPermissionPlanning::new(
                    &permission_policy,
                    &session_approvals,
                    path_scopes.as_ref(),
                )
                .with_sandbox_first_local_prompts(sandbox_first_local_prompts),
                subagent_scope: subagent_scope.as_ref(),
                subagent_scope_enforcement: &mez_agent::DEFAULT_SUBAGENT_SCOPE_ENFORCEMENT,
                available_mcp_servers,
                available_mcp_tools: &available_mcp_tools,
                memory_actions_enabled,
                issue_actions_enabled,
            };
            let execution = runner
                .run_turn_async_ref_with_allowed_actions(
                    &mut ledger,
                    turn.clone(),
                    &context,
                    allowed_actions.clone(),
                    interaction_kind,
                )
                .await?;
            let mut execution = execute_provider_worker_network_actions(&turn, execution).await?;
            execution.routing_token_usage_by_model = routing_token_usage_by_model;
            Ok(RuntimeAgentProviderWorkerOutcome::Execution(Box::new(
                execution,
            )))
        }
        RuntimeAgentProviderDispatchProvider::OpenAiCompatible(provider) => {
            let mut ledger = AgentTurnLedger::new(false);
            let runner = AgentTurnRunner {
                provider: &provider,
                model_profile,
                permissions: &crate::security::permissions::ProductPermissionPlanning::new(
                    &permission_policy,
                    &session_approvals,
                    path_scopes.as_ref(),
                )
                .with_sandbox_first_local_prompts(sandbox_first_local_prompts),
                subagent_scope: subagent_scope.as_ref(),
                subagent_scope_enforcement: &mez_agent::DEFAULT_SUBAGENT_SCOPE_ENFORCEMENT,
                available_mcp_servers,
                available_mcp_tools: &available_mcp_tools,
                memory_actions_enabled,
                issue_actions_enabled,
            };
            let execution = runner
                .run_turn_async_ref_with_allowed_actions(
                    &mut ledger,
                    turn.clone(),
                    &context,
                    allowed_actions.clone(),
                    interaction_kind,
                )
                .await?;
            let mut execution = execute_provider_worker_network_actions(&turn, execution).await?;
            execution.routing_token_usage_by_model = routing_token_usage_by_model;
            Ok(RuntimeAgentProviderWorkerOutcome::Execution(Box::new(
                execution,
            )))
        }
    }
}

/// Executes runtime-owned network actions before returning provider work to the
/// actor.
///
/// Provider workers already run outside the single-owner session actor. Keeping
/// `fetch_url` and `web_search` HTTP there prevents a large research batch from
/// monopolizing the actor while still returning ordinary action results for the
/// actor to present, audit, persist, and feed into any continuation request.
pub(in crate::host::async_runtime) async fn execute_provider_worker_network_actions(
    turn: &AgentTurnRecord,
    mut execution: AgentTurnExecution,
) -> Result<AgentTurnExecution> {
    if execution.terminal_state != AgentTurnState::Running {
        return Ok(execution);
    }
    let Some(batch) = execution.response.action_batch.clone() else {
        return Ok(execution);
    };
    let transport = ReqwestProviderHttpTransport;
    for index in 0..execution.action_results.len() {
        if execution.action_results[index].status != ActionStatus::Running
            || !matches!(
                execution.action_results[index].action_type,
                "web_search" | "fetch_url"
            )
        {
            continue;
        }
        let action_id = execution.action_results[index].action_id.clone();
        let action = batch
            .actions
            .iter()
            .find(|action| action.id == action_id)
            .cloned()
            .ok_or_else(|| {
                MezError::invalid_state("running network result does not match an action")
            })?;
        if !matches!(
            action.payload,
            AgentActionPayload::WebSearch { .. } | AgentActionPayload::FetchUrl { .. }
        ) {
            continue;
        }
        execution.action_results[index] =
            execute_network_action_with_transport_async(turn, &action, &transport).await?;
    }
    execution.terminal_state =
        mez_agent::turn_state_from_action_results(&execution.action_results, execution.final_turn);
    Ok(execution)
}

/// Executes one model-backed conversation compaction request.
async fn execute_runtime_agent_compaction_dispatch(
    dispatch: RuntimeAgentCompactionDispatch,
) -> Result<mez_agent::ModelResponse> {
    let RuntimeAgentCompactionDispatch { task, provider } = dispatch;
    match provider {
        RuntimeAgentProviderDispatchProvider::OpenAi(provider) => {
            runtime_send_compaction_request_with_output_limit_retry(
                &provider,
                task.request,
                &task.model_profile,
            )
            .await
        }
        RuntimeAgentProviderDispatchProvider::DeepSeek(provider) => {
            runtime_send_compaction_request_with_output_limit_retry(
                &provider,
                task.request,
                &task.model_profile,
            )
            .await
        }
        RuntimeAgentProviderDispatchProvider::Anthropic(provider) => {
            runtime_send_compaction_request_with_output_limit_retry(
                &provider,
                task.request,
                &task.model_profile,
            )
            .await
        }
        RuntimeAgentProviderDispatchProvider::OpenAiCompatible(provider) => {
            runtime_send_compaction_request_with_output_limit_retry(
                &provider,
                task.request,
                &task.model_profile,
            )
            .await
        }
    }
}

/// Executes one model-backed durable memory generation request.
async fn execute_runtime_agent_remember_dispatch(
    dispatch: RuntimeAgentRememberDispatch,
) -> Result<mez_agent::ModelResponse> {
    let RuntimeAgentRememberDispatch { task, provider } = dispatch;
    match provider {
        RuntimeAgentProviderDispatchProvider::OpenAi(provider) => {
            provider.send_request_async(&task.request).await
        }
        RuntimeAgentProviderDispatchProvider::Anthropic(provider) => {
            provider.send_request_async(&task.request).await
        }
        RuntimeAgentProviderDispatchProvider::DeepSeek(provider) => {
            provider.send_request_async(&task.request).await
        }
        RuntimeAgentProviderDispatchProvider::OpenAiCompatible(provider) => {
            provider.send_request_async(&task.request).await
        }
    }
}

/// Sends a model compaction request and retries once with stricter output
/// guidance when the provider cuts off generation at its output-token limit.
async fn runtime_send_compaction_request_with_output_limit_retry<P: AsyncModelProvider>(
    provider: &P,
    mut request: ModelRequest,
    model_profile: &ModelProfile,
) -> Result<ModelResponse> {
    match provider.send_request_async(&request).await {
        Ok(response) => Ok(response),
        Err(error)
            if matches!(
                provider_error_retry_class(&error),
                ProviderErrorRetryClass::OutputLimit
            ) =>
        {
            request =
                runtime_agent_compaction_request_with_output_limit_retry(request, model_profile);
            request.messages.push(ModelMessage {
                role: ModelMessageRole::Developer,
                source: ContextSourceKind::Configuration,
                placement: mez_agent::ContextPlacement::StablePrefix,
                content: "[ephemeral compaction output-limit retry]\n\
                    The previous compaction response was incomplete because generation hit max_output_tokens. \
                    Return exactly one final say action containing a compact durable summary. \
                    Keep the summary brief: preserve only active goals, decisions, changed files, validations, blockers, and next steps. \
                    Do not include full logs, transcript excerpts, plans, or explanations. \
                    This retry instruction is not durable transcript or future-turn context."
                    .to_string(),
            });
            provider.send_request_async(&request).await
        }
        Err(error) => Err(error),
    }
}

/// Returns a compaction request with an escalated output cap for any retry.
fn runtime_agent_compaction_request_with_output_limit_retry(
    mut request: ModelRequest,
    model_profile: &ModelProfile,
) -> ModelRequest {
    request.max_output_tokens = Some(model_profile.output_limit_retry_tokens());
    request
}

/// Runs the provider worker error kind operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn provider_worker_error_kind(error: &MezError) -> &'static str {
    match error.kind() {
        MezErrorKind::InvalidArgs => "invalid_args",
        MezErrorKind::InvalidState => "invalid_state",
        MezErrorKind::Config => "config",
        MezErrorKind::Io => "io",
        MezErrorKind::Conflict => "conflict",
        MezErrorKind::NotFound => "not_found",
        MezErrorKind::Forbidden => "forbidden",
        MezErrorKind::NotImplemented => "not_implemented",
    }
}
