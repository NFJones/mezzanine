//! Ordinary daemon startup isolates refused transcript owners before hydration.
//!
//! No failed conversation is installed, repaired, acknowledged or deleted. The
//! original complete active checkpoint is backed up before later checkpoints can
//! replace its bindings. Qualified native restoration uses strict recovery and
//! never enters this fallback. A user shell may start a fresh conversation;
//! explicit resume of the refused source still performs ordinary integrity checks.

use super::*;

impl RuntimeSessionService {
    /// Prepares independently recoverable ordinary bindings and emits explicit
    /// diagnostics for withheld source. Persistent children of withheld parents
    /// are withheld too, without creating replacement parent/native ownership.
    pub(super) fn prepare_startup_transcript_restore(
        &mut self,
        store: &AgentTranscriptStore,
        records: Vec<AgentSessionMetadata>,
    ) -> Result<Vec<AgentSessionMetadata>> {
        let mut failures = store.recover_append_receipts_for_startup()?;
        for record in &records {
            if let Err(error) = store.require_no_startup_refusal(&record.conversation_id) {
                failures
                    .entry(record.conversation_id.clone())
                    .or_insert(error);
            }
        }
        if failures.is_empty() {
            return Ok(records);
        }
        loop {
            let before = failures.len();
            for record in &records {
                if failures.contains_key(&record.conversation_id)
                    || !record.persistent_subagent_owned
                {
                    continue;
                }
                if let Some(parent) =
                    store.persistent_subagent_parent_conversation(&record.conversation_id)?
                    && failures.contains_key(&parent)
                {
                    failures.insert(
                        record.conversation_id.clone(),
                        MezError::conflict("persistent parent transcript recovery was refused"),
                    );
                }
            }
            if failures.len() == before {
                break;
            }
        }
        let backup = if records
            .iter()
            .any(|record| failures.contains_key(&record.conversation_id))
        {
            Some(store.preserve_refused_startup_metadata(self.session.id.as_str(), &records)?)
        } else {
            None
        };
        for conversation in failures.keys() {
            store.retain_startup_refusal(conversation)?;
        }
        for (conversation, error) in &failures {
            let notice = format!(
                "agent: conversation {conversation} was not restored: {error}; saved transcript and append receipts retained; start a new conversation or repair the saved source before resuming"
            );
            self.append_runtime_diagnostic_event(
                serde_json::json!({
                    "kind": "transcript_startup_recovery_refused",
                    "conversation_id": conversation,
                    "detail": notice,
                    "metadata_backup": backup.as_ref().map(|path| path.display().to_string()),
                })
                .to_string(),
            )?;
            let pane = records
                .iter()
                .find(|record| &record.conversation_id == conversation)
                .map(|record| record.pane_id.clone())
                .filter(|pane| runtime_pane_by_id(&self.session, pane).is_ok())
                .or_else(|| self.active_pane_id().ok());
            if let Some(pane) = pane {
                self.append_agent_status_text_to_terminal_buffer(&pane, &notice)?;
            }
        }
        Ok(records
            .into_iter()
            .filter(|record| !failures.contains_key(&record.conversation_id))
            .collect())
    }
}
