//! Indexed saved-conversation catalog queries.

use mez_agent::AgentConversationKind;
use mez_agent::transcript::{ConversationSummary, validate_conversation_id};
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};

use crate::error::{MezError, Result};

use super::super::types::{
    SavedAgentSession, SavedSessionLifecycleFilter, SavedSessionPage, SavedSessionPageAnchor,
    SavedSessionQuery,
};
use super::schema::sqlite_i64;
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
                payload_layout, archived_at, archive_compressed_bytes,
                archive_sha256
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
                payload_layout, archived_at, archive_compressed_bytes,
                archive_sha256
         FROM saved_conversations
         WHERE conversation_kind = 'root' AND archived_at IS NULL
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
#[cfg(test)]
pub(super) fn saved_sessions(connection: &Connection) -> Result<Vec<SavedAgentSession>> {
    let mut statement = connection.prepare(
        "SELECT conversation_id, conversation_kind, name, entry_count,
                first_created_at, last_created_at, last_turn_id,
                agent_id, pane_id, directory, initial_prompt,
                latest_user_prompt, has_transcript, has_presentation,
                payload_layout, archived_at, archive_compressed_bytes,
                archive_sha256
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
#[cfg(test)]
pub(super) fn transcript_summaries(connection: &Connection) -> Result<Vec<ConversationSummary>> {
    let mut statement = connection.prepare(
        "SELECT conversation_id, conversation_kind, name, entry_count,
                first_created_at, last_created_at, last_turn_id,
                agent_id, pane_id, directory, initial_prompt,
                latest_user_prompt, has_transcript, has_presentation,
                payload_layout, archived_at, archive_compressed_bytes,
                archive_sha256
         FROM saved_conversations
         WHERE has_transcript = 1 AND archived_at IS NULL
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

/// Selects one bounded oldest-first age-expiry batch outside the protected set.
pub(super) fn age_retention_candidates(
    connection: &Connection,
    cutoff_unix_seconds: u64,
    excluded_conversation_ids: &std::collections::BTreeSet<String>,
    limit: usize,
) -> Result<Vec<String>> {
    retention_candidates(
        connection,
        Some(cutoff_unix_seconds),
        excluded_conversation_ids,
        limit,
    )
}

/// Counts every active payload-backed session, including protected rows.
pub(super) fn active_payload_session_count(connection: &Connection) -> Result<usize> {
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM saved_conversations
         WHERE archived_at IS NULL
           AND (has_transcript = 1 OR has_presentation = 1)",
        [],
        |row| row.get(0),
    )?;
    usize::try_from(count)
        .map_err(|_| MezError::invalid_state("saved-session catalog count is invalid"))
}

/// Selects one bounded oldest-first count-enforcement batch outside the protected set.
pub(super) fn count_retention_candidates(
    connection: &Connection,
    excluded_conversation_ids: &std::collections::BTreeSet<String>,
    limit: usize,
) -> Result<Vec<String>> {
    retention_candidates(connection, None, excluded_conversation_ids, limit)
}

/// Builds one indexed active payload query with dynamic protected-id exclusion.
fn retention_candidates(
    connection: &Connection,
    cutoff_unix_seconds: Option<u64>,
    excluded_conversation_ids: &std::collections::BTreeSet<String>,
    limit: usize,
) -> Result<Vec<String>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut sql = String::from(
        "SELECT conversation_id FROM saved_conversations
         WHERE archived_at IS NULL
           AND (has_transcript = 1 OR has_presentation = 1)",
    );
    let mut values = Vec::<Value>::new();
    if let Some(cutoff) = cutoff_unix_seconds {
        sql.push_str(" AND last_created_at <= ?");
        values.push(Value::Integer(sqlite_i64(cutoff, "retention cutoff")?));
    }
    if !excluded_conversation_ids.is_empty() {
        sql.push_str(" AND conversation_id NOT IN (");
        sql.push_str(
            &std::iter::repeat_n("?", excluded_conversation_ids.len())
                .collect::<Vec<_>>()
                .join(", "),
        );
        sql.push(')');
        values.extend(excluded_conversation_ids.iter().cloned().map(Value::Text));
    }
    sql.push_str(
        " ORDER BY last_created_at ASC, first_created_at ASC, conversation_id ASC LIMIT ?",
    );
    values.push(Value::Integer(bounded_limit(limit)?));
    let mut statement = connection.prepare(&sql)?;
    statement
        .query_map(params_from_iter(values), |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

/// Returns bounded root-session completion records for one UUID prefix.
pub(super) fn root_session_completions(
    connection: &Connection,
    prefix: &str,
    limit: usize,
) -> Result<Vec<SavedAgentSession>> {
    let limit = bounded_limit(limit)?;
    let mut statement = connection.prepare(
        "SELECT conversation_id, conversation_kind, name, entry_count,
                first_created_at, last_created_at, last_turn_id,
                agent_id, pane_id, directory, initial_prompt,
                latest_user_prompt, has_transcript, has_presentation,
                payload_layout, archived_at, archive_compressed_bytes,
                archive_sha256
         FROM saved_conversations
         WHERE conversation_kind = 'root'
           AND archived_at IS NULL
           AND conversation_id LIKE ?1 ESCAPE '\\' COLLATE NOCASE
         ORDER BY last_created_at DESC, first_created_at DESC, conversation_id ASC
         LIMIT ?2",
    )?;
    let pattern = format!("{}%", escape_like(prefix));
    statement
        .query_map(params![pattern, limit], |row| {
            let conversation_id: String = row.get(0)?;
            Ok(decode_record_offset(row, &conversation_id, 1)?.session)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

/// Returns one bounded keyset page in named-first picker order.
pub(super) fn query_saved_sessions(
    connection: &Connection,
    query: &SavedSessionQuery,
) -> Result<SavedSessionPage> {
    let limit = bounded_limit(query.limit)?;
    let backwards = matches!(
        query.anchor,
        Some(
            SavedSessionPageAnchor::Before(_)
                | SavedSessionPageAnchor::At(_)
                | SavedSessionPageAnchor::Last
        )
    );
    let mut sql = String::from(
        "SELECT conversation_id, conversation_kind, name, entry_count,
                first_created_at, last_created_at, last_turn_id,
                agent_id, pane_id, directory, initial_prompt,
                latest_user_prompt, has_transcript, has_presentation,
                payload_layout, archived_at, archive_compressed_bytes,
                archive_sha256
         FROM saved_conversations WHERE 1 = 1",
    );
    let mut values = Vec::<Value>::new();
    match query.lifecycle {
        SavedSessionLifecycleFilter::Active => sql.push_str(" AND archived_at IS NULL"),
        SavedSessionLifecycleFilter::Archived => sql.push_str(" AND archived_at IS NOT NULL"),
    }
    if !query.include_subagents {
        sql.push_str(" AND conversation_kind = 'root'");
    }
    if query.require_latest_user_prompt {
        sql.push_str(" AND latest_user_prompt IS NOT NULL");
    }
    if let Some(directory) = query.directory.as_deref() {
        sql.push_str(" AND directory = ?");
        values.push(Value::Text(directory.to_string()));
    }
    if let Some(search) = query
        .search
        .as_deref()
        .map(str::trim)
        .filter(|search| !search.is_empty())
    {
        sql.push_str(
            " AND (conversation_id LIKE ? ESCAPE '\\' COLLATE NOCASE
                    OR name LIKE ? ESCAPE '\\' COLLATE NOCASE
                    OR latest_user_prompt LIKE ? ESCAPE '\\' COLLATE NOCASE
                    OR directory LIKE ? ESCAPE '\\' COLLATE NOCASE)",
        );
        let pattern = Value::Text(format!("%{}%", escape_like(search)));
        values.extend([pattern.clone(), pattern.clone(), pattern.clone(), pattern]);
    }
    if let Some(anchor) = query.anchor.as_ref()
        && !matches!(anchor, SavedSessionPageAnchor::Last)
    {
        let (cursor, comparison, inclusive) = match anchor {
            SavedSessionPageAnchor::After(cursor) => (cursor, "after", false),
            SavedSessionPageAnchor::Before(cursor) => (cursor, "before", false),
            SavedSessionPageAnchor::At(cursor) => (cursor, "before", true),
            SavedSessionPageAnchor::Last => unreachable!("last-page anchors have no cursor"),
        };
        let named = i64::from(cursor.named);
        if comparison == "after" {
            sql.push_str(
                " AND ((name IS NOT NULL) < ?
                    OR ((name IS NOT NULL) = ? AND last_created_at < ?)
                    OR ((name IS NOT NULL) = ? AND last_created_at = ? AND first_created_at < ?)
                    OR ((name IS NOT NULL) = ? AND last_created_at = ? AND first_created_at = ? AND conversation_id > ?))",
            );
        } else if inclusive {
            sql.push_str(
                " AND ((name IS NOT NULL) > ?
                    OR ((name IS NOT NULL) = ? AND last_created_at > ?)
                    OR ((name IS NOT NULL) = ? AND last_created_at = ? AND first_created_at > ?)
                    OR ((name IS NOT NULL) = ? AND last_created_at = ? AND first_created_at = ? AND conversation_id <= ?))",
            );
        } else {
            sql.push_str(
                " AND ((name IS NOT NULL) > ?
                    OR ((name IS NOT NULL) = ? AND last_created_at > ?)
                    OR ((name IS NOT NULL) = ? AND last_created_at = ? AND first_created_at > ?)
                    OR ((name IS NOT NULL) = ? AND last_created_at = ? AND first_created_at = ? AND conversation_id < ?))",
            );
        }
        values.extend([
            Value::Integer(named),
            Value::Integer(named),
            Value::Integer(sqlite_i64(
                cursor.last_created_at_unix_seconds,
                "cursor timestamp",
            )?),
            Value::Integer(named),
            Value::Integer(sqlite_i64(
                cursor.last_created_at_unix_seconds,
                "cursor timestamp",
            )?),
            Value::Integer(sqlite_i64(
                cursor.first_created_at_unix_seconds,
                "cursor timestamp",
            )?),
            Value::Integer(named),
            Value::Integer(sqlite_i64(
                cursor.last_created_at_unix_seconds,
                "cursor timestamp",
            )?),
            Value::Integer(sqlite_i64(
                cursor.first_created_at_unix_seconds,
                "cursor timestamp",
            )?),
            Value::Text(cursor.conversation_id.clone()),
        ]);
    }
    if backwards {
        sql.push_str(
            " ORDER BY (name IS NOT NULL) ASC, last_created_at ASC,
                       first_created_at ASC, conversation_id DESC",
        );
    } else {
        sql.push_str(
            " ORDER BY (name IS NOT NULL) DESC, last_created_at DESC,
                       first_created_at DESC, conversation_id ASC",
        );
    }
    sql.push_str(" LIMIT ?");
    values.push(Value::Integer(limit));
    let mut statement = connection.prepare(&sql)?;
    let mut sessions = statement
        .query_map(params_from_iter(values), |row| {
            let conversation_id: String = row.get(0)?;
            Ok(decode_record_offset(row, &conversation_id, 1)?.session)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if backwards {
        sessions.reverse();
    }
    Ok(SavedSessionPage { sessions })
}

/// Converts one bounded query limit into SQLite's integer domain.
fn bounded_limit(limit: usize) -> Result<i64> {
    if limit == 0 {
        return Err(MezError::invalid_args(
            "saved-session query limit must be greater than zero",
        ));
    }
    i64::try_from(limit.min(1_000))
        .map_err(|_| MezError::invalid_args("saved-session query limit exceeds SQLite range"))
}

/// Escapes one literal for a SQLite `LIKE ... ESCAPE '\\'` expression.
fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
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
            archived_at_unix_seconds: row_optional_u64(row, offset + 14)?,
            archive_compressed_bytes: row_optional_u64(row, offset + 15)?,
            archive_sha256: row.get(offset + 16)?,
        },
        has_transcript: row.get(offset + 11)?,
        has_presentation: row.get(offset + 12)?,
        payload_layout,
    })
}

/// Converts one optional non-negative SQLite integer into `u64`.
fn row_optional_u64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Option<u64>> {
    let value: Option<i64> = row.get(index)?;
    value
        .map(|value| {
            u64::try_from(value).map_err(|_| conversion_error(index, "negative catalog integer"))
        })
        .transpose()
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
