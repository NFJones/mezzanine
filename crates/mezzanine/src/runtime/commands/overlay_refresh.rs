//! Deferred record-browser refresh lane.
//!
//! An overlay refresh re-reads a store before the page can be shown. Reading it
//! inside the actor request parks every other client message behind a bounded
//! store wait, so a refresh instead claims a per-key generation, queues one
//! dispatch, and lets a worker rebuild the page off the actor. The completion
//! installs the rebuilt page only while the overlay still shows the same source
//! and no newer claim has been made, which keeps overlay state, selection, and
//! rendering unchanged apart from arrival timing.

use super::RuntimeSessionService;
use crate::error::{MezErrorKind, Result};
use crate::runtime::service_state::RuntimeRecordBrowserOverlaySource;
use crate::runtime::{
    RuntimeRecordBrowserRefreshDispatch, RuntimeRecordBrowserRefreshOutcome,
    RuntimeRecordBrowserRefreshWork,
};

/// Refresh key for the shared saved-session picker overlay.
///
/// The picker is one primary-overlay page whatever rows changed, so title changes
/// from different conversations claim the same key and coalesce into one rebuild;
/// pane-keyed refreshes for pane-scoped overlays arrive with their own slices.
pub(crate) const SAVED_SESSION_OVERLAY_REFRESH_KEY: &str = "saved-sessions";

impl RuntimeSessionService {
    /// Claims one overlay refresh and queues its worker dispatch.
    ///
    /// Returns the claim generation the caller stamps on the dispatch side
    /// effect, or `None` when no saved-session browser is open, because only
    /// that family reads a store tall enough to park the actor.
    pub(crate) fn begin_record_browser_refresh_claim(&mut self, refresh_key: &str) -> Option<u64> {
        self.active_saved_session_browser_source()?;
        let generation = self.presentation.begin_record_browser_refresh(refresh_key);
        self.presentation.push_pending_record_browser_refresh(
            RuntimeRecordBrowserRefreshDispatch {
                refresh_key: refresh_key.to_string(),
                generation,
            },
        );
        Some(generation)
    }

    /// Builds the owned work one queued refresh dispatch needs.
    ///
    /// Returns `None` when the claim is stale or the overlay no longer shows a
    /// saved-session browser, so a superseded dispatch costs nothing.
    pub(crate) fn claim_record_browser_refresh(
        &self,
        refresh_key: &str,
        generation: u64,
    ) -> Result<Option<RuntimeRecordBrowserRefreshWork>> {
        if self
            .presentation
            .record_browser_refresh_generation(refresh_key)
            != generation
        {
            return Ok(None);
        }
        let Some(source) = self.active_saved_session_browser_source() else {
            return Ok(None);
        };
        Ok(Some(RuntimeRecordBrowserRefreshWork {
            refresh_key: refresh_key.to_string(),
            generation,
            source,
            active_record_id: self.active_saved_session_browser_record_id(),
            transcript_store: self.persistence.transcript_store().cloned(),
            prompt_width: self.saved_session_prompt_width(),
            title_policy: self.agent_session_title_policy(),
        }))
    }

    /// Rebuilds one saved-session page off actor ownership.
    ///
    /// Deliberately static: the worker must not reach live service state, so
    /// everything it may read arrives in `work`.
    pub(crate) fn execute_record_browser_refresh(
        work: &RuntimeRecordBrowserRefreshWork,
    ) -> RuntimeRecordBrowserRefreshOutcome {
        let Some(store) = work.transcript_store.as_ref() else {
            return RuntimeRecordBrowserRefreshOutcome::Failed {
                message: "record browser refresh requires transcript storage".to_string(),
                kind: MezErrorKind::InvalidState,
            };
        };
        let RuntimeRecordBrowserOverlaySource::SavedSessions {
            directory,
            lifecycle,
            include_subagents,
            search,
            anchor,
            limit,
            ..
        } = &work.source
        else {
            return RuntimeRecordBrowserRefreshOutcome::Failed {
                message: "record browser refresh requires a saved-session source".to_string(),
                kind: MezErrorKind::InvalidState,
            };
        };
        match super::resume::runtime_agent_saved_sessions_browser(
            store,
            directory.as_deref(),
            *lifecycle,
            *include_subagents,
            search.as_deref(),
            anchor.clone(),
            *limit,
            work.prompt_width,
            work.title_policy,
        ) {
            Ok(mut browser) => {
                if let Some(active_record_id) = work.active_record_id.as_deref() {
                    browser.set_active_record_id(active_record_id);
                }
                RuntimeRecordBrowserRefreshOutcome::Rebuilt {
                    browser: Box::new(browser),
                }
            }
            Err(error) => RuntimeRecordBrowserRefreshOutcome::Failed {
                message: error.message().to_string(),
                kind: error.kind(),
            },
        }
    }

    /// Queues one refresh dispatch without an open overlay for tests.
    ///
    /// A test that asserts the actor emits what a real claim queues uses this
    /// instead of standing up the picker.
    #[cfg(test)]
    pub(crate) fn queue_record_browser_refresh_for_tests(&mut self, refresh_key: &str) {
        let generation = self.presentation.begin_record_browser_refresh(refresh_key);
        self.presentation.push_pending_record_browser_refresh(
            RuntimeRecordBrowserRefreshDispatch {
                refresh_key: refresh_key.to_string(),
                generation,
            },
        );
    }

    /// Runs one queued refresh through claim, execute, and complete for tests.
    ///
    /// The actor's real path emits a side effect a worker consumes; a test that
    /// asserts the rebuilt page drives the same three steps in order instead of
    /// standing up the runtime.
    #[cfg(test)]
    pub(crate) fn run_pending_record_browser_refresh_for_tests(&mut self) -> Result<bool> {
        let Some(dispatch) = self
            .take_pending_record_browser_refreshes()
            .into_iter()
            .next()
        else {
            return Ok(false);
        };
        let Some(work) =
            self.claim_record_browser_refresh(&dispatch.refresh_key, dispatch.generation)?
        else {
            return Ok(false);
        };
        let outcome = Self::execute_record_browser_refresh(&work);
        self.complete_record_browser_refresh(&work, outcome)
    }

    /// Installs one settled refresh while its overlay is still current.
    ///
    /// Returns whether a rebuilt page was installed. A dropped page is the
    /// documented outcome for a superseded claim: the overlay moved on while the
    /// worker was reading, so the newer claim owns the page.
    pub(crate) fn complete_record_browser_refresh(
        &mut self,
        work: &RuntimeRecordBrowserRefreshWork,
        outcome: RuntimeRecordBrowserRefreshOutcome,
    ) -> Result<bool> {
        match outcome {
            RuntimeRecordBrowserRefreshOutcome::Failed { message, kind } => {
                if self
                    .presentation
                    .record_browser_refresh_generation(&work.refresh_key)
                    != work.generation
                {
                    return Ok(false);
                }
                if self.active_saved_session_browser_source().as_ref() != Some(&work.source) {
                    return Ok(false);
                }
                Ok(self.set_active_saved_session_browser_error(&format!(
                    "overlay refresh failed: {message} ({kind:?})"
                )))
            }
            RuntimeRecordBrowserRefreshOutcome::Rebuilt { browser } => {
                if self
                    .presentation
                    .record_browser_refresh_generation(&work.refresh_key)
                    != work.generation
                {
                    return Ok(false);
                }
                if self.active_saved_session_browser_source().as_ref() != Some(&work.source) {
                    return Ok(false);
                }
                Ok(self.replace_active_saved_session_browser(work.source.clone(), *browser))
            }
        }
    }
}
