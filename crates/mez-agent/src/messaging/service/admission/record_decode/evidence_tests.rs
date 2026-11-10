//! Preservation framing is separate from acceptance and retains no body rights.

use super::{
    tests::{limits, source},
    *,
};
use sha2::{Digest, Sha256};
use std::io::{self, Read, Write};

/// Canonical format fixtures extend a real legacy admission with explicit raw
/// classification/namespaces/elapsed fields. They are codec data, not live caps;
/// real native source preservation is also tested at the product root boundary.
fn original(version: u32, payload: &str) -> Vec<u8> {
    let (bytes, _) = source(payload);
    let mut text = String::from_utf8(bytes).unwrap();
    text = text.replacen(
        "\"schema_version\":1",
        &format!("\"schema_version\":{version}"),
        1,
    );
    text.pop();
    if version >= 2 {
        text.push_str(",\"native_bindings\":{\"sender\":null,\"recipients\":[null]}");
    }
    if version >= 3 {
        let namespace = *uuid::Uuid::new_v4().as_bytes();
        text.push_str(&format!(
            ",\"recipient_registrations\":[{}]",
            serde_json::to_string(&namespace).unwrap()
        ));
    }
    if version >= 4 {
        text.push_str(&format!(
            ",\"custody_clock\":{}",
            serde_json::to_string(&[9u8; 16]).unwrap()
        ));
    }
    text.push('}');
    text.into_bytes()
}

fn preservation(bytes: &[u8]) -> (MessageAcceptanceMetadata, Vec<u8>) {
    let source = decode_message_acceptance_metadata(bytes, limits()).unwrap();
    let mut evidence = Vec::new();
    source
        .write_metadata_evidence(&mut evidence, &Sha256::digest(bytes).into(), bytes.len())
        .unwrap();
    (source, evidence)
}

#[test]
fn metadata_evidence_roundtrips_all_supported_source_fields_without_payload() {
    for version in 1..=4 {
        for payload in [String::new(), "OMITTED_BODY\0\"\n\\é😀".repeat(12_000)] {
            let bytes = original(version, &payload);
            let (source, evidence) = preservation(&bytes);
            let decoded = decode_message_acceptance_metadata_evidence(
                evidence.as_slice(),
                limits(),
                bytes.len(),
            )
            .unwrap();
            let actual = decoded.metadata();
            assert_eq!(actual.envelope(), source.envelope());
            assert_eq!(actual.sequence(), source.sequence());
            assert_eq!(actual.accepted_at_ms(), source.accepted_at_ms());
            assert_eq!(
                actual.sender_registered_after_sequence(),
                source.sender_registered_after_sequence()
            );
            assert_eq!(actual.audience(), source.audience());
            assert_eq!(actual.recipient_bindings(), source.recipient_bindings());
            assert_eq!(
                actual.has_native_binding_record(),
                source.has_native_binding_record()
            );
            assert_eq!(actual.native_sender(), source.native_sender());
            assert_eq!(actual.native_recipient(0), source.native_recipient(0));
            assert_eq!(
                actual.recipient_registration_namespace(0),
                source.recipient_registration_namespace(0)
            );
            assert_eq!(actual.custody_clock_domain(), source.custody_clock_domain());
            assert_eq!(
                decoded.source_sha256(),
                &<[u8; 32]>::from(Sha256::digest(&bytes))
            );
            assert_eq!(decoded.original_encoded_bytes(), bytes.len());
            let expected = String::from_utf8(bytes.clone()).unwrap().replacen(
                &format!("\"payload\":{}", serde_json::to_string(&payload).unwrap()),
                "\"payload\":\"\"",
                1,
            );
            assert_eq!(
                &evidence[88..],
                expected.as_bytes(),
                "preserve exact canonical metadata spelling"
            );
            assert!(
                !evidence
                    .windows(b"OMITTED_BODY".len())
                    .any(|part| part == b"OMITTED_BODY")
            );
            assert!(decode_message_acceptance(evidence.as_slice(), limits()).is_err());
            assert!(decode_message_acceptance_metadata(evidence.as_slice(), limits()).is_err());
            let mut repeated = Vec::new();
            actual
                .write_metadata_evidence(
                    &mut repeated,
                    decoded.source_sha256(),
                    decoded.original_encoded_bytes(),
                )
                .unwrap();
            assert_eq!(evidence, repeated);
        }
    }
}

#[test]
fn metadata_evidence_rejects_future_truncated_payload_and_inconsistent_dimensions() {
    let bytes = original(4, "original payload");
    let (_, evidence) = preservation(&bytes);
    for end in (0..88).chain([evidence.len() - 1]) {
        assert!(
            decode_message_acceptance_metadata_evidence(&evidence[..end], limits(), bytes.len())
                .is_err()
        );
    }
    for (offset, value) in [(7, b'2'), (40, 0), (48, 255)] {
        let mut changed = evidence.clone();
        if offset == 40 {
            changed[40..48].fill(0);
        } else {
            changed[offset] = value;
        }
        assert!(
            decode_message_acceptance_metadata_evidence(changed.as_slice(), limits(), bytes.len())
                .is_err()
        );
    }
    let mut changed = evidence.clone();
    changed.extend_from_slice(b" ");
    assert!(
        decode_message_acceptance_metadata_evidence(changed.as_slice(), limits(), bytes.len())
            .is_err()
    );
    let suffix = String::from_utf8(evidence[88..].to_vec()).unwrap();
    for replacement in [
        suffix.replace("\"payload\":\"\"", "\"payload\":\"forged\""),
        suffix.replace("\"sequence\":1", "\"sequence\":0"),
    ] {
        let mut changed = evidence[..88].to_vec();
        changed.extend_from_slice(replacement.as_bytes());
        assert!(
            decode_message_acceptance_metadata_evidence(changed.as_slice(), limits(), bytes.len())
                .is_err()
        );
    }
    let (_, empty) = preservation(&original(1, ""));
    let mut changed = empty;
    changed[56] ^= 1;
    assert!(
        decode_message_acceptance_metadata_evidence(changed.as_slice(), limits(), usize::MAX)
            .is_err()
    );
}

#[test]
fn metadata_evidence_respects_independent_resource_limits_and_partial_io() {
    let bytes = original(3, "12345678");
    let (source, evidence) = preservation(&bytes);
    for policy in [
        MessageAcceptanceDecodeLimits {
            payload_bytes: 7,
            ..limits()
        },
        MessageAcceptanceDecodeLimits {
            auxiliary_bytes: 0,
            ..limits()
        },
        MessageAcceptanceDecodeLimits {
            recipient_count: 0,
            ..limits()
        },
        MessageAcceptanceDecodeLimits {
            recipient_bytes: 0,
            ..limits()
        },
    ] {
        assert!(
            decode_message_acceptance_metadata_evidence(evidence.as_slice(), policy, bytes.len())
                .is_err()
        );
    }
    assert!(
        decode_message_acceptance_metadata_evidence(evidence.as_slice(), limits(), bytes.len() - 1)
            .is_err()
    );
    struct SmallReader<'a>(&'a [u8]);
    impl Read for SmallReader<'_> {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let count = output.len().min(1);
            self.0.read(&mut output[..count])
        }
    }
    assert!(
        decode_message_acceptance_metadata_evidence(SmallReader(&evidence), limits(), bytes.len())
            .is_ok()
    );
    struct RefuseAfter {
        bytes: usize,
        limit: usize,
    }
    impl Write for RefuseAfter {
        fn write(&mut self, input: &[u8]) -> io::Result<usize> {
            let count = input.len().min(self.limit - self.bytes);
            if count == 0 {
                return Err(io::Error::other("private test failure"));
            }
            self.bytes += count;
            Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    // Cover header refusal, entered JSON serialization and a nearly complete
    // suffix. None may publish a usable prefix or mutate the source on retry.
    for limit in [23, 100, evidence.len() - 1] {
        let mut writer = RefuseAfter { bytes: 0, limit };
        let error = source
            .write_metadata_evidence(&mut writer, &[1; 32], bytes.len())
            .unwrap_err();
        assert_eq!(writer.bytes, limit);
        assert!(!error.to_string().contains("private test failure"));
    }
    let mut retried = Vec::new();
    source
        .write_metadata_evidence(&mut retried, &Sha256::digest(&bytes).into(), bytes.len())
        .unwrap();
    assert_eq!(
        retried, evidence,
        "failed encoding never mutates original metadata"
    );
}
