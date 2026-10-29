//! Fresh-screen evidence belongs to exact successfully installed receiver rows.
//!
//! Replay may encounter repeated persisted sources or media suppressed by current
//! policy. Only actually installed received identities can settle the fresh
//! screen obligation, independently of equal text or sender-shaped source data.

use super::super::fixtures::peer_echo_pane_lines;
use super::*;

/// Equal payloads under different receipt identities remain two occurrences,
/// while repeated copies of one identity install once. Suppressed media,
/// foreign direction and peer-shaped data under another content type cannot
/// contribute installation evidence. Replay must not persist another source.
#[test]
fn runtime_fresh_peer_replay_reports_exact_successful_installations() {
    let root = temp_root("fresh-peer-installation-identities");
    let store = AgentTranscriptStore::new(root.clone());
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(store.clone());
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let conversation = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    service.set_pane_screen(
        "%1",
        TerminalScreen::new(Size::new(80, 24).unwrap(), 120).unwrap(),
    );
    let first = "peer-message recipient=agent-%1 sequence=1 id=equal-a";
    let second = "peer-message recipient=agent-%1 sequence=2 id=equal-b";
    let suppressed = "peer-message recipient=agent-%1 sequence=3 id=suppressed";
    let foreign = "peer-message recipient=agent-%1 sequence=4 id=foreign";
    let wrong_type = "peer-message recipient=agent-%1 sequence=5 id=wrong-type";
    let peer_type = "application/vnd.mezzanine.agent-presentation.peer-message+json; charset=utf-8";
    let rows = [
        (
            first,
            "received",
            "same payload",
            true,
            "text/plain; charset=utf-8",
            peer_type,
        ),
        (
            second,
            "received",
            "same payload",
            true,
            "text/plain; charset=utf-8",
            peer_type,
        ),
        (
            first,
            "received",
            "same payload",
            true,
            "text/plain; charset=utf-8",
            peer_type,
        ),
        (
            suppressed,
            "received",
            "suppressed payload",
            false,
            "application/json",
            peer_type,
        ),
        (
            foreign,
            "foreign",
            "foreign direction payload",
            true,
            "text/plain; charset=utf-8",
            peer_type,
        ),
        (
            wrong_type,
            "received",
            "wrong source type payload",
            true,
            "text/plain; charset=utf-8",
            "text/plain; charset=utf-8",
        ),
    ]
    .into_iter()
    .enumerate()
    .map(
        |(index, (identity, direction, payload, eligible, content_type, source_type))| {
            crate::storage::transcript::AgentPresentationEntry {
                presentation: Some(mez_core::presentation::PresentationEntry {
                    id: identity.to_string(),
                    occurrence: Default::default(),
                    first_row: true,
                }),
                conversation_id: conversation.clone(),
                sequence: index as u64 + 1,
                created_at_unix_seconds: 1,
                pane_id: "%1".to_string(),
                turn_id: None,
                terminal_width: 80,
                style_names: vec!["user-prompt".to_string()],
                display_lines: vec![payload.to_string()],
                copy_lines: vec![payload.to_string()],
                ansi_text: None,
                source_text: Some(
                    serde_json::json!({
                        "direction": direction,
                        "receive_identity": identity,
                        "peer": "sender",
                        "payload": payload,
                        "content_type": content_type,
                        "direct_parent": false,
                        "presentation_eligible": eligible,
                    })
                    .to_string(),
                ),
                source_content_type: Some(source_type.to_string()),
            }
        },
    )
    .collect::<Vec<_>>();
    let (replayed, installed) = service
        .replay_agent_presentation_entries_with_peer_installations("%1", &rows)
        .unwrap();
    assert!(replayed);
    assert_eq!(
        installed,
        [first.to_string(), second.to_string()]
            .into_iter()
            .collect()
    );
    let lines = peer_echo_pane_lines(&service, "%1");
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.contains("same payload"))
            .count(),
        2,
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains("suppressed payload")),
        "{lines:?}"
    );
    assert!(
        store
            .inspect_presentation(&conversation)
            .unwrap()
            .is_empty()
    );
    let _ = std::fs::remove_dir_all(root);
}
