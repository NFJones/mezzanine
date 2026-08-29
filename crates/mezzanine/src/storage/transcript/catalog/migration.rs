//! One-time import and explicit rebuild orchestration.

use rusqlite::Connection;

use crate::error::Result;

use super::super::types::AgentTranscriptStore;
use super::mutation;

/// Reconstructs and transactionally imports one canonical filesystem snapshot.
pub(super) fn import(
    store: &AgentTranscriptStore,
    connection: &mut Connection,
    now_unix_seconds: u64,
) -> Result<()> {
    let candidates = store.catalog_migration_candidates()?;
    mutation::replace_all(connection, &candidates, now_unix_seconds)
}
