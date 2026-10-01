//! Resize gesture coalescing and asynchronous source-backed presentation replay.
//!
//! Only the newest completed geometry can install a canonical projection.
//! Dragging defers reconstruction until release, and replay never appends source.

use super::*;

/// Verifies pane-divider dragging defers expensive source-backed agent replay
/// until the resize gesture finishes at its final pane size.
///
/// Geometry and terminal sizing must still update during the drag, repeated
/// movement must coalesce into one pending semantic presentation rebuild, and
/// a debounce firing while the pointer remains held must retain that work.
#[test]
fn runtime_agent_divider_drag_debounces_source_backed_presentation_replay() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-drag-resize-source"));
    let primary = service
        .attach_primary("primary", true, Size::new(40, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .append_agent_assistant_text_to_terminal_buffer(
            "%1",
            "# Deferred rebuild\n\nsemantic source uses the final drag width",
        )
        .unwrap();
    assert!(
        service
            .apply_attached_mux_action(&primary, MuxAction::SplitPaneVertical)
            .unwrap()
    );

    let border = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap()
        .mouse_border_cells
        .into_iter()
        .next()
        .expect("vertical split should expose a draggable divider");
    for column in [
        border.column,
        border.column.saturating_add(2),
        border.column.saturating_add(4),
    ] {
        let (_, transition) = service
            .apply_attached_terminal_step_transition(
                &primary,
                &AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::HandleMouse(
                        MouseAction::ResizePane {
                            column,
                            row: border.row,
                        },
                    )],
                    output_lines: Vec::new(),
                    output_line_style_spans: Vec::new(),
                    input_hangup: false,
                    output_hangup: false,
                    error_roles: Vec::new(),
                },
            )
            .unwrap();
        assert_eq!(
            transition.side_effects,
            vec![RuntimeSideEffect::RenderClient {
                client_id: primary.clone(),
                reason: RenderInvalidationReason::ResizeDrag,
            }]
        );
    }

    assert!(
        service
            .presentation
            .agent_presentation_resize_is_deferred("%1")
    );
    let intermediate = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!intermediate.contains("Deferred rebuild"), "{intermediate}");
    let final_size = service.agent_pane_screen("%1").unwrap().size();

    let transition = service
        .apply_resize_debounce_timer_transition(primary.as_str(), true)
        .unwrap();

    assert!(!transition.applied);
    assert!(transition.side_effects.is_empty());
    assert!(
        service
            .presentation
            .agent_presentation_resize_is_deferred("%1")
    );
    assert_eq!(service.agent_pane_screen("%1").unwrap().size(), final_size);
    let still_deferred = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        !still_deferred.contains("Deferred rebuild"),
        "{still_deferred}"
    );

    let (release, release_transition) = service
        .apply_attached_terminal_step_transition(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::FinishResizePane,
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert!(release.view_refresh_required);
    assert!(!release.full_redraw_required);
    assert!(
        release_transition
            .side_effects
            .iter()
            .all(|effect| !matches!(effect, RuntimeSideEffect::RenderClient { .. }))
    );
    assert!(
        service
            .presentation
            .agent_presentation_resize_is_deferred("%1")
    );
    let commit = service
        .apply_resize_debounce_timer_transition(primary.as_str(), true)
        .unwrap();
    assert!(commit.applied);
    assert!(commit.side_effects.iter().any(|effect| matches!(
        effect,
        RuntimeSideEffect::RenderClient {
            reason: RenderInvalidationReason::FullRedraw,
            ..
        }
    )));
    assert!(commit.side_effects.iter().any(|effect| matches!(
        effect,
        RuntimeSideEffect::DispatchAgentPresentationResize { pane_id, .. }
            if pane_id == "%1"
    )));
    let work = service
        .take_agent_presentation_resize_work("%1")
        .unwrap()
        .expect("released drag should expose one canonical resize generation");
    let result = RuntimeSessionService::build_agent_presentation_resize(work)
        .unwrap()
        .expect("semantic source should build a canonical resize generation");
    assert!(
        service
            .apply_agent_presentation_resize_result(result)
            .unwrap()
    );
    assert!(
        !service
            .presentation
            .agent_presentation_resize_is_deferred("%1")
    );
    let rebuilt = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n")
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    assert!(rebuilt.contains("Deferredrebuild"), "{rebuilt}");
    assert!(
        rebuilt.contains("semanticsourceusesthefinaldragwidth"),
        "{rebuilt}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies an asynchronous PTY resize completion rebuilds source-backed agent
/// presentation instead of resizing the stale terminal-cell projection.
#[test]
fn runtime_agent_async_resize_completion_rebuilds_source_backed_presentation() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-async-resize-source"));
    service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    transcript_store
        .append_presentation(&crate::storage::transcript::AgentPresentationEntry {
            conversation_id,
            sequence: 1,
            created_at_unix_seconds: 1,
            pane_id: "%1".to_string(),
            turn_id: None,
            terminal_width: 28,
            style_names: vec!["assistant".to_string()],
            display_lines: vec!["mez> stale async projection".to_string()],
            copy_lines: vec!["stale async projection".to_string()],
            ansi_text: None,
            source_text: Some("# Async rebuild\n\nsource survives completion resize".to_string()),
            source_content_type: Some("text/markdown; charset=utf-8".to_string()),
        })
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(28, 12).unwrap(), 120).unwrap(),
    );

    assert!(
        service
            .apply_pane_resize_completion_event("%1", Size::new(20, 12).unwrap())
            .unwrap()
    );

    let provisional = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!provisional.contains("Async rebuild"), "{provisional}");
    let work = service
        .take_agent_presentation_resize_work("%1")
        .unwrap()
        .expect("resize completion should expose one canonical resize generation");
    let result = RuntimeSessionService::build_agent_presentation_resize(work)
        .unwrap()
        .expect("semantic source should build a canonical resize generation");
    assert!(
        service
            .apply_agent_presentation_resize_result(result)
            .unwrap()
    );

    let rebuilt = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n")
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    assert!(rebuilt.contains("Asyncrebuild"), "{rebuilt}");
    assert!(
        rebuilt.contains("sourcesurvivescompletionresize"),
        "{rebuilt}"
    );
    assert!(!rebuilt.contains("staleasyncprojection"), "{rebuilt}");
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a stale adapter-owned resize completion cannot overwrite the
/// newest queued pane geometry or its source-backed agent projection.
#[test]
fn runtime_agent_ignores_superseded_async_resize_completion() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-stale-async-resize"));
    let primary = service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let _process = service.take_running_pane_process_for_adapter("%1").unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .append_agent_assistant_text_to_terminal_buffer(
            "%1",
            "semantic projection survives only the newest resize completion",
        )
        .unwrap();

    service
        .resize_attached_primary_terminal(&primary, Size::new(24, 12).unwrap())
        .unwrap();
    let stale_size = service
        .drain_pane_io_transition()
        .side_effects
        .into_iter()
        .find_map(|effect| match effect {
            RuntimeSideEffect::PaneProcessIo {
                effect: crate::runtime::PaneProcessIoEffect::Resize { size },
                ..
            } => Some(size),
            _ => None,
        })
        .unwrap();
    service
        .resize_attached_primary_terminal(&primary, Size::new(20, 12).unwrap())
        .unwrap();
    let newest_size = service
        .drain_pane_io_transition()
        .side_effects
        .into_iter()
        .find_map(|effect| match effect {
            RuntimeSideEffect::PaneProcessIo {
                effect: crate::runtime::PaneProcessIoEffect::Resize { size },
                ..
            } => Some(size),
            _ => None,
        })
        .unwrap();
    assert_ne!(stale_size, newest_size);
    assert!(
        !service
            .apply_pane_resize_completion_event("%1", stale_size)
            .unwrap()
    );
    assert!(
        service
            .apply_pane_resize_completion_event("%1", newest_size)
            .unwrap()
    );
    assert_eq!(service.pane_screen("%1").unwrap().size(), newest_size);
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies provider-produced Markdown tables persist their semantic source and
/// redraw through the attached client after a production resize path changes
/// the pane geometry.
#[test]
fn runtime_provider_markdown_table_persists_and_reprojects_after_resize() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("provider-table-projection"));
    let primary = service
        .attach_primary("primary", true, Size::new(48, 16).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"provider-table","method":"agent/shell/command","params":{"idempotency_key":"provider-table","input":"render a wide table"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let table = "| Component | Durable projection detail |\n| --- | --- |\n| renderer | semantic table cells reflow at the destination pane width |\n| resume | persisted source redraws after restoring a conversation |";
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: table.to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "render the requested table".to_string(),

                actions: vec![mez_agent::AgentAction {
                    id: "say-table".to_string(),

                    payload: mez_agent::AgentActionPayload::Say {
                        status: mez_agent::SayStatus::Final,
                        text: table.to_string(),
                        content_type: mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE
                            .to_string(),
                    },
                }],
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let entries = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    assert!(
        entries.iter().any(|entry| {
            entry.source_text.as_deref() == Some(table)
                && entry.source_content_type.as_deref()
                    == Some(mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE)
        }),
        "{entries:?}"
    );

    let wide_projection = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    service
        .resize_attached_primary_terminal(&primary, Size::new(24, 16).unwrap())
        .unwrap();
    let narrow_projection = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    assert_ne!(
        wide_projection, narrow_projection,
        "wide={wide_projection:?} narrow={narrow_projection:?}"
    );
    assert!(
        narrow_projection.iter().any(|line| line.contains('│')),
        "{narrow_projection:?}"
    );
    service
        .resize_attached_primary_terminal(&primary, Size::new(48, 16).unwrap())
        .unwrap();
    assert_eq!(
        transcript_store
            .inspect_presentation(&conversation_id)
            .unwrap()
            .len(),
        entries.len()
    );
    service.terminate_all_pane_processes().unwrap();
}
