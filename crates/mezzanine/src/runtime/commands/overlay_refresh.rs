//! Deferred record-browser refresh lane.
//!
//! An overlay refresh re-reads a store before the page can be shown. Reading it
//! inside the actor request parks every other client message behind a bounded
//! store wait, so a refresh instead claims a per-key generation, queues one
//! dispatch, and lets a worker rebuild the page off the actor. The completion
//! installs the rebuilt page only while the overlay still shows the same source
//! and no newer claim has been made, which keeps overlay state, selection, and
//! rendering unchanged apart from arrival timing. Each claim carries an intent:
//! a current-page rebuild restores the focused row, while an adjacent-page fetch
//! resolves the cursor record the operator stepped off into a keyset anchor, and
//! a filter change (scope, subagent visibility, lifecycle, search text) rebuilds
//! the page the operator will see. A key owns at most one pending claim, so a
//! claim that lands while a page-edge fetch is still queued replaces that step
//! instead of applying it twice.
//!
//! A filter key derives its target from the page on screen, so every
//! source-derived action (archive/restore, detail) agrees with the rows the
//! operator sees. The target stays pending while its page is fetched, and a
//! second filter key composes with it instead of dropping the earlier filter. The
//! completion installs that page only while the overlay is still the one the
//! claim observed: a newer claim, a source the operator changed, or a record
//! opened into detail all leave the deferred page uninstalled.
//!
//! A settled claim clears its pending filter target, so a filter whose rebuild
//! failed or was dropped is not replayed later: the operator presses the key
//! again once the list is back.
//!
//! A rebuild never closes a record detail, so a toggle or filter key pressed from
//! inside one is discarded rather than applied - the same re-press rule as a
//! dropped filter - and a delete is the one intent that owns its page even in
//! detail, because the row that detail showed is gone. A filter key also keeps
//! the row index the operator was on when the new filters no longer match the
//! focused record, which matches the raw-index refresh sites (the lifecycle
//! toggle, the filter prompt, and the deletes); the inline scope toggle used to
//! reset to the first row instead.

use super::RuntimeSessionService;
use crate::error::{MezError, MezErrorKind, Result};
use crate::runtime::service_state::RuntimeRecordBrowserOverlaySource;
use crate::runtime::{
    RuntimeRecordBrowserRefreshDispatch, RuntimeRecordBrowserRefreshIntent,
    RuntimeRecordBrowserRefreshOutcome, RuntimeRecordBrowserRefreshWork,
};
use crate::storage::transcript::SavedSessionPageAnchor;

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
        Some(self.begin_record_browser_refresh_claim_for_intent(
            refresh_key,
            RuntimeRecordBrowserRefreshIntent::RefreshInPlace {
                active_record_id: self.active_saved_session_browser_record_id(),
            },
        ))
    }

    /// Claims one adjacent saved-session page and queues its worker dispatch.
    ///
    /// The page-edge check and the page identity both read retained overlay
    /// state, so the keypress path never waits on the store: the worker resolves
    /// the edge cursor record and rebuilds the page. Returns `Ok(None)` when the
    /// overlay is not a saved-session page or the cursor stayed inside its page,
    /// which leaves cursor movement to the in-page handler.
    pub(crate) fn begin_record_browser_adjacent_page_claim(
        &mut self,
        delta: isize,
    ) -> Result<Option<u64>> {
        let Some((active_index, record_count, first_id, last_id)) =
            self.active_saved_session_browser_page_edges()
        else {
            return Ok(None);
        };
        let crosses_edge = if delta.is_positive() {
            active_index.saturating_add(delta.unsigned_abs()) >= record_count
        } else if delta.is_negative() {
            delta.unsigned_abs() > active_index
        } else {
            false
        };
        if !crosses_edge {
            return Ok(None);
        }
        self.persistence
            .transcript_store()
            .ok_or_else(|| MezError::invalid_state("resume requires transcript storage"))?;
        Ok(Some(self.begin_record_browser_refresh_claim_for_intent(
            SAVED_SESSION_OVERLAY_REFRESH_KEY,
            RuntimeRecordBrowserRefreshIntent::FetchAdjacent {
                delta,
                first_id,
                last_id,
            },
        )))
    }

    /// Claims one refresh generation for an intent and queues its dispatch.
    fn begin_record_browser_refresh_claim_for_intent(
        &mut self,
        refresh_key: &str,
        intent: RuntimeRecordBrowserRefreshIntent,
    ) -> u64 {
        let generation = self.presentation.begin_record_browser_refresh(refresh_key);
        self.presentation.push_pending_record_browser_refresh(
            RuntimeRecordBrowserRefreshDispatch {
                refresh_key: refresh_key.to_string(),
                generation,
            },
            intent,
        );
        generation
    }

    /// Claims one preserving page rebuild for a source the overlay does not show
    /// yet: an in-memory filter change (scope, subagent visibility, lifecycle,
    /// search text) or a refresh after a store mutation.
    ///
    /// The caller supplies the source the rebuild targets, the row the operator
    /// had focused, and the settlement error the rebuilt page displays when one
    /// applies. The claim keeps the source the overlay shows now as its staleness
    /// token, so a rebuild is dropped instead of installing over a page the
    /// operator moved on to. Returns `Ok(None)` when no saved-session browser is
    /// open, which leaves the caller's store mutation to stand alone.
    pub(crate) fn begin_record_browser_preserving_claim(
        &mut self,
        target: RuntimeRecordBrowserOverlaySource,
        active_record_id: Option<String>,
        error: Option<String>,
    ) -> Result<Option<u64>> {
        if self.active_saved_session_browser_source().is_none() {
            return Ok(None);
        }
        self.persistence
            .transcript_store()
            .ok_or_else(|| MezError::invalid_state("resume requires transcript storage"))?;
        self.presentation
            .set_record_browser_pending_target(SAVED_SESSION_OVERLAY_REFRESH_KEY, target.clone());
        Ok(Some(self.begin_record_browser_refresh_claim_for_intent(
            SAVED_SESSION_OVERLAY_REFRESH_KEY,
            RuntimeRecordBrowserRefreshIntent::ApplyFilter {
                target: Box::new(target),
                active_record_id,
                active_index: None,
                error,
            },
        )))
    }

    /// Claims one page rebuild after a delete, whatever store-backed browser the
    /// delete happened in.
    ///
    /// The caller has already removed the row, so the claim rebuilds the page the
    /// delete left and keeps the row index the operator was on: the deleted row
    /// held the focus, and the rebuilt page has no id left to restore.
    pub(crate) fn begin_record_browser_delete_claim(
        &mut self,
        active_index: usize,
    ) -> Result<Option<u64>> {
        let Some(source) = self.active_record_browser_source() else {
            return Ok(None);
        };
        if !matches!(
            source,
            RuntimeRecordBrowserOverlaySource::SavedSessions { .. }
                | RuntimeRecordBrowserOverlaySource::Issues { .. }
                | RuntimeRecordBrowserOverlaySource::Memories { .. }
                | RuntimeRecordBrowserOverlaySource::Context { .. }
        ) {
            return Ok(None);
        }
        let Some(refresh_key) = self.active_record_browser_refresh_key() else {
            return Ok(None);
        };
        if matches!(
            source,
            RuntimeRecordBrowserOverlaySource::SavedSessions { .. }
        ) && self.persistence.transcript_store().is_none()
        {
            return Err(MezError::invalid_state(
                "resume requires transcript storage",
            ));
        }
        Ok(Some(self.begin_record_browser_refresh_claim_for_intent(
            &refresh_key,
            RuntimeRecordBrowserRefreshIntent::RefreshAfterDelete { active_index },
        )))
    }

    /// Claims one page rebuild for a pane-scoped store-backed browser.
    ///
    /// The saved-session picker keeps its own entries because it also composes
    /// pending filter targets; the other store-backed families refresh from the
    /// source their key derived, with the same staleness and install rules.
    pub(crate) fn begin_record_browser_pane_claim(
        &mut self,
        target: RuntimeRecordBrowserOverlaySource,
        active_record_id: Option<String>,
        active_index: Option<usize>,
    ) -> Result<Option<u64>> {
        let Some(refresh_key) = self.active_record_browser_refresh_key() else {
            return Ok(None);
        };
        if !matches!(
            target,
            RuntimeRecordBrowserOverlaySource::Issues { .. }
                | RuntimeRecordBrowserOverlaySource::Memories { .. }
                | RuntimeRecordBrowserOverlaySource::Context { .. }
        ) {
            return Ok(None);
        }
        Ok(Some(self.begin_record_browser_refresh_claim_for_intent(
            &refresh_key,
            RuntimeRecordBrowserRefreshIntent::ApplyFilter {
                target: Box::new(target),
                active_record_id,
                active_index,
                error: None,
            },
        )))
    }

    /// Builds the owned work one queued refresh dispatch needs.
    ///
    /// Returns `None` when the claim is stale or the overlay no longer shows a
    /// saved-session browser, so a superseded dispatch costs nothing. The claim
    /// consumes the intent the generation was queued with, because the work it
    /// builds is the only owner of that intent from here on.
    pub(crate) fn claim_record_browser_refresh(
        &mut self,
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
        let Some(active_source) = self.active_record_browser_source() else {
            return Ok(None);
        };
        let Some(intent) = self
            .presentation
            .take_record_browser_refresh_intent(refresh_key)
        else {
            return Ok(None);
        };
        let source = match &intent {
            RuntimeRecordBrowserRefreshIntent::ApplyFilter { target, .. } => {
                target.as_ref().clone()
            }
            RuntimeRecordBrowserRefreshIntent::RefreshInPlace { .. }
            | RuntimeRecordBrowserRefreshIntent::FetchAdjacent { .. }
            | RuntimeRecordBrowserRefreshIntent::RefreshAfterDelete { .. } => active_source.clone(),
        };
        let config_root = self
            .integration
            .config_root()
            .map(|path| path.to_path_buf());
        let issue_database_path = match &source {
            RuntimeRecordBrowserOverlaySource::Issues { .. } => config_root
                .as_ref()
                .map(|root| super::issues::runtime_issue_database_path(self, root)),
            _ => None,
        };
        Ok(Some(RuntimeRecordBrowserRefreshWork {
            refresh_key: refresh_key.to_string(),
            generation,
            source,
            active_source,
            intent,
            transcript_store: self.persistence.transcript_store().cloned(),
            config_root,
            issue_database_path,
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
        if let RuntimeRecordBrowserRefreshIntent::ApplyFilter {
            active_record_id,
            active_index,
            error,
            ..
        } = &work.intent
        {
            return Self::execute_filter_page_refresh(
                work,
                active_record_id.as_deref(),
                *active_index,
                error.as_deref(),
            );
        }
        if let RuntimeRecordBrowserRefreshIntent::RefreshAfterDelete { active_index } = &work.intent
        {
            return Self::execute_delete_refresh(work, *active_index);
        }
        let Some(store) = work.transcript_store.as_ref() else {
            return RuntimeRecordBrowserRefreshOutcome::Failed {
                message: "record browser refresh requires transcript storage".to_string(),
                kind: MezErrorKind::InvalidState,
            };
        };
        let mut source = work.source.clone();
        let mut active_index = None;
        if let RuntimeRecordBrowserRefreshIntent::FetchAdjacent {
            delta,
            first_id,
            last_id,
        } = &work.intent
        {
            let current_anchor = match &source {
                RuntimeRecordBrowserOverlaySource::SavedSessions { anchor, .. } => anchor.clone(),
                _ => None,
            };
            let cursor_id = if delta.is_positive() {
                last_id
            } else {
                first_id
            };
            let cursor = match store.saved_session(cursor_id) {
                Ok(Some(session)) => {
                    crate::storage::transcript::SavedSessionCursor::from_session(&session)
                }
                Ok(None) => {
                    return RuntimeRecordBrowserRefreshOutcome::Failed {
                        message: "saved-session page cursor was not found".to_string(),
                        kind: MezErrorKind::NotFound,
                    };
                }
                Err(error) => {
                    return RuntimeRecordBrowserRefreshOutcome::Failed {
                        message: error.message().to_string(),
                        kind: error.kind(),
                    };
                }
            };
            let next_anchor = if delta.is_positive() {
                SavedSessionPageAnchor::After(cursor)
            } else if current_anchor.is_none() {
                SavedSessionPageAnchor::Last
            } else {
                SavedSessionPageAnchor::Before(cursor)
            };
            if let RuntimeRecordBrowserOverlaySource::SavedSessions { anchor, .. } = &mut source {
                *anchor = Some(next_anchor);
            }
        }
        let mut browser = match Self::rebuild_saved_session_page(store, &source, work) {
            Ok(browser) => browser,
            Err(error) => {
                return RuntimeRecordBrowserRefreshOutcome::Failed {
                    message: error.message().to_string(),
                    kind: error.kind(),
                };
            }
        };
        match &work.intent {
            RuntimeRecordBrowserRefreshIntent::RefreshInPlace { active_record_id } => {
                if let Some(active_record_id) = active_record_id.as_deref() {
                    browser.set_active_record_id(active_record_id);
                }
            }
            RuntimeRecordBrowserRefreshIntent::FetchAdjacent { delta, .. } => {
                if browser.records().is_empty() {
                    // The cursor record is the last row the catalog served, so an
                    // empty neighbour means the catalog ended between the two
                    // reads; the fallback anchor shows the same edge the inline
                    // fetch showed.
                    if let RuntimeRecordBrowserOverlaySource::SavedSessions { anchor, .. } =
                        &mut source
                    {
                        *anchor = if delta.is_positive() {
                            None
                        } else {
                            Some(SavedSessionPageAnchor::Last)
                        };
                    }
                    browser = match Self::rebuild_saved_session_page(store, &source, work) {
                        Ok(browser) => browser,
                        Err(error) => {
                            return RuntimeRecordBrowserRefreshOutcome::Failed {
                                message: error.message().to_string(),
                                kind: error.kind(),
                            };
                        }
                    };
                }
                active_index = Some(if delta.is_negative() {
                    browser.records().len().saturating_sub(1)
                } else {
                    0
                });
            }
            RuntimeRecordBrowserRefreshIntent::ApplyFilter { .. } => {}
            RuntimeRecordBrowserRefreshIntent::RefreshAfterDelete { .. } => {}
        }
        RuntimeRecordBrowserRefreshOutcome::Rebuilt {
            browser: Box::new(browser),
            source,
            active_index,
        }
    }

    /// Rebuilds one page for a filter change, whatever store backs it.
    ///
    /// Saved sessions run the preserving refresh that restores the focused row;
    /// the other store-backed families rebuild their page and restore that row
    /// when the new filters still match it.
    fn execute_filter_page_refresh(
        work: &RuntimeRecordBrowserRefreshWork,
        active_record_id: Option<&str>,
        active_index: Option<usize>,
        error: Option<&str>,
    ) -> RuntimeRecordBrowserRefreshOutcome {
        if matches!(
            work.source,
            RuntimeRecordBrowserOverlaySource::SavedSessions { .. }
        ) {
            let Some(store) = work.transcript_store.as_ref() else {
                return RuntimeRecordBrowserRefreshOutcome::Failed {
                    message: "record browser refresh requires transcript storage".to_string(),
                    kind: MezErrorKind::InvalidState,
                };
            };
            return Self::execute_preserving_page_refresh(store, work, active_record_id, error);
        }
        let mut browser = match Self::rebuild_store_backed_page(work) {
            Ok(browser) => browser,
            Err(error) => {
                return RuntimeRecordBrowserRefreshOutcome::Failed {
                    message: error.message().to_string(),
                    kind: error.kind(),
                };
            }
        };
        // The focused row restores by id when the new filters still match it; the
        // index the operator was on stands when they do not.
        let kept_index = match active_record_id {
            Some(record_id) if browser.set_active_record_id(record_id) => None,
            _ => active_index,
        };
        RuntimeRecordBrowserRefreshOutcome::Rebuilt {
            browser: Box::new(browser),
            source: work.source.clone(),
            active_index: kept_index,
        }
    }

    /// Rebuilds one page for a store-backed browser off actor ownership.
    ///
    /// Each family reads the store its source names; the claim captured whatever
    /// the read needs from live configuration, so the worker stays static.
    fn rebuild_store_backed_page(
        work: &RuntimeRecordBrowserRefreshWork,
    ) -> Result<mez_mux::record_browser::RecordBrowser> {
        match &work.source {
            RuntimeRecordBrowserOverlaySource::Issues { .. } => {
                let Some(database_path) = work.issue_database_path.clone() else {
                    return Err(MezError::config(
                        "show-issues requires a configured config root",
                    ));
                };
                RuntimeSessionService::read_issue_browser_for_refresh(database_path, &work.source)
            }
            RuntimeRecordBrowserOverlaySource::Memories { .. } => {
                let Some(config_root) = work.config_root.clone() else {
                    return Err(MezError::invalid_state(
                        "show-memories requires a configured Mezzanine config root",
                    ));
                };
                RuntimeSessionService::read_memory_browser_for_refresh(config_root, &work.source)
            }
            RuntimeRecordBrowserOverlaySource::Context {
                conversation_id,
                pane_id,
            } => {
                let Some(store) = work.transcript_store.as_ref() else {
                    return Err(MezError::invalid_state(
                        "context browser refresh requires transcript storage",
                    ));
                };
                RuntimeSessionService::read_context_browser_for_refresh(
                    store,
                    conversation_id,
                    pane_id,
                )
            }
            _ => Err(MezError::invalid_state(
                "record browser refresh requires a store-backed source",
            )),
        }
    }

    /// Rebuilds one saved-session page from owned work inputs.
    ///
    /// The store read stays in one place so both intents rebuild through the
    /// same query the inline refresh used, and a non-saved-session source fails
    /// with the kind the inline path reported.
    fn rebuild_saved_session_page(
        store: &crate::storage::transcript::AgentTranscriptStore,
        source: &RuntimeRecordBrowserOverlaySource,
        work: &RuntimeRecordBrowserRefreshWork,
    ) -> Result<mez_mux::record_browser::RecordBrowser> {
        let RuntimeRecordBrowserOverlaySource::SavedSessions {
            directory,
            default_directory,
            lifecycle,
            include_subagents,
            search,
            anchor,
            limit,
            ..
        } = source
        else {
            return Err(MezError::invalid_state(
                "record browser refresh requires a saved-session source",
            ));
        };
        let mut browser = super::resume::runtime_agent_saved_sessions_browser(
            store,
            directory.as_deref(),
            *lifecycle,
            *include_subagents,
            search.as_deref(),
            anchor.clone(),
            *limit,
            work.prompt_width,
            work.title_policy,
        )?;
        // The builder cannot see the retained default directory, so the shared
        // capability keeps a picker's scope toggle after `a` left its directory
        // scope; a deferred rebuild must not drop it.
        super::resume::apply_saved_session_scope_capability(
            &mut browser,
            directory.as_deref(),
            default_directory.as_deref(),
        );
        Ok(browser)
    }

    /// Rebuilds one saved-session page around the row the operator had focused.
    ///
    /// Mirrors the inline preserving refresh: the first page is tried, then the
    /// page anchored at the focused record. The first build already holds the page
    /// the inline fallback re-read, so it is reused instead of reading it again,
    /// and a record the new filters no longer match simply leaves that page.
    fn execute_preserving_page_refresh(
        store: &crate::storage::transcript::AgentTranscriptStore,
        work: &RuntimeRecordBrowserRefreshWork,
        active_record_id: Option<&str>,
        error: Option<&str>,
    ) -> RuntimeRecordBrowserRefreshOutcome {
        let finish = |mut browser: mez_mux::record_browser::RecordBrowser,
                      source: RuntimeRecordBrowserOverlaySource| {
            // A settlement status rides the rebuilt page, exactly as the inline
            // refresh stamped it before installing.
            browser.set_error(error.map(str::to_string));
            RuntimeRecordBrowserRefreshOutcome::Rebuilt {
                browser: Box::new(browser),
                source,
                active_index: None,
            }
        };
        let mut first_page = work.source.clone();
        if let RuntimeRecordBrowserOverlaySource::SavedSessions { anchor, .. } = &mut first_page {
            *anchor = None;
        }
        let browser = match Self::rebuild_saved_session_page(store, &first_page, work) {
            Ok(browser) => browser,
            Err(error) => {
                return RuntimeRecordBrowserRefreshOutcome::Failed {
                    message: error.message().to_string(),
                    kind: error.kind(),
                };
            }
        };
        let Some(active_record_id) = active_record_id else {
            return finish(browser, first_page);
        };
        let mut first_page_browser = browser;
        if first_page_browser.set_active_record_id(active_record_id) {
            return finish(first_page_browser, first_page);
        }
        let cursor = match store.saved_session(active_record_id) {
            Ok(Some(session)) => {
                crate::storage::transcript::SavedSessionCursor::from_session(&session)
            }
            Ok(None) => {
                return finish(first_page_browser, first_page);
            }
            Err(error) => {
                return RuntimeRecordBrowserRefreshOutcome::Failed {
                    message: error.message().to_string(),
                    kind: error.kind(),
                };
            }
        };
        let mut anchored = first_page.clone();
        if let RuntimeRecordBrowserOverlaySource::SavedSessions { anchor, .. } = &mut anchored {
            *anchor = Some(SavedSessionPageAnchor::At(cursor));
        }
        match Self::rebuild_saved_session_page(store, &anchored, work) {
            Ok(mut anchored_browser) => {
                if anchored_browser.set_active_record_id(active_record_id) {
                    finish(anchored_browser, anchored)
                } else {
                    finish(first_page_browser, first_page)
                }
            }
            Err(error) => RuntimeRecordBrowserRefreshOutcome::Failed {
                message: error.message().to_string(),
                kind: error.kind(),
            },
        }
    }

    /// Rebuilds the page one delete left behind and restores its row index.
    ///
    /// The deleted row held the focus, so the rebuilt page keeps the raw index
    /// instead of a record id. A page that comes back empty (the last row of an
    /// anchored page) falls back to the head of the same source, exactly as the
    /// inline path did.
    fn execute_delete_refresh(
        work: &RuntimeRecordBrowserRefreshWork,
        active_index: usize,
    ) -> RuntimeRecordBrowserRefreshOutcome {
        if !matches!(
            work.source,
            RuntimeRecordBrowserOverlaySource::SavedSessions { .. }
        ) {
            // The other store-backed families keep the row index the delete left
            // and rebuild through the same per-family read as a filter change.
            let browser = match Self::rebuild_store_backed_page(work) {
                Ok(browser) => browser,
                Err(error) => {
                    return RuntimeRecordBrowserRefreshOutcome::Failed {
                        message: error.message().to_string(),
                        kind: error.kind(),
                    };
                }
            };
            return RuntimeRecordBrowserRefreshOutcome::Rebuilt {
                browser: Box::new(browser),
                source: work.source.clone(),
                active_index: Some(active_index),
            };
        }
        let Some(store) = work.transcript_store.as_ref() else {
            return RuntimeRecordBrowserRefreshOutcome::Failed {
                message: "record browser refresh requires transcript storage".to_string(),
                kind: MezErrorKind::InvalidState,
            };
        };
        let mut source = work.source.clone();
        let mut kept_index = Some(active_index);
        let mut browser = match Self::rebuild_saved_session_page(store, &source, work) {
            Ok(browser) => browser,
            Err(error) => {
                return RuntimeRecordBrowserRefreshOutcome::Failed {
                    message: error.message().to_string(),
                    kind: error.kind(),
                };
            }
        };
        if browser.records().is_empty() {
            if let RuntimeRecordBrowserOverlaySource::SavedSessions { anchor, .. } = &mut source {
                *anchor = None;
            }
            browser = match Self::rebuild_saved_session_page(store, &source, work) {
                Ok(browser) => browser,
                Err(error) => {
                    return RuntimeRecordBrowserRefreshOutcome::Failed {
                        message: error.message().to_string(),
                        kind: error.kind(),
                    };
                }
            };
            // The inline fallback installed a fresh browser, which starts at the
            // first row; only the normal rebuild keeps the index the delete left.
            kept_index = None;
        }
        RuntimeRecordBrowserRefreshOutcome::Rebuilt {
            browser: Box::new(browser),
            source,
            active_index: kept_index,
        }
    }

    /// Queues one refresh dispatch without an open overlay for tests.
    ///
    /// A test that asserts the actor emits what a real claim queues uses this
    /// instead of standing up the picker.
    #[cfg(test)]
    pub(crate) fn queue_record_browser_refresh_for_tests(&mut self, refresh_key: &str) {
        self.begin_record_browser_refresh_claim_for_intent(
            refresh_key,
            RuntimeRecordBrowserRefreshIntent::RefreshInPlace {
                active_record_id: None,
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
        if self
            .presentation
            .record_browser_refresh_generation(&work.refresh_key)
            == work.generation
        {
            // This claim settles here, so the filter target it carried stops
            // composing into later keys whether or not its page installed. A
            // superseded claim leaves its successor's target in place.
            self.presentation
                .clear_record_browser_pending_target(&work.refresh_key);
        }
        match outcome {
            RuntimeRecordBrowserRefreshOutcome::Failed { message, kind } => {
                if self
                    .presentation
                    .record_browser_refresh_generation(&work.refresh_key)
                    != work.generation
                {
                    return Ok(false);
                }
                // Pane-scoped keys are not bumped on registration, so a claim must
                // still name the overlay it came from; the shared picker key relies
                // on its dismissal and registration bumps instead.
                if !matches!(
                    work.source,
                    RuntimeRecordBrowserOverlaySource::SavedSessions { .. }
                ) && self.active_record_browser_refresh_key().as_deref()
                    != Some(work.refresh_key.as_str())
                {
                    return Ok(false);
                }
                if !self.active_record_browser_matches(&work.active_source) {
                    return Ok(false);
                }
                if self.active_record_browser_is_detail() {
                    return Ok(false);
                }
                Ok(self.set_active_record_browser_error(&format!(
                    "overlay refresh failed: {message} ({kind:?})"
                )))
            }
            RuntimeRecordBrowserRefreshOutcome::Rebuilt {
                browser,
                source,
                active_index,
            } => {
                if self
                    .presentation
                    .record_browser_refresh_generation(&work.refresh_key)
                    != work.generation
                {
                    return Ok(false);
                }
                if !matches!(
                    work.source,
                    RuntimeRecordBrowserOverlaySource::SavedSessions { .. }
                ) && self.active_record_browser_refresh_key().as_deref()
                    != Some(work.refresh_key.as_str())
                {
                    return Ok(false);
                }
                if !self.active_record_browser_matches(&work.active_source) {
                    return Ok(false);
                }
                let mut browser = *browser;
                // A delete owns the page it emptied: the row whose detail was open
                // is gone, so the rebuilt page replaces it. Every other intent
                // leaves an open detail alone, which keeps a rebuild that settles
                // late from closing a record the operator just opened.
                if self.active_record_browser_is_detail()
                    && !matches!(
                        work.intent,
                        RuntimeRecordBrowserRefreshIntent::RefreshAfterDelete { .. }
                    )
                {
                    return Ok(false);
                }
                if let Some(active_index) = active_index {
                    browser.set_active_index(active_index);
                }
                Ok(self.replace_active_record_browser(source, browser))
            }
        }
    }
}
