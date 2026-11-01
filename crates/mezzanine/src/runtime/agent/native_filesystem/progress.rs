//! Display-only confirmation ingress from the actual registered producer.
//!
//! Original receipt allocations survive notification loss/cancellation; current
//! canonical group/conversation and existing presentation lineage gate paint.
//! Neither progress nor its acknowledgements authorize a write or continuation.

use super::*;
use crate::runtime::processes::native_filesystem_producer::NativeFilesystemProgress;

impl RuntimeSessionService {
    /// Resolves only registered actual confirmation data, not matching labels.
    pub(crate) fn native_filesystem_progress_is_registered(
        &self,
        progress: &mez_agent::ActionPresentationProgress,
    ) -> bool {
        self.agent
            .native_filesystem_actions
            .progress_is_registered(progress)
    }

    /// Applies a cumulative original receipt prefix through existing confirmed
    /// progress composition/promotion. Missing/stale owners remain unpainted;
    /// errors are optional display failures, never failures of committed writes.
    pub(crate) fn apply_native_filesystem_progress(
        &mut self,
        delivery: &NativeFilesystemProgress,
    ) -> Result<bool> {
        let producer = delivery.producer();
        let Some(original) = self
            .agent
            .native_filesystem_actions
            .producer(producer.attempt())
        else {
            return Ok(false);
        };
        if !original.same(producer) {
            return Ok(false);
        }
        if !self.filesystem_progress_paint_current(producer.attempt())? {
            return Ok(false);
        }
        let confirmed = delivery.confirmed()?;
        if confirmed.is_empty() || !self.filesystem_progress_header_ready(producer.attempt())? {
            return Ok(false);
        }
        let mut changed = false;
        for progress in confirmed {
            changed |= self.apply_action_presentation_progress(progress.clone())?;
            changed |= self.promote_confirmed_action_presentation_progress(&progress)?;
        }
        Ok(changed)
    }
}
