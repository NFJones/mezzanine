//! Typed filesystem result projection, preserving confirmed endpoints on failure.
//!
//! No stdout, exit status, signal or executable command is fabricated. Receipt
//! validation rejects contradictory or out-of-budget evidence while its original
//! raw owner remains retained. Display truncation cannot truncate endpoint facts,
//! completed operation identities or full native modification counts.

use super::*;
use crate::runtime::processes::native_patch::worker::{
    NativePatchEndpointKind, NativePatchEndpointReceipt,
};
use mez_agent::native_action::{
    NativeEffectCertainty, NativeFilesystemEnforcement, NativePatchPhase, PatchEffect,
};

/// Projects only bounded, internally consistent original worker evidence.
pub(super) fn action_result(
    work: &NativeFilesystemWork,
    raw: &NativePatchWorkerOutcome,
) -> Result<ActionResult> {
    validate(work, raw)?;
    let certainty = certainty_name(raw.result.certainty);
    let metadata = serde_json::json!({
        "execution_target": "native_runtime", "sent_to_pane": false,
        "enforcement": NativeFilesystemEnforcement::RuntimeCapabilities.as_str(), "effect_certainty": certainty,
        "automatic_replay_allowed": false,
        "confirmed_effects": raw.result.confirmed_effects.iter().map(effect_json).collect::<Vec<_>>(),
        "completed_operations": raw.completed_operations,
        "confirmed_endpoints": raw.receipts.iter().map(|receipt| serde_json::json!({
            "operation_ordinal": receipt.operation_ordinal, "endpoint_ordinal": receipt.endpoint_ordinal,
            "path": receipt.path, "resolved_path": receipt.resolved_path, "kind": endpoint_name(receipt.kind),
            "diff": receipt.diff, "diff_truncated": receipt.diff_truncated,
            "added_lines": receipt.added_lines, "removed_lines": receipt.removed_lines,
        })).collect::<Vec<_>>(),
    }).to_string();
    let mut result = if raw.result.certainty == NativeEffectCertainty::Confirmed {
        ActionResult::succeeded(
            &work.owner.turn,
            &work.owner.action,
            vec![format!(
                "Native patch completed; {} confirmed file endpoint(s).",
                raw.receipts.len()
            )],
            Some(metadata),
        )
    } else {
        let mut result = ActionResult::failed(&work.owner.turn, &work.owner.action, ActionStatus::Failed,
            format!("native_patch_{certainty}"), raw.result.diagnostic.as_deref().unwrap_or("Native patch did not complete; inspect confirmed endpoints before any new mutation."))?;
        result.structured_content_json = Some(metadata);
        result
    };
    result.permission_evaluation = work.owner.permission.clone();
    Ok(result)
}

/// Encodes only original semantic identity and validated outcome summary for
/// completion matching. Shell command, exit-code, stdout and diff fields are
/// deliberately absent; this payload can never authorize or execute a hook.
pub(super) fn completed_hook_payload(
    work: &NativeFilesystemWork,
    raw: &NativePatchWorkerOutcome,
    status: ActionStatus,
) -> String {
    serde_json::json!({
        "turn_id": work.owner.turn.turn_id,
        "agent_id": work.owner.turn.agent_id,
        "pane_id": work.owner.turn.pane_id,
        "conversation_id": work.owner.turn.conversation_id,
        "action_id": work.owner.action.id,
        "action_type": work.owner.action.action_type(),
        "semantic_action_type": work.owner.action.action_type(),
        "execution_target": "native_runtime", "sent_to_pane": false,
        "policy_descriptor": work.identity.operation.policy_descriptor(),
        "action_status": if status == ActionStatus::Succeeded { "succeeded" } else { "failed" },
        "effect_certainty": certainty_name(raw.result.certainty),
        "confirmed_endpoint_count": raw.receipts.len(),
        "completed_operation_count": raw.completed_operations.len(),
    })
    .to_string()
}

/// Checks semantic occurrences, dependencies, certainty and aggregate budgets.
fn validate(work: &NativeFilesystemWork, raw: &NativePatchWorkerOutcome) -> Result<()> {
    let effects = &work.identity.operation.effects;
    let invalid = || {
        MezError::invalid_state("filesystem worker evidence contradicts admitted semantic outcome")
    };
    if raw.result.identity != work.identity
        || raw.result.certainty == NativeEffectCertainty::InFlight
        || raw.receipts.len() > effects.len().saturating_mul(2)
        || raw.completed_operations.len() > effects.len()
        || raw
            .completed_operations
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        || raw
            .completed_operations
            .iter()
            .any(|ordinal| *ordinal >= effects.len())
        || raw.receipts.windows(2).any(|pair| {
            (pair[0].operation_ordinal, pair[0].endpoint_ordinal)
                >= (pair[1].operation_ordinal, pair[1].endpoint_ordinal)
        })
        || raw
            .receipts
            .iter()
            .map(|receipt| receipt.diff.len())
            .sum::<usize>()
            > mez_agent::semantic_patch_planning::native::NATIVE_PATCH_DIFF_BYTES
        || raw
            .result
            .diagnostic
            .as_ref()
            .is_some_and(|text| text.len() > 8192)
    {
        return Err(invalid());
    }
    for receipt in &raw.receipts {
        if receipt.path.len() > 4096
            || receipt.resolved_path.len() > 4096
            || !std::path::Path::new(&receipt.resolved_path).is_absolute()
            || receipt.added_lines
                > mez_agent::semantic_patch_planning::native::NATIVE_PATCH_FILE_BYTES
            || receipt.removed_lines
                > mez_agent::semantic_patch_planning::native::NATIVE_PATCH_FILE_BYTES
            || !matches_endpoint(effects.get(receipt.operation_ordinal), receipt)
            || (receipt.endpoint_ordinal == 1
                && !raw.receipts.iter().any(|earlier| {
                    earlier.operation_ordinal == receipt.operation_ordinal
                        && earlier.endpoint_ordinal == 0
                }))
        {
            return Err(invalid());
        }
    }
    for ordinal in &raw.completed_operations {
        let has_endpoint = |endpoint| {
            raw.receipts.iter().any(|receipt| {
                receipt.operation_ordinal == *ordinal && receipt.endpoint_ordinal == endpoint
            })
        };
        // Only updates may legitimately complete as unchanged shared-planner
        // operations. Create/delete/move completion always needs its actual
        // positive endpoint evidence, even when every receipt was omitted.
        let mandatory_receipts_present = match &effects[*ordinal] {
            PatchEffect::Update(_) => true,
            PatchEffect::Create(_) | PatchEffect::Delete(_) => has_endpoint(0),
            PatchEffect::Move { .. } => has_endpoint(0) && has_endpoint(1),
        };
        if !mandatory_receipts_present {
            return Err(invalid());
        }
    }
    let confirmed = raw
        .completed_operations
        .iter()
        .filter(|ordinal| {
            raw.receipts
                .iter()
                .any(|receipt| receipt.operation_ordinal == **ordinal)
        })
        .map(|ordinal| effects[*ordinal].clone())
        .collect::<Vec<_>>();
    if confirmed != raw.result.confirmed_effects
        || (raw.result.certainty == NativeEffectCertainty::Unexecuted && !raw.receipts.is_empty())
        || (raw.result.certainty == NativeEffectCertainty::Partial && raw.receipts.is_empty())
        || (raw.result.certainty == NativeEffectCertainty::Confirmed
            && (raw.result.phase != NativePatchPhase::Settle
                || raw.result.diagnostic.is_some()
                || raw.completed_operations != (0..effects.len()).collect::<Vec<_>>()))
    {
        return Err(invalid());
    }
    Ok(())
}

/// Matches each positive endpoint to the original exact authored occurrence.
fn matches_endpoint(effect: Option<&PatchEffect>, receipt: &NativePatchEndpointReceipt) -> bool {
    match effect {
        Some(PatchEffect::Create(path)) => {
            receipt.endpoint_ordinal == 0
                && receipt.path == *path
                && receipt.kind == NativePatchEndpointKind::Created
        }
        Some(PatchEffect::Update(path)) => {
            receipt.endpoint_ordinal == 0
                && receipt.path == *path
                && receipt.kind == NativePatchEndpointKind::Updated
        }
        Some(PatchEffect::Delete(path)) => {
            receipt.endpoint_ordinal == 0
                && receipt.path == *path
                && receipt.kind == NativePatchEndpointKind::Deleted
        }
        Some(PatchEffect::Move {
            source,
            destination,
        }) => match receipt.endpoint_ordinal {
            0 => receipt.path == *destination && receipt.kind == NativePatchEndpointKind::Created,
            1 => receipt.path == *source && receipt.kind == NativePatchEndpointKind::Deleted,
            _ => false,
        },
        None => false,
    }
}

/// Uses fixed protocol-safe certainty labels rather than debug strings.
fn certainty_name(certainty: NativeEffectCertainty) -> &'static str {
    match certainty {
        NativeEffectCertainty::Unexecuted => "unexecuted",
        NativeEffectCertainty::Confirmed => "confirmed",
        NativeEffectCertainty::Partial => "partial",
        NativeEffectCertainty::InFlight => "in_flight",
        NativeEffectCertainty::Unknown => "unknown",
    }
}

/// Uses fixed endpoint kind labels for confirmed filesystem facts.
fn endpoint_name(kind: NativePatchEndpointKind) -> &'static str {
    match kind {
        NativePatchEndpointKind::Created => "created",
        NativePatchEndpointKind::Updated => "updated",
        NativePatchEndpointKind::Deleted => "deleted",
    }
}

/// Serializes exact semantic intent without suggesting a partial move completed.
fn effect_json(effect: &PatchEffect) -> serde_json::Value {
    match effect {
        PatchEffect::Create(path) => serde_json::json!({"kind":"create","path":path}),
        PatchEffect::Update(path) => serde_json::json!({"kind":"update","path":path}),
        PatchEffect::Delete(path) => serde_json::json!({"kind":"delete","path":path}),
        PatchEffect::Move {
            source,
            destination,
        } => serde_json::json!({"kind":"move","source":source,"destination":destination}),
    }
}
