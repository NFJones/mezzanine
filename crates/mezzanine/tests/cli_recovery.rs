//! Actual-binary coverage of dependency-minimal recovery startup.
//!
//! Fixtures isolate HOME and runtime paths without changing process-global
//! environment. Read-only commands must not migrate, publish configuration,
//! or create runtime directories; normal commands still fail closed.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Owns an isolated binary fixture and removes it even after assertion failure.
struct Fixture(PathBuf);

impl Fixture {
    /// Creates a unique short root compatible with Unix socket path limits.
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "mez-recovery-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }

    /// Runs the Cargo-built product binary with no inherited pane selection.
    fn run(&self, args: &[&str], home: bool) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mez"));
        command
            .args(args)
            .current_dir(&self.0)
            .env_remove("MEZ")
            .env_remove("XDG_RUNTIME_DIR")
            .env("MEZ_TMPDIR", self.0.join("runtime"))
            .stdin(Stdio::null());
        if home {
            command.env("HOME", &self.0);
        } else {
            command.env_remove("HOME");
        }
        command.output().unwrap()
    }

    /// Writes only the requested primary fixture, returning its exact path.
    fn primary(&self, name: &str, text: &str) -> PathBuf {
        let root = self.0.join(".config/mezzanine");
        fs::create_dir_all(&root).unwrap();
        let path = root.join(name);
        fs::write(&path, text).unwrap();
        path
    }

    /// Requires a successful display and no runtime-directory publication.
    fn success(&self, args: &[&str], home: bool) -> String {
        let output = self.run(args, home);
        assert_eq!(output.status.code(), Some(0), "{args:?}: {output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        assert!(!self.0.join("runtime").exists());
        String::from_utf8(output.stdout).unwrap()
    }
}

impl Drop for Fixture {
    /// Cleans up only this test's uniquely owned root.
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Help, versions, defaults, and completion are code-owned display operations.
/// They must work without HOME and must not initialize config or socket state.
#[test]
fn pure_display_without_home() {
    let fixture = Fixture::new();
    for args in [
        vec!["--help"],
        vec!["help", "config"],
        vec!["config", "--help"],
        vec!["--version"],
        vec!["version"],
        vec!["config"],
        vec!["config", "default"],
        vec!["completion", "bash"],
    ] {
        assert!(!fixture.success(&args, false).is_empty());
    }
    assert!(!fixture.0.join(".config").exists());
}

/// Broken or ambiguous primary files must not prevent independent display or
/// explicit-file validation, and none of those operations may rewrite a file.
#[test]
fn recovery_ignores_broken_primary_and_never_migrates() {
    for primary in ["[broken", "version = 999999\n", "version = 1\n"] {
        let fixture = Fixture::new();
        let path = fixture.primary("config.toml", primary);
        let explicit = fixture.0.join("valid.toml");
        let default = fixture.success(&["config", "default"], true);
        fs::write(&explicit, &default).unwrap();
        for ambiguous in [false, true] {
            if ambiguous {
                fixture.primary("config.json", "{}");
            }
            for args in [vec!["--help"], vec!["--version"], vec!["version"]] {
                fixture.success(&args, true);
            }
            let output = fixture.success(
                &["--json", "config", "validate", explicit.to_str().unwrap()],
                true,
            );
            assert_eq!(serde_json::from_str::<serde_json::Value>(&output).unwrap()["valid"], true);
            assert_eq!(fs::read_to_string(&path).unwrap(), primary);
        }
        assert_eq!(fs::read_to_string(explicit).unwrap(), default);
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 2);
    }
}

/// Explicit-file validation requires only that file, not HOME. Invalid config
/// is a successful diagnostic response with valid=false, not a startup error.
#[test]
fn explicit_validation_without_home_preserves_diagnostic_exit_code() {
    let fixture = Fixture::new();
    let path = fixture.0.join("invalid.toml");
    fs::write(&path, "[broken").unwrap();
    let output = fixture.success(&["config", "validate", path.to_str().unwrap()], false);
    assert_eq!(serde_json::from_str::<serde_json::Value>(&output).unwrap()["valid"], false);
    assert_eq!(fs::read_to_string(path).unwrap(), "[broken");
    assert!(!fixture.0.join(".config").exists());
}

/// Selected-primary diagnostics read the original text rather than migrating
/// it. Path selection remains deterministic and rejects ambiguity.
#[test]
fn selected_primary_diagnostics_are_read_only() {
    let fixture = Fixture::new();
    let path = fixture.primary("config.toml", "version = 1\n");
    assert!(fixture.success(&["config", "path"], true).contains(path.to_str().unwrap()));
    let output = fixture.success(&["config", "validate"], true);
    assert!(output.contains("\"valid\":false"));
    assert_eq!(fs::read_to_string(&path).unwrap(), "version = 1\n");
    fixture.primary("config.yaml", "version: 1\n");
    for args in [vec!["config", "path"], vec!["config", "validate"]] {
        let output = fixture.run(&args, true);
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("multiple primary"));
    }
}

/// Runtime and non-recovery management commands must still validate the whole
/// primary config, including CPU and security policy, before doing any work.
#[test]
fn normal_startup_remains_strict() {
    for text in ["version = 103\n[runtime]\ncpu_count = 0\n", "version = 103\n[permissions]\napproval_policy = 'invalid'\n", "[broken", "version = 999999\n"] {
        let fixture = Fixture::new();
        fixture.primary("config.toml", text);
        for args in [vec!["serve"], vec!["host", "status"], vec!["attach"], vec!["config", "get"], vec!["config", "init"]] {
            let output = fixture.run(&args, true);
            assert_eq!(output.status.code(), Some(1), "{args:?}: {output:?}");
            assert!(!output.stderr.is_empty());
            assert!(!fixture.0.join("runtime").exists());
        }
    }
}

/// Grammar errors preserve the existing exit-one contract and are reported
/// before missing HOME or malformed config. Exact internal mode remains first.
#[test]
fn argument_errors_and_internal_mode_precede_config() {
    let fixture = Fixture::new();
    for args in [vec!["config", "validate", "a", "b"], vec!["--unknown"], vec!["-S", "relative", "config", "default"], vec!["-S", "/tmp/a.sock", "-L", "a", "config", "default"], vec!["-L", "../invalid", "config", "default"], vec!["--iroh-profile", "x", "config", "default"]] {
        let output = fixture.run(&args, false);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(!String::from_utf8_lossy(&output.stderr).contains("HOME is not set"));
    }
    let output = fixture.run(&["harness-peer"], false);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("HOME is not set"));
}

/// A missing primary validates built-in defaults without publishing them. Valid
/// current config and all supported explicit formats keep diagnostic success.
#[test]
fn valid_and_missing_primary_diagnostics() {
    let fixture = Fixture::new();
    let default = fixture.success(&["config", "default"], true);
    let output = fixture.success(&["config", "validate"], true);
    assert!(output.contains("\"valid\":true"));
    assert!(fixture.success(&["config", "path"], true).contains("config.toml"));
    assert!(!fixture.0.join(".config").exists());
    let path = fixture.primary("config.toml", &default);
    assert!(fixture.success(&["config", "validate"], true).contains("\"valid\":true"));
    assert_eq!(fs::read_to_string(path).unwrap(), default);
    for (name, text) in [("valid.json", "{\"version\":103}"), ("valid.yaml", "version: 103\n"), ("valid.yml", "version: 103\n")] {
        let path = fixture.0.join(name);
        fs::write(&path, text).unwrap();
        let output = fixture.success(&["config", "validate", path.to_str().unwrap()], false);
        assert!(output.contains("\"valid\":true"), "{output}");
    }
}

/// Recovery does not hide missing-HOME selection or file-read failures. A
/// directory in place of a document deterministically exercises I/O rejection
/// without changing permissions or depending on privileged test execution.
#[test]
fn recovery_selection_and_io_errors_remain_errors() {
    let fixture = Fixture::new();
    for args in [vec!["config", "path"], vec!["config", "validate"]] {
        let output = fixture.run(&args, false);
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("HOME is not set"));
    }
    let unreadable = fixture.0.join("directory.toml");
    fs::create_dir(&unreadable).unwrap();
    for path in [unreadable, fixture.0.join("missing.toml")] {
        let output = fixture.run(&["config", "validate", path.to_str().unwrap()], false);
        assert_eq!(output.status.code(), Some(1));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("HOME is not set"));
    }
}

/// Pure recovery intent cannot depend on inherited malformed pane metadata or
/// unusable runtime paths. Explicit historical/future diagnostics stay read-only.
#[test]
fn recovery_does_not_resolve_runtime_environment_or_rewrite_explicit_files() {
    let fixture = Fixture::new();
    for args in [vec!["version"], vec!["completion", "bash"], vec!["config", "default"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_mez"))
            .args(&args)
            .env_remove("HOME")
            .env("MEZ", "invalid-pane-metadata")
            .env("MEZ_TMPDIR", "relative")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{args:?}: {output:?}");
    }
    for text in ["version = 1\n", "version = 999999\n"] {
        let path = fixture.0.join("explicit.toml");
        fs::write(&path, text).unwrap();
        let output = fixture.success(&["config", "validate", path.to_str().unwrap()], false);
        assert!(output.contains("\"valid\":false"));
        assert_eq!(fs::read_to_string(path).unwrap(), text);
        assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 1);
    }
}
