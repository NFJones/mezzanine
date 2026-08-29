//! Transactional catalog replacement used by migration and rebuild.

use mez_agent::AgentConversationKind;
use rusqlite::{Connection, TransactionBehavior, params};

use crate::error::{MezError, Result};

use super::CatalogCandidate;
use super::schema::sqlite_i64;

/// Upserts one payload-derived row while preserving an existing durable name.
pub(super) fn upsert(
    connection: &Connection,
    candidate: &CatalogCandidate,
    now_unix_seconds: u64,
) -> Result<()> {
    let entry_count = u64::try_from(candidate.summary.entries).map_err(|_| {
        MezError::invalid_args(
            "saved-conversation catalog entry count exceeds unsigned integer range",
        )
    })?;
    connection.execute(
        "INSERT INTO saved_conversations (
             conversation_id, conversation_kind, name, named_at,
             entry_count, first_created_at, last_created_at,
             last_turn_id, agent_id, pane_id, directory,
             initial_prompt, latest_user_prompt, has_transcript,
             has_presentation, payload_layout, catalog_updated_at
         ) VALUES (
             ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
             ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17
         )
         ON CONFLICT(conversation_id) DO UPDATE SET
             conversation_kind = excluded.conversation_kind,
             name = COALESCE(excluded.name, saved_conversations.name),
             named_at = COALESCE(excluded.named_at, saved_conversations.named_at),
             entry_count = excluded.entry_count,
             first_created_at = excluded.first_created_at,
             last_created_at = excluded.last_created_at,
             last_turn_id = excluded.last_turn_id,
             agent_id = excluded.agent_id,
             pane_id = excluded.pane_id,
             directory = COALESCE(excluded.directory, saved_conversations.directory),
             initial_prompt = excluded.initial_prompt,
             latest_user_prompt = excluded.latest_user_prompt,
             has_transcript = excluded.has_transcript,
             has_presentation = excluded.has_presentation,
             payload_layout = excluded.payload_layout,
             archived_at = NULL,
             archive_compressed_bytes = NULL,
             archive_sha256 = NULL,
             catalog_updated_at = excluded.catalog_updated_at",
        params![
            candidate.summary.conversation_id,
            conversation_kind_name(candidate.conversation_kind),
            candidate.name,
            candidate
                .named_at_unix_seconds
                .map(|value| sqlite_i64(value, "naming timestamp"))
                .transpose()?,
            sqlite_i64(entry_count, "entry count")?,
            sqlite_i64(
                candidate.summary.first_created_at_unix_seconds,
                "first activity timestamp"
            )?,
            sqlite_i64(
                candidate.summary.last_created_at_unix_seconds,
                "last activity timestamp"
            )?,
            candidate.summary.last_turn_id,
            candidate.summary.agent_id,
            candidate.summary.pane_id,
            candidate.summary.directory,
            candidate.summary.initial_prompt,
            candidate.summary.latest_user_prompt,
            i64::from(candidate.has_transcript),
            i64::from(candidate.has_presentation),
            candidate.payload_layout.as_str(),
            sqlite_i64(now_unix_seconds, "catalog update timestamp")?,
        ],
    )?;
    Ok(())
}

/// Sets one name on an existing or freshly inserted catalog row.
pub(super) fn set_name(
    connection: &Connection,
    conversation_id: &str,
    name: &str,
    named_at_unix_seconds: u64,
) -> Result<()> {
    let changed = connection.execute(
        "UPDATE saved_conversations SET name = ?2, named_at = ?3
         WHERE conversation_id = ?1",
        params![
            conversation_id,
            name,
            sqlite_i64(named_at_unix_seconds, "naming timestamp")?
        ],
    )?;
    if changed != 1 {
        return Err(MezError::invalid_state(
            "saved-conversation catalog row missing during name update",
        ));
    }
    Ok(())
}

/// Clears one optional name while preserving its payload metadata.
pub(super) fn clear_name(connection: &Connection, conversation_id: &str) -> Result<()> {
    connection.execute(
        "UPDATE saved_conversations SET name = NULL, named_at = NULL
         WHERE conversation_id = ?1",
        [conversation_id],
    )?;
    Ok(())
}

/// Deletes one exact discovery row.
pub(super) fn delete(connection: &Connection, conversation_id: &str) -> Result<()> {
    connection.execute(
        "DELETE FROM saved_conversations WHERE conversation_id = ?1",
        [conversation_id],
    )?;
    Ok(())
}

/// Replaces catalog contents with one verified canonical migration snapshot.
pub(super) fn replace_all(
    connection: &mut Connection,
    candidates: &[CatalogCandidate],
    now_unix_seconds: u64,
) -> Result<()> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute("DELETE FROM saved_conversations", [])?;
    {
        let mut statement = transaction.prepare(
            "INSERT INTO saved_conversations (
                 conversation_id, conversation_kind, name, named_at,
                 entry_count, first_created_at, last_created_at,
                 last_turn_id, agent_id, pane_id, directory,
                 initial_prompt, latest_user_prompt, has_transcript,
                 has_presentation, payload_layout, catalog_updated_at
             ) VALUES (
                 ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                 ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17
             )",
        )?;
        for candidate in candidates {
            let entry_count = u64::try_from(candidate.summary.entries).map_err(|_| {
                MezError::invalid_args(
                    "saved-conversation catalog entry count exceeds unsigned integer range",
                )
            })?;
            statement.execute(params![
                candidate.summary.conversation_id,
                conversation_kind_name(candidate.conversation_kind),
                candidate.name,
                candidate
                    .named_at_unix_seconds
                    .map(|value| sqlite_i64(value, "naming timestamp"))
                    .transpose()?,
                sqlite_i64(entry_count, "entry count")?,
                sqlite_i64(
                    candidate.summary.first_created_at_unix_seconds,
                    "first activity timestamp"
                )?,
                sqlite_i64(
                    candidate.summary.last_created_at_unix_seconds,
                    "last activity timestamp"
                )?,
                candidate.summary.last_turn_id,
                candidate.summary.agent_id,
                candidate.summary.pane_id,
                candidate.summary.directory,
                candidate.summary.initial_prompt,
                candidate.summary.latest_user_prompt,
                i64::from(candidate.has_transcript),
                i64::from(candidate.has_presentation),
                candidate.payload_layout.as_str(),
                sqlite_i64(now_unix_seconds, "catalog update timestamp")?,
            ])?;
        }
    }
    verify_snapshot(&transaction, candidates)?;
    transaction.commit()?;
    Ok(())
}

/// Verifies counts and classifications before the import transaction commits.
fn verify_snapshot(
    transaction: &rusqlite::Transaction<'_>,
    candidates: &[CatalogCandidate],
) -> Result<()> {
    let actual: i64 =
        transaction.query_row("SELECT COUNT(*) FROM saved_conversations", [], |row| {
            row.get(0)
        })?;
    let expected = i64::try_from(candidates.len()).map_err(|_| {
        MezError::invalid_args("saved-conversation migration candidate count is too large")
    })?;
    if actual != expected {
        return Err(MezError::invalid_state(format!(
            "saved-conversation catalog migration count mismatch: expected {expected}, imported {actual}"
        )));
    }

    let expected_named = candidates
        .iter()
        .filter(|candidate| candidate.name.is_some())
        .count();
    let actual_named: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM saved_conversations WHERE name IS NOT NULL",
        [],
        |row| row.get(0),
    )?;
    if actual_named != i64::try_from(expected_named).unwrap_or(i64::MAX) {
        return Err(MezError::invalid_state(
            "saved-conversation catalog migration lost name metadata",
        ));
    }

    let expected_subagents = candidates
        .iter()
        .filter(|candidate| candidate.conversation_kind == AgentConversationKind::Subagent)
        .count();
    let actual_subagents: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM saved_conversations WHERE conversation_kind = 'subagent'",
        [],
        |row| row.get(0),
    )?;
    if actual_subagents != i64::try_from(expected_subagents).unwrap_or(i64::MAX) {
        return Err(MezError::invalid_state(
            "saved-conversation catalog migration lost conversation classification",
        ));
    }
    Ok(())
}

/// Returns the stable catalog representation of a conversation kind.
fn conversation_kind_name(kind: AgentConversationKind) -> &'static str {
    match kind {
        AgentConversationKind::Root => "root",
        AgentConversationKind::Subagent => "subagent",
    }
}
