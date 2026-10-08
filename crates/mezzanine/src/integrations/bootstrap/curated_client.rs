//! Literal curated SDK transport shared by adapters and native qualification.
//!
//! Rendering supplies only a fixed trusted helper path and a finite timer period;
//! no vendor callback content, credentials, PID, endpoint or RPC selector becomes
//! source code. The body runs after `next(e)` and cannot change its result. SDK
//! timers keep original public selectors, serialize helpers and never re-enroll.
//! This module alone installs/enables nothing and grants no replacement, usage or
//! daemon authority. The installer must separately establish trusted helper bytes.

use crate::error::{MezError, Result};

/// Renders one content-free SessionStart body for the literal curated SDK scope.
/// Requires an absolute fixed helper and a 1–10s interval within the daemon's
/// 30s proof window. The caller must invoke `next(e)` before this body and return
/// its unchanged result; helper failures are caught inside the rendered source.
pub(crate) fn session_start_body(helper: &std::path::Path, interval_ms: u64) -> Result<String> {
    let path = helper
        .to_str()
        .filter(|path| helper.is_absolute() && path.len() <= 4096 && !path.contains('\0'))
        .ok_or_else(|| {
            MezError::invalid_args("curated client requires an absolute fixed helper")
        })?;
    if !(1000..=10_000).contains(&interval_ms) {
        return Err(MezError::invalid_args("curated proof interval unavailable"));
    }
    let quoted = serde_json::to_string(path)
        .map_err(|_| MezError::invalid_args("curated helper path unavailable"))?;
    // Replace the interval first: a valid helper filename can contain the marker
    // spelling, and must never be interpreted as another template substitution.
    Ok(include_str!("curated_client.mjs")
        .replace("__MEZ_INTERVAL__", &interval_ms.to_string())
        .replace("__MEZ_HELPER__", &quoted))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixed argv quoting is data, not executable template input. Unsupported
    /// paths/periods fail before source generation, and marker-like filenames
    /// remain exact after rendering instead of becoming a timing selector.
    #[test]
    fn curated_client_rendering_preserves_fixed_helper_and_bounds() {
        assert!(session_start_body(std::path::Path::new("relative"), 1000).is_err());
        for period in [0, 999, 10_001, u64::MAX] {
            assert!(session_start_body(std::path::Path::new("/owned/mez"), period).is_err());
        }
        let path = std::path::Path::new("/owned/__MEZ_INTERVAL__\"helper");
        let source = session_start_body(path, 10_000).unwrap();
        let quoted = serde_json::to_string(path.to_str().unwrap()).unwrap();
        assert_eq!(source.matches(&quoted).count(), 2);
        assert!(source.contains("$.clock.every(10000,"));
        assert!(!source.contains("__MEZ_HELPER__"));
    }
}
