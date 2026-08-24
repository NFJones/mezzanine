//! Filesystem-backed snapshot repository operations.
//!
//! Repository methods own manifest and payload paths, listing, inspection,
//! deletion, and idempotent creation from live sessions.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::error::{MezError, Result};
use mez_mux::session::Session;

use super::encoding::{
    current_rfc3339_utc, has_manifest_control_character, reconcile_publication_temporaries,
    sync_directory, sync_directory_async, validate_snapshot_id, write_private_new_atomic,
    write_private_new_atomic_async, write_private_replace_atomic,
    write_private_replace_atomic_async,
};
use super::types::{
    SessionSnapshotPayload, SnapshotCreationContext, SnapshotKind, SnapshotManifest,
    SnapshotRepository, SnapshotState,
};
#[cfg(test)]
use super::types::{SnapshotConfigLayerMetadata, SnapshotFrameState, SnapshotPaneCapture};

/// Persisted global and per-session latest-snapshot identities.
#[derive(Debug, Default, PartialEq, Eq)]
struct LatestSnapshotIndex {
    latest_all: Option<String>,
    latest_by_session: BTreeMap<String, String>,
}

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
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "test-only adapter retained for focused boundary coverage"
    )]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Removes abandoned repository-owned temp files left before atomic
    /// publication completed.
    pub fn reconcile_publication_temporaries(&self) -> Result<usize> {
        reconcile_publication_temporaries(&self.root)
    }

    /// Runs the write operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn write(&self, manifest: &SnapshotManifest) -> Result<PathBuf> {
        let path = manifest.write_to_dir(&self.root)?;
        self.write_latest_indexes(&manifest.state)?;
        Ok(path)
    }

    /// Writes a snapshot manifest through Tokio filesystem APIs.
    pub async fn write_async(&self, manifest: &SnapshotManifest) -> Result<PathBuf> {
        let path = manifest.write_to_dir_async(&self.root).await?;
        self.write_latest_indexes_async(&manifest.state).await?;
        Ok(path)
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
        let path = self.payload_path(snapshot_id)?;
        let encoded = payload.encode()?;
        write_private_new_atomic(&path, encoded.as_bytes())
    }

    /// Writes a snapshot payload through Tokio filesystem APIs.
    pub async fn write_payload_async(
        &self,
        snapshot_id: &str,
        payload: &SessionSnapshotPayload,
    ) -> Result<PathBuf> {
        validate_snapshot_id(snapshot_id)?;
        payload.validate()?;
        let path = self.payload_path(snapshot_id)?;
        let encoded = payload.encode()?;
        write_private_new_atomic_async(&path, encoded.as_bytes()).await
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
        self.rebuild_latest_indexes()?;
        Ok(true)
    }

    /// Deletes one snapshot manifest and its local payload through Tokio filesystem APIs.
    pub async fn delete_async(&self, snapshot_id: &str) -> Result<bool> {
        let path = self.manifest_path(snapshot_id)?;
        let manifest = match SnapshotManifest::read_from_file_async(&path).await {
            Ok(manifest) => manifest,
            Err(error) if error.io_kind() == Some(std::io::ErrorKind::NotFound) => {
                let payload_path = self.payload_path(snapshot_id)?;
                let removed = match tokio::fs::metadata(&payload_path).await {
                    Ok(metadata) if metadata.is_dir() => {
                        tokio::fs::remove_dir_all(&payload_path).await?;
                        true
                    }
                    Ok(_) => {
                        tokio::fs::remove_file(&payload_path).await?;
                        true
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                    Err(error) => return Err(error.into()),
                };
                self.rebuild_latest_indexes_async().await?;
                return Ok(removed);
            }
            Err(error) => return Err(error),
        };
        self.remove_payload_if_local_async(&manifest).await?;
        tokio::fs::remove_file(&path).await?;
        self.rebuild_latest_indexes_async().await?;
        Ok(true)
    }

    /// Runs the create from session operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
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
    #[cfg(test)]
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
    #[cfg(test)]
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
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "test-only adapter retained for focused boundary coverage"
    )]
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
            restart_required_panes: plan.restart_required_panes,
        };

        self.write_payload(snapshot_id, &payload)?;
        match self.write(&manifest) {
            Ok(_) => Ok(manifest.state),
            Err(error) => {
                if !self.manifest_path(snapshot_id)?.exists()
                    && fs::remove_file(self.payload_path(snapshot_id)?).is_ok()
                {
                    let _ = sync_directory(&self.root);
                }
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
            restart_required_panes: plan.restart_required_panes,
        };

        self.write_payload_async(snapshot_id, &payload).await?;
        match self.write_async(&manifest).await {
            Ok(_) => Ok(manifest.state),
            Err(error) => {
                if tokio::fs::metadata(self.manifest_path(snapshot_id)?)
                    .await
                    .is_err()
                    && tokio::fs::remove_file(self.payload_path(snapshot_id)?)
                        .await
                        .is_ok()
                {
                    let _ = sync_directory_async(&self.root).await;
                }
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

    /// Returns the snapshot selected by the persisted latest index, when present.
    ///
    /// The helper keeps latest lookups on the small index file instead of
    /// requiring callers to enumerate and parse every manifest on hot paths.
    pub(crate) fn latest_from_index(
        &self,
        session_id: Option<&str>,
    ) -> Result<Option<SnapshotState>> {
        let Some(snapshot_id) = self.read_latest_index(session_id)? else {
            return Ok(None);
        };
        let manifest = self.inspect(&snapshot_id)?;
        if session_id.is_none_or(|session_id| manifest.state.session_id == session_id) {
            Ok(Some(manifest.state))
        } else {
            Ok(None)
        }
    }

    /// Compares two snapshot states by the repository latest ordering.
    ///
    /// Timestamps are primary, with snapshot ids as deterministic tie breakers
    /// so indexes and fallback scans choose the same candidate.
    pub(crate) fn compare_latest_snapshots(
        left: &SnapshotState,
        right: &SnapshotState,
    ) -> Ordering {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.id.cmp(&right.id))
    }

    /// Rebuilds the latest snapshot indexes from current manifests.
    ///
    /// Repository writes and deletes call this so lookup paths can inspect one
    /// bounded index file before falling back to a full manifest scan.
    fn rebuild_latest_indexes(&self) -> Result<()> {
        let snapshots = self.list()?;
        self.write_latest_index_file(&snapshots)
    }

    /// Rebuilds latest indexes asynchronously for deletion or recovery paths.
    async fn rebuild_latest_indexes_async(&self) -> Result<()> {
        let snapshots = self.list_async().await?;
        self.write_latest_index_file_async(&snapshots).await
    }

    /// Updates latest indexes after a successful manifest write.
    fn write_latest_indexes(&self, state: &SnapshotState) -> Result<()> {
        let mut index = match self.read_latest_index_state() {
            Ok(Some(index)) => index,
            Ok(None) | Err(_) => return self.rebuild_latest_indexes(),
        };
        if self.update_latest_index(&mut index, state).is_err() {
            return self.rebuild_latest_indexes();
        }
        self.write_latest_index_state(&index)
    }

    /// Updates latest indexes without synchronous filesystem work.
    async fn write_latest_indexes_async(&self, state: &SnapshotState) -> Result<()> {
        let mut index = match self.read_latest_index_state_async().await {
            Ok(Some(index)) => index,
            Ok(None) | Err(_) => return self.rebuild_latest_indexes_async().await,
        };
        if self
            .update_latest_index_async(&mut index, state)
            .await
            .is_err()
        {
            return self.rebuild_latest_indexes_async().await;
        }
        self.write_latest_index_state_async(&index).await
    }

    /// Compares a new snapshot with only the currently indexed winners.
    fn update_latest_index(
        &self,
        index: &mut LatestSnapshotIndex,
        state: &SnapshotState,
    ) -> Result<()> {
        let latest_all_id = index
            .latest_all
            .clone()
            .ok_or_else(|| MezError::invalid_state("snapshot latest index has no global entry"))?;
        let latest_all = self.inspect(&latest_all_id)?.state;
        if Self::compare_latest_snapshots(&latest_all, state) == Ordering::Less {
            index.latest_all = Some(state.id.clone());
        }

        if let Some(latest_session_id) = index.latest_by_session.get(&state.session_id).cloned() {
            let latest_session = self.inspect(&latest_session_id)?.state;
            if latest_session.session_id != state.session_id {
                return Err(MezError::invalid_state(
                    "snapshot latest index session entry points to another session",
                ));
            }
            if Self::compare_latest_snapshots(&latest_session, state) == Ordering::Less {
                index
                    .latest_by_session
                    .insert(state.session_id.clone(), state.id.clone());
            }
        } else {
            index
                .latest_by_session
                .insert(state.session_id.clone(), state.id.clone());
        }
        Ok(())
    }

    /// Async counterpart to [`Self::update_latest_index`].
    async fn update_latest_index_async(
        &self,
        index: &mut LatestSnapshotIndex,
        state: &SnapshotState,
    ) -> Result<()> {
        let latest_all_id = index
            .latest_all
            .clone()
            .ok_or_else(|| MezError::invalid_state("snapshot latest index has no global entry"))?;
        let latest_all = self.inspect_async(&latest_all_id).await?.state;
        if Self::compare_latest_snapshots(&latest_all, state) == Ordering::Less {
            index.latest_all = Some(state.id.clone());
        }

        if let Some(latest_session_id) = index.latest_by_session.get(&state.session_id).cloned() {
            let latest_session = self.inspect_async(&latest_session_id).await?.state;
            if latest_session.session_id != state.session_id {
                return Err(MezError::invalid_state(
                    "snapshot latest index session entry points to another session",
                ));
            }
            if Self::compare_latest_snapshots(&latest_session, state) == Ordering::Less {
                index
                    .latest_by_session
                    .insert(state.session_id.clone(), state.id.clone());
            }
        } else {
            index
                .latest_by_session
                .insert(state.session_id.clone(), state.id.clone());
        }
        Ok(())
    }

    /// Returns the filesystem path for the latest snapshot index.
    fn latest_index_path(&self) -> PathBuf {
        self.root.join("latest.index")
    }

    /// Reads one snapshot id from the latest index file.
    fn read_latest_index(&self, session_id: Option<&str>) -> Result<Option<String>> {
        let Some(index) = self.read_latest_index_state()? else {
            return Ok(None);
        };
        Ok(match session_id {
            Some(session_id) => index.latest_by_session.get(session_id).cloned(),
            None => index.latest_all,
        })
    }

    /// Reads and validates the complete latest-index state.
    fn read_latest_index_state(&self) -> Result<Option<LatestSnapshotIndex>> {
        let data = match fs::read_to_string(self.latest_index_path()) {
            Ok(data) => data,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        Self::decode_latest_index(&data).map(Some)
    }

    /// Reads and validates the complete latest-index state asynchronously.
    async fn read_latest_index_state_async(&self) -> Result<Option<LatestSnapshotIndex>> {
        let data = match tokio::fs::read_to_string(self.latest_index_path()).await {
            Ok(data) => data,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        Self::decode_latest_index(&data).map(Some)
    }

    /// Decodes one strict latest-index file.
    fn decode_latest_index(data: &str) -> Result<LatestSnapshotIndex> {
        let mut index = LatestSnapshotIndex::default();
        for line in data.lines() {
            if let Some(snapshot_id) = line.strip_prefix("all\t") {
                validate_snapshot_id(snapshot_id)?;
                if index.latest_all.replace(snapshot_id.to_string()).is_some() {
                    return Err(MezError::invalid_state(
                        "snapshot latest index contains duplicate global entries",
                    ));
                }
                continue;
            }
            let Some(rest) = line.strip_prefix("session\t") else {
                return Err(MezError::invalid_state(
                    "snapshot latest index contains an invalid entry",
                ));
            };
            let Some((session_id, snapshot_id)) = rest.split_once('\t') else {
                return Err(MezError::invalid_state(
                    "snapshot latest index contains a malformed session entry",
                ));
            };
            if session_id.is_empty() {
                return Err(MezError::invalid_state(
                    "snapshot latest index contains an empty session id",
                ));
            }
            validate_snapshot_id(snapshot_id)?;
            if index
                .latest_by_session
                .insert(session_id.to_string(), snapshot_id.to_string())
                .is_some()
            {
                return Err(MezError::invalid_state(
                    "snapshot latest index contains duplicate session entries",
                ));
            }
        }
        if index.latest_all.is_none() {
            return Err(MezError::invalid_state(
                "snapshot latest index has no global entry",
            ));
        }
        Ok(index)
    }

    /// Encodes one deterministic global and per-session latest index.
    fn encode_latest_index(index: &LatestSnapshotIndex) -> Result<String> {
        let latest_all = index
            .latest_all
            .as_deref()
            .ok_or_else(|| MezError::invalid_state("snapshot latest index has no global entry"))?;
        validate_snapshot_id(latest_all)?;
        let mut output = format!("all\t{latest_all}\n");
        for (session_id, snapshot_id) in &index.latest_by_session {
            if session_id.is_empty() || has_manifest_control_character(session_id) {
                return Err(MezError::invalid_state(
                    "snapshot latest index contains an invalid session id",
                ));
            }
            validate_snapshot_id(snapshot_id)?;
            output.push_str("session\t");
            output.push_str(session_id);
            output.push('\t');
            output.push_str(snapshot_id);
            output.push('\n');
        }
        Ok(output)
    }

    /// Atomically replaces the latest index with private durable contents.
    fn write_latest_index_state(&self, index: &LatestSnapshotIndex) -> Result<()> {
        let path = self.latest_index_path();
        let output = Self::encode_latest_index(index)?;
        write_private_replace_atomic(&path, output.as_bytes())
    }

    /// Async counterpart to [`Self::write_latest_index_state`].
    async fn write_latest_index_state_async(&self, index: &LatestSnapshotIndex) -> Result<()> {
        let path = self.latest_index_path();
        let output = Self::encode_latest_index(index)?;
        write_private_replace_atomic_async(&path, output.as_bytes()).await
    }

    /// Writes the latest index file for global and per-session lookups.
    fn write_latest_index_file(&self, snapshots: &[SnapshotState]) -> Result<()> {
        let path = self.latest_index_path();
        if snapshots.is_empty() {
            if path.exists() {
                fs::remove_file(path)?;
                sync_directory(&self.root)?;
            }
            return Ok(());
        }

        let mut latest_all: Option<&SnapshotState> = None;
        let mut latest_by_session: BTreeMap<&str, &SnapshotState> = BTreeMap::new();
        for snapshot in snapshots {
            if latest_all.is_none_or(|latest| {
                Self::compare_latest_snapshots(latest, snapshot) == Ordering::Less
            }) {
                latest_all = Some(snapshot);
            }
            let entry = latest_by_session
                .entry(snapshot.session_id.as_str())
                .or_insert(snapshot);
            if Self::compare_latest_snapshots(entry, snapshot) == Ordering::Less {
                *entry = snapshot;
            }
        }
        let index = LatestSnapshotIndex {
            latest_all: latest_all.map(|snapshot| snapshot.id.clone()),
            latest_by_session: latest_by_session
                .into_iter()
                .map(|(session_id, snapshot)| (session_id.to_string(), snapshot.id.clone()))
                .collect(),
        };
        self.write_latest_index_state(&index)
    }

    /// Async latest-index writer used by recovery and deletion paths.
    async fn write_latest_index_file_async(&self, snapshots: &[SnapshotState]) -> Result<()> {
        if snapshots.is_empty() {
            match tokio::fs::remove_file(self.latest_index_path()).await {
                Ok(()) => sync_directory_async(&self.root).await?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            return Ok(());
        }
        let mut latest_all: Option<&SnapshotState> = None;
        let mut latest_by_session: BTreeMap<&str, &SnapshotState> = BTreeMap::new();
        for snapshot in snapshots {
            if latest_all.is_none_or(|latest| {
                Self::compare_latest_snapshots(latest, snapshot) == Ordering::Less
            }) {
                latest_all = Some(snapshot);
            }
            let entry = latest_by_session
                .entry(snapshot.session_id.as_str())
                .or_insert(snapshot);
            if Self::compare_latest_snapshots(entry, snapshot) == Ordering::Less {
                *entry = snapshot;
            }
        }
        let index = LatestSnapshotIndex {
            latest_all: latest_all.map(|snapshot| snapshot.id.clone()),
            latest_by_session: latest_by_session
                .into_iter()
                .map(|(session_id, snapshot)| (session_id.to_string(), snapshot.id.clone()))
                .collect(),
        };
        self.write_latest_index_state_async(&index).await
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
