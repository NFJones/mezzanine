//! Fixed public argv capsule for curated APIs whose stdin is closed.
//!
//! No credentials, raw vendor payloads, PID/role/endpoint/method selectors or
//! general RPC enter this mode. Standard routing environment is only a hint;
//! native creator admission owns authorization. Exact argv bypasses HOME/config
//! and never reads stdin, retries, starts vendors or exposes raw daemon fields.
//! Typed public receipts go to the calling API's captured output, not a vendor
//! hook response. Unavailable transport remains neutral and content-free.
//! The local test submodule exercises capsule and receipt boundaries independently.

use super::{MezError, Result, Write};

/// Minimal public session declaration; everything identifying the transport and
/// source profile is fixed code, not a user-provided RPC or daemon capability.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Capsule {
    external_session_id: String,
    observer_instance: String,
    session_boundary: String,
}

/// Accepts bounded opaque ASCII identifiers, never prompt/path/control content.
fn identifier(text: &str, limit: usize) -> bool {
    !text.is_empty()
        && text.len() <= limit
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
}

impl Capsule {
    /// Strict raw boundary rejects duplicate/extra fields before fixed projection.
    fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > 4096 {
            return Err(MezError::invalid_args("source unavailable"));
        }
        let value = crate::protocol::strict_json::decode(bytes)
            .map_err(|_| MezError::invalid_args("source unavailable"))?;
        let capsule: Self = serde_json::from_value(value)
            .map_err(|_| MezError::invalid_args("source unavailable"))?;
        if !identifier(&capsule.external_session_id, 128)
            || !identifier(&capsule.observer_instance, 128)
            || !matches!(
                capsule.session_boundary.as_str(),
                "startup" | "resume" | "clear" | "fork"
            )
        {
            return Err(MezError::invalid_args("source unavailable"));
        }
        Ok(capsule)
    }
    /// Constructs only the known creator contract. Pane metadata is a routing
    /// hint; supplied capsule PID/name/pane/credentials/extra fields never enter.
    fn request(&self, pane: &str) -> Result<String> {
        if pane.is_empty()
            || pane.len() > 64
            || !pane.starts_with('%')
            || !pane[1..].bytes().all(|b| b.is_ascii_digit())
            || pane.len() == 1
        {
            return Err(MezError::invalid_args("source unavailable"));
        }
        Ok(serde_json::json!({"jsonrpc":"2.0","id":"harness-source","method":"agent/external/curated-enroll","params":{
            "pane_id":pane,"harness":"claude","version":"best-effort","display_name":"Claude Code",
            "external_session_id":self.external_session_id,"observer_instance":self.observer_instance,
            "observer_kind":"curated-command","source_contract":"claude-curated-command/1","session_boundary":self.session_boundary}}).to_string())
    }
    /// Validates and rebuilds public selectors only. Protocol mismatch, private
    /// fields, counters/content, wrong source/instance and unsupported receipts
    /// lose telemetry; no raw result or daemon error can escape to the caller.
    fn receipt(&self, value: &serde_json::Value) -> Option<serde_json::Value> {
        let allowed = [
            "protocol",
            "registered",
            "controls",
            "agent_id",
            "generation",
            "observer_witness",
            "run_id",
            "observer_epoch",
            "observer_instance",
            "external_session_id",
            "usage",
            "observer_transport",
            "expires_at_unix_seconds",
            "lease_seconds",
        ];
        let object = value.as_object()?;
        if object.keys().any(|key| !allowed.contains(&key.as_str()))
            || value["protocol"] != "external-agent/1"
            || value["registered"] != true
            || value["controls"] != serde_json::json!([])
            || value["external_session_id"] != self.external_session_id
            || value["observer_instance"] != self.observer_instance
            || value["usage"] != "unavailable-source-continuity"
            || value["observer_transport"] != "unavailable-curated-freshness"
            || value["lease_seconds"] != 60
        {
            return None;
        }
        let positive = |name: &str| {
            value[name]
                .as_u64()
                .filter(|n| *n > 0 && *n <= 9_007_199_254_740_991)
        };
        let generation = positive("generation")?;
        let run = positive("run_id")?;
        let epoch = positive("observer_epoch")?;
        let expires = positive("expires_at_unix_seconds")?;
        let witness = value["observer_witness"].as_str()?;
        if witness.len() != 64
            || !witness
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return None;
        }
        let agent = value["agent_id"].as_str()?;
        if !identifier(agent, 128) {
            return None;
        }
        Some(
            serde_json::json!({"protocol":"external-agent/1","registered":true,"controls":[],"agent_id":agent,
            "generation":generation,"observer_witness":witness,"run_id":run,"observer_epoch":epoch,
            "observer_instance":self.observer_instance,"external_session_id":self.external_session_id,
            "usage":"unavailable-source-continuity","observer_transport":"unavailable-curated-freshness",
            "expires_at_unix_seconds":expires,"lease_seconds":60}),
        )
    }
}

/// Runs only exact fixed argv before ordinary startup, with no stdin use. Missing
/// route/capsule, rejected/unavailable peer or malformed reply emits only a fixed
/// unavailable result. Other CLI forms keep normal config/argument handling.
pub(crate) fn run_internal_process(arguments: &[std::ffi::OsString]) -> Option<u8> {
    if arguments.len() != 3 || arguments[1] != "harness-source" {
        return None;
    }
    let result = (|| -> Option<serde_json::Value> {
        let capsule = Capsule::parse(arguments[2].to_str()?.as_bytes()).ok()?;
        let discovery = std::env::var_os("MEZ");
        let socket = super::env::socket_selection_from_mez(discovery.as_ref()).ok()??;
        let pane = std::env::var("MEZ_PANE").ok()?;
        let body = capsule.request(&pane).ok()?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;
        let response = runtime
            .block_on(super::harness_event::exchange_result(
                super::selected_socket_path(&socket),
                &body,
                "harness-source",
            ))
            .ok()?;
        capsule.receipt(&response)
    })()
    .unwrap_or_else(|| serde_json::json!({"registered":false}));
    Some(u8::from(writeln!(std::io::stdout(), "{result}").is_err()))
}

#[cfg(test)]
mod tests;
