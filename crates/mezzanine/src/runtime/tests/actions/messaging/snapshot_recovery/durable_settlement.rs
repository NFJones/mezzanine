//! Durable presentation evidence retires stale snapshot receipt ownership.
//!
//! Restoring an already settled receipt must not consume capacity or schedule
//! another visible row; the presentation log remains the durable evidence owner.

use super::*;

/// Verifies v6 snapshot startup drops an outbox receipt already settled in the
/// durable presentation log. The stale receipt must not consume shared outbox
/// capacity or re-enter the scheduling and rendering paths after restoration.
#[test]
fn runtime_v6_snapshot_restore_filters_durably_settled_peer_receipt() {
    let root = temp_root("runtime-v6-stale-peer-receipt");
    let store = AgentTranscriptStore::new(root);
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(store.clone());
    let identity = "peer-message recipient=agent-%1 sequence=1 id=stale-v6";
    store.append_presentation(&crate::storage::transcript::AgentPresentationEntry {
        conversation_id: "conversation".to_string(), sequence: 1, created_at_unix_seconds: 1,
        pane_id: "%1".to_string(), turn_id: None, terminal_width: 80,
        style_names: vec!["user-prompt".to_string()],
        display_lines: vec!["▐ sender> already durable".to_string()],
        copy_lines: vec!["▐ sender> already durable".to_string()], ansi_text: None,
        source_text: Some(format!(r#"{{"direction":"received","receive_identity":"{identity}","peer":"sender","payload":"already durable","content_type":"text/plain; charset=utf-8","direct_parent":false}}"#)),
        source_content_type: Some("application/vnd.mezzanine.agent-presentation.peer-message+json; charset=utf-8".to_string()),
    }).unwrap();
    let receipt = crate::storage::snapshot::SnapshotUnsettledPeerPresentation {
        identity: identity.to_string(),
        recipient_agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        conversation_id: "conversation".to_string(),
        turn_id: "turn".to_string(),
        sequence: 1,
        peer_label: "sender".to_string(),
        direct_parent: false,
        content_type: "text/plain; charset=utf-8".to_string(),
        payload: "already durable".to_string(),
        presentation_eligible: true,
        live_rendered: true,
    };
    service
        .restore_snapshot_unsettled_received_peer_message_presentations(&[receipt])
        .unwrap();
    assert!(
        service
            .snapshot_unsettled_received_peer_message_presentations()
            .is_empty()
    );
}
