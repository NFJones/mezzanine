//! Ordered positive file/directory facts shared by worker, journal and projection.
//!
//! Directory confirmations bind their intended authored and physical file target,
//! carry no file diff/line delta, and precede that destination's confirmation.
//! Confirmation ordinals are actual commit order; actor lease keys are a separate
//! namespace. These facts describe effects and never grant filesystem authority.

use crate::security::filesystem::capability::parent::MAX_NATIVE_PARENT_DIRECTORIES;
use mez_agent::native_action::PatchEffect;

/// Maximum encoded authoritative facts, excluding separately bounded diff text.
pub(crate) const NATIVE_PATCH_FACT_BYTES: usize = 1024 * 1024;
/// Facts plus worst-case JSON-escaped 256KiB diff and fixed diagnostic envelope.
pub(crate) const NATIVE_PATCH_RESULT_BYTES: usize = 3 * 1024 * 1024;

/// Positively completed endpoint, including separate directory side effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NativePatchEndpointKind {
    /// A missing file destination was published with no-replace semantics.
    Created,
    /// An exact existing file entry was replaced atomically.
    Updated,
    /// An exact existing file entry was removed through its held parent.
    Deleted,
    /// An independently authorized missing directory was created.
    DirectoryCreated,
}

/// Exact prepared file destination for a separately confirmed ancestor creation.
#[derive(Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct NativeParentTarget {
    /// Authored file spelling, bound to the original operation.
    pub(crate) path: String,
    /// Captured physical file target; aliases do not change this relationship.
    pub(crate) resolved_path: String,
}

/// Immutable effect retained immediately after the positive committing syscall.
#[derive(Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct NativePatchEndpointReceipt {
    /// Authored operation occurrence, not a sorted filename index.
    pub(crate) operation_ordinal: usize,
    /// File0/destination, file1/move-source, or reserved2+directory occurrence.
    pub(crate) endpoint_ordinal: usize,
    /// Actual ordered positive confirmation, including directories.
    pub(crate) confirmation_ordinal: usize,
    /// Original file spelling, or physical path of a separately created directory.
    pub(crate) path: String,
    /// Positively resolved physical target bound before publication.
    pub(crate) resolved_path: String,
    /// Actual completed mutation, never merely intended work.
    pub(crate) kind: NativePatchEndpointKind,
    /// Present only for a directory confirmation, not a counterfeit file write.
    pub(crate) parent_target: Option<NativeParentTarget>,
    /// Aggregate-budgeted native diff; directories have no file diff.
    pub(crate) diff: String,
    /// Full-file native additions, zero for directory effects.
    pub(crate) added_lines: usize,
    /// Full-file native removals, zero for directory effects.
    pub(crate) removed_lines: usize,
    /// Display truncation cannot remove this authoritative fact.
    pub(crate) diff_truncated: bool,
}

impl NativePatchEndpointReceipt {
    /// Returns whether this receipt represents a file rather than an ancestor.
    pub(crate) fn is_file(&self) -> bool {
        self.kind != NativePatchEndpointKind::DirectoryCreated
    }

    /// Encodes the same complete shape used by canonical result metadata.
    pub(crate) fn metadata(&self, include_diff: bool) -> serde_json::Value {
        serde_json::json!({
            "operation_ordinal": self.operation_ordinal, "endpoint_ordinal": self.endpoint_ordinal,
            "confirmation_ordinal": self.confirmation_ordinal, "path": self.path,
            "resolved_path": self.resolved_path, "kind": self.kind, "parent_target": self.parent_target,
            "diff": if include_diff { self.diff.as_str() } else { "" },
            "added_lines": self.added_lines, "removed_lines": self.removed_lines,
            "diff_truncated": self.diff_truncated,
        })
    }

    /// Checks chronological dependencies and unique directory/file occurrence
    /// facts against the actual prior prefix, independently of display identity.
    pub(crate) fn can_follow<'a>(&self, earlier: impl Iterator<Item = &'a Self>) -> bool {
        let mut count = 0;
        let mut directories = 0;
        let mut destination = false;
        for prior in earlier {
            count += 1;
            if !prior.is_file() {
                directories += 1;
            }
            if prior.operation_ordinal > self.operation_ordinal
                || (prior.operation_ordinal == self.operation_ordinal
                    && prior.endpoint_ordinal == self.endpoint_ordinal)
            {
                return false;
            }
            if !self.is_file() && !prior.is_file() && prior.resolved_path == self.resolved_path {
                return false;
            }
            if prior.operation_ordinal != self.operation_ordinal {
                continue;
            }
            if !self.is_file() {
                if prior.is_file()
                    || !std::path::Path::new(&self.resolved_path).starts_with(&prior.resolved_path)
                    || prior.parent_target != self.parent_target
                {
                    return false;
                }
            } else if prior.is_file() {
                if prior.endpoint_ordinal >= self.endpoint_ordinal {
                    return false;
                }
                destination |= prior.endpoint_ordinal == 0;
            } else if self.endpoint_ordinal == 0
                && prior.parent_target.as_ref().is_none_or(|target| {
                    target.path != self.path || target.resolved_path != self.resolved_path
                })
            {
                return false;
            }
        }
        self.confirmation_ordinal == count
            && (self.is_file() || self.endpoint_ordinal == 2 + directories)
            && (self.endpoint_ordinal != 1 || destination)
    }

    /// Validates bounded source facts and semantic role without authorizing I/O.
    pub(crate) fn matches_effect(&self, effect: Option<&PatchEffect>) -> bool {
        if self.path.len() > 4096
            || self.resolved_path.len() > 4096
            || !std::path::Path::new(&self.resolved_path).is_absolute()
        {
            return false;
        }
        if !self.is_file() {
            let Some(target) = &self.parent_target else {
                return false;
            };
            let accepted = match effect {
                Some(PatchEffect::Create(path)) => path == &target.path,
                Some(PatchEffect::Move { destination, .. }) => destination == &target.path,
                _ => false,
            };
            let parent = std::path::Path::new(&self.resolved_path);
            let child = std::path::Path::new(&target.resolved_path);
            return accepted
                && (2..2 + MAX_NATIVE_PARENT_DIRECTORIES).contains(&self.endpoint_ordinal)
                && self.path == self.resolved_path
                && target.path.len() <= 4096
                && target.resolved_path.len() <= 4096
                && normal_absolute(parent)
                && normal_absolute(child)
                && child != parent
                && child.starts_with(parent)
                && self.diff.is_empty()
                && !self.diff_truncated
                && self.added_lines == 0
                && self.removed_lines == 0;
        }
        if self.parent_target.is_some() || self.endpoint_ordinal > 1 {
            return false;
        }
        match effect {
            Some(PatchEffect::Create(path)) => {
                self.endpoint_ordinal == 0
                    && self.path == *path
                    && self.kind == NativePatchEndpointKind::Created
            }
            Some(PatchEffect::Update(path)) => {
                self.endpoint_ordinal == 0
                    && self.path == *path
                    && self.kind == NativePatchEndpointKind::Updated
            }
            Some(PatchEffect::Delete(path)) => {
                self.endpoint_ordinal == 0
                    && self.path == *path
                    && self.kind == NativePatchEndpointKind::Deleted
            }
            Some(PatchEffect::Move {
                source,
                destination,
            }) => match self.endpoint_ordinal {
                0 => self.path == *destination && self.kind == NativePatchEndpointKind::Created,
                1 => self.path == *source && self.kind == NativePatchEndpointKind::Deleted,
                _ => false,
            },
            None => false,
        }
    }
}

/// Requires canonical absolute spelling, not hidden parent traversal/aliases.
fn normal_absolute(path: &std::path::Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .collect::<std::path::PathBuf>()
            .as_os_str()
            == path.as_os_str()
        && !path.components().any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
}

/// Serializes original semantic intent independently of partial confirmations.
pub(crate) fn effect_json(effect: &PatchEffect) -> serde_json::Value {
    match effect {
        PatchEffect::Create(path) => serde_json::json!({"kind":"create","path":path}),
        PatchEffect::Update(path) => serde_json::json!({"kind":"update","path":path}),
        PatchEffect::Delete(path) => serde_json::json!({"kind":"delete","path":path}),
        PatchEffect::Move {
            source,
            destination,
        } => serde_json::json!({"kind":"move","source":source,"destination":destination}),
    }
}
