//! Peer-source replay retains direction, label, eligibility, and corruption guards.
//!
//! Live and durable rows share exact marker and payload semantics. Corrupted
//! records must never become invented assistant output or unbounded rendering.

use super::*;

/// Verifies logged peer and parent lines colorize their name markers only.
///
/// The marker is the bounded name plus its received-message glyph. The payload
/// keeps the terminal's default color and the span adds no display cells, so a
/// received peer line and a parent prompt retain their distinct markers without
/// changing line text or wrapping.
#[test]
fn runtime_agent_peer_and_parent_lines_colorize_name_markers() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(40, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
    );

    service
        .append_agent_received_peer_message_to_terminal_buffer(
            "%1",
            "agent-%3",
            "text/plain; charset=utf-8",
            "check cwd",
        )
        .unwrap();
    service
        .append_agent_received_direct_parent_message_to_terminal_buffer(
            "%1",
            "text/plain; charset=utf-8",
            "direct parent evidence",
        )
        .unwrap();
    service
        .append_agent_received_peer_message_to_terminal_buffer(
            "%1",
            "parent",
            "text/plain; charset=utf-8",
            "ordinary peer evidence",
        )
        .unwrap();
    service
        .append_agent_parent_prompt_to_terminal_buffer("%1", "restore parent")
        .unwrap();

    let styled_lines = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    assert_ne!(
        service.ui_theme().colors.agent_transcript_parent.foreground,
        service.ui_theme().colors.agent_transcript_error.foreground,
        "the parent marker must not reuse the transcript error foreground"
    );
    let gutter = "│ ".chars().count();
    for (text, marker, pair) in [
        (
            "│ agent-%3> check cwd",
            "agent-%3>",
            service.ui_theme().colors.agent_transcript_peer_sender,
        ),
        (
            "│ parent> direct parent evidence",
            "parent>",
            service.ui_theme().colors.agent_transcript_parent,
        ),
        (
            "│ parent> ordinary peer evidence",
            "parent>",
            service.ui_theme().colors.agent_transcript_peer_sender,
        ),
        (
            "│ parent> restore parent",
            "parent>",
            service.ui_theme().colors.agent_transcript_parent,
        ),
    ] {
        let line = styled_lines
            .iter()
            .find(|line| line.text == text)
            .unwrap_or_else(|| panic!("missing styled line {text:?}: {styled_lines:#?}"));
        let marker_end = gutter + marker.chars().count();
        assert!(
            line.style_spans.iter().any(|span| {
                span.start == gutter
                    && span.length == marker.chars().count()
                    && span.rendition.foreground == Some(pair.foreground)
                    && span.rendition.background.is_none()
            }),
            "{text:?} must colorize only its name marker: {:?}",
            line.style_spans
        );
        assert!(
            line.style_spans.iter().all(|span| {
                span.start.saturating_add(span.length) <= marker_end
                    || span.rendition.foreground != Some(pair.foreground)
            }),
            "{text:?} must not carry the name-marker color past the marker: {:?}",
            line.style_spans
        );
    }
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a logged received peer message persists its peer name and payload
/// so a presentation replay rebuilds the byte-identical prompt-style line.
///
/// A restart must not lose the originating agent: the peer content type stores a
/// JSON record instead of user-prompt text, so replay prints the same `{peer}> `
/// prefix at the destination geometry rather than a nameless or re-trusted
/// line. The stored record keeps the unbounded peer payload, so a
/// payload above the peer-context bound is bounded once, at render time, and the
/// live and replayed rows stay byte-identical, truncation marker included.
///
/// The stored media type keeps canonical plaintext filtering reproducible: a
/// suppressed non-plaintext payload leaves no record behind for replay to
/// resurrect.
#[test]
fn runtime_agent_peer_message_persists_source_for_replay() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-peer-message-source"));
    service
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

    // The live rows are captured from this fresh screen so the comparison below
    // isolates the peer echo instead of pane process output.
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
    );
    // A payload above the peer-context bound: the bound has to be applied once,
    // at render time, so the replayed line carries the same truncation marker.
    let large_payload = "peer payload segment ".repeat(16_000);
    assert!(
        large_payload.len() > 256 * 1024,
        "the payload must exceed the peer-context bound: {}",
        large_payload.len()
    );

    service
        .append_agent_received_peer_message_to_terminal_buffer(
            "%1",
            "agent-%3",
            "text/plain; charset=utf-8",
            large_payload.as_str(),
        )
        .unwrap();
    service
        .append_agent_received_peer_message_to_terminal_buffer(
            "%1",
            "agent-%3",
            "text/plain; charset=utf-8",
            "check the pane cwd",
        )
        .unwrap();
    service
        .append_agent_received_direct_parent_message_to_terminal_buffer(
            "%1",
            "text/plain; charset=utf-8",
            "direct parent replay evidence",
        )
        .unwrap();
    // JSON payloads, including runtime bridge traffic and a model-authored
    // result payload, remain presentation-silent in normal mode.
    service
        .append_agent_received_peer_message_to_terminal_buffer(
            "%1",
            "agent-%3",
            "application/json",
            r#"{"task_id":"task-10","state":"running","progress_percent":0,"summary":"working"}"#,
        )
        .unwrap();
    service
        .append_agent_received_peer_message_to_terminal_buffer(
            "%1",
            "agent-%3",
            "application/json",
            r#"{"task_id":"task-9","success":true,"summary":"done","output":{"rows":41,"ok":true}}"#,
        )
        .unwrap();
    let live_rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    let live_text = live_rows
        .join("\n")
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    for suppressed in ["rows41oktrue", "task9", "task10", "success", "working"] {
        assert!(
            !live_text.contains(suppressed),
            "normal mode suppresses JSON presentation, so {suppressed} must not reach the log: {live_text}"
        );
    }
    let live_styled_rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let entries = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    assert_eq!(
        entries.len(),
        3,
        "each suppressed JSON payload writes no presentation record: {entries:?}"
    );
    assert!(
        entries.iter().all(|entry| {
            !entry
                .source_text
                .as_deref()
                .is_some_and(|source| source.contains("task-10") || source.contains("task-9"))
        }),
        "suppressed JSON payloads must not be persisted for replay: {entries:#?}"
    );
    assert!(
        entries.iter().all(|entry| entry
            .source_content_type
            .as_deref()
            .is_some_and(|content_type| content_type.contains("peer-message+json"))),
        "{entries:?}"
    );
    let received_source = entries
        .iter()
        .find_map(|entry| {
            let source = entry.source_text.as_deref()?;
            source.contains("check the pane cwd").then_some(source)
        })
        .expect("received peer presentation source");
    assert!(
        received_source.contains("\"direction\":\"received\""),
        "{received_source}"
    );
    assert!(received_source.contains("agent-%3"), "{received_source}");
    let direct_parent_source = entries
        .iter()
        .find_map(|entry| {
            let source = entry.source_text.as_deref()?;
            source
                .contains("direct parent replay evidence")
                .then_some(source)
        })
        .expect("direct-parent peer presentation source");
    assert!(
        direct_parent_source.contains("\"peer\":\"parent\""),
        "{direct_parent_source}"
    );
    assert!(
        direct_parent_source.contains("\"direct_parent\":true"),
        "{direct_parent_source}"
    );
    let large_source = entries
        .iter()
        .find_map(|entry| {
            let source = entry.source_text.as_deref()?;
            (source.len() > large_payload.len()).then_some(source)
        })
        .expect("peer presentation source carrying the unbounded payload");
    assert!(
        !large_source.contains("truncated; original_bytes="),
        "the persisted peer source must keep the unbounded payload: {}",
        large_source.len()
    );

    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
    );
    assert!(
        service
            .rebuild_agent_presentation_after_resize("%1", Size::new(40, 12).unwrap())
            .unwrap()
    );
    let replayed = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    // Replay may rebuild a prefix from earlier persisted entries, so the live
    // rows must reappear byte-identical as the trailing rows of the rebuild.
    assert_eq!(
        live_rows.as_slice(),
        &replayed[replayed.len().saturating_sub(live_rows.len())..],
        "a live peer line and its replayed line must be byte-identical"
    );
    assert!(
        replayed
            .iter()
            .any(|line| line == "│ agent-%3> check the pane cwd"),
        "{replayed:#?}"
    );
    assert!(
        replayed
            .iter()
            .any(|line| line == "│ parent> direct parent replay evidence"),
        "{replayed:#?}"
    );
    // Replay re-derives each received-message marker from the persisted label,
    // so the replayed rows must keep the live spans exactly.
    let replayed_styled_rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    assert_ne!(
        service.ui_theme().colors.agent_transcript_parent.foreground,
        service.ui_theme().colors.agent_transcript_error.foreground,
        "the replayed parent marker must not reuse the transcript error foreground"
    );
    assert_eq!(
        live_styled_rows.as_slice(),
        &replayed_styled_rows[replayed_styled_rows
            .len()
            .saturating_sub(live_styled_rows.len())..],
        "a live peer line and its replayed line must keep identical name-marker spans"
    );
    let received_marker = replayed_styled_rows
        .iter()
        .find(|line| line.text == "│ agent-%3> check the pane cwd")
        .expect("replayed received peer line");
    assert!(
        received_marker.style_spans.iter().any(|span| {
            span.start == "▐ ".chars().count()
                && span.length == "agent-%3>".chars().count()
                && span.rendition.foreground
                    == Some(
                        service
                            .ui_theme()
                            .colors
                            .agent_transcript_peer_sender
                            .foreground,
                    )
                && span.rendition.background.is_none()
        }),
        "{received_marker:?}"
    );
    let direct_parent_marker = replayed_styled_rows
        .iter()
        .find(|line| line.text == "│ parent> direct parent replay evidence")
        .expect("replayed direct-parent peer line");
    assert!(
        direct_parent_marker.style_spans.iter().any(|span| {
            span.start == "▐ ".chars().count()
                && span.length == "parent>".chars().count()
                && span.rendition.foreground
                    == Some(service.ui_theme().colors.agent_transcript_parent.foreground)
                && span.rendition.background.is_none()
        }),
        "{direct_parent_marker:?}"
    );
    assert!(
        !replayed
            .iter()
            .any(|line| line.contains("task-10") || line.contains("task-9")),
        "suppressed JSON payloads must stay silent after replay: {replayed:#?}"
    );
    let replayed_text = replayed
        .join("\n")
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    assert!(
        replayed_text.contains("originalbytes"),
        "the replayed line must carry the peer-context truncation marker: {}",
        replayed_text.len()
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies legacy peer records without media types stay suppressed in normal
/// mode while verbose replay preserves their bounded raw payloads.
#[test]
fn runtime_agent_legacy_peer_message_record_still_replays() {
    let mut service = test_runtime_service();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
    );
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let legacy_peer_entry =
        |sequence: u64, source: &str| crate::storage::transcript::AgentPresentationEntry {
            conversation_id: conversation_id.clone(),
            sequence,
            created_at_unix_seconds: 1,
            pane_id: "%1".to_string(),
            turn_id: None,
            terminal_width: 40,
            style_names: vec!["user-prompt".to_string()],
            display_lines: vec![source.to_string()],
            copy_lines: Vec::new(),
            ansi_text: None,
            source_text: Some(source.to_string()),
            source_content_type: Some(
                "application/vnd.mezzanine.agent-presentation.peer-message+json; charset=utf-8"
                    .to_string(),
            ),
        };
    // Neither record carries the optional media-type field, exactly like a peer
    // presentation record written before canonical plaintext filtering existed.
    let entries = vec![
        legacy_peer_entry(
            1,
            r#"{"direction":"received","peer":"agent-%3","payload":"legacy peer evidence"}"#,
        ),
        legacy_peer_entry(
            2,
            r#"{"direction":"sent","peer":"agent-%2","payload":"legacy ack"}"#,
        ),
    ];

    assert!(
        service
            .replay_agent_presentation_entries_to_terminal_buffer("%1", &entries)
            .unwrap()
    );
    let rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    assert!(
        !rows
            .iter()
            .any(|line| line.contains("legacy peer evidence")),
        "normal mode suppresses legacy records without media types: {rows:#?}"
    );
    assert!(
        !rows.iter().any(|line| line.contains("legacy ack")),
        "normal mode suppresses legacy records without media types: {rows:#?}"
    );
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "verbose-peer-replay".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\npeer_message_log_mode = \"verbose\"\n".to_string(),
        }])
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
    );
    assert!(
        service
            .replay_agent_presentation_entries_to_terminal_buffer("%1", &entries)
            .unwrap()
    );
    let verbose_rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    assert!(
        verbose_rows
            .iter()
            .any(|line| line == "│ agent-%3> legacy peer evidence"),
        "verbose replay preserves legacy received payloads: {verbose_rows:#?}"
    );
    assert!(
        !verbose_rows.iter().any(|line| line.contains("legacy ack")),
        "verbose replay skips legacy sent records: {verbose_rows:#?}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies malformed, unknown-direction, and oversized stored peer sources are
/// skipped instead of panicking or rendering corrupt log state as transcript.
///
/// The peer content type is written only by the peer echo writer, so a record
/// that does not decode is damaged presentation state. Replay must drop that one
/// line, never invent assistant output from its raw bytes, and never allocate or
/// render from an untrusted stored length. The oversized case is valid JSON above
/// the decode guard, so it is skipped only because the guard is enforced.
#[test]
fn runtime_agent_peer_message_replay_skips_malformed_source() {
    let mut service = test_runtime_service();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
    );
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let peer_content_type =
        "application/vnd.mezzanine.agent-presentation.peer-message+json; charset=utf-8";
    let peer_entry =
        |sequence: u64, source: String| crate::storage::transcript::AgentPresentationEntry {
            conversation_id: conversation_id.clone(),
            sequence,
            created_at_unix_seconds: 1,
            pane_id: "%1".to_string(),
            turn_id: None,
            terminal_width: 40,
            style_names: vec!["user-prompt".to_string()],
            display_lines: vec![source.clone()],
            copy_lines: Vec::new(),
            ansi_text: None,
            source_text: Some(source),
            source_content_type: Some(peer_content_type.to_string()),
        };
    // A syntactically valid record larger than the decode guard. Its payload
    // would render as a visible peer line if the guard were removed, so the
    // skip cannot be a side effect of JSON parsing.
    let oversize_source = serde_json::json!({
        "direction": "received",
        "peer": "agent-%9",
        "payload": "oversize peer payload ".repeat(200_000),
    })
    .to_string();
    assert!(
        oversize_source.len() > 4 * 1024 * 1024,
        "the oversize case must exceed the decode guard: {}",
        oversize_source.len()
    );
    let entries = vec![
        peer_entry(1, "{\"direction\":\"received\"".to_string()),
        peer_entry(
            2,
            "{\"direction\":\"sideways\",\"peer\":\"agent-%3\",\"payload\":\"bad direction\"}"
                .to_string(),
        ),
        peer_entry(3, oversize_source),
    ];

    assert!(
        service
            .replay_agent_presentation_entries_to_terminal_buffer("%1", &entries)
            .unwrap()
    );
    let rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    assert!(
        rows.iter().all(|line| line.trim().is_empty()),
        "undecodable peer sources must not render: {rows:#?}"
    );
    assert!(
        rows.iter().all(|line| !line.contains("agent-%9")),
        "an oversize peer source must be skipped instead of rendered: {rows:#?}"
    );
}
