//! Quarantined decoding of version-one immutable acceptance source records.
//!
//! Admission serializes compact JSON in a fixed, versioned order. This decoder
//! recognizes only that original representation, charges strings and entries
//! before retaining them, and preserves the original sequence, audience, TTL,
//! payload and recipient floors. It does not reconstruct a live registration,
//! offer, acknowledgment, receipt, or delivery capability. The product must
//! first verify the complete committed source and then fold all relevant store
//! evidence under the original trusted owner before publishing any live state.

use std::io::Read;

use serde::{Deserialize, Deserializer, de};

use super::record_reader::RecordReader;
use super::*;

/// Independent bounds supplied by the trusted destination, not stored JSON.
#[derive(Clone, Copy)]
pub struct MessageAcceptanceDecodeLimits {
    /// Maximum original payload bytes, independent of metadata.
    pub payload_bytes: usize,
    /// Maximum logical immutable envelope metadata charge.
    pub auxiliary_bytes: usize,
    /// Maximum original recipient bindings.
    pub recipient_count: usize,
    /// Maximum logical original recipient binding charge.
    pub recipient_bytes: usize,
}

/// Immutable, quarantined record with no live sender or recipient authority.
pub struct MessageAcceptanceSource {
    sequence: u64,
    accepted_at_ms: u64,
    sender_registered_after_sequence: u64,
    envelope: MessageEnvelopeSnapshot,
    audience: MessageAudienceSnapshot,
    recipient_bindings: Vec<MessageRecipientBindingSnapshot>,
}

impl MessageAcceptanceSource {
    /// Moves validated decoded payload bytes into an envelope without cloning them.
    /// This grants no registration, acceptance, delivery or storage authority.
    pub(in crate::messaging) fn into_envelope(self) -> Result<Envelope> {
        let snapshot = self.envelope;
        let sender = sender_identity_from_snapshot(&snapshot.sender, 9)?;
        let recipient = recipient_from_snapshot(&snapshot.recipient)?;
        Ok(Envelope {
            protocol: MMP_PROTOCOL,
            id: snapshot.id,
            message_type: snapshot.message_type,
            time: snapshot.time,
            sender,
            recipient,
            correlation_id: snapshot.correlation_id,
            ttl_ms: snapshot.ttl_ms,
            content_type: snapshot.content_type,
            payload: snapshot.payload,
            extension_fields: snapshot
                .extension_fields
                .into_iter()
                .map(|field| (field.key, field.value_json))
                .collect(),
        })
    }

    /// Returns the original occurrence sequence; it is not a receipt.
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    /// Returns the original acceptance clock value, without renewing TTL.
    pub fn accepted_at_ms(&self) -> u64 {
        self.accepted_at_ms
    }
    /// Returns the original sender interval, without granting registration authority.
    pub fn sender_registered_after_sequence(&self) -> u64 {
        self.sender_registered_after_sequence
    }
    /// Borrows the original envelope; the value remains quarantined evidence.
    pub fn envelope(&self) -> &MessageEnvelopeSnapshot {
        &self.envelope
    }
    /// Borrows the captured audience without resolving current membership.
    pub fn audience(&self) -> &MessageAudienceSnapshot {
        &self.audience
    }
    /// Borrows the originally captured recipients and their interval floors.
    pub fn recipient_bindings(&self) -> &[MessageRecipientBindingSnapshot] {
        &self.recipient_bindings
    }
}

/// Charges immutable metadata with the same policy used by admission.
struct MetadataBudget {
    remaining: usize,
}

impl MetadataBudget {
    /// Reserves the fixed envelope overhead before reading variable fields.
    fn new(limit: usize) -> Result<Self> {
        Ok(Self {
            remaining: limit.checked_sub(1024).ok_or_else(capacity)?,
        })
    }

    /// Computes the largest next string that fits before allocating its bytes.
    fn string_limit(&self) -> Result<usize> {
        self.remaining
            .checked_sub(64)
            .map(|value| value / 4)
            .ok_or_else(capacity)
    }

    /// Charges the actual decoded string or an absent optional field.
    fn charge(&mut self, bytes: usize) -> Result<()> {
        let charge = bytes
            .checked_mul(4)
            .and_then(|value| value.checked_add(64))
            .ok_or_else(capacity)?;
        self.remaining = self.remaining.checked_sub(charge).ok_or_else(capacity)?;
        Ok(())
    }

    /// Reads and charges one mandatory metadata string.
    fn string<R: Read>(&mut self, reader: &mut RecordReader<R>) -> Result<String> {
        let value = reader.string(self.string_limit()?)?;
        self.charge(value.len())?;
        Ok(value)
    }

    /// Reads and charges an optional field, including absent-field overhead.
    fn optional<R: Read>(&mut self, reader: &mut RecordReader<R>) -> Result<Option<String>> {
        let value = reader.optional_string(self.string_limit()?)?;
        self.charge(value.as_ref().map_or(0, String::len))?;
        Ok(value)
    }
}

/// Returns a stable capacity error without echoing private record contents.
fn capacity() -> MessageError {
    MessageError::invalid_args("MMP acceptance decoded metadata exceeds its budget")
}

/// Reads one exact schema-one source under caller-supplied resource limits.
///
/// Success validates framing and semantic fields but grants no custody or
/// runtime authority. The caller must separately verify the complete committed
/// storage source and bind it to its trusted original owner before any use.
pub fn decode_message_acceptance<R: Read>(
    input: R,
    limits: MessageAcceptanceDecodeLimits,
) -> Result<MessageAcceptanceSource> {
    let mut reader = RecordReader::new(input);
    let mut metadata = MetadataBudget::new(limits.auxiliary_bytes)?;
    reader.literal(b"{\"schema_version\":")?;
    if reader.unsigned()? != 1 {
        return Err(MessageError::invalid_args(
            "unsupported MMP acceptance schema",
        ));
    }
    reader.literal(b",\"sequence\":")?;
    let sequence = reader.unsigned()?;
    reader.literal(b",\"accepted_at_ms\":")?;
    let accepted_at_ms = reader.unsigned()?;
    reader.literal(b",\"sender_registered_after_sequence\":")?;
    let sender_floor = reader
        .optional_unsigned()?
        .ok_or_else(|| MessageError::invalid_args("MMP acceptance sender interval is missing"))?;
    reader.literal(b",\"envelope\":{\"protocol\":")?;
    let protocol = metadata.string(&mut reader)?;
    reader.literal(b",\"id\":")?;
    let id = metadata.string(&mut reader)?;
    reader.literal(b",\"message_type\":")?;
    let message_type = metadata.string(&mut reader)?;
    reader.literal(b",\"time\":")?;
    let time = metadata.string(&mut reader)?;
    reader.literal(b",\"sender\":{\"agent_id\":")?;
    let agent_id = metadata.string(&mut reader)?;
    reader.literal(b",\"pane_id\":")?;
    let pane_id = metadata.optional(&mut reader)?;
    reader.literal(b",\"window_id\":")?;
    let window_id = metadata.optional(&mut reader)?;
    reader.literal(b",\"role\":")?;
    let role = metadata.optional(&mut reader)?;
    reader.literal(b",\"capabilities\":[")?;
    let mut capabilities = Vec::new();
    while reader.peek()? != Some(b']') {
        if !capabilities.is_empty() {
            reader.literal(b",")?;
        }
        let value = metadata.string(&mut reader)?;
        capabilities.try_reserve(1).map_err(|_| capacity())?;
        capabilities.push(value);
    }
    reader.literal(b"],\"objective\":")?;
    let objective = metadata.optional(&mut reader)?;
    reader.literal(b",\"project_scope\":")?;
    let project_scope = metadata.optional(&mut reader)?;
    reader.literal(b"},\"recipient\":{\"kind\":")?;
    let kind = metadata.string(&mut reader)?;
    reader.literal(b",\"value\":")?;
    let value = metadata.optional(&mut reader)?;
    reader.literal(b"},\"correlation_id\":")?;
    let correlation_id = metadata.optional(&mut reader)?;
    reader.literal(b",\"ttl_ms\":")?;
    let ttl_ms = reader.optional_unsigned()?;
    reader.literal(b",\"content_type\":")?;
    let content_type = metadata.string(&mut reader)?;
    reader.literal(b",\"payload\":")?;
    let payload = reader.string(limits.payload_bytes)?;
    reader.literal(b",\"extension_fields\":[")?;
    let mut extension_fields = Vec::new();
    while reader.peek()? != Some(b']') {
        if !extension_fields.is_empty() {
            reader.literal(b",")?;
        }
        reader.literal(b"{\"key\":")?;
        let key = metadata.string(&mut reader)?;
        reader.literal(b",\"value_json\":")?;
        let value_json = metadata.string(&mut reader)?;
        reader.literal(b"}")?;
        extension_fields.try_reserve(1).map_err(|_| capacity())?;
        extension_fields.push(MessageExtensionFieldSnapshot { key, value_json });
    }
    reader.literal(b"]},\"audience\":{\"kind\":")?;
    let audience_kind = reader.string(7)?;
    reader.literal(b",\"project_scope\":")?;
    let audience_scope = reader.optional_string(project_scope.as_ref().map_or(0, String::len))?;
    reader.literal(b"},\"recipient_bindings\":[")?;
    let mut bindings = Vec::new();
    let mut remaining = limits.recipient_bytes;
    while reader.peek()? != Some(b']') {
        if bindings.len() >= limits.recipient_count {
            return Err(capacity());
        }
        if !bindings.is_empty() {
            reader.literal(b",")?;
        }
        reader.literal(b"{\"recipient\":")?;
        let maximum = remaining.checked_sub(192).ok_or_else(capacity)?;
        let recipient = reader.string(maximum)?;
        remaining = maximum.checked_sub(recipient.len()).ok_or_else(capacity)?;
        reader.literal(b",\"registered_after_sequence\":")?;
        let floor = reader.unsigned()?;
        reader.literal(b"}")?;
        bindings.try_reserve(1).map_err(|_| capacity())?;
        bindings.push(MessageRecipientBindingSnapshot {
            recipient,
            registered_after_sequence: floor,
        });
    }
    reader.literal(b"]}")?;
    reader.finish()?;
    let candidate = MessageAcceptanceSource {
        sequence,
        accepted_at_ms,
        sender_registered_after_sequence: sender_floor,
        envelope: MessageEnvelopeSnapshot {
            protocol,
            id,
            message_type,
            time,
            sender: MessageIdentitySnapshot {
                agent_id,
                pane_id,
                window_id,
                role,
                capabilities,
                objective,
                project_scope,
            },
            recipient: MessageRecipientSnapshot { kind, value },
            correlation_id,
            ttl_ms,
            content_type,
            payload,
            extension_fields,
        },
        audience: MessageAudienceSnapshot {
            kind: audience_kind,
            project_scope: audience_scope,
        },
        recipient_bindings: bindings,
    };
    validate(&candidate)?;
    Ok(candidate)
}

/// Checks original semantic constraints without publishing any live authority.
fn validate(record: &MessageAcceptanceSource) -> Result<()> {
    if record.sequence == 0
        || record.sender_registered_after_sequence >= record.sequence
        || record.recipient_bindings.is_empty()
    {
        return Err(MessageError::invalid_args(
            "MMP acceptance sequence or bindings are invalid",
        ));
    }
    validate_protocol(&record.envelope.protocol)?;
    validate_message_type(&record.envelope.message_type)?;
    validate_sender(&record.envelope.sender)?;
    match (
        record.audience.kind.as_str(),
        record.audience.project_scope.as_deref(),
    ) {
        ("session", None) => {}
        ("project", Some(scope))
            if record.envelope.sender.project_scope.as_deref() == Some(scope)
                && ProjectScopeId::from_snapshot_value(scope).is_some() => {}
        _ => {
            return Err(MessageError::invalid_args(
                "MMP acceptance audience is invalid",
            ));
        }
    }
    let recipient = validate_selector(&record.envelope.recipient)?;
    if record.envelope.id.is_empty()
        || record.envelope.time.is_empty()
        || record.envelope.content_type.is_empty()
        || record.envelope.ttl_ms == Some(0)
    {
        return Err(MessageError::invalid_args(
            "MMP acceptance envelope fields are invalid",
        ));
    }
    for field in &record.envelope.extension_fields {
        let mut json = serde_json::Deserializer::from_str(&field.value_json);
        if field.key.is_empty() || ValidJson::deserialize(&mut json).is_err() || json.end().is_err()
        {
            return Err(MessageError::invalid_args(
                "MMP acceptance extension is invalid",
            ));
        }
    }
    let mut previous: Option<&str> = None;
    for binding in &record.recipient_bindings {
        validate_id(&binding.recipient)?;
        if binding.registered_after_sequence >= record.sequence
            || previous.is_some_and(|value| value >= binding.recipient.as_str())
            || super::super::auxiliary::outcome_bytes(&record.envelope.id, &binding.recipient)
                > super::super::auxiliary::OUTCOME_BYTES
        {
            return Err(MessageError::invalid_args(
                "MMP acceptance recipient interval is invalid",
            ));
        }
        previous = Some(&binding.recipient);
    }
    if let Some(id) = recipient
        && (record.recipient_bindings.len() != 1 || record.recipient_bindings[0].recipient != id)
    {
        return Err(MessageError::invalid_args(
            "MMP acceptance direct recipient disagrees with bindings",
        ));
    }
    Ok(())
}

/// Validates extension JSON with the same number parser as a JSON value while
/// retaining no object or array tree. Serde's ignored-value parser only checks
/// numeric syntax and would otherwise admit out-of-range floating-point values.
struct ValidJson;

impl<'de> Deserialize<'de> for ValidJson {
    /// Visits each value recursively without retaining keys, values or arrays.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct Visitor;
        impl<'de> de::Visitor<'de> for Visitor {
            type Value = ValidJson;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a valid JSON value")
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> std::result::Result<ValidJson, E> {
                Ok(ValidJson)
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> std::result::Result<ValidJson, E> {
                Ok(ValidJson)
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> std::result::Result<ValidJson, E> {
                Ok(ValidJson)
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> std::result::Result<ValidJson, E> {
                Ok(ValidJson)
            }
            fn visit_str<E: de::Error>(self, _: &str) -> std::result::Result<ValidJson, E> {
                Ok(ValidJson)
            }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<ValidJson, E> {
                Ok(ValidJson)
            }
            fn visit_seq<A: de::SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> std::result::Result<ValidJson, A::Error> {
                while sequence.next_element::<ValidJson>()?.is_some() {}
                Ok(ValidJson)
            }
            fn visit_map<A: de::MapAccess<'de>>(
                self,
                mut object: A,
            ) -> std::result::Result<ValidJson, A::Error> {
                while object.next_key::<ValidJson>()?.is_some() {
                    object.next_value::<ValidJson>()?;
                }
                Ok(ValidJson)
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

/// Checks one stored opaque identifier without allocating a second copy.
fn validate_id(value: &str) -> Result<()> {
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(MessageError::invalid_args(
            "MMP acceptance identity is invalid",
        ));
    }
    Ok(())
}

/// Checks sender fields without constructing a normalized objective or copies.
fn validate_sender(sender: &MessageIdentitySnapshot) -> Result<()> {
    validate_id(&sender.agent_id)?;
    for id in [sender.pane_id.as_deref(), sender.window_id.as_deref()]
        .into_iter()
        .flatten()
    {
        validate_id(id)?;
    }
    if sender
        .role
        .as_ref()
        .is_some_and(|value| value.is_empty() || value.chars().any(char::is_control))
        || sender
            .capabilities
            .iter()
            .any(|value| value.is_empty() || value.chars().any(char::is_control))
        || sender
            .project_scope
            .as_deref()
            .is_some_and(|value| ProjectScopeId::from_snapshot_value(value).is_none())
        || sender.objective.as_deref().is_some_and(|value| {
            value.trim().is_empty()
                || value.len() > super::super::super::validation::MMP_OBJECTIVE_MAX_BYTES
                || value
                    .split_whitespace()
                    .any(|word| word.chars().any(char::is_control))
        })
    {
        return Err(MessageError::invalid_args(
            "MMP acceptance sender is invalid",
        ));
    }
    Ok(())
}

/// Validates selector syntax and returns a direct agent target when present.
fn validate_selector(selector: &MessageRecipientSnapshot) -> Result<Option<&str>> {
    let value = selector.value.as_deref();
    match (selector.kind.as_str(), value) {
        ("session", None) => Ok(None),
        ("agent", Some(id)) => {
            validate_id(id)?;
            Ok(Some(id))
        }
        ("pane" | "window", Some(id)) => {
            validate_id(id)?;
            Ok(None)
        }
        // Existing registration and admission can capture an empty selector
        // when a registered target advertises the matching empty value.
        ("role" | "capability" | "group", Some(_)) => Ok(None),
        _ => Err(MessageError::invalid_args(
            "MMP acceptance selector is invalid",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes a real unpublished admission, preserving the sender's actual
    /// registration floor and the exact binding captured by the lower owner.
    fn source(payload: &str) -> (Vec<u8>, String) {
        let mut service = MessageService::default();
        let sender = service.register_agent(None, None, "sender", vec!["writer".into()]);
        let recipient = service.register_agent(None, None, "target", vec![]);
        let target = recipient.agent_id.to_string();
        let envelope = Envelope {
            protocol: MMP_PROTOCOL,
            id: "decoder-original".into(),
            message_type: "send".into(),
            time: "fixture-time".into(),
            sender: sender.clone(),
            recipient: Recipient::Agent(recipient.agent_id),
            correlation_id: Some("correlation".into()),
            ttl_ms: Some(30_000),
            content_type: "text/plain".into(),
            payload: payload.into(),
            extension_fields: vec![("test.extension".into(), "{\"x\":[1,true]}".into())],
        };
        let ticket = service
            .prepare_acceptance(
                &sender.agent_id,
                envelope,
                MessageScope::Session,
                42,
                4096,
                None,
            )
            .unwrap();
        let write = service.begin_acceptance_write(&ticket).unwrap();
        (serde_json::to_vec(&write).unwrap(), target)
    }

    /// Supplies the same production ceilings used by the existing lower owner.
    fn limits() -> MessageAcceptanceDecodeLimits {
        MessageAcceptanceDecodeLimits {
            payload_bytes: 1024 * 1024,
            auxiliary_bytes: 16 * 1024 * 1024,
            recipient_count: 4096,
            recipient_bytes: 1024 * 1024,
        }
    }

    /// A real admission round-trips its complete immutable payload, clocks,
    /// audience, original recipient and sender floors, including JSON escapes.
    /// The resulting value is descriptive and is never installed as live mail.
    #[test]
    fn real_admission_decodes_exact_original_without_live_publication() {
        let payload = "\0\"\n\\é😀".repeat(20_000);
        let (encoded, target) = source(&payload);
        let decoded = decode_message_acceptance(encoded.as_slice(), limits()).unwrap();
        assert_eq!(decoded.sequence, 1);
        assert_eq!(decoded.accepted_at_ms, 42);
        assert_eq!(decoded.sender_registered_after_sequence, 0);
        assert_eq!(decoded.envelope.payload, payload);
        assert_eq!(decoded.envelope.ttl_ms, Some(30_000));
        assert_eq!(
            decoded.envelope.correlation_id.as_deref(),
            Some("correlation")
        );
        assert_eq!(
            decoded.envelope.extension_fields[0].value_json,
            "{\"x\":[1,true]}"
        );
        assert_eq!(decoded.audience.kind, "session");
        assert_eq!(decoded.audience.project_scope, None);
        assert_eq!(decoded.recipient_bindings.len(), 1);
        assert_eq!(decoded.recipient_bindings[0].recipient, target);
        assert_eq!(decoded.recipient_bindings[0].registered_after_sequence, 0);
    }

    /// Each independent resource bound is enforced before the corresponding
    /// payload, metadata or recipient collection can be retained in full.
    #[test]
    fn decoder_enforces_independent_payload_metadata_and_binding_limits() {
        let (encoded, target) = source("12345678");
        let original = decode_message_acceptance(encoded.as_slice(), limits()).unwrap();
        let envelope = envelope_from_snapshot(&original.envelope, 9).unwrap();
        let exact_metadata = super::super::super::auxiliary::envelope_bytes(&envelope);
        let mut exact = limits();
        exact.payload_bytes = 8;
        exact.auxiliary_bytes = exact_metadata;
        exact.recipient_bytes = 192 + target.len();
        exact.recipient_count = 1;
        assert!(decode_message_acceptance(encoded.as_slice(), exact).is_ok());

        let mut policy = limits();
        policy.payload_bytes = 7;
        assert!(decode_message_acceptance(encoded.as_slice(), policy).is_err());
        let mut policy = limits();
        policy.auxiliary_bytes = exact_metadata - 1;
        assert!(decode_message_acceptance(encoded.as_slice(), policy).is_err());
        let mut policy = limits();
        policy.recipient_count = 0;
        assert!(decode_message_acceptance(encoded.as_slice(), policy).is_err());
        let mut policy = limits();
        policy.recipient_bytes = 192 + target.len() - 1;
        assert!(decode_message_acceptance(encoded.as_slice(), policy).is_err());
        assert!(decode_message_acceptance(encoded.as_slice(), limits()).is_ok());
    }

    /// Project fanout retains the exact ordered, already-resolved membership
    /// captured at acceptance, including both sender and target when eligible.
    /// A different project identity must not appear in the immutable audience.
    #[test]
    fn real_project_fanout_retains_scope_and_sorted_original_bindings() {
        let mut service = MessageService::default();
        let mut sender = service.register_agent(None, None, "sender", vec![]);
        let mut target = service.register_agent(None, None, "target", vec![]);
        let foreign = service.register_agent(None, None, "foreign", vec![]);
        let scope = ProjectScopeId::from_canonical_root_bytes(b"/project");
        sender.project_scope = Some(scope.clone());
        target.project_scope = Some(scope.clone());
        let sender = service.ensure_agent_identity(sender, 0).unwrap();
        let target = service.ensure_agent_identity(target, 0).unwrap();
        let envelope = Envelope {
            protocol: MMP_PROTOCOL,
            id: "project-message".into(),
            message_type: "send".into(),
            time: "fixture-time".into(),
            sender: sender.clone(),
            recipient: Recipient::Session,
            correlation_id: None,
            ttl_ms: None,
            content_type: "text/plain".into(),
            payload: "original".into(),
            extension_fields: vec![],
        };
        let ticket = service
            .prepare_acceptance(
                &sender.agent_id,
                envelope,
                MessageScope::Project,
                123,
                4096,
                None,
            )
            .unwrap();
        let write = service.begin_acceptance_write(&ticket).unwrap();
        let source = serde_json::to_vec(&write).unwrap();
        let decoded = decode_message_acceptance(source.as_slice(), limits()).unwrap();
        assert_eq!(decoded.audience.kind, "project");
        assert_eq!(
            decoded.audience.project_scope.as_deref(),
            Some(scope.snapshot_value())
        );
        let mut expected = vec![sender.agent_id.to_string(), target.agent_id.to_string()];
        expected.sort();
        assert_eq!(
            decoded
                .recipient_bindings
                .iter()
                .map(|item| item.recipient.clone())
                .collect::<Vec<_>>(),
            expected
        );
        assert!(
            !decoded
                .recipient_bindings
                .iter()
                .any(|item| item.recipient == foreign.agent_id.as_str())
        );
    }

    /// Existing registration permits an empty recipient role or capability,
    /// even though the sending identity is validated. Admission can therefore
    /// commit these selectors when they match an actual registered recipient;
    /// restoring the original must preserve them instead of stranding mail.
    #[test]
    fn decoder_preserves_real_admissions_with_empty_recipient_selectors() {
        for selector in [
            Recipient::Role(String::new()),
            Recipient::Capability(String::new()),
            Recipient::Group(String::new()),
        ] {
            let mut service = MessageService::default();
            let sender = service.register_agent(None, None, "sender", vec![]);
            let target = service.register_agent(None, None, "", vec![String::new()]);
            let envelope = Envelope {
                protocol: MMP_PROTOCOL,
                id: "empty-selector".into(),
                message_type: "send".into(),
                time: "fixture-time".into(),
                sender: sender.clone(),
                recipient: selector,
                correlation_id: None,
                ttl_ms: None,
                content_type: "text/plain".into(),
                payload: "original".into(),
                extension_fields: vec![],
            };
            let ticket = service
                .prepare_acceptance(
                    &sender.agent_id,
                    envelope,
                    MessageScope::Session,
                    1,
                    4096,
                    None,
                )
                .unwrap();
            let write = service.begin_acceptance_write(&ticket).unwrap();
            let encoded = serde_json::to_vec(&write).unwrap();
            let decoded = decode_message_acceptance(encoded.as_slice(), limits()).unwrap();
            assert_eq!(decoded.envelope.recipient.value.as_deref(), Some(""));
            assert_eq!(decoded.recipient_bindings.len(), 1);
            assert_eq!(
                decoded.recipient_bindings[0].recipient,
                target.agent_id.as_str()
            );
        }
    }

    /// The original source is authoritative only in its supported version and
    /// canonical framing. Invalid intervals, contradictory audiences, unknown
    /// fields, and trailing material must not become plausible restore inputs.
    #[test]
    fn decoder_rejects_forged_semantics_unknown_fields_and_trailing_input() {
        let (encoded, target) = source("original");
        let original = String::from_utf8(encoded).unwrap();
        let binding = format!("{{\"recipient\":\"{target}\",\"registered_after_sequence\":0}}");
        let cases = [
            original.replacen("\"schema_version\":1", "\"schema_version\":2", 1),
            original.replacen("\"sequence\":1", "\"sequence\":0", 1),
            original.replacen(
                "\"sender_registered_after_sequence\":0",
                "\"sender_registered_after_sequence\":1",
                1,
            ),
            original.replacen(
                "\"kind\":\"session\",\"project_scope\":null",
                "\"kind\":\"project\",\"project_scope\":null",
                1,
            ),
            original.replacen(&binding, &format!("{binding},{binding}"), 1),
            original.replacen("\"payload\":", "\"unexpected\":true,\"payload\":", 1),
            format!("{original} "),
        ];
        for (index, invalid) in cases.iter().enumerate() {
            assert!(
                decode_message_acceptance(invalid.as_bytes(), limits()).is_err(),
                "invalid case {index}"
            );
        }
    }

    /// Extension JSON must satisfy the same numerical range accepted at the
    /// original admission boundary, even when validation does not retain a
    /// dynamic JSON tree. A syntactically valid overflowing exponent is not a
    /// valid serde_json::Value and must not pass restoration validation.
    #[test]
    fn decoder_rejects_extension_number_outside_admission_range() {
        let (encoded, _) = source("original");
        let original = String::from_utf8(encoded).unwrap();
        let altered = original.replace(
            "\"value_json\":\"{\\\"x\\\":[1,true]}\"",
            "\"value_json\":\"1e999\"",
        );
        assert_ne!(altered, original);
        assert!(serde_json::from_str::<serde_json::Value>("1e999").is_err());
        assert!(decode_message_acceptance(altered.as_bytes(), limits()).is_err());
    }
}
