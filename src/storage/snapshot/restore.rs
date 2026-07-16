//! Snapshot resume, rollback, and session-restore planning.
//!
//! Restore methods combine manifest metadata with payload inspection and delegate
//! actual session reconstruction to the session module.

use crate::error::{MezError, Result};
use crate::host::shell::ResolvedShell;
use mez_core::ids::{PaneId, SessionId, WindowGroupId, WindowId};
use mez_mux::layout::{LayoutPolicy, PaneGeometry, Size};
use mez_mux::session::{
    RestoredPane, RestoredSessionState, RestoredWindow, RestoredWindowGroup, Session,
    SessionRestoreInput,
};
use std::collections::BTreeMap;

use super::types::{
    LayoutLoadPlan, SessionSnapshotPayload, SnapshotManifest, SnapshotRepository,
    SnapshotRestoreResult, SnapshotRollbackPlan, SnapshotSessionState, SnapshotState,
};

/// Decodes a validated product snapshot into dependency-neutral session data.
pub(crate) fn session_restore_input(
    payload: &SessionSnapshotPayload,
) -> Result<SessionRestoreInput> {
    payload.validate()?;
    let session_id = SessionId::parse('$', payload.session_id.clone())
        .ok_or_else(|| MezError::invalid_args("snapshot payload contains an invalid session id"))?;
    let authoritative_size = Size::new(payload.authoritative_columns, payload.authoritative_rows)?;
    let mut window_ids = BTreeMap::new();
    let mut windows = Vec::with_capacity(payload.windows.len());

    for window_payload in &payload.windows {
        let id = WindowId::parse('@', window_payload.window_id.clone()).ok_or_else(|| {
            MezError::invalid_args("snapshot payload contains an invalid window id")
        })?;
        window_ids.insert(window_payload.window_id.clone(), id.clone());
        let panes = window_payload
            .panes
            .iter()
            .map(|pane_payload| {
                let id = PaneId::parse('%', pane_payload.pane_id.clone()).ok_or_else(|| {
                    MezError::invalid_args("snapshot payload contains an invalid pane id")
                })?;
                Ok(RestoredPane {
                    id,
                    index: pane_payload.index,
                    title: pane_payload.title.clone(),
                    active: pane_payload.active,
                    size: Size::new(pane_payload.columns, pane_payload.rows)?,
                    geometry: pane_payload.geometry.as_ref().map(|geometry| PaneGeometry {
                        index: pane_payload.index,
                        column: geometry.column,
                        row: geometry.row,
                        columns: geometry.columns,
                        rows: geometry.rows,
                    }),
                    current_working_directory: pane_payload.current_working_directory.clone(),
                    readiness_state: pane_payload.readiness_state.clone(),
                    alternate_screen_active: pane_payload.alternate_screen_active,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        windows.push(RestoredWindow {
            id,
            index: window_payload.index,
            name: window_payload.name.clone(),
            active: Some(window_payload.window_id.as_str()) == payload.active_window_id.as_deref()
                || window_payload.active,
            size: Size::new(window_payload.columns, window_payload.rows)?,
            layout_policy: LayoutPolicy::from_name(&window_payload.layout_policy).ok_or_else(
                || MezError::invalid_args("snapshot window layout policy is invalid"),
            )?,
            layout_root: window_payload
                .layout_root
                .as_ref()
                .map(|root| root.to_layout_node(&window_payload.panes))
                .transpose()?,
            panes,
        });
    }

    let active_window_id = payload
        .active_window_id
        .as_ref()
        .map(|id| {
            window_ids.get(id).cloned().ok_or_else(|| {
                MezError::invalid_args("snapshot active_window_id references an unknown window")
            })
        })
        .transpose()?;
    let window_groups = if payload.window_groups.is_empty() {
        let active_window_id = active_window_id
            .clone()
            .or_else(|| {
                windows
                    .iter()
                    .find(|window| window.active)
                    .map(|window| window.id.clone())
            })
            .ok_or_else(|| {
                MezError::invalid_args("snapshot payload must contain exactly one active window")
            })?;
        vec![RestoredWindowGroup {
            id: WindowGroupId::parse('g', "g1").ok_or_else(|| {
                MezError::invalid_args("snapshot restore could not allocate default group id")
            })?,
            index: 0,
            name: "0".to_string(),
            window_ids: windows.iter().map(|window| window.id.clone()).collect(),
            active_window_id: Some(active_window_id),
            last_active_window_id: None,
            active: true,
        }]
    } else {
        payload
            .window_groups
            .iter()
            .map(|group| {
                let resolve_window = |id: &String| {
                    window_ids.get(id).cloned().ok_or_else(|| {
                        MezError::invalid_args("snapshot window group references an unknown window")
                    })
                };
                Ok(RestoredWindowGroup {
                    id: WindowGroupId::parse('g', group.group_id.clone()).ok_or_else(|| {
                        MezError::invalid_args(
                            "snapshot payload contains an invalid window group id",
                        )
                    })?,
                    index: group.index,
                    name: group.name.clone(),
                    window_ids: group
                        .window_ids
                        .iter()
                        .map(resolve_window)
                        .collect::<Result<Vec<_>>>()?,
                    active_window_id: group
                        .active_window_id
                        .as_ref()
                        .map(resolve_window)
                        .transpose()?,
                    last_active_window_id: group
                        .last_active_window_id
                        .as_ref()
                        .map(resolve_window)
                        .transpose()?,
                    active: group.active,
                })
            })
            .collect::<Result<Vec<_>>>()?
    };

    Ok(SessionRestoreInput {
        session_id,
        name: payload.name.clone(),
        state: match payload.state {
            SnapshotSessionState::Running => RestoredSessionState::Running,
            SnapshotSessionState::Detached => RestoredSessionState::Detached,
            SnapshotSessionState::Empty => RestoredSessionState::Empty,
            SnapshotSessionState::Stopping => RestoredSessionState::Stopping,
            SnapshotSessionState::Failed => RestoredSessionState::Failed,
        },
        authoritative_size,
        active_window_id,
        windows,
        window_groups,
    })
}

impl SnapshotRepository {
    /// Runs the resume plan operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn resume_plan(&self, snapshot_id: &str) -> Result<LayoutLoadPlan> {
        let manifest = self.inspect(snapshot_id)?;
        Ok(LayoutLoadPlan {
            session_id: manifest.state.session_id,
            window_count: manifest.state.window_count,
            pane_count: manifest.state.pane_count,
            restart_required_panes: manifest.restart_required_panes,
            limitations: manifest.state.limitations,
        })
    }

    /// Runs the latest operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn latest(&self, session_id: Option<&str>) -> Result<Option<SnapshotState>> {
        if let Some(snapshot) = self.latest_from_index(session_id)? {
            return Ok(Some(snapshot));
        }
        Ok(self
            .list()?
            .into_iter()
            .filter(|snapshot| {
                session_id.is_none_or(|session_id| snapshot.session_id == session_id)
            })
            .max_by(Self::compare_latest_snapshots))
    }

    /// Runs the latest resume plan operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn latest_resume_plan(&self, session_id: Option<&str>) -> Result<LayoutLoadPlan> {
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
        let session = Session::from_restore_input(shell, session_restore_input(payload)?)?;
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
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "test-only adapter retained for focused boundary coverage"
    )]
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
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "test-only adapter retained for focused boundary coverage"
    )]
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
