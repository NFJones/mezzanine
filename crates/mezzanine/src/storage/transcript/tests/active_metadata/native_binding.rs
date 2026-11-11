//! Inert native epoch persistence, strict row8 framing and legacy unknown state.

use super::*;

/// A private scoped checkpoint must retain the actual epoch exactly through a
/// fresh store instance. Those saved bytes are data; no live shell is created by
/// decoding the row, and generic runtime restore is covered separately.
#[test]
fn active_metadata_native_epoch_round_trips_actual_scoped_publication() {
    let root = temp_root("active-native-epoch");
    let store = AgentTranscriptStore::new(root.clone());
    let mut record = agent_session_metadata("$scope", "original-native");
    record.native_message_incarnation =
        Some(mez_agent::messaging::MessageNativeIncarnation::default().snapshot_identity());
    store
        .save_agent_session_metadata_checkpoint("$scope", std::slice::from_ref(&record))
        .unwrap();
    assert_eq!(
        AgentTranscriptStore::new(root)
            .load_agent_session_metadata("$scope")
            .unwrap(),
        vec![record]
    );
}

/// Row7 ownership/steering facts survive migration while its missing epoch
/// remains unknown. Neither re-encoding nor an extra undeclared field may turn
/// that legacy absence into a native binding or silently change older columns.
#[test]
fn active_metadata_version7_epoch_absence_remains_unknown_during_migration() {
    let mut record = agent_session_metadata("$scope", "legacy-native");
    record.persistent_subagent_owned = true;
    let current = encode_agent_session_metadata(&record).unwrap();
    let legacy = pre_native_epoch_metadata(&current);
    let migrated = decode_agent_session_metadata(&legacy).unwrap();
    assert_eq!(migrated, record);
    assert_eq!(migrated.native_message_incarnation, None);
    assert!(migrated.persistent_subagent_owned);
    let rewritten = encode_agent_session_metadata(&migrated).unwrap();
    assert!(rewritten.starts_with("mez-agent-session-metadata/9\t"));
    assert_eq!(decode_agent_session_metadata(&rewritten).unwrap(), record);
    assert!(decode_agent_session_metadata(&format!("{legacy}\tnull")).is_err());
}

/// Exact row8 width, UUID version/variant and fixed16-byte JSON framing are
/// mandatory. Missing/extra, unsupported future and malformed epoch fields fail
/// closed rather than being ignored, shifted or used to create a live identity.
#[test]
fn active_metadata_native_epoch_rejects_malformed_and_future_rows() {
    let record = agent_session_metadata("$scope", "native");
    let encoded = encode_agent_session_metadata(&record).unwrap();
    let fields: Vec<_> = encoded.split('\t').collect();
    assert_eq!(fields.len(), 38);
    for value in [
        "true".to_string(),
        serde_json::to_string(&[0; 16]).unwrap(),
        serde_json::to_string(&[1; 15]).unwrap(),
    ] {
        let mut altered = fields.clone();
        altered[36] = &value;
        assert!(decode_agent_session_metadata(&altered.join("\t")).is_err());
    }
    assert!(decode_agent_session_metadata(&fields[..36].join("\t")).is_err());
    assert!(decode_agent_session_metadata(&format!("{encoded}\tnull")).is_err());
    assert!(
        decode_agent_session_metadata(&encoded.replacen("metadata/9", "metadata/10", 1)).is_err()
    );
}

/// Qualification must not overwrite a cooperating writer that publishes after
/// the last read. The compound writer compares the actual original under its
/// session lock before touching either sidecars or the authoritative checkpoint.
#[test]
fn native_metadata_checkpoint_publication_requires_unchanged_original() {
    let root = temp_root("native-metadata-publication-fence");
    let store = AgentTranscriptStore::new(root);
    let mut original = agent_session_metadata("$scope", "original");
    original.native_message_incarnation =
        Some(mez_agent::messaging::MessageNativeIncarnation::default().snapshot_identity());
    store
        .save_agent_session_metadata_checkpoint("$scope", &[original.clone()])
        .unwrap();
    let (origin, records) = store.capture_native_session_metadata("$scope").unwrap();
    origin.revalidate().unwrap();
    let mut successor = original.clone();
    successor.native_message_incarnation =
        Some(mez_agent::messaging::MessageNativeIncarnation::default().snapshot_identity());
    store
        .save_agent_session_metadata_checkpoint("$scope", &[successor.clone()])
        .unwrap();
    let path = store.agent_session_metadata_checkpoint_file("$scope");
    let retained = fs::read(&path).unwrap();
    assert!(
        store
            .save_agent_session_metadata_checkpoint_checked("$scope", &records, Some(&origin))
            .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), retained);
    assert_eq!(
        store.load_agent_session_metadata("$scope").unwrap(),
        vec![successor]
    );
    let (current, records) = store.capture_native_session_metadata("$scope").unwrap();
    store
        .save_agent_session_metadata_checkpoint_checked("$scope", &records, Some(&current))
        .unwrap();
    assert_eq!(fs::read(&path).unwrap(), retained);
}

/// Dedicated native reads cannot import a missing legacy checkpoint or allocate
/// an oversized scoped source. Ordinary metadata APIs retain their own larger
/// budget; native startup must preserve the source on conservative refusal.
#[test]
fn native_metadata_capture_refuses_missing_and_oversized_sources() {
    let root = temp_root("native-metadata-read-limit");
    let store = AgentTranscriptStore::new(root);
    assert!(store.capture_native_session_metadata("$scope").is_err());
    let path = store.agent_session_metadata_checkpoint_file("$scope");
    assert!(!path.exists());
    store.save_agent_session_metadata("$scope", &[]).unwrap();
    let original = vec![b' '; 256 * 1024 + 1];
    fs::write(&path, &original).unwrap();
    assert!(store.capture_native_session_metadata("$scope").is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
}

/// Fresh custody must start with a real scoped private source without replacing
/// existing conversation bindings. Empty initialization is retryable; nonempty
/// current metadata remains byte-for-byte intact when fresh setup refuses it.
#[test]
fn fresh_native_metadata_initialization_preserves_existing_conversations() {
    let root = temp_root("fresh-native-metadata");
    let store = AgentTranscriptStore::new(root);
    store.initialize_native_session_metadata("$scope").unwrap();
    store.initialize_native_session_metadata("$scope").unwrap();
    assert!(
        store
            .capture_native_session_metadata("$scope")
            .unwrap()
            .1
            .is_empty()
    );
    let record = agent_session_metadata("$scope", "retained-conversation");
    store
        .save_agent_session_metadata("$scope", &[record])
        .unwrap();
    let path = store.agent_session_metadata_checkpoint_file("$scope");
    let original = fs::read(&path).unwrap();
    assert!(store.initialize_native_session_metadata("$scope").is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
}
