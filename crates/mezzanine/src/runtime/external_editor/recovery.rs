//! Durable external-editor lifecycle manifests and orphan discovery.
//!
//! Manifests contain only bounded non-draft metadata. Draft text remains in
//! the owner-only `draft.md` file and is reopened through the artifact
//! validator before any recovery operation may use it.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::artifacts::{ExternalEditorArtifacts, validate_external_editor_draft};
use super::session::ExternalEditTarget;
use crate::error::{MezError, Result};

const RECOVERY_MANIFEST_VERSION: u8 = 1;
const RECOVERY_MANIFEST_FILE: &str = "session.json";
const RECOVERY_MANIFEST_MAX_BYTES: u64 = 64 * 1024;
pub(super) const RECOVERY_DRAFT_MAX_BYTES: u64 =
    mez_mux::readline::MAX_READLINE_HISTORY_ENTRY_BYTES as u64;
pub(super) const RECOVERY_DRAFT_MAX_LINES: usize = 100_000;
static NEXT_TEMPORARY_ID: AtomicU64 = AtomicU64::new(0);

/// Durable lifecycle state for one retained editor artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExternalEditorRecoveryState {
    /// Editor launch or process execution had not settled before restart.
    Interrupted,
    /// Editor exited nonzero after producing a changed draft.
    NonzeroExit,
    /// Final draft failed filesystem or content validation.
    Invalid,
    /// Valid changed content has not yet been applied to its target.
    ChangedUnapplied,
    /// A future durable target reported a compare-and-swap conflict.
    Conflicted,
}

impl ExternalEditorRecoveryState {
    /// Returns the stable bounded label shown without draft content.
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Interrupted => "interrupted",
            Self::NonzeroExit => "nonzero_exit",
            Self::Invalid => "invalid",
            Self::ChangedUnapplied => "changed_unapplied",
            Self::Conflicted => "conflicted",
        }
    }
}

/// Non-secret durable metadata stored beside one private editor draft.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ExternalEditorRecoveryManifest {
    version: u8,
    pub(super) session_id: String,
    pub(super) runtime_session_id: String,
    pub(super) pane_id: String,
    pub(super) target: ExternalEditTarget,
    original_sha256: String,
    pub(super) state: ExternalEditorRecoveryState,
    pub(super) exit_code: Option<i32>,
}

/// One validated orphan available for explicit recovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExternalEditorRecoveryRecord {
    /// Opaque retained session identity.
    pub(crate) session_id: String,
    /// Runtime session that owns the retained draft.
    pub(crate) runtime_session_id: String,
    /// Pane that originally hosted the editor.
    pub(crate) pane_id: String,
    /// Typed target that may consume a validated recovery.
    pub(crate) target: ExternalEditTarget,
    /// Why this draft remains recoverable.
    pub(crate) state: ExternalEditorRecoveryState,
    /// Original editor exit code when available.
    pub(crate) exit_code: Option<i32>,
    pub(super) artifacts: ExternalEditorArtifacts,
}

impl ExternalEditorRecoveryManifest {
    /// Creates the initial interrupted-safe manifest before editor launch.
    pub(super) fn new(
        session_id: String,
        runtime_session_id: String,
        pane_id: String,
        target: ExternalEditTarget,
        original_content: &str,
    ) -> Self {
        Self {
            version: RECOVERY_MANIFEST_VERSION,
            session_id,
            runtime_session_id,
            pane_id,
            target,
            original_sha256: sha256_text(original_content),
            state: ExternalEditorRecoveryState::Interrupted,
            exit_code: None,
        }
    }

    /// Reports whether validated content differs from the original target.
    pub(super) fn content_changed(&self, content: &str) -> bool {
        self.original_sha256 != sha256_text(content)
    }

    /// Reports whether content still matches the snapshot captured at launch.
    pub(super) fn original_content_matches(&self, content: &str) -> bool {
        self.original_sha256 == sha256_text(content)
    }

    /// Advances durable lifecycle metadata without serializing draft content.
    pub(super) fn set_state(&mut self, state: ExternalEditorRecoveryState, exit_code: Option<i32>) {
        self.state = state;
        self.exit_code = exit_code;
    }

    /// Converts durable metadata and private artifacts into a recovery record.
    pub(super) fn into_record(
        self,
        artifacts: ExternalEditorArtifacts,
    ) -> ExternalEditorRecoveryRecord {
        ExternalEditorRecoveryRecord {
            session_id: self.session_id,
            runtime_session_id: self.runtime_session_id,
            pane_id: self.pane_id,
            target: self.target,
            state: self.state,
            exit_code: self.exit_code,
            artifacts,
        }
    }
}

/// Atomically writes one owner-only recovery manifest.
pub(super) fn write_recovery_manifest(
    artifacts: &ExternalEditorArtifacts,
    manifest: &ExternalEditorRecoveryManifest,
) -> Result<()> {
    let bytes = serde_json::to_vec(manifest).map_err(|error| {
        MezError::invalid_state(format!(
            "failed to encode editor recovery manifest: {error}"
        ))
    })?;
    if bytes.len() as u64 > RECOVERY_MANIFEST_MAX_BYTES {
        return Err(MezError::invalid_args(
            "external editor recovery manifest exceeds the size limit",
        ));
    }
    let path = artifacts.session_directory.join(RECOVERY_MANIFEST_FILE);
    let temporary = artifacts.session_directory.join(format!(
        ".session.{}.{}.tmp",
        std::process::id(),
        NEXT_TEMPORARY_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, &path)?;
        validate_private_manifest(&path)?;
        fs::File::open(&artifacts.session_directory)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

/// Discovers valid retained editor sessions without auto-applying drafts.
pub(super) fn discover_external_editor_recoveries(
    runtime_root: &Path,
    runtime_session_id: &str,
) -> Result<Vec<ExternalEditorRecoveryRecord>> {
    let root = runtime_root.join("editor-sessions");
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    let mut entries = fs::read_dir(&root)?
        .filter_map(|entry| entry.ok())
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let session_directory = entry.path();
        let session_id = entry.file_name().to_string_lossy().into_owned();
        if !valid_session_id(&session_id) {
            continue;
        }
        let artifacts = ExternalEditorArtifacts {
            draft_path: session_directory.join("draft.md"),
            session_directory,
        };
        let Ok(mut manifest) = read_recovery_manifest(&artifacts) else {
            continue;
        };
        if manifest.session_id != session_id || manifest.runtime_session_id != runtime_session_id {
            continue;
        }
        if validate_external_editor_draft(
            &artifacts,
            RECOVERY_DRAFT_MAX_BYTES,
            RECOVERY_DRAFT_MAX_LINES,
        )
        .is_err()
        {
            manifest.state = ExternalEditorRecoveryState::Invalid;
            let _ = write_recovery_manifest(&artifacts, &manifest);
        }
        records.push(ExternalEditorRecoveryRecord {
            session_id: manifest.session_id,
            runtime_session_id: manifest.runtime_session_id,
            pane_id: manifest.pane_id,
            target: manifest.target,
            state: manifest.state,
            exit_code: manifest.exit_code,
            artifacts,
        });
    }
    Ok(records)
}

/// Reads and validates one retained manifest through `O_NOFOLLOW`.
pub(super) fn read_recovery_manifest(
    artifacts: &ExternalEditorArtifacts,
) -> Result<ExternalEditorRecoveryManifest> {
    let path = artifacts.session_directory.join(RECOVERY_MANIFEST_FILE);
    validate_private_manifest(&path)?;
    let directory = fs::File::open(&artifacts.session_directory)?;
    let descriptor = rustix::fs::openat(
        &directory,
        RECOVERY_MANIFEST_FILE,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(std::io::Error::from)?;
    let mut file = fs::File::from(descriptor);
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(RECOVERY_MANIFEST_MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > RECOVERY_MANIFEST_MAX_BYTES {
        return Err(MezError::invalid_args(
            "external editor recovery manifest exceeds the size limit",
        ));
    }
    let manifest: ExternalEditorRecoveryManifest = serde_json::from_slice(&bytes)
        .map_err(|_| MezError::invalid_args("invalid external editor recovery manifest"))?;
    if manifest.version != RECOVERY_MANIFEST_VERSION || !valid_session_id(&manifest.session_id) {
        return Err(MezError::invalid_args(
            "unsupported external editor recovery manifest",
        ));
    }
    Ok(manifest)
}

/// Removes one retained session after explicit discard or successful apply.
pub(super) fn discard_recovery_artifacts(record: &ExternalEditorRecoveryRecord) -> Result<()> {
    match fs::remove_dir_all(&record.artifacts.session_directory) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn validate_private_manifest(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o077 != 0
            || metadata.nlink() != 1
        {
            return Err(MezError::forbidden(
                "external editor recovery manifest must be a private single-link regular file",
            ));
        }
    }
    #[cfg(not(unix))]
    if !metadata.is_file() {
        return Err(MezError::forbidden(
            "external editor recovery manifest must be a regular file",
        ));
    }
    Ok(())
}

fn valid_session_id(value: &str) -> bool {
    value.len() == 96 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn sha256_text(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies valid manifests survive restart discovery, malformed metadata
    /// is skipped, and explicit discard is idempotent.
    #[test]
    fn discovers_valid_recoveries_and_discards_idempotently() {
        let root =
            std::env::temp_dir().join(format!("mez-editor-recovery-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let session_id = "a".repeat(96);
        let artifacts = super::super::artifacts::create_external_editor_artifacts(
            &root,
            &session_id,
            "changed",
        )
        .unwrap();
        let manifest = ExternalEditorRecoveryManifest::new(
            session_id.clone(),
            "runtime-session".to_string(),
            "%1".to_string(),
            ExternalEditTarget::AgentPrompt,
            "before",
        );
        write_recovery_manifest(&artifacts, &manifest).unwrap();
        fs::create_dir_all(root.join("editor-sessions/not-a-session")).unwrap();

        let records = discover_external_editor_recoveries(&root, "runtime-session").unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].session_id, session_id);
        assert_eq!(records[0].state, ExternalEditorRecoveryState::Interrupted);
        discard_recovery_artifacts(&records[0]).unwrap();
        discard_recovery_artifacts(&records[0]).unwrap();
        let _ = fs::remove_dir_all(root);
    }
}
