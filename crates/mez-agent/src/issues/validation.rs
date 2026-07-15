//! Storage-independent issue field and MAAP validation.

use std::collections::BTreeSet;

use super::{IssueError, IssueKind, IssueResult, IssueState, MAX_ISSUE_QUERY_LIMIT};

/// Borrowed fields used to validate one model-authored issue update.
#[derive(Debug, Clone, Copy)]
pub struct IssueUpdateValidation<'a> {
    /// Optional replacement issue kind.
    pub kind: Option<&'a str>,
    /// Optional replacement issue state.
    pub state: Option<&'a str>,
    /// Optional replacement title.
    pub title: Option<&'a str>,
    /// Optional replacement body.
    pub body: Option<&'a str>,
    /// Whether the existing body should be removed.
    pub clear_body: bool,
    /// Optional replacement progress notes.
    pub notes: Option<&'a str>,
    /// Whether the existing notes should be removed.
    pub clear_notes: bool,
    /// Optional replacement dependency identifiers.
    pub depends_on: Option<&'a [String]>,
    /// Whether existing dependencies should be removed.
    pub clear_depends_on: bool,
}

/// Borrowed fields used to validate one model-authored issue query.
#[derive(Debug, Clone, Copy)]
pub struct IssueQueryValidation<'a> {
    /// Optional issue kind filter.
    pub kind: Option<&'a str>,
    /// Optional issue state filter.
    pub state: Option<&'a str>,
    /// Optional title/body substring filter.
    pub text: Option<&'a str>,
    /// Optional maximum result count.
    pub limit: Option<u64>,
}

/// Validates a user or runtime resolved project key.
pub fn validate_project_key(project: &str) -> IssueResult<()> {
    validate_non_empty_single_line("issue project", project)
}

/// Validates the stable model-facing issue kind grammar.
pub fn validate_issue_kind(kind: &str) -> IssueResult<()> {
    IssueKind::parse(kind).map(|_| ())
}

/// Validates the stable model-facing issue state grammar.
pub fn validate_issue_state(state: &str) -> IssueResult<()> {
    IssueState::parse(state).map(|_| ())
}

/// Validates an issue title.
pub fn validate_issue_title(title: &str) -> IssueResult<()> {
    validate_non_empty_single_line("issue title", title)
}

/// Validates optional issue body text.
pub fn validate_issue_body(body: Option<&str>) -> IssueResult<()> {
    validate_optional_text("issue body", body)
}

/// Validates optional mutable issue notes text.
pub fn validate_issue_notes(notes: Option<&str>) -> IssueResult<()> {
    validate_optional_text("issue notes", notes)
}

/// Validates issue dependency ids before project-specific lookup.
pub fn validate_issue_dependency_ids(
    issue_id: Option<&str>,
    depends_on: &[String],
) -> IssueResult<()> {
    let mut seen = BTreeSet::new();
    for dependency_id in depends_on {
        if dependency_id.trim().is_empty() || dependency_id.bytes().any(|byte| byte == 0) {
            return Err(IssueError::invalid_args(
                "issue dependency id must not be empty",
            ));
        }
        if issue_id.is_some_and(|id| id == dependency_id) {
            return Err(IssueError::invalid_args("issue cannot depend on itself"));
        }
        if !seen.insert(dependency_id.as_str()) {
            return Err(IssueError::invalid_args(
                "issue dependencies must not contain duplicates",
            ));
        }
    }
    Ok(())
}

/// Validates one model-authored issue update without persistence state.
pub fn validate_issue_update(update: IssueUpdateValidation<'_>) -> IssueResult<()> {
    if !(update.kind.is_some()
        || update.state.is_some()
        || update.title.is_some()
        || update.body.is_some()
        || update.clear_body
        || update.notes.is_some()
        || update.clear_notes
        || update.depends_on.is_some()
        || update.clear_depends_on)
    {
        return Err(IssueError::invalid_args(
            "issue update requires at least one field to change",
        ));
    }
    if update.body.is_some() && update.clear_body {
        return Err(IssueError::invalid_args(
            "issue update cannot set and clear body",
        ));
    }
    if update.notes.is_some() && update.clear_notes {
        return Err(IssueError::invalid_args(
            "issue update cannot set and clear notes",
        ));
    }
    if update.depends_on.is_some() && update.clear_depends_on {
        return Err(IssueError::invalid_args(
            "issue update cannot set and clear dependencies",
        ));
    }
    if let Some(kind) = update.kind {
        validate_issue_kind(kind)?;
    }
    if let Some(state) = update.state {
        validate_issue_state(state)?;
    }
    if let Some(title) = update.title {
        validate_issue_title(title)?;
    }
    validate_issue_body(update.body)?;
    validate_issue_notes(update.notes)?;
    if let Some(depends_on) = update.depends_on {
        validate_issue_dependency_ids(None, depends_on)?;
    }
    Ok(())
}

/// Validates one model-authored issue query without persistence state.
pub fn validate_issue_query(query: IssueQueryValidation<'_>) -> IssueResult<()> {
    if let Some(kind) = query.kind {
        validate_issue_kind(kind)?;
    }
    if let Some(state) = query.state {
        validate_issue_state(state)?;
    }
    validate_optional_text("issue query text", query.text)?;
    if let Some(limit) = query.limit {
        if limit == 0 {
            return Err(IssueError::invalid_args(
                "issue query limit must be greater than zero",
            ));
        }
        if limit > MAX_ISSUE_QUERY_LIMIT as u64 {
            return Err(IssueError::invalid_args(format!(
                "issue query limit must be less than or equal to {MAX_ISSUE_QUERY_LIMIT}"
            )));
        }
    }
    Ok(())
}

fn validate_optional_text(label: &str, value: Option<&str>) -> IssueResult<()> {
    if value.is_some_and(|value| value.bytes().any(|byte| byte == 0)) {
        return Err(IssueError::invalid_args(format!(
            "{label} must not contain NUL bytes"
        )));
    }
    Ok(())
}

fn validate_non_empty_single_line(label: &str, value: &str) -> IssueResult<()> {
    if value.trim().is_empty() {
        return Err(IssueError::invalid_args(format!(
            "{label} must not be empty"
        )));
    }
    if value
        .bytes()
        .any(|byte| byte == 0 || byte == b'\n' || byte == b'\r')
    {
        return Err(IssueError::invalid_args(format!(
            "{label} must be a single line without NUL bytes"
        )));
    }
    Ok(())
}
