//! Regression coverage for the deferred record-browser refresh lane.

use crate::runtime::RuntimeRecordBrowserRefreshOutcome;
use crate::runtime::RuntimeSessionService;
use crate::runtime::service_state::RuntimeRecordBrowserOverlaySource;
use crate::runtime::tests::{temp_root, test_runtime_service};
use crate::runtime::{RuntimeSideEffect, SessionArchiveOperation};
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

/// Reports whether the active saved-session picker offers its scope toggle.
fn saved_session_scope_toggle_enabled(service: &RuntimeSessionService) -> bool {
    service
        .primary_display_overlay()
        .and_then(|overlay| overlay.record_browser.as_ref())
        .is_some_and(|record_browser| record_browser.browser.scope_toggle_enabled())
}

/// Reports whether the active saved-session picker shows one record's detail.
fn saved_session_detail_open(service: &RuntimeSessionService) -> bool {
    service
        .primary_display_overlay()
        .and_then(|overlay| overlay.record_browser.as_ref())
        .is_some_and(|record_browser| record_browser.browser.is_detail_view())
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

/// Verifies a deferred page rebuild keeps the picker's retained scope toggle.
///
/// A picker opened on a directory scope keeps its `a` toggle after switching to
/// the unbounded scope, because the retained default directory can still be
/// toggled back to. The inline refresh always applied that capability, so a
/// deferred rebuild that drops it leaves the operator without the key.
#[test]
fn overlay_refresh_adjacent_page_keeps_the_retained_scope_toggle() {
    let root = temp_root("overlay-refresh-scope-toggle");
    let scoped_root = root.join("scoped");
    let other_root = root.join("other");
    for project in [&scoped_root, &other_root] {
        std::fs::create_dir_all(project.join(".git")).unwrap();
    }
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(root.join("sessions"));
    for index in 0..10 {
        transcript_store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: format!("scoped-{index:02}"),
                sequence: 1,
                created_at_unix_seconds: 1000 - index,
                role: mez_agent::transcript::TranscriptRole::System,
                turn_id: format!("turn-scoped-{index}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: format!("cwd={}", scoped_root.display()),
            })
            .unwrap();
        transcript_store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: format!("scoped-{index:02}"),
                sequence: 2,
                created_at_unix_seconds: 1000 - index,
                role: mez_agent::transcript::TranscriptRole::User,
                turn_id: format!("turn-scoped-{index}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: format!("scoped page prompt {index}"),
            })
            .unwrap();
    }
    for index in 0..25 {
        transcript_store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: format!("unscoped-{index:02}"),
                sequence: 1,
                created_at_unix_seconds: 200 - index,
                role: mez_agent::transcript::TranscriptRole::System,
                turn_id: format!("turn-unscoped-{index}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: format!("cwd={}", other_root.display()),
            })
            .unwrap();
        transcript_store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: format!("unscoped-{index:02}"),
                sequence: 2,
                created_at_unix_seconds: 200 - index,
                role: mez_agent::transcript::TranscriptRole::User,
                turn_id: format!("turn-unscoped-{index}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: format!("unscoped page prompt {index}"),
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
    service.set_pane_current_working_directory("%1", scoped_root);
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
    assert!(
        saved_session_scope_toggle_enabled(&service),
        "a picker opened on a directory scope offers its scope toggle"
    );
    let scoped_ids = saved_session_page_ids(&service);
    assert_eq!(scoped_ids.len(), 10);
    assert!(scoped_ids.iter().all(|id| id.starts_with("scoped-")));

    // `a` drops the directory scope and keeps only the retained default, so the
    // picker still offers the toggle while it lists every directory.
    service
        .apply_primary_display_overlay_input(&primary, b"a")
        .unwrap();
    assert_eq!(
        saved_session_page_ids(&service),
        scoped_ids,
        "a claimed filter change leaves the current page installed"
    );
    assert!(
        service
            .run_pending_record_browser_refresh_for_tests()
            .unwrap(),
        "the scope toggle claims the unbounded page"
    );
    assert!(saved_session_scope_toggle_enabled(&service));
    let unscoped_ids = saved_session_page_ids(&service);
    assert_eq!(unscoped_ids.len(), 20);
    assert!(
        unscoped_ids.iter().any(|id| id.starts_with("unscoped-")),
        "the toggled picker lists every directory: {unscoped_ids:?}"
    );

    // The fetch past the page edge runs in the lane, so its rebuild has to keep
    // the capability the inline refresh kept.
    move_saved_session_cursor_to_last_row(&mut service, &primary);
    service
        .apply_primary_display_overlay_input(&primary, b"\x1b[B")
        .unwrap();
    assert!(settle_saved_session_page_claim(&mut service));
    let fetched_ids = saved_session_page_ids(&service);
    assert!(
        fetched_ids.iter().all(|id| id.starts_with("unscoped-")),
        "the fetched page continues the toggled catalog: {fetched_ids:?}"
    );
    assert!(
        saved_session_scope_toggle_enabled(&service),
        "the deferred page rebuild keeps the retained scope toggle"
    );

    // The retained default directory still toggles back into scope.
    service
        .apply_primary_display_overlay_input(&primary, b"a")
        .unwrap();
    assert!(
        service
            .run_pending_record_browser_refresh_for_tests()
            .unwrap(),
        "the scope toggle claims the directory page again"
    );
    assert_eq!(saved_session_page_ids(&service), scoped_ids);
}

/// Verifies a settling rebuild leaves a record the operator opened into detail.
///
/// The detail view keeps the picker's source, so the staleness guard alone would
/// let an in-flight rebuild replace the browser and close the detail; the
/// completion drops that rebuild instead.
#[test]
fn overlay_refresh_settles_without_closing_an_open_detail_view() {
    let mut service = test_runtime_service();
    let primary = open_saved_session_picker(&mut service, "overlay-refresh-detail", 45);
    service
        .apply_primary_display_overlay_input(&primary, b"u")
        .unwrap();
    service
        .apply_primary_display_overlay_input(&primary, b"i")
        .unwrap();
    assert!(
        saved_session_detail_open(&service),
        "the focused row opens its detail view"
    );
    assert!(
        !service
            .run_pending_record_browser_refresh_for_tests()
            .unwrap(),
        "a settling rebuild must not close the open detail"
    );
    assert!(saved_session_detail_open(&service));
}

/// Verifies two filter keys pressed before settlement compose into one page.
///
/// A filter key switches the retained source as it arrives, so the second target
/// is derived from the first filter instead of the page the picker still shows.
#[test]
fn overlay_refresh_composes_filter_keys_pressed_before_settlement() {
    let mut service = test_runtime_service();
    let primary = open_saved_session_picker(&mut service, "overlay-refresh-compose-filters", 45);
    service
        .apply_primary_display_overlay_input(&primary, b"u")
        .unwrap();
    service
        .apply_primary_display_overlay_input(&primary, b"r")
        .unwrap();
    assert!(
        service
            .run_pending_record_browser_refresh_for_tests()
            .unwrap(),
        "the newest claim installs the composed page"
    );
    let source = service
        .active_saved_session_browser_source()
        .expect("the picker stays open");
    let RuntimeRecordBrowserOverlaySource::SavedSessions {
        include_subagents,
        lifecycle,
        ..
    } = source
    else {
        panic!("the picker keeps a saved-session source");
    };
    assert!(
        include_subagents,
        "the subagent toggle survives the second key"
    );
    assert!(matches!(lifecycle, SavedSessionLifecycleFilter::Archived));
}

/// Verifies a dismissed picker's pending filter does not install on reopen.
///
/// Dismissal and registration both invalidate the picker's outstanding claims, so
/// a picker opened afterwards shows its own page instead of inheriting the closed
/// one's rebuild.
#[test]
fn overlay_refresh_drops_a_pending_filter_after_dismiss_and_reopen() {
    let mut service = test_runtime_service();
    let primary = open_saved_session_picker(&mut service, "overlay-refresh-reopen", 45);
    let first_ids = saved_session_page_ids(&service);
    service
        .apply_primary_display_overlay_input(&primary, b"u")
        .unwrap();
    assert!(service.dismiss_primary_display_overlay());
    let response = service
        .execute_agent_shell_command(&primary, "/resume")
        .unwrap();
    assert!(
        response.contains(r#""body":null"#),
        "the deferred lane acknowledges the reopened /resume: {response}"
    );
    service
        .run_pending_deferred_agent_command_for_tests()
        .unwrap()
        .expect("the reopened picker applies its page");
    assert!(
        !service
            .run_pending_record_browser_refresh_for_tests()
            .unwrap(),
        "a dismissed claim must not install into the reopened picker"
    );
    assert_eq!(saved_session_page_ids(&service), first_ids);
}

/// Verifies source-derived keys keep acting on the page the picker displays.
///
/// The retained source describes the displayed page, so a lifecycle toggle whose
/// page is still being fetched must not make the archive key queue a restore or
/// the detail key look the active row up as archived.
#[test]
fn overlay_refresh_keeps_source_derived_keys_on_the_displayed_page() {
    let mut service = test_runtime_service();
    let primary = open_saved_session_picker(&mut service, "overlay-refresh-displayed-page", 45);
    service
        .apply_primary_display_overlay_input(&primary, b"r")
        .unwrap();
    service
        .apply_primary_display_overlay_input(&primary, b"A")
        .unwrap();
    let effects = service
        .drain_transcript_persistence_transition()
        .side_effects;
    assert!(
        effects.iter().any(|effect| matches!(
            effect,
            RuntimeSideEffect::PersistSessionArchive {
                operation: SessionArchiveOperation::Archive { .. },
                ..
            }
        )),
        "the archive key still archives the displayed active page: {effects:?}"
    );
    service
        .apply_primary_display_overlay_input(&primary, b"i")
        .unwrap();
    assert!(
        saved_session_detail_open(&service),
        "the detail key still opens the displayed active row"
    );
}

/// Verifies a post-mutation refresh installs its page with the settlement status.
///
/// An archive settlement claims the refresh with the status string and the row to
/// keep; the worker stamps that status on the page it rebuilds, exactly as the
/// inline refresh did before installing.
#[test]
fn overlay_refresh_installs_a_post_mutation_page_with_its_settlement_status() {
    let mut service = test_runtime_service();
    let _primary = open_saved_session_picker(&mut service, "overlay-refresh-settlement", 45);
    let mut source = service
        .active_saved_session_browser_source()
        .expect("the picker keeps a saved-session source");
    // The status renders in the page's empty state, so the claim targets a search
    // that matches nothing and the settlement status is the only content left.
    let RuntimeRecordBrowserOverlaySource::SavedSessions { search, .. } = &mut source else {
        panic!("the picker keeps a saved-session source");
    };
    *search = Some("no-such-prompt".to_string());
    assert!(
        service
            .begin_record_browser_preserving_claim(
                source,
                None,
                false,
                Some("archive completed for fixture".to_string()),
            )
            .unwrap()
            .is_some(),
        "an open picker claims the settlement refresh"
    );
    assert!(
        service
            .run_pending_record_browser_refresh_for_tests()
            .unwrap(),
        "the settlement page installs while its claim is current"
    );
    assert!(saved_session_page_ids(&service).is_empty());
    let rendered = service
        .primary_display_overlay()
        .and_then(|overlay| overlay.record_browser.as_ref())
        .expect("the picker stays open")
        .browser
        .render_page()
        .markdown;
    assert!(
        rendered.contains("Error: archive completed for fixture"),
        "the settlement status rides the installed page: {rendered}"
    );
}

/// Verifies a failed archive settlement shows its status on the picker's page.
///
/// The failure event claims the refresh with its status and the conversation it
/// names, so the operator reads the settlement result on the page the picker
/// installs instead of only in the pane transcript.
#[test]
fn overlay_refresh_shows_a_failed_archive_settlement_status() {
    let mut service = test_runtime_service();
    let _primary =
        open_saved_session_picker(&mut service, "overlay-refresh-settlement-failure", 45);
    let first_ids = saved_session_page_ids(&service);
    let conversation_id = first_ids
        .first()
        .cloned()
        .expect("the picker lists a conversation");
    service
        .apply_persistence_transition(crate::runtime::PersistenceEvent::SessionArchiveFailed {
            conversation_id: conversation_id.clone(),
            operation: SessionArchiveOperation::Archive {
                archived_at_unix_seconds: 20,
            },
            error: "test failure".to_string(),
        })
        .unwrap();
    assert!(
        service
            .run_pending_record_browser_refresh_for_tests()
            .unwrap(),
        "the failed settlement claims the refresh"
    );
    assert_eq!(
        service.active_saved_session_browser_record_id(),
        Some(conversation_id),
        "the settled page keeps the conversation it names"
    );
    let rendered = service
        .primary_display_overlay()
        .and_then(|overlay| overlay.record_browser.as_ref())
        .expect("the picker stays open")
        .browser
        .render_page()
        .markdown;
    assert!(
        rendered.contains("Error: test failure"),
        "the failure status rides the installed page: {rendered}"
    );
}

/// Verifies a delete refresh rebuilds the page and keeps the row index.
///
/// The deleted row held the focus and its id is gone, so the claim records the
/// raw index the operator was on and the page shows the row that slid into it.
#[test]
fn overlay_refresh_after_delete_keeps_the_row_index() {
    let mut service = test_runtime_service();
    let primary = open_saved_session_picker(&mut service, "overlay-refresh-delete", 45);
    let first_ids = saved_session_page_ids(&service);
    service
        .apply_primary_display_overlay_input(&primary, b"\x1b[B")
        .unwrap();
    service
        .apply_primary_display_overlay_input(&primary, b"d")
        .unwrap();
    assert!(
        service
            .run_pending_record_browser_refresh_for_tests()
            .unwrap(),
        "the delete claims the page it left behind"
    );
    let remaining = saved_session_page_ids(&service);
    assert_eq!(
        remaining.len(),
        first_ids.len(),
        "the page refills from the rows the delete left behind"
    );
    assert!(!remaining.contains(&first_ids[1]));
    assert_eq!(
        remaining[1], first_ids[2],
        "the rebuilt page keeps the deleted row's index"
    );
    assert_eq!(
        service.active_saved_session_browser_record_id(),
        Some(first_ids[2].clone()),
        "the focus stays on the kept index"
    );
}

/// Verifies a delete that empties an anchored page falls back to its source head.
///
/// Deleting every row of a later page leaves the anchored query empty, so the
/// worker re-reads the head of the same source exactly as the inline path did.
#[test]
fn overlay_refresh_after_delete_falls_back_when_the_page_empties() {
    let mut service = test_runtime_service();
    let primary = open_saved_session_picker(&mut service, "overlay-refresh-delete-fallback", 45);
    let first_ids = saved_session_page_ids(&service);
    move_saved_session_cursor_to_last_row(&mut service, &primary);
    service
        .apply_primary_display_overlay_input(&primary, b"\x1b[B")
        .unwrap();
    assert!(settle_saved_session_page_claim(&mut service));
    let later_page = saved_session_page_ids(&service);
    assert_eq!(later_page.len(), first_ids.len());
    assert!(first_ids.iter().all(|id| !later_page.contains(id)));

    // The anchored page serves the catalog's tail: everything after the page the
    // operator left, which is the whole fixture minus its first page.
    let tail_len = 45 - first_ids.len();
    for deleted in 0..tail_len {
        service
            .apply_primary_display_overlay_input(&primary, b"d")
            .unwrap();
        assert!(
            settle_saved_session_page_claim(&mut service),
            "delete {deleted} claims the page it leaves"
        );
    }
    assert_eq!(
        saved_session_page_ids(&service),
        first_ids,
        "the emptied anchored page falls back to the source head"
    );
}

/// Verifies an emptied delete page reports no kept index.
///
/// The inline fallback installed a fresh browser, which opens on the first row,
/// so the worker must not carry the deleted row's index onto the fallback page.
/// The fallback needs an anchor whose successors are gone, which the picker's own
/// keys cannot produce (deleting a row always leaves its page's other rows), so
/// the claim's work is assembled directly.
#[test]
fn overlay_refresh_delete_fallback_reports_no_kept_index() {
    let mut service = test_runtime_service();
    let _primary =
        open_saved_session_picker(&mut service, "overlay-refresh-delete-fallback-index", 25);
    // The fixture names its rows `refresh-page-{index:02}`, so the catalog's last
    // row is the last one it appended.
    let last_id = "refresh-page-24".to_string();
    let store = service
        .persistence
        .cloned_transcript_store()
        .expect("the picker holds its catalog store");
    let session = store
        .saved_session(&last_id)
        .unwrap()
        .expect("the catalog row exists");
    let mut source = service
        .active_saved_session_browser_source()
        .expect("the picker keeps a saved-session source");
    let RuntimeRecordBrowserOverlaySource::SavedSessions { anchor, .. } = &mut source else {
        panic!("the picker keeps a saved-session source");
    };
    *anchor = Some(crate::storage::transcript::SavedSessionPageAnchor::After(
        crate::storage::transcript::SavedSessionCursor::from_session(&session),
    ));
    let work = crate::runtime::RuntimeRecordBrowserRefreshWork {
        refresh_key: crate::runtime::SAVED_SESSION_OVERLAY_REFRESH_KEY.to_string(),
        generation: 0,
        active_source: source.clone(),
        source,
        intent: crate::runtime::RuntimeRecordBrowserRefreshIntent::RefreshAfterDelete {
            active_index: 5,
        },
        transcript_store: Some(store),
        config_root: None,
        issue_database_path: None,
        prompt_width: service.saved_session_prompt_width(),
        title_policy: service.agent_session_title_policy(),
    };
    let outcome = RuntimeSessionService::execute_record_browser_refresh(&work);
    let RuntimeRecordBrowserRefreshOutcome::Rebuilt {
        browser,
        source,
        active_index,
    } = outcome
    else {
        panic!("the emptied anchored page falls back instead of failing");
    };
    assert!(
        !browser.records().is_empty(),
        "the fallback shows the head of the same source"
    );
    assert!(
        matches!(
            source,
            RuntimeRecordBrowserOverlaySource::SavedSessions { anchor: None, .. }
        ),
        "the fallback drops the emptied anchor"
    );
    assert_eq!(
        active_index, None,
        "the fallback opens on the first row, not the deleted row's index"
    );
}

/// Verifies a store-backed claim without its captured store fails cleanly.
///
/// The issue resolver reads live config, so a claim made before a config root
/// existed carries none; the worker must report the diagnostic the inline refresh
/// raised instead of panicking or reading the wrong store.
#[test]
fn overlay_refresh_reports_a_missing_issue_database_path() {
    let service = test_runtime_service();
    let source = RuntimeRecordBrowserOverlaySource::Issues {
        project_glob: None,
        default_project_glob: None,
        kind: None,
        state: None,
        active_only: false,
        text: None,
        limit: 20,
    };
    let work = crate::runtime::RuntimeRecordBrowserRefreshWork {
        refresh_key: "%1".to_string(),
        generation: 0,
        active_source: source.clone(),
        source: source.clone(),
        intent: crate::runtime::RuntimeRecordBrowserRefreshIntent::ApplyFilter {
            target: Box::new(source),
            active_record_id: None,
            active_index: None,
            replaces_detail: false,
            error: None,
        },
        transcript_store: None,
        config_root: None,
        issue_database_path: None,
        prompt_width: 40,
        title_policy: service.agent_session_title_policy(),
    };
    let outcome = RuntimeSessionService::execute_record_browser_refresh(&work);
    let RuntimeRecordBrowserRefreshOutcome::Failed { message, .. } = outcome else {
        panic!("a missing issue database path must fail the rebuild");
    };
    assert!(
        message.contains("config root"),
        "the failure keeps the inline diagnostic: {message}"
    );
}

/// Verifies a filter key pressed inside a detail view replaces that detail.
///
/// The lane never closes a detail a rebuild did not open; the key itself is the
/// operator's intent, so a claim it makes from inside a detail installs its page
/// and leaves the detail behind, while a rebuild that settles after a detail was
/// opened still drops.
#[test]
fn overlay_refresh_applies_a_filter_key_pressed_inside_a_detail_view() {
    let mut service = test_runtime_service();
    let primary = open_saved_session_picker(&mut service, "overlay-refresh-detail-key", 45);
    service
        .apply_primary_display_overlay_input(&primary, b"i")
        .unwrap();
    assert!(
        saved_session_detail_open(&service),
        "the focused row opens its detail view"
    );
    service
        .apply_primary_display_overlay_input(&primary, b"u")
        .unwrap();
    assert!(
        service
            .run_pending_record_browser_refresh_for_tests()
            .unwrap(),
        "the key pressed inside the detail claims its page"
    );
    assert!(
        !saved_session_detail_open(&service),
        "the settled page replaces the detail the key was pressed in"
    );

    // Paging behaves like the filter keys: a page-edge step taken from inside a
    // detail owns the page it fetches instead of being discarded.
    move_saved_session_cursor_to_last_row(&mut service, &primary);
    service
        .apply_primary_display_overlay_input(&primary, b"i")
        .unwrap();
    assert!(saved_session_detail_open(&service));
    service
        .apply_primary_display_overlay_input(&primary, b"\x1b[B")
        .unwrap();
    assert!(
        service
            .run_pending_record_browser_refresh_for_tests()
            .unwrap(),
        "the page-edge step taken inside the detail claims its page"
    );
    assert!(
        !saved_session_detail_open(&service),
        "the fetched page replaces the detail the paging key was pressed in"
    );
}

/// Verifies a memory-browser claim rebuilds its store-backed page.
///
/// The memories family reads the persistent store through the same static reader
/// the inline path used, so a settled claim carries the records the store holds.
#[test]
fn overlay_refresh_rebuilds_a_memory_page() {
    let service = test_runtime_service();
    let config_root = temp_root("overlay-refresh-memories");
    crate::storage::memory::PersistentMemoryStore::under_config_root(&config_root)
        .upsert(mez_agent::memory::MemoryRecord::new_with_defaults(
            "memory-claim",
            mez_agent::memory::MemoryScope::Global,
            10,
            10,
            mez_agent::memory::MemorySource::Agent,
            50,
            "claimable memory body",
        ))
        .unwrap();
    let source = RuntimeRecordBrowserOverlaySource::Memories {
        scope: None,
        default_scope: None,
        kind: None,
        state: None,
        text: None,
        limit: 20,
    };
    let work = crate::runtime::RuntimeRecordBrowserRefreshWork {
        refresh_key: "%1".to_string(),
        generation: 0,
        active_source: source.clone(),
        source: source.clone(),
        intent: crate::runtime::RuntimeRecordBrowserRefreshIntent::ApplyFilter {
            target: Box::new(source),
            active_record_id: None,
            active_index: None,
            replaces_detail: false,
            error: None,
        },
        transcript_store: None,
        config_root: Some(config_root.clone()),
        issue_database_path: None,
        prompt_width: 40,
        title_policy: service.agent_session_title_policy(),
    };
    let outcome = RuntimeSessionService::execute_record_browser_refresh(&work);
    let RuntimeRecordBrowserRefreshOutcome::Rebuilt { browser, .. } = outcome else {
        panic!("the claim rebuilds the memory page");
    };
    assert_eq!(browser.records().len(), 1);
    assert!(
        browser
            .render_page()
            .raw_markdown
            .contains("claimable memory body"),
        "the rebuilt page carries the stored memory"
    );
    let _ = std::fs::remove_dir_all(&config_root);
}
