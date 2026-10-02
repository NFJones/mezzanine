//! Canonical results for native patch execution, without shell evidence.
//!
//! Workers supply positively confirmed endpoints, not intended operations.
//! Move destination and source deletion are distinct confirmations, so a partial
//! move cannot be represented as a completed move merely from its plan. This
//! owner projects evidence only; actor commit authorization and exactly-once
//! settlement remain product-owned. Bounded display never removes confirmations.

use crate::execution::LocalExecutionProjectionError;
use crate::native_action::{
    NativeCommitIdentity, NativeEffectCertainty, NativeFilesystemEnforcement, NativePatchOperation,
};
use crate::{ActionResult, ActionStatus, AgentAction, AgentActionPayload, AgentTurnResultIdentity};

#[cfg(test)]
mod tests;

/// Exact endpoint kind positively confirmed by filesystem publication evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeConfirmedKind {
    /// Existing entry atomically replaced, or missing destination created.
    Written,
    /// Existing entry removed after exact preimage/object verification.
    Deleted,
    /// Explicit separately authorized parent directory created.
    DirectoryCreated,
}

/// One positively confirmed effect, independent of diff/display retention.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct NativeConfirmedEndpoint {
    /// Authored operation index; both move endpoints share this identity.
    pub operation_ordinal: usize,
    /// Unique monotonically ordered confirmation index within the transaction.
    pub confirmation_ordinal: usize,
    /// Exact logical endpoint, not a display label parsed from a diff.
    pub path: String,
    /// Positively observed endpoint effect.
    pub kind: NativeConfirmedKind,
}

/// Projects settled typed evidence into the existing canonical action record.
/// Rejects mismatched operation/turn/action ownership and inconsistent certainty.
/// In-flight work remains running; unknown/partial effects forbid automatic replay.
#[allow(
    clippy::too_many_arguments,
    reason = "projection binds exact plan and independent worker evidence without changing canonical ownership"
)]
pub fn native_patch_result(
    turn: &(impl AgentTurnResultIdentity + ?Sized),
    action: &AgentAction,
    identity: &NativeCommitIdentity,
    plan: &crate::semantic_patch_planning::native::NativePatchPlan,
    certainty: NativeEffectCertainty,
    confirmations: &[NativeConfirmedEndpoint],
    display: &str,
    diagnostic: Option<&str>,
) -> Result<ActionResult, LocalExecutionProjectionError> {
    let invalid = |message: &str| crate::LocalActionPlanningError::new(message);
    let AgentActionPayload::ApplyPatch { patch, strip } = &action.payload else {
        return Err(invalid("native patch evidence requires apply_patch").into());
    };
    if identity.turn_id != turn.turn_id()
        || identity.action_id != action.id
        || identity.operation != NativePatchOperation::parse(patch, *strip)?
        || plan.operation() != &identity.operation
    {
        return Err(invalid("native patch evidence does not match exact action ownership").into());
    }
    if certainty == NativeEffectCertainty::Unexecuted && !confirmations.is_empty() {
        return Err(invalid("unexecuted native outcome contains confirmed effects").into());
    }
    let mut previous = None;
    let mut seen = std::collections::BTreeSet::new();
    let mut previous_plan_position = None;
    let planned_endpoints = plan
        .operations()
        .iter()
        .flat_map(|operation| {
            operation.changes().iter().map(move |change| {
                (
                    operation.ordinal(),
                    change.path.as_str(),
                    if change.final_bytes.is_some() {
                        NativeConfirmedKind::Written
                    } else {
                        NativeConfirmedKind::Deleted
                    },
                )
            })
        })
        .collect::<Vec<_>>();
    for confirmation in confirmations {
        let Some(operation) = plan
            .operations()
            .iter()
            .find(|operation| operation.ordinal() == confirmation.operation_ordinal)
        else {
            return Err(invalid("native confirmation has unknown operation identity").into());
        };
        let position = planned_endpoints.iter().position(|(ordinal, path, kind)| {
            *ordinal == confirmation.operation_ordinal
                && *path == confirmation.path
                && *kind == confirmation.kind
        });
        let is_endpoint = position.is_some();
        if let Some(position) = position {
            if previous_plan_position.is_some_and(|previous| previous >= position) {
                return Err(
                    invalid("native confirmations violate accepted plan dependency order").into(),
                );
            }
            // Move deletion requires its preceding destination confirmation,
            // even for partial outcomes that otherwise allow skipped failures.
            if let crate::native_action::PatchEffect::Move { destination, .. } = operation.effect()
                && confirmation.kind == NativeConfirmedKind::Deleted
                && !confirmations.iter().any(|earlier| {
                    earlier.operation_ordinal == operation.ordinal()
                        && earlier.path == *destination
                        && earlier.kind == NativeConfirmedKind::Written
                        && earlier.confirmation_ordinal < confirmation.confirmation_ordinal
                })
            {
                return Err(invalid("native move deletion lacks confirmed destination").into());
            }
            previous_plan_position = Some(position);
        }
        let is_parent = confirmation.kind == NativeConfirmedKind::DirectoryCreated
            && !confirmation.path.is_empty()
            && !std::path::Path::new(&confirmation.path)
                .components()
                .any(|part| {
                    matches!(
                        part,
                        std::path::Component::ParentDir | std::path::Component::CurDir
                    )
                })
            && operation.changes().iter().any(|change| {
                change.final_bytes.is_some()
                    && std::path::Path::new(&change.path).starts_with(&confirmation.path)
                    && change.path != confirmation.path
            });
        if (!is_endpoint && !is_parent)
            || !seen.insert((
                confirmation.operation_ordinal,
                confirmation.path.clone(),
                format!("{:?}", confirmation.kind),
            ))
            || previous.is_some_and(|ordinal| ordinal >= confirmation.confirmation_ordinal)
        {
            return Err(invalid("native confirmation path/order does not match operation").into());
        }
        previous = Some(confirmation.confirmation_ordinal);
    }
    if certainty == NativeEffectCertainty::Confirmed {
        if !plan.errors().is_empty()
            || plan.operations().len() != identity.operation.patch.operations.len()
        {
            return Err(invalid("confirmed native result has incomplete planning evidence").into());
        }
        for operation in plan.operations() {
            for change in operation.changes() {
                let kind = if change.final_bytes.is_some() {
                    NativeConfirmedKind::Written
                } else {
                    NativeConfirmedKind::Deleted
                };
                if !confirmations.iter().any(|confirmation| {
                    confirmation.operation_ordinal == operation.ordinal()
                        && confirmation.path == change.path
                        && confirmation.kind == kind
                }) {
                    return Err(
                        invalid("confirmed native result omitted a planned endpoint").into(),
                    );
                }
            }
        }
    }
    let budget = crate::semantic_patch_planning::native::NATIVE_PATCH_DIFF_BYTES;
    let mut end = display.len().min(budget);
    while !display.is_char_boundary(end) {
        end -= 1;
    }
    let content = vec![display[..end].to_string()];
    let structured = serde_json::json!({
        "execution_transport": "native_runtime", "sent_to_pane": false,
        "filesystem_enforcement": NativeFilesystemEnforcement::RuntimeCapabilities.as_str(),
        "transaction": identity.transaction, "authority_generation": identity.authority_generation,
        "effect_certainty": format!("{certainty:?}").to_ascii_lowercase(),
        "confirmed_endpoints": confirmations, "display_truncated": end < display.len(),
        "automatic_replay": false,
    })
    .to_string();
    if certainty == NativeEffectCertainty::InFlight {
        return Ok(ActionResult::running(
            turn,
            action,
            content,
            Some(structured),
        ));
    }
    if certainty == NativeEffectCertainty::Confirmed && diagnostic.is_none() {
        return Ok(ActionResult::succeeded(
            turn,
            action,
            content,
            Some(structured),
        ));
    }
    let mut result = ActionResult::failed(
        turn,
        action,
        ActionStatus::Failed,
        "native_patch_incomplete",
        diagnostic.unwrap_or("native patch effects are incomplete or unknown"),
    )?;
    result.content = crate::action_text_content_blocks(content);
    result.structured_content_json = Some(structured);
    Ok(result)
}
