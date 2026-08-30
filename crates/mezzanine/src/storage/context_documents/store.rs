//! SQLite persistence for user-owned context documents.

use std::time::Duration;

use rand::Rng;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use super::{
    CompareAndSwapContextDocumentResult, ContextDocument, ContextDocumentScope,
    ContextDocumentSelection, MAX_INCLUDED_CONTEXT_DOCUMENT_BYTES, MAX_INCLUDED_CONTEXT_DOCUMENTS,
    MezError, PathBuf, Result, default_context_document_database_path, ensure_private_parent,
    set_private_file_permissions,
};

/// Private SQLite repository for persisted context source artifacts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContextDocumentStore {
    path: PathBuf,
}

impl ContextDocumentStore {
    pub(crate) fn under_config_root(config_root: impl AsRef<std::path::Path>) -> Self {
        Self {
            path: default_context_document_database_path(config_root),
        }
    }

    #[cfg(test)]
    pub(crate) fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub(crate) fn create(
        &self,
        scope: ContextDocumentScope,
        title: String,
        content: String,
        enabled: bool,
        now_unix_seconds: u64,
    ) -> Result<ContextDocument> {
        let document = ContextDocument {
            id: generate_document_id(),
            scope,
            title,
            content,
            enabled,
            created_at_unix_seconds: now_unix_seconds,
            updated_at_unix_seconds: now_unix_seconds,
        };
        document.validate()?;
        let connection = self.open()?;
        insert_document(&connection, &document)?;
        Ok(document)
    }

    pub(crate) fn inspect(&self, id: &str) -> Result<Option<ContextDocument>> {
        let connection = self.open()?;
        select_document(&connection, id)
    }

    pub(crate) fn list(&self) -> Result<Vec<ContextDocument>> {
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT id, scope, project_root, title, content, enabled, created_at, updated_at
             FROM context_documents
             ORDER BY CASE scope WHEN 'global' THEN 0 ELSE 1 END, project_root, id",
        )?;
        let rows = statement.query_map([], row_to_document)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(MezError::from)
    }

    pub(crate) fn select_enabled_for_project(
        &self,
        project: &str,
    ) -> Result<ContextDocumentSelection> {
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT id, scope, project_root, title, content, enabled, created_at, updated_at
             FROM context_documents
             WHERE enabled = 1 AND (scope = 'global' OR (scope = 'project' AND project_root = ?1))
             ORDER BY CASE scope WHEN 'global' THEN 0 ELSE 1 END, id",
        )?;
        let candidates = statement
            .query_map([project], row_to_document)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let total = candidates.len();
        let mut bytes = 0usize;
        let documents = candidates
            .into_iter()
            .take(MAX_INCLUDED_CONTEXT_DOCUMENTS)
            .take_while(|document| {
                let Some(next) = bytes.checked_add(document.content.len()) else {
                    return false;
                };
                if next > MAX_INCLUDED_CONTEXT_DOCUMENT_BYTES {
                    return false;
                }
                bytes = next;
                true
            })
            .collect::<Vec<_>>();
        Ok(ContextDocumentSelection {
            omitted: total.saturating_sub(documents.len()),
            documents,
        })
    }

    pub(crate) fn revision(&self, document: &ContextDocument) -> Result<String> {
        document.validate()?;
        Ok(document_revision(document))
    }

    pub(crate) fn set_enabled(
        &self,
        id: &str,
        enabled: bool,
        now_unix_seconds: u64,
    ) -> Result<Option<ContextDocument>> {
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(mut document) = select_document(&transaction, id)? else {
            transaction.commit()?;
            return Ok(None);
        };
        if enabled && document.content.is_empty() {
            return Err(MezError::invalid_args(
                "empty context documents cannot be enabled",
            ));
        }
        document.enabled = enabled;
        document.updated_at_unix_seconds = now_unix_seconds;
        document.validate()?;
        update_document(&transaction, &document)?;
        transaction.commit()?;
        Ok(Some(document))
    }

    pub(crate) fn compare_and_swap_content(
        &self,
        id: &str,
        expected_revision: &str,
        content: String,
        now_unix_seconds: u64,
    ) -> Result<CompareAndSwapContextDocumentResult> {
        validate_revision(expected_revision)?;
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(mut document) = select_document(&transaction, id)? else {
            transaction.commit()?;
            return Ok(CompareAndSwapContextDocumentResult::Deleted);
        };
        let current_revision = document_revision(&document);
        if current_revision != expected_revision {
            transaction.commit()?;
            return Ok(CompareAndSwapContextDocumentResult::Stale { current_revision });
        }
        document.content = content;
        document.updated_at_unix_seconds = now_unix_seconds;
        document.validate()?;
        update_document(&transaction, &document)?;
        transaction.commit()?;
        Ok(CompareAndSwapContextDocumentResult::Updated(Box::new(
            document,
        )))
    }

    pub(crate) fn delete(&self, id: &str) -> Result<bool> {
        let connection = self.open()?;
        Ok(connection.execute("DELETE FROM context_documents WHERE id = ?1", [id])? == 1)
    }

    fn open(&self) -> Result<Connection> {
        ensure_private_parent(&self.path)?;
        let connection = Connection::open(&self.path)?;
        connection.busy_timeout(Duration::from_millis(250))?;
        initialize_schema(&connection)?;
        set_private_file_permissions(&self.path)?;
        Ok(connection)
    }
}

fn initialize_schema(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;
         CREATE TABLE IF NOT EXISTS context_documents (
             id TEXT PRIMARY KEY NOT NULL,
             scope TEXT NOT NULL CHECK (scope IN ('global', 'project')),
             project_root TEXT,
             title TEXT NOT NULL,
             content TEXT NOT NULL,
             enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
             created_at INTEGER NOT NULL,
             updated_at INTEGER NOT NULL,
             CHECK ((scope = 'global' AND project_root IS NULL) OR
                    (scope = 'project' AND project_root IS NOT NULL))
         );
         CREATE INDEX IF NOT EXISTS context_documents_inclusion_idx
             ON context_documents(enabled, scope, project_root, id);",
    )?;
    Ok(())
}

fn insert_document(connection: &Connection, document: &ContextDocument) -> Result<()> {
    let (scope, project_root) = encode_scope(&document.scope);
    connection.execute(
        "INSERT INTO context_documents
         (id, scope, project_root, title, content, enabled, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            document.id,
            scope,
            project_root,
            document.title,
            document.content,
            document.enabled,
            sqlite_i64(document.created_at_unix_seconds)?,
            sqlite_i64(document.updated_at_unix_seconds)?,
        ],
    )?;
    Ok(())
}

fn update_document(connection: &Connection, document: &ContextDocument) -> Result<()> {
    let (scope, project_root) = encode_scope(&document.scope);
    connection.execute(
        "UPDATE context_documents SET scope = ?2, project_root = ?3, title = ?4,
         content = ?5, enabled = ?6, updated_at = ?7 WHERE id = ?1",
        params![
            document.id,
            scope,
            project_root,
            document.title,
            document.content,
            document.enabled,
            sqlite_i64(document.updated_at_unix_seconds)?,
        ],
    )?;
    Ok(())
}

fn select_document(connection: &Connection, id: &str) -> Result<Option<ContextDocument>> {
    connection
        .query_row(
            "SELECT id, scope, project_root, title, content, enabled, created_at, updated_at
             FROM context_documents WHERE id = ?1",
            [id],
            row_to_document,
        )
        .optional()
        .map_err(MezError::from)
}

fn row_to_document(row: &rusqlite::Row<'_>) -> rusqlite::Result<ContextDocument> {
    let scope: String = row.get(1)?;
    let project_root: Option<String> = row.get(2)?;
    Ok(ContextDocument {
        id: row.get(0)?,
        scope: decode_scope(&scope, project_root).map_err(conversion_error)?,
        title: row.get(3)?,
        content: row.get(4)?,
        enabled: row.get(5)?,
        created_at_unix_seconds: row_u64(row, 6)?,
        updated_at_unix_seconds: row_u64(row, 7)?,
    })
}

fn encode_scope(scope: &ContextDocumentScope) -> (&'static str, Option<&str>) {
    match scope {
        ContextDocumentScope::Global => ("global", None),
        ContextDocumentScope::Project { root } => ("project", Some(root)),
    }
}

fn decode_scope(scope: &str, project_root: Option<String>) -> Result<ContextDocumentScope> {
    match (scope, project_root) {
        ("global", None) => Ok(ContextDocumentScope::Global),
        ("project", Some(root)) => Ok(ContextDocumentScope::Project { root }),
        _ => Err(MezError::invalid_state(
            "persisted context document has invalid scope metadata",
        )),
    }
}

fn document_revision(document: &ContextDocument) -> String {
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, &document.id);
    match &document.scope {
        ContextDocumentScope::Global => hash_field(&mut hasher, "global"),
        ContextDocumentScope::Project { root } => {
            hash_field(&mut hasher, "project");
            hash_field(&mut hasher, root);
        }
    }
    hash_field(&mut hasher, &document.title);
    hash_field(&mut hasher, &document.content);
    hash_field(&mut hasher, if document.enabled { "1" } else { "0" });
    hash_field(&mut hasher, &document.created_at_unix_seconds.to_string());
    hash_field(&mut hasher, &document.updated_at_unix_seconds.to_string());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn hash_field(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
}

fn validate_revision(value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(MezError::invalid_args(
            "context document revision must be a lowercase SHA-256 digest",
        ));
    }
    Ok(())
}

fn generate_document_id() -> String {
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15],
    )
}

fn sqlite_i64(value: u64) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| MezError::invalid_args("context document timestamp is too large"))
}

fn row_u64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|_| {
        conversion_error(MezError::invalid_state(
            "context document timestamp is negative",
        ))
    })
}

fn conversion_error(error: MezError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}
