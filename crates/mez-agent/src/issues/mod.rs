//! Canonical agent issue records, queries, updates, and validation.
//!
//! This module owns storage-independent issue contracts shared by MAAP and the
//! product repository. SQLite schema/query execution, dependency graph lookup,
//! project discovery, ID generation, paths, and filesystem permissions remain
//! product adapter concerns.

mod error;
mod presentation;
mod types;
mod validation;

pub use error::{IssueError, IssueResult};
pub use presentation::{
    issue_delete_action_result, issue_query_action_result, issue_record_action_result,
    issue_record_json, issue_update_action_result,
};
pub use types::{
    DeleteIssueResult, IssueBrowserQuery, IssueKind, IssueQuery, IssueRecord, IssueState,
    IssueUpdate, NewIssueRecord, UpdateIssueResult,
};
pub use validation::{
    IssueQueryValidation, IssueUpdateValidation, validate_issue_body,
    validate_issue_dependency_ids, validate_issue_kind, validate_issue_notes, validate_issue_query,
    validate_issue_state, validate_issue_title, validate_issue_update, validate_project_key,
};

/// Default maximum issue records returned by one query.
pub const DEFAULT_ISSUE_QUERY_LIMIT: usize = 50;
/// Hard upper bound for one issue query result set.
pub const MAX_ISSUE_QUERY_LIMIT: usize = 200;

#[cfg(test)]
mod tests;
