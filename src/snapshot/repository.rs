//! Filesystem-backed snapshot repository operations.
//!
//! Repository methods own manifest and payload paths, listing, inspection,
//! deletion, and idempotent creation from live sessions.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

use crate::error::{MezError, Result};
use crate::session::Session;

use super::encoding::{
    current_rfc3339_utc, has_manifest_control_character, set_private_dir_permissions,
    set_private_dir_permissions_async, set_private_file_permissions,
    set_private_file_permissions_async, validate_snapshot_id,
};
use super::types::{
    SessionSnapshotPayload, SnapshotConfigLayerMetadata, SnapshotCreationContext,
    SnapshotFrameState, SnapshotKind, SnapshotManifest, SnapshotPaneCapture, SnapshotRepository,
    SnapshotState,
};

impl SnapshotRepository {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Runs the root operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Runs the write operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn write(&self, manifest: &SnapshotManifest) -> Result<PathBuf> {
        manifest.write_to_dir(&self.root)
    }

    /// Writes a snapshot manifest through Tokio filesystem APIs.
    pub async fn write_async(&self, manifest: &SnapshotManifest) -> Result<PathBuf> {
        manifest.write_to_dir_async(&self.root).await
    }

    /// Runs the write payload operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn write_payload(
        &self,
        snapshot_id: &str,
        payload: &SessionSnapshotPayload,
    ) -> Result<PathBuf> {
        validate_snapshot_id(snapshot_id)?;
        payload.validate()?;
        fs::create_dir_all(&self.root)?;
        set_private_dir_permissions(&self.root)?;
        let path = self.payload_path(snapshot_id)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let encoded = payload.encode()?;
        file.write_all(encoded.as_bytes())?;
        set_private_file_permissions(&path)?;
        Ok(path)
    }

    /// Writes a snapshot payload through Tokio filesystem APIs.
    pub async fn write_payload_async(
        &self,
        snapshot_id: &str,
        payload: &SessionSnapshotPayload,
    ) -> Result<PathBuf> {
        validate_snapshot_id(snapshot_id)?;
        payload.validate()?;
        tokio::fs::create_dir_all(&self.root).await?;
        set_private_dir_permissions_async(&self.root).await?;
        let path = self.payload_path(snapshot_id)?;
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await?;
        let encoded = payload.encode()?;
        file.write_all(encoded.as_bytes()).await?;
        set_private_file_permissions_async(&path).await?;
        Ok(path)
    }

    /// Runs the inspect payload operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn inspect_payload(&self, snapshot_id: &str) -> Result<SessionSnapshotPayload> {
        let path = self.payload_path(snapshot_id)?;
        if !path.exists() {
            return Err(MezError::new(
                crate::error::MezErrorKind::NotFound,
                "snapshot payload not found",
            ));
        }
        let mut data = String::new();
        fs::File::open(path)?.read_to_string(&mut data)?;
        SessionSnapshotPayload::decode(&data)
    }

    /// Reads a snapshot payload through Tokio filesystem APIs.
    pub async fn inspect_payload_async(&self, snapshot_id: &str) -> Result<SessionSnapshotPayload> {
        let path = self.payload_path(snapshot_id)?;
        let data = match tokio::fs::read_to_string(&path).await {
            Ok(data) => data,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "snapshot payload not found",
                ));
            }
            Err(error) => return Err(error.into()),
        };
        SessionSnapshotPayload::decode(&data)
    }

    /// Runs the list operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn list(&self) -> Result<Vec<SnapshotState>> {
        let mut snapshots = Vec::new();
        if !self.root.exists() {
            return Ok(snapshots);
        }

        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("manifest") {
                continue;
            }
            snapshots.push(SnapshotManifest::read_from_file(&path)?.state);
        }
        snapshots.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(snapshots)
    }

    /// Lists snapshot manifests through Tokio filesystem APIs.
    pub async fn list_async(&self) -> Result<Vec<SnapshotState>> {
        let mut snapshots = Vec::new();
        let mut entries = match tokio::fs::read_dir(&self.root).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(snapshots),
            Err(error) => return Err(error.into()),
        };

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("manifest") {
                continue;
            }
            snapshots.push(SnapshotManifest::read_from_file_async(&path).await?.state);
        }
        snapshots.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(snapshots)
    }

    /// Runs the inspect operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn inspect(&self, snapshot_id: &str) -> Result<SnapshotManifest> {
        let path = self.manifest_path(snapshot_id)?;
        if !path.exists() {
            return Err(MezError::new(
                crate::error::MezErrorKind::NotFound,
                "snapshot not found",
            ));
        }
        SnapshotManifest::read_from_file(&path)
    }

    /// Inspects one snapshot manifest through Tokio filesystem APIs.
    pub async fn inspect_async(&self, snapshot_id: &str) -> Result<SnapshotManifest> {
        let path = self.manifest_path(snapshot_id)?;
        match SnapshotManifest::read_from_file_async(&path).await {
            Ok(manifest) => Ok(manifest),
            Err(error) if error.io_kind() == Some(std::io::ErrorKind::NotFound) => Err(
                MezError::new(crate::error::MezErrorKind::NotFound, "snapshot not found"),
            ),
            Err(error) => Err(error),
        }
    }

    /// Runs the delete operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn delete(&self, snapshot_id: &str) -> Result<bool> {
        let path = self.manifest_path(snapshot_id)?;
        if !path.exists() {
            return Ok(false);
        }
        let manifest = SnapshotManifest::read_from_file(&path)?;
        fs::remove_file(&path)?;
        self.remove_payload_if_local(&manifest)?;
        Ok(true)
    }

    /// Deletes one snapshot manifest and its local payload through Tokio filesystem APIs.
    pub async fn delete_async(&self, snapshot_id: &str) -> Result<bool> {
        let path = self.manifest_path(snapshot_id)?;
        let manifest = match SnapshotManifest::read_from_file_async(&path).await {
            Ok(manifest) => manifest,
            Err(error) if error.io_kind() == Some(std::io::ErrorKind::NotFound) => {
                return Ok(false);
            }
            Err(error) => return Err(error),
        };
        tokio::fs::remove_file(&path).await?;
        self.remove_payload_if_local_async(&manifest).await?;
        Ok(true)
    }

    /// Runs the create from session operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn create_from_session(
        &self,
        snapshot_id: &str,
        name: Option<String>,
        session: &Session,
    ) -> Result<SnapshotState> {
        self.create_from_session_with_captures(snapshot_id, name, session, &[])
    }

    /// Runs the create from session with captures operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn create_from_session_with_captures(
        &self,
        snapshot_id: &str,
        name: Option<String>,
        session: &Session,
        pane_captures: &[SnapshotPaneCapture],
    ) -> Result<SnapshotState> {
        self.create_from_session_with_captures_and_config_layers(
            snapshot_id,
            name,
            session,
            pane_captures,
            &[],
        )
    }

    /// Runs the create from session with captures and config layers operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn create_from_session_with_captures_and_config_layers(
        &self,
        snapshot_id: &str,
        name: Option<String>,
        session: &Session,
        pane_captures: &[SnapshotPaneCapture],
        active_config_layers: &[SnapshotConfigLayerMetadata],
    ) -> Result<SnapshotState> {
        let frame_state = SnapshotFrameState::default();
        self.create_from_session_with_context(
            snapshot_id,
            name,
            session,
            SnapshotCreationContext::new(pane_captures, active_config_layers, &frame_state, &[]),
        )
    }

    /// Runs the create from session with captures and config layers and frame state operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn create_from_session_with_captures_and_config_layers_and_frame_state(
        &self,
        snapshot_id: &str,
        name: Option<String>,
        session: &Session,
        pane_captures: &[SnapshotPaneCapture],
        active_config_layers: &[SnapshotConfigLayerMetadata],
        frame_state: &SnapshotFrameState,
    ) -> Result<SnapshotState> {
        self.create_from_session_with_context(
            snapshot_id,
            name,
            session,
            SnapshotCreationContext::new(pane_captures, active_config_layers, frame_state, &[]),
        )
    }

    /// Runs the create from session with context operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn create_from_session_with_context(
        &self,
        snapshot_id: &str,
        name: Option<String>,
        session: &Session,
        context: SnapshotCreationContext<'_>,
    ) -> Result<SnapshotState> {
        validate_snapshot_id(snapshot_id)?;
        if name.as_deref().is_some_and(has_manifest_control_character) {
            return Err(MezError::invalid_args(
                "snapshot name must not contain manifest control characters",
            ));
        }

        if let Ok(existing) = self.inspect(snapshot_id) {
            let requested_name = name.as_deref();
            let existing_name = existing.state.name.as_deref();
            if existing.state.session_id == session.id.to_string()
                && existing_name == requested_name
            {
                return Ok(existing.state);
            }
            return Err(MezError::conflict(
                "idempotent snapshot create key refers to a different snapshot",
            ));
        }

        let payload = SessionSnapshotPayload::from_session_with_context(session, context);
        let plan = payload.resume_plan();
        let contains_terminal_history = payload.contains_terminal_history();
        let contains_agent_transcripts = payload.contains_agent_transcripts();
        let manifest = SnapshotManifest {
            state: SnapshotState {
                id: snapshot_id.to_string(),
                version: 1,
                session_id: payload.session_id.clone(),
                name,
                created_at: current_rfc3339_utc(),
                kind: SnapshotKind::Manual,
                restorable: true,
                window_count: plan.window_count,
                pane_count: plan.pane_count,
                limitations: plan.limitations,
                storage_ref: format!("{snapshot_id}.payload"),
            },
            contains_terminal_history,
            contains_agent_transcripts,
            contains_raw_credentials: false,
            active_approvals_restored: false,
        };

        self.write_payload(snapshot_id, &payload)?;
        match self.write(&manifest) {
            Ok(_) => Ok(manifest.state),
            Err(error) => {
                let _ = fs::remove_file(self.payload_path(snapshot_id)?);
                Err(error)
            }
        }
    }

    /// Creates a snapshot from live session state through Tokio filesystem APIs.
    pub async fn create_from_session_with_context_async(
        &self,
        snapshot_id: &str,
        name: Option<String>,
        session: &Session,
        context: SnapshotCreationContext<'_>,
    ) -> Result<SnapshotState> {
        validate_snapshot_id(snapshot_id)?;
        if name.as_deref().is_some_and(has_manifest_control_character) {
            return Err(MezError::invalid_args(
                "snapshot name must not contain manifest control characters",
            ));
        }

        if let Ok(existing) = self.inspect_async(snapshot_id).await {
            let requested_name = name.as_deref();
            let existing_name = existing.state.name.as_deref();
            if existing.state.session_id == session.id.to_string()
                && existing_name == requested_name
            {
                return Ok(existing.state);
            }
            return Err(MezError::conflict(
                "idempotent snapshot create key refers to a different snapshot",
            ));
        }

        let payload = SessionSnapshotPayload::from_session_with_context(session, context);
        let plan = payload.resume_plan();
        let contains_terminal_history = payload.contains_terminal_history();
        let contains_agent_transcripts = payload.contains_agent_transcripts();
        let manifest = SnapshotManifest {
            state: SnapshotState {
                id: snapshot_id.to_string(),
                version: 1,
                session_id: payload.session_id.clone(),
                name,
                created_at: current_rfc3339_utc(),
                kind: SnapshotKind::Manual,
                restorable: true,
                window_count: plan.window_count,
                pane_count: plan.pane_count,
                limitations: plan.limitations,
                storage_ref: format!("{snapshot_id}.payload"),
            },
            contains_terminal_history,
            contains_agent_transcripts,
            contains_raw_credentials: false,
            active_approvals_restored: false,
        };

        self.write_payload_async(snapshot_id, &payload).await?;
        match self.write_async(&manifest).await {
            Ok(_) => Ok(manifest.state),
            Err(error) => {
                let _ = tokio::fs::remove_file(self.payload_path(snapshot_id)?).await;
                Err(error)
            }
        }
    }

    /// Runs the manifest path operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn manifest_path(&self, snapshot_id: &str) -> Result<PathBuf> {
        validate_snapshot_id(snapshot_id)?;
        Ok(self.root.join(format!("{snapshot_id}.manifest")))
    }

    /// Runs the payload path operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn payload_path(&self, snapshot_id: &str) -> Result<PathBuf> {
        validate_snapshot_id(snapshot_id)?;
        Ok(self.root.join(format!("{snapshot_id}.payload")))
    }

    /// Runs the remove payload if local operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn remove_payload_if_local(&self, manifest: &SnapshotManifest) -> Result<()> {
        let storage_ref = Path::new(&manifest.state.storage_ref);
        let payload_path = if storage_ref.is_absolute() {
            storage_ref.to_path_buf()
        } else {
            self.root.join(storage_ref)
        };

        if !payload_path.starts_with(&self.root) || !payload_path.exists() {
            return Ok(());
        }
        if payload_path.is_dir() {
            fs::remove_dir_all(payload_path)?;
        } else {
            fs::remove_file(payload_path)?;
        }
        Ok(())
    }

    /// Runs the remove payload if local async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    async fn remove_payload_if_local_async(&self, manifest: &SnapshotManifest) -> Result<()> {
        let storage_ref = Path::new(&manifest.state.storage_ref);
        let payload_path = if storage_ref.is_absolute() {
            storage_ref.to_path_buf()
        } else {
            self.root.join(storage_ref)
        };

        if !payload_path.starts_with(&self.root) {
            return Ok(());
        }
        let Ok(metadata) = tokio::fs::metadata(&payload_path).await else {
            return Ok(());
        };
        if metadata.is_dir() {
            tokio::fs::remove_dir_all(payload_path).await?;
        } else {
            tokio::fs::remove_file(payload_path).await?;
        }
        Ok(())
    }
}
