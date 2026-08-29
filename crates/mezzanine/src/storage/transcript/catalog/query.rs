//! Indexed saved-conversation catalog queries.

use mez_agent::AgentConversationKind;
use mez_agent::transcript::{ConversationSummary, validate_conversation_id};
use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{MezError, Result};

use super::super::types::SavedAgentSession;
use super::{CatalogPayloadLayout, CatalogRecord};

/// Loads one catalog row by its exact durable identity.
pub(super) fn record(
    connection: &Connection,
    conversation_id: &str,
) -> Result<Option<CatalogRecord>> {
    validate_conversation_id(conversation_id)?;
    let mut statement = connection.prepare(
        "SELECT conversation_kind, name, entry_count,
                first_created_at, last_created_at, last_turn_id,
                agent_id, pane_id, directory, initial_prompt,
                latest_user_prompt, has_transcript, has_presentation,
                payload_layout
         FROM saved_conversations
         WHERE conversation_id = ?1",
    )?;
    statement
        .query_row([conversation_id], |row| decode_record(row, conversation_id))
        .optional()
        .map_err(Into::into)
}

/// Loads the most recently active root conversation with an indexed query.
pub(super) fn latest_root_record(connection: &Connection) -> Result<Option<CatalogRecord>> {
    let mut statement = connection.prepare(
        "SELECT conversation_id, conversation_kind, name, entry_count,
                first_created_at, last_created_at, last_turn_id,
                agent_id, pane_id, directory, initial_prompt,
                latest_user_prompt, has_transcript, has_presentation,
                payload_layout
         FROM saved_conversations
         WHERE conversation_kind = 'root'
         ORDER BY last_created_at DESC, first_created_at DESC, conversation_id ASC
         LIMIT 1",
    )?;
    statement
        .query_row([], |row| {
            let conversation_id: String = row.get(0)?;
            decode_record_offset(row, &conversation_id, 1)
        })
        .optional()
        .map_err(Into::into)
}

/// Lists all saved sessions for temporary compatibility callers.
pub(super) fn saved_sessions(connection: &Connection) -> Result<Vec<SavedAgentSession>> {
    let mut statement = connection.prepare(
        "SELECT conversation_id, conversation_kind, name, entry_count,
                first_created_at, last_created_at, last_turn_id,
                agent_id, pane_id, directory, initial_prompt,
                latest_user_prompt, has_transcript, has_presentation,
                payload_layout
         FROM saved_conversations
         ORDER BY conversation_id ASC",
    )?;
    statement
        .query_map([], |row| {
            let conversation_id: String = row.get(0)?;
            Ok(decode_record_offset(row, &conversation_id, 1)?.session)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

/// Lists transcript-backed summaries for the legacy compatibility API.
pub(super) fn transcript_summaries(connection: &Connection) -> Result<Vec<ConversationSummary>> {
    let mut statement = connection.prepare(
        "SELECT conversation_id, conversation_kind, name, entry_count,
                first_created_at, last_created_at, last_turn_id,
                agent_id, pane_id, directory, initial_prompt,
                latest_user_prompt, has_transcript, has_presentation,
                payload_layout
         FROM saved_conversations
         WHERE has_transcript = 1
         ORDER BY conversation_id ASC",
    )?;
    statement
        .query_map([], |row| {
            let conversation_id: String = row.get(0)?;
            Ok(decode_record_offset(row, &conversation_id, 1)?
                .session
                .summary)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

/// Selects only the oldest unnamed payload-backed rows beyond `limit`.
pub(super) fn unnamed_prune_candidates(
    connection: &Connection,
    limit: usize,
) -> Result<Vec<String>> {
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM saved_conversations
         WHERE name IS NULL AND (has_transcript = 1 OR has_presentation = 1)",
        [],
        |row| row.get(0),
    )?;
    let limit = i64::try_from(limit)
        .map_err(|_| MezError::invalid_args("saved-session limit exceeds SQLite range"))?;
    if count <= limit {
        return Ok(Vec::new());
    }
    let excess = count - limit;
    let mut statement = connection.prepare(
        "SELECT conversation_id FROM saved_conversations
         WHERE name IS NULL AND (has_transcript = 1 OR has_presentation = 1)
         ORDER BY last_created_at ASC, first_created_at ASC, conversation_id ASC
         LIMIT ?1",
    )?;
    statement
        .query_map([excess], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

/// Returns whether one exact catalog row currently has a durable name.
pub(super) fn is_named(connection: &Connection, conversation_id: &str) -> Result<bool> {
    validate_conversation_id(conversation_id)?;
    connection
        .query_row(
            "SELECT name IS NOT NULL FROM saved_conversations WHERE conversation_id = ?1",
            params![conversation_id],
            |row| row.get(0),
        )
        .optional()
        .map(|value| value.unwrap_or(false))
        .map_err(Into::into)
}

/// Decodes a record whose first selected column is the conversation kind.
fn decode_record(
    row: &rusqlite::Row<'_>,
    conversation_id: &str,
) -> rusqlite::Result<CatalogRecord> {
    decode_record_offset(row, conversation_id, 0)
}

/// Decodes one catalog row beginning at `offset` after its id column.
fn decode_record_offset(
    row: &rusqlite::Row<'_>,
    conversation_id: &str,
    offset: usize,
) -> rusqlite::Result<CatalogRecord> {
    let kind = match row.get::<_, String>(offset)?.as_str() {
        "root" => AgentConversationKind::Root,
        "subagent" => AgentConversationKind::Subagent,
        _ => return Err(conversion_error(offset, "invalid conversation kind")),
    };
    let payload_layout = match row.get::<_, String>(offset + 13)?.as_str() {
        "directory" => CatalogPayloadLayout::Directory,
        "legacy-tsv" => CatalogPayloadLayout::LegacyTsv,
        _ => return Err(conversion_error(offset + 13, "invalid payload layout")),
    };
    Ok(CatalogRecord {
        session: SavedAgentSession {
            summary: ConversationSummary {
                conversation_id: conversation_id.to_string(),
                entries: row_usize(row, offset + 2)?,
                first_created_at_unix_seconds: row_u64(row, offset + 3)?,
                last_created_at_unix_seconds: row_u64(row, offset + 4)?,
                last_turn_id: row.get(offset + 5)?,
                agent_id: row.get(offset + 6)?,
                pane_id: row.get(offset + 7)?,
                directory: row.get(offset + 8)?,
                initial_prompt: row.get(offset + 9)?,
                latest_user_prompt: row.get(offset + 10)?,
            },
            name: row.get(offset + 1)?,
            conversation_kind: kind,
        },
        has_transcript: row.get(offset + 11)?,
        has_presentation: row.get(offset + 12)?,
        payload_layout,
    })
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
