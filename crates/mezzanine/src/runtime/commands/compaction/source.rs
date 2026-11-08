//! Original compaction storage ownership and immutable manual-source work.
//!
//! Accepted logical tasks retain their store through correction and retry.
//! Pending rows come from the existing exact-target persistence owners, never
//! from an unqualified conversation lookup after installation changes.
//!
//! The runtime captures the installed transcript store and exact conversation
//! before decoding any source. Execution consumes that capture and never looks
//! up a current pane, configuration, turn, or replacement store. It performs no
//! scheduling or model calls. Existing terminal handoff recovery precedes manual
//! snapshot capture; source and canonical history then share one lease. The caller
//! remains responsible for
//! generation/freshness validation before adopting these rows or committing an
//! epoch. Missing storage is an honest no-source outcome; corruption is an error.
//!
//! This boundary preserves the existing synchronous source-read contract while
//! making it transferable to a bounded worker. It alone does not claim that
//! manual command preparation has become asynchronous or visibly admitted.

use super::Result;
use crate::storage::transcript::AgentTranscriptStore;

/// Actor-captured store and conversation, never a live service reference.
#[derive(Debug, Clone)]
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

    /// Produces manual source and canonical history from one frozen logical
    /// frontier. It cannot consult replacement actor state or reread the epoch.
    pub(super) fn execute_with_history(
        self,
        history: crate::runtime::control::RuntimeAgentPromptHistoryWork,
        high_water: u64,
    ) -> Result<crate::runtime::RuntimeAgentTranscriptSnapshot> {
        history.execute_source_snapshot(self.store.as_ref(), &self.conversation_id, high_water)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mez_agent::TranscriptEntry;

    /// Captures the same paired source/history used by manual preparation. It
    /// performs no source I/O until the caller executes the actual snapshot.
    fn history(
        store: Option<AgentTranscriptStore>,
        conversation: &str,
        frontier: u64,
    ) -> crate::runtime::control::RuntimeAgentPromptHistoryWork {
        let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
        if let Some(store) = store {
            service.set_agent_transcript_store(store);
        }
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        service
            .agent_shell_store_mut()
            .bind_conversation("%1", conversation, frontier)
            .unwrap();
        service.prepare_runtime_agent_prompt_history_work("%1")
    }

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
        let history = history(Some(store.clone()), "source-corrupt", 1);
        let captured = ManualCompactionSourceWork::capture(Some(store), "source-corrupt".into());
        let error = captured.execute_with_history(history, 1).unwrap_err();
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
        let original_history = history(Some(store.clone()), &entry.conversation_id, 1);
        let captured =
            ManualCompactionSourceWork::capture(Some(store), entry.conversation_id.clone());
        let replacement = AgentTranscriptStore::new(root.join("replacement"));
        // A different source cannot borrow the original history capture, even
        // if its conversation ID matches. A genuinely empty capture is separate.
        assert!(
            ManualCompactionSourceWork::capture(
                Some(replacement.clone()),
                entry.conversation_id.clone()
            )
            .execute_with_history(original_history.clone(), 1)
            .is_err()
        );
        let empty = history(Some(replacement.clone()), &entry.conversation_id, 0);
        assert!(
            ManualCompactionSourceWork::capture(Some(replacement), entry.conversation_id.clone())
                .execute_with_history(empty, 0)
                .unwrap()
                .rows
                .is_empty()
        );
        assert_eq!(
            captured
                .execute_with_history(original_history, 1)
                .unwrap()
                .rows,
            vec![entry]
        );
        let absent = history(None, "source-original", 0);
        assert!(
            ManualCompactionSourceWork::capture(None, "source-original".into())
                .execute_with_history(absent, 0)
                .unwrap()
                .rows
                .is_empty()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
