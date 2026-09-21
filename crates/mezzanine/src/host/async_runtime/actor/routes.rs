//! Typed ownership for runtime side-effect delivery.

use crate::host::async_runtime::VecDeque;
use crate::runtime::{PaneProcessInstance, RenderInvalidationReason, RuntimeSideEffect};
use std::collections::{BTreeMap, HashMap};

/// Dedicated worker-owned work that can be drained without inspecting the
/// compatibility queue used by unrelated runtime services.
#[derive(Debug, Default)]
pub(crate) struct RuntimeSideEffectRouter {
    clipboard: VecDeque<RuntimeSideEffect>,
    hooks: VecDeque<RuntimeSideEffect>,
    render_order: VecDeque<crate::host::async_runtime::ClientId>,
    renders: HashMap<crate::host::async_runtime::ClientId, RenderInvalidationReason>,
    flush_order: VecDeque<crate::host::async_runtime::ClientId>,
    flushes: HashMap<crate::host::async_runtime::ClientId, RuntimeSideEffect>,
    pane_processes: BTreeMap<PaneProcessInstance, VecDeque<RuntimeSideEffect>>,
    persistence: VecDeque<RuntimeSideEffect>,
    commands: VecDeque<RuntimeSideEffect>,
    provider: VecDeque<RuntimeSideEffect>,
    status: VecDeque<RuntimeSideEffect>,
    timers: VecDeque<RuntimeSideEffect>,
}

impl RuntimeSideEffectRouter {
    /// Returns the number of worker-owned clipboard reads waiting for delivery.
    pub(super) fn len(&self) -> usize {
        self.clipboard
            .len()
            .saturating_add(self.hooks.len())
            .saturating_add(self.renders.len())
            .saturating_add(self.flushes.len())
            .saturating_add(
                self.pane_processes
                    .values()
                    .map(VecDeque::len)
                    .sum::<usize>(),
            )
            .saturating_add(self.persistence.len())
            .saturating_add(self.commands.len())
            .saturating_add(self.provider.len())
            .saturating_add(self.status.len())
            .saturating_add(self.timers.len())
    }

    /// Returns whether no dedicated worker-owned work remains.
    pub(super) fn is_empty(&self) -> bool {
        self.clipboard.is_empty()
            && self.hooks.is_empty()
            && self.renders.is_empty()
            && self.flushes.is_empty()
            && self.pane_processes.values().all(VecDeque::is_empty)
            && self.persistence.is_empty()
            && self.commands.is_empty()
            && self.provider.is_empty()
            && self.status.is_empty()
            && self.timers.is_empty()
    }

    /// Summarizes every routed family for aggregate capacity diagnostics.
    pub(super) fn kind_summary(&self) -> String {
        let mut counts = BTreeMap::<&'static str, usize>::new();
        let mut record = |effect: &RuntimeSideEffect| {
            let kind = super::coalesce::runtime_side_effect_kind(effect);
            *counts.entry(kind).or_default() = counts
                .get(kind)
                .copied()
                .unwrap_or_default()
                .saturating_add(1);
        };
        for lane in [
            &self.clipboard,
            &self.commands,
            &self.hooks,
            &self.persistence,
            &self.provider,
            &self.status,
            &self.timers,
        ] {
            for effect in lane {
                record(effect);
            }
        }
        for lane in self.pane_processes.values() {
            for effect in lane {
                record(effect);
            }
        }
        if !self.renders.is_empty() {
            counts.insert("render-client", self.renders.len());
        }
        if !self.flushes.is_empty() {
            counts.insert("flush-client-output", self.flushes.len());
        }
        if counts.is_empty() {
            return "none".to_string();
        }
        counts
            .into_iter()
            .map(|(kind, count)| format!("{kind}:{count}"))
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Enqueues one bounded host clipboard read for its sole owning worker.
    pub(super) fn push_clipboard(&mut self, effect: RuntimeSideEffect) {
        debug_assert!(matches!(
            effect,
            RuntimeSideEffect::ReadHostClipboard { .. }
        ));
        self.clipboard.push_back(effect);
    }

    /// Enqueues one program hook for its sole owning worker.
    pub(super) fn push_hook(&mut self, effect: RuntimeSideEffect) {
        debug_assert!(matches!(effect, RuntimeSideEffect::RunProgramHook { .. }));
        self.hooks.push_back(effect);
    }

    /// Coalesces one render invalidation into the newest pending request for
    /// the exact client while preserving first-admission order between clients.
    pub(super) fn push_render(
        &mut self,
        client_id: crate::host::async_runtime::ClientId,
        reason: RenderInvalidationReason,
    ) -> bool {
        if let Some(queued_reason) = self.renders.get_mut(&client_id) {
            *queued_reason =
                super::coalesce::coalesce_render_invalidation_reason(*queued_reason, reason);
            return true;
        }
        self.render_order.push_back(client_id.clone());
        self.renders.insert(client_id, reason);
        false
    }

    /// Replaces a pending full client-output frame for the exact client while
    /// retaining the client's original worker-admission position.
    pub(super) fn push_flush(&mut self, effect: RuntimeSideEffect) -> bool {
        let RuntimeSideEffect::FlushClientOutput { client_id, .. } = &effect else {
            debug_assert!(false, "flush route received a non-flush effect");
            return false;
        };
        let client_id = client_id.clone();
        if self.flushes.insert(client_id.clone(), effect).is_some() {
            return true;
        }
        self.flush_order.push_back(client_id);
        false
    }

    /// Merges an incoming render invalidation only when this client already
    /// owns a pending routed request. Same-batch coalescing remains with the
    /// enqueue transaction so rejected work is not admitted prematurely.
    pub(super) fn coalesce_pending_render(
        &mut self,
        client_id: &crate::host::async_runtime::ClientId,
        reason: RenderInvalidationReason,
    ) -> bool {
        let Some(queued_reason) = self.renders.get_mut(client_id) else {
            return false;
        };
        *queued_reason =
            super::coalesce::coalesce_render_invalidation_reason(*queued_reason, reason);
        true
    }

    /// Returns whether one exact client has a pending render invalidation.
    pub(super) fn has_pending_render(
        &self,
        client_id: &crate::host::async_runtime::ClientId,
    ) -> bool {
        self.renders.contains_key(client_id)
    }

    /// Replaces an incoming full frame only when this client already owns a
    /// pending routed frame. The replacement is safe because full frames are
    /// level-triggered presentation snapshots.
    pub(super) fn coalesce_pending_flush(&mut self, effect: RuntimeSideEffect) -> bool {
        let RuntimeSideEffect::FlushClientOutput { client_id, .. } = &effect else {
            debug_assert!(false, "flush coalescing received a non-flush effect");
            return false;
        };
        let Some(queued) = self.flushes.get_mut(client_id) else {
            return false;
        };
        *queued = effect;
        true
    }

    /// Replaces a pending registry update for the same registry session while
    /// preserving its original persistence-worker admission position.
    pub(super) fn coalesce_pending_registry(
        &mut self,
        registry: &crate::storage::registry::SessionRegistry,
        session_id: &str,
        effect: &mut Option<RuntimeSideEffect>,
    ) -> bool {
        self.persistence.iter_mut().any(|queued_effect| {
            let RuntimeSideEffect::PersistRegistry {
                registry: queued_registry,
                update,
            } = queued_effect
            else {
                return false;
            };
            if queued_registry != registry
                || super::coalesce::registry_update_session_id(update) != session_id
            {
                return false;
            }
            let Some(replacement) = effect.take() else {
                return false;
            };
            *queued_effect = replacement;
            true
        })
    }

    /// Removes one pending repaint request so bounded overflow can retain the
    /// existing compensation contract after client work moved out of the
    /// compatibility queue.
    pub(super) fn pop_droppable_repaint(&mut self) -> Option<RuntimeSideEffect> {
        if let Some(client_id) = self.render_order.pop_front()
            && let Some(reason) = self.renders.remove(&client_id)
        {
            return Some(RuntimeSideEffect::RenderClient { client_id, reason });
        }
        if let Some(client_id) = self.flush_order.pop_front() {
            return self.flushes.remove(&client_id);
        }
        None
    }

    /// Enqueues one exact-process operation in its owning worker's FIFO.
    pub(super) fn push_pane_process(&mut self, effect: RuntimeSideEffect) {
        let RuntimeSideEffect::PaneProcessIo {
            instance,
            effect: pane_effect,
        } = &effect
        else {
            debug_assert!(false, "exact pane route received a non-pane effect");
            return;
        };
        let queue = self.pane_processes.entry(instance.clone()).or_default();
        if matches!(
            pane_effect,
            crate::runtime::PaneProcessIoEffect::WriteShellInput { delivery } if delivery.priority
        ) {
            queue.push_front(effect);
        } else {
            queue.push_back(effect);
        }
    }

    /// Cancels one exact-process shell delivery before it reaches its worker.
    pub(super) fn cancel_pane_process_shell_input(
        &mut self,
        instance: &PaneProcessInstance,
        delivery_id: &str,
        effect: RuntimeSideEffect,
    ) {
        let queue = self.pane_processes.entry(instance.clone()).or_default();
        queue.retain(|queued| {
            !matches!(
                queued,
                RuntimeSideEffect::PaneProcessIo {
                    instance: queued_instance,
                    effect: crate::runtime::PaneProcessIoEffect::WriteShellInput { delivery },
                } if queued_instance == instance
                    && delivery.delivery_id.as_deref() == Some(delivery_id)
            )
        });
        queue.push_front(effect);
    }

    /// Takes one exact-process FIFO for actor-owned lease arbitration.
    pub(super) fn take_pane_process(
        &mut self,
        instance: &PaneProcessInstance,
    ) -> VecDeque<RuntimeSideEffect> {
        self.pane_processes.remove(instance).unwrap_or_default()
    }

    /// Takes every exact-process queue for one legacy pane adapter.
    ///
    /// The legacy adapter has no process generation, so its compatibility
    /// drain must still observe all generations for the requested pane.
    pub(super) fn take_pane_processes_for_pane(
        &mut self,
        pane_id: &str,
    ) -> VecDeque<RuntimeSideEffect> {
        let instances = self
            .pane_processes
            .keys()
            .filter(|instance| instance.pane_id == pane_id)
            .cloned()
            .collect::<Vec<_>>();
        let mut effects = VecDeque::new();
        for instance in instances {
            effects.extend(self.take_pane_process(&instance));
        }
        effects
    }

    /// Restores retained exact-process work after actor-owned arbitration.
    pub(super) fn restore_pane_process(
        &mut self,
        instance: PaneProcessInstance,
        effects: VecDeque<RuntimeSideEffect>,
    ) {
        if !effects.is_empty() {
            self.pane_processes.insert(instance, effects);
        }
    }

    /// Enqueues one durable persistence operation for its sole owning worker.
    pub(super) fn push_persistence(&mut self, effect: RuntimeSideEffect) {
        debug_assert!(matches!(
            effect,
            RuntimeSideEffect::Persist { .. }
                | RuntimeSideEffect::PersistAuditLog { .. }
                | RuntimeSideEffect::PersistTranscriptEntries { .. }
                | RuntimeSideEffect::PersistAgentSessionMetadata { .. }
                | RuntimeSideEffect::PersistPresentationEntries { .. }
                | RuntimeSideEffect::PersistSessionArchive { .. }
                | RuntimeSideEffect::PersistSavedSessionRetention { .. }
                | RuntimeSideEffect::PersistPromptHistory { .. }
                | RuntimeSideEffect::PersistCommandPromptHistory { .. }
                | RuntimeSideEffect::PersistTokenUsage { .. }
                | RuntimeSideEffect::SettleAgentProviderPersistence { .. }
                | RuntimeSideEffect::PersistRegistry { .. }
        ));
        if let RuntimeSideEffect::PersistAgentSessionMetadata {
            mezzanine_session_id,
            generation,
            ..
        } = &effect
            && let Some((position, queued_generation)) = self
                .persistence
                .iter()
                .enumerate()
                .find_map(|(position, queued)| match queued {
                    RuntimeSideEffect::PersistAgentSessionMetadata {
                        mezzanine_session_id: queued_session_id,
                        generation: queued_generation,
                        ..
                    } if queued_session_id == mezzanine_session_id => {
                        Some((position, *queued_generation))
                    }
                    _ => None,
                })
        {
            if queued_generation >= *generation {
                return;
            }
            self.persistence.remove(position);
        }
        self.persistence.push_back(effect);
    }

    /// Enqueues one deferred interactive command for its dedicated worker.
    pub(super) fn push_command(&mut self, effect: RuntimeSideEffect) {
        debug_assert!(matches!(
            effect,
            RuntimeSideEffect::DispatchAgentCommand { .. }
        ));
        self.commands.push_back(effect);
    }

    /// Enqueues one provider-service dispatch for its sole owning worker.
    pub(super) fn push_provider(&mut self, effect: RuntimeSideEffect) {
        debug_assert!(matches!(
            effect,
            RuntimeSideEffect::DispatchAgentProvider { .. }
                | RuntimeSideEffect::DispatchApprovedExternalAction { .. }
                | RuntimeSideEffect::DispatchNativeShellAction { .. }
                | RuntimeSideEffect::DispatchAgentCompaction { .. }
                | RuntimeSideEffect::DispatchAgentRemember { .. }
                | RuntimeSideEffect::DispatchAgentSessionTitle { .. }
                | RuntimeSideEffect::DispatchAgentPresentationResize { .. }
        ));
        self.provider.push_back(effect);
    }

    /// Enqueues one status refresh or provider-preparation request for the
    /// dedicated status worker.
    pub(super) fn push_status(&mut self, effect: RuntimeSideEffect) {
        debug_assert!(matches!(
            effect,
            RuntimeSideEffect::RefreshStatusPill { .. }
                | RuntimeSideEffect::PreparePaneStatusProviders
                | RuntimeSideEffect::RefreshPaneStatusProvider { .. }
        ));
        self.status.push_back(effect);
    }

    /// Enqueues one timer schedule or cancellation in its original actor
    /// emission order for the dedicated timer worker.
    pub(super) fn push_timer(&mut self, effect: RuntimeSideEffect) {
        debug_assert!(matches!(
            effect,
            RuntimeSideEffect::ScheduleTimer { .. } | RuntimeSideEffect::CancelTimer { .. }
        ));
        self.timers.push_back(effect);
    }

    /// Drains bounded clipboard work in FIFO order without scanning unrelated
    /// render, pane, provider, persistence, timer, hook, or status effects.
    pub(super) fn drain_clipboard(&mut self, limit: usize) -> Vec<RuntimeSideEffect> {
        self.clipboard
            .drain(..limit.min(self.clipboard.len()))
            .collect()
    }

    /// Drains bounded program hooks in enqueue order without inspecting other
    /// worker-owned or compatibility-queue effects.
    pub(super) fn drain_hooks(&mut self, limit: usize) -> Vec<RuntimeSideEffect> {
        self.hooks.drain(..limit.min(self.hooks.len())).collect()
    }

    /// Drains bounded persistence work in enqueue order without inspecting
    /// clipboard, hooks, or compatibility-queue effects.
    pub(super) fn drain_persistence(&mut self, limit: usize) -> Vec<RuntimeSideEffect> {
        self.persistence
            .drain(..limit.min(self.persistence.len()))
            .collect()
    }

    /// Drains deferred interactive commands without inspecting provider work.
    pub(super) fn drain_commands(&mut self, limit: usize) -> Vec<RuntimeSideEffect> {
        self.commands
            .drain(..limit.min(self.commands.len()))
            .collect()
    }

    /// Drains provider-service work in FIFO order without inspecting unrelated
    /// clipboard, hook, persistence, status, timer, or compatibility effects.
    pub(super) fn drain_provider(&mut self, limit: usize) -> Vec<RuntimeSideEffect> {
        self.provider
            .drain(..limit.min(self.provider.len()))
            .collect()
    }

    /// Drains coalesced client render invalidations in first-admission order.
    pub(super) fn drain_renders(&mut self, limit: usize) -> Vec<RuntimeSideEffect> {
        let mut drained = Vec::new();
        while drained.len() < limit {
            let Some(client_id) = self.render_order.pop_front() else {
                break;
            };
            if let Some(reason) = self.renders.remove(&client_id) {
                drained.push(RuntimeSideEffect::RenderClient { client_id, reason });
            }
        }
        drained
    }

    /// Drains one client's coalesced render invalidation without inspecting
    /// other client routes.
    pub(super) fn drain_render_for_client(
        &mut self,
        client_id: &crate::host::async_runtime::ClientId,
    ) -> Option<RuntimeSideEffect> {
        let reason = self.renders.remove(client_id)?;
        self.render_order.retain(|queued| queued != client_id);
        Some(RuntimeSideEffect::RenderClient {
            client_id: client_id.clone(),
            reason,
        })
    }

    /// Drains latest complete output frames for either every client or one
    /// exact client without scanning unrelated side-effect families.
    pub(super) fn drain_flushes(
        &mut self,
        client_id: Option<&crate::host::async_runtime::ClientId>,
        limit: usize,
    ) -> Vec<RuntimeSideEffect> {
        let mut drained = Vec::new();
        if let Some(client_id) = client_id {
            if limit > 0
                && let Some(effect) = self.flushes.remove(client_id)
            {
                self.flush_order.retain(|queued| queued != client_id);
                drained.push(effect);
            }
            return drained;
        }
        while drained.len() < limit {
            let Some(client_id) = self.flush_order.pop_front() else {
                break;
            };
            if let Some(effect) = self.flushes.remove(&client_id) {
                drained.push(effect);
            }
        }
        drained
    }

    /// Returns whether queued provider-service work satisfies one ownership
    /// predicate without scanning unrelated worker routes.
    pub(super) fn any_provider(&self, predicate: impl FnMut(&RuntimeSideEffect) -> bool) -> bool {
        self.provider.iter().any(predicate)
    }

    /// Drains status refresh work in FIFO order without inspecting unrelated
    /// clipboard, hook, persistence, or compatibility-queue effects.
    pub(super) fn drain_status(&mut self, limit: usize) -> Vec<RuntimeSideEffect> {
        self.status.drain(..limit.min(self.status.len())).collect()
    }

    /// Drains timer operations in actor emission order without inspecting
    /// clipboard, hook, persistence, status, or compatibility-queue effects.
    pub(super) fn drain_timers(&mut self, limit: usize) -> Vec<RuntimeSideEffect> {
        self.timers.drain(..limit.min(self.timers.len())).collect()
    }

    /// Drains dedicated worker lanes for the test-only generic adapter after
    /// it has consumed compatibility-queue work. Production workers always
    /// claim their exact lane directly; this bounded helper keeps existing
    /// generic test probes able to observe queued effects.
    pub(super) fn drain_compat(&mut self, limit: usize) -> Vec<RuntimeSideEffect> {
        let mut drained = Vec::new();
        drained.extend(self.drain_renders(limit));
        drained.extend(self.drain_flushes(None, limit.saturating_sub(drained.len())));
        for lane in [
            &mut self.clipboard,
            &mut self.hooks,
            &mut self.persistence,
            &mut self.commands,
            &mut self.provider,
            &mut self.status,
            &mut self.timers,
        ] {
            let remaining = limit.saturating_sub(drained.len());
            if remaining == 0 {
                break;
            }
            drained.extend(lane.drain(..remaining.min(lane.len())));
        }
        for lane in self.pane_processes.values_mut() {
            let remaining = limit.saturating_sub(drained.len());
            if remaining == 0 {
                break;
            }
            drained.extend(lane.drain(..remaining.min(lane.len())));
        }
        self.pane_processes.retain(|_, lane| !lane.is_empty());
        drained
    }
}
