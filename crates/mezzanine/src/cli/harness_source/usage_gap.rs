//! Strict public Claude coverage capsules for an exact native-qualified owner.
//!
//! No counters, callback content, paths, credentials or generic method selectors
//! are accepted. A typed observation receipt is diagnostic custody only; it is
//! never a durable expense acknowledgment or permission to revive a source.

use super::*;
use crate::integrations::bootstrap::claude_usage_gap::UsageGapReason;

/// Fixed original-epoch selectors and one closed metadata-only diagnostic.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UsageGap {
    operation: String,
    external_session_id: String,
    generation: u64,
    observer_witness: String,
    reason: UsageGapReason,
}

impl UsageGap {
    /// Rejects ambiguous/extra fields and unsafe selectors before any exchange.
    pub(super) fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > 4096 {
            return Err(MezError::invalid_args("usage coverage unavailable"));
        }
        let value = crate::protocol::strict_json::decode(bytes)
            .map_err(|_| MezError::invalid_args("usage coverage unavailable"))?;
        let parsed: Self = serde_json::from_value(value)
            .map_err(|_| MezError::invalid_args("usage coverage unavailable"))?;
        if parsed.operation != "curated-usage-gap"
            || !identifier(&parsed.external_session_id, 128)
            || parsed.generation == 0
            || parsed.generation > 9_007_199_254_740_991
            || parsed.observer_witness.len() != 64
            || !parsed
                .observer_witness
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(MezError::invalid_args("usage coverage unavailable"));
        }
        Ok(parsed)
    }

    /// Projects only the single fixed Claude diagnostic method and selectors.
    pub(super) fn request(&self) -> String {
        serde_json::json!({"jsonrpc":"2.0","id":"harness-source",
            "method":"agent/external/curated-usage-gap","params":{
                "harness":"claude","external_session_id":self.external_session_id,
                "generation":self.generation,"observer_witness":self.observer_witness,
                "reason":self.reason}})
        .to_string()
    }

    /// Requires exact typed diagnostic custody, without claiming expense commit.
    pub(super) fn receipt(&self, value: &serde_json::Value) -> Option<serde_json::Value> {
        let object = value.as_object()?;
        (object.len() == 2
            && value["observed"] == true
            && value["reason"] == serde_json::json!(self.reason))
        .then(|| serde_json::json!({"observed":true,"reason":self.reason}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only original public selectors plus the fixed diagnostic are accepted;
    /// raw/private/ambiguous metadata cannot become another method or receipt.
    #[test]
    fn harness_source_usage_gap_capsule_and_receipt_are_strict() {
        let base = serde_json::json!({"operation":"curated-usage-gap","external_session_id":"session-a",
            "generation":1,"observer_witness":"a".repeat(64),"reason":"finalized-counters"});
        let gap = UsageGap::parse(base.to_string().as_bytes()).unwrap();
        let projected: serde_json::Value = serde_json::from_str(&gap.request()).unwrap();
        assert_eq!(projected["method"], "agent/external/curated-usage-gap");
        assert_eq!(projected["params"]["harness"], "claude");
        assert_eq!(
            gap.receipt(&serde_json::json!({"observed":true,"reason":"finalized-counters"})),
            Some(serde_json::json!({"observed":true,"reason":"finalized-counters"}))
        );
        for value in [
            serde_json::json!({}),
            serde_json::json!({"observed":true,"reason":"capacity"}),
            serde_json::json!({"observed":true,"reason":"finalized-counters","private":"PRIVATE"}),
        ] {
            assert!(gap.receipt(&value).is_none());
        }
        for field in ["harness", "launch_token", "pid", "counters", "request_id"] {
            let mut value = base.clone();
            value[field] = serde_json::Value::Null;
            assert!(UsageGap::parse(value.to_string().as_bytes()).is_err());
        }
        for (field, value) in [
            ("reason", serde_json::json!("PRIVATE")),
            ("generation", serde_json::json!(0)),
            ("generation", serde_json::json!(9_007_199_254_740_992_u64)),
            ("observer_witness", serde_json::json!("invalid")),
        ] {
            let mut altered = base.clone();
            altered[field] = value;
            assert!(UsageGap::parse(altered.to_string().as_bytes()).is_err());
        }
        let ambiguous =
            base.to_string()
                .replacen("\"reason\":", "\"reason\":\"capacity\",\"reason\":", 1);
        assert!(UsageGap::parse(ambiguous.as_bytes()).is_err());
    }
}
