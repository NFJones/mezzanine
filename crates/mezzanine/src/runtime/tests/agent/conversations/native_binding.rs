//! Descriptive native binding epochs in actual active-session checkpoints.
//!
//! These records must distinguish independent resumes before trusted MMP cold
//! recovery can qualify an original binding. Persisted bytes remain inert and
//! generic transcript restoration must continue creating a fresh live epoch.

use super::*;

/// Reads the actual scoped checkpoint row rather than deriving an epoch from a
/// conversation label or fixture token. The field must be present in the next
/// row format; older data has explicitly unknown native binding identity.
fn checkpoint_epoch(service: &mut RuntimeSessionService, store: &AgentTranscriptStore) -> [u8; 16] {
    service.checkpoint_agent_session_metadata().unwrap();
    let path = store.agent_session_metadata_checkpoint_file(service.session().id.as_str());
    let checkpoint: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let row = checkpoint["rows"][0].as_str().unwrap();
    let field = row
        .split('\t')
        .nth(36)
        .expect("actual active-session checkpoint must preserve the shell's native binding epoch");
    serde_json::from_str::<Option<[u8; 16]>>(field)
        .unwrap()
        .expect("actual shell checkpoint cannot invent an unknown native epoch")
}

/// Actual checkpoint publication must preserve exact rollback while changing
/// the saved native epoch for an independent binding of the SAME conversation.
/// Neither public pane/conversation labels nor unchanged MMP allocation floors
/// can distinguish this boundary after a crash without the persisted epoch.
#[test]
fn runtime_checkpoint_preserves_native_epoch_across_same_conversation_rebindings() {
    let store = AgentTranscriptStore::new(temp_root("native-binding-checkpoint"));
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let original = service.agent_shell_store().get("%1").unwrap().clone();
    let conversation = original.session_id.clone();
    let first = checkpoint_epoch(&mut service, &store);
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", &conversation, 0)
        .unwrap();
    let second = checkpoint_epoch(&mut service, &store);
    assert_ne!(first, second);
    assert_eq!(service.message_service().next_message_sequence(), 1);
    service
        .agent_shell_store_mut()
        .restore_session("%1", original)
        .unwrap();
    assert_eq!(checkpoint_epoch(&mut service, &store), first);
}

/// Even a valid epoch saved by the actual active checkpoint is not permission
/// for ordinary transcript restore to recreate that original live binding. Same
/// session/pane/conversation labels still receive a fresh opaque shell identity;
/// only the future trusted original-store recovery path may qualify continuity.
#[test]
fn runtime_generic_transcript_restore_keeps_saved_native_epoch_inert() {
    let store = AgentTranscriptStore::new(temp_root("native-binding-generic-restore"));
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let original = checkpoint_epoch(&mut service, &store);
    let session = service.session().clone();
    let mut restored =
        crate::test_support::runtime::RuntimeServiceFixture::new().build_with_session(session);
    restored.set_agent_transcript_store(store);
    assert_eq!(
        restored
            .restore_agent_sessions_from_transcript_store()
            .unwrap(),
        1
    );
    assert_ne!(
        restored
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .native_message_incarnation
            .snapshot_identity(),
        original
    );
}
