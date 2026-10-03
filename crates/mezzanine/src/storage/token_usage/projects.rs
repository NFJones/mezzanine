//! Private accounting project mapping, independent of execution authority.
//!
//! Root bytes are stored losslessly once, never copied into usage events. A
//! canonical path and directory object identity allocate an opaque ID together.
//! Path replacement creates a new mapping; relocation is never guessed. Old
//! mappings remain as historical tombstones. Trust inventory, not usage keys,
//! determines registered rows, including zero-use and revoked projects.

use std::collections::BTreeMap;
use std::os::unix::ffi::OsStrExt;
#[cfg(test)]
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use super::{MezError, Result, TokenUsageStore, new_token_usage_event_id};
use crate::security::project::{ProjectTrustRecord, TrustDecision};
use rusqlite::{OptionalExtension, params};

const MAX_REGISTERED_PROJECTS: usize = 4096;

/// Opaque accounting identity. Possession grants no filesystem or MMP authority.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct AccountingProjectId(String);

impl AccountingProjectId {
    /// Returns opaque storage identity without granting project authority.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    /// Validates a stored opaque identity; resolution and trust remain separate.
    pub(crate) fn from_stored(value: String) -> Result<Self> {
        if value.len() != 36
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
        {
            return Err(MezError::invalid_state(
                "stored accounting project identity is invalid",
            ));
        }
        Ok(Self(value))
    }
}

impl AccountingOrigin {
    /// Returns the frozen project ID, or absence for unattributed expense.
    pub(crate) fn project_id(&self) -> Option<&AccountingProjectId> {
        match self {
            Self::Unattributed => None,
            Self::Project(id) => Some(id),
        }
    }
}

/// Frozen attribution attached to one request independently of its execution owner.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum AccountingOrigin {
    /// No currently eligible trusted root or qualified mapping was available.
    #[default]
    Unattributed,
    /// Exact opaque project identity captured before provider dispatch.
    Project(AccountingProjectId),
}

/// One registered inventory row; unavailable directories remain reportable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AccountingProjectRecord {
    /// ID exists only when a directory object could be qualified.
    pub(crate) id: Option<AccountingProjectId>,
    /// Canonical root or original missing-root bytes, for inert display only.
    pub(crate) root: PathBuf,
    /// Current trust state from the inventory snapshot, not authority by itself.
    pub(crate) trust: TrustDecision,
    /// Trust policy version associated with this inventory row.
    pub(crate) trust_policy_version: u32,
    /// Configuration version associated with this inventory row.
    pub(crate) configuration_schema_version: u32,
    /// Captured directory object used to reject path reuse during request capture.
    object: Option<String>,
}

/// Reads bounded native directory identity without spawning processes.
fn directory_object(path: &Path) -> Result<String> {
    let metadata = std::fs::metadata(path)?;
    if !metadata.is_dir() {
        return Err(MezError::invalid_args(
            "accounting project root is not a directory",
        ));
    }
    Ok(format!("{}:{}", metadata.dev(), metadata.ino()))
}

/// Resolves an already trusted root against a qualified mapping snapshot.
/// Canonicalization or object mismatch fails unattributed, never to an ancestor.
pub(crate) fn accounting_origin_for_root(
    root: Option<&Path>,
    inventory: Option<&[AccountingProjectRecord]>,
) -> AccountingOrigin {
    let Some(root) = root.and_then(|root| std::fs::canonicalize(root).ok()) else {
        return AccountingOrigin::Unattributed;
    };
    let Ok(object) = directory_object(&root) else {
        return AccountingOrigin::Unattributed;
    };
    inventory
        .and_then(|rows| {
            rows.iter()
                .find(|row| row.root == root && row.object.as_ref() == Some(&object))
        })
        .and_then(|row| row.id.clone())
        .map(AccountingOrigin::Project)
        .unwrap_or_default()
}

impl TokenUsageStore {
    /// Qualifies registered trust inventory under one mapping writer transaction.
    /// Runs on the preparation worker, never a render or status-read path. Missing
    /// roots remain labelled inventory rows but cannot acquire request attribution.
    pub(crate) fn prepare_accounting_projects(
        &self,
        records: &[ProjectTrustRecord],
    ) -> Result<Vec<AccountingProjectRecord>> {
        if records.len() > MAX_REGISTERED_PROJECTS {
            return Err(MezError::invalid_state(
                "accounting project inventory exceeds its bound",
            ));
        }
        let mut connection = self.open()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut rows = BTreeMap::new();
        for record in records {
            let root = std::fs::canonicalize(&record.project_root)
                .unwrap_or_else(|_| record.project_root.clone());
            let object = directory_object(&root).ok();
            let id = if let Some(object) = object.as_ref() {
                let bytes = root.as_os_str().as_bytes();
                let existing: Option<String> = tx
                    .query_row(
                        "SELECT id FROM accounting_projects WHERE root=?1 AND object=?2",
                        params![bytes, object],
                        |row| row.get(0),
                    )
                    .optional()?;
                let id = match existing {
                    Some(id) => id,
                    None => {
                        let id = new_token_usage_event_id();
                        tx.execute(
                            "INSERT INTO accounting_projects(id,root,object) VALUES(?1,?2,?3)",
                            params![id, bytes, object],
                        )?;
                        id
                    }
                };
                Some(AccountingProjectId(id))
            } else {
                None
            };
            rows.insert(
                root.clone(),
                AccountingProjectRecord {
                    id,
                    root,
                    object,
                    trust: record.state,
                    trust_policy_version: record.trust_policy_version,
                    configuration_schema_version: record.configuration_schema_version,
                },
            );
        }
        tx.commit()?;
        Ok(rows.into_values().collect())
    }

    /// Lists historical mappings without requiring their root directories to exist.
    /// Registration and trust labels are supplied separately by current inventory.
    #[cfg(test)]
    pub(crate) fn historical_accounting_projects(
        &self,
    ) -> Result<Vec<(AccountingProjectId, PathBuf)>> {
        let connection = self.open()?;
        let mut statement =
            connection.prepare("SELECT id,root FROM accounting_projects ORDER BY id")?;
        let rows = statement.query_map([], |row| {
            Ok((
                AccountingProjectId(row.get(0)?),
                PathBuf::from(std::ffi::OsString::from_vec(row.get::<_, Vec<u8>>(1)?)),
            ))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }
}
