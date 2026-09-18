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
