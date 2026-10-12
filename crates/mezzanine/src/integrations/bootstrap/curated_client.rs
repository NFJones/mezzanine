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

/// Renders the fixed accounting runner in a scope with `$`, the imported pure
/// client and an already admitted `observerLifetime`. No event/counter schema,
/// vendor hook, capability, installation or re-enrollment is supplied here.
pub(crate) fn request_accounting_body(helper: &std::path::Path) -> Result<String> {
    let path = helper
        .to_str()
        .filter(|path| helper.is_absolute() && path.len() <= 4096 && !path.contains('\0'))
        .ok_or_else(|| MezError::invalid_args("curated request helper must be absolute"))?;
    let quoted = serde_json::to_string(path)
        .map_err(|_| MezError::invalid_args("curated request helper unavailable"))?;
    Ok(include_str!("curated_request_runner.mjs").replace("__MEZ_HELPER__", &quoted))
}

/// Supplies only two literal sibling modules required by the accounting runner.
/// Adapter-owned deployment must establish trusted bytes; this performs no I/O.
pub(crate) fn request_accounting_sources() -> [(&'static str, &'static str); 2] {
    [
        (
            "curated_request_accounting.mjs",
            include_str!("curated_request_accounting.mjs"),
        ),
        (
            "request_accounting.mjs",
            include_str!("request_accounting.mjs"),
        ),
    ]
}

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
    compose_module(
        helper,
        interval_ms,
        include_str!("curated_module.mjs"),
        true,
        true,
        include_str!("curated_client.mjs"),
        include_str!("claude_request_step.mjs"),
    )
}

/// Reconstructs the shipped revision-1 module only while every unchanged source
/// input remains byte-identical. Future edits fail closed until their owner adds
/// an independent snapshot; active code never silently redefines old authority.
pub(super) fn historical_claude_module_v1(helper: &std::path::Path) -> Result<String> {
    use sha2::Digest;
    for (bytes, expected) in [
        (
            include_bytes!("history/curated_client_claude_v1.mjs").as_slice(),
            "1a395b002716be900ace04094ed4d1d64e64d81d598ce72a2b93d862dfa8c0bb",
        ),
        (
            include_bytes!("history/claude_request_step_v4.mjs").as_slice(),
            "f438a048fa4fb1de35812a8a04ba138bd0de3629eb44949f77e15df28c35ce92",
        ),
        (
            include_bytes!("curated_request_runner.mjs").as_slice(),
            "4ea8ec867b94fe53578e78774f02e8b8a12069abbaef5f5754b6bba7950eb0b1",
        ),
        (
            include_bytes!("curated_lifetime.mjs").as_slice(),
            "6e0fa6c94d99a7b84663d9fe19f72e499464eb95d35ad69d85417c2286ad17b2",
        ),
        (
            include_bytes!("claude_request.mjs").as_slice(),
            "32b4b295e16a9d07afffd604aa4ec7500fa417f6945a4f3f6e2e5f0051ebecf4",
        ),
        (
            include_bytes!("curated_request_accounting.mjs").as_slice(),
            "20cbc1c0ce6f66836a4fa0200bc98418d7b78941138e575fd4d5c6bae9101a2e",
        ),
        (
            include_bytes!("request_accounting.mjs").as_slice(),
            "43788869b5404f5a098513da7bba66463fd084731b7bd6f76bd9809eff134053",
        ),
        (
            include_bytes!("history/curated_module_claude_v1.mjs").as_slice(),
            "d4392ec13b02a4bfea8ce6b6d3aa1983f0ad6b327e7df82ebd5c1ddb0517d65e",
        ),
        (
            include_bytes!("history/claude_observer_v1.mjs").as_slice(),
            "cd84275d11543d8b566b425ffdbfb2e3787e83c04deee37ab6271bcf3003e47b",
        ),
    ] {
        let actual = sha2::Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        if actual != expected {
            return Err(MezError::invalid_state(
                "Claude historical source snapshot changed",
            ));
        }
    }
    compose_module(
        helper,
        10_000,
        include_str!("history/curated_module_claude_v1.mjs"),
        false,
        false,
        include_str!("history/curated_client_claude_v1.mjs"),
        include_str!("history/claude_request_step_v4.mjs"),
    )
}

/// Rebuilds the exact revision-2 module from its independent frozen template.
/// Shared sources must still match the reviewed shipped inputs; no receipt path
/// or current paired-clear template can redefine historical ownership.
pub(super) fn historical_claude_module_v2(helper: &std::path::Path) -> Result<String> {
    use sha2::Digest;
    historical_claude_module_v1(helper)?;
    for (bytes, expected) in [
        (
            include_bytes!("history/curated_module_claude_v2.mjs").as_slice(),
            "e80bd972c6d20c1e6348c8aea353298ebf1bbf8963ce73feea362a4cb4c1797c",
        ),
        (
            include_bytes!("claude_observer.mjs").as_slice(),
            "22df7f5ce5b50a8ebb6866023f2e3f815a41dad2267ab5802c01465a65e6d454",
        ),
        (
            include_bytes!("claude_lifecycle_runner.mjs").as_slice(),
            "ee644ef636ca93104cc3128ea0095d4d27419c1f2abd6714295967fd59800b67",
        ),
    ] {
        let actual = sha2::Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        if actual != expected {
            return Err(MezError::invalid_state(
                "Claude historical revision-2 source changed",
            ));
        }
    }
    compose_module(
        helper,
        10_000,
        include_str!("history/curated_module_claude_v2.mjs"),
        true,
        false,
        include_str!("history/curated_client_claude_v1.mjs"),
        include_str!("history/claude_request_step_v4.mjs"),
    )
}

/// Rebuilds revision-3 paired-clear bytes with the frozen pre-rejection client.
/// Unchanged module inputs remain fingerprint-guarded; future source changes
/// must add their own snapshot rather than redefine installed ownership.
pub(super) fn historical_claude_module_v3(helper: &std::path::Path) -> Result<String> {
    use sha2::Digest;
    historical_claude_module_v2(helper)?;
    let actual = sha2::Sha256::digest(include_bytes!("history/curated_module_claude_v4.mjs"))
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if actual != "87db659ad7a77fc35178e6cbdf3840028129f056aa7ac1e12de673b484386ae2" {
        return Err(MezError::invalid_state(
            "Claude historical revision-3 source changed",
        ));
    }
    compose_module(
        helper,
        10_000,
        include_str!("history/curated_module_claude_v4.mjs"),
        true,
        true,
        include_str!("history/curated_client_claude_v1.mjs"),
        include_str!("history/claude_request_step_v4.mjs"),
    )
}

/// Reconstructs shipped revision-4 negative-proof closure independently of
/// current retention/client changes. Earlier recipes validate shared inputs;
/// the frozen final revision-4 client preserves transient versus terminal proof.
pub(super) fn historical_claude_module_v4(helper: &std::path::Path) -> Result<String> {
    use sha2::Digest;
    historical_claude_module_v3(helper)?;
    let actual = sha2::Sha256::digest(include_bytes!("history/curated_client_claude_v4.mjs"))
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if actual != "1376398079a8c98804ca77902589ce1fb875cff28d63ceb383e6aabfef2b15e0" {
        return Err(MezError::invalid_state(
            "Claude historical revision-4 source changed",
        ));
    }
    compose_module(
        helper,
        10_000,
        include_str!("history/curated_module_claude_v4.mjs"),
        true,
        true,
        include_str!("history/curated_client_claude_v4.mjs"),
        include_str!("history/claude_request_step_v4.mjs"),
    )
}

/// One-pass insertion preserves literal helper filenames even when they contain
/// template markers. Both source graphs are code-owned, never caller input.
fn compose_module(
    helper: &std::path::Path,
    interval_ms: u64,
    template: &str,
    lifecycle: bool,
    paired_clear: bool,
    client_template: &str,
    step_template: &str,
) -> Result<String> {
    let body = render_body_mode(
        helper,
        interval_ms,
        "mez-curated-client-1",
        false,
        true,
        paired_clear,
        client_template,
    )?;
    let request = request_accounting_body(helper)?;
    let (step_before, step_after) = step_template
        .split_once("__MEZ_REQUEST_BODY__")
        .ok_or_else(|| MezError::invalid_state("Claude request runner marker unavailable"))?;
    let step = format!("{step_before}{request}{step_after}");
    let (before, tail) = template
        .split_once("__MEZ_SOURCE_BODY__")
        .ok_or_else(|| MezError::invalid_state("curated source marker unavailable"))?;
    let (middle, lifecycle_body, tail) = if lifecycle {
        let (middle, tail) = tail
            .split_once("__MEZ_LIFECYCLE_BODY__")
            .ok_or_else(|| MezError::invalid_state("Claude lifecycle marker unavailable"))?;
        let quoted = serde_json::to_string(
            helper
                .to_str()
                .ok_or_else(|| MezError::invalid_args("Claude helper unavailable"))?,
        )
        .map_err(|_| MezError::invalid_args("Claude helper unavailable"))?;
        (
            middle,
            include_str!("claude_lifecycle_runner.mjs").replace("__MEZ_HELPER__", &quoted),
            tail,
        )
    } else {
        ("", String::new(), tail)
    };
    let (step_middle, after) = tail
        .split_once("__MEZ_STEP_BODY__")
        .ok_or_else(|| MezError::invalid_state("curated step marker unavailable"))?;
    // Insert each literal once; marker-like helper filenames remain inert data.
    Ok(format!(
        "{before}{body}{middle}{lifecycle_body}{step_middle}{step}{after}"
    ))
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
    render_body_mode(
        helper,
        interval_ms,
        instance,
        replacing,
        owned,
        false,
        include_str!("curated_client.mjs"),
    )
}

/// Expands only fixed literals or code-owned module-local clear selectors.
/// The paired mode receives no vendor object or arbitrary expression; historical
/// and explicit standalone bodies retain their original literal substitutions.
fn render_body_mode(
    helper: &std::path::Path,
    interval_ms: u64,
    instance: &str,
    replacing: bool,
    owned: bool,
    paired_clear: bool,
    template: &str,
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
        (
            "__MEZ_INSTANCE__",
            if paired_clear {
                "startInstance"
            } else {
                instance.as_str()
            },
        ),
        (
            "__MEZ_REPLACING__",
            if paired_clear {
                "previousObserver !== undefined"
            } else if replacing {
                "true"
            } else {
                "false"
            },
        ),
        (
            "__MEZ_PREDECESSOR__",
            if replacing || paired_clear {
                "previousObserver"
            } else {
                "undefined"
            },
        ),
    ];
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

    /// The SDK runner quotes a single trusted helper as data and fences its
    /// captured receipt identity. Pure sibling source has no Node/DOM imports;
    /// generation/counter content never becomes generated executable code.
    #[test]
    fn curated_request_runner_keeps_fixed_helper_and_pure_sources() {
        assert!(request_accounting_body(std::path::Path::new("relative")).is_err());
        let helper = std::path::Path::new("/owned/__MEZ_HELPER__\"binary");
        let body = request_accounting_body(helper).unwrap();
        let quoted = serde_json::to_string(helper.to_str().unwrap()).unwrap();
        assert_eq!(body.matches(&quoted).count(), 1);
        assert!(body.contains("observerLifetime.receipt === originalObserver"));
        assert!(body.contains("$.process.run(["));
        for (_, source) in request_accounting_sources() {
            assert!(!source.contains("from 'node:"));
            assert!(!source.contains("from \"node:"));
            assert!(!source.contains("Buffer.byteLength"));
        }
    }

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
        assert_eq!(source.matches("on('turn.step'").count(), 1);
        assert!(source.contains("yield* next(e)"));
        assert!(!source.contains("on('session.start'"));
        assert!(source.contains("lifetime = observerLifetime;"));
        assert_eq!(source.matches("\"/owned/__MEZ_SOURCE_BODY__\"").count(), 4);
        assert!(!source.contains("store.get"));
    }
}
