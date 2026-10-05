//! Identity-bearing settled steering display source, separate from model history.
//!
//! Only positive local admission, explicit not-sent or restart uncertainty may
//! enter durable history. Pending source belongs to the live transient owner.
//! Exact occurrence identity permits presentation-only retry without input replay.

use mez_agent::transcript::{SteeringRecoveryReceipt, SteeringRecoveryStatus};
use serde::{Deserialize, Serialize};

use crate::error::{MezError, Result};

/// Version-one settled occurrence envelope, independent of configuration schema.
pub(crate) const CONTENT_TYPE: &str =
    "application/vnd.mezzanine.agent-presentation.steering-v1+json; charset=utf-8";
/// JSON escaping can expand the already bounded display source.
const MAX_BYTES: usize = 8 * 1024 * 1024;

/// Immutable settled source and its owning conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Source {
    /// Explicit envelope version.
    pub(crate) version: u8,
    /// Exact durable owner; replay must not rebind this from pane metadata.
    pub(crate) conversation_id: String,
    /// Execution-inert occurrence with positively settled local evidence.
    pub(crate) receipt: SteeringRecoveryReceipt,
}

impl Source {
    /// Rejects pending or malformed evidence without changing receipt state.
    pub(crate) fn validate(&self) -> Result<()> {
        mez_agent::transcript::validate_conversation_id(&self.conversation_id)?;
        self.receipt.validate()?;
        if self.version != 1 || self.receipt.status == SteeringRecoveryStatus::Pending {
            return Err(MezError::invalid_args("invalid settled steering source"));
        }
        Ok(())
    }

    /// Encodes the exact original display source, never execution input.
    pub(crate) fn encode(&self) -> Result<String> {
        self.validate()?;
        let encoded = serde_json::to_string(self)
            .map_err(|_| MezError::invalid_args("steering source encoding unavailable"))?;
        if encoded.len() > MAX_BYTES {
            return Err(MezError::invalid_args("steering source budget exhausted"));
        }
        Ok(encoded)
    }

    /// Decodes bounded source; corrupt envelopes cannot become ordinary speech.
    pub(crate) fn decode(encoded: &str) -> Result<Self> {
        if encoded.len() > MAX_BYTES {
            return Err(MezError::invalid_args("steering source budget exhausted"));
        }
        let source: Self = serde_json::from_str(encoded)
            .map_err(|_| MezError::invalid_args("invalid settled steering source"))?;
        source.validate()?;
        Ok(source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exact durable retry must add no second row, including duplicates within
    /// one batch. Equal display source with a different occurrence ID remains
    /// distinct, while conflicting reuse leaves previously committed rows intact.
    #[test]
    fn settled_steering_storage_replay_is_occurrence_fenced() {
        let root = std::env::temp_dir().join(format!(
            "mez-steering-store-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        let store = super::super::AgentTranscriptStore::new(root.clone());
        let source = Source {
            version: 1,
            conversation_id: "conversation".into(),
            receipt: SteeringRecoveryReceipt {
                id: "first".into(),
                acceptance_order: 1,
                turn_id: Some("turn".into()),
                event_sequence: Some(1),
                display: "same display".into(),
                status: SteeringRecoveryStatus::Admitted(7),
            },
        };
        let entry = super::super::AgentPresentationEntry {
            conversation_id: source.conversation_id.clone(),
            sequence: 1,
            created_at_unix_seconds: 1,
            pane_id: "%1".into(),
            turn_id: Some("turn".into()),
            terminal_width: 80,
            style_names: vec!["user-prompt".into()],
            display_lines: vec!["user> same display".into()],
            copy_lines: Vec::new(),
            ansi_text: None,
            source_text: Some(source.encode().unwrap()),
            source_content_type: Some(CONTENT_TYPE.into()),
        };
        assert!(
            store
                .append_presentation_many(&[entry.clone(), entry.clone()])
                .unwrap()
                > 0
        );
        assert_eq!(
            store
                .append_presentation_many(std::slice::from_ref(&entry))
                .unwrap(),
            0
        );
        let mut second = source.clone();
        second.receipt.id = "second".into();
        second.receipt.acceptance_order = 2;
        let mut second_entry = entry.clone();
        second_entry.source_text = Some(second.encode().unwrap());
        store.append_presentation_many(&[second_entry]).unwrap();
        let before = store.inspect_presentation("conversation").unwrap();
        assert_eq!(before.len(), 2);
        let mut conflicting = source;
        conflicting.receipt.display = "changed".into();
        let mut conflict_entry = entry;
        conflict_entry.source_text = Some(conflicting.encode().unwrap());
        assert!(store.append_presentation_many(&[conflict_entry]).is_err());
        assert_eq!(store.inspect_presentation("conversation").unwrap(), before);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Exact Unicode/control source survives encoding; pending and unknown
    /// versions reject rather than being promoted by the storage layer.
    #[test]
    fn settled_steering_source_preserves_identity_and_rejects_pending() {
        let mut source = Source {
            version: 1,
            conversation_id: "conversation".into(),
            receipt: SteeringRecoveryReceipt {
                id: "occurrence".into(),
                acceptance_order: 1,
                turn_id: Some("turn".into()),
                event_sequence: Some(1),
                display: "雪\r\n\u{1b}[2J".into(),
                status: SteeringRecoveryStatus::Admitted(7),
            },
        };
        assert_eq!(Source::decode(&source.encode().unwrap()).unwrap(), source);
        source.receipt.status = SteeringRecoveryStatus::Pending;
        assert!(source.encode().is_err());
        source.receipt.status = SteeringRecoveryStatus::NotSent;
        source.version = 2;
        assert!(source.encode().is_err());
    }
}
