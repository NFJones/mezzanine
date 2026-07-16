//! Attached-terminal step planning and application through the async actor handle.

use super::{
    AsyncRuntimeSessionHandle, AttachedClientStepApplication, AttachedTerminalClientStepPlan,
    AttachedTerminalFdReadiness, ClientId, ClientStatusLine, ClientViewRole, Result, Size,
    TerminalClientLoopConfig, plan_attached_terminal_client_step,
};

/// Runs the plan async attached terminal client step operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn plan_async_attached_terminal_client_step(
    handle: &AsyncRuntimeSessionHandle,
    role: ClientViewRole,
    client_size: Size,
    config: TerminalClientLoopConfig,
    readiness: &[AttachedTerminalFdReadiness],
    input: Option<&[u8]>,
    status: Option<&ClientStatusLine>,
) -> Result<AttachedTerminalClientStepPlan> {
    let frame = handle
        .render_client_frame(role, client_size, config, true)
        .await?;
    plan_attached_terminal_client_step(readiness, input, frame.view.as_ref(), status, &frame.config)
}

/// Carries Async Attached Terminal Step Request state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone)]
pub struct AsyncAttachedTerminalStepRequest<'a> {
    /// Stores the primary client id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub primary_client_id: ClientId,
    /// Stores the role value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub role: ClientViewRole,
    /// Stores the client size value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub client_size: Size,
    /// Stores the config value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub config: TerminalClientLoopConfig,
    /// Stores the readiness value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub readiness: &'a [AttachedTerminalFdReadiness],
    /// Stores the input value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub input: Option<&'a [u8]>,
    /// Stores the status value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub status: Option<&'a ClientStatusLine>,
}

/// Runs the plan and apply async attached terminal client step operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn plan_and_apply_async_attached_terminal_client_step(
    handle: &AsyncRuntimeSessionHandle,
    request: AsyncAttachedTerminalStepRequest<'_>,
) -> Result<(
    AttachedTerminalClientStepPlan,
    AttachedClientStepApplication,
)> {
    let plan = plan_async_attached_terminal_client_step(
        handle,
        request.role,
        request.client_size,
        request.config,
        request.readiness,
        request.input,
        request.status,
    )
    .await?;
    let application = handle
        .apply_attached_terminal_step_plan(request.primary_client_id, plan.clone())
        .await?;
    Ok((plan, application))
}
