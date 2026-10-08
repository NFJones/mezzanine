//! Deterministic vendor user-root selection, separate from filesystem authority.
//!
//! Explicit --root wins over one documented vendor directory variable, then
//! applicable XDG/default HOME conventions. Never probes cwd, Git, focus, vendor
//! binaries, credentials or files. Invalid selected overrides fail rather than
//! silently choose another tree. Selection does not create roots, normalize
//! permissions or prove installation/runtime support; publication owns access.
//! Sources: Claude env-vars/settings; Codex config-advanced; GitHub Copilot CLI
//! configuration directory; Cursor CLI configuration; Pi configuration; OpenCode
//! config/custom-directory and its installed public XDG path implementation.

use crate::error::{MezError, Result};
use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

/// Root spelling plus inert diagnostic provenance; neither is a capability.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SelectedRoot {
    /// Absolute bounded selected path; filesystem checks remain publisher-owned.
    pub(crate) path: PathBuf,
    /// Fixed origin label, never an arbitrary environment value or secret.
    pub(crate) source: &'static str,
}

/// Reject ambiguous/unsafe spellings without reads, writes or shell expansion.
/// Literal non-UTF8 Unix paths remain supported; control/NUL bytes and parent
/// traversal cannot enter diagnostics or descriptor traversal.
fn checked(path: &Path, source: &'static str) -> Result<SelectedRoot> {
    let bytes = path.as_os_str().as_bytes();
    if !path.is_absolute()
        || bytes.len() > 4096
        || bytes.iter().any(|byte| byte.is_ascii_control())
        || path
            .components()
            .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
    {
        return Err(MezError::invalid_args(format!(
            "bootstrap {source} must select a bounded absolute directory without traversal"
        )));
    }
    Ok(SelectedRoot {
        path: path.into(),
        source,
    })
}

/// Selects from only the necessary process variables. macOS Cursor ignores XDG,
/// while OpenCode follows XDG on both supported Unix platforms. Retired/unknown
/// names reject before reading any environment or considering an explicit root.
pub(crate) fn resolve(harness: &str, explicit: Option<&Path>) -> Result<SelectedRoot> {
    resolve_with_environment(
        harness,
        explicit,
        |key| std::env::var_os(key),
        cfg!(target_os = "macos"),
    )
}

/// Injectable pure selector keeps tests isolated from unsafe global env mutation.
/// Empty, relative or invalid overrides are errors, never silent fallback.
pub(crate) fn resolve_with_environment(
    harness: &str,
    explicit: Option<&Path>,
    mut environment: impl FnMut(&str) -> Option<OsString>,
    macos: bool,
) -> Result<SelectedRoot> {
    let (variable, suffix, xdg) = match harness {
        "claude" => ("CLAUDE_CONFIG_DIR", ".claude", false),
        "codex" => ("CODEX_HOME", ".codex", false),
        "copilot" => ("COPILOT_HOME", ".copilot", false),
        "cursor" => ("CURSOR_CONFIG_DIR", ".cursor", !macos),
        "pi" => ("PI_CODING_AGENT_DIR", ".pi/agent", false),
        "opencode" => ("OPENCODE_CONFIG_DIR", ".config/opencode", true),
        _ => {
            return Err(MezError::invalid_args(
                "bootstrap harness has no user-root policy",
            ));
        }
    };
    if let Some(root) = explicit {
        return checked(root, "explicit");
    }
    if let Some(root) = environment(variable) {
        return checked(Path::new(&root), variable);
    }
    if xdg && let Some(base) = environment("XDG_CONFIG_HOME") {
        let base = checked(Path::new(&base), "XDG_CONFIG_HOME")?;
        return checked(&base.path.join(harness), "XDG_CONFIG_HOME");
    }
    let home = environment("HOME").ok_or_else(|| {
        MezError::invalid_args("bootstrap HOME is unavailable; cannot select vendor user root")
    })?;
    let home = checked(Path::new(&home), "HOME")?;
    checked(&home.path.join(suffix), "vendor-default")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All six user defaults are documented harness-specific locations, not
    /// arbitrary project ancestors or aliases for one generic configuration tree.
    #[test]
    fn bootstrap_roots_six_defaults_are_vendor_specific() {
        for (harness, suffix) in [
            ("claude", ".claude"),
            ("codex", ".codex"),
            ("copilot", ".copilot"),
            ("cursor", ".cursor"),
            ("pi", ".pi/agent"),
            ("opencode", ".config/opencode"),
        ] {
            for macos in [true, false] {
                let selected = resolve_with_environment(
                    harness,
                    None,
                    |key| (key == "HOME").then(|| "/home/fixture".into()),
                    macos,
                )
                .unwrap();
                assert_eq!(selected.path, Path::new("/home/fixture").join(suffix));
                assert_eq!(selected.source, "vendor-default");
            }
        }
    }

    /// Explicit override needs no HOME/env probe. Vendor directory variables
    /// override XDG/HOME, and irrelevant credential/file variables are never read.
    #[test]
    fn bootstrap_roots_explicit_and_vendor_environment_precedence() {
        for (harness, variable) in [
            ("claude", "CLAUDE_CONFIG_DIR"),
            ("codex", "CODEX_HOME"),
            ("copilot", "COPILOT_HOME"),
            ("cursor", "CURSOR_CONFIG_DIR"),
            ("pi", "PI_CODING_AGENT_DIR"),
            ("opencode", "OPENCODE_CONFIG_DIR"),
        ] {
            let selected = resolve_with_environment(
                harness,
                Some(Path::new("/explicit")),
                |_| panic!("explicit root probed environment"),
                false,
            )
            .unwrap();
            assert_eq!(selected.path, Path::new("/explicit"));
            let selected = resolve_with_environment(
                harness,
                None,
                |key| {
                    assert_eq!(key, variable);
                    Some("/override".into())
                },
                false,
            )
            .unwrap();
            assert_eq!(selected.path, Path::new("/override"));
            assert_eq!(selected.source, variable);
        }
    }

    /// Cursor XDG is Linux/BSD-specific; OpenCode honors it on macOS too.
    /// Copilot's previous XDG location is not reused by this modern selector.
    #[test]
    fn bootstrap_roots_xdg_is_platform_and_vendor_specific() {
        for (harness, macos, expected) in [
            ("cursor", false, "/xdg/cursor"),
            ("cursor", true, "/home/fixture/.cursor"),
            ("opencode", false, "/xdg/opencode"),
            ("opencode", true, "/xdg/opencode"),
            ("copilot", false, "/home/fixture/.copilot"),
        ] {
            let selected = resolve_with_environment(
                harness,
                None,
                |key| match key {
                    "HOME" => Some("/home/fixture".into()),
                    "XDG_CONFIG_HOME" => Some("/xdg".into()),
                    _ => None,
                },
                macos,
            )
            .unwrap();
            assert_eq!(selected.path, Path::new(expected));
        }
    }

    /// Root spellings are data, not expansion or filesystem eligibility. Bounds
    /// and NUL/control rejection are deterministic; non-UTF8 Unix paths retain
    /// exact OS bytes rather than being rewritten for diagnostic serialization.
    #[test]
    fn bootstrap_roots_literal_unix_path_bounds_are_exact() {
        use std::os::unix::ffi::OsStringExt;
        let path = PathBuf::from(OsString::from_vec(b"/root/\xff".to_vec()));
        let selected = checked(&path, "explicit").unwrap();
        assert_eq!(
            selected.path.as_os_str().as_bytes(),
            path.as_os_str().as_bytes()
        );
        for bytes in [
            b"/root/\0bad".to_vec(),
            b"/root/\x7fbad".to_vec(),
            format!("/{}", "a".repeat(4096)).into_bytes(),
        ] {
            assert!(checked(&PathBuf::from(OsString::from_vec(bytes)), "explicit").is_err());
        }
    }

    /// Invalid selected values never choose a different destination. Unknown or
    /// retired harnesses cannot probe environment, even with explicit paths.
    #[test]
    fn bootstrap_roots_reject_invalid_selected_overrides_without_fallback() {
        for value in ["", "relative", "~/quoted", "/root/../other", "/root\nother"] {
            assert!(
                resolve_with_environment(
                    "pi",
                    None,
                    |key| {
                        assert_eq!(key, "PI_CODING_AGENT_DIR");
                        Some(value.into())
                    },
                    false
                )
                .is_err()
            );
        }
        assert!(resolve_with_environment("pi", None, |_| None, false).is_err());
        assert!(
            resolve_with_environment(
                "opencode",
                None,
                |key| (key == "XDG_CONFIG_HOME").then(|| "relative".into()),
                false
            )
            .is_err()
        );
        for harness in ["gemini", "unknown"] {
            assert!(
                resolve_with_environment(
                    harness,
                    Some(Path::new("/explicit")),
                    |_| panic!("retired/unknown root probed environment"),
                    false
                )
                .is_err()
            );
        }
    }
}
