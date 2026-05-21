//! Snapshot resume, rollback, and session-restore planning.
//!
//! Restore methods combine manifest metadata with payload inspection and delegate
//! actual session reconstruction to the session module.

use crate::error::{MezError, Result};
use crate::session::Session;
use crate::shell::ResolvedShell;

use super::types::{
    SessionSnapshotPayload, SnapshotManifest, SnapshotRepository, SnapshotRestoreResult,
    SnapshotResumePlan, SnapshotRollbackPlan, SnapshotState,
};

impl SnapshotRepository {
    /// Runs the resume plan operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn resume_plan(&self, snapshot_id: &str) -> Result<SnapshotResumePlan> {
        let payload = self.inspect_payload(snapshot_id)?;
        Ok(payload.resume_plan())
    }

    /// Runs the latest operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn latest(&self, session_id: Option<&str>) -> Result<Option<SnapshotState>> {
        let mut snapshots = self
            .list()?
            .into_iter()
            .filter(|snapshot| {
                session_id.is_none_or(|session_id| snapshot.session_id == session_id)
            })
            .collect::<Vec<_>>();
        snapshots.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(snapshots.pop())
    }

    /// Runs the latest resume plan operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn latest_resume_plan(&self, session_id: Option<&str>) -> Result<SnapshotResumePlan> {
        let latest = self.latest(session_id)?.ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "no matching snapshot found",
            )
        })?;
        self.resume_plan(&latest.id)
    }

    /// Runs the restore loaded session operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn restore_loaded_session(
        manifest: SnapshotManifest,
        payload: &SessionSnapshotPayload,
        shell: ResolvedShell,
    ) -> Result<SnapshotRestoreResult> {
        if !manifest.state.restorable {
            return Err(MezError::invalid_state(
                "snapshot manifest is marked non-restorable",
            ));
        }
        if manifest.state.session_id != payload.session_id {
            return Err(MezError::invalid_state(
                "snapshot manifest and payload refer to different sessions",
            ));
        }
        let resume_plan = payload.resume_plan();
        let session = Session::from_snapshot_payload(shell, payload)?;
        Ok(SnapshotRestoreResult {
            session,
            resume_plan,
        })
    }

    /// Runs the restore session operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn restore_session(
        &self,
        snapshot_id: &str,
        shell: ResolvedShell,
    ) -> Result<SnapshotRestoreResult> {
        let manifest = self.inspect(snapshot_id)?;
        let payload = self.inspect_payload(snapshot_id)?;
        Self::restore_loaded_session(manifest, &payload, shell)
    }

    /// Restores one loaded snapshot payload after synchronously inspecting its manifest.
    pub fn restore_session_from_payload(
        &self,
        snapshot_id: &str,
        payload: &SessionSnapshotPayload,
        shell: ResolvedShell,
    ) -> Result<SnapshotRestoreResult> {
        let manifest = self.inspect(snapshot_id)?;
        Self::restore_loaded_session(manifest, payload, shell)
    }

    /// Restores one loaded snapshot payload after asynchronously inspecting its manifest.
    pub async fn restore_session_from_payload_async(
        &self,
        snapshot_id: &str,
        payload: &SessionSnapshotPayload,
        shell: ResolvedShell,
    ) -> Result<SnapshotRestoreResult> {
        let manifest = self.inspect_async(snapshot_id).await?;
        Self::restore_loaded_session(manifest, payload, shell)
    }

    /// Restores one snapshot through Tokio filesystem APIs.
    pub async fn restore_session_async(
        &self,
        snapshot_id: &str,
        shell: ResolvedShell,
    ) -> Result<SnapshotRestoreResult> {
        let manifest = self.inspect_async(snapshot_id).await?;
        let payload = self.inspect_payload_async(snapshot_id).await?;
        Self::restore_loaded_session(manifest, &payload, shell)
    }

    /// Runs the restore latest session operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn restore_latest_session(
        &self,
        session_id: Option<&str>,
        shell: ResolvedShell,
    ) -> Result<SnapshotRestoreResult> {
        let latest = self.latest(session_id)?.ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "no matching snapshot found",
            )
        })?;
        self.restore_session(&latest.id, shell)
    }

    /// Runs the rollback plan operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn rollback_plan(&self, snapshot_id: &str) -> Result<SnapshotRollbackPlan> {
        let manifest = self.inspect(snapshot_id)?;
        let mut limitations = manifest.state.limitations.clone();
        if !manifest.state.restorable {
            limitations.push("snapshot manifest is marked non-restorable".to_string());
            return Ok(SnapshotRollbackPlan {
                snapshot_id: manifest.state.id,
                session_id: manifest.state.session_id,
                available: false,
                restore_command: None,
                restart_required_panes: Vec::new(),
                limitations,
            });
        }

        let payload = match self.inspect_payload(snapshot_id) {
            Ok(payload) => payload,
            Err(error) => {
                limitations.push(format!(
                    "snapshot payload is unavailable: {}",
                    error.message()
                ));
                return Ok(SnapshotRollbackPlan {
                    snapshot_id: manifest.state.id,
                    session_id: manifest.state.session_id,
                    available: false,
                    restore_command: None,
                    restart_required_panes: Vec::new(),
                    limitations,
                });
            }
        };
        let resume = payload.resume_plan();
        limitations.extend(resume.limitations);
        Ok(SnapshotRollbackPlan {
            snapshot_id: manifest.state.id,
            session_id: manifest.state.session_id,
            available: true,
            restore_command: Some(format!("mez snapshot resume {snapshot_id}")),
            restart_required_panes: resume.restart_required_panes,
            limitations,
        })
    }
}
