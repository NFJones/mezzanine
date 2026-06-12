//! Issue record and query types.
//!
//! These types define the durable shape shared by the issue SQLite store,
//! process CLI, runtime slash commands, and model-authored semantic actions.

use crate::error::{MezError, Result};

use super::{
    DEFAULT_ISSUE_QUERY_LIMIT, MAX_ISSUE_QUERY_LIMIT, validate_issue_body, validate_issue_title,
    validate_project_key,
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

/// One durable issue record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueRecord {
    /// Stable issue id.
    pub id: String,
    /// Canonical project key.
    pub project: String,
    /// Defect or task classification.
    pub kind: IssueKind,
    /// Required single-line issue summary.
    pub title: String,
    /// Optional issue detail text.
    pub body: Option<String>,
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
        now_unix_seconds: u64,
    ) -> Result<Self> {
        let record = Self {
            id,
            project,
            kind,
            title,
            body,
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

/// Query filters for issue lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueQuery {
    /// Required canonical project key.
    pub project: String,
    /// Optional defect/task filter.
    pub kind: Option<IssueKind>,
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
