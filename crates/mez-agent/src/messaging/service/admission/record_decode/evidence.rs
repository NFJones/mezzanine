//! Payloadless preservation of immutable original-source attribution.
//!
//! The separate framing retains source/payload fingerprints, never payload or
//! live rights. Its JSON suffix uses the original grammar with an empty payload,
//! so the same bounded semantic decoder validates all other fields. The outer
//! frame is NOT an acceptance source. Callers must qualify original checksums,
//! settlement, ownership and complete dependencies before persisting or using it;
//! decoding cannot authenticate the claimed original source or omitted payload.

use super::*;
use serde::{Serialize, Serializer, ser::SerializeSeq};
use std::io::{self, Write};

const MAGIC: &[u8; 8] = b"MMPAME01";
const HEADER_BYTES: usize = 88;

/// Descriptive preserved source, with no conversion to mail, a receipt or a
/// recovered occurrence. Storage must independently qualify its provenance.
pub struct MessageAcceptanceMetadataEvidence {
    metadata: MessageAcceptanceMetadata,
    source_sha256: [u8; 32],
    original_encoded_bytes: usize,
}

impl MessageAcceptanceMetadataEvidence {
    /// Borrows immutable attribution and an explicitly bodyless fingerprint.
    pub fn metadata(&self) -> &MessageAcceptanceMetadata {
        &self.metadata
    }
    /// Claimed original full-source checksum, not verified by this decoder.
    pub fn source_sha256(&self) -> &[u8; 32] {
        &self.source_sha256
    }
    /// Original encoded size, checked against the caller's independent limit.
    pub fn original_encoded_bytes(&self) -> usize {
        self.original_encoded_bytes
    }
}

impl MessageAcceptanceMetadata {
    /// Streams a separate version-one preservation record without retaining a
    /// second source, copying attribution vectors or materializing payloads.
    /// The caller supplies already-qualified ORIGINAL checksum/encoded size and
    /// must bound/charge its output. Partial output on error must be discarded.
    /// Success grants no retirement, physical verification or cleanup authority.
    pub fn write_metadata_evidence<W: Write>(
        &self,
        mut output: W,
        source_sha256: &[u8; 32],
        original_encoded_bytes: usize,
    ) -> Result<()> {
        if original_encoded_bytes == 0 || original_encoded_bytes < self.envelope.payload.bytes() {
            return Err(invalid());
        }
        let original = u64::try_from(original_encoded_bytes).map_err(|_| invalid())?;
        let payload = u64::try_from(self.envelope.payload.bytes()).map_err(|_| invalid())?;
        output.write_all(MAGIC).map_err(write_error)?;
        output.write_all(source_sha256).map_err(write_error)?;
        output
            .write_all(&original.to_le_bytes())
            .map_err(write_error)?;
        output
            .write_all(&payload.to_le_bytes())
            .map_err(write_error)?;
        output
            .write_all(self.envelope.payload.sha256())
            .map_err(write_error)?;
        serde_json::to_writer(&mut output, &Record(self))
            .map_err(|_| write_error(io::Error::other("encoding")))
    }
}

/// Decodes ONLY payloadless evidence under independent source/metadata limits.
/// The unchanged acceptance decoder rejects this frame. A nonempty JSON-suffix
/// payload, future format, trailing bytes, oversized dimensions or malformed
/// attribution refuses in full. Checksums of omitted bytes remain descriptive.
pub fn decode_message_acceptance_metadata_evidence<R: Read>(
    mut input: R,
    limits: MessageAcceptanceDecodeLimits,
    original_encoded_limit: usize,
) -> Result<MessageAcceptanceMetadataEvidence> {
    let mut header = [0u8; HEADER_BYTES];
    input.read_exact(&mut header).map_err(|_| invalid())?;
    if &header[..8] != MAGIC {
        return Err(invalid());
    }
    let source_sha256 = header[8..40].try_into().map_err(|_| invalid())?;
    let original_encoded_bytes = number(&header[40..48])?;
    let payload_bytes = number(&header[48..56])?;
    let payload_sha256 = header[56..88].try_into().map_err(|_| invalid())?;
    if original_encoded_bytes == 0
        || original_encoded_bytes > original_encoded_limit
        || payload_bytes > limits.payload_bytes
        || payload_bytes > original_encoded_bytes
    {
        return Err(invalid());
    }
    let mut counted = CountedRead { input, bytes: 0 };
    let mut metadata = decode_message_acceptance_metadata(
        &mut counted,
        MessageAcceptanceDecodeLimits {
            payload_bytes: 0,
            ..limits
        },
    )?;
    let minimum = counted
        .bytes
        .checked_add(payload_bytes)
        .ok_or_else(invalid)?;
    if original_encoded_bytes < minimum
        || (payload_bytes == 0
            && (original_encoded_bytes != minimum
                || &payload_sha256 != metadata.envelope.payload.sha256()))
    {
        return Err(invalid());
    }
    metadata.envelope.payload = super::super::record_reader::MessagePayloadFingerprint::preserved(
        payload_bytes,
        payload_sha256,
    );
    Ok(MessageAcceptanceMetadataEvidence {
        metadata,
        source_sha256,
        original_encoded_bytes,
    })
}

fn number(bytes: &[u8]) -> Result<usize> {
    usize::try_from(u64::from_le_bytes(bytes.try_into().map_err(|_| invalid())?))
        .map_err(|_| invalid())
}

struct CountedRead<R> {
    input: R,
    bytes: usize,
}
impl<R: Read> Read for CountedRead<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let count = self.input.read(output)?;
        self.bytes = self
            .bytes
            .checked_add(count)
            .ok_or_else(|| io::Error::other("size overflow"))?;
        Ok(count)
    }
}

/// Borrowed spelling preserves explicit null identity fields, unlike generic
/// snapshot serialization, and the exact schema1/2/3/4 absence distinctions.
struct Record<'a>(&'a MessageAcceptanceMetadata);
impl Serialize for Record<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Identity<'a> {
            agent_id: &'a str,
            pane_id: &'a Option<String>,
            window_id: &'a Option<String>,
            role: &'a Option<String>,
            capabilities: &'a [String],
            objective: &'a Option<String>,
            project_scope: &'a Option<String>,
        }
        #[derive(Serialize)]
        struct Envelope<'a> {
            protocol: &'a str,
            id: &'a str,
            message_type: &'a str,
            time: &'a str,
            sender: Identity<'a>,
            recipient: &'a MessageRecipientSnapshot,
            correlation_id: &'a Option<String>,
            ttl_ms: Option<u64>,
            content_type: &'a str,
            payload: &'static str,
            extension_fields: &'a [MessageExtensionFieldSnapshot],
        }
        #[derive(Serialize)]
        struct Source<'a> {
            schema_version: u32,
            sequence: u64,
            accepted_at_ms: u64,
            sender_registered_after_sequence: Option<u64>,
            envelope: Envelope<'a>,
            audience: &'a MessageAudienceSnapshot,
            recipient_bindings: &'a [MessageRecipientBindingSnapshot],
            #[serde(skip_serializing_if = "Option::is_none")]
            native_bindings: Option<&'a super::super::super::native_binding::NativeBindings>,
            #[serde(skip_serializing_if = "Option::is_none")]
            recipient_registrations: Option<Namespaces<'a>>,
            #[serde(skip_serializing_if = "Option::is_none")]
            custody_clock: Option<&'a [u8; 16]>,
        }
        let source = self.0;
        let envelope = &source.envelope;
        let sender = &envelope.sender;
        Source {
            schema_version: if source.custody_clock.is_some() {
                4
            } else if source.registration_bindings.is_some() {
                3
            } else if source.native_bindings.is_some() {
                2
            } else {
                1
            },
            sequence: source.sequence,
            accepted_at_ms: source.accepted_at_ms,
            sender_registered_after_sequence: Some(source.sender_registered_after_sequence),
            envelope: Envelope {
                protocol: &envelope.protocol,
                id: &envelope.id,
                message_type: &envelope.message_type,
                time: &envelope.time,
                sender: Identity {
                    agent_id: &sender.agent_id,
                    pane_id: &sender.pane_id,
                    window_id: &sender.window_id,
                    role: &sender.role,
                    capabilities: &sender.capabilities,
                    objective: &sender.objective,
                    project_scope: &sender.project_scope,
                },
                recipient: &envelope.recipient,
                correlation_id: &envelope.correlation_id,
                ttl_ms: envelope.ttl_ms,
                content_type: &envelope.content_type,
                payload: "",
                extension_fields: &envelope.extension_fields,
            },
            audience: &source.audience,
            recipient_bindings: &source.recipient_bindings,
            native_bindings: source.native_bindings.as_ref(),
            recipient_registrations: source.registration_bindings.as_ref().map(Namespaces),
            custody_clock: source.custody_clock.as_ref(),
        }
        .serialize(serializer)
    }
}

struct Namespaces<'a>(&'a super::super::super::registration_binding::RegistrationBindings);
impl Serialize for Namespaces<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.0.recipients.len()))?;
        for binding in &self.0.recipients {
            sequence.serialize_element(&binding.namespace)?;
        }
        sequence.end()
    }
}

fn invalid() -> MessageError {
    MessageError::invalid_args("MMP metadata evidence framing is invalid")
}
fn write_error(_: io::Error) -> MessageError {
    MessageError::invalid_state("MMP metadata evidence write unavailable")
}
