//! Agent tests for tool discovery behavior.
//!
//! This bounded leaf owns the scenarios for this concern while shared
//! fixtures remain in the parent module.

use super::*;

#[test]
/// Verifies discovery script uses shell command lookup.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn discovery_script_uses_shell_command_lookup() {
    let script = tool_discovery_script();

    assert!(script.contains("command -v"));
    assert!(script.contains("--version"));
    assert!(script.contains("date +%s"));
    assert!(script.contains("tool\\t"));
    assert!(script.contains("python3"));
    assert!(script.contains("rg"));
}

#[test]
/// Verifies environment signature known fields includes all fields.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn environment_signature_known_fields_includes_all_fields() {
    let sig = test_env_signature("myhost", "me", "/bin/bash", "/repo");
    let fields = sig.known_fields();

    assert!(fields.iter().any(|f| f == "os=linux"));
    assert!(fields.iter().any(|f| f == "arch=x86_64"));
    assert!(fields.iter().any(|f| f == "host=myhost"));
    assert!(fields.iter().any(|f| f == "user=me"));
    assert!(fields.iter().any(|f| f == "shell_path=/bin/bash"));
    assert!(fields.iter().any(|f| f == "shell_classification=bash"));
    assert!(fields.iter().any(|f| f == "working_directory=/repo"));
    assert!(fields.iter().any(|f| f == "git_repo=0"));
}

#[test]
/// Verifies model-facing environment context uses a fixed-width signature hash.
///
/// Full host/user/PATH data is useful for internal caches and audit, but it is
/// not task-specific model context. The model projection should stay compact
/// and stable even when the shell environment is large.
fn environment_signature_model_fields_use_hashed_identity() {
    let sig = EnvironmentSignature::new(
        "linux",
        "x86_64",
        Some("6.6.0".to_string()),
        "myhost",
        "me",
        "/bin/bash",
        ShellClassification::Bash,
        Some("GNU bash".to_string()),
        Some("/usr/bin:/bin:/very/long/tool/path".to_string()),
        "/repo",
        Some("/repo".to_string()),
        true,
        None,
        vec!["mise".to_string()],
    )
    .expect("test environment signature should be valid");

    let fields = sig.model_context_fields();
    let joined = fields.join("\n");

    assert!(joined.contains("env_signature=sha256:"));
    assert!(joined.contains("cwd=/repo"));
    assert!(joined.contains("shell=bash"));
    assert!(joined.contains("path_entries=3"));
    assert!(!joined.contains("host=myhost"), "{joined}");
    assert!(!joined.contains("user=me"), "{joined}");
    assert!(!joined.contains("/very/long/tool/path"), "{joined}");
    assert_eq!(sig.stable_hash().len(), 64);
}

#[test]
/// Verifies environment signature rejects empty required fields.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn environment_signature_rejects_empty_required_fields() {
    let error = EnvironmentSignature::new(
        "",
        "x86_64",
        None,
        "host",
        "user",
        "/bin/sh",
        ShellClassification::PosixSh,
        None,
        None,
        "/repo",
        None,
        false,
        None,
        Vec::new(),
    )
    .unwrap_err();

    assert_eq!(error.kind(), AgentShellValidationErrorKind::InvalidArgs);
}

#[test]
/// Verifies tool cache requires bootstrap after signature change.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn tool_cache_requires_bootstrap_after_signature_change() {
    let first = test_env_signature("host", "user", "/bin/sh", "/repo");
    let second = test_env_signature("host", "user", "/bin/sh", "/repo/sub");
    let mut cache = ToolDiscoveryCache::default();

    assert!(cache.requires_bootstrap(&first));
    cache.record(
        first.clone(),
        ToolInventory::parse_bootstrap_output("sed=1\ngrep=1\npython=1\nrg=1\n"),
    );

    assert!(!cache.requires_bootstrap(&first));
    assert!(cache.requires_bootstrap(&second));
}

#[test]
/// Verifies unknown environment signatures always trigger bootstrap.
///
/// The unknown signature is a sentinel used before the runtime captures a real
/// environment identity. Caching that sentinel must not suppress future tool
/// discovery for panes that still report only unknown details.
fn tool_cache_requires_bootstrap_for_unknown_signature_even_if_recorded() {
    let signature = EnvironmentSignature::unknown();
    let mut cache = ToolDiscoveryCache::default();

    assert!(cache.requires_bootstrap(&signature));
    cache.record(
        signature.clone(),
        ToolInventory::parse_bootstrap_output("sed=1\ngrep=1\npython=1\nrg=1\n"),
    );

    assert!(cache.requires_bootstrap(&signature));
    assert!(cache.get(&signature).is_none());
}

#[test]
/// Verifies tool discovery reports shell failures.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn tool_inventory_parses_bootstrap_output() {
    let inventory = ToolInventory::parse_bootstrap_output(
        "tool\tsed\t1\t/usr/bin/sed\tGNU sed 4.9\tcommand -v sed\t0\t/usr/bin/sed --version\t0\t1714500000\n\
         tool\tgrep\t1\t/usr/bin/grep\tGNU grep 3.11\tcommand -v grep\t0\t/usr/bin/grep --version\t0\t1714500000\n\
         tool\tpython\t1\t/usr/bin/python3\tPython 3.12.3\tcommand -v python3 || command -v python\t0\t/usr/bin/python3 --version\t0\t1714500000\n\
         tool\trg\t0\t\t\tcommand -v rg\t1\t\t\t1714500000\n\
         fd=1\n",
    );

    assert!(inventory.sed);
    assert!(inventory.grep);
    assert!(inventory.python);
    assert!(!inventory.rg);
    assert_eq!(inventory.modern_tools, vec!["fd"]);
    let sed = inventory.tools.get("sed").unwrap();
    assert_eq!(sed.path.as_deref(), Some("/usr/bin/sed"));
    assert_eq!(sed.version.as_deref(), Some("GNU sed 4.9"));
    assert_eq!(sed.lookup_command, "command -v sed");
    assert_eq!(sed.lookup_exit_status, Some(0));
    assert_eq!(
        sed.version_command.as_deref(),
        Some("/usr/bin/sed --version")
    );
    assert_eq!(sed.version_exit_status, Some(0));
    assert_eq!(sed.discovered_at_unix_seconds, Some(1714500000));
    let rg = inventory.tools.get("rg").unwrap();
    assert_eq!(rg.lookup_exit_status, Some(1));
    assert_eq!(rg.path, None);
    let fd = inventory.tools.get("fd").unwrap();
    assert!(fd.available);
    assert_eq!(fd.discovered_at_unix_seconds, None);
}
