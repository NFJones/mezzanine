//! Canonical command, sidecar and artifact records consumed by shell receivers.
//!
//! Both dialect renderers use this encoder. Sidecar frames preserve occurrence
//! order, exact byte lengths and SHA-256 digests; artifact bytes remain inert
//! Base64 data until the receiver materializes the validated launch contract.

use super::{
    SHELL_TRANSACTION_COMMAND_BASE64_LINE_BYTES, SHELL_TRANSACTION_SIDECAR_FRAME_BYTES,
    ShellLaunchArtifact,
};
use base64::Engine;
use sha2::{Digest, Sha256};

/// Returns a sentinel line that cannot be mistaken for standard base64 data.
pub(super) fn command_payload_end_marker(marker: &str) -> String {
    format!("__MEZ_COMMAND_PAYLOAD_END_{marker}__")
}

/// Appends version-one logical sidecar frames to a receiver payload.
fn append_framed_sidecar_payload(payload: &mut String, input_sidecar: &str) {
    let mut sequence = 0usize;
    let mut frame = String::new();
    for record in input_sidecar.split_inclusive('\n') {
        if !frame.is_empty()
            && frame.len().saturating_add(record.len()) > SHELL_TRANSACTION_SIDECAR_FRAME_BYTES
        {
            append_sidecar_frame(payload, sequence, &frame);
            sequence = sequence.saturating_add(1);
            frame.clear();
        }
        frame.push_str(record);
    }
    if !frame.is_empty() {
        append_sidecar_frame(payload, sequence, &frame);
    }
}

/// Appends one sequenced frame as canonical-safe physical records.
fn append_sidecar_frame(payload: &mut String, sequence: usize, frame: &str) {
    let digest = Sha256::digest(frame.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    payload.push_str(&format!("S1B {sequence} {} {digest}\n", frame.len()));
    for record in frame.lines() {
        payload.push_str("S1D ");
        payload.push_str(record);
        payload.push('\n');
    }
    payload.push_str(&format!("S1E {sequence}\n"));
}

/// Renders the base64 command payload consumed by the transaction receiver.
pub(super) fn command_payload_lines(
    command: &str,
    end_marker: &str,
    input_sidecar: Option<&str>,
    artifacts: &[ShellLaunchArtifact],
) -> String {
    let mut command_source = command.to_string();
    if !command_source.ends_with('\n') {
        command_source.push('\n');
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(command_source.as_bytes());
    let mut payload = String::new();
    for chunk in encoded
        .as_bytes()
        .chunks(SHELL_TRANSACTION_COMMAND_BASE64_LINE_BYTES)
    {
        let chunk = std::str::from_utf8(chunk)
            .expect("standard base64 output should always be valid UTF-8");
        payload.push_str("C ");
        payload.push_str(chunk);
        payload.push('\n');
    }
    if let Some(input_sidecar) = input_sidecar {
        append_framed_sidecar_payload(&mut payload, input_sidecar);
    }
    for (index, artifact) in artifacts.iter().enumerate() {
        let encoded = base64::engine::general_purpose::STANDARD.encode(&artifact.content);
        for chunk in encoded
            .as_bytes()
            .chunks(SHELL_TRANSACTION_COMMAND_BASE64_LINE_BYTES)
        {
            let chunk = std::str::from_utf8(chunk)
                .expect("standard base64 output should always be valid UTF-8");
            payload.push_str(&format!("A{index} {chunk}\n"));
        }
    }
    payload.push_str(end_marker);
    payload.push('\n');
    payload
}
