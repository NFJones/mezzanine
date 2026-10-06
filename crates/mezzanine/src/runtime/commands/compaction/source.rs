//! Immutable durable-source work for manual conversation compaction preparation.
//!
//! The runtime captures the installed transcript store and exact conversation
//! before decoding any source. Execution consumes that capture and never looks
//! up a current pane, configuration, turn, or replacement store. It performs no
//! publication, scheduling or model call. The caller remains responsible for
//! generation/freshness validation before adopting these rows or committing an
//! epoch. Missing storage is an honest no-source outcome; corruption is an error.
//!
//! This boundary preserves the existing synchronous source-read contract while
//! making it transferable to a bounded worker. It alone does not claim that
//! manual command preparation has become asynchronous or visibly admitted.

use super::{Result, TranscriptEntry};
use crate::storage::transcript::AgentTranscriptStore;

/// Actor-captured store and conversation, never a live service reference.
pub(super) struct ManualCompactionSourceWork {
    store: Option<AgentTranscriptStore>,
    conversation_id: String,
}

impl ManualCompactionSourceWork {
    /// Captures immutable source ownership without reading transcript files.
    /// A missing installed store stays missing; execution cannot rediscover it.
    pub(super) fn capture(store: Option<AgentTranscriptStore>, conversation_id: String) -> Self {
        Self {
            store,
            conversation_id,
        }
    }

    /// Decodes the captured durable archive without accessing actor state.
    /// Missing archive/store returns no source, while malformed or inaccessible
    /// source propagates its existing typed error and is never silently skipped.
    pub(super) fn execute(self) -> Result<Vec<TranscriptEntry>> {
        let Some(store) = self.store else {
            return Ok(Vec::new());
        };
        match store.inspect(&self.conversation_id) {
            Ok(entries) => Ok(entries),
            Err(error) if error.kind() == crate::error::MezErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Capturing source ownership performs no archive decode. A malformed file
    /// encountered later must remain an error, not become a successful empty
    /// selection that would falsely classify corruption as a no-work outcome.
    #[test]
    fn manual_compaction_source_corruption_propagates_without_skip() {
        let root = std::env::temp_dir().join(format!(
            "mez-compaction-corrupt-{:032x}",
            rand::random::<u128>()
        ));
        let store = AgentTranscriptStore::new(root.clone());
        let path = store.transcript_path("source-corrupt").unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not-a-transcript-record\n").unwrap();
        let direct = store.inspect("source-corrupt").unwrap_err();
        let captured = ManualCompactionSourceWork::capture(Some(store), "source-corrupt".into());
        let error = captured.execute().unwrap_err();
        assert_eq!(error.kind(), direct.kind());
        assert_eq!(error.message(), direct.message());
        assert_eq!(std::fs::read(&path).unwrap(), b"not-a-transcript-record\n");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A captured source remains bound to its original conversation and store
    /// even when callers later install another store. No source returns an empty
    /// projection, while a valid original archive preserves exact chronology.
    #[test]
    fn manual_compaction_source_capture_keeps_original_archive_identity() {
        let root = std::env::temp_dir().join(format!(
            "mez-compaction-source-{:032x}",
            rand::random::<u128>()
        ));
        let store = AgentTranscriptStore::new(root.clone());
        let entry = TranscriptEntry {
            conversation_id: "source-original".into(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::Assistant,
            turn_id: "source-turn".into(),
            agent_id: "agent-%1".into(),
            pane_id: "%1".into(),
            content: "exact source".into(),
        };
        store.append(&entry).unwrap();
        let captured =
            ManualCompactionSourceWork::capture(Some(store), entry.conversation_id.clone());
        let replacement = AgentTranscriptStore::new(root.join("replacement"));
        assert!(
            ManualCompactionSourceWork::capture(Some(replacement), entry.conversation_id.clone())
                .execute()
                .unwrap()
                .is_empty()
        );
        assert_eq!(captured.execute().unwrap(), vec![entry]);
        assert!(
            ManualCompactionSourceWork::capture(None, "source-original".into())
                .execute()
                .unwrap()
                .is_empty()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
