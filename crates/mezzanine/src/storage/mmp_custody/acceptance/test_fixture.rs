//! Narrow helpers shared by real acceptance and recovery test owners.
//! No test verdicts or one-consumer fault setup belongs in this fixture module.

use super::*;
use mez_agent::messaging::{Envelope, MessageScope, MessageService, Recipient};

/// Owns only one generated private directory, removed on normal/unwind cleanup.
pub(super) struct Fixture(pub(super) std::path::PathBuf);
impl Fixture {
    /// Allocates an initially absent private store destination.
    pub(super) fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "mez-mmp-stream-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        )))
    }
}
impl Drop for Fixture {
    /// Removes only this fixture's generated directory.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Creates a real unpublished original with enough existing payload allowance.
pub(super) fn original(
    payload: String,
) -> (
    MessageService,
    mez_agent::messaging::MessageAcceptanceTicket,
    MessageAcceptanceWrite,
) {
    let mut service = MessageService::with_limits(16, 4 * 1024 * 1024);
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    let envelope = Envelope {
        protocol: "mmp/1",
        id: "original".into(),
        message_type: "send".into(),
        time: "original-time".into(),
        sender: sender.clone(),
        recipient: Recipient::Agent(target.agent_id),
        correlation_id: None,
        ttl_ms: Some(1234),
        content_type: "text/plain".into(),
        payload,
        extension_fields: vec![("frozen".into(), "{\"order\":7}".into())],
    };
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope,
            MessageScope::Session,
            77,
            4096,
            None,
        )
        .unwrap();
    let write = service.begin_acceptance_write(&ticket).unwrap();
    (service, ticket, write)
}

/// Reads a complete test-only oracle while checking each production chunk's
/// individual bound. Production never collects these buffers into one Vec.
pub(super) fn stored_bytes(store: &MmpCustodyStore, sequence: u64) -> Vec<u8> {
    let prefix = format!("a1:{sequence:016x}:");
    let marker = store.read(&format!("{prefix}commit")).unwrap().unwrap();
    let header: serde_json::Value = serde_json::from_slice(&marker).unwrap();
    let mut bytes = Vec::new();
    for index in 0..header["chunks"].as_u64().unwrap() as usize {
        let chunk = store.read(&chunk_key(&prefix, index)).unwrap().unwrap();
        assert!(chunk.len() <= CHUNK_BYTES);
        bytes.extend_from_slice(&chunk);
    }
    assert_eq!(
        bytes.len(),
        header["encoded_bytes"].as_u64().unwrap() as usize
    );
    bytes
}
