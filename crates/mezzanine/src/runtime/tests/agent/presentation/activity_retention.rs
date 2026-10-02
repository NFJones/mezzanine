//! Bounded long-history activity preparation and production reflow measurements.
//!
//! Timings describe this executable synthetic fixture, not terminal usability or
//! a platform-independent latency guarantee. The enclosing test timeout checks
//! hangs; semantic assertions pin retention and navigation independently of load.

use super::*;
use crate::storage::transcript::activity::{
    ACTIVITY_CONTENT_TYPE, ActivityComponentKind, ActivitySource,
};
use std::time::Instant;

/// A thousand durable components produce only the bounded 200-record snapshot.
/// Repeated production overlay reflow operates on that retained snapshot and
/// preserves selection/export bytes without rereading or appending history.
#[test]
fn activity_long_history_has_bounded_snapshot_and_stable_reflow() {
    let store = AgentTranscriptStore::new(temp_root("activity-retention-measurement"))
        .with_presentation_compaction_threshold(16 * 1024 * 1024)
        .unwrap();
    let entries = (1..=1000)
        .map(|sequence| {
            let source = ActivitySource {
                version: 1,
                conversation_id: "history".into(),
                turn_id: "turn".into(),
                response_id: "response".into(),
                action_id: Some(format!("action-{sequence}")),
                action_ordinal: Some(sequence as usize),
                transaction: None,
                mutation: None,
                kind: ActivityComponentKind::Result,
                status: "succeeded".into(),
                content_type: "text/plain; charset=utf-8".into(),
                source: format!(
                    "Retained component {sequence}: 雪 {}",
                    "bounded source ".repeat(8)
                ),
                preview_source: None,
                intent: Default::default(),
            };
            crate::storage::transcript::AgentPresentationEntry {
                conversation_id: "history".into(),
                sequence,
                created_at_unix_seconds: 1,
                pane_id: "%1".into(),
                turn_id: Some("turn".into()),
                terminal_width: 80,
                style_names: vec!["status".into()],
                display_lines: vec!["preview".into()],
                copy_lines: Vec::new(),
                ansi_text: None,
                source_text: Some(source.encode().unwrap()),
                source_content_type: Some(ACTIVITY_CONTENT_TYPE.into()),
            }
        })
        .collect::<Vec<_>>();
    store.append_presentation_many(&entries).unwrap();
    let started = Instant::now();
    let read = crate::runtime::commands::read_context_browser_for_command(
        &store,
        "history",
        "%2",
        "/show-context activity",
    )
    .unwrap();
    let preparation = started.elapsed();
    assert_eq!(read.browser.records().len(), 200);
    assert_eq!(read.browser.records().first().unwrap().id, "801");
    assert_eq!(read.browser.records().last().unwrap().id, "1000");
    let mut browser = read.browser;
    browser.set_active_record_id("950");
    let expected_export = browser
        .apply_action(mez_mux::record_browser::RecordBrowserAction::CopyActive)
        .unwrap();
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.register_pending_record_browser_overlay("%1", "show-context", browser, None);
    let response = crate::runtime::runtime_agent_shell_command_response_json(
        "%1",
        "/show-context activity",
        Some(&crate::runtime::AgentShellCommandOutcome::Display {
            command: "show-context".into(),
            body: read.markdown,
        }),
    );
    service
        .set_agent_prompt_response_display_output_for_tests("%1", &response)
        .unwrap();
    let started = Instant::now();
    for _ in 0..20 {
        service.reflow_primary_record_browser_overlay();
    }
    let reflow = started.elapsed();
    let mut retained = service
        .primary_display_overlay()
        .unwrap()
        .record_browser
        .as_ref()
        .unwrap()
        .browser
        .clone();
    assert_eq!(retained.records().len(), 200);
    assert_eq!(retained.active_record_id(), Some("950"));
    assert_eq!(
        retained
            .apply_action(mez_mux::record_browser::RecordBrowserAction::CopyActive)
            .unwrap(),
        expected_export
    );
    assert_eq!(store.inspect_presentation("history").unwrap().len(), 1000);
    eprintln!(
        "activity fixture: history=1000 snapshot=200 preparation={preparation:?} reflow_20={reflow:?}"
    );
}
