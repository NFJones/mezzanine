//! Indexed saved-conversation catalog queries for focused migration tests.
//!
//! Phase-one migration uses exact lookup for validation and focused tests. The
//! subsequent persistence phase moves production resume and pruning callers to
//! these intent-specific queries.

use mez_agent::AgentConversationKind;
use mez_agent::transcript::{ConversationSummary, validate_conversation_id};
use rusqlite::{Connection, OptionalExtension};

use crate::error::{MezError, Result};

use super::super::types::SavedAgentSession;

/// Loads one saved-session record by its exact durable identity.
pub(super) fn saved_session(
    connection: &Connection,
    conversation_id: &str,
) -> Result<Option<SavedAgentSession>> {
    validate_conversation_id(conversation_id)?;
    let mut statement = connection.prepare(
        "SELECT conversation_kind, name, entry_count,
                first_created_at, last_created_at, last_turn_id,
                agent_id, pane_id, directory, initial_prompt,
                latest_user_prompt
         FROM saved_conversations
         WHERE conversation_id = ?1",
    )?;
    statement
        .query_row([conversation_id], |row| {
            let kind = match row.get::<_, String>(0)?.as_str() {
                "root" => AgentConversationKind::Root,
                "subagent" => AgentConversationKind::Subagent,
                _ => return Err(conversion_error(0, "invalid conversation kind")),
            };
            Ok(SavedAgentSession {
                summary: ConversationSummary {
                    conversation_id: conversation_id.to_string(),
                    entries: row_usize(row, 2)?,
                    first_created_at_unix_seconds: row_u64(row, 3)?,
                    last_created_at_unix_seconds: row_u64(row, 4)?,
                    last_turn_id: row.get(5)?,
                    agent_id: row.get(6)?,
                    pane_id: row.get(7)?,
                    directory: row.get(8)?,
                    initial_prompt: row.get(9)?,
                    latest_user_prompt: row.get(10)?,
                },
                name: row.get(1)?,
                conversation_kind: kind,
            })
        })
        .optional()
        .map_err(Into::into)
}

/// Converts a non-negative SQLite integer into `u64`.
fn row_u64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|_| conversion_error(index, "negative catalog integer"))
}

/// Converts a non-negative SQLite integer into the platform `usize` domain.
fn row_usize(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<usize> {
    let value = row_u64(row, index)?;
    usize::try_from(value).map_err(|_| conversion_error(index, "catalog integer is too large"))
}

/// Builds one typed SQLite conversion failure for corrupt catalog rows.
fn conversion_error(index: usize, message: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Integer,
        Box::new(MezError::invalid_args(message)),
    )
}
