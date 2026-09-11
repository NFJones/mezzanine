//! Pane-creation environment boundary for runtime-created agent-owned panes.
//!
//! Runtime-created agent-owned panes start from a cleared base instead of the
//! ambient `mez` daemon environment. This module is the single owner of that
//! boundary: it projects a fixed, documented allowlist out of the daemon
//! environment snapshot and pins the harness-owned `SHELL` value, so a
//! credential that exists only in the daemon environment never becomes an
//! agent-owned pane root's exec-time environment, and therefore never becomes
//! native workload evidence.
//!
//! User-initiated panes keep inheriting the user environment exactly as before;
//! only `RuntimePaneProcessPurpose::AgentOwned` panes cross this boundary.
//!
//! The guarantee is deliberate composition, not credential non-possession: a
//! value deliberately passed at pane creation, or later exported by the pane
//! shell, remains authoritative pane evidence and is forwarded by design.
//!
//! Allowlist rules:
//! - Only the documented names below are projected from the daemon snapshot.
//!   `SSH_AUTH_SOCK`, proxy variables, toolchain variables, and `XDG_*` are
//!   never forwarded.
//! - Each projected key and value must satisfy the shared native workload
//!   environment validation authority; malformed or oversized values are
//!   dropped deterministically instead of failing the launch.
//! - `PATH` falls back to the documented native workload search path, `HOME`
//!   falls back to the runtime home directory, and `TMPDIR` must be absolute.
//! - `SHELL` is a fixed harness value equal to the resolved launch shell when
//!   that path is absolute, independent of any daemon value.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;

use mez_mux::process::RawEnvironmentEntry;

use super::native_workload_environment::{
    NATIVE_WORKLOAD_MAX_ENTRIES, NATIVE_WORKLOAD_MAX_TOTAL_VALUE_BYTES,
    NATIVE_WORKLOAD_PATH_FALLBACK, environment_value_is_valid, portable_environment_key,
};
use super::startup::runtime_home_directory;

/// Daemon locale and identity-class names forwarded verbatim when valid.
const AGENT_OWNED_VERBATIM_NAMES: &[&str] = &[
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    "LC_COLLATE",
    "LC_NUMERIC",
    "LC_TIME",
    "TZ",
    "USER",
    "LOGNAME",
];

/// Composes the pane-creation environment for one runtime-created agent-owned pane.
///
/// The result starts from a cleared base: only the documented allowlist names
/// are projected from `daemon`, each validated through the shared native
/// workload environment authority. Malformed or oversized daemon values are
/// dropped deterministically, and absent optional values fall back to their
/// documented defaults instead of producing a typed error.
pub(crate) fn agent_owned_pane_environment(
    daemon: &[RawEnvironmentEntry],
    resolved_shell: &Path,
) -> Vec<(OsString, OsString)> {
    let mut entries: Vec<(OsString, OsString)> = Vec::new();
    let mut total_value_bytes = 0usize;

    let path = daemon_allowlisted_value(daemon, "PATH")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| NATIVE_WORKLOAD_PATH_FALLBACK.as_bytes().to_vec());
    push_allowlisted(&mut entries, &mut total_value_bytes, "PATH", &path);

    let home = daemon_allowlisted_value(daemon, "HOME")
        .filter(|value| !value.is_empty())
        .or_else(|| runtime_home_directory().map(|home| home.into_os_string().into_vec()));
    if let Some(home) = home {
        push_allowlisted(&mut entries, &mut total_value_bytes, "HOME", &home);
    }

    for name in AGENT_OWNED_VERBATIM_NAMES {
        if let Some(value) = daemon_allowlisted_value(daemon, name) {
            push_allowlisted(&mut entries, &mut total_value_bytes, name, &value);
        }
    }

    let tmpdir = daemon_allowlisted_value(daemon, "TMPDIR")
        .filter(|value| Path::new(OsStr::from_bytes(value)).is_absolute());
    if let Some(tmpdir) = tmpdir {
        push_allowlisted(&mut entries, &mut total_value_bytes, "TMPDIR", &tmpdir);
    }

    if resolved_shell.is_absolute() {
        push_allowlisted(
            &mut entries,
            &mut total_value_bytes,
            "SHELL",
            resolved_shell.as_os_str().as_bytes(),
        );
    }

    entries
}

/// Returns the last validated daemon value for one allowlisted name.
///
/// Only allowlisted names are ever looked up, so a daemon-only credential is
/// structurally unreachable. An invalid occurrence is skipped so the last
/// valid occurrence wins, matching the shared evidence policy.
fn daemon_allowlisted_value(daemon: &[RawEnvironmentEntry], name: &str) -> Option<Vec<u8>> {
    let key = name.as_bytes();
    if !portable_environment_key(key) {
        return None;
    }
    daemon
        .iter()
        .rev()
        .find(|entry| entry.key == key && environment_value_is_valid(&entry.value))
        .map(|entry| entry.value.clone())
}

/// Pushes one validated allowlist entry while honoring the documented budgets.
fn push_allowlisted(
    entries: &mut Vec<(OsString, OsString)>,
    total_value_bytes: &mut usize,
    name: &str,
    value: &[u8],
) {
    if !portable_environment_key(name.as_bytes()) || !environment_value_is_valid(value) {
        return;
    }
    if entries.len() >= NATIVE_WORKLOAD_MAX_ENTRIES
        || total_value_bytes.saturating_add(value.len()) > NATIVE_WORKLOAD_MAX_TOTAL_VALUE_BYTES
    {
        return;
    }
    *total_value_bytes = total_value_bytes.saturating_add(value.len());
    entries.push((OsString::from(name), OsString::from_vec(value.to_vec())));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: &str, value: &str) -> RawEnvironmentEntry {
        RawEnvironmentEntry {
            key: key.as_bytes().to_vec(),
            value: value.as_bytes().to_vec(),
        }
    }

    fn value_for(environment: &[(OsString, OsString)], name: &str) -> Option<OsString> {
        environment
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    }

    fn forwards(environment: &[(OsString, OsString)], name: &str) -> bool {
        environment.iter().any(|(key, _)| key == name)
    }

    #[test]
    fn forwards_only_the_documented_allowlist() {
        let daemon = vec![
            entry("PATH", "/usr/local/bin:/usr/bin:/bin"),
            entry("HOME", "/home/agent"),
            entry("LANG", "en_US.UTF-8"),
            entry("LC_ALL", "C.UTF-8"),
            entry("LC_CTYPE", "C.UTF-8"),
            entry("LC_MESSAGES", "C"),
            entry("LC_COLLATE", "C"),
            entry("LC_NUMERIC", "C"),
            entry("LC_TIME", "C"),
            entry("TZ", "UTC"),
            entry("USER", "agent"),
            entry("LOGNAME", "agent"),
            entry("TMPDIR", "/tmp/pane"),
            entry("SSH_AUTH_SOCK", "/run/user/1000/ssh-agent.sock"),
            entry("HTTPS_PROXY", "http://proxy.invalid:3128"),
            entry("CARGO_HOME", "/daemon-only/cargo"),
            entry("RUSTUP_HOME", "/daemon-only/rustup"),
            entry("XDG_CONFIG_HOME", "/daemon-only/config"),
            entry("MEZ_DAEMON_ONLY_SENTINEL", "daemon-only"),
        ];
        let environment = agent_owned_pane_environment(&daemon, Path::new("/bin/bash"));

        assert_eq!(
            value_for(&environment, "PATH"),
            Some(OsString::from("/usr/local/bin:/usr/bin:/bin"))
        );
        assert_eq!(
            value_for(&environment, "HOME"),
            Some(OsString::from("/home/agent"))
        );
        assert_eq!(
            value_for(&environment, "LANG"),
            Some(OsString::from("en_US.UTF-8"))
        );
        assert_eq!(
            value_for(&environment, "LC_ALL"),
            Some(OsString::from("C.UTF-8"))
        );
        assert_eq!(
            value_for(&environment, "LC_CTYPE"),
            Some(OsString::from("C.UTF-8"))
        );
        assert_eq!(
            value_for(&environment, "LC_MESSAGES"),
            Some(OsString::from("C"))
        );
        assert_eq!(
            value_for(&environment, "LC_COLLATE"),
            Some(OsString::from("C"))
        );
        assert_eq!(
            value_for(&environment, "LC_NUMERIC"),
            Some(OsString::from("C"))
        );
        assert_eq!(
            value_for(&environment, "LC_TIME"),
            Some(OsString::from("C"))
        );
        assert_eq!(value_for(&environment, "TZ"), Some(OsString::from("UTC")));
        assert_eq!(
            value_for(&environment, "USER"),
            Some(OsString::from("agent"))
        );
        assert_eq!(
            value_for(&environment, "LOGNAME"),
            Some(OsString::from("agent"))
        );
        assert_eq!(
            value_for(&environment, "TMPDIR"),
            Some(OsString::from("/tmp/pane"))
        );
        assert_eq!(
            value_for(&environment, "SHELL"),
            Some(OsString::from("/bin/bash"))
        );

        for dropped in [
            "SSH_AUTH_SOCK",
            "HTTPS_PROXY",
            "CARGO_HOME",
            "RUSTUP_HOME",
            "XDG_CONFIG_HOME",
            "MEZ_DAEMON_ONLY_SENTINEL",
        ] {
            assert!(
                !forwards(&environment, dropped),
                "{dropped} must not be forwarded"
            );
        }
    }

    #[test]
    fn duplicates_resolve_to_the_last_valid_occurrence() {
        let environment = agent_owned_pane_environment(
            &[entry("LANG", "first"), entry("LANG", "last")],
            Path::new("/bin/sh"),
        );
        assert_eq!(
            value_for(&environment, "LANG"),
            Some(OsString::from("last"))
        );

        let trailing_invalid = agent_owned_pane_environment(
            &[
                entry("LANG", "en_US.UTF-8"),
                entry("LANG", "invalid\0duplicate"),
            ],
            Path::new("/bin/sh"),
        );
        assert_eq!(
            value_for(&trailing_invalid, "LANG"),
            Some(OsString::from("en_US.UTF-8"))
        );
    }

    #[test]
    fn path_and_home_fall_back_when_daemon_values_are_absent_or_empty() {
        let fallback = OsString::from(NATIVE_WORKLOAD_PATH_FALLBACK);
        let expected_home = runtime_home_directory().map(|home| home.into_os_string());

        let absent = agent_owned_pane_environment(&[], Path::new("/bin/sh"));
        assert_eq!(value_for(&absent, "PATH"), Some(fallback.clone()));
        assert_eq!(value_for(&absent, "HOME"), expected_home);

        let empty = agent_owned_pane_environment(
            &[entry("PATH", ""), entry("HOME", "")],
            Path::new("/bin/sh"),
        );
        assert_eq!(value_for(&empty, "PATH"), Some(fallback));
        assert_eq!(value_for(&empty, "HOME"), expected_home);
    }

    #[test]
    fn drops_malformed_nul_and_oversized_values_deterministically() {
        let oversized = "x".repeat(NATIVE_WORKLOAD_MAX_TOTAL_VALUE_BYTES + 1);
        let nul_value = "value\0with-nul";
        let daemon = vec![
            entry("PATH", "/usr/bin:/bin"),
            entry("LANG", oversized.as_str()),
            entry("TZ", nul_value),
            entry("LC_ALL", "C.UTF-8"),
            entry("CARGO_HOME", "/daemon-only/cargo"),
        ];
        let environment = agent_owned_pane_environment(&daemon, Path::new("/bin/sh"));

        assert!(
            !forwards(&environment, "LANG"),
            "oversized value must be dropped"
        );
        assert!(!forwards(&environment, "TZ"), "NUL value must be dropped");
        assert!(
            !forwards(&environment, "CARGO_HOME"),
            "a well-formed non-allowlisted name must not be forwarded"
        );
        assert_eq!(
            value_for(&environment, "LC_ALL"),
            Some(OsString::from("C.UTF-8"))
        );

        let invalid_path =
            agent_owned_pane_environment(&[entry("PATH", nul_value)], Path::new("/bin/sh"));
        assert_eq!(
            value_for(&invalid_path, "PATH"),
            Some(OsString::from(NATIVE_WORKLOAD_PATH_FALLBACK))
        );
    }

    #[test]
    fn requires_absolute_tmpdir_and_pins_shell_to_the_resolved_launch_shell() {
        let relative = agent_owned_pane_environment(
            &[
                entry("TMPDIR", "relative/tmp"),
                entry("SHELL", "/daemon/evil-shell"),
            ],
            Path::new("/bin/bash"),
        );
        assert!(
            !forwards(&relative, "TMPDIR"),
            "relative TMPDIR must be dropped"
        );
        assert_eq!(
            value_for(&relative, "SHELL"),
            Some(OsString::from("/bin/bash"))
        );

        let absolute = agent_owned_pane_environment(
            &[
                entry("TMPDIR", "/var/tmp"),
                entry("SHELL", "/daemon/evil-shell"),
            ],
            Path::new("/usr/bin/zsh"),
        );
        assert_eq!(
            value_for(&absolute, "TMPDIR"),
            Some(OsString::from("/var/tmp"))
        );
        assert_eq!(
            value_for(&absolute, "SHELL"),
            Some(OsString::from("/usr/bin/zsh"))
        );

        let relative_shell = agent_owned_pane_environment(
            &[entry("SHELL", "/daemon/evil-shell")],
            Path::new("bash"),
        );
        assert!(
            !forwards(&relative_shell, "SHELL"),
            "relative resolved shell must not be pinned"
        );
    }
}
