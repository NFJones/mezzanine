//! Nonauthorizing persistent-pair qualification in active checkpoint version 7.
//!
//! Qualification preserves provenance from runtime capture; it does not replace
//! restored pair membership, immutable contracts or exact native owner checks.
//! Legacy rows remain readable but cannot invent this newer qualification.

use super::*;

/// Current true/false qualification round-trips exactly with every other field.
#[test]
fn active_metadata_ownership_qualification_round_trips() {
    for owned in [false, true] {
        let mut metadata = agent_session_metadata("$ownership", "persistent-child");
        metadata.persistent_subagent_owned = owned;
        let encoded = encode_agent_session_metadata(&metadata).unwrap();
        assert!(encoded.starts_with("mez-agent-session-metadata/7\t"));
        assert_eq!(decode_agent_session_metadata(&encoded).unwrap(), metadata);
    }
}

/// A version-six row has no provenance marker; appending a true value under
/// the old version is rejected rather than reinterpreted as ownership evidence.
#[test]
fn active_metadata_legacy_missing_ownership_is_false() {
    let mut metadata = agent_session_metadata("$ownership", "persistent-child");
    metadata.persistent_subagent_owned = true;
    let encoded = encode_agent_session_metadata(&metadata).unwrap();
    let (without_marker, marker) = encoded.rsplit_once('\t').unwrap();
    assert_eq!(marker, "true");
    let legacy = without_marker.replacen(
        "mez-agent-session-metadata/7",
        "mez-agent-session-metadata/6",
        1,
    );
    metadata.persistent_subagent_owned = false;
    assert_eq!(decode_agent_session_metadata(&legacy).unwrap(), metadata);
    assert!(decode_agent_session_metadata(&format!("{legacy}\ttrue")).is_err());
}

/// Current missing/malformed markers and extra columns fail closed; a decoder
/// must never search an arbitrary tail or coerce a string into permission.
#[test]
fn active_metadata_ownership_rejects_ambiguous_wire_layouts() {
    let encoded =
        encode_agent_session_metadata(&agent_session_metadata("$ownership", "child")).unwrap();
    let (prefix, _) = encoded.rsplit_once('\t').unwrap();
    assert!(decode_agent_session_metadata(prefix).is_err());
    assert!(decode_agent_session_metadata(&format!("{prefix}\tyes")).is_err());
    assert!(decode_agent_session_metadata(&format!("{encoded}\ttrue")).is_err());
}
