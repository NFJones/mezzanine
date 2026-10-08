//! Fixed original-epoch observer proof capsule, never caller lease renewal.
use super::*;

/// Strict public selectors for one existing creator's original observer epoch.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Heartbeat {
    operation: String,
    external_session_id: String,
    generation: u64,
    observer_witness: String,
    sequence: u64,
}

impl Heartbeat {
    /// Rejects raw ambiguity/extra fields/private handles and invalid selectors
    /// before fixed projection. Metadata does not supply native authority.
    pub(super) fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > 4096 {
            return Err(MezError::invalid_args("observer proof unavailable"));
        }
        let value = crate::protocol::strict_json::decode(bytes)
            .map_err(|_| MezError::invalid_args("observer proof unavailable"))?;
        let proof: Self = serde_json::from_value(value)
            .map_err(|_| MezError::invalid_args("observer proof unavailable"))?;
        if proof.operation != "curated-heartbeat"
            || !identifier(&proof.external_session_id, 128)
            || proof.generation == 0
            || proof.generation > 9_007_199_254_740_991
            || proof.sequence == 0
            || proof.sequence > 9_007_199_254_740_991
            || proof.observer_witness.len() != 64
            || !proof
                .observer_witness
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(MezError::invalid_args("observer proof unavailable"));
        }
        Ok(proof)
    }
    /// Fixed method/harness with no role, PID, endpoint, credential or TTL claim.
    pub(super) fn request(&self) -> String {
        serde_json::json!({"jsonrpc":"2.0","id":"harness-source","method":"agent/external/curated-heartbeat","params":{
            "harness":"claude","external_session_id":self.external_session_id,"generation":self.generation,
            "observer_witness":self.observer_witness,"sequence":self.sequence}}).to_string()
    }
    /// Rebuilds only the matching observed/sequence/change receipt. Unknown,
    /// private or content fields fail closed; no raw daemon result is echoed.
    pub(super) fn receipt(&self, value: &serde_json::Value) -> Option<serde_json::Value> {
        let object = value.as_object()?;
        if object.len() != 3
            || value["observed"] != true
            || value["sequence"].as_u64() != Some(self.sequence)
        {
            return None;
        }
        let changed = value["changed"].as_bool()?;
        Some(serde_json::json!({"observed":true,"sequence":self.sequence,"changed":changed}))
    }
}
