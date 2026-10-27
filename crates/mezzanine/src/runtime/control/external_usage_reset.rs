//! Captured, worker-committed external pane-view resets.
//!
//! Preparation does no I/O or baseline mutation. The existing command worker
//! persists exact captured checkpoints and seals its actual result. Adoption
//! rechecks control/store/root and native counter ownership before changing a
//! pane view. Committed reset facts survive lost callbacks; stale completions
//! never reset a replacement owner or hide expense arriving after capture.

use crate::error::{MezError, MezErrorKind, Result};
use crate::runtime::{RuntimeSessionService, processes::RuntimePaneProcessIdentity};
use crate::storage::token_usage::{ExternalCounters, ExternalUsageCommit, TokenUsageStore};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock};

/// One projected stream's reset point and exact member topology at capture.
#[derive(Debug, Clone)]
struct ResetPoint {
    /// Existing projection identity, not a new accounting namespace.
    id: String,
    /// Absolute captured counters; later counters are not part of this reset.
    totals: ExternalCounters,
    /// Group member identities must not be silently reinterpreted at adoption.
    members: Option<BTreeSet<String>>,
}

/// Immutable worker input and sealed storage outcome; contains no credentials.
#[derive(Debug, Clone)]
pub(crate) struct ExternalUsageResetWork {
    /// Pane whose exact original root owns the projected view.
    pane: String,
    /// Exact captured root, never established from output or payload hints.
    process: RuntimePaneProcessIdentity,
    /// Full control replacement cannot adopt an old reset callback.
    incarnation: Arc<()>,
    /// Actual private destination captured at admission.
    store: TokenUsageStore,
    /// Existing durable member checkpoints, bounded before worker dispatch.
    checkpoints: Vec<ExternalUsageCommit>,
    /// Projection mapping needed for a no-partial-update preflight.
    points: Vec<ResetPoint>,
    /// Native usage may not disappear if it changed while storage ran.
    native: BTreeMap<mez_agent::ModelTokenUsageKey, mez_agent::ModelTokenUsage>,
    /// Native project attribution is fenced independently of aggregate totals.
    projects: Vec<mez_agent::ProjectTokenUsage>,
    /// First actual invocation is cached; supplied callback success cannot seal.
    result: Arc<OnceLock<std::result::Result<(), (MezErrorKind, String)>>>,
}

impl ExternalUsageResetWork {
    /// Performs bounded SQLite work outside the actor. Exact repeated execution
    /// observes the same result, never blindly replays an ambiguous mutation.
    pub(crate) fn execute(&self) -> Result<()> {
        match self.result.get_or_init(|| {
            self.store
                .reset_external_usage_view(&self.checkpoints)
                .map_err(|error| (error.kind(), error.message().to_owned()))
        }) {
            Ok(()) => Ok(()),
            Err((kind, message)) => Err(MezError::new(*kind, message)),
        }
    }
}

impl RuntimeSessionService {
    /// Whether reset needs worker-owned durability rather than an inline native
    /// counter clear. A missing store must refuse, not downgrade durable usage.
    pub(crate) fn external_usage_reset_requires_worker(&self, pane: &str) -> bool {
        self.control
            .external_usage()
            .streams
            .values()
            .any(|stream| stream.pane_id == pane)
    }

    /// Freezes current-root projections and native pane totals without I/O.
    /// At most 4096 durable members can be reset by one finite command claim.
    pub(crate) fn prepare_external_usage_reset(
        &self,
        pane: &str,
    ) -> Result<ExternalUsageResetWork> {
        let process = self
            .pane_process_identity(pane)
            .map_err(|_| MezError::conflict("external reset pane incarnation unavailable"))?;
        let store = self.persistence.cloned_token_usage_store().ok_or_else(|| {
            MezError::invalid_state("external reset accounting store unavailable")
        })?;
        let projection = self.control.external_usage();
        let mut checkpoints = BTreeMap::new();
        let mut points = Vec::new();
        for (id, stream) in &projection.streams {
            if stream.pane_id != pane || !stream.process.same_incarnation(&process) {
                continue;
            }
            let members = if let Some(members) = &stream.members {
                for (id, checkpoint) in members {
                    checkpoints.insert(id.clone(), checkpoint.clone());
                    if checkpoints.len() > 4096 {
                        return Err(MezError::invalid_state(
                            "external reset member capacity unavailable",
                        ));
                    }
                }
                Some(members.keys().cloned().collect())
            } else {
                checkpoints.insert(
                    id.clone(),
                    ExternalUsageCommit {
                        stream_id: id.clone(),
                        revision: stream.revision,
                        totals: stream.totals,
                        applied: false,
                    },
                );
                None
            };
            if checkpoints.len() > 4096 {
                return Err(MezError::invalid_state(
                    "external reset member capacity unavailable",
                ));
            }
            points.push(ResetPoint {
                id: id.clone(),
                totals: stream.totals,
                members,
            });
        }
        Ok(ExternalUsageResetWork {
            pane: pane.into(),
            process,
            incarnation: projection.incarnation.clone(),
            store,
            checkpoints: checkpoints.into_values().collect(),
            points,
            native: self.agent_token_usage_for_pane(pane),
            projects: self.project_usage_for_pane(pane),
            result: Arc::new(OnceLock::new()),
        })
    }

    /// Adopts only actual committed evidence and leaves all views unchanged on
    /// failed preflight. Late expense remains above the captured reset baseline.
    pub(crate) fn complete_external_usage_reset(
        &mut self,
        work: &ExternalUsageResetWork,
    ) -> Result<()> {
        if !matches!(work.result.get(), Some(Ok(()))) {
            return Err(MezError::conflict("external reset durability unpublished"));
        }
        if !Arc::ptr_eq(
            &self.control.external_usage().incarnation,
            &work.incarnation,
        ) || self.persistence.token_usage_store() != Some(&work.store)
            || !self.pane_process_identity_is_current(&work.pane, &work.process)
            || self.agent_token_usage_for_pane(&work.pane) != work.native
            || self.project_usage_for_pane(&work.pane) != work.projects
        {
            return Err(MezError::conflict(
                "external reset owner changed after commit",
            ));
        }
        for point in &work.points {
            let stream = self
                .control
                .external_usage()
                .streams
                .get(&point.id)
                .filter(|stream| {
                    stream.pane_id == work.pane && stream.process.same_incarnation(&work.process)
                })
                .ok_or_else(|| {
                    MezError::conflict("external reset projection changed after commit")
                })?;
            if stream
                .members
                .as_ref()
                .map(|members| members.keys().cloned().collect::<BTreeSet<_>>())
                != point.members
            {
                return Err(MezError::conflict(
                    "external reset members changed after commit",
                ));
            }
            stream.totals.difference(point.totals)?;
            if stream.reset_baseline.difference(point.totals).is_err() {
                point.totals.difference(stream.reset_baseline)?;
            }
        }
        for point in &work.points {
            if let Some(stream) = self.control.external_usage_mut().streams.get_mut(&point.id)
                && stream.reset_baseline.difference(point.totals).is_err()
            {
                stream.reset_baseline = point.totals;
            }
        }
        self.reset_native_token_usage_for_pane(&work.pane);
        Ok(())
    }
}
