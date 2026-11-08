//! Historical terminal-certificate integrity at ordinary append admission.
//! Existing recovery evidence keeps exact source/ordinal checks. New compaction
//! results use the canonical publisher and never create terminal certificates.

use super::*;

impl RuntimeSessionService {
    /// Checks the complete logical archive before accepting terminal audit
    /// evidence. Interleaved display rows cannot enter the existing selective
    /// epoch contract; explicitly retain raw replay instead of committing a
    /// permanently unreconcilable witness. Conflicting source remains an error.
    pub(in crate::runtime) fn check_terminal_compaction_handoff_admission(
        &mut self,
        turn: &AgentTurnRecord,
        history: &[TranscriptEntry],
        entries: &mut [TranscriptEntry],
    ) -> Result<()> {
        let mut logical = history.to_vec();
        logical.extend_from_slice(entries);
        for entry in entries {
            if Self::admit_terminal_compaction_handoff_entry(entry, &logical)? {
                if self
                    .agent_shell_store()
                    .get(&turn.pane_id)
                    .is_some_and(|session| session.session_id == turn.conversation_id)
                {
                    self.append_agent_status_text_to_terminal_buffer(&turn.pane_id,
                        "agent: terminal compaction handoff unsupported for interleaved or split execution-group archive source; raw replay remains authoritative")?;
                    self.append_agent_trace_turn_event(&turn.pane_id, &turn.turn_id,
                        "context_compaction terminal_handoff supported=false reason=noncontiguous_archive_source")?;
                } else {
                    self.append_lifecycle_event(crate::runtime::EventKind::Diagnostic, serde_json::json!({
                        "kind": "terminal_compaction_handoff", "conversation_id": turn.conversation_id,
                        "turn_id": turn.turn_id, "supported": false, "reason": "noncontiguous_archive_source",
                    }).to_string())?;
                }
            }
        }
        Ok(())
    }

    /// Checks detached original-archive evidence without pane or diagnostic I/O.
    /// Required malformed/conflicting source errors are unchanged; unsupported
    /// interleaving retains the exact existing raw-authority marker rewrite.
    pub(in crate::runtime) fn check_detached_terminal_compaction_handoff_admission(
        history: &[TranscriptEntry],
        entries: &mut [TranscriptEntry],
    ) -> Result<()> {
        let mut logical = history.to_vec();
        logical.extend_from_slice(entries);
        for entry in entries {
            Self::admit_terminal_compaction_handoff_entry(entry, &logical)?;
        }
        Ok(())
    }

    /// Pure shared validation/transformation for one prospective certificate.
    /// Returns true only when this exact entry becomes the unsupported marker.
    /// The complete prospective archive is required even for per-entry callers;
    /// no live pane identity, presentation, trace, or lifecycle diagnostic enters
    /// this decision. Legacy callers project each returned fact independently.
    fn admit_terminal_compaction_handoff_entry(
        entry: &mut TranscriptEntry,
        logical: &[TranscriptEntry],
    ) -> Result<bool> {
        use crate::storage::transcript::compaction_handoff::{
            MARKER, terminal_handoff_source_is_contiguous,
        };
        if entry.role != TranscriptRole::System || !entry.content.starts_with(MARKER) {
            return Ok(false);
        }
        if terminal_handoff_source_is_contiguous(&entry.content, logical)? {
            return Ok(false);
        }
        entry.content =
            "[mez-terminal-compaction-handoff/unsupported]\nreason=noncontiguous_archive_source"
                .to_string();
        Ok(true)
    }
}
