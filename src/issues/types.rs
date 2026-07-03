//! Issue record and query types.
//!
//! These types define the durable shape shared by the issue SQLite store,
//! process CLI, runtime slash commands, and model-authored semantic actions.

use std::collections::BTreeSet;

use crate::error::{MezError, Result};

use super::{
    DEFAULT_ISSUE_QUERY_LIMIT, MAX_ISSUE_QUERY_LIMIT, validate_issue_body, validate_issue_notes,
    validate_issue_title, validate_project_key,
};

/// Classification for one locally tracked issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueKind {
    /// A product or implementation defect.
    Defect,
    /// A planned or requested task.
    Task,
}

impl IssueKind {
    /// Returns the stable storage and JSON name for this kind.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Defect => "defect",
            Self::Task => "task",
        }
    }

    /// Parses a user or model supplied issue kind.
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "defect" => Ok(Self::Defect),
            "task" => Ok(Self::Task),
            _ => Err(MezError::invalid_args("issue kind must be defect or task")),
        }
    }
}

/// Workflow state for one locally tracked issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueState {
    /// Issue is still active and should be returned by default work queries.
    Open,
    /// Issue implementation and verification are complete, but history remains queryable.
    Resolved,
}

impl IssueState {
    /// Returns the stable storage and JSON name for this state.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Resolved => "resolved",
        }
    }

    /// Parses a user or model supplied issue state.
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "open" => Ok(Self::Open),
            "resolved" => Ok(Self::Resolved),
            _ => Err(MezError::invalid_args(
                "issue state must be open or resolved",
            )),
        }
    }
}

/// User-authored fields used to create one issue record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewIssueRecord {
    /// Canonical project key.
    pub project: String,
    /// Defect or task classification.
    pub kind: IssueKind,
    /// Required single-line issue summary.
    pub title: String,
    /// Optional issue detail text.
    pub body: Option<String>,
    /// Optional mutable progress or handoff notes.
    pub notes: Option<String>,
    /// Issue ids that must be completed before this issue can be worked.
    pub depends_on: Vec<String>,
}

/// One durable issue record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueRecord {
    /// Stable issue id.
    pub id: String,
    /// Canonical project key.
    pub project: String,
    /// Defect or task classification.
    pub kind: IssueKind,
    /// Open or resolved workflow state.
    pub state: IssueState,
    /// Required single-line issue summary.
    pub title: String,
    /// Optional issue detail text.
    pub body: Option<String>,
    /// Optional mutable progress or handoff notes.
    pub notes: Option<String>,
    /// Issue ids that must be completed before this issue can be worked.
    pub depends_on: Vec<String>,
    /// Creation time as Unix seconds.
    pub created_at_unix_seconds: u64,
    /// Last update time as Unix seconds.
    pub updated_at_unix_seconds: u64,
}

impl IssueRecord {
    /// Builds a new issue record after validating user-authored fields.
    pub fn new(
        id: String,
        project: String,
        kind: IssueKind,
        title: String,
        body: Option<String>,
        notes: Option<String>,
        now_unix_seconds: u64,
    ) -> Result<Self> {
        Self::new_with_fields(
            id,
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

    /// Builds a new issue record from user-authored fields.
    pub fn new_with_fields(
        id: String,
        fields: NewIssueRecord,
        now_unix_seconds: u64,
    ) -> Result<Self> {
        let record = Self {
            id,
            project: fields.project,
            kind: fields.kind,
            state: IssueState::Open,
            title: fields.title,
            body: fields.body,
            notes: fields.notes,
            depends_on: fields.depends_on,
            created_at_unix_seconds: now_unix_seconds,
            updated_at_unix_seconds: now_unix_seconds,
        };
        record.validate()?;
        Ok(record)
    }

    /// Validates that this record can be persisted.
    pub fn validate(&self) -> Result<()> {
        validate_project_key(&self.project)?;
        validate_issue_title(&self.title)?;
        validate_issue_body(self.body.as_deref())?;
        validate_issue_notes(self.notes.as_deref())?;
        validate_issue_dependency_ids(Some(&self.id), &self.depends_on)?;
        if self.id.trim().is_empty() || self.id.bytes().any(|byte| byte == 0) {
            return Err(MezError::invalid_args("issue id must not be empty"));
        }
        if self.created_at_unix_seconds == 0 || self.updated_at_unix_seconds == 0 {
            return Err(MezError::invalid_args(
                "issue timestamps must be positive Unix seconds",
            ));
        }
        Ok(())
    }
}

/// Requested mutable issue field updates.
///
/// Optional fields distinguish unchanged values from explicit replacements.
/// `clear_body` and `clear_notes` remove optional text fields, while `body`
/// and `notes` replace them with new content.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IssueUpdate {
    /// Optional replacement defect/task classification.
    pub kind: Option<IssueKind>,
    /// Optional replacement open/resolved workflow state.
    pub state: Option<IssueState>,
    /// Optional replacement single-line title.
    pub title: Option<String>,
    /// Optional replacement issue description.
    pub body: Option<String>,
    /// Whether to clear the issue description.
    pub clear_body: bool,
    /// Optional replacement mutable progress or handoff notes.
    pub notes: Option<String>,
    /// Whether to clear mutable progress or handoff notes.
    pub clear_notes: bool,
    /// Optional replacement dependency issue ids.
    pub depends_on: Option<Vec<String>>,
    /// Whether to clear dependency issue ids.
    pub clear_depends_on: bool,
}

impl IssueUpdate {
    /// Returns whether the patch requests at least one field mutation.
    pub fn has_changes(&self) -> bool {
        self.kind.is_some()
            || self.state.is_some()
            || self.title.is_some()
            || self.body.is_some()
            || self.clear_body
            || self.notes.is_some()
            || self.clear_notes
            || self.depends_on.is_some()
            || self.clear_depends_on
    }

    /// Validates update fields before they are applied to a record.
    pub fn validate(&self) -> Result<()> {
        if !self.has_changes() {
            return Err(MezError::invalid_args(
                "issue update requires at least one field to change",
            ));
        }
        if self.body.is_some() && self.clear_body {
            return Err(MezError::invalid_args(
                "issue update cannot set and clear body",
            ));
        }
        if self.notes.is_some() && self.clear_notes {
            return Err(MezError::invalid_args(
                "issue update cannot set and clear notes",
            ));
        }
        if self.depends_on.is_some() && self.clear_depends_on {
            return Err(MezError::invalid_args(
                "issue update cannot set and clear dependencies",
            ));
        }
        if let Some(title) = self.title.as_deref() {
            validate_issue_title(title)?;
        }
        validate_issue_body(self.body.as_deref())?;
        validate_issue_notes(self.notes.as_deref())?;
        if let Some(depends_on) = self.depends_on.as_deref() {
            validate_issue_dependency_ids(None, depends_on)?;
        }
        Ok(())
    }
}

/// Validates issue dependency ids before project-specific lookup.
pub fn validate_issue_dependency_ids(issue_id: Option<&str>, depends_on: &[String]) -> Result<()> {
    let mut seen = BTreeSet::new();
    for dependency_id in depends_on {
        if dependency_id.trim().is_empty() || dependency_id.bytes().any(|byte| byte == 0) {
            return Err(MezError::invalid_args(
                "issue dependency id must not be empty",
            ));
        }
        if issue_id.is_some_and(|id| id == dependency_id) {
            return Err(MezError::invalid_args("issue cannot depend on itself"));
        }
        if !seen.insert(dependency_id.as_str()) {
            return Err(MezError::invalid_args(
                "issue dependencies must not contain duplicates",
            ));
        }
    }
    Ok(())
}

/// Result of updating one issue record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateIssueResult {
    /// Project key used for the update.
    pub project: String,
    /// Issue id targeted by the update.
    pub id: String,
    /// Whether a row was updated.
    pub updated: bool,
    /// Updated record when the issue existed in the project.
    pub record: Option<IssueRecord>,
}

/// Query filters for issue lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueQuery {
    /// Required canonical project key.
    pub project: String,
    /// Optional defect/task filter.
    pub kind: Option<IssueKind>,
    /// Optional open/resolved filter.
    pub state: Option<IssueState>,
    /// Optional case-insensitive title/body substring query.
    pub text: Option<String>,
    /// Maximum records returned.
    pub limit: usize,
}

impl IssueQuery {
    /// Builds a validated query with default and maximum limit enforcement.
    pub fn new(
        project: String,
        kind: Option<IssueKind>,
        text: Option<String>,
        limit: Option<usize>,
    ) -> Result<Self> {
        Self::new_with_state(project, kind, Some(IssueState::Open), text, limit)
    }

    /// Builds a validated query with an explicit state filter.
    pub fn new_with_state(
        project: String,
        kind: Option<IssueKind>,
        state: Option<IssueState>,
        text: Option<String>,
        limit: Option<usize>,
    ) -> Result<Self> {
        validate_project_key(&project)?;
        let text = text
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if let Some(text) = text.as_deref()
            && text.bytes().any(|byte| byte == 0)
        {
            return Err(MezError::invalid_args(
                "issue query text must not contain NUL bytes",
            ));
        }
        let limit = limit.unwrap_or(DEFAULT_ISSUE_QUERY_LIMIT);
        if limit == 0 {
            return Err(MezError::invalid_args("issue query limit must be positive"));
        }
        Ok(Self {
            project,
            kind,
            state,
            text,
            limit: limit.min(MAX_ISSUE_QUERY_LIMIT),
        })
    }
}

/// Result of deleting one issue record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteIssueResult {
    /// Project key used for the deletion.
    pub project: String,
    /// Issue id targeted by the deletion.
    pub id: String,
    /// Whether a row was removed.
    pub deleted: bool,
}
