//! Borrowed version1 admission record encoding for trusted off-actor storage.
//!
//! Encoding visits frozen fields directly, without copying identity or payload
//! strings. The adapter must bound its output/streaming buffers and commit the
//! complete record before success. No runtime ticket or authority is serialized.

use super::*;
use serde::{Serialize, Serializer, ser::SerializeSeq};

/// Borrowed original sender fields, compatible with snapshot identity spelling.
#[derive(Serialize)]
struct Identity<'a> {
    agent_id: &'a str,
    pane_id: Option<&'a str>,
    window_id: Option<&'a str>,
    role: Option<&'a str>,
    capabilities: &'a [String],
    objective: Option<&'a str>,
    project_scope: Option<&'a str>,
}
/// Borrowed original selector, not a newly resolved recipient set.
#[derive(Serialize)]
struct Selector<'a> {
    kind: &'static str,
    value: Option<&'a str>,
}
/// Borrowed original project/session audience.
#[derive(Serialize)]
struct Audience<'a> {
    kind: &'static str,
    project_scope: Option<&'a str>,
}
/// Frozen envelope view with no independently owned payload buffer.
#[derive(Serialize)]
struct Input<'a> {
    protocol: &'a str,
    id: &'a str,
    message_type: &'a str,
    time: &'a str,
    sender: Identity<'a>,
    recipient: Selector<'a>,
    correlation_id: Option<&'a str>,
    ttl_ms: Option<u64>,
    content_type: &'a str,
    payload: &'a str,
    extension_fields: Extensions<'a>,
}
/// Borrowed immutable extension entries.
struct Extensions<'a>(&'a [(String, String)]);
/// Borrowed frozen recipient intervals.
struct Bindings<'a>(&'a [(AgentId, MessageSequence)]);
/// Explicitly versioned immutable record; a byte representation is not a receipt.
#[derive(Serialize)]
struct Record<'a> {
    schema_version: u32,
    sequence: MessageSequence,
    accepted_at_ms: u64,
    sender_registered_after_sequence: Option<MessageSequence>,
    envelope: Input<'a>,
    audience: Audience<'a>,
    recipient_bindings: Bindings<'a>,
}

impl Serialize for Extensions<'_> {
    /// Serializes extensions without interpreting untrusted raw JSON values.
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Field<'a> {
            key: &'a str,
            value_json: &'a str,
        }
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for (key, value) in self.0 {
            seq.serialize_element(&Field {
                key,
                value_json: value,
            })?;
        }
        seq.end()
    }
}
impl Serialize for Bindings<'_> {
    /// Preserves the original stable binding order and exact registration floor.
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Binding<'a> {
            recipient: &'a str,
            registered_after_sequence: MessageSequence,
        }
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for (id, floor) in self.0 {
            seq.serialize_element(&Binding {
                recipient: id.as_str(),
                registered_after_sequence: *floor,
            })?;
        }
        seq.end()
    }
}
impl Serialize for MessageAcceptanceWrite {
    /// Streams the same immutable canonical view used by source fingerprints.
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        FrozenRecord(&self.row).serialize(serializer)
    }
}

/// Borrowed canonical fields, independent of mutable ACK/offer/terminal state.
struct FrozenRecord<'a>(&'a QueuedEnvelope);
impl Serialize for FrozenRecord<'_> {
    /// Streams only the original immutable semantic record, never live capabilities.
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let envelope = self
            .0
            .envelope
            .resident()
            .map_err(serde::ser::Error::custom)?;
        let sender = &envelope.sender;
        let (kind, value) = match &envelope.recipient {
            Recipient::Agent(id) => ("agent", Some(id.as_str())),
            Recipient::Pane(id) => ("pane", Some(id.as_str())),
            Recipient::Window(id) => ("window", Some(id.as_str())),
            Recipient::Session => ("session", None),
            Recipient::Role(value) => ("role", Some(value.as_str())),
            Recipient::Capability(value) => ("capability", Some(value.as_str())),
            Recipient::Group(value) => ("group", Some(value.as_str())),
        };
        let audience = match &self.0.audience {
            ResolvedMessageAudience::Session => Audience {
                kind: "session",
                project_scope: None,
            },
            ResolvedMessageAudience::Project(scope) => Audience {
                kind: "project",
                project_scope: Some(scope.snapshot_value()),
            },
        };
        Record {
            schema_version: 1,
            sequence: self.0.sequence,
            accepted_at_ms: self.0.accepted_at_ms,
            sender_registered_after_sequence: self.0.sender_registered_after_sequence,
            envelope: Input {
                protocol: envelope.protocol,
                id: &envelope.id,
                message_type: &envelope.message_type,
                time: &envelope.time,
                sender: Identity {
                    agent_id: sender.agent_id.as_str(),
                    pane_id: sender.pane_id.as_ref().map(PaneId::as_str),
                    window_id: sender.window_id.as_ref().map(WindowId::as_str),
                    role: sender.role.as_deref(),
                    capabilities: &sender.capabilities,
                    objective: sender.objective.as_deref(),
                    project_scope: sender
                        .project_scope
                        .as_ref()
                        .map(ProjectScopeId::snapshot_value),
                },
                recipient: Selector { kind, value },
                correlation_id: envelope.correlation_id.as_deref(),
                ttl_ms: envelope.ttl_ms,
                content_type: &envelope.content_type,
                payload: &envelope.payload,
                extension_fields: Extensions(&envelope.extension_fields),
            },
            audience,
            recipient_bindings: Bindings(&self.0.recipient_bindings),
        }
        .serialize(serializer)
    }
}

/// Hashes the exact accepted record off actor without an encoded payload copy.
pub(super) fn source_digest(
    row: &QueuedEnvelope,
) -> std::result::Result<[u8; 32], serde_json::Error> {
    use sha2::{Digest, Sha256};
    /// Streaming checksum sink with no retained serialization buffer.
    struct HashWriter(Sha256);
    impl std::io::Write for HashWriter {
        /// Consumes arbitrary borrowed encoder fragments without copying them.
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        /// Hashing has no buffered output to flush.
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut hash = HashWriter(Sha256::new());
    serde_json::to_writer(&mut hash, &FrozenRecord(row))?;
    Ok(hash.0.finalize().into())
}
