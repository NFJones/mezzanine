//! Authenticated managed Bash source framing and parent-return proof records.
//!
//! The Readline trigger admits no generated source into ordinary history. Source
//! records remain separate until runtime observes receiver-ready. RX2 retains
//! its parent-only proof, frame digests and exact DATA-count semantics.

use super::{
    BASH_PRIVATE_SOURCE_FRAME_BYTES, MarkerToken, SHELL_WRAPPER_BASE64_LINE_BYTES,
    ShellClassification, ShellTransactionInput,
};
use base64::Engine;
use sha2::{Digest, Sha256};

/// Admission trigger and authenticated source frames for managed Bash.
pub(super) struct BashPrivateReceiverTransport {
    /// Non-newline Readline trigger followed by source-free admission metadata.
    pub(super) trigger: String,
    /// Bounded, sequenced source records delivered after receiver-ready.
    pub(super) payload: String,
}

/// Renders Bash source for the managed private Readline receiver.
///
/// The trigger is a bound control byte rather than a newline-terminated
/// command, so Bash never admits generated source into ordinary history. The
/// admission record contains no generated source. Runtime must wait for the
/// receiver-ready event before delivering the bounded source records.
pub(super) fn bash_private_receiver_transport(
    source: &str,
    classification: ShellClassification,
    token: Option<&MarkerToken>,
    marker: &str,
    parent_proof: Option<&MarkerToken>,
) -> Option<BashPrivateReceiverTransport> {
    if classification != ShellClassification::Bash {
        return None;
    }
    let token = token?;
    let source = format!("unset MEZ_WRAPPER_SOURCE\n{source}");
    let digest = Sha256::digest(source.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let encoded = base64::engine::general_purpose::STANDARD.encode(source.as_bytes());
    // Frame boundaries are not necessarily aligned to the base64 line size, so
    // the sum of per-frame chunk counts can exceed the whole-string ceiling.
    // The BEGIN and END records must declare the emitted DATA record count.
    let chunk_count = if parent_proof.is_some() {
        encoded
            .as_bytes()
            .chunks(BASH_PRIVATE_SOURCE_FRAME_BYTES)
            .map(|frame| frame.len().div_ceil(SHELL_WRAPPER_BASE64_LINE_BYTES))
            .sum()
    } else {
        encoded.len().div_ceil(SHELL_WRAPPER_BASE64_LINE_BYTES)
    };
    let record_version = if parent_proof.is_some() { "RX2" } else { "RX1" };
    let trigger = parent_proof.map_or_else(
        || {
            format!(
                "\x07MEZ_BASH_RX1_BEGIN {} {} {} {} {}\n",
                token.as_str(),
                marker,
                source.len(),
                digest,
                chunk_count
            )
        },
        |proof| {
            format!(
                "\x07MEZ_BASH_RX2_BEGIN {} {} {} {} {} {}\n",
                token.as_str(),
                marker,
                source.len(),
                digest,
                chunk_count,
                proof.as_str()
            )
        },
    );
    let mut payload = String::new();
    if parent_proof.is_some() {
        let mut sequence = 0usize;
        for (frame_sequence, frame) in encoded
            .as_bytes()
            .chunks(BASH_PRIVATE_SOURCE_FRAME_BYTES)
            .enumerate()
        {
            let frame_digest = Sha256::digest(frame)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            let frame_chunks = frame.len().div_ceil(SHELL_WRAPPER_BASE64_LINE_BYTES);
            payload.push_str(&format!(
                "MEZ_BASH_RX2_FRAME {} {} {} {} {} {}\n",
                token.as_str(),
                marker,
                frame_sequence,
                frame.len(),
                frame_digest,
                frame_chunks
            ));
            for chunk in frame.chunks(SHELL_WRAPPER_BASE64_LINE_BYTES) {
                let chunk = std::str::from_utf8(chunk)
                    .expect("standard base64 output should always be valid UTF-8");
                payload.push_str(&format!(
                    "MEZ_BASH_RX2_DATA {} {} {} {}\n",
                    token.as_str(),
                    marker,
                    sequence,
                    chunk
                ));
                sequence = sequence.saturating_add(1);
            }
            payload.push_str(&format!(
                "MEZ_BASH_RX2_FRAME_END {} {} {} {}\n",
                token.as_str(),
                marker,
                frame_sequence,
                sequence
            ));
        }
    } else {
        for (sequence, chunk) in encoded
            .as_bytes()
            .chunks(SHELL_WRAPPER_BASE64_LINE_BYTES)
            .enumerate()
        {
            let chunk = std::str::from_utf8(chunk)
                .expect("standard base64 output should always be valid UTF-8");
            payload.push_str(&format!(
                "MEZ_BASH_{record_version}_DATA {} {} {} {}\n",
                token.as_str(),
                marker,
                sequence,
                chunk
            ));
        }
    }
    payload.push_str(&format!(
        "MEZ_BASH_{record_version}_END {} {} {} {} {}\n",
        token.as_str(),
        marker,
        chunk_count,
        source.len(),
        digest
    ));
    Some(BashPrivateReceiverTransport { trigger, payload })
}

/// Renders arbitrary runtime-owned Bash source for private receiver admission.
///
/// The returned wrapper contains only the non-newline admission trigger. The
/// authenticated source frames remain separate in `receiver_payload` so the
/// runtime can retain them until receiver-ready while holding its input lease.
pub fn bash_private_source_input(
    source: &str,
    token: &MarkerToken,
    marker: &str,
) -> ShellTransactionInput {
    let transport = bash_private_receiver_transport(
        source,
        ShellClassification::Bash,
        Some(token),
        marker,
        None,
    )
    .expect("explicit Bash private source rendering requires a receiver transport");
    ShellTransactionInput {
        wrapper: transport.trigger,
        receiver_payload: transport.payload,
        payload: String::new(),
        payload_receiver_acknowledgements: true,
    }
}

/// Renders one persistent Bash child handoff with parent-only return proof.
///
/// The proof is consumed only by the original parent Readline callback. It is
/// not embedded in evaluated source, child arguments, or child environment, so
/// only that callback can authenticate the later parent-ready event.
pub fn bash_private_handoff_source_input(
    source: &str,
    token: &MarkerToken,
    marker: &str,
    parent_proof: &MarkerToken,
) -> ShellTransactionInput {
    let transport = bash_private_receiver_transport(
        source,
        ShellClassification::Bash,
        Some(token),
        marker,
        Some(parent_proof),
    )
    .expect("persistent Bash handoff rendering requires a receiver transport");
    ShellTransactionInput {
        wrapper: transport.trigger,
        receiver_payload: transport.payload,
        payload: String::new(),
        payload_receiver_acknowledgements: true,
    }
}

/// Renders authenticated cancellation for one admitted Bash handoff.
///
/// Cancellation is accepted only as the first RX2 data record, before any
/// generated source bytes have been consumed by the parent callback.
pub fn bash_private_handoff_cancel_input(
    token: &MarkerToken,
    marker: &str,
    parent_proof: &MarkerToken,
) -> String {
    format!(
        "MEZ_BASH_RX2_CANCEL {} {} {}\n",
        token.as_str(),
        marker,
        parent_proof.as_str()
    )
}
