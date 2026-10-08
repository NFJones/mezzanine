//! Literal curated SDK transport shared by adapters and native qualification.
//!
//! Rendering supplies only a fixed trusted helper path and a finite timer period;
//! no vendor callback content, credentials, PID, endpoint or RPC selector becomes
//! source code. The body runs after `next(e)` and cannot change its result. SDK
//! timers keep original public selectors, serialize helpers and never re-enroll.
//! Explicit successor bodies require the caller's captured prior public receipt,
//! not a newest lookup, and pin only the newly admitted original-epoch selectors.
//! This module alone installs/enables nothing and grants no usage or
//! daemon authority. The installer must separately establish trusted helper bytes.

use crate::error::{MezError, Result};

/// Renders one content-free SessionStart body for the literal curated SDK scope.
/// Requires an absolute fixed helper and a 1–10s interval within the daemon's
/// 30s proof window. The caller must invoke `next(e)` before this body and return
/// its unchanged result; helper failures are caught inside the rendered source.
pub(crate) fn session_start_body(helper: &std::path::Path, interval_ms: u64) -> Result<String> {
    render_body(helper, interval_ms, "mez-curated-client-1", false, false)
}

/// Renders an explicit same-run handoff in a scope containing `previousObserver`,
/// the caller's actual captured public receipt. Invalid/missing prior identity
/// disables delivery without falling back to initial admission. The instance is
/// a fixed opaque adapter selector, not executable source or native authority.
pub(crate) fn session_start_successor_body(
    helper: &std::path::Path,
    interval_ms: u64,
    instance: &str,
) -> Result<String> {
    render_body(helper, interval_ms, instance, true, false)
}

/// Builds the literal module entry using fixed sibling imports and owned source.
/// It installs/enables nothing, retains no credentials, and neither persists nor
/// fabricates a predecessor across module reload or same-ID session restart.
pub(crate) fn module_source(helper: &std::path::Path, interval_ms: u64) -> Result<String> {
    let body = session_start_owned_body(helper, interval_ms)?;
    Ok(include_str!("curated_module.mjs").replace("__MEZ_SOURCE_BODY__", &body))
}

/// Renders initial source bound to the caller-retained `observerLifetime` pure
/// owner. Duplicate/late admissions cannot publish or schedule another timer;
/// queued callbacks require the original ticket to remain active.
pub(crate) fn session_start_owned_body(
    helper: &std::path::Path,
    interval_ms: u64,
) -> Result<String> {
    render_body(helper, interval_ms, "mez-curated-client-1", false, true)
}

/// Renders successor source using both fixed `previousObserver` and fresh
/// `observerLifetime` scopes. Old owners must be stopped, never rebound.
pub(crate) fn session_start_owned_successor_body(
    helper: &std::path::Path,
    interval_ms: u64,
    instance: &str,
) -> Result<String> {
    render_body(helper, interval_ms, instance, true, true)
}

/// Projects fixed literals exactly once so marker-like filenames or instance
/// identifiers remain data, never trigger subsequent template substitution.
fn render_body(
    helper: &std::path::Path,
    interval_ms: u64,
    instance: &str,
    replacing: bool,
    owned: bool,
) -> Result<String> {
    let path = helper
        .to_str()
        .filter(|path| helper.is_absolute() && path.len() <= 4096 && !path.contains('\0'))
        .ok_or_else(|| {
            MezError::invalid_args("curated client requires an absolute fixed helper")
        })?;
    if !(1000..=10_000).contains(&interval_ms) {
        return Err(MezError::invalid_args("curated proof interval unavailable"));
    }
    if instance.is_empty()
        || instance.len() > 128
        || !instance
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
    {
        return Err(MezError::invalid_args(
            "curated observer instance unavailable",
        ));
    }
    let quoted = serde_json::to_string(path)
        .map_err(|_| MezError::invalid_args("curated helper path unavailable"))?;
    let period = interval_ms.to_string();
    let instance = serde_json::to_string(instance)
        .map_err(|_| MezError::invalid_args("curated observer instance unavailable"))?;
    let literals = [
        ("__MEZ_OWNED__", if owned { "true" } else { "false" }),
        (
            "__MEZ_LIFETIME__",
            if owned {
                "observerLifetime"
            } else {
                "undefined"
            },
        ),
        ("__MEZ_INTERVAL__", period.as_str()),
        ("__MEZ_HELPER__", quoted.as_str()),
        ("__MEZ_INSTANCE__", instance.as_str()),
        (
            "__MEZ_REPLACING__",
            if replacing { "true" } else { "false" },
        ),
        (
            "__MEZ_PREDECESSOR__",
            if replacing {
                "previousObserver"
            } else {
                "undefined"
            },
        ),
    ];
    let template = include_str!("curated_client.mjs");
    let mut result = String::with_capacity(template.len() + quoted.len());
    let mut end = 0;
    for (offset, _) in template.match_indices("__MEZ_") {
        let (marker, literal) = literals
            .iter()
            .find(|(marker, _)| template[offset..].starts_with(marker))
            .ok_or_else(|| MezError::invalid_state("curated source marker unavailable"))?;
        result.push_str(&template[end..offset]);
        result.push_str(literal);
        end = offset + marker.len();
    }
    result.push_str(&template[end..]);
    Ok(result)
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

    /// Public predecessor scope is fixed code, not caller-supplied JavaScript;
    /// marker-like instance/helper bytes survive one-pass rendering exactly.
    #[test]
    fn curated_successor_rendering_keeps_instance_data_and_fixed_scope() {
        let helper = std::path::Path::new("/owned/__MEZ_INSTANCE__");
        let source = session_start_successor_body(helper, 1000, "__MEZ_HELPER__").unwrap();
        assert_eq!(source.matches("\"/owned/__MEZ_INSTANCE__\"").count(), 2);
        assert!(source.contains("const instance = \"__MEZ_HELPER__\";"));
        assert!(source.contains("const prior = previousObserver;"));
        assert!(source.contains("const replacing = true;"));
        for instance in ["", "content with spaces", "a\n", "\"source"] {
            assert!(session_start_successor_body(helper, 1000, instance).is_err());
        }
    }

    /// Explicit owned variants bind only their fixed caller owner scope; missing
    /// scope cannot silently fall back to unowned scheduling or code injection.
    #[test]
    fn curated_owned_source_rendering_keeps_lifetime_scope_explicit() {
        let helper = std::path::Path::new("/owned/mez");
        for source in [
            session_start_owned_body(helper, 1000).unwrap(),
            session_start_owned_successor_body(helper, 1000, "module-b").unwrap(),
        ] {
            assert!(source.contains("const owned = true;"));
            assert!(source.contains("lifetime = observerLifetime;"));
            assert!(source.contains("lifetime.begin(session)"));
            assert!(source.contains("lifetime.current(ticket)"));
            assert!(!source.contains("__MEZ_"));
        }
    }

    /// Module source composes only fixed literal wrappers/imports with the
    /// validated helper body; marker-like helper data is not substituted again.
    #[test]
    fn curated_module_rendering_keeps_fixed_callbacks_and_helper_data() {
        let source =
            module_source(std::path::Path::new("/owned/__MEZ_SOURCE_BODY__"), 1000).unwrap();
        assert_eq!(source.matches("on('classic.SessionStart'").count(), 1);
        assert_eq!(source.matches("on('classic.SessionEnd'").count(), 1);
        assert!(!source.contains("on('session.start'"));
        assert!(source.contains("lifetime = observerLifetime;"));
        assert_eq!(source.matches("\"/owned/__MEZ_SOURCE_BODY__\"").count(), 2);
        assert!(!source.contains("store.get"));
    }
}
