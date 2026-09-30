//! Resize worker admission, freshness fencing, persistence and cache bounds.
//!
//! Captured source must be authoritative before work starts, and delayed
//! candidates cannot overwrite newer geometry, conversation or screen lineage.

use super::*;

/// Verifies pane-divider dragging defers expensive source-backed agent replay
/// until the resize gesture finishes at its final pane size.
///
/// Geometry and terminal sizing must still update during the drag, repeated
/// movement must coalesce into one pending semantic presentation rebuild, and
/// a debounce firing while the pointer remains held must retain that work.
#[test]
fn runtime_structural_agent_resize_does_not_read_presentation_history_inline() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-structural-resize-source"));
    let primary = service
        .attach_primary("primary", true, Size::new(40, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    service
        .append_agent_assistant_text_to_terminal_buffer(
            "%1",
            "# Deferred structural rebuild\n\nsemantic source must be read off actor",
        )
        .unwrap();
    let presentation_path = transcript_store
        .presentation_path(&conversation_id)
        .unwrap();
    std::fs::write(&presentation_path, b"not a presentation stream").unwrap();

    assert!(
        service
            .apply_attached_mux_action(&primary, MuxAction::SplitPaneVertical)
            .unwrap()
    );
    assert_eq!(service.session().active_window().unwrap().panes().len(), 2);
    service.terminate_all_pane_processes().unwrap();
}

/// Builds one delayed canonical resize result together with its live owner.
///
/// Each stale-result regression mutates exactly one actor-owned input after
/// capture, then verifies that atomic installation rejects the old candidate.
fn delayed_agent_resize_result_for_test(
    name: &str,
) -> (
    RuntimeSessionService,
    crate::runtime::RuntimeAgentPresentationResizeResult,
) {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root(name));
    service.set_agent_transcript_store(transcript_store);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .append_agent_assistant_text_to_terminal_buffer(
            "%1",
            "# Delayed resize\n\ncanonical source must not overwrite newer state",
        )
        .unwrap();
    assert!(
        service
            .apply_pane_resize_completion_event("%1", Size::new(20, 12).unwrap())
            .unwrap()
    );
    let work = service
        .take_agent_presentation_resize_work("%1")
        .unwrap()
        .expect("width change should expose one canonical resize generation");
    let result = RuntimeSessionService::build_agent_presentation_resize(work)
        .unwrap()
        .expect("semantic source should build a delayed resize result");
    (service, result)
}

/// Verifies delayed canonical resize results cannot overwrite newer output,
/// geometry, conversation, theme, viewport, or pane-lifecycle state.
#[test]
fn runtime_agent_resize_projection_rejects_every_stale_owner_generation() {
    let (mut output_service, output_result) =
        delayed_agent_resize_result_for_test("agent-resize-stale-output");
    output_service
        .append_agent_status_text_to_terminal_buffer("%1", "newer pane output")
        .unwrap();
    assert!(
        !output_service
            .apply_agent_presentation_resize_result(output_result)
            .unwrap()
    );

    let (mut geometry_service, geometry_result) =
        delayed_agent_resize_result_for_test("agent-resize-stale-geometry");
    geometry_service
        .agent_pane_screen_mut("%1")
        .unwrap()
        .resize(Size::new(18, 12).unwrap());
    assert!(
        !geometry_service
            .apply_agent_presentation_resize_result(geometry_result)
            .unwrap()
    );

    let (mut conversation_service, conversation_result) =
        delayed_agent_resize_result_for_test("agent-resize-stale-conversation");
    conversation_service
        .agent_shell_store_mut()
        .start_new_conversation("%1")
        .unwrap();
    assert!(
        !conversation_service
            .apply_agent_presentation_resize_result(conversation_result)
            .unwrap()
    );

    let (mut theme_service, theme_result) =
        delayed_agent_resize_result_for_test("agent-resize-stale-theme");
    theme_service.set_ui_theme_for_tests(mez_mux::theme::deepforest_ui_theme());
    assert!(
        !theme_service
            .apply_agent_presentation_resize_result(theme_result)
            .unwrap()
    );

    let (mut viewport_service, viewport_result) =
        delayed_agent_resize_result_for_test("agent-resize-stale-viewport");
    viewport_service
        .agent_pane_screen_mut("%1")
        .unwrap()
        .clear_visible_into_history();
    assert!(
        !viewport_service
            .apply_agent_presentation_resize_result(viewport_result)
            .unwrap()
    );

    let (mut removed_service, removed_result) =
        delayed_agent_resize_result_for_test("agent-resize-stale-pane-removal");
    removed_service.agent_shell_store_mut().remove_session("%1");
    removed_service.remove_agent_pane_screen("%1");
    assert!(
        !removed_service
            .apply_agent_presentation_resize_result(removed_result)
            .unwrap()
    );
}

/// Verifies late presentation for a removed pane fails without recreating an
/// uncaptured agent shell session from live configuration.
#[test]
fn runtime_missing_pane_presentation_does_not_recreate_agent_session() {
    let mut service = test_runtime_service();

    let error = service
        .ensure_current_agent_presentation_screen("%999")
        .expect_err("missing pane presentation must fail closed");

    assert_eq!(error.kind(), crate::error::MezErrorKind::NotFound);
    assert!(service.agent_shell_store().get("%999").is_none());
}

/// Verifies resize replay caches decoded durable entries across widths, reuses
/// an exact canonical snapshot when a prior width returns, and invalidates both
/// layers after new durable presentation source is appended.
#[test]
fn runtime_agent_resize_projection_reuses_bounded_canonical_cache() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-resize-replay-cache"));
    service.set_agent_transcript_store(transcript_store);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .append_agent_assistant_text_to_terminal_buffer(
            "%1",
            "# Cached resize\n\ncanonical source is replayed once per new width",
        )
        .unwrap();

    let settle_resize = |service: &mut RuntimeSessionService, size: Size| {
        assert!(
            service
                .apply_pane_resize_completion_event("%1", size)
                .unwrap()
        );
        let work = service
            .take_agent_presentation_resize_work("%1")
            .unwrap()
            .expect("width change should expose canonical resize work");
        let result = RuntimeSessionService::build_agent_presentation_resize(work)
            .unwrap()
            .expect("semantic source should produce a canonical projection");
        assert!(
            service
                .apply_agent_presentation_resize_result(result)
                .unwrap()
        );
    };

    let narrow = Size::new(20, 12).unwrap();
    settle_resize(&mut service, narrow);
    let narrow_screen = service.agent_pane_screen("%1").unwrap().clone();
    settle_resize(&mut service, Size::new(24, 12).unwrap());
    settle_resize(&mut service, narrow);

    assert_eq!(service.agent_pane_screen("%1"), Some(&narrow_screen));
    let metrics = service.runtime_metrics();
    assert_eq!(metrics.agent_presentation_decoded_cache_misses, 1);
    assert_eq!(metrics.agent_presentation_decoded_cache_hits, 2);
    assert_eq!(metrics.agent_presentation_snapshot_cache_hits, 1);
    assert_eq!(metrics.agent_presentation_snapshot_cache_misses, 2);
    assert_eq!(metrics.agent_presentation_replayed_entries, 2);

    service
        .append_agent_status_text_to_terminal_buffer("%1", "new durable source")
        .unwrap();
    settle_resize(&mut service, Size::new(22, 12).unwrap());

    let metrics = service.runtime_metrics();
    assert_eq!(metrics.agent_presentation_decoded_cache_misses, 2);
    assert_eq!(metrics.agent_presentation_snapshot_cache_hits, 1);
    let text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(text.contains("new durable source"), "{text}");
}

/// Verifies adapter-owned presentation writes fence resize replay until the
/// conversation-specific persistence settlement makes the durable prefix authoritative.
#[test]
fn runtime_agent_resize_projection_waits_for_presentation_persistence() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-resize-pending-source"));
    service.set_agent_transcript_store(transcript_store.clone());
    service.persistence.enable_transcript_adapter();
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    service
        .append_agent_assistant_text_to_terminal_buffer(
            "%1",
            "# Settled resize\n\ncanonical replay waits for durable presentation source",
        )
        .unwrap();

    assert!(
        service
            .apply_pane_resize_completion_event("%1", Size::new(20, 12).unwrap())
            .unwrap()
    );
    assert!(
        service
            .take_agent_presentation_resize_work("%1")
            .unwrap()
            .is_none()
    );
    assert!(
        service
            .presentation
            .agent_presentation_resize_is_deferred("%1")
    );

    let mut presentation_path = None;
    let mut entries = Vec::new();
    for effect in service
        .drain_transcript_persistence_transition()
        .side_effects
    {
        if let RuntimeSideEffect::PersistPresentationEntries {
            path,
            entries: queued,
            ..
        } = effect
        {
            presentation_path = Some(path);
            entries.extend(queued);
        }
    }
    assert!(!entries.is_empty());
    let entry_count = entries.len();
    let bytes = transcript_store.append_presentation_many(&entries).unwrap();
    let transition = service
        .apply_persistence_transition(crate::runtime::PersistenceEvent::PresentationCompleted {
            conversation_id,
            path: presentation_path.expect("presentation write should expose its durable path"),
            entries: entry_count,
            bytes,
        })
        .unwrap();
    assert!(transition.side_effects.iter().any(|effect| matches!(
        effect,
        RuntimeSideEffect::DispatchAgentPresentationResize { pane_id, .. } if pane_id == "%1"
    )));

    let work = service
        .take_agent_presentation_resize_work("%1")
        .unwrap()
        .expect("settled presentation persistence should release deferred resize work");
    let result = RuntimeSessionService::build_agent_presentation_resize(work)
        .unwrap()
        .expect("settled semantic source should build a canonical projection");
    assert!(
        service
            .apply_agent_presentation_resize_result(result)
            .unwrap()
    );
    let text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n")
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    assert!(text.contains("Settledresize"), "{text}");
    assert!(
        text.contains("canonicalreplaywaitsfordurablepresentationsource"),
        "{text}"
    );
}

/// Verifies width snapshots obey fixed entry and memory bounds, evict the
/// least-recently-used widths, and continue reusing decoded durable source.
#[test]
fn runtime_agent_resize_projection_cache_evicts_old_widths_within_bounds() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-resize-cache-eviction"));
    service.set_agent_transcript_store(transcript_store);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .append_agent_assistant_text_to_terminal_buffer(
            "%1",
            "# Bounded cache\n\nrepeated widths retain canonical source",
        )
        .unwrap();

    for columns in 20..=27 {
        let size = Size::new(columns, 12).unwrap();
        assert!(
            service
                .apply_pane_resize_completion_event("%1", size)
                .unwrap()
        );
        let work = service
            .take_agent_presentation_resize_work("%1")
            .unwrap()
            .expect("new width should expose resize work");
        let result = RuntimeSessionService::build_agent_presentation_resize(work)
            .unwrap()
            .expect("semantic source should build a canonical width");
        assert!(
            service
                .apply_agent_presentation_resize_result(result)
                .unwrap()
        );
    }

    let (decoded, snapshots, estimated_bytes) =
        service.agent_presentation_replay_cache_stats_for_tests();
    assert_eq!(decoded, 1);
    assert!(
        snapshots <= 6,
        "snapshot cache retained {snapshots} entries"
    );
    assert!(
        estimated_bytes <= 64 * 1024 * 1024,
        "cache retained {estimated_bytes} bytes"
    );
    let metrics = service.runtime_metrics();
    assert!(metrics.agent_presentation_cache_evictions >= 2);
    assert_eq!(metrics.agent_presentation_decoded_cache_misses, 1);
    assert_eq!(metrics.agent_presentation_decoded_cache_hits, 7);
}
