//! Display-only retry regressions with inert receipts and no external workers.
//!
//! The fixture creates only a unique temporary directory and an in-memory runtime
//! service. Persistence effects are inspected, never dispatched; no native pane,
//! provider, editor, timer worker or shell process is started.

use super::*;
use mez_agent::transcript::SteeringRecoveryReceipt;

/// Owns the fixture's exact temporary directory without touching shared state.
struct Fixture {
    service: RuntimeSessionService,
    root: std::path::PathBuf,
    conversation: String,
    receipt: SteeringRecoveryReceipt,
}

impl Fixture {
    /// Restores one accepted display source without reconstructing executable input.
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "mez-steering-display-retry-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        let mut service = crate::test_support::runtime::RuntimeServiceFixture::new()
            .control_socket(root.join("control.sock"))
            .build();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        let conversation = service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .session_id
            .clone();
        let receipt = SteeringRecoveryReceipt {
            occurrence: mez_core::presentation::PresentationOccurrence::new(
                Some(1234),
                mez_core::presentation::PresentationOccurrenceBasis::LocalAcceptance,
            ),
            id: "original-steering-occurrence".into(),
            acceptance_order: 1,
            turn_id: Some("inert-original-turn".into()),
            event_sequence: Some(1),
            display: "original accepted source\r\ntrailing source\n".into(),
            status: SteeringRecoveryStatus::Admitted(7),
        };
        service
            .restore_steering_recovery("%1", &conversation, std::slice::from_ref(&receipt))
            .unwrap();
        service.set_agent_transcript_store(crate::storage::transcript::AgentTranscriptStore::new(
            root.join("transcripts"),
        ));
        service.mark_steering_recovery_published();
        Self {
            service,
            root,
            conversation,
            receipt,
        }
    }

    /// Reads retained agent rows, not a terminal, provider or process fixture.
    fn text(&self) -> String {
        self.service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
    }

    /// Models elapsed idle time without a sleep, request, new input or worker.
    fn make_due(&mut self) {
        for retry in self
            .service
            .presentation
            .steering_presentation_retries
            .values_mut()
        {
            retry.next_attempt_at = std::time::Instant::now();
        }
    }
}

impl Drop for Fixture {
    /// Removes only the unique directory created by this fixture.
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

/// A current checkpoint must not hide the lost-obligation bug with a fresh
/// presentation request. Fail both the initial install and its scheduled retry,
/// then drain normally with a healthy renderer and no input/request. The original
/// receipt must install and publish once, preserving exact source and chronology.
#[test]
fn steering_display_retry_two_failures_then_ordinary_drain_installs_original_once() {
    let mut fixture = Fixture::new();
    fixture.service.fail_agent_presentation_install_for_tests(0);
    fixture.service.request_steering_presentation("%1");
    assert!(!fixture.service.steering_recovery_needs_publication());
    fixture.service.fail_agent_presentation_install_for_tests(0);
    fixture.make_due();
    let failed = fixture.service.drain_transcript_persistence_transition();
    assert!(failed.side_effects.is_empty());
    assert_eq!(
        fixture.service.presentation.agent_install_failure_countdown,
        None
    );
    assert!(!fixture.text().contains("original accepted source"));

    fixture.make_due();
    let recovered = fixture.service.drain_transcript_persistence_transition();
    assert_eq!(
        fixture
            .text()
            .matches("user> original accepted source")
            .count(),
        1
    );
    let sources = recovered
        .side_effects
        .into_iter()
        .map(|effect| {
            let crate::runtime::RuntimeSideEffect::PersistSteeringPresentation { entry, .. } =
                effect
            else {
                panic!("display recovery emitted a non-steering effect");
            };
            assert_eq!(entry.presentation.as_ref().unwrap().id, fixture.receipt.id);
            crate::storage::transcript::steering::Source::decode(
                entry.source_text.as_deref().unwrap(),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].conversation_id, fixture.conversation);
    assert_eq!(sources[0].receipt, fixture.receipt);
    assert!(
        fixture
            .service
            .drain_transcript_persistence_transition()
            .side_effects
            .is_empty()
    );
    assert_eq!(
        fixture
            .text()
            .matches("user> original accepted source")
            .count(),
        1
    );
    assert!(
        fixture
            .service
            .presentation
            .steering_presentation_retries
            .is_empty()
    );
    assert!(fixture.service.agent_turn_ledger().turns().is_empty());
    assert_eq!(
        fixture
            .service
            .steering_presentation_receipts("%1")
            .unwrap(),
        vec![fixture.receipt.clone()]
    );
}

/// Persistent unavailability must stay enrolled in the one tracked idle timer,
/// not exhaust a lifetime attempt budget. Hot drains and fresh dirty-checkpoint
/// requests cannot bypass pacing. Many due failures retain one obligation and
/// one source-free diagnostic; the next healthy idle drain installs once.
#[test]
fn steering_display_retry_many_failures_keep_paced_idle_recovery_live() {
    let mut fixture = Fixture::new();
    let before = fixture.service.event_log().unwrap().latest_event_id();
    fixture.service.fail_agent_presentation_install_for_tests(0);
    fixture.service.request_steering_presentation("%1");
    let diagnostic = fixture.service.event_log().unwrap().latest_event_id();
    assert_eq!(diagnostic, before + 1);
    let events = fixture
        .service
        .event_log()
        .unwrap()
        .replay_for(&crate::runtime::EventAudience::AllPrimaries);
    let event = events
        .iter()
        .find(|event| event.payload.contains("steering display unavailable"))
        .unwrap();
    assert!(!event.payload.contains("original accepted source"));
    assert!(event.payload.contains(&fixture.conversation));
    let progress = std::collections::BTreeSet::new();
    for _ in 1..=12 {
        assert!(fixture.service.steering_presentation_retry_timer_needed());
        fixture.service.fail_agent_presentation_install_for_tests(0);
        // A future deadline, not elapsed wall time, proves no hot-drain attempt.
        fixture
            .service
            .presentation
            .steering_presentation_retries
            .values_mut()
            .next()
            .unwrap()
            .next_attempt_at = std::time::Instant::now() + std::time::Duration::from_secs(60);
        fixture.service.request_steering_presentation("%1");
        assert!(
            fixture
                .service
                .drain_transcript_persistence_transition()
                .side_effects
                .is_empty()
        );
        assert_eq!(
            fixture.service.presentation.agent_install_failure_countdown,
            Some(0)
        );
        fixture.make_due();
        fixture
            .service
            .apply_idle_cleanup_timer_event_with_actor_progress(&progress)
            .unwrap();
        assert!(
            fixture
                .service
                .drain_transcript_persistence_transition()
                .side_effects
                .is_empty()
        );
        assert_eq!(
            fixture.service.presentation.agent_install_failure_countdown,
            None
        );
        assert_eq!(
            fixture
                .service
                .presentation
                .steering_presentation_retries
                .len(),
            1
        );
        assert!(
            fixture
                .service
                .presentation
                .steering_presentation_retries
                .values()
                .next()
                .unwrap()
                .backoff
                <= 5
        );
        assert_eq!(
            fixture.service.event_log().unwrap().latest_event_id(),
            diagnostic
        );
    }
    fixture.make_due();
    fixture
        .service
        .apply_idle_cleanup_timer_event_with_actor_progress(&progress)
        .unwrap();
    let recovered = fixture.service.drain_transcript_persistence_transition();
    assert_eq!(recovered.side_effects.len(), 1);
    assert_eq!(
        fixture
            .text()
            .matches("user> original accepted source")
            .count(),
        1
    );
    assert!(!fixture.service.steering_presentation_retry_timer_needed());
    assert!(
        !fixture
            .service
            .idle_cleanup_timer_needed_with_actor_progress(&progress)
    );
    assert!(fixture.service.agent_turn_ledger().turns().is_empty());
}

/// A successful prefix is already displayed and has independent persistence
/// ownership when a later equal-text occurrence fails. Retry preserves acceptance
/// order and exact IDs without publishing or displaying the prefix a second time.
#[test]
fn steering_display_retry_partial_prefix_preserves_equal_text_order() {
    let mut fixture = Fixture::new();
    let mut second = fixture.receipt.clone();
    second.id = "second-equal-text-occurrence".into();
    second.acceptance_order = 2;
    second.occurrence.unix_millis = Some(1235);
    fixture
        .service
        .restore_steering_recovery(
            "%1",
            &fixture.conversation,
            &[fixture.receipt.clone(), second.clone()],
        )
        .unwrap();
    fixture.service.fail_agent_presentation_install_for_tests(1);
    fixture.service.request_steering_presentation("%1");
    assert_eq!(
        fixture.service.presentation.agent_install_failure_countdown,
        None
    );
    assert_eq!(
        fixture
            .text()
            .matches("user> original accepted source")
            .count(),
        1
    );
    let first = fixture.service.drain_transcript_persistence_transition();
    fixture.make_due();
    let second_write = fixture.service.drain_transcript_persistence_transition();
    let sources = first
        .side_effects
        .into_iter()
        .chain(second_write.side_effects)
        .map(|effect| {
            let crate::runtime::RuntimeSideEffect::PersistSteeringPresentation { entry, .. } =
                effect
            else {
                panic!("display recovery emitted a non-steering effect");
            };
            crate::storage::transcript::steering::Source::decode(
                entry.source_text.as_deref().unwrap(),
            )
            .unwrap()
            .receipt
        })
        .collect::<Vec<_>>();
    assert_eq!(sources, vec![fixture.receipt.clone(), second]);
    assert_eq!(
        fixture
            .text()
            .matches("user> original accepted source")
            .count(),
        2
    );
    assert!(
        fixture
            .service
            .drain_transcript_persistence_transition()
            .side_effects
            .is_empty()
    );
    assert_eq!(
        fixture
            .service
            .presentation
            .presented_steering_receipts
            .len(),
        2
    );
    assert!(fixture.service.agent_turn_ledger().turns().is_empty());
}

/// A checkpoint publication can ask for all panes again in the same drain. A
/// failed due attempt must remain paced rather than receiving an immediate hidden
/// second attempt; checkpoint ownership and display recovery remain independent.
#[test]
fn steering_display_retry_dirty_checkpoint_does_not_bypass_failed_attempt() {
    let mut fixture = Fixture::new();
    fixture.service.fail_agent_presentation_install_for_tests(0);
    fixture.service.request_steering_presentation("%1");
    fixture.service.mark_steering_recovery_dirty_for_tests();
    fixture.service.fail_agent_presentation_install_for_tests(0);
    fixture.make_due();
    let drained = fixture.service.drain_transcript_persistence_transition();
    assert!(!drained.side_effects.iter().any(|effect| matches!(
        effect,
        crate::runtime::RuntimeSideEffect::PersistSteeringPresentation { .. }
    )));
    assert_eq!(
        fixture.service.presentation.agent_install_failure_countdown,
        None
    );
    assert_eq!(
        fixture
            .service
            .presentation
            .steering_presentation_retries
            .len(),
        1
    );
    assert!(!fixture.text().contains("original accepted source"));
    fixture.make_due();
    fixture.service.drain_transcript_persistence_transition();
    assert_eq!(
        fixture
            .text()
            .matches("user> original accepted source")
            .count(),
        1
    );
}

/// Conversation replacement and explicit pane presentation teardown retire only
/// the old display obligation. Neither path reconstructs a missing shell session,
/// publishes old source into a replacement conversation or replays executable work.
#[test]
fn steering_display_retry_retires_replaced_and_removed_owners() {
    for remove in [false, true] {
        let mut fixture = Fixture::new();
        fixture.service.fail_agent_presentation_install_for_tests(0);
        fixture.service.request_steering_presentation("%1");
        if remove {
            fixture
                .service
                .presentation
                .remove_agent_presentation_state("%1");
            fixture.service.agent_shell_store_mut().remove_session("%1");
        } else {
            fixture
                .service
                .agent_shell_store_mut()
                .start_new_conversation("%1")
                .unwrap();
        }
        fixture.make_due();
        assert!(
            fixture
                .service
                .drain_transcript_persistence_transition()
                .side_effects
                .is_empty()
        );
        assert!(
            fixture
                .service
                .presentation
                .steering_presentation_retries
                .is_empty()
        );
        assert!(!fixture.service.steering_presentation_retry_timer_needed());
        assert!(!fixture.text().contains("original accepted source"));
        assert!(fixture.service.agent_turn_ledger().turns().is_empty());
        if remove {
            assert!(fixture.service.agent_shell_store().get("%1").is_none());
        }
    }
}

/// A projected resume can replace presentation before a later transaction error.
/// Rollback must restore the original display obligation with its exact pacing,
/// installed prefix and receipt source, not silently retire it or fresh-request
/// recovery. Only a subsequent due ordinary drain may install the remaining row.
#[test]
fn steering_display_retry_failed_projected_resume_restores_exact_pacing_and_prefix() {
    let mut fixture = Fixture::new();
    let mut second = fixture.receipt.clone();
    second.id = "rollback-second-occurrence".into();
    second.acceptance_order = 2;
    fixture
        .service
        .restore_steering_recovery(
            "%1",
            &fixture.conversation,
            &[fixture.receipt.clone(), second.clone()],
        )
        .unwrap();
    fixture.service.mark_steering_recovery_published();
    fixture.service.fail_agent_presentation_install_for_tests(1);
    fixture.service.request_steering_presentation("%1");
    let key = ("%1".to_string(), fixture.conversation.clone());
    let retry = fixture
        .service
        .presentation
        .steering_presentation_retries
        .get_mut(&key)
        .unwrap();
    retry.next_attempt_at = std::time::Instant::now() + std::time::Duration::from_secs(60);
    retry.backoff = 5;
    let deadline = retry.next_attempt_at;
    let prefix = fixture.service.drain_transcript_persistence_transition();
    assert_eq!(prefix.side_effects.len(), 1);
    let store =
        crate::storage::transcript::AgentTranscriptStore::new(fixture.root.join("transcripts"));
    store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "rollback-target".into(),
            sequence: 1,
            created_at_unix_seconds: 10,
            role: mez_agent::transcript::TranscriptRole::User,
            turn_id: "inert-target-turn".into(),
            agent_id: "agent-%9".into(),
            pane_id: "%9".into(),
            content: "replacement source".into(),
        })
        .unwrap();
    let mut prepared =
        crate::runtime::commands::read_direct_resume(&store, "rollback-target", None).unwrap();
    let size = fixture.service.agent_pane_screen("%1").unwrap().size();
    prepared.projection = Some(
        crate::runtime::service_state::RuntimeDirectResumeProjection {
            screen: mez_terminal::TerminalScreen::new_with_history_config(size, 100, 10).unwrap(),
            steering_receipt_ids: Default::default(),
            prompt_history: Vec::new(),
            size,
            presentation_settings: fixture.service.presentation.settings.clone(),
            history_limit: fixture.service.terminal_history_limit(),
            history_rotate_lines: fixture.service.terminal_history_rotate_lines(),
            presentation_source_revision: None,
        },
    );
    fixture
        .service
        .fail_next_agent_resume_after_authority_restore_for_tests();
    let error = fixture
        .service
        .execute_agent_shell_resume_command_with_read("%1", store, prepared, false)
        .unwrap_err();
    assert!(
        error.message().contains("post-authority restoration"),
        "{error}"
    );
    assert_eq!(
        fixture
            .service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .session_id,
        fixture.conversation
    );
    let restored = fixture
        .service
        .presentation
        .steering_presentation_retries
        .get(&key)
        .unwrap();
    assert_eq!(restored.next_attempt_at, deadline);
    assert_eq!(restored.backoff, 5);
    assert!(!fixture.service.steering_recovery_needs_publication());
    assert!(
        fixture
            .service
            .drain_transcript_persistence_transition()
            .side_effects
            .is_empty()
    );
    assert_eq!(
        fixture
            .text()
            .matches("user> original accepted source")
            .count(),
        1
    );
    fixture.make_due();
    let recovered = fixture.service.drain_transcript_persistence_transition();
    let [crate::runtime::RuntimeSideEffect::PersistSteeringPresentation { entry, .. }] =
        recovered.side_effects.as_slice()
    else {
        panic!("expected only the remaining original occurrence");
    };
    let source =
        crate::storage::transcript::steering::Source::decode(entry.source_text.as_deref().unwrap())
            .unwrap();
    assert_eq!(source.conversation_id, fixture.conversation);
    assert_eq!(source.receipt, second);
    assert_eq!(
        fixture
            .text()
            .matches("user> original accepted source")
            .count(),
        2
    );
    assert!(!fixture.text().contains("replacement source"));
    assert!(!fixture.service.steering_presentation_retry_timer_needed());
    assert!(
        fixture
            .service
            .drain_transcript_persistence_transition()
            .side_effects
            .is_empty()
    );
    assert!(fixture.service.agent_turn_ledger().turns().is_empty());
}

/// More due owners than one drain's work limit must make fair progress. Pure
/// session layout construction creates no native panes. Keep the lexically first
/// owner failing while a later owner beyond the first batch must still install;
/// new source is neither dropped to cap backlog nor copied into another pane.
#[test]
fn steering_display_retry_bounds_batch_and_does_not_starve_later_panes() {
    let mut fixture = Fixture::new();
    let primary = fixture
        .service
        .session_mut_for_tests()
        .attach_primary("fixture", true)
        .unwrap();
    let mut panes = vec!["%1".to_string()];
    for index in 0..9 {
        fixture
            .service
            .session_mut_for_tests()
            .new_window(&primary, format!("retry-{index}"), true)
            .unwrap();
        let pane = fixture
            .service
            .session()
            .active_window()
            .unwrap()
            .active_pane()
            .id
            .to_string();
        fixture
            .service
            .agent_shell_store_mut()
            .enter_or_resume(&pane)
            .unwrap();
        let conversation = fixture
            .service
            .agent_shell_store()
            .get(&pane)
            .unwrap()
            .session_id
            .clone();
        fixture
            .service
            .restore_steering_recovery(&pane, &conversation, std::slice::from_ref(&fixture.receipt))
            .unwrap();
        panes.push(pane);
    }
    for pane in &panes {
        fixture.service.fail_agent_presentation_install_for_tests(0);
        fixture.service.request_steering_presentation(pane);
    }
    fixture.service.mark_steering_recovery_published();
    assert_eq!(
        fixture
            .service
            .presentation
            .steering_presentation_retries
            .len(),
        10
    );
    fixture.make_due();
    fixture.service.fail_agent_presentation_install_for_tests(0);
    let first = fixture.service.drain_transcript_persistence_transition();
    assert_eq!(
        first.side_effects.len(),
        STEERING_DISPLAY_RETRIES_PER_DRAIN - 1
    );
    assert_eq!(
        fixture
            .service
            .presentation
            .steering_presentation_retries
            .len(),
        3
    );
    let cursor = fixture
        .service
        .presentation
        .steering_presentation_retry_cursor
        .clone();
    assert!(
        fixture
            .service
            .presentation
            .steering_presentation_retries
            .keys()
            .any(|key| key > &cursor)
    );
    fixture.make_due();
    let later = fixture.service.drain_transcript_persistence_transition();
    assert_eq!(later.side_effects.len(), 3);
    assert!(
        fixture
            .service
            .presentation
            .steering_presentation_retries
            .is_empty()
    );
    for pane in panes {
        let text = fixture
            .service
            .agent_pane_screen(&pane)
            .unwrap()
            .normal_content_lines()
            .join("\n");
        assert_eq!(
            text.matches("user> original accepted source").count(),
            1,
            "{pane}: {text}"
        );
    }
    assert!(fixture.service.agent_turn_ledger().turns().is_empty());
}

/// Hidden agent logs retain their conversation-bound screen rather than writing
/// the process surface. Recovery must preserve that existing eligibility policy:
/// install once into the retained agent screen and reveal it without re-promotion.
#[test]
fn steering_display_retry_hidden_surface_retains_exact_source_until_revealed() {
    let mut fixture = Fixture::new();
    fixture.service.fail_agent_presentation_install_for_tests(0);
    fixture.service.request_steering_presentation("%1");
    fixture
        .service
        .agent_shell_store_mut()
        .request_exit("%1")
        .unwrap();
    fixture.make_due();
    let recovered = fixture.service.drain_transcript_persistence_transition();
    assert_eq!(recovered.side_effects.len(), 1);
    assert_eq!(
        fixture
            .text()
            .matches("user> original accepted source")
            .count(),
        1
    );
    fixture
        .service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    fixture.service.request_steering_presentation("%1");
    assert!(
        fixture
            .service
            .drain_transcript_persistence_transition()
            .side_effects
            .is_empty()
    );
    assert_eq!(
        fixture
            .text()
            .matches("user> original accepted source")
            .count(),
        1
    );
    assert_eq!(
        fixture
            .service
            .steering_presentation_receipts("%1")
            .unwrap(),
        vec![fixture.receipt.clone()]
    );
}

/// A recovered retained screen must request repaint in the same ordinary drain,
/// not wait for unrelated input to expose the row. Pure session client attachment
/// selects a display recipient without attaching a native runtime or timer worker.
#[test]
fn steering_display_retry_success_emits_same_drain_repaint() {
    let mut fixture = Fixture::new();
    let primary = fixture
        .service
        .session_mut_for_tests()
        .attach_primary("fixture", true)
        .unwrap();
    fixture.service.fail_agent_presentation_install_for_tests(0);
    fixture.service.request_steering_presentation("%1");
    fixture.make_due();
    let recovered = fixture.service.drain_deferred_effects_transition();
    assert_eq!(
        recovered
            .side_effects
            .iter()
            .filter(|effect| matches!(effect,
        crate::runtime::RuntimeSideEffect::RenderClient { client_id, .. } if client_id == &primary))
            .count(),
        1
    );
    assert_eq!(
        recovered
            .side_effects
            .iter()
            .filter(|effect| matches!(
                effect,
                crate::runtime::RuntimeSideEffect::PersistSteeringPresentation { .. }
            ))
            .count(),
        1
    );
    assert!(
        fixture
            .service
            .drain_deferred_effects_transition()
            .side_effects
            .is_empty()
    );
    assert_eq!(
        fixture
            .text()
            .matches("user> original accepted source")
            .count(),
        1
    );
}

/// Prefix installation is visible progress even while a later receipt keeps the
/// same obligation live. Each partial retry must publish only its newly installed
/// source and deliver one same-drain repaint, without awaiting whole-pass success.
#[test]
fn steering_display_retry_partial_success_repaints_before_later_failure_recovers() {
    let mut fixture = Fixture::new();
    let primary = fixture
        .service
        .session_mut_for_tests()
        .attach_primary("fixture", true)
        .unwrap();
    let receipts = (1..=3)
        .map(|order| {
            let mut receipt = fixture.receipt.clone();
            receipt.id = format!("ordered-occurrence-{order}");
            receipt.acceptance_order = order;
            receipt
        })
        .collect::<Vec<_>>();
    fixture
        .service
        .restore_steering_recovery("%1", &fixture.conversation, &receipts)
        .unwrap();
    fixture.service.fail_agent_presentation_install_for_tests(0);
    fixture.service.request_steering_presentation("%1");
    for (index, receipt) in receipts.iter().enumerate() {
        fixture.make_due();
        if index < 2 {
            fixture.service.fail_agent_presentation_install_for_tests(1);
        }
        let transition = fixture.service.drain_deferred_effects_transition();
        assert_eq!(transition.side_effects.iter().filter(|effect| matches!(effect,
            crate::runtime::RuntimeSideEffect::RenderClient { client_id, .. } if client_id == &primary)).count(), 1);
        let sources = transition
            .side_effects
            .iter()
            .filter_map(|effect| {
                let crate::runtime::RuntimeSideEffect::PersistSteeringPresentation {
                    entry, ..
                } = effect
                else {
                    return None;
                };
                Some(
                    crate::storage::transcript::steering::Source::decode(
                        entry.source_text.as_deref().unwrap(),
                    )
                    .unwrap()
                    .receipt,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(sources, vec![receipt.clone()]);
        assert_eq!(
            fixture
                .text()
                .matches("user> original accepted source")
                .count(),
            index + 1
        );
        assert_eq!(
            fixture.service.steering_presentation_retry_timer_needed(),
            index < 2
        );
    }
    assert!(
        fixture
            .service
            .drain_deferred_effects_transition()
            .side_effects
            .is_empty()
    );
}
