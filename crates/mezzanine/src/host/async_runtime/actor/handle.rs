//! Public asynchronous handle API for the serialized runtime actor.

use super::{
    AgentId, AsyncControlInputResult, AsyncIrohRenderSnapshot, AsyncMessageFanout,
    AsyncMessageInputResult, AsyncRenderedClientFrame, AsyncRuntimeRequest,
    AsyncRuntimeRequestEnvelope, AsyncRuntimeSessionHandle, AsyncTerminalClientConfigInput,
    AsyncTerminalClientConfigSnapshot, AttachedClientStepApplication,
    AttachedTerminalClientStepPlan, ClientClipboardRouteCleanup, ClientClipboardRouteLease,
    ClientId, ClientViewRole, ControlConnectionState, DeliveryCursor, FanoutBatch,
    MessageConnection, MezError, PaneResizeUpdate, Result, RuntimeAgentProviderDispatch,
    RuntimeApprovedExternalActionDispatch, RuntimeApprovedExternalActionOutcome, RuntimeEventBatch,
    RuntimeEventIngressReport, RuntimeEventWakeup, RuntimeLifecycleState, RuntimeSideEffect, Size,
    TerminalClientLoopConfig, oneshot, watch,
};
#[cfg(test)]
use super::{
    AsyncRenderedClientFlush, ClientStatusLine, RenderedClientView, RuntimeAgentProviderTask,
    RuntimeEventConnectionTable,
};
use crate::host::async_runtime::actor_types::AsyncClientRenderToken;
use crate::runtime::RuntimeNativeShellDispatch;

impl AsyncRuntimeSessionHandle {
    /// Installs the privacy-safe diagnostics populated by a host-routed Iroh transport.
    pub(crate) async fn set_host_routed_iroh_diagnostics(
        &self,
        diagnostics: crate::runtime::RuntimeIrohDiagnostics,
    ) -> Result<()> {
        self.request(|reply| AsyncRuntimeRequest::SetHostRoutedIrohDiagnostics {
            diagnostics,
            reply,
        })
        .await
    }

    /// Runs the lifecycle state operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn lifecycle_state(&self) -> Result<RuntimeLifecycleState> {
        self.request(|reply| AsyncRuntimeRequest::LifecycleState { reply })
            .await
    }

    /// Captures and persists one host-admin checkpoint from serialized actor state.
    pub(crate) async fn create_host_checkpoint(
        &self,
        snapshots: crate::storage::snapshot::SnapshotRepository,
        snapshot_id: String,
        name: Option<String>,
    ) -> Result<crate::storage::snapshot::SnapshotState> {
        self.request(|reply| AsyncRuntimeRequest::CreateHostCheckpoint {
            snapshots,
            snapshot_id,
            name,
            reply,
        })
        .await?
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

    /// Best-effort records one fixed worker-phase latency observation.
    pub(crate) fn record_latency_phase(
        &self,
        phase: crate::host::async_runtime::AsyncRuntimeLatencyPhase,
        elapsed_ms: u64,
    ) {
        let _ = self.sender.try_send(AsyncRuntimeRequestEnvelope::new(
            AsyncRuntimeRequest::RecordLatencyPhase { phase, elapsed_ms },
        ));
    }

    /// Sends bytes directly to a process-owned pane without visible-surface routing.
    #[cfg(test)]
    pub async fn write_input_to_pane(
        &self,
        primary_client_id: ClientId,
        pane_id: &str,
        input: Vec<u8>,
    ) -> Result<crate::runtime::PaneInputDispatch> {
        let pane_id = pane_id.to_string();
        self.request(|reply| AsyncRuntimeRequest::WriteInputToPane {
            primary_client_id,
            pane_id,
            input,
            reply,
        })
        .await?
    }

    /// Returns managed-shell child and parent-restoration state for boundary tests.
    #[cfg(test)]
    pub async fn managed_shell_lifecycle_state(&self, pane_id: &str) -> Result<(bool, bool, bool)> {
        let pane_id = pane_id.to_string();
        self.request(|reply| AsyncRuntimeRequest::ManagedShellLifecycleState { pane_id, reply })
            .await
    }

    /// Returns semantic pane bootstrap-certification state for boundary tests.
    #[cfg(test)]
    pub async fn pane_certification_snapshot(
        &self,
        pane_id: &str,
    ) -> Result<crate::host::async_runtime::AsyncPaneCertificationSnapshot> {
        let pane_id = pane_id.to_string();
        self.request(|reply| AsyncRuntimeRequest::PaneCertificationSnapshot { pane_id, reply })
            .await
    }

    /// Returns the retained process presentation during a managed-shell handoff.
    #[cfg(test)]
    pub async fn managed_shell_process_screen_text(&self, pane_id: &str) -> Result<String> {
        let pane_id = pane_id.to_string();
        self.request(|reply| AsyncRuntimeRequest::ManagedShellProcessScreenText { pane_id, reply })
            .await
    }

    /// Reports whether the current Zsh process installed its managed trigger.
    #[cfg(test)]
    pub async fn managed_zsh_admission_ready(&self, pane_id: &str) -> Result<bool> {
        let pane_id = pane_id.to_string();
        self.request(|reply| AsyncRuntimeRequest::ManagedZshAdmissionReady { pane_id, reply })
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
        client_id: ClientId,
        role: ClientViewRole,
        client_size: Size,
        config: TerminalClientLoopConfig,
        render: bool,
    ) -> Result<AsyncRenderedClientFrame> {
        self.request(|reply| AsyncRuntimeRequest::RenderClientFrame {
            client_id,
            role,
            client_size,
            config: AsyncTerminalClientConfigInput::Raw(Box::new(config)),
            render,
            reply,
        })
        .await?
    }

    /// Captures one authoritative exact-client snapshot for an Iroh v3 stream.
    pub(crate) async fn render_iroh_client_snapshot(
        &self,
        client_id: ClientId,
        invalidate_output: bool,
    ) -> Result<Option<AsyncIrohRenderSnapshot>> {
        self.request(|reply| AsyncRuntimeRequest::RenderIrohClientSnapshot {
            client_id,
            invalidate_output,
            reply,
        })
        .await?
    }

    /// Renders from an actor-resolved snapshot, refreshing stale generations.
    pub(in crate::host::async_runtime) async fn render_client_frame_with_snapshot(
        &self,
        client_id: ClientId,
        role: ClientViewRole,
        client_size: Size,
        config: AsyncTerminalClientConfigSnapshot,
        render: bool,
    ) -> Result<AsyncRenderedClientFrame> {
        self.request(|reply| AsyncRuntimeRequest::RenderClientFrame {
            client_id,
            role,
            client_size,
            config: AsyncTerminalClientConfigInput::Snapshot(config),
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

    /// Completes one prepared interactive provider refresh and fences actor ordering.
    #[cfg(test)]
    pub async fn complete_agent_prompt_provider_info_refresh_for_tests(
        &self,
        refresh: crate::runtime::RuntimeAgentPromptProviderInfoRefresh,
        outcome: crate::runtime::RuntimeProviderInfoRefreshOutcome,
    ) -> Result<()> {
        self.sender
            .send(AsyncRuntimeRequestEnvelope::new(
                AsyncRuntimeRequest::CompleteAgentPromptProviderInfoRefresh { refresh, outcome },
            ))
            .await
            .map_err(|_| MezError::invalid_state("async runtime session actor is closed"))?;
        let _ = self.lifecycle_state().await?;
        Ok(())
    }

    /// Runs the terminal client loop config operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "test-only compatibility API is exercised by selected async runtime suites"
    )]
    pub async fn terminal_client_loop_config(
        &self,
        config: TerminalClientLoopConfig,
    ) -> Result<TerminalClientLoopConfig> {
        self.resolve_terminal_client_loop_config(config)
            .await
            .map(|snapshot| snapshot.config().clone())
    }

    /// Resolves raw terminal configuration into one shared actor snapshot.
    pub(in crate::host::async_runtime) async fn resolve_terminal_client_loop_config(
        &self,
        config: TerminalClientLoopConfig,
    ) -> Result<AsyncTerminalClientConfigSnapshot> {
        self.request(
            |reply| AsyncRuntimeRequest::TerminalClientLoopConfigSnapshot {
                config: AsyncTerminalClientConfigInput::Raw(Box::new(config)),
                reply,
            },
        )
        .await?
    }

    /// Reuses a current snapshot locally or asks the actor to refresh it.
    pub(in crate::host::async_runtime) async fn refresh_terminal_client_loop_config(
        &self,
        snapshot: AsyncTerminalClientConfigSnapshot,
    ) -> Result<AsyncTerminalClientConfigSnapshot> {
        if *self.terminal_config_generation_rx.borrow() == snapshot.generation() {
            return Ok(snapshot);
        }
        self.request(
            |reply| AsyncRuntimeRequest::TerminalClientLoopConfigSnapshot {
                config: AsyncTerminalClientConfigInput::Snapshot(snapshot),
                reply,
            },
        )
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
                output_prefix: Vec::new(),
                consumed_prefix: 0,
                record_metrics: true,
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

    /// Returns an independent durable event-delivery revision watcher.
    ///
    /// Long-lived consumers should create this watcher before querying event
    /// state, mark the current revision before each query, and await `changed`
    /// only after an empty result. Unlike the compatibility notification port,
    /// one consumer cannot take another consumer's pending wakeup.
    pub fn event_delivery_watcher(&self) -> watch::Receiver<u64> {
        self.event_delivery_revision_rx.clone()
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
    #[cfg(test)]
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

    /// Returns one bounded event batch after revalidating the live session client.
    pub async fn event_wakeups_for_client(
        &self,
        caller_client_id: ClientId,
        connection_id: String,
        last_delivered_event_id: u64,
        limit_per_connection: usize,
    ) -> Result<Vec<RuntimeEventWakeup>> {
        self.request(|reply| AsyncRuntimeRequest::EventWakeupsForClient {
            caller_client_id,
            connection_id,
            last_delivered_event_id,
            limit_per_connection,
            reply,
        })
        .await?
    }

    /// Registers one exact authenticated Iroh v2 primary effect route.
    pub(crate) async fn register_client_clipboard_route(
        &self,
        client_id: ClientId,
    ) -> Result<ClientClipboardRouteLease> {
        let generation = self
            .request(|reply| AsyncRuntimeRequest::RegisterClientClipboardRoute {
                client_id: client_id.clone(),
                reply,
            })
            .await?;
        Ok(ClientClipboardRouteLease {
            handle: self.clone(),
            client_id,
            generation,
            armed: true,
        })
    }

    /// Removes one exact route and clears any unsent clipboard payload.
    async fn unregister_client_clipboard_route(
        &self,
        client_id: ClientId,
        generation: u64,
    ) -> Result<bool> {
        self.request(
            |reply| AsyncRuntimeRequest::UnregisterClientClipboardRoute {
                client_id,
                generation,
                reply,
            },
        )
        .await
    }

    /// Coalesces one bounded clipboard payload for an exact live route.
    #[cfg(test)]
    pub(crate) async fn enqueue_client_clipboard_write(
        &self,
        client_id: ClientId,
        content: String,
    ) -> Result<bool> {
        self.request(|reply| AsyncRuntimeRequest::EnqueueClientClipboardWrite {
            client_id,
            content,
            reply,
        })
        .await
    }

    /// Takes one pending write from an exact connection-local route.
    pub(crate) async fn take_client_clipboard_write(
        &self,
        client_id: ClientId,
        generation: u64,
    ) -> Result<Option<crate::runtime::ClientClipboardWrite>> {
        self.request(|reply| AsyncRuntimeRequest::TakeClientClipboardWrite {
            client_id,
            generation,
            reply,
        })
        .await
    }

    /// Consumes one short-lived Unix event binding for the authenticated peer.
    pub async fn consume_unix_event_binding(
        &self,
        token: String,
        peer_uid: u32,
    ) -> Result<ClientId> {
        self.request(|reply| AsyncRuntimeRequest::ConsumeUnixEventBinding {
            token,
            peer_uid,
            reply,
        })
        .await?
    }

    /// Runs the apply attached terminal step plan operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub async fn apply_attached_terminal_step_plan(
        &self,
        primary_client_id: ClientId,
        step: AttachedTerminalClientStepPlan,
    ) -> Result<AttachedClientStepApplication> {
        self.request(|reply| AsyncRuntimeRequest::ApplyAttachedTerminalStep {
            primary_client_id,
            render_token: None,
            step,
            reply,
        })
        .await?
    }

    /// Applies a terminal step fenced by the exact frame that produced it.
    pub(in crate::host::async_runtime) async fn apply_attached_terminal_step_plan_for_frame(
        &self,
        primary_client_id: ClientId,
        render_token: Option<AsyncClientRenderToken>,
        step: AttachedTerminalClientStepPlan,
    ) -> Result<AttachedClientStepApplication> {
        self.request(|reply| AsyncRuntimeRequest::ApplyAttachedTerminalStep {
            primary_client_id,
            render_token,
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
        let preparation = self
            .request(|reply| AsyncRuntimeRequest::PrepareConfiguredAgentProviderTask { reply })
            .await??;
        let preparation =
            super::RuntimeSessionService::execute_agent_provider_preparation(preparation).await;
        self.request(
            |reply| AsyncRuntimeRequest::ClaimConfiguredAgentProviderTask {
                agent_id,
                turn_id,
                preparation,
                reply,
            },
        )
        .await?
    }

    /// Claims one approved network or MCP action for worker execution.
    pub async fn claim_approved_external_action(
        &self,
        turn_id: String,
        action_id: String,
    ) -> Result<Option<RuntimeApprovedExternalActionDispatch>> {
        self.request(|reply| AsyncRuntimeRequest::ClaimApprovedExternalAction {
            turn_id,
            action_id,
            reply,
        })
        .await?
    }

    /// Claims one authorized native shell action for worker execution.
    pub async fn claim_native_shell_action(
        &self,
        turn_id: String,
        action_id: String,
    ) -> Result<Option<RuntimeNativeShellDispatch>> {
        self.request(|reply| AsyncRuntimeRequest::ClaimNativeShellAction {
            turn_id,
            action_id,
            reply,
        })
        .await?
    }

    /// Returns one approved external-action result to actor-owned state.
    pub async fn complete_approved_external_action(
        &self,
        outcome: RuntimeApprovedExternalActionOutcome,
    ) -> Result<bool> {
        self.request(|reply| AsyncRuntimeRequest::CompleteApprovedExternalAction { outcome, reply })
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

    /// Captures an immutable cumulative streaming generation for off-actor projection.
    pub async fn take_streaming_say_projection_work(
        &self,
        pane_id: String,
        turn_id: String,
    ) -> Result<Option<crate::runtime::RuntimeStreamingSayProjectionWork>> {
        self.request(
            |reply| AsyncRuntimeRequest::TakeStreamingSayProjectionWork {
                pane_id,
                turn_id,
                reply,
            },
        )
        .await?
    }

    /// Installs an atomic projection only when its captured generation is current.
    pub async fn apply_streaming_say_projection(
        &self,
        result: crate::runtime::RuntimeStreamingSayProjectionResult,
    ) -> Result<bool> {
        self.request(|reply| AsyncRuntimeRequest::ApplyStreamingSayProjection { result, reply })
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

    /// Drains bounded host-clipboard reads for the supervised external worker.
    pub async fn drain_host_clipboard_side_effects(
        &self,
        limit: usize,
    ) -> Result<Vec<RuntimeSideEffect>> {
        self.request(|reply| AsyncRuntimeRequest::DrainHostClipboardSideEffects { limit, reply })
            .await?
    }

    /// Drains command-backed status-pill refreshes for the supervised worker.
    pub async fn drain_status_pill_side_effects(
        &self,
        limit: usize,
    ) -> Result<Vec<RuntimeSideEffect>> {
        self.request(|reply| AsyncRuntimeRequest::DrainStatusPillSideEffects { limit, reply })
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

    /// Drains pane I/O effects targeted to one exact process instance.
    pub async fn drain_pane_process_io_side_effects(
        &self,
        instance: crate::runtime::PaneProcessInstance,
        limit: usize,
    ) -> Result<Vec<RuntimeSideEffect>> {
        self.request(|reply| AsyncRuntimeRequest::DrainPaneProcessIoSideEffects {
            instance,
            limit,
            reply,
        })
        .await?
    }

    /// Moves running pane process handles out of the serialized runtime owner
    /// so external pane process adapters can own PTY I/O.
    pub async fn take_running_pane_process_instances_for_adapter(
        &self,
        limit: usize,
    ) -> Result<
        Vec<(
            crate::runtime::PaneProcessInstance,
            crate::host::async_runtime::PaneProcess,
        )>,
    > {
        self.request(
            |reply| AsyncRuntimeRequest::TakeRunningPaneProcessesForAdapter { limit, reply },
        )
        .await?
    }

    /// Moves running pane processes while retaining the legacy pane-id-only
    /// handoff shape used by compatibility tests.
    #[cfg(test)]
    pub async fn take_running_pane_processes_for_adapter(
        &self,
        limit: usize,
    ) -> Result<Vec<(String, crate::host::async_runtime::PaneProcess)>> {
        self.take_running_pane_process_instances_for_adapter(limit)
            .await
            .map(|processes| {
                processes
                    .into_iter()
                    .map(|(instance, process)| (instance.pane_id, process))
                    .collect()
            })
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
            .send(AsyncRuntimeRequestEnvelope::new(build_request(reply)))
            .await
            .map_err(|_| MezError::invalid_state("async runtime session actor is closed"))?;
        response
            .await
            .map_err(|_| MezError::invalid_state("async runtime session actor reply was dropped"))
    }
}

impl ClientClipboardRouteLease {
    /// Returns the generation fencing this route ownership lifetime.
    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }

    /// Removes this route through the serialized actor and disarms Drop.
    pub(crate) async fn close(mut self) -> Result<bool> {
        let removed = self
            .handle
            .unregister_client_clipboard_route(self.client_id.clone(), self.generation)
            .await?;
        self.armed = false;
        Ok(removed)
    }
}

impl Drop for ClientClipboardRouteLease {
    fn drop(&mut self) {
        if self.armed {
            let _ =
                self.handle
                    .client_clipboard_route_cleanup_tx
                    .send(ClientClipboardRouteCleanup {
                        client_id: self.client_id.clone(),
                        generation: self.generation,
                    });
        }
    }
}
