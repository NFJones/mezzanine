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
        service.agent_shell_store_mut().enter_or_resume("%1").unwrap();
        let conversation = service.agent_shell_store().get("%1").unwrap().session_id.clone();
        let receipt = SteeringRecoveryReceipt {
            occurrence: Default::default(),
            id: "original-steering-occurrence".into(),
            acceptance_order: 1,
            turn_id: None,
            event_sequence: None,
            display: "original accepted source\r\ntrailing source\n".into(),
            status: SteeringRecoveryStatus::Admitted(7),
        };
        service.restore_steering_recovery("%1", &conversation, std::slice::from_ref(&receipt)).unwrap();
        service.set_agent_transcript_store(crate::storage::transcript::AgentTranscriptStore::new(root.join("transcripts")));
        service.mark_steering_recovery_published();
        Self { service, root, conversation, receipt }
    }

    /// Reads retained agent rows, not a terminal, provider or process fixture.
    fn text(&self) -> String {
        self.service.agent_pane_screen("%1").unwrap().normal_content_lines().join("\n")
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
    let failed = fixture.service.drain_transcript_persistence_transition();
    assert!(failed.side_effects.is_empty());
    assert!(!fixture.text().contains("original accepted source"));

    let recovered = fixture.service.drain_transcript_persistence_transition();
    assert_eq!(fixture.text().matches("user> original accepted source").count(), 1);
    let sources = recovered.side_effects.into_iter().map(|effect| {
        let crate::runtime::RuntimeSideEffect::PersistSteeringPresentation { entry, .. } = effect else {
            panic!("display recovery emitted a non-steering effect");
        };
        assert_eq!(entry.presentation.as_ref().unwrap().id, fixture.receipt.id);
        crate::storage::transcript::steering::Source::decode(entry.source_text.as_deref().unwrap()).unwrap()
    }).collect::<Vec<_>>();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].conversation_id, fixture.conversation);
    assert_eq!(sources[0].receipt, fixture.receipt);
    assert!(fixture.service.drain_transcript_persistence_transition().side_effects.is_empty());
    assert_eq!(fixture.text().matches("user> original accepted source").count(), 1);
    assert!(fixture.service.presentation.steering_presentation_retries.is_empty());
    assert!(fixture.service.agent_turn_ledger().turns().is_empty());
    assert_eq!(fixture.service.steering_presentation_receipts("%1").unwrap(), vec![fixture.receipt.clone()]);
}
