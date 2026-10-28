//! Fixed public request-accounting capsules for native direct-child helpers.
//!
//! Selectors cannot carry capabilities, PID/project/endpoint/epoch claims or
//! callback content. Canonical harness names only narrow an already admitted
//! native producer; same-UID direct-parent proof remains authoritative. Begin
//! receipt is required before genuine new request work; final normalized reports
//! retain the same marker. Neutral helper exit never implies durable accounting.

use super::*;

#[cfg(test)]
mod tests {
    use super::*;

    /// Explicit canonical selectors reuse one fixed request shape for all six
    /// adapters; unknown, retired and null names cannot select a generic RPC.
    #[test]
    fn harness_source_usage_capsules_select_only_six_canonical_adapters() {
        let base = serde_json::json!({"operation":"curated-usage-begin","external_session_id":"session-a","generation":1,"observer_witness":"a".repeat(64),"request_id":"b".repeat(64)});
        for harness in ["pi", "opencode", "claude", "codex", "copilot", "cursor"] {
            let mut input = base.clone();
            input["harness"] = serde_json::json!(harness);
            let parsed = Usage::parse(input.to_string().as_bytes()).unwrap();
            let request: serde_json::Value = serde_json::from_str(&parsed.request()).unwrap();
            assert_eq!(request["params"]["harness"], harness);
            assert_eq!(request["method"], "agent/external/curated-usage-begin");
        }
        for harness in [
            serde_json::Value::Null,
            serde_json::json!("gemini"),
            serde_json::json!("unknown"),
        ] {
            let mut input = base.clone();
            input["harness"] = harness;
            assert!(Usage::parse(input.to_string().as_bytes()).is_err());
        }
    }

    /// Actual response/provider labels are data, distinct from opaque source
    /// selectors. Documented cloud model punctuation and bounded UTF8 survive
    /// exact projection; control/bidi/oversized labels and unsafe IDs still fail.
    #[test]
    fn harness_source_usage_model_labels_preserve_bounded_metadata() {
        let base = serde_json::json!({"operation":"curated-usage-end","external_session_id":"session-a",
            "generation":1,"observer_witness":"a".repeat(64),"request_id":"b".repeat(64),
            "event_id":"c".repeat(64),"observed_at":10,"provider":"unavailable",
            "model":"claude-haiku-4-5@20251001","counters":{"input_tokens":10,"output_tokens":3}});
        for model in [
            "claude-haiku-4-5@20251001".to_string(),
            "publisher/model@revision".into(),
            "é".repeat(64),
        ] {
            let mut input = base.clone();
            input["model"] = serde_json::json!(model);
            let parsed = Usage::parse(input.to_string().as_bytes()).unwrap();
            let request: serde_json::Value = serde_json::from_str(&parsed.request()).unwrap();
            assert_eq!(request["params"]["model"], model);
        }
        for model in [
            "".to_string(),
            "  ".into(),
            "model\n".into(),
            "model\u{85}".into(),
            "model\u{202e}".into(),
            "é".repeat(65),
        ] {
            let mut input = base.clone();
            input["model"] = serde_json::json!(model);
            assert!(Usage::parse(input.to_string().as_bytes()).is_err());
        }
        let mut input = base.clone();
        input["external_session_id"] = serde_json::json!("session@other");
        assert!(Usage::parse(input.to_string().as_bytes()).is_err());
        input = base;
        input["request_id"] = serde_json::json!("marker@other");
        assert!(Usage::parse(input.to_string().as_bytes()).is_err());
    }

    /// Unique bounded public selectors project only fixed Claude methods;
    /// credentials, unknown payload content, null begin outcomes and numeric
    /// ambiguity cannot enter a captured helper request or receipt.
    #[test]
    fn harness_source_usage_capsules_and_receipts_are_strict_content_free() {
        let base = serde_json::json!({"operation":"curated-usage-begin","external_session_id":"session-a","generation":1,"observer_witness":"a".repeat(64),"request_id":"b".repeat(64)});
        let begin = Usage::parse(base.to_string().as_bytes()).unwrap();
        let projected: serde_json::Value = serde_json::from_str(&begin.request()).unwrap();
        assert_eq!(projected["method"], "agent/external/curated-usage-begin");
        assert!(projected["params"].get("launch_token").is_none());
        for field in ["project", "owner", "pid", "launch_token", "event_id"] {
            let mut altered = base.clone();
            altered[field] = serde_json::Value::Null;
            assert!(Usage::parse(altered.to_string().as_bytes()).is_err());
        }
        let ready = serde_json::json!({"protocol":"external-request/1","accepted":true,"request_id":"b".repeat(64),"admitted_at":10,"source_witness":"c".repeat(64),"origin":"unattributed","controls":[]});
        assert!(begin.receipt(&ready).is_some());
        let mut leaked = ready.clone();
        leaked["launch_token"] = serde_json::json!("PRIVATE");
        assert!(begin.receipt(&leaked).is_none());
        let mut end = base;
        end["operation"] = serde_json::json!("curated-usage-end");
        end["event_id"] = serde_json::json!("d".repeat(64));
        end["observed_at"] = serde_json::json!(10);
        end["provider"] = serde_json::json!("fixture");
        end["model"] = serde_json::json!("model");
        end["counters"] = serde_json::json!({"input_tokens":10,"output_tokens":3});
        let parsed = Usage::parse(end.to_string().as_bytes()).unwrap();
        let receipt = serde_json::json!({"accepted":true,"durable":true,"applied":false,"revision":1,"coverage":{"source":"external-reported","reasoning_known":false}});
        assert!(parsed.receipt(&receipt).is_some());
        let mut neutral = receipt.clone();
        neutral["durable"] = serde_json::json!(false);
        assert!(parsed.receipt(&neutral).is_none());
        end["counters"]["input_tokens"] = serde_json::json!(9_007_199_254_740_992_u64);
        assert!(Usage::parse(end.to_string().as_bytes()).is_err());
    }
}

/// Explicit narrow public metadata; optional outcome fields belong only to end.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Usage {
    operation: String,
    #[serde(default = "default_harness")]
    harness: String,
    external_session_id: String,
    generation: u64,
    observer_witness: String,
    request_id: String,
    #[serde(default)]
    event_id: Option<String>,
    #[serde(default)]
    observed_at: Option<u64>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    counters: Option<crate::storage::token_usage::ExternalCounters>,
}

/// Existing installed Claude capsules omit the selector; other ordinary sources
/// name their canonical harness, still subject to independent native admission.
fn default_harness() -> String {
    "claude".into()
}

/// Canonical opaque selectors cannot store raw event content or case aliases.
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Provider/model labels are bounded inert UTF8 metadata, never authority or
/// executable arguments. IDs and source selectors retain their own grammar.
fn label(value: &str) -> bool {
    value.len() <= 128
        && !value.trim().is_empty()
        && !value.chars().any(|character| {
            character.is_control()
                || matches!(character, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
}

impl Usage {
    /// Rejects duplicate/extra fields, private handles and unsafe numeric data
    /// before fixed-method serialization. No upstream object is forwarded.
    pub(super) fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > 4096 {
            return Err(MezError::invalid_args("request telemetry unavailable"));
        }
        let value = crate::protocol::strict_json::decode(bytes)?;
        let parsed: Self = serde_json::from_value(value.clone())
            .map_err(|_| MezError::invalid_args("request telemetry unavailable"))?;
        if !matches!(
            parsed.operation.as_str(),
            "curated-usage-begin" | "curated-usage-end"
        ) || !matches!(
            parsed.harness.as_str(),
            "pi" | "opencode" | "claude" | "codex" | "copilot" | "cursor"
        ) || !identifier(&parsed.external_session_id, 128)
            || parsed.generation == 0
            || parsed.generation > 9_007_199_254_740_991
            || !digest(&parsed.observer_witness)
            || !digest(&parsed.request_id)
        {
            return Err(MezError::invalid_args(
                "request telemetry selectors unavailable",
            ));
        }
        if parsed.operation == "curated-usage-begin" {
            if ["event_id", "observed_at", "provider", "model", "counters"]
                .iter()
                .any(|key| value.get(key).is_some())
            {
                return Err(MezError::invalid_args(
                    "request begin cannot carry outcome data",
                ));
            }
        } else {
            if parsed.event_id.as_ref().is_none_or(|id| !digest(id))
                || parsed
                    .observed_at
                    .is_none_or(|time| time == 0 || time > 9_007_199_254_740_991)
                || parsed.provider.as_ref().is_none_or(|value| !label(value))
                || parsed.model.as_ref().is_none_or(|value| !label(value))
            {
                return Err(MezError::invalid_args(
                    "request outcome metadata unavailable",
                ));
            }
            let counters = parsed
                .counters
                .as_ref()
                .ok_or_else(|| MezError::invalid_args("request counters unavailable"))?;
            counters.validate()?;
            for value in [
                Some(counters.input_tokens),
                Some(counters.output_tokens),
                counters.reasoning_tokens,
                counters.cached_input_tokens,
                counters.cache_write_input_tokens,
            ]
            .into_iter()
            .flatten()
            {
                if value > 9_007_199_254_740_991 {
                    return Err(MezError::invalid_args(
                        "request counter exceeds exact numeric boundary",
                    ));
                }
            }
        }
        Ok(parsed)
    }
    /// Selects only the fixed native-creator telemetry method and harness.
    pub(super) fn request(&self) -> String {
        let mut params = serde_json::json!({"harness":self.harness,"external_session_id":self.external_session_id,"generation":self.generation,"observer_witness":self.observer_witness,"request_id":self.request_id});
        if self.operation == "curated-usage-end" {
            params["event_id"] = serde_json::json!(self.event_id);
            params["observed_at"] = serde_json::json!(self.observed_at);
            params["provider"] = serde_json::json!(self.provider);
            params["model"] = serde_json::json!(self.model);
            params["counters"] = serde_json::json!(self.counters);
        }
        serde_json::json!({"jsonrpc":"2.0","id":"harness-source","method":format!("agent/external/{}",self.operation),"params":params}).to_string()
    }
    /// Rebuilds only validated typed readiness/durability facts. Unknown/private
    /// response data cannot escape to the calling API's captured output.
    pub(super) fn receipt(&self, value: &serde_json::Value) -> Option<serde_json::Value> {
        let object = value.as_object()?;
        if self.operation == "curated-usage-begin" {
            if object.keys().any(|key| {
                ![
                    "protocol",
                    "accepted",
                    "request_id",
                    "admitted_at",
                    "source_witness",
                    "origin",
                    "controls",
                ]
                .contains(&key.as_str())
            }) || value["protocol"] != "external-request/1"
                || value["accepted"] != true
                || value["request_id"] != self.request_id
                || value["controls"] != serde_json::json!([])
                || value["admitted_at"]
                    .as_u64()
                    .is_none_or(|n| n == 0 || n > 9_007_199_254_740_991)
                || value["source_witness"]
                    .as_str()
                    .is_none_or(|value| !digest(value))
                || !["project", "unattributed"].contains(&value["origin"].as_str()?)
            {
                return None;
            }
            return Some(
                serde_json::json!({"protocol":"external-request/1","accepted":true,"request_id":self.request_id,"admitted_at":value["admitted_at"],"source_witness":value["source_witness"],"origin":value["origin"],"controls":[]}),
            );
        }
        if object.keys().any(|key| {
            !["accepted", "durable", "applied", "revision", "coverage"].contains(&key.as_str())
        }) || value["accepted"] != true
            || value["durable"] != true
            || value["revision"]
                .as_u64()
                .is_none_or(|n| n == 0 || n > 9_007_199_254_740_991)
        {
            return None;
        }
        let applied = value["applied"].as_bool()?;
        let coverage = value["coverage"].as_object()?;
        if coverage
            .keys()
            .any(|key| !["source", "reasoning_known"].contains(&key.as_str()))
            || value["coverage"]["source"] != "external-reported"
        {
            return None;
        }
        let reasoning_known = value["coverage"]["reasoning_known"].as_bool()?;
        Some(
            serde_json::json!({"accepted":true,"durable":true,"applied":applied,"revision":value["revision"],"coverage":{"source":"external-reported","reasoning_known":reasoning_known}}),
        )
    }
}
