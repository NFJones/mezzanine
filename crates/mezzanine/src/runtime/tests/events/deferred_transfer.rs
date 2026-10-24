//! Original-source ownership during capacity-bounded external-effect transfer.
//!
//! These process-free fixtures inspect dispatch identities only. They never run
//! a hook, clipboard command, timer, native process or persistence worker.

use super::*;

/// Returns an inert identity for source-queue extraction tests, never execution.
fn inert_dispatch(owner: &str) -> RuntimeSideEffect {
    RuntimeSideEffect::ScheduleTimer {
        key: crate::runtime::RuntimeTimerKey::new(
            crate::runtime::RuntimeTimerKind::PanePipeHealth,
            owner,
            1,
        ),
        delay_ms: 1,
    }
}

/// A zero-slot transfer leaves original tokens intact, while a one-slot transfer
/// rotates among independent owners even when pane input is refilled each time.
/// The synthetic hook payload proves custody only, not worker validity.
#[test]
fn deferred_transfer_fair_prefixes_preserve_source_identities() {
    let mut service = test_runtime_service();
    service
        .persistence
        .queue_pane_input(inert_dispatch("input"));
    service
        .presentation
        .defer_agent_presentation_resize("resize", Size::new(80, 24).unwrap());
    service
        .persistence
        .queue_program_hook(inert_dispatch("hook"));
    assert!(
        service
            .drain_deferred_effects_transition_up_to(0)
            .side_effects
            .is_empty()
    );
    let first = service
        .drain_deferred_effects_transition_up_to(1)
        .side_effects;
    assert!(
        matches!(&first[..], [RuntimeSideEffect::ScheduleTimer { key, .. }] if key.owner_id == "input")
    );
    service
        .persistence
        .queue_pane_input(inert_dispatch("refill"));
    let second = service
        .drain_deferred_effects_transition_up_to(1)
        .side_effects;
    assert!(
        matches!(&second[..], [RuntimeSideEffect::DispatchAgentPresentationResize { pane_id, .. }] if pane_id == "resize")
    );
    let third = service
        .drain_deferred_effects_transition_up_to(1)
        .side_effects;
    assert!(
        matches!(&third[..], [RuntimeSideEffect::ScheduleTimer { key, .. }] if key.owner_id == "hook")
    );
    let fourth = service
        .drain_deferred_effects_transition_up_to(1)
        .side_effects;
    assert!(
        matches!(&fourth[..], [RuntimeSideEffect::ScheduleTimer { key, .. }] if key.owner_id == "refill")
    );
    assert!(
        service
            .drain_deferred_effects_transition_up_to(1)
            .side_effects
            .is_empty()
    );
}

/// A zero-budget clipboard drain must retain the pending request, not leave a
/// phantom paste generation with no worker dispatch. A later one-slot drain
/// transfers it once without reading the host clipboard in the test.
#[test]
fn deferred_transfer_clipboard_zero_budget_keeps_pending_generation() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let descriptor = service.active_window_pane_descriptor(None).unwrap();
    service
        .paste_clipboard_or_most_recent_buffer_to_pane(&primary, &descriptor)
        .unwrap();
    assert!(
        service
            .drain_host_clipboard_read_transition_up_to(0)
            .side_effects
            .is_empty()
    );
    let effects = service
        .drain_host_clipboard_read_transition_up_to(1)
        .side_effects;
    assert!(matches!(
        &effects[..],
        [RuntimeSideEffect::ReadHostClipboard { .. }]
    ));
    assert!(
        service
            .drain_host_clipboard_read_transition_up_to(1)
            .side_effects
            .is_empty()
    );
}

/// Durable source transfer and receipt enrollment must not wait for a transient
/// slot, and repeated zero-slot checks must not enroll the immutable append twice.
/// The retained input later transfers unchanged without replaying the append.
#[test]
fn deferred_transfer_zero_capacity_enrolls_durable_source_once() {
    let mut service = test_runtime_service();
    let store = AgentTranscriptStore::new(std::env::temp_dir().join("mez-inert-transfer-store"));
    let row = TranscriptEntry {
        conversation_id: "transfer-conversation".into(),
        sequence: 1,
        created_at_unix_seconds: 1,
        role: TranscriptRole::User,
        turn_id: "turn-1".into(),
        agent_id: "agent-%1".into(),
        pane_id: "%1".into(),
        content: "immutable source".into(),
    };
    let path = store.transcript_path(&row.conversation_id).unwrap();
    service
        .persistence
        .queue_transcript(RuntimeSideEffect::PersistTranscriptEntries {
            path: path.clone(),
            store,
            entries: vec![row.clone()],
        });
    service
        .persistence
        .queue_pane_input(inert_dispatch("retained"));
    let first = service
        .drain_deferred_effects_transition_up_to(0)
        .side_effects;
    assert!(
        matches!(&first[..], [RuntimeSideEffect::PersistTranscriptEntries { entries, .. }] if entries == std::slice::from_ref(&row))
    );
    for _ in 0..3 {
        assert!(
            service
                .drain_deferred_effects_transition_up_to(0)
                .side_effects
                .is_empty()
        );
    }
    assert_eq!(
        service
            .persistence
            .pending_transcript_entries(&row.conversation_id),
        vec![row.clone()]
    );
    assert!(service.persistence.settle_transcript_write_at_path(
        &row.conversation_id,
        row.sequence,
        &path,
        std::slice::from_ref(&row)
    ));
    assert!(
        service
            .persistence
            .pending_transcript_entries(&row.conversation_id)
            .is_empty()
    );
    let last = service
        .drain_deferred_effects_transition_up_to(1)
        .side_effects;
    assert!(
        matches!(&last[..], [RuntimeSideEffect::ScheduleTimer { key, .. }] if key.owner_id == "retained")
    );
}
