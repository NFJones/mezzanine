//! Projection acceptance is independent of physical redraw intent.
//!
//! Candidates are rendered from real current outbound source and a height-only
//! resize. Whole-screen equality is a prerequisite, never permission to bypass
//! ownership, geometry, settings or intervening-write rejection.

use super::*;
use crate::runtime::render::RuntimeStreamingSayProjectionApplyOutcome as Outcome;

/// Builds a real current metadata-dirty outbound projection whose complete
/// screen is already installed after an admitted height-only resize.
fn current_outbound_noop_work() -> (
    RuntimeSessionService,
    crate::runtime::RuntimeStreamingSayProjectionWork,
    String,
) {
    let mut service = test_runtime_service();
    let conversation = service.agent_shell_store_mut().enter_or_resume("%1").unwrap().session_id.clone();
    set_agent_pane_screen_for_test(&mut service, "%1", TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap());
    for event in [
        mez_agent::StreamingSayEvent::MessageStarted { action_index: 0, recipient: "agent:agent-%2".into(), content_type: "text/plain; charset=utf-8".into() },
        mez_agent::StreamingSayEvent::MessagePayloadDelta { action_index: 0, text: "current outbound source".into() },
        mez_agent::StreamingSayEvent::MessagePayloadComplete { action_index: 0 },
    ] {
        service.apply_agent_streaming_say_event_to_terminal_buffer("%1", "outcome-turn", &event).unwrap();
    }
    let work = service.take_agent_streaming_say_projection_work("%1", "outcome-turn").unwrap().unwrap();
    let initial = service.agent_pane_screen_lineage("%1", &conversation);
    let outcome = service.apply_agent_streaming_say_projection_outcome(RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap()).unwrap();
    assert_eq!(outcome, Outcome::AcceptedChanged);
    assert!(outcome.is_accepted() && outcome.screen_changed());
    assert_ne!(service.agent_pane_screen_lineage("%1", &conversation), initial);
    assert!(service.apply_pane_resize_completion_event("%1", Size::new(40, 13).unwrap()).unwrap());
    let work = service.take_agent_streaming_say_projection_work("%1", "outcome-turn").unwrap().unwrap();
    let candidate = RuntimeSessionService::build_agent_streaming_say_projection(work.clone()).unwrap();
    assert!(&candidate.screen == service.agent_pane_screen("%1").unwrap(), "the complete candidate, including copy and occurrence metadata, must be identical");
    (service, work, conversation)
}

/// An unchanged acceptance refreshes projection revision/context/action/rationale
/// metadata without screen replacement or lineage advance. The redraw-facing
/// boolean remains false, and metrics count neither install nor rejection.
#[test]
fn runtime_streaming_projection_outcomes_distinguish_noop_from_redraw() {
    let (mut service, work, conversation) = current_outbound_noop_work();
    let screen = service.agent_pane_screen("%1").unwrap().clone();
    let lineage = service.agent_pane_screen_lineage("%1", &conversation);
    let before = service.runtime_metrics().clone();
    let outcome = service.apply_agent_streaming_say_projection_outcome(RuntimeSessionService::build_agent_streaming_say_projection(work.clone()).unwrap()).unwrap();
    assert_eq!(outcome, Outcome::AcceptedUnchanged);
    assert!(outcome.is_accepted());
    assert!(!outcome.screen_changed());
    assert!(service.take_agent_streaming_say_projection_work("%1", "outcome-turn").unwrap().is_none(), "acceptance must commit current reconciliation metadata");
    assert!(!service.apply_agent_streaming_say_projection_result(RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap()).unwrap(), "the existing async redraw contract stays false for accepted no-op");
    assert!(service.agent_pane_screen("%1").unwrap() == &screen);
    assert_eq!(service.agent_pane_screen_lineage("%1", &conversation), lineage);
    let after = service.runtime_metrics();
    assert_eq!(after.agent_streaming_projection_results, before.agent_streaming_projection_results + 2);
    assert_eq!(after.agent_streaming_projection_installs, before.agent_streaming_projection_installs);
    assert_eq!(after.agent_streaming_projection_rejections, before.agent_streaming_projection_rejections);
    assert_eq!(after.agent_streaming_projection_lineage_rejections, before.agent_streaming_projection_lineage_rejections);
}

/// Even a complete identical candidate must reject incompatible captured fields
/// before equality. Real source, settings, geometry, ABA and durable-write
/// transitions also revoke old authority without changing intervening state or
/// committing stale reconciliation metadata. No branch executes a send.
#[test]
fn runtime_streaming_projection_outcomes_reject_unsafe_identical_work() {
    for case in ["turn", "response", "conversation", "revision", "thinking", "columns", "frame", "table", "captured-size", "new-source", "resize", "settings", "aba", "write"] {
        let (mut service, work, conversation) = current_outbound_noop_work();
        let mut candidate = RuntimeSessionService::build_agent_streaming_say_projection(work.clone()).unwrap();
        match case {
            "turn" => candidate.turn_id.push_str("-stale"),
            "response" => candidate.response_index += 1,
            "conversation" => candidate.conversation_id.push_str("-stale"),
            "revision" => candidate.revision += 1,
            "thinking" => candidate.thinking_enabled = !candidate.thinking_enabled,
            "columns" => candidate.presentation_columns += 1,
            "frame" => candidate.frame_width += 1,
            "table" => candidate.table_width += 1,
            "captured-size" => candidate.screen_size = Size::new(40, 14).unwrap(),
            "new-source" => service.apply_agent_streaming_say_event_to_terminal_buffer("%1", "outcome-turn", &mez_agent::StreamingSayEvent::RationaleStarted).unwrap(),
            "resize" => { assert!(service.apply_pane_resize_completion_event("%1", Size::new(40, 14).unwrap()).unwrap()); },
            "settings" => super::timestamp_settings::replace_timestamp_settings(&mut service, true, 40, ""),
            "aba" => {
                let same = service.agent_pane_screen("%1").unwrap().clone();
                service.set_agent_pane_screen("%1", conversation.clone(), same);
            },
            "write" => service.append_agent_status_text_to_terminal_buffer("%1", "intervening durable content").unwrap(),
            _ => unreachable!("bounded rejection cases"),
        }
        let screen = service.agent_pane_screen("%1").unwrap().clone();
        let lineage = service.agent_pane_screen_lineage("%1", &conversation);
        let pending = service.take_agent_streaming_say_projection_work("%1", "outcome-turn").unwrap();
        let before = service.runtime_metrics().clone();
        assert_eq!(service.apply_agent_streaming_say_projection_outcome(candidate).unwrap(), Outcome::Rejected, "{case}");
        assert!(service.agent_pane_screen("%1").unwrap() == &screen, "rejection must preserve the entire intervening screen: {case}");
        assert_eq!(service.agent_pane_screen_lineage("%1", &conversation), lineage, "{case}");
        assert_eq!(service.take_agent_streaming_say_projection_work("%1", "outcome-turn").unwrap().map(|work| (work.revision, work.installed_lineage)), pending.map(|work| (work.revision, work.installed_lineage)), "rejection cannot commit stale projection metadata: {case}");
        let after = service.runtime_metrics();
        assert_eq!(after.agent_streaming_projection_results, before.agent_streaming_projection_results + 1, "{case}");
        assert_eq!(after.agent_streaming_projection_rejections, before.agent_streaming_projection_rejections + 1, "{case}");
        assert_eq!(after.agent_streaming_projection_installs, before.agent_streaming_projection_installs, "{case}");
    }
}
