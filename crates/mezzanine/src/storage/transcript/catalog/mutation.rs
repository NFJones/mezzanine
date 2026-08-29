//! Transactional catalog replacement used by migration and rebuild.

use mez_agent::AgentConversationKind;
use rusqlite::{Connection, TransactionBehavior, params};

use crate::error::{MezError, Result};

use super::CatalogCandidate;
use super::schema::sqlite_i64;

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
