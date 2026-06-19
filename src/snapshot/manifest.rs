//! Snapshot manifest validation and line-oriented persistence.
//!
//! Manifests carry user-facing snapshot metadata and safety flags. Persistence
//! validates that credentials and active approval authority are never restored.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

use crate::error::{MezError, Result};

use super::encoding::{
    manifest_string_array, non_empty_optional, parse_bool, parse_string_array, parse_u32,
    parse_usize, required, set_private_dir_permissions, set_private_dir_permissions_async,
    set_private_file_permissions, set_private_file_permissions_async,
};
use super::types::{SnapshotKind, SnapshotManifest, SnapshotState};

impl SnapshotManifest {
    /// Runs the validate for persistence operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn validate_for_persistence(&self) -> Result<()> {
        if self.contains_raw_credentials {
            return Err(MezError::forbidden(
                "session snapshots must not contain raw credentials",
            ));
        }
        if self.active_approvals_restored {
            return Err(MezError::forbidden(
                "session snapshots must not restore active approval authority",
            ));
        }
        Ok(())
    }

    /// Runs the write to dir operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn write_to_dir(&self, dir: &Path) -> Result<PathBuf> {
        self.validate_for_persistence()?;
        fs::create_dir_all(dir)?;
        set_private_dir_permissions(dir)?;
        let path = dir.join(format!("{}.manifest", self.state.id));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.write_all(self.encode().as_bytes())?;
        set_private_file_permissions(&path)?;
        Ok(path)
    }

    /// Writes the manifest through Tokio filesystem APIs with private permissions.
    pub async fn write_to_dir_async(&self, dir: &Path) -> Result<PathBuf> {
        self.validate_for_persistence()?;
        tokio::fs::create_dir_all(dir).await?;
        set_private_dir_permissions_async(dir).await?;
        let path = dir.join(format!("{}.manifest", self.state.id));
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await?;
        file.write_all(self.encode().as_bytes()).await?;
        set_private_file_permissions_async(&path).await?;
        Ok(path)
    }

    /// Runs the read from file operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn read_from_file(path: &Path) -> Result<Self> {
        let mut data = String::new();
        fs::File::open(path)?.read_to_string(&mut data)?;
        Self::decode(&data)
    }

    /// Reads and decodes a manifest through Tokio filesystem APIs.
    pub async fn read_from_file_async(path: &Path) -> Result<Self> {
        let data = tokio::fs::read_to_string(path).await?;
        Self::decode(&data)
    }

    /// Runs the encode operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn encode(&self) -> String {
        format!(
            "id={}\nversion={}\nsession_id={}\nname={}\ncreated_at={}\nkind={}\nrestorable={}\nwindow_count={}\npane_count={}\nlimitations={}\nstorage_ref={}\ncontains_terminal_history={}\ncontains_agent_transcripts={}\ncontains_raw_credentials={}\nactive_approvals_restored={}\nrestart_required_panes={}\n",
            self.state.id,
            self.state.version,
            self.state.session_id,
            self.state.name.as_deref().unwrap_or(""),
            self.state.created_at,
            match &self.state.kind {
                SnapshotKind::Live => "live",
                SnapshotKind::Manual => "manual",
                SnapshotKind::Automatic => "automatic",
                SnapshotKind::CrashRecovery => "crash_recovery",
            },
            self.state.restorable,
            self.state.window_count,
            self.state.pane_count,
            manifest_string_array(&self.state.limitations),
            self.state.storage_ref,
            self.contains_terminal_history,
            self.contains_agent_transcripts,
            self.contains_raw_credentials,
            self.active_approvals_restored,
            manifest_string_array(&self.restart_required_panes),
        )
    }

    /// Runs the decode operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn decode(data: &str) -> Result<Self> {
        let mut map = std::collections::BTreeMap::new();
        for line in data.lines() {
            let Some((key, value)) = line.split_once('=') else {
                return Err(MezError::invalid_args("malformed snapshot manifest line"));
            };
            map.insert(key, value);
        }

        let version = map
            .get("version")
            .copied()
            .map(parse_u32)
            .transpose()?
            .unwrap_or(1);

        let kind = match map.get("kind").copied() {
            Some("live" | "Live") => SnapshotKind::Live,
            Some("manual" | "Manual") => SnapshotKind::Manual,
            Some("automatic" | "Automatic") => SnapshotKind::Automatic,
            Some("crash_recovery" | "CrashRecovery") => SnapshotKind::CrashRecovery,
            Some(_) => return Err(MezError::invalid_args("unknown snapshot kind")),
            None if version <= 1 => SnapshotKind::Manual,
            None => {
                return Err(MezError::invalid_args(
                    "missing snapshot manifest field `kind`",
                ));
            }
        };

        let manifest = Self {
            state: SnapshotState {
                id: required(&map, "id")?.to_string(),
                version,
                session_id: required(&map, "session_id")?.to_string(),
                name: non_empty_optional(&map, "name"),
                created_at: map
                    .get("created_at")
                    .copied()
                    .unwrap_or("1970-01-01T00:00:00Z")
                    .to_string(),
                kind,
                restorable: parse_bool(required(&map, "restorable")?)?,
                window_count: parse_usize(required(&map, "window_count")?)?,
                pane_count: parse_usize(required(&map, "pane_count")?)?,
                limitations: map
                    .get("limitations")
                    .copied()
                    .map(parse_string_array)
                    .transpose()?
                    .unwrap_or_default(),
                storage_ref: required(&map, "storage_ref")?.to_string(),
            },
            contains_terminal_history: parse_bool(required(&map, "contains_terminal_history")?)?,
            contains_agent_transcripts: parse_bool(required(&map, "contains_agent_transcripts")?)?,
            contains_raw_credentials: parse_bool(required(&map, "contains_raw_credentials")?)?,
            active_approvals_restored: parse_bool(required(&map, "active_approvals_restored")?)?,
            restart_required_panes: map
                .get("restart_required_panes")
                .copied()
                .map(parse_string_array)
                .transpose()?
                .unwrap_or_default(),
        };
        manifest.validate_for_persistence()?;
        Ok(manifest)
    }
}
