//! OAuth refresh rejection and session-local fencing.
//!
//! Permanent rejection and uncommitted successful rotation are cached;
//! transient provider failures are not. Per-identity async locks
//! serialize rotation without blocking other providers. Fingerprints never
//! leave this module or appear in diagnostics; replacing metadata or secrets
//! permits explicit login recovery without restarting the runtime.

use crate::error::{MezError, Result};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// One credential identity's serialized refresh and last rejected generation.
type RefreshFence = Arc<tokio::sync::Mutex<RefreshState>>;

/// Successful rotations survive lock contention/cancellation until committed or
/// superseded by explicit login. Secret-bearing credentials redact Debug.
#[derive(Default)]
pub(super) struct RefreshState {
    pub(super) rejected: Option<[u8; 32]>,
    pub(super) provider_rotation: Option<([u8; 32], super::OpenAiProviderCredential)>,
    pub(super) mcp_rotation: Option<([u8; 32], super::McpOAuthCredential)>,
}

/// Shared session-local refresh fences, with secret-safe debug output.
#[derive(Clone, Default)]
pub(super) struct RefreshFences(Arc<Mutex<BTreeMap<String, RefreshFence>>>);

impl std::fmt::Debug for RefreshFences {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RefreshFences { .. }")
    }
}

impl RefreshFences {
    /// Reads completed rejection state without waiting on in-flight refresh I/O.
    pub(super) fn is_rejected(&self, identity: String, generation: [u8; 32]) -> Result<bool> {
        let fence = self.for_identity(identity)?;
        Ok(fence
            .try_lock()
            .is_ok_and(|state| state.rejected == Some(generation)))
    }

    /// Gets a scoped lock without holding the map lock across network I/O.
    pub(super) fn for_identity(&self, identity: String) -> Result<RefreshFence> {
        let mut fences = self
            .0
            .lock()
            .map_err(|_| MezError::invalid_state("auth refresh state unavailable"))?;
        Ok(fences.entry(identity).or_default().clone())
    }
}

/// Hashes exact credential generation bytes without exposing them in state.
pub(super) fn fingerprint(metadata: &impl std::fmt::Debug, token: &str) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(format!("{metadata:?}"));
    hash.update([0]);
    hash.update(token.as_bytes());
    hash.finalize().into()
}

/// Classifies only permanent OAuth rejection, not rate limits or server errors.
pub(super) fn rejected(status: u16, body: &str) -> bool {
    if status == 401 {
        return true;
    }
    if status != 400 && status != 403 {
        return false;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };
    let code = value
        .get("error")
        .and_then(|v| v.as_str())
        .or_else(|| value.pointer("/error/code").and_then(|v| v.as_str()));
    matches!(
        code,
        Some(
            "invalid_grant"
                | "invalid_token"
                | "refresh_token_expired"
                | "refresh_token_reused"
                | "refresh_token_invalidated"
        )
    )
}

/// Produces actionable bounded diagnostics from trusted local identities only.
pub(super) fn required(identity: &str, mcp: bool) -> MezError {
    let command = if mcp {
        format!("mez mcp login {identity}")
    } else {
        if identity == "openai" {
            "mez auth login --provider openai".to_string()
        } else {
            format!("mez auth login --provider openai --auth-profile {identity}")
        }
    };
    MezError::forbidden(format!(
        "authentication required for {} `{identity}`; refresh credential is unavailable or rejected; run `{command}`",
        if mcp {
            "MCP server"
        } else {
            "provider auth profile"
        }
    ))
}
