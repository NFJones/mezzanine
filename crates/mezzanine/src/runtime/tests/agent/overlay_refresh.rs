//! Regression coverage for the deferred record-browser refresh lane.

use crate::runtime::RuntimeRecordBrowserRefreshOutcome;
use crate::runtime::RuntimeSessionService;
use crate::runtime::service_state::RuntimeRecordBrowserOverlaySource;
use crate::runtime::tests::{temp_root, test_runtime_service};
use crate::storage::transcript::{AgentTranscriptStore, SavedSessionLifecycleFilter};
use mez_mux::layout::Size;

/// Verifies a rebuilt saved-session page installs while its claim is current and
/// a page rebuilt for a superseded claim is dropped.
///
/// The lane exists so the store walk behind a page refresh does not park the
/// serialized actor: the actor claims a generation, a worker rebuilds the page,
/// and the completion installs it only while no newer claim owns the overlay.
#[test]
fn overlay_refresh_installs_current_generation_and_drops_superseded_pages() {
    let mut service = test_runtime_service();
    let _primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let transcript_store = AgentTranscriptStore::new(temp_root("overlay-refresh-history"));
    service.set_agent_transcript_store(transcript_store);
    let source = RuntimeRecordBrowserOverlaySource::SavedSessions {
        directory: None,
        default_directory: None,
        lifecycle: SavedSessionLifecycleFilter::Active,
        include_subagents: false,
        search: None,
        anchor: None,
        limit: 20,
    };
    let browser = service
        .saved_sessions_record_browser_for_query(
            None,
            SavedSessionLifecycleFilter::Active,
            false,
            None,
            None,
            20,
        )
        .unwrap();
    // Open the picker the way `/resume` does: register the browser, then apply
    // the command response that promotes it to the live primary overlay.
    let page = browser.render_page();
    service.register_pending_record_browser_overlay("$overlay", "resume", browser, Some(source));
    let response = crate::runtime::runtime_agent_shell_command_response_json(
        "$overlay",
        "/resume",
        Some(&crate::runtime::AgentShellCommandOutcome::Display {
            command: "resume".to_string(),
            body: page.raw_markdown,
        }),
    );
    service
        .set_agent_prompt_response_display_output_for_tests("$overlay", &response)
        .unwrap();
    assert!(
        service.active_saved_session_browser_source().is_some(),
        "the fixture must open a saved-session browser"
    );

    let generation = service
        .begin_record_browser_refresh_claim("$overlay")
        .expect("an open saved-session browser claims a refresh");
    assert_eq!(
        service.take_pending_record_browser_refreshes().len(),
        1,
        "the claim queues exactly one worker dispatch"
    );
    let work = service
        .claim_record_browser_refresh("$overlay", generation)
        .unwrap()
        .expect("the current claim yields owned work");
    let outcome = RuntimeSessionService::execute_record_browser_refresh(&work);
    assert!(
        service
            .complete_record_browser_refresh(&work, outcome)
            .unwrap(),
        "a current claim installs its rebuilt page"
    );

    let superseded = service
        .begin_record_browser_refresh_claim("$overlay")
        .expect("the overlay still claims refreshes");
    assert!(superseded > generation);
    let stale_outcome = RuntimeSessionService::execute_record_browser_refresh(&work);
    assert!(
        !service
            .complete_record_browser_refresh(&work, stale_outcome)
            .unwrap(),
        "a superseded generation must not replace the page its successor owns"
    );

    // The same guard covers failures: a superseded rebuild that fails must not
    // stamp an error onto the page its successor owns.
    let failed = RuntimeRecordBrowserRefreshOutcome::Failed {
        message: "superseded rebuild failed".to_string(),
        kind: crate::error::MezErrorKind::InvalidState,
    };
    assert!(
        !service
            .complete_record_browser_refresh(&work, failed)
            .unwrap(),
        "a superseded failure must not mark the successor page"
    );
}

/// Opens the shared saved-session picker over `sessions` catalog rows and returns
/// the attached primary client.
///
/// The picker opens through the deferred `/resume` lane because that is the path
/// that promotes the browser to the live primary overlay with selectable rows,
/// which is what the paging claims read.
fn open_saved_session_picker(
    service: &mut RuntimeSessionService,
    root: &str,
    sessions: usize,
) -> mez_core::ids::ClientId {
    let transcript_store = AgentTranscriptStore::new(temp_root(root));
    for index in 0..sessions {
        transcript_store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: format!("refresh-page-{index:02}"),
                sequence: 1,
                created_at_unix_seconds: 100 - index as u64,
                role: mez_agent::transcript::TranscriptRole::User,
                turn_id: format!("turn-{index}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: format!("page prompt {index}"),
            })
            .unwrap();
    }
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(120, 12).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let response = service
        .execute_agent_shell_command(&primary, "/resume")
        .unwrap();
    assert!(
        response.contains(r#""body":null"#),
        "the deferred lane acknowledges /resume: {response}"
    );
    service
        .run_pending_deferred_agent_command_for_tests()
        .unwrap()
        .expect("the deferred /resume picker applies its page");
    primary
}

/// Returns the active saved-session picker page's record ids.
fn saved_session_page_ids(service: &RuntimeSessionService) -> Vec<String> {
    service
        .primary_display_overlay()
        .and_then(|overlay| overlay.record_browser.as_ref())
        .map(|record_browser| {
            record_browser
                .browser
                .records()
                .iter()
                .map(|record| record.id.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

/// Steps the picker cursor onto the last row of the installed page.
fn move_saved_session_cursor_to_last_row(
    service: &mut RuntimeSessionService,
    primary: &mez_core::ids::ClientId,
) {
    for _ in 1..saved_session_page_ids(service).len() {
        service
            .apply_primary_display_overlay_input(primary, b"\x1b[B")
            .unwrap();
    }
}

/// Settles one claimed page fetch through claim, execute, and completion.
fn settle_saved_session_page_claim(service: &mut RuntimeSessionService) -> bool {
    let dispatch = service
        .take_pending_record_browser_refreshes()
        .into_iter()
        .next()
        .expect("the edge keypress claims one page fetch");
    assert_eq!(
        dispatch.refresh_key,
        crate::runtime::SAVED_SESSION_OVERLAY_REFRESH_KEY
    );
    let work = service
        .claim_record_browser_refresh(&dispatch.refresh_key, dispatch.generation)
        .unwrap()
        .expect("the current claim yields owned work");
    let outcome = RuntimeSessionService::execute_record_browser_refresh(&work);
    service
        .complete_record_browser_refresh(&work, outcome)
        .unwrap()
}

/// Verifies a page-edge keypress claims the adjacent saved-session page and
/// installs it only when the deferred fetch settles.
///
/// The claim reads the page identity from retained overlay state, and the worker
/// resolves the edge cursor record into a keyset anchor, so paging never waits
/// on the catalog read inside the actor.
#[test]
fn overlay_refresh_claims_adjacent_page_and_installs_the_fetch() {
    let mut service = test_runtime_service();
    let primary = open_saved_session_picker(&mut service, "overlay-refresh-pages", 45);
    let first_ids = saved_session_page_ids(&service);
    assert_eq!(first_ids.len(), 20);
    move_saved_session_cursor_to_last_row(&mut service, &primary);

    // The last row's step leaves the page, so the keypress claims a fetch and the
    // operator keeps the current page until that fetch settles.
    service
        .apply_primary_display_overlay_input(&primary, b"\x1b[B")
        .unwrap();
    assert_eq!(
        saved_session_page_ids(&service),
        first_ids,
        "a claimed page fetch leaves the current page installed"
    );
    assert!(
        settle_saved_session_page_claim(&mut service),
        "the adjacent page installs while its claim is current"
    );
    let second_ids = saved_session_page_ids(&service);
    assert_eq!(second_ids.len(), 20);
    assert!(first_ids.iter().all(|id| !second_ids.contains(id)));
    assert_eq!(
        service.active_saved_session_browser_record_id(),
        second_ids.first().cloned(),
        "a forward fetch focuses the first row of the fetched page"
    );

    // A backward step from the fetched page's first row claims its predecessor.
    service
        .apply_primary_display_overlay_input(&primary, b"\x1b[A")
        .unwrap();
    assert_eq!(
        saved_session_page_ids(&service),
        second_ids,
        "the backward claim keeps the current page until it settles"
    );
    assert!(
        settle_saved_session_page_claim(&mut service),
        "the previous page installs while its claim is current"
    );
    assert_eq!(saved_session_page_ids(&service), first_ids);
    assert_eq!(
        service.active_saved_session_browser_record_id(),
        first_ids.last().cloned(),
        "a backward fetch focuses the last row of the fetched page"
    );
}

/// Verifies a forward fetch past the catalog's last row falls back to the head
/// of the catalog, the page the inline fetch showed for the same edge.
#[test]
fn overlay_refresh_adjacent_page_past_the_catalog_end_falls_back_to_the_edge() {
    let mut service = test_runtime_service();
    let primary = open_saved_session_picker(&mut service, "overlay-refresh-catalog-end", 25);
    let first_ids = saved_session_page_ids(&service);
    assert_eq!(first_ids.len(), 20);
    move_saved_session_cursor_to_last_row(&mut service, &primary);
    service
        .apply_primary_display_overlay_input(&primary, b"\x1b[B")
        .unwrap();
    assert!(settle_saved_session_page_claim(&mut service));
    let remainder_ids = saved_session_page_ids(&service);
    assert_eq!(
        remainder_ids.len(),
        5,
        "the trailing page holds the catalog's remainder"
    );

    // The remainder's last row steps past the final catalog row, so the fetch
    // comes back empty and the worker re-reads from the catalog head.
    move_saved_session_cursor_to_last_row(&mut service, &primary);
    service
        .apply_primary_display_overlay_input(&primary, b"\x1b[B")
        .unwrap();
    assert!(settle_saved_session_page_claim(&mut service));
    assert_eq!(saved_session_page_ids(&service), first_ids);
    assert_eq!(
        service.active_saved_session_browser_record_id(),
        first_ids.first().cloned(),
        "the fallback page focuses its first row"
    );
}
