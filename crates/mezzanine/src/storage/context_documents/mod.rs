//! Persisted user-owned context documents and deterministic inclusion policy.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{MezError, Result};

mod store;

pub(crate) use store::ContextDocumentStore;

/// Maximum content retained by one persisted context document.
pub(crate) const MAX_CONTEXT_DOCUMENT_BYTES: usize = 64 * 1024;
/// Maximum enabled documents injected into one newly created turn.
pub(crate) const MAX_INCLUDED_CONTEXT_DOCUMENTS: usize = 16;
/// Maximum aggregate persisted-document content injected into one new turn.
pub(crate) const MAX_INCLUDED_CONTEXT_DOCUMENT_BYTES: usize = 128 * 1024;

/// User-owned scope controlling where one context document may be included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ContextDocumentScope {
    /// Included in every future turn when enabled.
    Global,
    /// Included only for future turns in one exact project.
    Project { root: String },
}

/// One persisted source artifact eligible for future context assembly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContextDocument {
    pub(crate) id: String,
    pub(crate) scope: ContextDocumentScope,
    pub(crate) title: String,
    pub(crate) content: String,
    pub(crate) enabled: bool,
    pub(crate) created_at_unix_seconds: u64,
    pub(crate) updated_at_unix_seconds: u64,
}

impl ContextDocument {
    /// Validates the complete persisted document.
    pub(crate) fn validate(&self) -> Result<()> {
        if !valid_document_id(&self.id) {
            return Err(MezError::invalid_args(
                "context document id must be a lowercase UUID",
            ));
        }
        validate_single_line("context document title", &self.title)?;
        if self.content.as_bytes().contains(&0) {
            return Err(MezError::invalid_args(
                "context document content must not contain NUL bytes",
            ));
        }
        if self.content.len() > MAX_CONTEXT_DOCUMENT_BYTES {
            return Err(MezError::invalid_args(
                "context document content exceeds the byte limit",
            ));
        }
        if let ContextDocumentScope::Project { root } = &self.scope {
            validate_single_line("context document project root", root)?;
        }
        if self.created_at_unix_seconds == 0
            || self.updated_at_unix_seconds < self.created_at_unix_seconds
        {
            return Err(MezError::invalid_args(
                "context document timestamps are invalid",
            ));
        }
        Ok(())
    }

    /// Reports whether this document is visible to one exact project.
    pub(crate) fn visible_to_project(&self, project: &str) -> bool {
        match &self.scope {
            ContextDocumentScope::Global => true,
            ContextDocumentScope::Project { root } => root == project,
        }
    }
}

/// Transactional content update outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CompareAndSwapContextDocumentResult {
    Updated(Box<ContextDocument>),
    Stale { current_revision: String },
    Deleted,
}

/// Deterministic bounded enabled-document selection for one future turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContextDocumentSelection {
    pub(crate) documents: Vec<ContextDocument>,
    pub(crate) omitted: usize,
}

pub(super) fn default_context_document_database_path(config_root: impl AsRef<Path>) -> PathBuf {
    config_root.as_ref().join("context-documents.sqlite")
}

pub(super) fn ensure_private_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        set_private_permissions(parent, 0o700)?;
    }
    Ok(())
}

pub(super) fn set_private_file_permissions(path: &Path) -> Result<()> {
    set_private_permissions(path, 0o600)
}

fn set_private_permissions(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
    Ok(())
}

fn validate_single_line(label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() || value.bytes().any(|byte| matches!(byte, 0 | b'\n' | b'\r')) {
        return Err(MezError::invalid_args(format!(
            "{label} must be a non-empty single line"
        )));
    }
    Ok(())
}

fn valid_document_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte),
        })
}

#[cfg(test)]
mod tests;
