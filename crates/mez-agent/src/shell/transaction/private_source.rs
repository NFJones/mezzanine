//! Staged authenticated source frames for managed Fish and Zsh receivers.
//!
//! Source-free triggers and HOLD/BEGIN metadata remain separate from payload
//! records so product adapters can enforce semantic admission before sending
//! source. Frame bounds, digests and cancellation records preserve each dialect's
//! existing protocol; this module never writes pane input or evaluates source.

use super::{
    AgentShellValidationError, AgentShellValidationResult, ManagedZshTrigger, MarkerToken,
    SHELL_WRAPPER_BASE64_LINE_BYTES, ZSH_PRIVATE_SOURCE_FRAME_BYTES, ZSH_PRIVATE_SOURCE_MAX_BYTES,
};
use base64::Engine;
use sha2::{Digest, Sha256};

/// Staged private input for one managed Fish editor handoff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FishPrivateSourceInput {
    /// Source-free trigger that starts the private receiver callback.
    pub wrapper: String,
    /// Authenticated hold metadata released after the receiver starts reading.
    pub receiver_hold: String,
    /// Trigger-only stage released after Fish queues its empty-line repaint.
    pub editor_clear_confirmation: String,
    /// Second trigger and authenticated frame header released after editor hold.
    pub receiver_admission: String,
    /// Bounded DATA and END records released after frame admission.
    pub receiver_payload: String,
    /// Whether DATA and END records require receiver acknowledgements.
    pub payload_receiver_acknowledgements: bool,
}

/// Renders a staged private Fish editor trigger and deferred source frame.
///
/// The first trigger is source-free. Runtime releases authenticated HOLD
/// metadata only after Fish publishes receiver-awaiting, then waits for the
/// native editor to clear and repaint before releasing the second trigger and
/// bounded BEGIN header. DATA and END remain withheld until the adapter
/// publishes semantic frame admission.
pub fn fish_private_source_input(
    source: &str,
    token: &MarkerToken,
    marker: &str,
) -> FishPrivateSourceInput {
    let digest = Sha256::digest(source.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let encoded = base64::engine::general_purpose::STANDARD.encode(source.as_bytes());
    let chunks = encoded.as_bytes().chunks(SHELL_WRAPPER_BASE64_LINE_BYTES);
    let chunk_count = chunks.len();
    let wrapper = "\x1b\x07".to_string();
    let receiver_hold = format!("MEZ_FISH_RX1_HOLD {} {}\n", token.as_str(), marker);
    let receiver_admission = format!(
        "\x1b\x07MEZ_FISH_RX1_BEGIN {} {} {} {} {}\n",
        token.as_str(),
        marker,
        source.len(),
        digest,
        chunk_count
    );
    let mut receiver_payload = String::new();
    for (sequence, chunk) in chunks.enumerate() {
        let chunk = std::str::from_utf8(chunk)
            .expect("standard base64 output should always be valid UTF-8");
        receiver_payload.push_str(&format!(
            "MEZ_FISH_RX1_DATA {} {} {} {}\n",
            token.as_str(),
            marker,
            sequence,
            chunk
        ));
    }
    receiver_payload.push_str(&format!(
        "MEZ_FISH_RX1_END {} {} {} {} {}\n",
        token.as_str(),
        marker,
        chunk_count,
        source.len(),
        digest
    ));
    FishPrivateSourceInput {
        wrapper,
        receiver_hold,
        editor_clear_confirmation: "\x1b\x07".to_string(),
        receiver_admission,
        receiver_payload,
        payload_receiver_acknowledgements: true,
    }
}

/// Renders an authenticated cancellation record for one pending Fish admission.
///
/// The record is valid only before runtime releases the corresponding source
/// payload. It lets the bound Fish callback restore its saved editor state
/// without evaluating a partial child-shell handoff.
pub fn fish_private_source_cancel_input(token: &MarkerToken, marker: &str) -> String {
    format!("MEZ_FISH_RX1_CANCEL {} {}\n", token.as_str(), marker)
}

/// Staged private input for one managed Zsh editor handoff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZshPrivateSourceInput {
    /// Fixed ZLE trigger accepted without queued metadata bytes.
    pub wrapper: String,
    /// Authenticated source-free HOLD metadata delivered after receiver startup.
    pub receiver_hold: String,
    /// Authenticated bounded BEGIN header released after editor hold.
    pub receiver_admission: String,
    /// Bounded DATA and END records released after frame admission.
    pub receiver_payload: String,
    /// Whether DATA and END records require receiver acknowledgements.
    pub payload_receiver_acknowledgements: bool,
}

/// Renders a source-free ZLE trigger and staged authenticated source frames.
///
/// The fixed trigger is consumed by the managed Zsh widget before it can become
/// editable input. HOLD lets the widget correlate and publish editor ownership;
/// BEGIN and framed DATA/END remain independently gated by runtime semantic
/// events. Frame digests bound acknowledgement pacing without relaxing the
/// whole-source length and digest validation performed before evaluation.
pub fn zsh_private_source_input(
    source: &str,
    token: &MarkerToken,
    marker: &str,
    trigger: ManagedZshTrigger,
) -> AgentShellValidationResult<ZshPrivateSourceInput> {
    if source.len() > ZSH_PRIVATE_SOURCE_MAX_BYTES {
        return Err(AgentShellValidationError::invalid_args(format!(
            "managed zsh private source exceeds {ZSH_PRIVATE_SOURCE_MAX_BYTES} bytes"
        )));
    }
    let digest = Sha256::digest(source.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let encoded = base64::engine::general_purpose::STANDARD.encode(source.as_bytes());
    let frames = encoded
        .as_bytes()
        .chunks(ZSH_PRIVATE_SOURCE_FRAME_BYTES)
        .collect::<Vec<_>>();
    let frame_count = frames.len();
    let chunk_count = frames
        .iter()
        .map(|frame| frame.len().div_ceil(SHELL_WRAPPER_BASE64_LINE_BYTES))
        .sum::<usize>();
    let receiver_admission = format!(
        "MEZ_ZSH_RX2_BEGIN {} {} {} {} {} {}\n",
        token.as_str(),
        marker,
        source.len(),
        digest,
        frame_count,
        chunk_count
    );
    let mut receiver_payload = String::new();
    for (frame_sequence, frame) in frames.iter().enumerate() {
        let frame_digest = Sha256::digest(frame)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let frame_chunks = frame.len().div_ceil(SHELL_WRAPPER_BASE64_LINE_BYTES);
        receiver_payload.push_str(&format!(
            "MEZ_ZSH_RX2_FRAME {} {} {} {} {} {}\n",
            token.as_str(),
            marker,
            frame_sequence,
            frame.len(),
            frame_digest,
            frame_chunks
        ));
        for (chunk_sequence, chunk) in frame.chunks(SHELL_WRAPPER_BASE64_LINE_BYTES).enumerate() {
            let chunk = std::str::from_utf8(chunk)
                .expect("standard base64 output should always be valid UTF-8");
            receiver_payload.push_str(&format!(
                "MEZ_ZSH_RX2_DATA {} {} {} {} {}\n",
                token.as_str(),
                marker,
                frame_sequence,
                chunk_sequence,
                chunk
            ));
        }
        receiver_payload.push_str(&format!(
            "MEZ_ZSH_RX2_FRAME_END {} {} {} {}\n",
            token.as_str(),
            marker,
            frame_sequence,
            frame_chunks
        ));
    }
    receiver_payload.push_str(&format!(
        "MEZ_ZSH_RX2_END {} {} {} {} {} {}\n",
        token.as_str(),
        marker,
        frame_count,
        chunk_count,
        source.len(),
        digest
    ));
    Ok(ZshPrivateSourceInput {
        wrapper: trigger.input().to_string(),
        receiver_hold: format!("MEZ_ZSH_RX2_HOLD {} {}\n", token.as_str(), marker),
        receiver_admission,
        receiver_payload,
        payload_receiver_acknowledgements: true,
    })
}

/// Renders an authenticated cancellation record for pending zsh admission.
///
/// The managed receiver accepts this source-free record only before a BEGIN
/// frame, allowing runtime to restore the saved parent editor without launching
/// a child after agent mode has already been hidden.
pub fn zsh_private_source_cancel_input(token: &MarkerToken, marker: &str) -> String {
    format!("MEZ_ZSH_RX2_CANCEL {} {}\n", token.as_str(), marker)
}
