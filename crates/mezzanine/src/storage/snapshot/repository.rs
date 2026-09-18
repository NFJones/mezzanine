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
    write_private_new_atomic_async,
};
use super::types::{
    SessionSnapshotPayload, SnapshotCreationContext, SnapshotKind, SnapshotManifest,
    SnapshotRepository, SnapshotState,
};
#[cfg(test)]
use super::types::{SnapshotConfigLayerMetadata, SnapshotFrameState, SnapshotPaneCapture};

/// Persisted global and per-session latest-snapshot identities.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct LatestSnapshotIndex {
    /// Snapshot selected across every session, when one exists.
    pub(super) latest_all: Option<String>,
    /// Snapshot selected for each session id.
    pub(super) latest_by_session: BTreeMap<String, String>,
}

/// Runs one blocking snapshot-index store call off the async runtime.
///
/// The index lives in SQLite with the shared busy timeout, so an async caller
/// hands the call to the blocking pool instead of parking a runtime worker.
async fn spawn_blocking_snapshot_index<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| MezError::invalid_state(format!("snapshot index task failed: {error}")))?
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
        super::sqlite::insert_metadata_row(&self.root, &manifest.state)?;
        self.write_latest_indexes(&manifest.state)?;
        Ok(path)
    }

    /// Writes a snapshot manifest through Tokio filesystem APIs.
    pub async fn write_async(&self, manifest: &SnapshotManifest) -> Result<PathBuf> {
        let path = manifest.write_to_dir_async(&self.root).await?;
        let root = self.root.clone();
        let state = manifest.state.clone();
        spawn_blocking_snapshot_index(move || super::sqlite::insert_metadata_row(&root, &state))
            .await?;
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

    /// Lists every published snapshot from the metadata index.
    ///
    /// The index answers the call without opening a manifest. It is a derived
    /// cache, so the listing rebuilds it from the manifests whenever the number
    /// of indexed rows and the number of manifest files disagree: a first run
    /// after this migration, a deleted database, or an interrupted write or
    /// delete heals on the next listing instead of hiding or inventing a
    /// snapshot.
    pub fn list(&self) -> Result<Vec<SnapshotState>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        if let Some(states) = super::sqlite::read_metadata(&self.root)?
            && Self::metadata_covers_manifests(&self.root, states.len())?
        {
            return Ok(states);
        }
        self.rebuild_metadata()
    }

    /// Lists snapshots through the metadata index with Tokio filesystem APIs.
    pub async fn list_async(&self) -> Result<Vec<SnapshotState>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let root = self.root.clone();
        let indexed =
            spawn_blocking_snapshot_index(move || super::sqlite::read_metadata(&root)).await?;
        if let Some(states) = indexed
            && Self::metadata_covers_manifests_async(&self.root, states.len()).await?
        {
            return Ok(states);
        }
        self.rebuild_metadata_async().await
    }

    /// Renders the stored latest winners for `mez storage export snapshots`.
    ///
    /// The body keeps the retired `latest.index` shape so the winners stay
    /// inspectable and diffable after the file disappeared. The read is
    /// read-only, so an inspection command never creates, migrates, or writes
    /// the store, and it reports `None` when no database exists yet.
    pub fn export_tsv_read_only(&self) -> Result<Option<String>> {
        super::sqlite::export_tsv_read_only(&self.root)
    }

    /// Reports whether the metadata index covers every manifest on disk.
    ///
    /// The check reads directory entries only: it never opens a manifest, so a
    /// listing stays a metadata query while still noticing a manifest that a
    /// crashed writer published without its row, or a row whose manifest is
    /// already gone. Both cases rebuild the index from the manifests.
    fn metadata_covers_manifests(root: &Path, indexed_rows: usize) -> Result<bool> {
        let mut manifests = 0;
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            if entry
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                == Some("manifest")
            {
                manifests += 1;
            }
        }
        Ok(manifests == indexed_rows)
    }

    /// Async counterpart to [`Self::metadata_covers_manifests`].
    async fn metadata_covers_manifests_async(root: &Path, indexed_rows: usize) -> Result<bool> {
        let mut entries = tokio::fs::read_dir(root).await?;
        let mut manifests = 0;
        while let Some(entry) = entries.next_entry().await? {
            if entry
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                == Some("manifest")
            {
                manifests += 1;
            }
        }
        Ok(manifests == indexed_rows)
    }

    /// Rebuilds the metadata index from every manifest and returns the listing.
    ///
    /// This is the migration and recovery path: it parses each manifest with the
    /// existing decoder, and replaces the indexed rows in one transaction so a
    /// reader sees either the previous index or the complete new one.
    fn rebuild_metadata(&self) -> Result<Vec<SnapshotState>> {
        let states = self.list_manifests()?;
        super::sqlite::write_metadata(&self.root, &states)?;
        Ok(states)
    }

    /// Async counterpart to [`Self::rebuild_metadata`].
    async fn rebuild_metadata_async(&self) -> Result<Vec<SnapshotState>> {
        let states = self.list_manifests_async().await?;
        let root = self.root.clone();
        let rows = states.clone();
        spawn_blocking_snapshot_index(move || super::sqlite::write_metadata(&root, &rows)).await?;
        Ok(states)
    }

    /// Scans every manifest; the rebuild path for the derived index.
    fn list_manifests(&self) -> Result<Vec<SnapshotState>> {
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

    /// Scans every manifest through Tokio filesystem APIs.
    async fn list_manifests_async(&self) -> Result<Vec<SnapshotState>> {
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
        super::sqlite::remove_metadata_row(&self.root, snapshot_id)?;
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
                let root = self.root.clone();
                let orphan = snapshot_id.to_string();
                spawn_blocking_snapshot_index(move || {
                    super::sqlite::remove_metadata_row(&root, &orphan)
                })
                .await?;
                self.rebuild_latest_indexes_async().await?;
                return Ok(removed);
            }
            Err(error) => return Err(error),
        };
        self.remove_payload_if_local_async(&manifest).await?;
        tokio::fs::remove_file(&path).await?;
        let root = self.root.clone();
        let deleted = snapshot_id.to_string();
        spawn_blocking_snapshot_index(move || super::sqlite::remove_metadata_row(&root, &deleted))
            .await?;
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
    /// The helper keeps latest lookups on the small indexed winners instead of
    /// requiring callers to enumerate every manifest on hot paths.
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
    /// Repository writes and deletes call this so lookup paths read the stored
    /// winners instead of enumerating every manifest.
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

    /// Reads one indexed winner's recorded state without parsing its manifest.
    ///
    /// The latest-index update only compares ordering fields, so the metadata
    /// row answers it. The manifest existence check keeps the previous repair
    /// behavior: a winner whose manifest has already disappeared reports an
    /// error, and the caller rebuilds the index from the manifests instead of
    /// keeping a phantom winner.
    fn indexed_winner_state(&self, snapshot_id: &str) -> Result<SnapshotState> {
        if !self.manifest_path(snapshot_id)?.exists() {
            return Err(MezError::invalid_state(format!(
                "snapshot {snapshot_id} manifest is missing"
            )));
        }
        super::sqlite::read_metadata_row(&self.root, snapshot_id)?.ok_or_else(|| {
            MezError::invalid_state(format!("snapshot {snapshot_id} has no metadata row"))
        })
    }

    /// Async counterpart to [`Self::indexed_winner_state`].
    async fn indexed_winner_state_async(&self, snapshot_id: &str) -> Result<SnapshotState> {
        if !self.manifest_path(snapshot_id)?.exists() {
            return Err(MezError::invalid_state(format!(
                "snapshot {snapshot_id} manifest is missing"
            )));
        }
        let root = self.root.clone();
        let indexed_id = snapshot_id.to_string();
        let state = spawn_blocking_snapshot_index(move || {
            super::sqlite::read_metadata_row(&root, &indexed_id)
        })
        .await?;
        state.ok_or_else(|| {
            MezError::invalid_state(format!("snapshot {snapshot_id} has no metadata row"))
        })
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
        let latest_all = self.indexed_winner_state(&latest_all_id)?;
        if Self::compare_latest_snapshots(&latest_all, state) == Ordering::Less {
            index.latest_all = Some(state.id.clone());
        }

        if let Some(latest_session_id) = index.latest_by_session.get(&state.session_id).cloned() {
            let latest_session = self.indexed_winner_state(&latest_session_id)?;
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
        let latest_all = self.indexed_winner_state_async(&latest_all_id).await?;
        if Self::compare_latest_snapshots(&latest_all, state) == Ordering::Less {
            index.latest_all = Some(state.id.clone());
        }

        if let Some(latest_session_id) = index.latest_by_session.get(&state.session_id).cloned() {
            let latest_session = self.indexed_winner_state_async(&latest_session_id).await?;
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

    /// Reads one snapshot id from the stored latest index.
    fn read_latest_index(&self, session_id: Option<&str>) -> Result<Option<String>> {
        let Some(index) = self.read_latest_index_state()? else {
            return Ok(None);
        };
        Ok(match session_id {
            Some(session_id) => index.latest_by_session.get(session_id).cloned(),
            None => index.latest_all,
        })
    }

    /// Reads the complete latest-index state, rebuilding an empty index once.
    ///
    /// An empty table means a fresh install, a deleted database, or a store that
    /// predates this build: the winners are recomputed from the manifests and
    /// stored, which is the same ordering the previous file index produced.
    fn read_latest_index_state(&self) -> Result<Option<LatestSnapshotIndex>> {
        if let Some(index) = super::sqlite::read(&self.root)? {
            return Ok(Some(index));
        }
        let snapshots = self.list()?;
        if snapshots.is_empty() {
            return Ok(None);
        }
        self.write_latest_index_file(&snapshots)?;
        super::sqlite::read(&self.root)
    }

    /// Reads the complete latest-index state asynchronously, rebuilding once.
    async fn read_latest_index_state_async(&self) -> Result<Option<LatestSnapshotIndex>> {
        let root = self.root.clone();
        if let Some(index) =
            spawn_blocking_snapshot_index(move || super::sqlite::read(&root)).await?
        {
            return Ok(Some(index));
        }
        let snapshots = self.list_async().await?;
        if snapshots.is_empty() {
            return Ok(None);
        }
        self.write_latest_index_file_async(&snapshots).await?;
        let root = self.root.clone();
        spawn_blocking_snapshot_index(move || super::sqlite::read(&root)).await
    }

    /// Atomically replaces the stored latest index.
    fn write_latest_index_state(&self, index: &LatestSnapshotIndex) -> Result<()> {
        super::sqlite::write(&self.root, index)
    }

    /// Async counterpart to [`Self::write_latest_index_state`].
    async fn write_latest_index_state_async(&self, index: &LatestSnapshotIndex) -> Result<()> {
        let root = self.root.clone();
        let index = index.clone();
        spawn_blocking_snapshot_index(move || super::sqlite::write(&root, &index)).await
    }

    /// Stores the latest index for global and per-session lookups.
    fn write_latest_index_file(&self, snapshots: &[SnapshotState]) -> Result<()> {
        if snapshots.is_empty() {
            // No snapshots means no winners: storing an empty index is the state
            // the absent file used to represent, and a later read rebuilds from the
            // (empty) manifest list without looping.
            return self.write_latest_index_state(&LatestSnapshotIndex::default());
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
            return self
                .write_latest_index_state_async(&LatestSnapshotIndex::default())
                .await;
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
