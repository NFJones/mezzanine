//! Initial canonical ownership at the actual empty-history task boundary.
//!
//! The actor supplies its captured conversation and structural history boundary;
//! no label, archive count or summary can fabricate an initial source. Ownership
//! starts before provider execution so existing append/retirement ports retain
//! their original lifecycle fences. Publication still owns durable source backing
//! and acknowledgment. Nonempty raw/legacy migration has its own qualification.

use super::*;

pub(super) fn fresh_context(
    records: &RuntimeContextRecords,
    mut blocks: Vec<ContextBlock>,
    sources: &[(usize, mez_agent::StableContextSlotId)],
    execution: &[mez_agent::ImportedExecutionEvent],
    new_task: bool,
    conversation: &str,
) -> Result<AgentContext> {
    let prefix = blocks
        .iter()
        .take_while(|block| block.placement == mez_agent::ContextPlacement::StablePrefix)
        .count();
    let suffix = blocks.split_off(prefix);
    let prefix = AgentContext::import_durable_blocks_with_stable_sources(
        blocks,
        &[],
        records.stable.as_deref().unwrap_or_default(),
        sources,
    )?;
    let mut owner = mez_agent::ReplayEpochContext::new(
        mez_agent::ReplayConversationId::new(conversation)?,
        prefix,
    )?;
    if new_task {
        let prompt = suffix
            .iter()
            .position(|block| {
                block.source == mez_agent::ContextSourceKind::UserInstruction
                    && block.label == "user prompt"
            })
            .ok_or_else(|| MezError::conflict("initial task has no original user root"))?;
        owner.admit_instruction_task(suffix, &[], execution, prompt)?;
        return Ok(owner.into_owned_context());
    }
    let mut body = owner.into_owned_context();
    body.append_imported_history_with_execution(suffix, &[], execution)?;
    Ok(body)
}
