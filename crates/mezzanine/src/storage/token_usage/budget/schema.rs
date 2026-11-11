//! Transactional schema addition. Full unsigned counters/revisions are encoded
//! in bounded typed JSON, never narrowed to SQLite's signed integer range.

use super::*;

pub(super) fn initialize_schema(transaction: &rusqlite::Transaction<'_>) -> Result<()> {
    transaction.execute_batch(
        "CREATE TABLE budget_namespace (
            singleton INTEGER PRIMARY KEY CHECK(singleton=1), namespace TEXT NOT NULL UNIQUE CHECK(length(namespace) BETWEEN 1 AND 256));
         CREATE TABLE budget_roots (
            id TEXT PRIMARY KEY NOT NULL CHECK(length(id) BETWEEN 1 AND 256),
            body TEXT NOT NULL CHECK(length(CAST(body AS BLOB))<=4096));
         CREATE TABLE budget_memberships (
            conversation TEXT PRIMARY KEY NOT NULL CHECK(length(conversation) BETWEEN 1 AND 256),
            root TEXT NOT NULL REFERENCES budget_roots(id),
            operation TEXT NOT NULL UNIQUE CHECK(length(operation) BETWEEN 1 AND 256),
            state INTEGER NOT NULL CHECK(state IN (0,1,2)),
            body TEXT NOT NULL CHECK(length(CAST(body AS BLOB))<=4096));
         CREATE INDEX budget_memberships_root ON budget_memberships(root,state);
         CREATE INDEX budget_memberships_state ON budget_memberships(state);
         CREATE TABLE budget_attempts (
            id TEXT PRIMARY KEY NOT NULL CHECK(length(id) BETWEEN 1 AND 256),
            root TEXT NOT NULL REFERENCES budget_roots(id),
            conversation TEXT NOT NULL REFERENCES budget_memberships(conversation),
            body TEXT NOT NULL CHECK(length(CAST(body AS BLOB))<=4096),
            snapshot TEXT NOT NULL CHECK(length(CAST(snapshot AS BLOB))<=4096),
            terminal INTEGER NOT NULL CHECK(terminal IN (0,1)));
         CREATE INDEX budget_attempts_unsettled ON budget_attempts(root,terminal);
         CREATE INDEX budget_attempts_terminal ON budget_attempts(terminal);
         CREATE TABLE budget_receipts (
            id TEXT PRIMARY KEY NOT NULL CHECK(length(id) BETWEEN 1 AND 256),
            attempt TEXT NOT NULL REFERENCES budget_attempts(id),
            body TEXT NOT NULL CHECK(length(CAST(body AS BLOB))<=4096),
            outcome TEXT NOT NULL CHECK(length(CAST(outcome AS BLOB))<=4096));
         CREATE TABLE budget_operations (
            id TEXT PRIMARY KEY NOT NULL CHECK(length(id) BETWEEN 1 AND 256),
            root TEXT NOT NULL REFERENCES budget_roots(id),
            body TEXT NOT NULL CHECK(length(CAST(body AS BLOB))<=4096),
            outcome TEXT NOT NULL CHECK(length(CAST(outcome AS BLOB))<=4096));
         CREATE TABLE budget_bindings (
            conversation TEXT PRIMARY KEY NOT NULL REFERENCES budget_memberships(conversation),
            owner TEXT NOT NULL CHECK(length(owner) BETWEEN 1 AND 256));",
    )?;
    transaction.execute(
        "INSERT INTO budget_namespace(singleton,namespace) VALUES(1,?1)",
        [super::super::new_token_usage_event_id()],
    )?;
    Ok(())
}
