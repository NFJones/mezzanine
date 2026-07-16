//! Product persistence adapter for agent execution transcripts.
//!
//! Deterministic execution-to-transcript projection is owned by `mez-agent`.
//! This module supplies the product transcript store, sequence allocation, and
//! `MezError` projection used by runtime composition.

#[cfg(test)]
use super::super::{AgentTurnRecord, TranscriptEntry};
#[cfg(test)]
use mez_agent::{AgentTurnExecution, transcript_entries_for_execution};

use super::super::{MezError, Result, TranscriptPersistence};

/// Appends a completed bounded turn execution to the durable transcript store.
#[cfg(test)]
pub fn persist_turn_execution_transcript<P>(
    store: &P,
    conversation_id: &str,
    created_at_unix_seconds: u64,
    turn: &AgentTurnRecord,
    execution: &AgentTurnExecution,
) -> Result<Vec<TranscriptEntry>>
where
    P: TranscriptPersistence<Error = MezError>,
{
    let first_sequence = next_transcript_sequence(store, conversation_id)?;
    let entries = transcript_entries_for_execution(
        conversation_id,
        first_sequence,
        created_at_unix_seconds,
        turn,
        execution,
    )?;
    for entry in &entries {
        store.append(entry)?;
    }
    Ok(entries)
}

/// Returns the next one-based transcript sequence for a conversation.
pub fn next_transcript_sequence<P>(store: &P, conversation_id: &str) -> Result<u64>
where
    P: TranscriptPersistence<Error = MezError>,
{
    Ok(store.next_sequence(conversation_id)?.unwrap_or(1))
}
