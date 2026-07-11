//! Async runtime side-effect conversion and coalescing helpers.
//!
//! This module owns the pure transformations that convert deferred runtime
//! service work into actor side effects. Keeping these helpers separate leaves
//! the actor facade focused on request handling, event application, and queue
//! draining while preserving the existing side-effect ordering contracts.

use super::*;

/// Runs the deferred pane inputs to side effects operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn deferred_pane_inputs_to_side_effects(
    deferred_pane_inputs: Vec<DeferredPaneInput>,
) -> Vec<RuntimeSideEffect> {
    deferred_pane_inputs
        .into_iter()
        .map(|input| {
            if input.priority {
                RuntimeSideEffect::WritePaneInputPriority {
                    pane_id: input.pane_id,
                    bytes: input.bytes,
                }
            } else {
                RuntimeSideEffect::WritePaneInput {
                    pane_id: input.pane_id,
                    bytes: input.bytes,
                }
            }
        })
        .collect()
}

/// Runs the deferred pane resizes to side effects operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn deferred_pane_resizes_to_side_effects(
    deferred_pane_resizes: Vec<(String, DeferredPaneResize)>,
) -> Vec<RuntimeSideEffect> {
    deferred_pane_resizes
        .into_iter()
        .map(|(pane_id, resize)| RuntimeSideEffect::ResizePane {
            pane_id,
            size: resize.size,
        })
        .collect()
}

/// Runs the deferred pane terminations to side effects operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn deferred_pane_terminations_to_side_effects(
    deferred_pane_terminations: Vec<(String, DeferredPaneTermination)>,
) -> Vec<RuntimeSideEffect> {
    deferred_pane_terminations
        .into_iter()
        .map(|(pane_id, termination)| RuntimeSideEffect::TerminatePane {
            pane_id,
            force: termination.force,
        })
        .collect()
}

/// Runs the deferred audit writes to side effects operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn deferred_audit_writes_to_side_effects(
    deferred_audit_writes: Vec<AuditDeferredWrite>,
) -> Vec<RuntimeSideEffect> {
    deferred_audit_writes
        .into_iter()
        .map(|write| RuntimeSideEffect::PersistAuditLog {
            path: write.path,
            bytes: write.bytes,
            retention: write.retention,
        })
        .collect()
}

/// Runs the deferred agent transcript writes to side effects operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn deferred_agent_transcript_writes_to_side_effects(
    deferred_transcript_writes: Vec<DeferredAgentTranscriptWrite>,
) -> Vec<RuntimeSideEffect> {
    deferred_transcript_writes
        .into_iter()
        .map(|write| RuntimeSideEffect::PersistTranscriptEntries {
            store: write.store,
            path: write.path,
            entries: write.entries,
        })
        .collect()
}

/// Runs the deferred agent prompt history writes to side effects operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn deferred_agent_prompt_history_writes_to_side_effects(
    deferred_prompt_history_writes: Vec<DeferredAgentPromptHistoryWrite>,
) -> Vec<RuntimeSideEffect> {
    deferred_prompt_history_writes
        .into_iter()
        .map(|write| RuntimeSideEffect::PersistPromptHistory {
            store: write.store,
            path: write.path,
            conversation_id: write.conversation_id,
            prompt: write.prompt,
        })
        .collect()
}

/// Runs the deferred command prompt history writes to side effects operation
/// for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn deferred_command_prompt_history_writes_to_side_effects(
    deferred_prompt_history_writes: Vec<DeferredCommandPromptHistoryWrite>,
) -> Vec<RuntimeSideEffect> {
    deferred_prompt_history_writes
        .into_iter()
        .map(|write| RuntimeSideEffect::PersistCommandPromptHistory {
            store: write.store,
            path: write.path,
            command: write.command,
        })
        .collect()
}

/// Runs the deferred config file writes to side effects operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn deferred_config_file_writes_to_side_effects(
    deferred_config_file_writes: Vec<DeferredConfigFileWrite>,
) -> Vec<RuntimeSideEffect> {
    coalesce_deferred_config_file_writes(deferred_config_file_writes)
        .into_iter()
        .map(|write| {
            let target = match write.scope {
                crate::config::ConfigScope::Primary => PersistenceTarget::Config,
                crate::config::ConfigScope::ProjectOverlay => PersistenceTarget::ProjectConfig,
                crate::config::ConfigScope::LiveOverride => PersistenceTarget::Config,
            };
            RuntimeSideEffect::Persist {
                target,
                path: write.path,
                bytes: write.text.into_bytes(),
                mode: PersistenceWriteMode::Replace,
            }
        })
        .collect()
}

/// Keeps only the final replacement text for each deferred config file target.
///
/// # Parameters
/// - `writes`: Deferred config writes produced during one actor wakeup.
pub(super) fn coalesce_deferred_config_file_writes(
    writes: Vec<DeferredConfigFileWrite>,
) -> Vec<DeferredConfigFileWrite> {
    let mut coalesced = Vec::<DeferredConfigFileWrite>::new();
    for write in writes {
        if let Some(existing) = coalesced
            .iter_mut()
            .find(|existing| existing.scope == write.scope && existing.path == write.path)
        {
            *existing = write;
        } else {
            coalesced.push(write);
        }
    }
    coalesced
}

/// Runs the deferred project config writes to side effects operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn deferred_project_config_writes_to_side_effects(
    deferred_project_config_writes: Vec<DeferredProjectConfigWrite>,
) -> Vec<RuntimeSideEffect> {
    deferred_project_config_writes
        .into_iter()
        .map(|write| RuntimeSideEffect::Persist {
            target: PersistenceTarget::ProjectConfig,
            path: write.path,
            bytes: write.text.into_bytes(),
            mode: PersistenceWriteMode::Replace,
        })
        .collect()
}

/// Runs the deferred project instruction writes to side effects operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn deferred_project_instruction_writes_to_side_effects(
    deferred_project_instruction_writes: Vec<DeferredProjectInstructionWrite>,
) -> Vec<RuntimeSideEffect> {
    deferred_project_instruction_writes
        .into_iter()
        .map(|write| RuntimeSideEffect::Persist {
            target: PersistenceTarget::ProjectInstruction,
            path: write.path,
            bytes: write.bytes,
            mode: PersistenceWriteMode::CreateNew,
        })
        .collect()
}

/// Runs the deferred pane pipe writes to side effects operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn deferred_pane_pipe_writes_to_side_effects(
    deferred_pane_pipe_writes: Vec<DeferredPanePipeWrite>,
) -> Vec<RuntimeSideEffect> {
    deferred_pane_pipe_writes
        .into_iter()
        .map(|write| RuntimeSideEffect::Persist {
            target: PersistenceTarget::PanePipe,
            path: write.path,
            bytes: write.bytes,
            mode: PersistenceWriteMode::Append,
        })
        .collect()
}

/// Runs the deferred program hooks to side effects operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn deferred_program_hooks_to_side_effects(
    deferred_program_hooks: Vec<DeferredProgramHook>,
) -> Vec<RuntimeSideEffect> {
    deferred_program_hooks
        .into_iter()
        .map(|hook| RuntimeSideEffect::RunProgramHook {
            plan: Box::new(hook.plan),
            triggering_event_completed: hook.triggering_event_completed,
        })
        .collect()
}

/// Runs the deferred registry update to side effect operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn deferred_registry_update_to_side_effect(
    deferred_registry_update: Option<(
        crate::registry::SessionRegistry,
        crate::runtime::RuntimeRegistryUpdatePlan,
    )>,
) -> Vec<RuntimeSideEffect> {
    deferred_registry_update
        .map(|(registry, update)| RuntimeSideEffect::PersistRegistry { registry, update })
        .into_iter()
        .collect()
}
