//! Content-free settled assistant snapshots pinned to OpenCode v1.17.13.
//!
//! Release owners: packages/schema/src/v1/session.ts (AssistantMessage) and
//! packages/opencode/src/session/session.ts (getUsage). Upstream input excludes
//! cache reads/writes; output excludes reasoning. Restore inclusive ledger totals
//! with checked arithmetic. Completed snapshots use one immutable message stream:
//! identical replay adds nothing and changed completed counters fail closed.
//! Partial revisions are not charged; the future bound plugin must qualify any
//! correction protocol rather than invent callback-local revision numbers.
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

/// Projects one completed message into an immutable durable delta stream.
/// `owner` and `project` must come from current server-authorized launch binding,
/// never upstream payloads. The bound session/message pair determines the epoch;
/// model, counters and completion time remain fingerprinted payload so corrected
/// completed observations conflict rather than silently adding more expense.
/// Returns no report for partial, user or unbound messages. This does not deliver
/// an RPC, authorize historical import or certify live plugin callback semantics.
pub(crate) fn completed_report(
    release: &str,
    bound_session: &str,
    owner: &str,
    project: Option<crate::storage::token_usage::AccountingProjectId>,
    bytes: &[u8],
) -> Result<Option<crate::storage::token_usage::ExternalUsageReport>> {
    use sha2::{Digest, Sha256};

    if owner.is_empty() || owner.len() > 128 || owner.chars().any(char::is_control) {
        return Err(unavailable());
    }
    let Some(snapshot) = normalize(release, bound_session, bytes)? else {
        return Ok(None);
    };
    // JSON tuple encoding avoids delimiter collisions in inert vendor IDs.
    // Do not include counters/model/time in this identity: a changed replay
    // must meet the existing receipt, not create a second charged stream.
    let identity = serde_json::to_vec(&(
        "opencode-completed-message/1",
        release,
        &snapshot.session,
        &snapshot.message,
    ))
    .map_err(|_| unavailable())?;
    let digest = Sha256::digest(identity);
    let digest = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let epoch = format!("opencode-message:{digest}");
    Ok(Some(crate::storage::token_usage::ExternalUsageReport {
        owner: owner.to_string(),
        project,
        harness: "opencode".to_string(),
        epoch,
        event_id: format!("completed:{}", snapshot.completed_at_ms),
        sequence: 1,
        mode: "delta".to_string(),
        baseline: false,
        observed_at: snapshot.completed_at_ms / 1000,
        model: mez_agent::ModelTokenUsageKey::new(snapshot.provider, snapshot.model),
        counters: snapshot.counters,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Completed snapshots are immutable accounting observations, not deltas
    /// on every callback. Real storage must conserve expense after reopen,
    /// reordered messages and conflicting corrections, without retaining content.
    #[test]
    fn opencode_completed_reports_replay_once_and_reject_corrections() {
        use crate::storage::token_usage::{TokenHistoryScope, TokenUsageStore};

        let root = std::env::temp_dir().join(format!(
            "mez-opencode-replay-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        let store = TokenUsageStore::new(root.join("usage.sqlite"));
        let bytes = br#"{"role":"assistant","sessionID":"bound","id":"message","providerID":"provider","modelID":"model","time":{"completed":100000},"tokens":{"input":10,"output":4,"reasoning":2,"cache":{"read":3,"write":5}},"text":"PRIVATE"}"#;
        let report = completed_report(RELEASE, "bound", "server-owner", None, bytes)
            .unwrap()
            .unwrap();
        assert_eq!(report.mode, "delta");
        assert_eq!(report.sequence, 1);
        assert_eq!(report.observed_at, 100);
        assert!(store.ingest_external(&report, 100).unwrap().applied);
        let reopened = TokenUsageStore::new(store.path());
        assert!(!reopened.ingest_external(&report, 100).unwrap().applied);

        // Even a correction within the same rounded ledger second is not an
        // identical immutable observation. Its stream sequence must conflict.
        let mut changed_time: serde_json::Value = serde_json::from_slice(bytes).unwrap();
        changed_time["time"]["completed"] = 100001.into();
        let changed_time = completed_report(
            RELEASE,
            "bound",
            "server-owner",
            None,
            &serde_json::to_vec(&changed_time).unwrap(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(changed_time.epoch, report.epoch);
        assert_ne!(changed_time.event_id, report.event_id);
        assert!(reopened.ingest_external(&changed_time, 100).is_err());

        let mut corrected: serde_json::Value = serde_json::from_slice(bytes).unwrap();
        corrected["tokens"]["input"] = 11.into();
        let changed = completed_report(
            RELEASE,
            "bound",
            "server-owner",
            None,
            &serde_json::to_vec(&corrected).unwrap(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(changed.epoch, report.epoch);
        assert!(reopened.ingest_external(&changed, 100).is_err());
        corrected["id"] = "other-message".into();
        let other = completed_report(
            RELEASE,
            "bound",
            "server-owner",
            None,
            &serde_json::to_vec(&corrected).unwrap(),
        )
        .unwrap()
        .unwrap();
        assert_ne!(other.epoch, report.epoch);
        assert!(reopened.ingest_external(&other, 100).unwrap().applied);
        assert!(!reopened.ingest_external(&report, 100).unwrap().applied);
        let history = reopened
            .history_snapshot(100, &[1], &TokenHistoryScope::default())
            .unwrap();
        let usage = history.windows[&1].values().next().unwrap();
        assert_eq!(usage.usage.input_tokens, 37);
        assert_eq!(usage.usage.output_tokens, 12);
        assert!(
            completed_report(RELEASE, "different", "server-owner", None, bytes)
                .unwrap()
                .is_none()
        );
        assert!(completed_report(RELEASE, "bound", "", None, bytes).is_err());
        let digest = report.epoch.strip_prefix("opencode-message:").unwrap();
        assert_eq!(digest.len(), 64);
        assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(!format!("{report:?}").contains("PRIVATE"));
        std::fs::remove_dir_all(root).unwrap();
    }

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
