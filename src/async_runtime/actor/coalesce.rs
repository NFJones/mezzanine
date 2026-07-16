//! Side-effect coalescing, classification, and time helpers.

use super::*;

/// Coalesces bursty output effects before they enter the bounded actor queue.
///
/// Render invalidations and full client-output frames are level-triggered:
/// multiple pending requests for the same client can be represented by one
/// request with the latest frame or strongest invalidation reason. Coalescing
/// at enqueue time prevents pane-output bursts from filling the shared
/// side-effect queue with stale repaint work before the attached client or
/// render worker can drain it.
pub(super) fn coalesce_output_side_effects_for_enqueue(
    queued: &mut VecDeque<RuntimeSideEffect>,
    side_effects: Vec<RuntimeSideEffect>,
) -> (Vec<RuntimeSideEffect>, usize) {
    let mut retained = Vec::new();
    let mut coalesced = 0usize;
    for effect in side_effects {
        match effect {
            RuntimeSideEffect::RenderClient { client_id, reason } => {
                if coalesce_render_side_effect_into_queue(queued, &client_id, reason)
                    || coalesce_render_side_effect_into_vec(&mut retained, &client_id, reason)
                {
                    coalesced = coalesced.saturating_add(1);
                } else {
                    retained.push(RuntimeSideEffect::RenderClient { client_id, reason });
                }
            }
            RuntimeSideEffect::FlushClientOutput {
                client_id,
                lines,
                line_style_spans,
                modes,
            } => {
                let mut effect = Some(RuntimeSideEffect::FlushClientOutput {
                    client_id: client_id.clone(),
                    lines,
                    line_style_spans,
                    modes,
                });
                if coalesce_flush_side_effect_into_queue(queued, &client_id, &mut effect)
                    || coalesce_flush_side_effect_into_vec(&mut retained, &client_id, &mut effect)
                {
                    coalesced = coalesced.saturating_add(1);
                } else if let Some(effect) = effect {
                    retained.push(effect);
                }
            }
            RuntimeSideEffect::PersistRegistry { registry, update } => {
                let session_id = registry_update_session_id(&update).to_string();
                let mut effect = Some(RuntimeSideEffect::PersistRegistry {
                    registry: registry.clone(),
                    update,
                });
                if coalesce_registry_side_effect_into_queue(
                    queued,
                    &registry,
                    &session_id,
                    &mut effect,
                ) || coalesce_registry_side_effect_into_vec(
                    &mut retained,
                    &registry,
                    &session_id,
                    &mut effect,
                ) {
                    coalesced = coalesced.saturating_add(1);
                } else if let Some(effect) = effect {
                    retained.push(effect);
                }
            }
            effect => retained.push(effect),
        }
    }
    (retained, coalesced)
}

/// Merges one render invalidation into an existing queued invalidation.
pub(super) fn coalesce_render_side_effect_into_queue(
    queued: &mut VecDeque<RuntimeSideEffect>,
    client_id: &ClientId,
    reason: RenderInvalidationReason,
) -> bool {
    queued.iter_mut().any(|effect| {
        let RuntimeSideEffect::RenderClient {
            client_id: queued_client_id,
            reason: queued_reason,
        } = effect
        else {
            return false;
        };
        if queued_client_id != client_id {
            return false;
        }
        *queued_reason = coalesce_render_invalidation_reason(*queued_reason, reason);
        true
    })
}

/// Merges one render invalidation into a same-batch invalidation.
pub(super) fn coalesce_render_side_effect_into_vec(
    retained: &mut [RuntimeSideEffect],
    client_id: &ClientId,
    reason: RenderInvalidationReason,
) -> bool {
    retained.iter_mut().any(|effect| {
        let RuntimeSideEffect::RenderClient {
            client_id: retained_client_id,
            reason: retained_reason,
        } = effect
        else {
            return false;
        };
        if retained_client_id != client_id {
            return false;
        }
        *retained_reason = coalesce_render_invalidation_reason(*retained_reason, reason);
        true
    })
}

/// Replaces a pending client-output flush already queued for the same client.
pub(super) fn coalesce_flush_side_effect_into_queue(
    queued: &mut VecDeque<RuntimeSideEffect>,
    client_id: &ClientId,
    effect: &mut Option<RuntimeSideEffect>,
) -> bool {
    queued.iter_mut().any(|queued_effect| {
        let RuntimeSideEffect::FlushClientOutput {
            client_id: queued_client_id,
            ..
        } = queued_effect
        else {
            return false;
        };
        if queued_client_id != client_id {
            return false;
        }
        let Some(replacement) = effect.take() else {
            return false;
        };
        *queued_effect = replacement;
        true
    })
}

/// Replaces a same-batch client-output flush for the same client.
pub(super) fn coalesce_flush_side_effect_into_vec(
    retained: &mut [RuntimeSideEffect],
    client_id: &ClientId,
    effect: &mut Option<RuntimeSideEffect>,
) -> bool {
    retained.iter_mut().any(|retained_effect| {
        let RuntimeSideEffect::FlushClientOutput {
            client_id: retained_client_id,
            ..
        } = retained_effect
        else {
            return false;
        };
        if retained_client_id != client_id {
            return false;
        }
        let Some(replacement) = effect.take() else {
            return false;
        };
        *retained_effect = replacement;
        true
    })
}

/// Replaces a pending registry persistence effect for the same session.
pub(super) fn coalesce_registry_side_effect_into_queue(
    queued: &mut VecDeque<RuntimeSideEffect>,
    registry: &crate::registry::SessionRegistry,
    session_id: &str,
    effect: &mut Option<RuntimeSideEffect>,
) -> bool {
    queued.iter_mut().any(|queued_effect| {
        let RuntimeSideEffect::PersistRegistry {
            registry: queued_registry,
            update,
        } = queued_effect
        else {
            return false;
        };
        if queued_registry != registry || registry_update_session_id(update) != session_id {
            return false;
        }
        let Some(replacement) = effect.take() else {
            return false;
        };
        *queued_effect = replacement;
        true
    })
}

/// Replaces a same-batch registry persistence effect for the same session.
pub(super) fn coalesce_registry_side_effect_into_vec(
    retained: &mut [RuntimeSideEffect],
    registry: &crate::registry::SessionRegistry,
    session_id: &str,
    effect: &mut Option<RuntimeSideEffect>,
) -> bool {
    retained.iter_mut().any(|retained_effect| {
        let RuntimeSideEffect::PersistRegistry {
            registry: retained_registry,
            update,
        } = retained_effect
        else {
            return false;
        };
        if retained_registry != registry || registry_update_session_id(update) != session_id {
            return false;
        }
        let Some(replacement) = effect.take() else {
            return false;
        };
        *retained_effect = replacement;
        true
    })
}

/// Returns the session targeted by a registry persistence plan.
pub(super) fn registry_update_session_id(
    update: &crate::runtime::RuntimeRegistryUpdatePlan,
) -> &str {
    match update {
        crate::runtime::RuntimeRegistryUpdatePlan::Upsert(record) => &record.session_id,
        crate::runtime::RuntimeRegistryUpdatePlan::Remove { session_id } => session_id,
    }
}

/// Returns whether applying an event can change the session registry record.
pub(super) fn runtime_event_requires_registry_persistence(event: &RuntimeEvent) -> bool {
    match event {
        RuntimeEvent::Pane(
            PaneEvent::Output { .. }
            | PaneEvent::InputWritten { .. }
            | PaneEvent::WriteFailed { .. }
            | PaneEvent::Resized { .. }
            | PaneEvent::ForegroundProcess { .. },
        )
        | RuntimeEvent::Hook(_)
        | RuntimeEvent::Persistence(_)
        | RuntimeEvent::Timer(_) => false,
        RuntimeEvent::Client(_)
        | RuntimeEvent::Process(_)
        | RuntimeEvent::AgentProvider(_)
        | RuntimeEvent::AgentCompaction(_)
        | RuntimeEvent::AgentRemember(_)
        | RuntimeEvent::Shutdown(_) => true,
    }
}

/// Returns the owning turn for a provider retry timer side effect.
///
/// Retry timer side effects are created before they are registered in the
/// actor's scheduled-timer map. Reconciliation inspects the not-yet-queued
/// side-effect list through this helper so retryable provider failures are not
/// mistaken for unreachable running turns.
pub(super) fn provider_retry_timer_side_effect_turn_id(
    effect: &RuntimeSideEffect,
) -> Option<String> {
    match effect {
        RuntimeSideEffect::ScheduleTimer { key, .. }
            if key.kind == RuntimeTimerKind::ProviderRetry =>
        {
            Some(key.owner_id.clone())
        }
        _ => None,
    }
}

/// Runs the runtime client step application applied operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the coalesce render invalidation reason operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn coalesce_render_invalidation_reason(
    current: RenderInvalidationReason,
    incoming: RenderInvalidationReason,
) -> RenderInvalidationReason {
    if render_invalidation_reason_priority(incoming) >= render_invalidation_reason_priority(current)
    {
        incoming
    } else {
        current
    }
}

/// Runs the render invalidation reason priority operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn render_invalidation_reason_priority(reason: RenderInvalidationReason) -> u8 {
    match reason {
        RenderInvalidationReason::CursorBlink => 0,
        RenderInvalidationReason::StatusLine => 1,
        RenderInvalidationReason::PaneOutput => 2,
        RenderInvalidationReason::AgentPrompt => 3,
        RenderInvalidationReason::Overlay => 4,
        RenderInvalidationReason::Configuration => 5,
        RenderInvalidationReason::Resize => 6,
        RenderInvalidationReason::Layout => 7,
        RenderInvalidationReason::FullRedraw => 8,
    }
}

/// Runs the pane io side effect targets pane operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn pane_io_side_effect_targets_pane(
    effect: &RuntimeSideEffect,
    target_pane_id: &str,
) -> bool {
    match effect {
        RuntimeSideEffect::WritePaneInput { pane_id, .. }
        | RuntimeSideEffect::WritePaneInputPriority { pane_id, .. }
        | RuntimeSideEffect::ResizePane { pane_id, .. }
        | RuntimeSideEffect::TerminatePane { pane_id, .. } => pane_id == target_pane_id,
        _ => false,
    }
}

/// Runs the timer side effect targets timer worker operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn timer_side_effect_targets_timer_worker(effect: &RuntimeSideEffect) -> bool {
    matches!(
        effect,
        RuntimeSideEffect::ScheduleTimer { .. } | RuntimeSideEffect::CancelTimer { .. }
    )
}

/// Builds a compact count summary for queued side-effect diagnostics.
pub(super) fn runtime_side_effect_kind_summary<'a>(
    effects: impl Iterator<Item = &'a RuntimeSideEffect>,
) -> String {
    let mut counts: Vec<(&'static str, usize)> = Vec::new();
    for effect in effects {
        let kind = runtime_side_effect_kind(effect);
        if let Some((_, count)) = counts.iter_mut().find(|(existing, _)| *existing == kind) {
            *count = count.saturating_add(1);
        } else {
            counts.push((kind, 1));
        }
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

/// Returns a stable diagnostic family for one queued side effect.
pub(super) fn runtime_side_effect_kind(effect: &RuntimeSideEffect) -> &'static str {
    match effect {
        RuntimeSideEffect::WritePaneInput { .. } => "write-pane-input",
        RuntimeSideEffect::WritePaneInputPriority { .. } => "write-pane-input-priority",
        RuntimeSideEffect::ResizePane { .. } => "resize-pane",
        RuntimeSideEffect::TerminatePane { .. } => "terminate-pane",
        RuntimeSideEffect::RenderClient { .. } => "render-client",
        RuntimeSideEffect::ScheduleTimer { .. } => "schedule-timer",
        RuntimeSideEffect::CancelTimer { .. } => "cancel-timer",
        RuntimeSideEffect::DispatchAgentProvider { .. } => "dispatch-agent-provider",
        RuntimeSideEffect::DispatchAgentCompaction { .. } => "dispatch-agent-compaction",
        RuntimeSideEffect::DispatchAgentRemember { .. } => "dispatch-agent-remember",
        RuntimeSideEffect::RunProgramHook { .. } => "run-program-hook",
        RuntimeSideEffect::Persist { .. } => "persist",
        RuntimeSideEffect::PersistAuditLog { .. } => "persist-audit-log",
        RuntimeSideEffect::PersistTranscriptEntries { .. } => "persist-transcript",
        RuntimeSideEffect::PersistPromptHistory { .. } => "persist-prompt-history",
        RuntimeSideEffect::PersistCommandPromptHistory { .. } => "persist-command-prompt-history",
        RuntimeSideEffect::PersistRegistry { .. } => "persist-registry",
        RuntimeSideEffect::FlushClientOutput { .. } => "flush-client-output",
    }
}

/// Runs the runtime timer kind is shell transaction operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_timer_kind_is_shell_transaction(kind: RuntimeTimerKind) -> bool {
    matches!(
        kind,
        RuntimeTimerKind::ShellTransaction
            | RuntimeTimerKind::ReadinessProbe
            | RuntimeTimerKind::Bootstrap
            | RuntimeTimerKind::FocusedShellHook
    )
}

/// Runs the shell transaction schedule timer keys operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the shell transaction cancel timer keys operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the idle cleanup schedule timer key operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the idle cleanup cancel timer keys operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the resize debounce schedule timer keys operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the resize debounce cancel timer keys operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the cursor blink schedule timer keys operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the cursor blink cancel timer keys operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the status refresh required by config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
/// Runs the provider failure is retryable operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the provider poll schedule timer key operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the provider poll cancel timer keys operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the provider retry schedule timer keys operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the provider retry cancel timer keys operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the provider claim schedule timer keys operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the provider claim cancel timer keys operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the pane pipe health schedule timer keys operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the pane pipe health cancel timer keys operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// Runs the side effects include registry persistence operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn side_effects_include_registry_persistence(effects: &[RuntimeSideEffect]) -> bool {
    effects
        .iter()
        .any(|effect| matches!(effect, RuntimeSideEffect::PersistRegistry { .. }))
}

/// Runs the async runtime current unix millis operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn async_runtime_current_unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

/// Runs the async runtime duration millis operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn async_runtime_duration_millis(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis())
        .unwrap_or(u64::MAX)
        .max(1)
}
