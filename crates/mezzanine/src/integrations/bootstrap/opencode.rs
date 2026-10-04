//! Content-free settled assistant snapshots pinned to OpenCode v1.17.13.
//!
//! Release owners: packages/schema/src/v1/session.ts (AssistantMessage) and
//! packages/opencode/src/session/session.ts (getUsage). Upstream input excludes
//! cache reads/writes; output excludes reasoning. Restore inclusive ledger totals
//! with checked arithmetic. A snapshot is not a new additive delta: the future
//! bound plugin must deduplicate by message identity and reconcile revisions.
//! This pure owner never reads transcripts, installs plugins, infers pane
//! authority, forwards content, or counts StepFinishPart and message totals twice.

use crate::error::{MezError, Result};
use crate::storage::token_usage::ExternalCounters;

/// Exact release whose counter conversion has been inspected.
pub(crate) const RELEASE: &str = "1.17.13";
/// Largest exact integer representable by the upstream JavaScript number type.
const MAX_EXACT_INTEGER: u64 = (1_u64 << 53) - 1;

/// One settled message observation, not an automatically charged delta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Snapshot {
    /// Session must equal the launcher-owned explicit binding.
    pub(crate) session: String,
    /// Stable upsert identity; repeated delivery is never fresh expense.
    pub(crate) message: String,
    /// Reported producer identity, not execution authority.
    pub(crate) provider: String,
    /// Reported model retained independently of focus or pane state.
    pub(crate) model: String,
    /// Upstream completion timestamp in milliseconds.
    pub(crate) completed_at_ms: u64,
    /// Inclusive normalized counters with known optional categories.
    pub(crate) counters: ExternalCounters,
}

/// Returns a generic content-free diagnostic for unavailable observations.
fn unavailable() -> MezError {
    MezError::invalid_args("OpenCode usage observation unavailable")
}

/// Reads a bounded inert identifier without echoing untrusted input.
fn identifier(value: &serde_json::Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|text| !text.is_empty() && text.len() <= 128 && !text.chars().any(char::is_control))
        .map(str::to_string)
        .ok_or_else(unavailable)
}

/// Requires exact nonnegative JavaScript integers; never rounds or guesses zero.
fn counter(value: Option<&serde_json::Value>) -> Result<u64> {
    value
        .and_then(serde_json::Value::as_u64)
        .filter(|value| *value <= MAX_EXACT_INTEGER)
        .ok_or_else(unavailable)
}

/// Projects one assistant-message snapshot from an explicitly bound session.
/// Unsettled/user/other-session messages are inert. Cost, total, paths, errors,
/// text, tool parts and all other upstream payloads are intentionally discarded.
pub(crate) fn normalize(
    release: &str,
    bound_session: &str,
    bytes: &[u8],
) -> Result<Option<Snapshot>> {
    if release != RELEASE
        || bound_session.is_empty()
        || bound_session.len() > 128
        || bytes.len() > 64 * 1024
    {
        return Err(unavailable());
    }
    let info: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| unavailable())?;
    if !info.is_object() {
        return Err(unavailable());
    }
    if info.get("role").and_then(serde_json::Value::as_str) != Some("assistant") {
        return Ok(None);
    }
    let session = identifier(&info, "sessionID")?;
    if session != bound_session {
        return Ok(None);
    }
    let Some(completed) = info.pointer("/time/completed") else {
        return Ok(None);
    };
    let tokens = info.get("tokens").ok_or_else(unavailable)?;
    let uncached = counter(tokens.get("input"))?;
    let output = counter(tokens.get("output"))?;
    let reasoning = counter(tokens.get("reasoning"))?;
    let read = counter(tokens.pointer("/cache/read"))?;
    let write = counter(tokens.pointer("/cache/write"))?;
    let counters = ExternalCounters {
        input_tokens: uncached
            .checked_add(read)
            .and_then(|value| value.checked_add(write))
            .ok_or_else(unavailable)?,
        output_tokens: output.checked_add(reasoning).ok_or_else(unavailable)?,
        reasoning_tokens: Some(reasoning),
        cached_input_tokens: Some(read),
        cache_write_input_tokens: Some(write),
    };
    counters.validate().map_err(|_| unavailable())?;
    Ok(Some(Snapshot {
        session,
        message: identifier(&info, "id")?,
        provider: identifier(&info, "providerID")?,
        model: identifier(&info, "modelID")?,
        completed_at_ms: counter(Some(completed))?,
        counters,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Released disjoint categories restore inclusive totals while repeated
    /// snapshots retain one upsert identity. Sensitive fields never leave output.
    #[test]
    fn opencode_pinned_snapshot_restores_inclusive_categories_without_content() {
        let bytes = br#"{"role":"assistant","sessionID":"ses_bound","id":"msg_one","providerID":"provider","modelID":"model","time":{"completed":1000},"tokens":{"input":10,"output":4,"reasoning":2,"cache":{"read":3,"write":5},"total":999},"path":{"cwd":"PRIVATE"},"structured":"PRIVATE","error":{"message":"PRIVATE"},"cost":123}"#;
        let first = normalize(RELEASE, "ses_bound", bytes).unwrap().unwrap();
        assert_eq!(first.counters.input_tokens, 18);
        assert_eq!(first.counters.output_tokens, 6);
        assert_eq!(first.counters.cached_input_tokens, Some(3));
        assert_eq!(first.counters.cache_write_input_tokens, Some(5));
        assert_eq!(first.counters.reasoning_tokens, Some(2));
        assert_eq!(
            normalize(RELEASE, "ses_bound", bytes).unwrap(),
            Some(first.clone())
        );
        assert!(!format!("{first:?}").contains("PRIVATE"));
        assert_eq!(normalize(RELEASE, "different", bytes).unwrap(), None);
    }

    /// Partial/user snapshots stay inert; missing/fractional/negative/inexact
    /// counters fail instead of fabricated zero. Wrong release is not admitted.
    #[test]
    fn opencode_pinned_snapshot_refuses_ambiguous_counters_and_unbound_work() {
        assert_eq!(
            normalize(RELEASE, "bound", br#"{"role":"user"}"#).unwrap(),
            None
        );
        assert_eq!(
            normalize(
                RELEASE,
                "bound",
                br#"{"role":"assistant","sessionID":"bound"}"#
            )
            .unwrap(),
            None
        );
        assert!(normalize("future", "bound", b"{}").is_err());
        assert!(normalize(RELEASE, "bound", &vec![b'x'; 65537]).is_err());
        for invalid in [
            serde_json::json!(-1),
            serde_json::json!(1.5),
            serde_json::json!(MAX_EXACT_INTEGER + 1),
            serde_json::Value::Null,
        ] {
            assert!(counter(Some(&invalid)).is_err());
        }
        assert!(counter(None).is_err());
    }
}
