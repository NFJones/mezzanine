//! SQLite persistence for local project issues.
//!
//! The store initializes its own schema, validates records before writes, and
//! keeps queries bounded. It deliberately uses exact project-key matching so
//! issue records from different repositories do not bleed across surfaces.

use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension, params};

#[cfg(test)]
use super::Path;
use super::{
    DeleteIssueResult, IssueBrowserQuery, IssueDatabasePath, IssueKind, IssueQuery, IssueRecord,
    IssueState, IssueUpdate, MezError, NewIssueRecord, PathBuf, Result, UpdateIssueResult,
    ensure_private_parent, generate_issue_id, set_private_issue_file_permissions,
};

/// SQLite-backed local issue store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueStore {
    path: PathBuf,
    manage_private_parent: bool,
}

impl IssueStore {
    /// Returns an issue store using the default database under a config root.
    #[cfg(test)]
    pub fn under_config_root(config_root: impl Into<PathBuf>) -> Self {
        Self {
            path: super::default_issue_database_path(config_root.into()),
            manage_private_parent: true,
        }
    }

    /// Builds an issue store at an explicit SQLite database path.
    #[cfg(test)]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            manage_private_parent: true,
        }
    }

    /// Builds an issue store from a resolved database location.
    pub fn from_database_path(database_path: IssueDatabasePath) -> Self {
        Self {
            manage_private_parent: database_path.manages_private_parent(),
            path: database_path.into_path(),
        }
    }

    /// Returns the SQLite database path used by this store.
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "test-only adapter retained for focused boundary coverage"
    )]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Adds one issue record and returns the persisted value.
    #[cfg(test)]
    pub fn add_issue(
        &self,
        project: String,
        kind: IssueKind,
        title: String,
        body: Option<String>,
        notes: Option<String>,
        now_unix_seconds: u64,
    ) -> Result<IssueRecord> {
        self.add_issue_with_dependencies(
            NewIssueRecord {
                project,
                kind,
                title,
                body,
                notes,
                depends_on: Vec::new(),
            },
            now_unix_seconds,
        )
    }

    /// Adds one issue record with dependency ids and returns the persisted value.
    pub fn add_issue_with_dependencies(
        &self,
        fields: NewIssueRecord,
        now_unix_seconds: u64,
    ) -> Result<IssueRecord> {
        let record = IssueRecord::new_with_fields(generate_issue_id(), fields, now_unix_seconds)?;
        let mut connection = self.open()?;
        let transaction = connection.transaction()?;
        insert_issue(&transaction, &record)?;
        validate_issue_dependency_targets(&transaction, &record.project, &record.depends_on)?;
        replace_issue_dependencies(
            &transaction,
            &record.project,
            &record.id,
            &record.depends_on,
        )?;
        ensure_no_issue_dependency_cycle(&transaction, &record.project, &record.id)?;
        transaction.commit()?;
        Ok(record)
    }

    /// Queries issues matching the provided filters.
    pub fn query_issues(&self, query: &IssueQuery) -> Result<Vec<IssueRecord>> {
        let connection = self.open()?;
        let mut sql = String::from(
            "SELECT id, project, kind, state, title, body, notes, created_at, updated_at FROM issues WHERE project = ?1",
        );
        let kind_name = query.kind.map(IssueKind::as_str);
        let state_name = query.state.map(IssueState::as_str);
        let mut parameter_index = 2usize;
        if kind_name.is_some() {
            sql.push_str(&format!(" AND kind = ?{parameter_index}"));
            parameter_index = parameter_index.saturating_add(1);
        }
        if state_name.is_some() {
            sql.push_str(&format!(" AND state = ?{parameter_index}"));
            parameter_index = parameter_index.saturating_add(1);
        }
        let text = query
            .text
            .as_deref()
            .map(|value| format!("%{}%", escape_like(value)));
        if text.is_some() {
            sql.push_str(&format!(
                " AND (title LIKE ?{parameter_index} ESCAPE '\\' OR body LIKE ?{parameter_index} ESCAPE '\\')"
            ));
            parameter_index = parameter_index.saturating_add(1);
        }
        sql.push_str(" ORDER BY updated_at DESC, created_at DESC, id ASC LIMIT ?");
        sql.push_str(&parameter_index.to_string());

        let limit = i64::try_from(query.limit)
            .map_err(|_| MezError::invalid_args("issue query limit exceeded SQLite range"))?;
        let mut statement = connection.prepare(&sql)?;
        let rows = match (kind_name, state_name, text) {
            (Some(kind), Some(state), Some(text)) => statement.query_map(
                params![query.project, kind, state, text, limit],
                row_to_issue_record,
            )?,
            (Some(kind), Some(state), None) => statement.query_map(
                params![query.project, kind, state, limit],
                row_to_issue_record,
            )?,
            (Some(kind), None, Some(text)) => statement.query_map(
                params![query.project, kind, text, limit],
                row_to_issue_record,
            )?,
            (Some(kind), None, None) => {
                statement.query_map(params![query.project, kind, limit], row_to_issue_record)?
            }
            (None, Some(state), Some(text)) => statement.query_map(
                params![query.project, state, text, limit],
                row_to_issue_record,
            )?,
            (None, Some(state), None) => {
                statement.query_map(params![query.project, state, limit], row_to_issue_record)?
            }
            (None, None, Some(text)) => {
                statement.query_map(params![query.project, text, limit], row_to_issue_record)?
            }
            (None, None, None) => {
                statement.query_map(params![query.project, limit], row_to_issue_record)?
            }
        };
        let mut records = rows
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(MezError::from)?;
        load_issue_dependencies(&connection, &query.project, &mut records)?;
        Ok(records)
    }

    /// Queries issues for the interactive issue browser.
    pub fn query_issue_browser(&self, query: &IssueBrowserQuery) -> Result<Vec<IssueRecord>> {
        let connection = self.open()?;
        let mut sql = String::from(
            "SELECT id, project, kind, state, title, body, notes, created_at, updated_at FROM issues WHERE 1 = 1",
        );
        let project_glob = query.project_glob.as_deref().map(project_glob_like_pattern);
        let kind_name = query.kind.map(IssueKind::as_str);
        let state_name = query.state.map(IssueState::as_str);
        let text = query
            .text
            .as_deref()
            .map(|value| format!("%{}%", escape_like(value)));
        let mut parameter_index = 1usize;
        if project_glob.is_some() {
            sql.push_str(&format!(" AND project LIKE ?{parameter_index} ESCAPE '\\'"));
            parameter_index = parameter_index.saturating_add(1);
        }
        if kind_name.is_some() {
            sql.push_str(&format!(" AND kind = ?{parameter_index}"));
            parameter_index = parameter_index.saturating_add(1);
        }
        if state_name.is_some() {
            sql.push_str(&format!(" AND state = ?{parameter_index}"));
            parameter_index = parameter_index.saturating_add(1);
        }
        if text.is_some() {
            sql.push_str(&format!(
                " AND (title LIKE ?{parameter_index} ESCAPE '\\' OR body LIKE ?{parameter_index} ESCAPE '\\')"
            ));
            parameter_index = parameter_index.saturating_add(1);
        }
        sql.push_str(" ORDER BY updated_at DESC, created_at DESC, id ASC LIMIT ?");
        sql.push_str(&parameter_index.to_string());

        let limit = i64::try_from(query.limit).map_err(|_| {
            MezError::invalid_args("issue browser query limit exceeded SQLite range")
        })?;
        let mut statement = connection.prepare(&sql)?;
        let rows = match (project_glob, kind_name, state_name, text) {
            (Some(project), Some(kind), Some(state), Some(text)) => statement.query_map(
                params![project, kind, state, text, limit],
                row_to_issue_record,
            )?,
            (Some(project), Some(kind), Some(state), None) => {
                statement.query_map(params![project, kind, state, limit], row_to_issue_record)?
            }
            (Some(project), Some(kind), None, Some(text)) => {
                statement.query_map(params![project, kind, text, limit], row_to_issue_record)?
            }
            (Some(project), Some(kind), None, None) => {
                statement.query_map(params![project, kind, limit], row_to_issue_record)?
            }
            (Some(project), None, Some(state), Some(text)) => {
                statement.query_map(params![project, state, text, limit], row_to_issue_record)?
            }
            (Some(project), None, Some(state), None) => {
                statement.query_map(params![project, state, limit], row_to_issue_record)?
            }
            (Some(project), None, None, Some(text)) => {
                statement.query_map(params![project, text, limit], row_to_issue_record)?
            }
            (Some(project), None, None, None) => {
                statement.query_map(params![project, limit], row_to_issue_record)?
            }
            (None, Some(kind), Some(state), Some(text)) => {
                statement.query_map(params![kind, state, text, limit], row_to_issue_record)?
            }
            (None, Some(kind), Some(state), None) => {
                statement.query_map(params![kind, state, limit], row_to_issue_record)?
            }
            (None, Some(kind), None, Some(text)) => {
                statement.query_map(params![kind, text, limit], row_to_issue_record)?
            }
            (None, Some(kind), None, None) => {
                statement.query_map(params![kind, limit], row_to_issue_record)?
            }
            (None, None, Some(state), Some(text)) => {
                statement.query_map(params![state, text, limit], row_to_issue_record)?
            }
            (None, None, Some(state), None) => {
                statement.query_map(params![state, limit], row_to_issue_record)?
            }
            (None, None, None, Some(text)) => {
                statement.query_map(params![text, limit], row_to_issue_record)?
            }
            (None, None, None, None) => statement.query_map(params![limit], row_to_issue_record)?,
        };
        let mut records = rows
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(MezError::from)?;
        for record in &mut records {
            record.depends_on =
                issue_dependencies_for_record(&connection, &record.project, &record.id)?;
        }
        Ok(records)
    }

    /// Lists distinct nonempty issue project keys in deterministic lexical order.
    pub fn list_issue_projects(&self) -> Result<Vec<String>> {
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT DISTINCT project FROM issues WHERE TRIM(project) <> '' ORDER BY project ASC",
        )?;
        statement
            .query_map([], |row| row.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(MezError::from)
    }

    /// Deletes one issue by project and id.
    pub fn delete_issue(&self, project: String, id: String) -> Result<DeleteIssueResult> {
        super::validate_project_key(&project)?;
        if id.trim().is_empty() {
            return Err(MezError::invalid_args("issue id must not be empty"));
        }
        let mut connection = self.open()?;
        let transaction = connection.transaction()?;
        let mut statement = transaction.prepare(
            "SELECT issues.id FROM issue_dependencies
             JOIN issues ON issues.project = issue_dependencies.project
                AND issues.id = issue_dependencies.issue_id
             WHERE issue_dependencies.project = ?1
               AND issue_dependencies.depends_on_id = ?2
               AND issues.state = 'open'
             ORDER BY issues.id ASC",
        )?;
        let open_dependents = statement
            .query_map(params![project, id], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(statement);
        if !open_dependents.is_empty() {
            return Err(MezError::conflict(format!(
                "issue cannot be deleted while open issues depend on it: {}",
                open_dependents.join(", ")
            )));
        }
        let changed = transaction.execute(
            "DELETE FROM issues WHERE project = ?1 AND id = ?2",
            params![project, id],
        )?;
        transaction.commit()?;
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
        let mut connection = self.open()?;
        let transaction = connection.transaction()?;
        let Some(mut record) = select_issue(&transaction, &project, &id)? else {
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
        if let Some(state) = update.state {
            record.state = state;
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
        if update.clear_depends_on {
            record.depends_on = Vec::new();
        } else if let Some(depends_on) = update.depends_on {
            record.depends_on = depends_on;
        }
        record.updated_at_unix_seconds = now_unix_seconds;
        record.validate()?;
        validate_issue_dependency_targets(&transaction, &project, &record.depends_on)?;
        update_issue_row(&transaction, &record)?;
        replace_issue_dependencies(&transaction, &project, &id, &record.depends_on)?;
        ensure_no_issue_dependency_cycle(&transaction, &project, &id)?;
        transaction.commit()?;
        Ok(UpdateIssueResult {
            project,
            id,
            updated: true,
            record: Some(record),
        })
    }

    fn open(&self) -> Result<Connection> {
        if self.manage_private_parent {
            ensure_private_parent(&self.path)?;
        }
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
             state TEXT NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'resolved')),
             title TEXT NOT NULL,
             body TEXT,
             notes TEXT,
             created_at INTEGER NOT NULL,
             updated_at INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS issues_project_kind_idx
             ON issues(project, kind, updated_at DESC, id ASC);
         CREATE INDEX IF NOT EXISTS issues_project_updated_idx
             ON issues(project, updated_at DESC, id ASC);
         CREATE TABLE IF NOT EXISTS issue_dependencies (
             project TEXT NOT NULL,
             issue_id TEXT NOT NULL,
             depends_on_id TEXT NOT NULL,
             PRIMARY KEY (project, issue_id, depends_on_id),
             FOREIGN KEY (issue_id) REFERENCES issues(id) ON DELETE CASCADE,
             FOREIGN KEY (depends_on_id) REFERENCES issues(id) ON DELETE CASCADE
         );
         CREATE INDEX IF NOT EXISTS issue_dependencies_project_issue_idx
             ON issue_dependencies(project, issue_id);
         CREATE INDEX IF NOT EXISTS issue_dependencies_project_depends_on_idx
             ON issue_dependencies(project, depends_on_id);",
    )?;
    ensure_notes_column(connection)?;
    ensure_state_column(connection)?;
    connection.execute(
        "CREATE INDEX IF NOT EXISTS issues_project_state_kind_idx
         ON issues(project, state, kind, updated_at DESC, id ASC)",
        [],
    )?;
    Ok(())
}

fn ensure_state_column(connection: &Connection) -> Result<()> {
    let mut statement = connection.prepare("PRAGMA table_info(issues)")?;
    let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
    for column in columns {
        if column? == "state" {
            return Ok(());
        }
    }
    connection.execute(
        "ALTER TABLE issues ADD COLUMN state TEXT NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'resolved'))",
        [],
    )?;
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
        "INSERT INTO issues (id, project, kind, state, title, body, notes, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            record.id,
            record.project,
            record.kind.as_str(),
            record.state.as_str(),
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
    let state: String = row.get(3)?;
    Ok(IssueRecord {
        id: row.get(0)?,
        project: row.get(1)?,
        kind: IssueKind::parse(&kind).map_err(|error| rusqlite_from_mez_error(error.into()))?,
        state: IssueState::parse(&state).map_err(|error| rusqlite_from_mez_error(error.into()))?,
        title: row.get(4)?,
        body: row.get(5)?,
        notes: row.get(6)?,
        depends_on: Vec::new(),
        created_at_unix_seconds: row_u64(row, 7)?,
        updated_at_unix_seconds: row_u64(row, 8)?,
    })
}

fn update_issue_row(connection: &Connection, record: &IssueRecord) -> Result<()> {
    connection.execute(
        "UPDATE issues SET kind = ?3, state = ?4, title = ?5, body = ?6, notes = ?7, updated_at = ?8
         WHERE project = ?1 AND id = ?2",
        params![
            record.project,
            record.id,
            record.kind.as_str(),
            record.state.as_str(),
            record.title,
            record.body,
            record.notes,
            sqlite_i64_from_u64(record.updated_at_unix_seconds)?,
        ],
    )?;
    Ok(())
}

fn select_issue(connection: &Connection, project: &str, id: &str) -> Result<Option<IssueRecord>> {
    let mut record = connection
        .query_row(
            "SELECT id, project, kind, state, title, body, notes, created_at, updated_at FROM issues WHERE project = ?1 AND id = ?2",
            params![project, id],
            row_to_issue_record,
        )
        .optional()
        .map_err(MezError::from)?;
    if let Some(record) = record.as_mut() {
        record.depends_on = issue_dependencies_for_record(connection, project, &record.id)?;
    }
    Ok(record)
}

fn load_issue_dependencies(
    connection: &Connection,
    project: &str,
    records: &mut [IssueRecord],
) -> Result<()> {
    for record in records {
        record.depends_on = issue_dependencies_for_record(connection, project, &record.id)?;
    }
    Ok(())
}

fn issue_dependencies_for_record(
    connection: &Connection,
    project: &str,
    issue_id: &str,
) -> Result<Vec<String>> {
    let mut statement = connection.prepare(
        "SELECT depends_on_id FROM issue_dependencies WHERE project = ?1 AND issue_id = ?2 ORDER BY depends_on_id ASC",
    )?;
    let rows = statement.query_map(params![project, issue_id], |row| row.get::<_, String>(0))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(MezError::from)
}

fn validate_issue_dependency_targets(
    connection: &Connection,
    project: &str,
    depends_on: &[String],
) -> Result<()> {
    for dependency_id in depends_on {
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM issues WHERE project = ?1 AND id = ?2)",
            params![project, dependency_id],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(MezError::invalid_args(format!(
                "issue dependency `{dependency_id}` does not exist in this project"
            )));
        }
    }
    Ok(())
}

fn replace_issue_dependencies(
    connection: &Connection,
    project: &str,
    issue_id: &str,
    depends_on: &[String],
) -> Result<()> {
    connection.execute(
        "DELETE FROM issue_dependencies WHERE project = ?1 AND issue_id = ?2",
        params![project, issue_id],
    )?;
    for dependency_id in depends_on {
        connection.execute(
            "INSERT INTO issue_dependencies (project, issue_id, depends_on_id) VALUES (?1, ?2, ?3)",
            params![project, issue_id, dependency_id],
        )?;
    }
    Ok(())
}

fn ensure_no_issue_dependency_cycle(
    connection: &Connection,
    project: &str,
    issue_id: &str,
) -> Result<()> {
    let mut visited = BTreeSet::new();
    if issue_dependency_path_reaches(connection, project, issue_id, issue_id, &mut visited)? {
        return Err(MezError::invalid_args("issue dependency cycle detected"));
    }
    Ok(())
}

fn issue_dependency_path_reaches(
    connection: &Connection,
    project: &str,
    current_id: &str,
    target_id: &str,
    visited: &mut BTreeSet<String>,
) -> Result<bool> {
    if !visited.insert(current_id.to_string()) {
        return Ok(false);
    }
    for dependency_id in issue_dependencies_for_record(connection, project, current_id)? {
        if dependency_id == target_id
            || issue_dependency_path_reaches(
                connection,
                project,
                &dependency_id,
                target_id,
                visited,
            )?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn project_glob_like_pattern(value: &str) -> String {
    let mut pattern = String::new();
    for character in value.chars() {
        match character {
            '*' => pattern.push('%'),
            '?' => pattern.push('_'),
            '\\' => pattern.push_str("\\\\"),
            '%' => pattern.push_str("\\%"),
            '_' => pattern.push_str("\\_"),
            other => pattern.push(other),
        }
    }
    pattern
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
