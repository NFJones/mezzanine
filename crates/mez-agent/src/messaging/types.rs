//! Message protocol data types and service state containers.
//!
//! These types define agent identity, recipients, delivery batches, presence,
//! task payloads, and queue state without owning dispatch or serialization code.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::Arc;

use mez_core::ids::{AgentId, IdFactory, PaneId, WindowId};
use serde::{Deserialize, Serialize};

/// Defines the MMP PROTOCOL const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const MMP_PROTOCOL: &str = "mmp/1";
/// Defines the MMP DUPLICATE MESSAGE ID MESSAGE const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const MMP_DUPLICATE_MESSAGE_ID_MESSAGE: &str =
    "message id has already been accepted with different envelope content";
/// Defines the MMP EXPIRED MESSAGE const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const MMP_EXPIRED_MESSAGE: &str = "message expired before delivery";
/// Defines the MMP PAYLOAD TOO LARGE MESSAGE const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const MMP_PAYLOAD_TOO_LARGE_MESSAGE: &str =
    "message payload exceeds configured payload size limit";
/// Defines the MMP UNDELIVERABLE MESSAGE const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const MMP_UNDELIVERABLE_MESSAGE: &str =
    "message recipient is not registered or available";
/// Defines the MMP UNSUPPORTED PROTOCOL MESSAGE const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const MMP_UNSUPPORTED_PROTOCOL_MESSAGE: &str = "unsupported message protocol";
/// Defines the MMP CONTENT TYPE const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const MMP_CONTENT_TYPE: &str = "application/vnd.mezzanine.mmp+json; version=1";

/// Opaque trusted identity for one canonical project root.
///
/// The digest is service/runtime-owned routing state. It must not be included
/// in model-visible identity projections or transport sender JSON.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProjectScopeId(String);

impl ProjectScopeId {
    /// Derives a versioned full SHA-256 identity from canonical project-root bytes.
    pub fn from_canonical_root_bytes(root: &[u8]) -> Self {
        use sha2::{Digest, Sha256};

        let mut hasher = Sha256::new();
        hasher.update(b"mezzanine-mmp-project-scope-v1\0");
        hasher.update(root);
        let digest = hasher.finalize();
        Self(digest.iter().map(|byte| format!("{byte:02x}")).collect())
    }

    /// Restores one canonical opaque scope identifier from snapshot state.
    pub(crate) fn from_snapshot_value(value: &str) -> Option<Self> {
        (value.len() == 64
            && value.bytes().all(|byte| {
                byte.is_ascii_digit() || (byte.is_ascii_lowercase() && byte.is_ascii_hexdigit())
            }))
        .then(|| Self(value.to_string()))
    }

    /// Returns the canonical opaque value for durable private snapshot state.
    pub(crate) fn snapshot_value(&self) -> &str {
        &self.0
    }

    /// Derives a scope from an already-canonical project-root path.
    #[cfg(unix)]
    pub fn from_canonical_root_path(root: &std::path::Path) -> Self {
        use std::os::unix::ffi::OsStrExt;

        Self::from_canonical_root_bytes(root.as_os_str().as_bytes())
    }
}

/// Runtime-trusted project membership captured for one agent conversation.
///
/// The canonical root remains private runtime state used for durable
/// checkpointing and restoration, while the opaque scope identifier is used
/// only for in-memory routing metadata. Neither value is part of message
/// transport or model-visible identity JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectMembership {
    canonical_root: std::path::PathBuf,
    scope_id: ProjectScopeId,
}

impl ProjectMembership {
    /// Builds membership from a root the runtime has already canonicalized.
    #[cfg(unix)]
    pub fn from_canonical_root(canonical_root: std::path::PathBuf) -> Self {
        let scope_id = ProjectScopeId::from_canonical_root_path(&canonical_root);
        Self {
            canonical_root,
            scope_id,
        }
    }

    /// Returns the immutable canonical root retained for durable checkpoints.
    pub fn canonical_root(&self) -> &std::path::Path {
        &self.canonical_root
    }

    /// Clones the opaque routing identifier without exposing the root.
    pub fn scope_id(&self) -> ProjectScopeId {
        self.scope_id.clone()
    }
}

/// Audience selected for a discovery or delivery operation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MessageScope {
    /// Restrict the operation to the requester's trusted project membership.
    #[default]
    Project,
    /// Deliberately widen the operation to the containing Mezzanine session.
    Session,
}

/// Resolved in-memory delivery audience for one accepted message.
///
/// Project delivery records the authenticated sender's opaque membership at
/// acceptance time; session delivery deliberately crosses that boundary. This
/// runtime-only value is not serialized into message snapshots or transport
/// payloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ResolvedMessageAudience {
    /// Recipients must share this opaque trusted project membership.
    Project(ProjectScopeId),
    /// Recipients may belong to any trusted project in the current session.
    Session,
}

/// Carries Sender Identity state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SenderIdentity {
    /// Stores the agent id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub agent_id: AgentId,
    /// Trusted opaque project membership assigned by the runtime.
    pub project_scope: Option<ProjectScopeId>,
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_id: Option<PaneId>,
    /// Stores the window id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub window_id: Option<WindowId>,
    /// Stores the role value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub role: Option<String>,
    /// Stores the capabilities value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub capabilities: Vec<String>,
    /// Stores the bounded generated objective value for this data structure.
    ///
    /// The objective is the agent's current factual objective published for
    /// peer discovery. It is additive to mmp/1 with no version bump, is absent
    /// when the agent has no generated objective yet, and is always normalized
    /// through the shared objective bounds when present.
    pub objective: Option<String>,
}

/// Carries Recipient state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Recipient {
    /// Represents the Agent case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Agent(AgentId),
    /// Represents the Pane case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Pane(PaneId),
    /// Represents the Window case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Window(WindowId),
    /// Represents the Session case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Session,
    /// Represents the Role case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Role(String),
    /// Represents the Capability case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Capability(String),
    /// Represents the Group case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Group(String),
}

/// Carries Envelope state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    /// Stores the protocol value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub protocol: &'static str,
    /// Stores the id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub id: String,
    /// Stores the message type value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub message_type: String,
    /// Stores the time value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub time: String,
    /// Stores the sender value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub sender: SenderIdentity,
    /// Stores the recipient value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub recipient: Recipient,
    /// Stores the correlation id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub correlation_id: Option<String>,
    /// Stores the ttl ms value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub ttl_ms: Option<u64>,
    /// Stores the content type value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub content_type: String,
    /// Stores the payload value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub payload: String,
    /// Stores the extension fields value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub extension_fields: Vec<(String, String)>,
}

/// Defines the Message Sequence type used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub type MessageSequence = u64;

/// Carries Sequenced Envelope state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequencedEnvelope {
    /// Stores the sequence value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub sequence: MessageSequence,
    /// Stores the envelope value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub envelope: Arc<Envelope>,
}

/// Carries Delivery Cursor state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryCursor {
    /// Stores the recipient value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub recipient: AgentId,
    /// Stores the last sequence value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub last_sequence: MessageSequence,
}

/// Carries Delivery Batch state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryBatch {
    /// Stores the cursor value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub cursor: DeliveryCursor,
    /// Stores the messages value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub messages: Vec<SequencedEnvelope>,
}

/// Carries Fanout Batch state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FanoutBatch {
    /// Stores the recipient value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub recipient: AgentId,
    /// Stores the batch value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub batch: DeliveryBatch,
}

/// Aggregate work limits for one fair message-fanout cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FanoutBudget {
    /// Maximum subscribers considered during one cycle.
    pub max_recipients: usize,
    /// Maximum messages selected across all subscriber batches.
    pub max_messages: usize,
    /// Maximum payload bytes selected across all subscriber batches.
    pub max_payload_bytes: usize,
}

impl Default for FanoutBudget {
    fn default() -> Self {
        Self {
            max_recipients: 64,
            max_messages: 1_024,
            max_payload_bytes: 4 * 1_024 * 1_024,
        }
    }
}

/// Cumulative low-cardinality diagnostics for indexed fanout work.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MessageFanoutDiagnostics {
    /// Number of bounded fanout selection cycles.
    pub cycles: u64,
    /// Number of subscribers considered across fanout cycles.
    pub recipients_considered: u64,
    /// Number of retained sequence lookups performed for delivery.
    pub sequence_lookups: u64,
    /// Number of messages selected across subscriber batches.
    pub messages_selected: u64,
    /// Payload bytes selected across subscriber batches.
    pub payload_bytes_selected: u64,
}

/// Carries Delivery state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    /// Stores the accepted value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub accepted: bool,
    /// Stores the message id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub message_id: String,
    /// Stores the sequence value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub sequence: MessageSequence,
    /// Stores the queued recipients value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub queued_recipients: usize,
    /// Stores the status value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub status: DeliveryStatus,
}

/// Accepted message metadata retained for idempotent resend handling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AcceptedMessage {
    /// Stores the envelope value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) envelope: Arc<Envelope>,
    /// Audience resolved when the envelope was accepted.
    pub(super) audience: ResolvedMessageAudience,
    /// Stores the delivery value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) delivery: Delivery,
    /// Stores the accepted at ms value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) accepted_at_ms: u64,
}

/// Carries Delivery Status state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryStatus {
    /// Represents the Accepted case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Accepted,
    /// Represents the Undeliverable case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Undeliverable,
    /// Represents the Expired case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Expired,
}

/// Carries Agent Presence Status state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentPresenceStatus {
    /// Represents the Available case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Available,
    /// Represents the Busy case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Busy,
    /// Represents the Blocked case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Blocked,
    /// Represents the Offline case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Offline,
}

/// Carries Presence Record state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresenceRecord {
    /// Stores the identity value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub identity: SenderIdentity,
    /// Stores the status value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub status: AgentPresenceStatus,
    /// Stores the updated at ms value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub updated_at_ms: u64,
}

/// Carries Task State state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    /// Represents the Queued case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Queued,
    /// Represents the Running case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Running,
    /// Represents the Blocked case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Blocked,
    /// Represents the Succeeded case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Succeeded,
    /// Represents the Failed case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Failed,
    /// Represents the Cancelled case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Cancelled,
}

/// Carries Task Status Payload state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskStatusPayload {
    /// Stores the task id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub task_id: String,
    /// Stores the state value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub state: TaskState,
    /// Stores the progress percent value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub progress_percent: Option<u8>,
    /// Stores the summary value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub summary: String,
}

/// Carries Task Result Payload state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskResultPayload {
    /// Stores the task id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub task_id: String,
    /// Stores the success value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub success: bool,
    /// Stores the summary value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub summary: String,
    /// Stores the output value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub output: String,
}

/// Carries Queued Envelope state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct QueuedEnvelope {
    /// Stores the sequence value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) sequence: MessageSequence,
    /// Stores the envelope value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) envelope: Arc<Envelope>,
    /// Audience resolved from the authenticated sender at acceptance time.
    pub(super) audience: ResolvedMessageAudience,
    /// Stores the accepted at ms value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) accepted_at_ms: u64,
}

/// Carries Message Service state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug)]
pub struct MessageService {
    /// Stores the ids value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) ids: IdFactory,
    /// Stores the registered value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) registered: HashMap<AgentId, SenderIdentity>,
    /// Stores the presence value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) presence: HashMap<AgentId, PresenceRecord>,
    /// Stores the subscriptions value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) subscriptions: HashMap<AgentId, DeliveryCursor>,
    /// Stores the accepted messages value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) accepted_messages: HashMap<String, AcceptedMessage>,
    /// Stores the queue value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) queue: VecDeque<Arc<QueuedEnvelope>>,
    /// Retained messages keyed by sequence for indexed delivery lookup.
    pub(super) queued_by_sequence: BTreeMap<MessageSequence, Arc<QueuedEnvelope>>,
    /// Retained sequence numbers grouped by normalized recipient selector.
    pub(super) queued_by_recipient: HashMap<Recipient, BTreeSet<MessageSequence>>,
    /// Subscription order used to resume fair fanout without sorting.
    pub(super) subscription_order: BTreeMap<String, AgentId>,
    /// Last subscriber considered by bounded fanout.
    pub(super) fanout_after_recipient: Option<String>,
    /// Cumulative bounded-fanout diagnostics.
    pub(super) fanout_diagnostics: MessageFanoutDiagnostics,
    /// Stores the next sequence value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) next_sequence: MessageSequence,
    /// Stores the retention messages value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) retention_messages: usize,
    /// Stores the retention bytes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) retention_bytes: usize,
    /// Stores the queued bytes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) queued_bytes: usize,
}

/// Carries Message Connection state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MessageConnection {
    /// Stores the agent id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub agent_id: Option<AgentId>,
    /// Stores the delivery cursor value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub delivery_cursor: Option<DeliveryCursor>,
}

/// Serializable local message protocol state captured in session snapshots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageServiceSnapshot {
    /// Stores the protocol value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub protocol: String,
    /// Stores the schema version value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub schema_version: u32,
    /// Stores the next sequence value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub next_sequence: MessageSequence,
    /// Stores the retention messages value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub retention_messages: usize,
    /// Stores the retention bytes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub retention_bytes: usize,
    /// Stores the registered agents value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub registered_agents: Vec<MessageIdentitySnapshot>,
    /// Stores the presence value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub presence: Vec<MessagePresenceSnapshot>,
    /// Stores the subscriptions value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub subscriptions: Vec<MessageDeliveryCursorSnapshot>,
    /// Stores the retained messages value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub retained_messages: Vec<MessageQueuedEnvelopeSnapshot>,
    /// Stores the accepted messages value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub accepted_messages: Vec<MessageAcceptedSnapshot>,
}

/// Serializable MMP sender or registered agent identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageIdentitySnapshot {
    /// Stores the agent id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub agent_id: String,
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_id: Option<String>,
    /// Stores the window id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub window_id: Option<String>,
    /// Stores the role value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub role: Option<String>,
    /// Stores the capabilities value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub capabilities: Vec<String>,
    /// Stores the bounded generated objective value for this data structure.
    ///
    /// The field is additive to the identity snapshot schema: legacy snapshots
    /// without it deserialize to `None` and it is omitted again when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
    /// Opaque trusted project membership retained only in durable routing state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_scope: Option<String>,
}

/// Serializable MMP presence record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessagePresenceSnapshot {
    /// Stores the identity value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub identity: MessageIdentitySnapshot,
    /// Stores the status value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub status: String,
    /// Stores the updated at ms value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub updated_at_ms: u64,
}

/// Serializable MMP delivery cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageDeliveryCursorSnapshot {
    /// Stores the recipient value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub recipient: String,
    /// Stores the last sequence value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub last_sequence: MessageSequence,
}

/// Serializable retained MMP message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageQueuedEnvelopeSnapshot {
    /// Stores the sequence value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub sequence: MessageSequence,
    /// Stores the accepted at ms value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub accepted_at_ms: u64,
    /// Stores the envelope value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub envelope: MessageEnvelopeSnapshot,
    /// Resolved routing audience captured when the message was accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<MessageAudienceSnapshot>,
}

/// Serializable accepted MMP message idempotency record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageAcceptedSnapshot {
    /// Stores the accepted at ms value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub accepted_at_ms: u64,
    /// Stores the envelope value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub envelope: MessageEnvelopeSnapshot,
    /// Stores the delivery value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub delivery: MessageDeliverySnapshot,
    /// Resolved routing audience captured when the message was accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<MessageAudienceSnapshot>,
}

/// Serializable resolved MMP routing audience.
///
/// The scope identifier is private snapshot routing metadata and is never
/// included in model-visible or MMP transport projections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageAudienceSnapshot {
    /// Audience kind: `project` or `session`.
    pub kind: String,
    /// Opaque trusted project scope for a project audience only.
    pub project_scope: Option<String>,
}

/// Serializable MMP delivery metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageDeliverySnapshot {
    /// Stores the accepted value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub accepted: bool,
    /// Stores the message id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub message_id: String,
    /// Stores the sequence value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub sequence: MessageSequence,
    /// Stores the queued recipients value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub queued_recipients: usize,
    /// Stores the status value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub status: String,
}

/// Serializable MMP envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageEnvelopeSnapshot {
    /// Stores the protocol value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub protocol: String,
    /// Stores the id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub id: String,
    /// Stores the message type value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub message_type: String,
    /// Stores the time value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub time: String,
    /// Stores the sender value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub sender: MessageIdentitySnapshot,
    /// Stores the recipient value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub recipient: MessageRecipientSnapshot,
    /// Stores the correlation id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub correlation_id: Option<String>,
    /// Stores the ttl ms value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub ttl_ms: Option<u64>,
    /// Stores the content type value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub content_type: String,
    /// Stores the payload value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub payload: String,
    /// Stores the extension fields value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub extension_fields: Vec<MessageExtensionFieldSnapshot>,
}

/// Serializable MMP recipient selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageRecipientSnapshot {
    /// Stores the kind value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub kind: String,
    /// Stores the value value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub value: Option<String>,
}

/// Serializable MMP extension field containing raw JSON value text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageExtensionFieldSnapshot {
    /// Stores the key value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub key: String,
    /// Stores the value json value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub value_json: String,
}
