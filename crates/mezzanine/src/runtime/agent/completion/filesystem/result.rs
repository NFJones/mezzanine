//! Typed filesystem result projection, preserving confirmed endpoints on failure.
//!
//! No stdout, exit status, signal or executable command is fabricated. Receipt
//! validation rejects contradictory or out-of-budget evidence while its original
//! raw owner remains retained. Display truncation cannot truncate endpoint facts,
//! completed operation identities or full native modification counts.

use super::*;
use crate::runtime::processes::native_patch::worker::{
    NATIVE_PATCH_FACT_BYTES, NATIVE_PATCH_RESULT_BYTES, effect_json,
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
        "configured_sandbox_intent": work.owner.sandbox_intent,
        "enforcement": NativeFilesystemEnforcement::RuntimeCapabilities.as_str(), "effect_certainty": certainty,
        "automatic_replay_allowed": false,
        "confirmed_effects": raw.result.confirmed_effects.iter().map(effect_json).collect::<Vec<_>>(),
        "completed_operations": raw.completed_operations,
        "confirmed_endpoints": raw.receipts.iter().map(|receipt| receipt.metadata(true)).collect::<Vec<_>>(),
    }).to_string();
    if metadata.len() > NATIVE_PATCH_RESULT_BYTES {
        return Err(MezError::invalid_state(
            "filesystem result exceeds admitted metadata budget",
        ));
    }
    let files = raw
        .receipts
        .iter()
        .filter(|receipt| receipt.is_file())
        .count();
    let directories = raw.receipts.len() - files;
    let mut summary = format!("Native patch completed; {files} confirmed file endpoint(s).");
    if directories > 0 {
        summary.push_str(&format!(
            " {directories} separately confirmed directory effect(s)."
        ));
    }
    let mut result = if raw.result.certainty == NativeEffectCertainty::Confirmed {
        ActionResult::succeeded(
            &work.owner.turn,
            &work.owner.action,
            vec![summary],
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

/// Projects only the separate positive no-dispatch proof. Empty semantic data
/// does not imply native join; cancellation/rejection remains original provenance.
pub(super) fn refusal_result(
    work: &NativeFilesystemWork,
    refusal: &crate::runtime::processes::native_filesystem_producer::NativeFilesystemRefusal,
) -> Result<ActionResult> {
    let mut result = action_result(work, refusal.outcome())?;
    result.status = if refusal.cancelled() {
        ActionStatus::Cancelled
    } else if refusal.cause().kind() == crate::error::MezErrorKind::Forbidden {
        ActionStatus::Denied
    } else {
        ActionStatus::Failed
    };
    let text = result
        .structured_content_json
        .as_deref()
        .ok_or_else(|| MezError::invalid_state("filesystem refusal metadata absent"))?;
    let mut metadata: serde_json::Value = serde_json::from_str(text)
        .map_err(|_| MezError::invalid_state("filesystem refusal metadata invalid"))?;
    metadata["dispatch_state"] = serde_json::json!("not_dispatched");
    metadata["refusal_error_kind"] = serde_json::json!(format!("{:?}", refusal.cause().kind()));
    result.structured_content_json = Some(metadata.to_string());
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
        "action_status": match status { ActionStatus::Succeeded => "succeeded", ActionStatus::Cancelled => "cancelled", _ => "failed" },
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
        || raw.receipts.len()
            > effects.len().saturating_mul(2)
                + crate::security::filesystem::capability::parent::MAX_NATIVE_PARENT_DIRECTORIES
        || raw.completed_operations.len() > effects.len()
        || raw
            .completed_operations
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        || raw
            .completed_operations
            .iter()
            .any(|ordinal| *ordinal >= effects.len())
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
    let facts = raw
        .receipts
        .iter()
        .map(|receipt| receipt.metadata(false).to_string().len() + 1)
        .sum::<usize>()
        + raw
            .result
            .confirmed_effects
            .iter()
            .map(|effect| effect_json(effect).to_string().len() + 1)
            .sum::<usize>();
    if facts > NATIVE_PATCH_FACT_BYTES {
        return Err(invalid());
    }
    for (index, receipt) in raw.receipts.iter().enumerate() {
        if receipt.path.len() > 4096
            || receipt.resolved_path.len() > 4096
            || !std::path::Path::new(&receipt.resolved_path).is_absolute()
            || receipt.added_lines
                > mez_agent::semantic_patch_planning::native::NATIVE_PATCH_FILE_BYTES
            || receipt.removed_lines
                > mez_agent::semantic_patch_planning::native::NATIVE_PATCH_FILE_BYTES
            || !receipt.matches_effect(effects.get(receipt.operation_ordinal))
            || !receipt.can_follow(raw.receipts[..index].iter())
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
                receipt.is_file()
                    && receipt.operation_ordinal == *ordinal
                    && receipt.endpoint_ordinal == endpoint
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
