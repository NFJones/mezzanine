//! Public asynchronous handle API for the serialized runtime actor.

use super::{
    AgentId, AsyncControlInputResult, AsyncMessageFanout, AsyncMessageInputResult,
    AsyncRenderedClientFrame, AsyncRuntimeRequest, AsyncRuntimeSessionHandle,
    AttachedClientStepApplication, AttachedTerminalClientStepPlan, ClientId, ClientViewRole,
    ControlConnectionState, DeliveryCursor, FanoutBatch, MessageConnection, MezError,
    PaneResizeUpdate, Result, RuntimeAgentProviderDispatch, RuntimeEventBatch,
    RuntimeEventConnectionTable, RuntimeEventIngressReport, RuntimeEventWakeup,
    RuntimeLifecycleState, RuntimeSideEffect, Size, TerminalClientLoopConfig, oneshot, watch,
};
#[cfg(test)]
use super::{
    AsyncRenderedClientFlush, ClientStatusLine, RenderedClientView, RuntimeAgentProviderTask,
};

impl AsyncRuntimeSessionHandle {
    /// Runs the lifecycle state operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn lifecycle_state(&self) -> Result<RuntimeLifecycleState> {
        self.request(|reply| AsyncRuntimeRequest::LifecycleState { reply })
            .await
    }

    /// Returns a watch receiver for actor-owned lifecycle state changes.
    ///
    /// Long-running socket services keep one receiver for their whole loop so
    /// they cannot miss a transition that occurs between a state check and an
    /// awaited socket read or accept.
    pub fn lifecycle_state_watcher(&self) -> watch::Receiver<RuntimeLifecycleState> {
        self.lifecycle_state_rx.clone()
    }

    /// Returns actor metrics captured at the serialized runtime boundary.
    #[cfg(test)]
    pub async fn metrics(&self) -> Result<crate::host::async_runtime::AsyncRuntimeActorMetrics> {
        self.request(|reply| AsyncRuntimeRequest::Metrics { reply })
            .await
    }

    /// Runs the render client view operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub async fn render_client_view(
        &self,
        role: ClientViewRole,
        client_size: Size,
        config: TerminalClientLoopConfig,
    ) -> Result<Option<RenderedClientView>> {
        self.request(|reply| AsyncRuntimeRequest::RenderClientView {
            role,
            client_size,
            config,
            reply,
        })
        .await?
    }

    /// Runs the render client frame operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn render_client_frame(
        &self,
        role: ClientViewRole,
        client_size: Size,
        config: TerminalClientLoopConfig,
        render: bool,
    ) -> Result<AsyncRenderedClientFrame> {
        self.request(|reply| AsyncRuntimeRequest::RenderClientFrame {
            role,
            client_size,
            config,
            render,
            reply,
        })
        .await?
    }

    /// Runs the render client side effect operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub async fn render_client_side_effect(
        &self,
        client_id: ClientId,
        config: TerminalClientLoopConfig,
        status: Option<ClientStatusLine>,
        cursor_blink_elapsed_ms: u64,
    ) -> Result<Option<AsyncRenderedClientFlush>> {
        self.request(|reply| AsyncRuntimeRequest::RenderClientSideEffect {
            client_id,
            config,
            status,
            cursor_blink_elapsed_ms,
            reply,
        })
        .await?
    }

    /// Runs the ensure client render timers operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn ensure_client_render_timers(&self, client_id: ClientId) -> Result<usize> {
        self.request(|reply| AsyncRuntimeRequest::EnsureClientRenderTimers { client_id, reply })
            .await?
    }

    /// Runs the terminal client loop config operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn terminal_client_loop_config(
        &self,
        config: TerminalClientLoopConfig,
    ) -> Result<TerminalClientLoopConfig> {
        self.request(|reply| AsyncRuntimeRequest::TerminalClientLoopConfig { config, reply })
            .await?
    }

    /// Runs the handle control input for connection operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn handle_control_input_for_connection(
        &self,
        input: Vec<u8>,
        max_content_length: usize,
        connection: ControlConnectionState,
    ) -> Result<AsyncControlInputResult> {
        self.request(|reply| AsyncRuntimeRequest::HandleControlInput {
            input,
            max_content_length,
            connection,
            reply,
        })
        .await?
    }

    /// Runs the handle control input for connection with snapshots operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn handle_control_input_for_connection_with_snapshots(
        &self,
        input: Vec<u8>,
        max_content_length: usize,
        connection: ControlConnectionState,
        snapshots: crate::storage::snapshot::SnapshotRepository,
    ) -> Result<AsyncControlInputResult> {
        self.request(
            |reply| AsyncRuntimeRequest::HandleControlInputWithSnapshots {
                input,
                max_content_length,
                connection,
                snapshots,
                reply,
            },
        )
        .await?
    }

    /// Runs the handle message input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn handle_message_input(
        &self,
        input: Vec<u8>,
        max_content_length: usize,
        connection: MessageConnection,
        now_ms: u64,
    ) -> Result<AsyncMessageInputResult> {
        self.request(|reply| AsyncRuntimeRequest::HandleMessageInput {
            input,
            max_content_length,
            connection,
            now_ms,
            reply,
        })
        .await?
    }

    /// Runs the message fanout ready for operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn message_fanout_ready_for(
        &self,
        recipient: AgentId,
        now_ms: u64,
        limit: usize,
    ) -> Result<Option<AsyncMessageFanout>> {
        self.request(|reply| AsyncRuntimeRequest::MessageFanoutReadyFor {
            recipient,
            now_ms,
            limit,
            reply,
        })
        .await?
    }

    /// Runs the acknowledge message fanout operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn acknowledge_message_fanout(&self, batch: FanoutBatch) -> Result<DeliveryCursor> {
        self.request(|reply| AsyncRuntimeRequest::AcknowledgeMessageFanout { batch, reply })
            .await?
    }

    /// Runs the wait for message delivery operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn wait_for_message_delivery(&self) {
        self.message_delivery_notify.notified().await;
    }

    /// Runs the wait for event delivery operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn wait_for_event_delivery(&self) {
        self.event_delivery_notify.notified().await;
    }

    /// Waits until the actor queues at least one runtime side effect.
    #[cfg(test)]
    pub async fn wait_for_runtime_side_effects(&self) {
        self.side_effect_delivery_notify.notified().await;
    }

    /// Returns a non-consuming side-effect delivery revision watcher.
    pub fn side_effect_delivery_watcher(&self) -> watch::Receiver<u64> {
        self.side_effect_delivery_rx.clone()
    }

    /// Runs the event wakeups operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn event_wakeups(
        &self,
        connections: RuntimeEventConnectionTable,
        limit_per_connection: usize,
    ) -> Result<Vec<RuntimeEventWakeup>> {
        self.request(|reply| AsyncRuntimeRequest::EventWakeups {
            connections,
            limit_per_connection,
            reply,
        })
        .await?
    }

    /// Runs the apply attached terminal step plan operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn apply_attached_terminal_step_plan(
        &self,
        primary_client_id: ClientId,
        step: AttachedTerminalClientStepPlan,
    ) -> Result<AttachedClientStepApplication> {
        self.request(|reply| AsyncRuntimeRequest::ApplyAttachedTerminalStep {
            primary_client_id,
            step,
            reply,
        })
        .await?
    }

    /// Runs the resize attached primary terminal operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn resize_attached_primary_terminal(
        &self,
        primary_client_id: ClientId,
        size: Size,
    ) -> Result<Vec<PaneResizeUpdate>> {
        self.request(|reply| AsyncRuntimeRequest::ResizeAttachedPrimaryTerminal {
            primary_client_id,
            size,
            reply,
        })
        .await?
    }

    /// Runs the execute terminal command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub async fn execute_terminal_command(
        &self,
        primary_client_id: ClientId,
        input: String,
    ) -> Result<String> {
        self.request(|reply| AsyncRuntimeRequest::ExecuteTerminalCommand {
            primary_client_id,
            input,
            reply,
        })
        .await?
    }
    /// Refreshes cached provider metadata through actor-owned runtime state.
    pub async fn refresh_provider_info(&self) -> Result<String> {
        self.request(|reply| AsyncRuntimeRequest::RefreshProviderInfo { reply })
            .await?
    }

    /// Shows a primary-client modal display overlay through actor-owned state.
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "test-only adapter retained for focused boundary coverage"
    )]
    pub async fn show_primary_display_overlay(&self, lines: Vec<String>) -> Result<()> {
        self.request(|reply| AsyncRuntimeRequest::ShowPrimaryDisplayOverlay { lines, reply })
            .await?
    }

    /// Shows a primary-client recoverable error overlay through actor-owned state.
    pub async fn show_primary_error_overlay(&self, lines: Vec<String>) -> Result<()> {
        self.request(|reply| AsyncRuntimeRequest::ShowPrimaryErrorOverlay { lines, reply })
            .await?
    }

    /// Runs the execute agent shell command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub async fn execute_agent_shell_command(
        &self,
        primary_client_id: ClientId,
        input: String,
    ) -> Result<String> {
        self.request(|reply| AsyncRuntimeRequest::ExecuteAgentShellCommand {
            primary_client_id,
            input,
            reply,
        })
        .await?
    }

    /// Runs the pending agent provider tasks operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub async fn pending_agent_provider_tasks(&self) -> Result<Vec<RuntimeAgentProviderTask>> {
        self.request(|reply| AsyncRuntimeRequest::PendingAgentProviderTasks { reply })
            .await?
    }

    /// Checks whether a provider worker should continue waiting for a turn.
    pub async fn agent_turn_is_running(&self, turn_id: &str) -> Result<bool> {
        let turn_id = turn_id.to_string();
        self.request(|reply| AsyncRuntimeRequest::AgentTurnIsRunning { turn_id, reply })
            .await?
    }

    /// Queues a provider-poll timer when pending provider work exists and no
    /// provider-poll generation is already scheduled.
    pub async fn queue_provider_poll_timer_if_needed(
        &self,
        generation: u64,
        delay_ms: u64,
    ) -> Result<bool> {
        self.request(
            |reply| AsyncRuntimeRequest::QueueProviderPollTimerIfNeeded {
                generation,
                delay_ms,
                reply,
            },
        )
        .await?
    }

    /// Runs the claim configured agent provider task operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn claim_configured_agent_provider_task(
        &self,
        agent_id: AgentId,
        turn_id: String,
    ) -> Result<Option<RuntimeAgentProviderDispatch>> {
        self.request(
            |reply| AsyncRuntimeRequest::ClaimConfiguredAgentProviderTask {
                agent_id,
                turn_id,
                reply,
            },
        )
        .await?
    }

    /// Claims one queued model-backed compaction task for async execution.
    pub async fn claim_agent_compaction_task(
        &self,
        pane_id: String,
    ) -> Result<Option<crate::runtime::RuntimeAgentCompactionDispatch>> {
        self.request(|reply| AsyncRuntimeRequest::ClaimAgentCompactionTask { pane_id, reply })
            .await?
    }

    /// Claims one queued model-backed durable memory task for async execution.
    pub async fn claim_agent_remember_task(
        &self,
        pane_id: String,
    ) -> Result<Option<crate::runtime::RuntimeAgentRememberDispatch>> {
        self.request(|reply| AsyncRuntimeRequest::ClaimAgentRememberTask { pane_id, reply })
            .await?
    }

    /// Runs the submit runtime events operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn submit_runtime_events(
        &self,
        batch: RuntimeEventBatch,
    ) -> Result<RuntimeEventIngressReport> {
        self.request(|reply| AsyncRuntimeRequest::SubmitRuntimeEvents { batch, reply })
            .await?
    }

    /// Drains queued actor side effects for supervised external adapters.
    ///
    /// The returned effects are already ordered by the runtime events that
    /// produced them. A zero limit is rejected so callers cannot accidentally
    /// spin while making no progress.
    #[cfg(test)]
    pub async fn drain_runtime_side_effects(&self, limit: usize) -> Result<Vec<RuntimeSideEffect>> {
        self.request(|reply| AsyncRuntimeRequest::DrainRuntimeSideEffects { limit, reply })
            .await?
    }

    /// Runs the queue runtime side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn queue_runtime_side_effects(
        &self,
        side_effects: Vec<RuntimeSideEffect>,
    ) -> Result<usize> {
        self.request(|reply| AsyncRuntimeRequest::QueueRuntimeSideEffects {
            side_effects,
            reply,
        })
        .await?
    }

    /// Runs the drain agent provider dispatch side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn drain_agent_provider_dispatch_side_effects(
        &self,
        limit: usize,
    ) -> Result<Vec<RuntimeSideEffect>> {
        self.request(
            |reply| AsyncRuntimeRequest::DrainAgentProviderDispatchSideEffects { limit, reply },
        )
        .await?
    }

    /// Runs the drain render side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub async fn drain_render_side_effects(&self, limit: usize) -> Result<Vec<RuntimeSideEffect>> {
        self.request(|reply| AsyncRuntimeRequest::DrainRenderSideEffects { limit, reply })
            .await?
    }

    /// Runs the drain render side effects for client operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn drain_render_side_effects_for_client(
        &self,
        client_id: ClientId,
        limit: usize,
    ) -> Result<Vec<RuntimeSideEffect>> {
        self.request(
            |reply| AsyncRuntimeRequest::DrainRenderSideEffectsForClient {
                client_id,
                limit,
                reply,
            },
        )
        .await?
    }

    /// Runs the drain client output flush side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn drain_client_output_flush_side_effects(
        &self,
        client_id: Option<ClientId>,
        limit: usize,
    ) -> Result<Vec<RuntimeSideEffect>> {
        self.request(
            |reply| AsyncRuntimeRequest::DrainClientOutputFlushSideEffects {
                client_id,
                limit,
                reply,
            },
        )
        .await?
    }

    /// Runs the drain timer side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn drain_timer_side_effects(&self, limit: usize) -> Result<Vec<RuntimeSideEffect>> {
        self.request(|reply| AsyncRuntimeRequest::DrainTimerSideEffects { limit, reply })
            .await?
    }

    /// Runs the drain persistence side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn drain_persistence_side_effects(
        &self,
        limit: usize,
    ) -> Result<Vec<RuntimeSideEffect>> {
        self.request(|reply| AsyncRuntimeRequest::DrainPersistenceSideEffects { limit, reply })
            .await?
    }

    /// Runs the drain hook side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn drain_hook_side_effects(&self, limit: usize) -> Result<Vec<RuntimeSideEffect>> {
        self.request(|reply| AsyncRuntimeRequest::DrainHookSideEffects { limit, reply })
            .await?
    }

    /// Runs the drain pane io side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn drain_pane_io_side_effects(
        &self,
        pane_id: impl Into<String>,
        limit: usize,
    ) -> Result<Vec<RuntimeSideEffect>> {
        let pane_id = pane_id.into();
        self.request(|reply| AsyncRuntimeRequest::DrainPaneIoSideEffects {
            pane_id,
            limit,
            reply,
        })
        .await?
    }

    /// Moves running pane process handles out of the serialized runtime owner
    /// so external pane process adapters can own PTY I/O.
    pub async fn take_running_pane_processes_for_adapter(
        &self,
        limit: usize,
    ) -> Result<Vec<(String, crate::host::async_runtime::PaneProcess)>> {
        self.request(
            |reply| AsyncRuntimeRequest::TakeRunningPaneProcessesForAdapter { limit, reply },
        )
        .await?
    }

    /// Runs the shutdown operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn shutdown(&self) -> Result<RuntimeLifecycleState> {
        self.request(|reply| AsyncRuntimeRequest::Shutdown { reply })
            .await
    }

    /// Runs the request operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) async fn request<T>(
        &self,
        build_request: impl FnOnce(oneshot::Sender<T>) -> AsyncRuntimeRequest,
    ) -> Result<T> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(build_request(reply))
            .await
            .map_err(|_| MezError::invalid_state("async runtime session actor is closed"))?;
        response
            .await
            .map_err(|_| MezError::invalid_state("async runtime session actor reply was dropped"))
    }
}
