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
    assert!(script.contains("rg"));
    assert!(!script.contains("python3"));
}

#[test]
/// Verifies Fish tool discovery records the version producer's status rather
/// than the status of the `head` process that truncates its output.
///
/// The fixture places deterministic `sed` and `grep` executables first on
/// `PATH`: both print usable first lines, but only `sed` fails. This ensures
/// output capture and producer success remain independent probe facts.
fn fish_discovery_preserves_version_producer_status() {
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::Instant;

    let temp = test_temp_dir("fish-tool-version-status");
    for (tool, version, status) in [
        ("sed", "failing sed 1.0", 37),
        ("grep", "successful grep 2.0", 0),
    ] {
        let path = temp.join(tool);
        std::fs::write(
            &path,
            format!("#!/bin/sh\nprintf '%s\\n' '{version}' ignored\nexit {status}\n"),
        )
        .expect("the fake version probe should be written");
        let mut permissions = std::fs::metadata(&path)
            .expect("the fake version probe metadata should be readable")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions)
            .expect("the fake version probe should be executable");
    }

    let inherited_path = std::env::var_os("PATH").unwrap_or_default();
    let mut probe_paths = vec![temp.clone()];
    probe_paths.extend(std::env::split_paths(&inherited_path));
    let probe_path =
        std::env::join_paths(&probe_paths).expect("the Fish probe PATH should be valid");
    let mut child = match std::process::Command::new("fish")
        .args(["--no-config", "-c", fish_tool_discovery_script()])
        .env("PATH", probe_path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skipping real-Fish discovery assertion because fish is unavailable");
            std::fs::remove_dir_all(temp).unwrap();
            return;
        }
        Err(error) => panic!("the Fish discovery process should spawn: {error}"),
    };
    let deadline = Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if child
            .try_wait()
            .expect("the Fish discovery process should remain observable")
            .is_some()
        {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the Fish discovery process exceeded its five-second deadline");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let output = child
        .wait_with_output()
        .expect("the Fish discovery output should be collected");
    std::fs::remove_dir_all(temp).unwrap();

    assert!(output.status.success(), "{output:?}");
    let inventory = ToolInventory::parse_bootstrap_output(&String::from_utf8_lossy(&output.stdout));
    let sed = inventory
        .tools
        .get("sed")
        .expect("sed should be discovered");
    let grep = inventory
        .tools
        .get("grep")
        .expect("grep should be discovered");
    assert_eq!(sed.version.as_deref(), Some("failing sed 1.0"));
    assert_eq!(sed.version_exit_status, Some(37));
    assert_eq!(grep.version.as_deref(), Some("successful grep 2.0"));
    assert_eq!(grep.version_exit_status, Some(0));
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
        Some("/home/me".to_string()),
        "/bin/bash",
        ShellClassification::Bash,
        Some("GNU bash".to_string()),
        Some("/usr/bin:/bin:/very/long/tool/path".to_string()),
        "/repo",
        Some("/repo".to_string()),
        true,
        None,
        vec![
            "cargo-bin:/private/home/.cargo/bin".to_string(),
            "rustup:/private/home/.rustup".to_string(),
        ],
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
    assert!(
        joined.contains("environment_managers=cargo-bin,rustup"),
        "{joined}"
    );
    assert!(!joined.contains("/private/home"), "{joined}");
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
        None,
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
        ToolInventory::parse_bootstrap_output("sed=1\ngrep=1\nrg=1\n"),
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
        ToolInventory::parse_bootstrap_output("sed=1\ngrep=1\nrg=1\n"),
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
         tool\trg\t0\t\t\tcommand -v rg\t1\t\t\t1714500000\n\
         fd=1\n",
    );

    assert!(inventory.sed);
    assert!(inventory.grep);
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
