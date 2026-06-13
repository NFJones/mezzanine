//! SQLite persistence for local project issues.
//!
//! The store initializes its own schema, validates records before writes, and
//! keeps queries bounded. It deliberately uses exact project-key matching so
//! issue records from different repositories do not bleed across surfaces.

use rusqlite::{Connection, OptionalExtension, params};

use super::{
    DeleteIssueResult, IssueKind, IssueQuery, IssueRecord, IssueUpdate, MezError, Path, PathBuf,
    Result, UpdateIssueResult, ensure_private_parent, generate_issue_id,
    set_private_issue_file_permissions,
};

/// SQLite-backed local issue store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueStore {
    path: PathBuf,
}

impl IssueStore {
    /// Returns an issue store using the default database under a config root.
    pub fn under_config_root(config_root: impl Into<PathBuf>) -> Self {
        Self {
            path: super::default_issue_database_path(config_root.into()),
        }
    }

    /// Builds an issue store at an explicit SQLite database path.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Returns the SQLite database path used by this store.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Adds one issue record and returns the persisted value.
    pub fn add_issue(
        &self,
        project: String,
        kind: IssueKind,
        title: String,
        body: Option<String>,
        notes: Option<String>,
        now_unix_seconds: u64,
    ) -> Result<IssueRecord> {
        let record = IssueRecord::new(
            generate_issue_id(),
            project,
            kind,
            title,
            body,
            notes,
            now_unix_seconds,
        )?;
        let connection = self.open()?;
        insert_issue(&connection, &record)?;
        Ok(record)
    }

    /// Queries issues matching the provided filters.
    pub fn query_issues(&self, query: &IssueQuery) -> Result<Vec<IssueRecord>> {
        let connection = self.open()?;
        let mut sql = String::from(
            "SELECT id, project, kind, title, body, notes, created_at, updated_at FROM issues WHERE project = ?1",
        );
        let kind_name = query.kind.map(IssueKind::as_str);
        if kind_name.is_some() {
            sql.push_str(" AND kind = ?2");
        }
        let text = query
            .text
            .as_deref()
            .map(|value| format!("%{}%", escape_like(value)));
        if text.is_some() {
            if kind_name.is_some() {
                sql.push_str(" AND (title LIKE ?3 ESCAPE '\\' OR body LIKE ?3 ESCAPE '\\')");
            } else {
                sql.push_str(" AND (title LIKE ?2 ESCAPE '\\' OR body LIKE ?2 ESCAPE '\\')");
            }
        }
        sql.push_str(" ORDER BY updated_at DESC, created_at DESC, id ASC LIMIT ?");
        let limit_index = if kind_name.is_some() && text.is_some() {
            4
        } else if kind_name.is_some() || text.is_some() {
            3
        } else {
            2
        };
        sql.push_str(&limit_index.to_string());

        let limit = i64::try_from(query.limit)
            .map_err(|_| MezError::invalid_args("issue query limit exceeded SQLite range"))?;
        let mut statement = connection.prepare(&sql)?;
        let rows = match (kind_name, text) {
            (Some(kind), Some(text)) => statement.query_map(
                params![query.project, kind, text, limit],
                row_to_issue_record,
            )?,
            (Some(kind), None) => {
                statement.query_map(params![query.project, kind, limit], row_to_issue_record)?
            }
            (None, Some(text)) => {
                statement.query_map(params![query.project, text, limit], row_to_issue_record)?
            }
            (None, None) => {
                statement.query_map(params![query.project, limit], row_to_issue_record)?
            }
        };
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(MezError::from)
    }

    /// Deletes one issue by project and id.
    pub fn delete_issue(&self, project: String, id: String) -> Result<DeleteIssueResult> {
        super::validate_project_key(&project)?;
        if id.trim().is_empty() {
            return Err(MezError::invalid_args("issue id must not be empty"));
        }
        let connection = self.open()?;
        let changed = connection.execute(
            "DELETE FROM issues WHERE project = ?1 AND id = ?2",
            params![project, id],
        )?;
        Ok(DeleteIssueResult {
            project,
            id,
            deleted: changed > 0,
        })
    }

    /// Returns one issue by project and id when it exists.
    pub fn get_issue(&self, project: String, id: String) -> Result<Option<IssueRecord>> {
        super::validate_project_key(&project)?;
        if id.trim().is_empty() {
            return Err(MezError::invalid_args("issue id must not be empty"));
        }
        let connection = self.open()?;
        select_issue(&connection, &project, &id)
    }

    /// Updates one issue by project and id and returns the updated record.
    pub fn update_issue(
        &self,
        project: String,
        id: String,
        update: IssueUpdate,
        now_unix_seconds: u64,
    ) -> Result<UpdateIssueResult> {
        super::validate_project_key(&project)?;
        if id.trim().is_empty() {
            return Err(MezError::invalid_args("issue id must not be empty"));
        }
        update.validate()?;
        let connection = self.open()?;
        let Some(mut record) = select_issue(&connection, &project, &id)? else {
            return Ok(UpdateIssueResult {
                project,
                id,
                updated: false,
                record: None,
            });
        };
        if let Some(kind) = update.kind {
            record.kind = kind;
        }
        if let Some(title) = update.title {
            record.title = title;
        }
        if update.clear_body {
            record.body = None;
        } else if let Some(body) = update.body {
            record.body = Some(body);
        }
        if update.clear_notes {
            record.notes = None;
        } else if let Some(notes) = update.notes {
            record.notes = Some(notes);
        }
        record.updated_at_unix_seconds = now_unix_seconds;
        record.validate()?;
        update_issue_row(&connection, &record)?;
        Ok(UpdateIssueResult {
            project,
            id,
            updated: true,
            record: Some(record),
        })
    }

    fn open(&self) -> Result<Connection> {
        ensure_private_parent(&self.path)?;
        let connection = Connection::open(&self.path)?;
        initialize_schema(&connection)?;
        set_private_issue_file_permissions(&self.path)?;
        Ok(connection)
    }
}

fn initialize_schema(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA foreign_keys = ON;
         CREATE TABLE IF NOT EXISTS issues (
             id TEXT PRIMARY KEY NOT NULL,
             project TEXT NOT NULL,
             kind TEXT NOT NULL CHECK (kind IN ('defect', 'task')),
             title TEXT NOT NULL,
             body TEXT,
             notes TEXT,
             created_at INTEGER NOT NULL,
             updated_at INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS issues_project_kind_idx
             ON issues(project, kind, updated_at DESC, id ASC);
         CREATE INDEX IF NOT EXISTS issues_project_updated_idx
             ON issues(project, updated_at DESC, id ASC);",
    )?;
    ensure_notes_column(connection)?;
    Ok(())
}

fn ensure_notes_column(connection: &Connection) -> Result<()> {
    let mut statement = connection.prepare("PRAGMA table_info(issues)")?;
    let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
    for column in columns {
        if column? == "notes" {
            return Ok(());
        }
    }
    connection.execute("ALTER TABLE issues ADD COLUMN notes TEXT", [])?;
    Ok(())
}

fn insert_issue(connection: &Connection, record: &IssueRecord) -> Result<()> {
    record.validate()?;
    connection.execute(
        "INSERT INTO issues (id, project, kind, title, body, notes, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            record.id,
            record.project,
            record.kind.as_str(),
            record.title,
            record.body,
            record.notes,
            sqlite_i64_from_u64(record.created_at_unix_seconds)?,
            sqlite_i64_from_u64(record.updated_at_unix_seconds)?,
        ],
    )?;
    Ok(())
}

fn row_to_issue_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<IssueRecord> {
    let kind: String = row.get(2)?;
    Ok(IssueRecord {
        id: row.get(0)?,
        project: row.get(1)?,
        kind: IssueKind::parse(&kind).map_err(rusqlite_from_mez_error)?,
        title: row.get(3)?,
        body: row.get(4)?,
        notes: row.get(5)?,
        created_at_unix_seconds: row_u64(row, 6)?,
        updated_at_unix_seconds: row_u64(row, 7)?,
    })
}

fn update_issue_row(connection: &Connection, record: &IssueRecord) -> Result<()> {
    connection.execute(
        "UPDATE issues SET kind = ?3, title = ?4, body = ?5, notes = ?6, updated_at = ?7
         WHERE project = ?1 AND id = ?2",
        params![
            record.project,
            record.id,
            record.kind.as_str(),
            record.title,
            record.body,
            record.notes,
            sqlite_i64_from_u64(record.updated_at_unix_seconds)?,
        ],
    )?;
    Ok(())
}

fn select_issue(connection: &Connection, project: &str, id: &str) -> Result<Option<IssueRecord>> {
    connection
        .query_row(
            "SELECT id, project, kind, title, body, notes, created_at, updated_at FROM issues WHERE project = ?1 AND id = ?2",
            params![project, id],
            row_to_issue_record,
        )
        .optional()
        .map_err(MezError::from)
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn sqlite_i64_from_u64(value: u64) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| MezError::invalid_args("issue timestamp exceeded SQLite range"))
}

fn row_u64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            Box::new(MezError::invalid_args("invalid issue timestamp")),
        )
    })
}

fn rusqlite_from_mez_error(error: MezError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}

#[allow(dead_code)]
fn inspect_issue(connection: &Connection, id: &str) -> Result<Option<IssueRecord>> {
    connection
        .query_row(
            "SELECT id, project, kind, title, body, notes, created_at, updated_at FROM issues WHERE id = ?1",
            params![id],
            row_to_issue_record,
        )
        .optional()
        .map_err(MezError::from)
}
