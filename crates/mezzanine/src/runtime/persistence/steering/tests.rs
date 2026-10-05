//! Exact persistence attempt fencing without repeating visible promotion.

use super::*;
use crate::storage::transcript::steering::{CONTENT_TYPE, Source};
use mez_agent::transcript::{SteeringRecoveryReceipt, SteeringRecoveryStatus};

/// A commit followed by a lost reply retries only the immutable presentation
/// source. Old generations and wrong paths cannot retire the replacement, and
/// exact durable replay adds no row. Failure permits at most one retry.
#[test]
fn steering_persistence_retry_is_exact_bounded_and_lost_reply_safe() {
    let root = std::env::temp_dir().join(format!(
        "mez-steering-attempt-{}",
        crate::storage::token_usage::new_token_usage_event_id()
    ));
    let store = AgentTranscriptStore::new(root.clone());
    let source = Source {
        version: 1,
        conversation_id: "conversation".into(),
        receipt: SteeringRecoveryReceipt {
            id: "occurrence".into(),
            acceptance_order: 1,
            turn_id: Some("turn".into()),
            event_sequence: Some(1),
            display: "display".into(),
            status: SteeringRecoveryStatus::Admitted(7),
        },
    };
    let entry = AgentPresentationEntry {
        conversation_id: "conversation".into(),
        sequence: 0,
        created_at_unix_seconds: 1,
        pane_id: "%1".into(),
        turn_id: Some("turn".into()),
        terminal_width: 80,
        style_names: vec!["user-prompt".into()],
        display_lines: vec!["user> display".into()],
        copy_lines: Vec::new(),
        ansi_text: None,
        source_text: Some(source.encode().unwrap()),
        source_content_type: Some(CONTENT_TYPE.into()),
    };
    let mut owner = RuntimePersistenceComponent::default();
    owner
        .queue_steering_presentation(store.clone(), entry.clone())
        .unwrap();
    owner
        .queue_steering_presentation(store.clone(), entry)
        .unwrap();
    let effects = owner.take_transcript_effects();
    assert_eq!(effects.len(), 1);
    assert!(owner.presentation_reconstruction_pending("conversation"));
    assert!(!owner.presentation_write_pending("conversation"));
    let RuntimeSideEffect::PersistSteeringPresentation {
        path,
        entry,
        generation,
        ..
    } = &effects[0]
    else {
        panic!("expected exact occurrence write");
    };
    assert!(
        store
            .append_presentation_many(std::slice::from_ref(entry))
            .unwrap()
            > 0
    );
    assert!(!owner.settle_steering_presentation(
        "conversation",
        "occurrence",
        *generation,
        &root.join("wrong"),
        true
    ));
    assert!(owner.settle_steering_presentation(
        "conversation",
        "occurrence",
        *generation,
        path,
        false
    ));
    let retry = owner.take_transcript_effects();
    assert_eq!(retry.len(), 1);
    assert!(owner.presentation_reconstruction_pending("conversation"));
    let RuntimeSideEffect::PersistSteeringPresentation {
        entry: retried,
        generation: next,
        retry_attempt,
        ..
    } = &retry[0]
    else {
        panic!("expected bounded retry");
    };
    assert_eq!(retry_attempt, &1);
    assert!(next > generation);
    assert_eq!(retried, entry);
    assert!(!owner.settle_steering_presentation(
        "conversation",
        "occurrence",
        *generation,
        path,
        true
    ));
    assert_eq!(
        store
            .append_presentation_many(std::slice::from_ref(retried))
            .unwrap(),
        0
    );
    assert!(owner.settle_steering_presentation("conversation", "occurrence", *next, path, false));
    assert!(owner.take_transcript_effects().is_empty());
    assert!(owner.presentation_reconstruction_pending("conversation"));
    assert!(!owner.presentation_write_pending("conversation"));
    assert_eq!(store.inspect_presentation("conversation").unwrap().len(), 1);
    std::fs::remove_dir_all(root).unwrap();
}
