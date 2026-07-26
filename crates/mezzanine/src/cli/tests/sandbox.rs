//! Regression coverage for sandbox workflow commands.
//!
//! These tests protect the status output contract and, critically, the
//! invariant that inspection never migrates configuration, creates runtime
//! directories, writes trust state, prepares managed homes, or probes Bubblewrap.

use super::*;

/// Verifies sandbox status reports a trusted-project Bubblewrap boundary in
/// JSON without creating any inspection-owned state.
#[test]
fn sandbox_status_is_structured_and_strictly_read_only() {
    let (env, home) = test_env("sandbox-status-read-only");
    let config_root = home.join(".config/mezzanine");
    fs::create_dir_all(&config_root).unwrap();
    let config_path = config_root.join("config.toml");
    let config_text = "version = 25\n[permissions]\napproval_policy = \"full-access\"\nsandbox = \"bubblewrap\"\nread_scopes = [\"/tmp\"]\nwrite_scopes = []\n[permissions.bubblewrap]\nexecutable = \"/bin/sh\"\nunavailable = \"fail\"\nnetwork = \"isolated\"\nenvironment = \"minimal\"\n";
    fs::write(&config_path, config_text).unwrap();
    let project = home.join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    let before_config = fs::read(&config_path).unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let exit_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "status".to_string(),
            project.to_string_lossy().into_owned(),
        ]),
        env,
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap();

    assert_eq!(exit_code, 0);
    let output: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(output["configured"]["sandbox"], "bubblewrap");
    assert_eq!(output["effective"]["sandbox"], "bubblewrap");
    assert_eq!(output["effective"]["scope_provenance"], "explicit");
    assert_eq!(
        output["effective"]["bubblewrap_executable_state"],
        "available"
    );
    assert_eq!(output["effective"]["bubblewrap_probe_state"], "not-probed");
    assert_eq!(output["mutations"], serde_json::json!([]));
    assert_eq!(output["confirmation"]["required"], false);
    assert!(stderr.is_empty());
    assert_eq!(fs::read(&config_path).unwrap(), before_config);
    assert!(!config_root.join("project-trust.tsv").exists());
    assert!(!config_root.join("sandbox").exists());
    assert!(!home.join("runtime/mez-0").exists());

    let _ = fs::remove_dir_all(home);
}

/// Toolchain detection reports canonical roots without creating or mutating
/// configuration, trust, managed-home, or runtime state.
#[test]
fn sandbox_toolchain_detection_is_strictly_read_only() {
    let (env, home) = test_env("sandbox-toolchain-detect");
    fs::create_dir_all(home.join(".cargo/bin")).unwrap();
    fs::create_dir_all(home.join(".rustup")).unwrap();
    let project = home.join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let exit_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "toolchains".to_string(),
            "detect".to_string(),
            project.to_string_lossy().into_owned(),
        ]),
        env,
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap();

    assert_eq!(exit_code, 0);
    let output: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(output["kind"], "rust");
    assert_eq!(output["available"], true);
    assert_eq!(output["read_only"], true);
    assert_eq!(output["applied"], false);
    assert_eq!(output["confirmation_required"], false);
    assert!(
        output["cargo_bin"]
            .as_str()
            .unwrap()
            .ends_with("/.cargo/bin")
    );
    assert!(
        output["rustup_home"]
            .as_str()
            .unwrap()
            .ends_with("/.rustup")
    );
    assert!(stderr.is_empty());
    assert!(!home.join(".config/mezzanine").exists());
    assert!(!home.join("runtime/mez-0").exists());

    let _ = fs::remove_dir_all(home);
}

/// Verifies a confirmed CLI toolchain mutation cannot fall back to editing
/// primary configuration when no live runtime and primary client are present.
fn assert_toolchain_enable_requires_live_primary(
    env: CliEnv,
    kind: &str,
    config_path: &std::path::Path,
    stdout: &mut Vec<u8>,
    stderr: &mut Vec<u8>,
) {
    stdout.clear();
    let error = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "toolchains".to_string(),
            "enable".to_string(),
            kind.to_string(),
            "--yes".to_string(),
        ]),
        env,
        false,
        stdout,
        stderr,
    ))
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Io);
    assert!(!config_path.exists());
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
}

/// Rust activation requires explicit confirmation and a live primary client;
/// it never falls back to offline primary-config persistence.
#[test]
fn sandbox_toolchain_enable_requires_confirmation_and_persists_only_kind() {
    let (env, home) = test_env("sandbox-toolchain-enable");
    fs::create_dir_all(home.join(".cargo/bin")).unwrap();
    fs::create_dir_all(home.join(".rustup")).unwrap();
    fs::create_dir_all(home.join("project/.git")).unwrap();
    let config_path = home.join(".config/mezzanine/config.toml");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let preview_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "toolchains".to_string(),
            "enable".to_string(),
            "rust".to_string(),
        ]),
        env.clone(),
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap();
    assert_eq!(preview_code, 1);
    let preview: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(preview["confirmation_required"], true);
    assert!(!config_path.exists());

    assert_toolchain_enable_requires_live_primary(
        env,
        "rust",
        &config_path,
        &mut stdout,
        &mut stderr,
    );

    let _ = fs::remove_dir_all(home);
}

/// Custom definition previews validate and disclose the exact constrained
/// request without writing config or requiring a live service.
#[test]
fn sandbox_custom_toolchain_define_preview_is_typed_and_read_only() {
    let (env, home) = test_env("sandbox-custom-toolchain-preview");
    fs::create_dir_all(home.join("project/.git")).unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let exit_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "toolchains".to_string(),
            "custom".to_string(),
            "define".to_string(),
            "acme".to_string(),
            "--root".to_string(),
            "/opt/acme-sdk".to_string(),
            "--path".to_string(),
            "0:bin".to_string(),
            "--require".to_string(),
            "0:bin/acme".to_string(),
            "--env-root".to_string(),
            "ACME_HOME=0:.".to_string(),
        ]),
        env,
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap();

    assert_eq!(exit_code, 1);
    let preview: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(preview["operation"], "define");
    assert_eq!(preview["selector"], "custom:acme");
    assert_eq!(
        preview["definition"]["roots"],
        serde_json::json!(["/opt/acme-sdk"])
    );
    assert_eq!(preview["definition"]["environment"]["ACME_HOME"], "0:.");
    assert_eq!(preview["confirmation_required"], true);
    assert_eq!(preview["applied"], false);
    assert_eq!(preview["request_digest"].as_str().unwrap().len(), 64);
    assert!(stderr.is_empty());
    assert!(!home.join(".config/mezzanine/config.toml").exists());
    assert!(!home.join("runtime/mez-0").exists());

    let _ = fs::remove_dir_all(home);
}

/// Toolchain list and status load custom identities from effective config
/// without creating trust, managed-home, or runtime state.
#[test]
fn sandbox_toolchain_list_and_status_are_offline_and_read_only() {
    let (env, home) = test_env("sandbox-toolchain-list-status");
    let config_root = home.join(".config/mezzanine");
    fs::create_dir_all(&config_root).unwrap();
    let config_path = config_root.join("config.toml");
    fs::write(
        &config_path,
        "version = 32\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = [\"custom:acme\"]\n[permissions.bubblewrap.custom_toolchains.acme]\ndescription = \"Acme SDK\"\nroots = [\"/opt/acme-sdk\"]\npath_entries = [\"0:bin\"]\nrequired_executables = [\"0:bin/acme\"]\n",
    )
    .unwrap();
    let project = home.join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    let before = fs::read(&config_path).unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let list_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "toolchains".to_string(),
            "list".to_string(),
        ]),
        env.clone(),
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap();
    assert_eq!(list_code, 0);
    let list: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert!(
        list["built_ins"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "rust")
    );
    assert_eq!(list["custom"], serde_json::json!(["custom:acme"]));

    stdout.clear();
    let status_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "toolchains".to_string(),
            "status".to_string(),
            "custom:acme".to_string(),
            project.to_string_lossy().into_owned(),
        ]),
        env,
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap();
    assert_eq!(status_code, 0);
    let status: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(status["configured"], serde_json::json!(["custom:acme"]));
    assert_eq!(status["toolchains"][0]["identity"], "custom:acme");
    assert_eq!(status["toolchains"][0]["configured"], true);
    assert_eq!(status["toolchains"][0]["description"], "Acme SDK");
    assert!(stderr.is_empty());
    assert_eq!(fs::read(&config_path).unwrap(), before);
    assert!(!config_root.join("project-trust.tsv").exists());
    assert!(!home.join("runtime/mez-0").exists());

    let _ = fs::remove_dir_all(home);
}

/// Zig detection and activation use the captured CLI search path, preserve
/// read-only discovery, and persist only the typed kind rather than its root.
#[test]
fn sandbox_zig_toolchain_detects_and_persists_only_kind() {
    let (mut env, home) = test_env("sandbox-zig-toolchain");
    let zig_root = home.join("zig-0.14.0");
    fs::create_dir_all(zig_root.join("lib")).unwrap();
    fs::write(zig_root.join("zig"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(zig_root.join("zig"), fs::Permissions::from_mode(0o755)).unwrap();
    let zig_root = zig_root.canonicalize().unwrap();
    env.path = Some(zig_root.clone().into_os_string());
    let project = home.join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    let config_path = home.join(".config/mezzanine/config.toml");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let detect_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "toolchains".to_string(),
            "detect".to_string(),
            "--kind".to_string(),
            "zig".to_string(),
            project.to_string_lossy().into_owned(),
        ]),
        env.clone(),
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap();
    assert_eq!(detect_code, 0);
    let detected: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(detected["kind"], "zig");
    assert_eq!(detected["available"], true);
    assert_eq!(detected["zig_root"], zig_root.to_string_lossy().as_ref());
    assert!(!config_path.exists());

    assert_toolchain_enable_requires_live_primary(
        env,
        "zig",
        &config_path,
        &mut stdout,
        &mut stderr,
    );

    let _ = fs::remove_dir_all(home);
}

/// Go detection and activation use the captured CLI search path, keep host
/// caches and user tool bins out of the result, and persist only the typed kind.
#[test]
fn sandbox_go_toolchain_detects_and_persists_only_kind() {
    let (mut env, home) = test_env("sandbox-go-toolchain");
    let go_root = home.join("go-sdk");
    fs::create_dir_all(go_root.join("bin")).unwrap();
    fs::create_dir_all(go_root.join("src")).unwrap();
    fs::write(go_root.join("bin/go"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(go_root.join("bin/go"), fs::Permissions::from_mode(0o755)).unwrap();
    let go_root = go_root.canonicalize().unwrap();
    env.path = Some(go_root.join("bin").into_os_string());
    let project = home.join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    let config_path = home.join(".config/mezzanine/config.toml");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let detect_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "toolchains".to_string(),
            "detect".to_string(),
            "--kind".to_string(),
            "go".to_string(),
            project.to_string_lossy().into_owned(),
        ]),
        env.clone(),
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap();
    assert_eq!(detect_code, 0);
    let detected: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(detected["kind"], "go");
    assert_eq!(detected["available"], true);
    assert_eq!(detected["go_root"], go_root.to_string_lossy().as_ref());
    assert!(!config_path.exists());

    assert_toolchain_enable_requires_live_primary(
        env,
        "go",
        &config_path,
        &mut stdout,
        &mut stderr,
    );

    let _ = fs::remove_dir_all(home);
}

/// Deno detection and activation use the captured CLI search path, keep host
/// cache and authentication state out of the result, and persist only its kind.
#[test]
fn sandbox_deno_toolchain_detects_and_persists_only_kind() {
    let (mut env, home) = test_env("sandbox-deno-toolchain");
    let deno_root = home.join("deno-runtime");
    fs::create_dir_all(&deno_root).unwrap();
    fs::write(deno_root.join("deno"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(deno_root.join("deno"), fs::Permissions::from_mode(0o755)).unwrap();
    let deno_root = deno_root.canonicalize().unwrap();
    env.path = Some(deno_root.clone().into_os_string());
    let project = home.join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    let config_path = home.join(".config/mezzanine/config.toml");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let detect_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "toolchains".to_string(),
            "detect".to_string(),
            "--kind".to_string(),
            "deno".to_string(),
            project.to_string_lossy().into_owned(),
        ]),
        env.clone(),
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap();
    assert_eq!(detect_code, 0);
    let detected: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(detected["kind"], "deno");
    assert_eq!(detected["available"], true);
    assert_eq!(detected["deno_root"], deno_root.to_string_lossy().as_ref());
    assert!(!config_path.exists());

    assert_toolchain_enable_requires_live_primary(
        env,
        "deno",
        &config_path,
        &mut stdout,
        &mut stderr,
    );

    let _ = fs::remove_dir_all(home);
}

/// Bun detection and activation use the captured CLI search path, keep host
/// package and credential state out of the result, and persist only its kind.
#[test]
fn sandbox_bun_toolchain_detects_and_persists_only_kind() {
    let (mut env, home) = test_env("sandbox-bun-toolchain");
    let bun_root = home.join("bun-runtime");
    fs::create_dir_all(bun_root.join("bin")).unwrap();
    fs::write(bun_root.join("bin/bun"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(bun_root.join("bin/bun"), fs::Permissions::from_mode(0o755)).unwrap();
    let bun_root = bun_root.canonicalize().unwrap();
    env.path = Some(bun_root.join("bin").into_os_string());
    let project = home.join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    let config_path = home.join(".config/mezzanine/config.toml");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let detect_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "toolchains".to_string(),
            "detect".to_string(),
            "--kind".to_string(),
            "bun".to_string(),
            project.to_string_lossy().into_owned(),
        ]),
        env.clone(),
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap();
    assert_eq!(detect_code, 0);
    let detected: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(detected["kind"], "bun");
    assert_eq!(detected["available"], true);
    assert_eq!(detected["bun_root"], bun_root.to_string_lossy().as_ref());
    assert!(!config_path.exists());

    assert_toolchain_enable_requires_live_primary(
        env,
        "bun",
        &config_path,
        &mut stdout,
        &mut stderr,
    );

    let _ = fs::remove_dir_all(home);
}

/// Node.js detection and activation use the captured CLI search path, keep
/// host package credentials and global tools hidden, and persist only its kind.
#[test]
fn sandbox_node_toolchain_detects_and_persists_only_kind() {
    let (mut env, home) = test_env("sandbox-node-toolchain");
    let node_root = home.join("node-runtime");
    fs::create_dir_all(node_root.join("bin")).unwrap();
    fs::create_dir_all(node_root.join("lib")).unwrap();
    fs::write(node_root.join("bin/node"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(
        node_root.join("bin/node"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let node_root = node_root.canonicalize().unwrap();
    env.path = Some(node_root.join("bin").into_os_string());
    let project = home.join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    let config_path = home.join(".config/mezzanine/config.toml");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let detect_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "toolchains".to_string(),
            "detect".to_string(),
            "--kind".to_string(),
            "node".to_string(),
            project.to_string_lossy().into_owned(),
        ]),
        env.clone(),
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap();
    assert_eq!(detect_code, 0);
    let detected: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(detected["kind"], "node");
    assert_eq!(detected["available"], true);
    assert_eq!(detected["node_root"], node_root.to_string_lossy().as_ref());
    assert!(!config_path.exists());

    assert_toolchain_enable_requires_live_primary(
        env,
        "node",
        &config_path,
        &mut stdout,
        &mut stderr,
    );

    let _ = fs::remove_dir_all(home);
}

/// Python detection and activation use only the captured CLI search path,
/// report the canonical base runtime, and persist no host path or package state.
#[test]
fn sandbox_python_toolchain_detects_and_persists_only_kind() {
    let (mut env, home) = test_env("sandbox-python-toolchain");
    let python_root = home.join("python-runtime");
    fs::create_dir_all(python_root.join("bin")).unwrap();
    fs::create_dir_all(python_root.join("lib")).unwrap();
    fs::write(python_root.join("bin/python3"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(
        python_root.join("bin/python3"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let python_root = python_root.canonicalize().unwrap();
    env.path = Some(python_root.join("bin").into_os_string());
    let project = home.join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    let config_path = home.join(".config/mezzanine/config.toml");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let detect_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "toolchains".to_string(),
            "detect".to_string(),
            "--kind".to_string(),
            "python".to_string(),
            project.to_string_lossy().into_owned(),
        ]),
        env.clone(),
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap();
    assert_eq!(detect_code, 0);
    let detected: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(detected["kind"], "python");
    assert_eq!(detected["available"], true);
    assert_eq!(
        detected["python_root"],
        python_root.to_string_lossy().as_ref()
    );
    assert!(!config_path.exists());

    assert_toolchain_enable_requires_live_primary(
        env,
        "python",
        &config_path,
        &mut stdout,
        &mut stderr,
    );

    let _ = fs::remove_dir_all(home);
}

/// JDK detection and activation use only the captured CLI search path, require
/// a complete SDK, and persist no host path, manager home, or build-tool state.
#[test]
fn sandbox_jdk_toolchain_detects_and_persists_only_kind() {
    let (mut env, home) = test_env("sandbox-jdk-toolchain");
    let jdk_root = home.join("jdk-runtime");
    fs::create_dir_all(jdk_root.join("bin")).unwrap();
    fs::create_dir_all(jdk_root.join("lib")).unwrap();
    for executable in ["java", "javac", "jar"] {
        let path = jdk_root.join("bin").join(executable);
        fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let jdk_root = jdk_root.canonicalize().unwrap();
    env.path = Some(jdk_root.join("bin").into_os_string());
    let project = home.join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    let config_path = home.join(".config/mezzanine/config.toml");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let detect_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "toolchains".to_string(),
            "detect".to_string(),
            "--kind".to_string(),
            "jdk".to_string(),
            project.to_string_lossy().into_owned(),
        ]),
        env.clone(),
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap();
    assert_eq!(detect_code, 0);
    let detected: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(detected["kind"], "jdk");
    assert_eq!(detected["available"], true);
    assert_eq!(detected["jdk_root"], jdk_root.to_string_lossy().as_ref());
    assert!(!config_path.exists());

    assert_toolchain_enable_requires_live_primary(
        env,
        "jdk",
        &config_path,
        &mut stdout,
        &mut stderr,
    );

    let _ = fs::remove_dir_all(home);
}

/// .NET SDK detection uses only the captured CLI search path, requires a
/// complete installation, and persists no host path, NuGet state, or tools.
#[test]
fn sandbox_dotnet_toolchain_detects_and_persists_only_kind() {
    let (mut env, home) = test_env("sandbox-dotnet-toolchain");
    let dotnet_root = home.join("dotnet-sdk");
    for directory in ["sdk", "shared", "packs"] {
        fs::create_dir_all(dotnet_root.join(directory)).unwrap();
    }
    fs::write(dotnet_root.join("dotnet"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(
        dotnet_root.join("dotnet"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let dotnet_root = dotnet_root.canonicalize().unwrap();
    env.path = Some(dotnet_root.clone().into_os_string());
    let project = home.join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    let config_path = home.join(".config/mezzanine/config.toml");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let detect_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "toolchains".to_string(),
            "detect".to_string(),
            "--kind".to_string(),
            "dotnet".to_string(),
            project.to_string_lossy().into_owned(),
        ]),
        env.clone(),
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap();
    assert_eq!(detect_code, 0);
    let detected: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(detected["kind"], "dotnet");
    assert_eq!(detected["available"], true);
    assert_eq!(
        detected["dotnet_root"],
        dotnet_root.to_string_lossy().as_ref()
    );
    assert!(!config_path.exists());

    assert_toolchain_enable_requires_live_primary(
        env,
        "dotnet",
        &config_path,
        &mut stdout,
        &mut stderr,
    );

    let _ = fs::remove_dir_all(home);
}

/// Dart SDK detection uses only the captured CLI search path, requires a
/// complete standalone SDK, and persists no host path, Pub state, or tools.
#[test]
fn sandbox_dart_toolchain_detects_and_persists_only_kind() {
    let (mut env, home) = test_env("sandbox-dart-toolchain");
    let dart_root = home.join("dart-sdk");
    fs::create_dir_all(dart_root.join("bin")).unwrap();
    fs::create_dir_all(dart_root.join("lib")).unwrap();
    fs::write(dart_root.join("bin/dart"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(
        dart_root.join("bin/dart"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let dart_root = dart_root.canonicalize().unwrap();
    env.path = Some(dart_root.join("bin").into_os_string());
    let project = home.join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    let config_path = home.join(".config/mezzanine/config.toml");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let detect_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "toolchains".to_string(),
            "detect".to_string(),
            "--kind".to_string(),
            "dart".to_string(),
            project.to_string_lossy().into_owned(),
        ]),
        env.clone(),
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap();
    assert_eq!(detect_code, 0);
    let detected: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(detected["kind"], "dart");
    assert_eq!(detected["available"], true);
    assert_eq!(detected["dart_root"], dart_root.to_string_lossy().as_ref());
    assert!(!config_path.exists());

    assert_toolchain_enable_requires_live_primary(
        env,
        "dart",
        &config_path,
        &mut stdout,
        &mut stderr,
    );

    let _ = fs::remove_dir_all(home);
}

/// Kotlin/JVM compiler detection uses only the captured CLI search path,
/// validates a standalone distribution, and never persists its host root.
#[test]
fn sandbox_kotlin_toolchain_detects_and_persists_only_kind() {
    let (mut env, home) = test_env("sandbox-kotlin-toolchain");
    let kotlin_root = home.join("kotlin-compiler");
    fs::create_dir_all(kotlin_root.join("bin")).unwrap();
    fs::create_dir_all(kotlin_root.join("lib")).unwrap();
    for executable in ["kotlinc", "kotlin"] {
        let executable_path = kotlin_root.join("bin").join(executable);
        fs::write(&executable_path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(executable_path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let kotlin_root = kotlin_root.canonicalize().unwrap();
    env.path = Some(kotlin_root.join("bin").into_os_string());
    let project = home.join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    let config_path = home.join(".config/mezzanine/config.toml");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let detect_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "toolchains".to_string(),
            "detect".to_string(),
            "--kind".to_string(),
            "kotlin".to_string(),
            project.to_string_lossy().into_owned(),
        ]),
        env.clone(),
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap();
    assert_eq!(detect_code, 0);
    let detected: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(detected["kind"], "kotlin");
    assert_eq!(detected["available"], true);
    assert_eq!(
        detected["kotlin_root"],
        kotlin_root.to_string_lossy().as_ref()
    );
    assert!(!config_path.exists());

    assert_toolchain_enable_requires_live_primary(
        env,
        "kotlin",
        &config_path,
        &mut stdout,
        &mut stderr,
    );

    let _ = fs::remove_dir_all(home);
}

/// Ruby runtime detection uses only the captured CLI search path, validates
/// matching package executables, and never persists host roots or gem state.
#[test]
fn sandbox_ruby_toolchain_detects_and_persists_only_kind() {
    let (mut env, home) = test_env("sandbox-ruby-toolchain");
    let ruby_root = home.join("ruby-runtime");
    fs::create_dir_all(ruby_root.join("bin")).unwrap();
    fs::create_dir_all(ruby_root.join("lib/ruby")).unwrap();
    for executable in ["ruby", "gem", "bundle"] {
        let executable_path = ruby_root.join("bin").join(executable);
        fs::write(&executable_path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(executable_path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let ruby_root = ruby_root.canonicalize().unwrap();
    env.path = Some(ruby_root.join("bin").into_os_string());
    let project = home.join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    let config_path = home.join(".config/mezzanine/config.toml");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let detect_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "toolchains".to_string(),
            "detect".to_string(),
            "--kind".to_string(),
            "ruby".to_string(),
            project.to_string_lossy().into_owned(),
        ]),
        env.clone(),
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap();
    assert_eq!(detect_code, 0);
    let detected: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(detected["kind"], "ruby");
    assert_eq!(detected["available"], true);
    assert_eq!(detected["ruby_root"], ruby_root.to_string_lossy().as_ref());
    assert!(!config_path.exists());

    assert_toolchain_enable_requires_live_primary(
        env,
        "ruby",
        &config_path,
        &mut stdout,
        &mut stderr,
    );

    let _ = fs::remove_dir_all(home);
}

/// PHP and Composer detection use only captured CLI search-path entries,
/// report canonical roots, and never persist host paths or package state.
#[test]
fn sandbox_php_and_composer_toolchains_detect_without_persisting_roots() {
    let (mut env, home) = test_env("sandbox-php-composer-toolchain");
    let php_root = home.join("php-runtime");
    fs::create_dir_all(php_root.join("bin")).unwrap();
    fs::create_dir_all(php_root.join("lib/php")).unwrap();
    fs::write(php_root.join("bin/php"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(php_root.join("bin/php"), fs::Permissions::from_mode(0o755)).unwrap();
    let composer_root = home.join("composer-runtime");
    fs::create_dir_all(composer_root.join("bin")).unwrap();
    fs::write(composer_root.join("bin/composer"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(
        composer_root.join("bin/composer"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let php_root = php_root.canonicalize().unwrap();
    let composer_root = composer_root.canonicalize().unwrap();
    env.path =
        Some(std::env::join_paths([php_root.join("bin"), composer_root.join("bin")]).unwrap());
    let project = home.join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    let config_path = home.join(".config/mezzanine/config.toml");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    for (kind, field, root) in [
        ("php", "php_root", &php_root),
        ("composer", "composer_root", &composer_root),
    ] {
        stdout.clear();
        stderr.clear();
        let detect_code = block_on_cli_code(crate::cli::run_with(
            with_json_output(vec![
                "mez".to_string(),
                "sandbox".to_string(),
                "toolchains".to_string(),
                "detect".to_string(),
                "--kind".to_string(),
                kind.to_string(),
                project.to_string_lossy().into_owned(),
            ]),
            env.clone(),
            false,
            &mut stdout,
            &mut stderr,
        ))
        .unwrap();
        assert_eq!(detect_code, 0);
        let detected: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(detected["kind"], kind);
        assert_eq!(detected["available"], true);
        assert_eq!(detected[field], root.to_string_lossy().as_ref());
        assert!(stderr.is_empty());
        assert!(!config_path.exists());
    }

    assert_toolchain_enable_requires_live_primary(
        env,
        "composer",
        &config_path,
        &mut stdout,
        &mut stderr,
    );

    let _ = fs::remove_dir_all(home);
}

/// Erlang and Elixir detection uses only captured CLI search-path entries,
/// reports canonical roots, and never persists host paths or package state.
#[test]
fn sandbox_erlang_and_elixir_toolchains_detect_without_persisting_roots() {
    let (mut env, home) = test_env("sandbox-erlang-elixir-toolchain");
    let erlang_root = home.join("erlang-runtime");
    fs::create_dir_all(erlang_root.join("bin")).unwrap();
    fs::create_dir_all(erlang_root.join("lib/erlang")).unwrap();
    for executable in ["erl", "erlc", "escript"] {
        let path = erlang_root.join("bin").join(executable);
        fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let elixir_root = home.join("elixir-runtime");
    fs::create_dir_all(elixir_root.join("bin")).unwrap();
    fs::create_dir_all(elixir_root.join("lib/elixir")).unwrap();
    for executable in ["elixir", "elixirc", "mix"] {
        let path = elixir_root.join("bin").join(executable);
        fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let erlang_root = erlang_root.canonicalize().unwrap();
    let elixir_root = elixir_root.canonicalize().unwrap();
    env.path =
        Some(std::env::join_paths([erlang_root.join("bin"), elixir_root.join("bin")]).unwrap());
    let project = home.join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    let config_path = home.join(".config/mezzanine/config.toml");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    for (kind, field, root) in [
        ("erlang", "erlang_root", &erlang_root),
        ("elixir", "elixir_root", &elixir_root),
    ] {
        stdout.clear();
        stderr.clear();
        let detect_code = block_on_cli_code(crate::cli::run_with(
            with_json_output(vec![
                "mez".to_string(),
                "sandbox".to_string(),
                "toolchains".to_string(),
                "detect".to_string(),
                "--kind".to_string(),
                kind.to_string(),
                project.to_string_lossy().into_owned(),
            ]),
            env.clone(),
            false,
            &mut stdout,
            &mut stderr,
        ))
        .unwrap();
        assert_eq!(detect_code, 0);
        let detected: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(detected["kind"], kind);
        assert_eq!(detected["available"], true);
        assert_eq!(detected[field], root.to_string_lossy().as_ref());
        assert!(stderr.is_empty());
        assert!(!config_path.exists());
    }

    assert_toolchain_enable_requires_live_primary(
        env,
        "elixir",
        &config_path,
        &mut stdout,
        &mut stderr,
    );

    let _ = fs::remove_dir_all(home);
}

/// GHC, Cabal, and Stack detection uses only captured CLI search-path entries,
/// reports canonical roots, and never persists host paths or package state.
#[test]
fn sandbox_haskell_toolchains_detect_without_persisting_roots() {
    let (mut env, home) = test_env("sandbox-haskell-toolchains");
    let ghc_root = home.join("ghc-compiler");
    fs::create_dir_all(ghc_root.join("bin")).unwrap();
    fs::create_dir_all(ghc_root.join("lib/ghc")).unwrap();
    for executable in ["ghc", "ghci", "runghc", "ghc-pkg"] {
        let path = ghc_root.join("bin").join(executable);
        fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let cabal_root = home.join("cabal-companion");
    fs::create_dir_all(cabal_root.join("bin")).unwrap();
    fs::write(cabal_root.join("bin/cabal"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(
        cabal_root.join("bin/cabal"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let stack_root = home.join("stack-companion");
    fs::create_dir_all(stack_root.join("bin")).unwrap();
    fs::write(stack_root.join("bin/stack"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(
        stack_root.join("bin/stack"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let ghc_root = ghc_root.canonicalize().unwrap();
    let cabal_root = cabal_root.canonicalize().unwrap();
    let stack_root = stack_root.canonicalize().unwrap();
    env.path = Some(
        std::env::join_paths([
            ghc_root.join("bin"),
            cabal_root.join("bin"),
            stack_root.join("bin"),
        ])
        .unwrap(),
    );
    let project = home.join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    let config_path = home.join(".config/mezzanine/config.toml");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    for (kind, field, root) in [
        ("ghc", "ghc_root", &ghc_root),
        ("cabal", "cabal_root", &cabal_root),
        ("stack", "stack_root", &stack_root),
    ] {
        stdout.clear();
        stderr.clear();
        let detect_code = block_on_cli_code(crate::cli::run_with(
            with_json_output(vec![
                "mez".to_string(),
                "sandbox".to_string(),
                "toolchains".to_string(),
                "detect".to_string(),
                "--kind".to_string(),
                kind.to_string(),
                project.to_string_lossy().into_owned(),
            ]),
            env.clone(),
            false,
            &mut stdout,
            &mut stderr,
        ))
        .unwrap();
        assert_eq!(detect_code, 0);
        let detected: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(detected["kind"], kind);
        assert_eq!(detected["available"], true);
        assert_eq!(detected[field], root.to_string_lossy().as_ref());
        assert!(stderr.is_empty());
        assert!(!config_path.exists());
    }

    assert_toolchain_enable_requires_live_primary(
        env,
        "stack",
        &config_path,
        &mut stdout,
        &mut stderr,
    );

    let _ = fs::remove_dir_all(home);
}

/// OCaml detection accepts only the canonical project-local `_opam` switch,
/// reports that switch without consulting PATH, and never persists its host
/// path or creates configuration during read-only inspection.
#[test]
fn sandbox_ocaml_toolchain_detects_only_project_local_switch() {
    let (env, home) = test_env("sandbox-ocaml-toolchain");
    let project = home.join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    let environment = project.join("_opam");
    for directory in ["bin", "lib", "share"] {
        fs::create_dir_all(environment.join(directory)).unwrap();
    }
    for executable in ["ocaml", "ocamlc", "ocamlopt", "dune"] {
        let path = environment.join("bin").join(executable);
        fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let project = project.canonicalize().unwrap();
    let environment = environment.canonicalize().unwrap();
    let config_path = home.join(".config/mezzanine/config.toml");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let detect_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "toolchains".to_string(),
            "detect".to_string(),
            "--kind".to_string(),
            "ocaml".to_string(),
            project.to_string_lossy().into_owned(),
        ]),
        env.clone(),
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap();

    assert_eq!(detect_code, 0);
    let detected: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(detected["kind"], "ocaml");
    assert_eq!(detected["available"], true);
    assert_eq!(
        detected["ocaml_root"],
        environment.to_string_lossy().as_ref()
    );
    assert_eq!(
        detected["sandbox_path"],
        environment.to_string_lossy().as_ref()
    );
    assert!(stderr.is_empty());
    assert!(!config_path.exists());

    let _ = fs::remove_dir_all(home);
}

/// Native compiler and build-tool detection accepts only complete standalone
/// roots from the captured CLI search path, reports canonical roots, and does
/// not persist host paths or create configuration during read-only inspection.
#[test]
fn sandbox_native_toolchains_detect_without_persisting_roots() {
    let (mut env, home) = test_env("sandbox-native-toolchains");
    let specifications = [
        (
            "llvm",
            ["clang", "clang++", "llvm-ar", "llvm-config"].as_slice(),
            ["lib/clang"].as_slice(),
        ),
        (
            "gcc",
            ["gcc", "g++", "gcc-ar"].as_slice(),
            ["lib/gcc"].as_slice(),
        ),
        (
            "cmake",
            ["cmake", "ctest"].as_slice(),
            ["share/cmake"].as_slice(),
        ),
        ("ninja", ["ninja"].as_slice(), [].as_slice()),
        ("meson", ["meson"].as_slice(), [].as_slice()),
    ];
    let mut roots = Vec::new();
    for (kind, executables, directories) in specifications {
        let root = home.join(kind);
        fs::create_dir_all(root.join("bin")).unwrap();
        for directory in directories {
            fs::create_dir_all(root.join(directory)).unwrap();
        }
        for executable in executables {
            let path = root.join("bin").join(executable);
            fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        roots.push((kind, root.canonicalize().unwrap()));
    }
    env.path = Some(std::env::join_paths(roots.iter().map(|(_, root)| root.join("bin"))).unwrap());
    let project = home.join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    let config_path = home.join(".config/mezzanine/config.toml");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    for (kind, root) in &roots {
        stdout.clear();
        stderr.clear();
        let detect_code = block_on_cli_code(crate::cli::run_with(
            with_json_output(vec![
                "mez".to_string(),
                "sandbox".to_string(),
                "toolchains".to_string(),
                "detect".to_string(),
                "--kind".to_string(),
                (*kind).to_string(),
                project.to_string_lossy().into_owned(),
            ]),
            env.clone(),
            false,
            &mut stdout,
            &mut stderr,
        ))
        .unwrap();
        assert_eq!(detect_code, 0);
        let detected: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(detected["kind"], *kind);
        assert_eq!(detected["available"], true);
        assert_eq!(
            detected[format!("{kind}_root")],
            root.to_string_lossy().as_ref()
        );
        assert!(stderr.is_empty());
        assert!(!config_path.exists());
    }

    let _ = fs::remove_dir_all(home);
}

/// Swift detection accepts a complete standalone Linux distribution from the
/// captured CLI search path, reports its canonical root, and remains read-only.
#[test]
fn sandbox_swift_toolchain_detects_without_persisting_root() {
    let (mut env, home) = test_env("sandbox-swift-toolchain");
    let root = home.join("swift");
    fs::create_dir_all(root.join("bin")).unwrap();
    fs::create_dir_all(root.join("lib/swift/linux")).unwrap();
    for executable in ["swift", "swiftc", "swift-package", "sourcekit-lsp"] {
        let path = root.join("bin").join(executable);
        fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let root = root.canonicalize().unwrap();
    env.path = Some(std::env::join_paths([root.join("bin")]).unwrap());
    let project = home.join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    let config_path = home.join(".config/mezzanine/config.toml");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let detect_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "toolchains".to_string(),
            "detect".to_string(),
            "--kind".to_string(),
            "swift".to_string(),
            project.to_string_lossy().into_owned(),
        ]),
        env,
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap();

    assert_eq!(detect_code, 0);
    let detected: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(detected["kind"], "swift");
    assert_eq!(detected["available"], true);
    assert_eq!(detected["swift_root"], root.to_string_lossy().as_ref());
    assert_eq!(
        detected["sandbox_path"],
        "/opt/mez/toolchains/swift/root/bin:/usr/bin:/bin"
    );
    assert!(stderr.is_empty());
    assert!(!config_path.exists());

    let _ = fs::remove_dir_all(home);
}

/// Maven and Gradle detection prefers complete repository wrappers over host
/// distributions, reports the canonical project root, and remains read-only.
#[test]
fn sandbox_jvm_build_tool_wrappers_detect_without_persisting_roots() {
    let (env, home) = test_env("sandbox-jvm-build-tool-wrappers");
    let project = home.join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    fs::create_dir_all(project.join(".mvn/wrapper")).unwrap();
    fs::create_dir_all(project.join("gradle/wrapper")).unwrap();
    for wrapper in ["mvnw", "gradlew"] {
        let path = project.join(wrapper);
        fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::write(
        project.join(".mvn/wrapper/maven-wrapper.properties"),
        "distributionUrl=https://repo.maven.apache.org/maven.zip\n",
    )
    .unwrap();
    fs::write(
        project.join("gradle/wrapper/gradle-wrapper.properties"),
        "distributionUrl=https\\://services.gradle.org/distributions/gradle.zip\n",
    )
    .unwrap();
    let project = project.canonicalize().unwrap();
    let config_path = home.join(".config/mezzanine/config.toml");

    for kind in ["maven", "gradle"] {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let detect_code = block_on_cli_code(crate::cli::run_with(
            with_json_output(vec![
                "mez".to_string(),
                "sandbox".to_string(),
                "toolchains".to_string(),
                "detect".to_string(),
                "--kind".to_string(),
                kind.to_string(),
                project.to_string_lossy().into_owned(),
            ]),
            env.clone(),
            false,
            &mut stdout,
            &mut stderr,
        ))
        .unwrap();

        assert_eq!(detect_code, 0);
        let detected: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(detected["kind"], kind);
        assert_eq!(detected["available"], true);
        assert_eq!(
            detected[format!("{kind}_root")],
            project.to_string_lossy().as_ref()
        );
        assert_eq!(
            detected["sandbox_path"],
            "/opt/mez/toolchains/jdk/root/bin:/usr/bin:/bin"
        );
        assert!(stderr.is_empty());
        assert!(!config_path.exists());
    }

    let _ = fs::remove_dir_all(home);
}

/// Guided setup planning is strictly read-only and reports the complete
/// code-owned preset mutation set without creating config or trust state.
#[test]
fn sandbox_setup_plan_is_read_only_and_requires_explicit_authority() {
    let (env, home) = test_env("sandbox-setup-plan");
    let project = home.join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let exit_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "plan".to_string(),
            "--preset".to_string(),
            "project-safe".to_string(),
            "--authority".to_string(),
            "explicit-scope".to_string(),
            "--path".to_string(),
            project.to_string_lossy().into_owned(),
        ]),
        env,
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap();

    assert_eq!(exit_code, 0);
    let output: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(output["preset"], "project-safe");
    assert_eq!(output["authority"], "explicit-scope");
    assert_eq!(output["dry_run"], true);
    assert_eq!(output["applied"], false);
    assert_eq!(output["trust_current_project"], false);
    assert!(!home.join(".config/mezzanine").exists());
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Noninteractive setup previews without confirmation, then atomically
/// persists the selected explicit-scope preset when `--yes` is supplied.
#[test]
fn sandbox_setup_enable_requires_confirmation_and_persists_preset() {
    let (env, home) = test_env("sandbox-setup-enable");
    let project = home.join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    let config_path = home.join(".config/mezzanine/config.toml");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let base = vec![
        "mez".to_string(),
        "sandbox".to_string(),
        "enable".to_string(),
        "--preset".to_string(),
        "project-auto".to_string(),
        "--authority".to_string(),
        "explicit-scope".to_string(),
        "--path".to_string(),
        project.to_string_lossy().into_owned(),
    ];

    let preview_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(base.clone()),
        env.clone(),
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap();
    assert_eq!(preview_code, 1);
    assert!(!config_path.exists());

    stdout.clear();
    let mut confirmed = base;
    confirmed.push("--yes".to_string());
    let applied_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(confirmed),
        env,
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap();
    assert_eq!(applied_code, 0);
    let config = fs::read_to_string(&config_path).unwrap();
    assert!(config.contains("sandbox = \"bubblewrap\""), "{config}");
    assert!(
        config.contains("approval_policy = \"auto-allow\""),
        "{config}"
    );
    assert!(
        config.contains(
            &project
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        )
    );
    assert!(!home.join(".config/mezzanine/project-trust.tsv").exists());
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Trusted-project setup persists the independently discovered project trust
/// record while leaving explicit scope arrays omitted from user configuration.
#[test]
fn sandbox_setup_trusted_project_persists_trust_without_explicit_scopes() {
    let (env, home) = test_env("sandbox-setup-trusted-project");
    let project = home.join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let exit_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "enable".to_string(),
            "--preset".to_string(),
            "project-safe".to_string(),
            "--authority".to_string(),
            "trusted-project".to_string(),
            "--path".to_string(),
            project.to_string_lossy().into_owned(),
            "--yes".to_string(),
        ]),
        env,
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap();

    assert_eq!(exit_code, 0);
    let config = fs::read_to_string(home.join(".config/mezzanine/config.toml")).unwrap();
    assert!(config.contains("sandbox = \"bubblewrap\""), "{config}");
    assert!(!config.contains("read_scopes"), "{config}");
    assert!(!config.contains("write_scopes"), "{config}");
    let trust =
        ProjectTrustStore::load_from_file(&home.join(".config/mezzanine/project-trust.tsv"))
            .unwrap();
    assert_eq!(
        trust.get(&project).map(|record| record.state),
        Some(TrustDecision::Trusted)
    );
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// The read-only preset grants only project read authority, while disabling
/// Bubblewrap later changes only the backend and retains scopes and policy.
#[test]
fn sandbox_setup_read_only_and_disable_retain_expected_policy() {
    let (env, home) = test_env("sandbox-setup-read-only-disable");
    let project = home.join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let apply_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "preset".to_string(),
            "apply".to_string(),
            "--preset".to_string(),
            "project-read-only".to_string(),
            "--authority".to_string(),
            "explicit-scope".to_string(),
            "--path".to_string(),
            project.to_string_lossy().into_owned(),
            "--yes".to_string(),
        ]),
        env.clone(),
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap();
    assert_eq!(apply_code, 0);

    stdout.clear();
    let disable_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "disable".to_string(),
            "--yes".to_string(),
        ]),
        env,
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap();
    assert_eq!(disable_code, 0);
    let config = fs::read_to_string(home.join(".config/mezzanine/config.toml")).unwrap();
    assert!(config.contains("sandbox = \"policy-only\""), "{config}");
    assert!(config.contains("approval_policy = \"ask\""), "{config}");
    assert!(config.contains("read_scopes = ["), "{config}");
    assert!(config.contains("write_scopes = []"), "{config}");
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// A trust-store write failure after config persistence restores the original
/// config document instead of leaving a partially applied trusted preset.
#[test]
fn sandbox_setup_rolls_back_config_when_trust_persistence_fails() {
    let (env, home) = test_env("sandbox-setup-trust-rollback");
    let project = home.join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    let config_root = home.join(".config/mezzanine");
    fs::create_dir_all(&config_root).unwrap();
    let config_path = config_root.join("config.toml");
    let original =
        "version = 25\n[permissions]\nsandbox = \"policy-only\"\napproval_policy = \"ask\"\n";
    fs::write(&config_path, original).unwrap();
    fs::create_dir(config_root.join("project-trust.tsv")).unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let error = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "enable".to_string(),
            "--preset".to_string(),
            "project-safe".to_string(),
            "--authority".to_string(),
            "trusted-project".to_string(),
            "--path".to_string(),
            project.to_string_lossy().into_owned(),
            "--yes".to_string(),
        ]),
        env,
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Io);
    assert_eq!(fs::read_to_string(&config_path).unwrap(), original);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Profile export emits only the versioned allowlisted recipe fields and
/// excludes host paths, identity, executable, and unrelated configuration.
#[test]
fn sandbox_profile_export_is_deterministic_and_sanitized() {
    let (env, home) = test_env("sandbox-profile-export");
    let project = home.join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    let config_root = home.join(".config/mezzanine");
    fs::create_dir_all(&config_root).unwrap();
    fs::write(
        config_root.join("config.toml"),
        "version = 25\n[permissions]\nsandbox = \"bubblewrap\"\napproval_policy = \"auto-allow\"\nread_scopes = [\"/private/host/path\"]\n[permissions.bubblewrap]\nexecutable = \"/private/bwrap\"\ngit_user_name = \"Private Author\"\ngit_user_email = \"private@example.invalid\"\ntoolchains = [\"rust\"]\n",
    )
    .unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let exit_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "profile".to_string(),
            "export".to_string(),
            "--path".to_string(),
            project.to_string_lossy().into_owned(),
        ]),
        env,
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap();

    assert_eq!(exit_code, 0);
    let recipe: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(
        recipe,
        serde_json::json!({
            "version": 1,
            "preset": "project-read-only",
            "authority": "explicit-scope",
            "toolchains": ["rust"]
        })
    );
    let rendered = String::from_utf8(stdout).unwrap();
    for excluded in [
        "/private",
        "Private Author",
        "private@example.invalid",
        "bwrap",
    ] {
        assert!(!rendered.contains(excluded), "{rendered}");
    }
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Profile export fails closed when the primary config enables a custom
/// toolchain so no declared host root can enter a portable recipe.
#[test]
fn sandbox_profile_export_rejects_custom_toolchain_host_roots() {
    let (env, home) = test_env("sandbox-profile-export-custom");
    let project = home.join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    let config_root = home.join(".config/mezzanine");
    fs::create_dir_all(&config_root).unwrap();
    fs::write(
        config_root.join("config.toml"),
        "version = 32\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = [\"custom:acme\"]\n[permissions.bubblewrap.custom_toolchains.acme]\nroots = [\"/private/acme\"]\npath_entries = [\"0:bin\"]\nrequired_executables = [\"0:bin/acme\"]\n",
    )
    .unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let error = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "profile".to_string(),
            "export".to_string(),
            "--path".to_string(),
            project.to_string_lossy().into_owned(),
        ]),
        env,
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert!(error.message().contains("cannot include custom toolchains"));
    assert!(!String::from_utf8_lossy(&stdout).contains("/private/acme"));
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Profile import rejects unknown or unsafe recipe fields before creating any
/// configuration state.
#[test]
fn sandbox_profile_import_rejects_unknown_and_unsafe_fields() {
    let (env, home) = test_env("sandbox-profile-reject");
    let project = home.join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    let recipe = home.join("unsafe.json");
    fs::write(
        &recipe,
        r#"{"version":1,"preset":"project-safe","authority":"explicit-scope","toolchains":[],"host_path":"/private"}"#,
    )
    .unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let error = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "profile".to_string(),
            "import".to_string(),
            recipe.to_string_lossy().into_owned(),
            "--path".to_string(),
            project.to_string_lossy().into_owned(),
            "--yes".to_string(),
        ]),
        env,
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert!(error.message().contains("unknown field"), "{error}");
    assert!(!home.join(".config/mezzanine").exists());
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Import previews without confirmation, then applies only the reviewed
/// recipe selections to the independently resolved local project.
#[test]
fn sandbox_profile_import_requires_confirmation_and_uses_local_root() {
    let (env, home) = test_env("sandbox-profile-import");
    let project = home.join("local-project");
    fs::create_dir_all(project.join(".git")).unwrap();
    let recipe = home.join("profile.json");
    fs::write(
        &recipe,
        r#"{"version":1,"preset":"project-read-only","authority":"explicit-scope","toolchains":["rust"]}"#,
    )
    .unwrap();
    let config_path = home.join(".config/mezzanine/config.toml");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let base = vec![
        "mez".to_string(),
        "sandbox".to_string(),
        "profile".to_string(),
        "import".to_string(),
        recipe.to_string_lossy().into_owned(),
        "--path".to_string(),
        project.to_string_lossy().into_owned(),
    ];

    let preview_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(base.clone()),
        env.clone(),
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap();
    assert_eq!(preview_code, 1);
    assert!(!config_path.exists());

    stdout.clear();
    let mut confirmed = base;
    confirmed.push("--yes".to_string());
    let applied_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(confirmed),
        env,
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap();
    assert_eq!(applied_code, 0);
    let config = fs::read_to_string(&config_path).unwrap();
    let local_root = project
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(config.contains(&local_root), "{config}");
    assert!(config.contains("write_scopes = []"), "{config}");
    assert!(config.contains("toolchains = [\"rust\"]"), "{config}");
    assert!(!home.join(".config/mezzanine/project-trust.tsv").exists());
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Managed-home cache status is read-only, clear previews without `--yes`,
/// and confirmed clear removes only the selected project's inactive home.
#[test]
fn sandbox_cache_status_and_clear_require_confirmation() {
    let (env, home) = test_env("sandbox-cache-clear");
    let project = home.join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    let canonical_project = project.canonicalize().unwrap();
    let config_root = home.join(".config/mezzanine");
    let managed =
        crate::security::sandbox::prepare_bubblewrap_managed_home(&config_root, &canonical_project)
            .unwrap();
    fs::write(managed.host_path.join(".cache/cli-payload"), b"payload").unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let status_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "cache".to_string(),
            "status".to_string(),
            project.to_string_lossy().into_owned(),
        ]),
        env.clone(),
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap();
    assert_eq!(status_code, 0);
    let status: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(status["operation"], "status");
    assert!(status["total_bytes"].as_u64().unwrap() >= 7);
    assert_eq!(status["homes"][0]["exists"], true);
    assert!(managed.host_path.exists());

    stdout.clear();
    let preview_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "cache".to_string(),
            "clear".to_string(),
            project.to_string_lossy().into_owned(),
        ]),
        env.clone(),
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap();
    assert_eq!(preview_code, 1);
    let preview: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(preview["confirmation_required"], true);
    assert_eq!(preview["candidate_homes"], 1);
    assert_eq!(preview["removed_homes"], 0);
    assert!(managed.host_path.exists());

    stdout.clear();
    let clear_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "cache".to_string(),
            "clear".to_string(),
            project.to_string_lossy().into_owned(),
            "--yes".to_string(),
        ]),
        env,
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap();
    assert_eq!(clear_code, 0);
    let cleared: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(cleared["removed_homes"], 1);
    assert!(!managed.host_path.exists());
    assert!(stderr.is_empty());

    let _ = fs::remove_dir_all(home);
}

/// Confirmed pruning removes inactive homes while reporting and retaining a
/// home whose shared workload activity lock is still held.
#[test]
fn sandbox_cache_prune_skips_active_managed_homes() {
    let (env, home) = test_env("sandbox-cache-prune-active");
    let config_root = home.join(".config/mezzanine");
    let active_project = home.join("active-project");
    let inactive_project = home.join("inactive-project");
    fs::create_dir_all(active_project.join(".git")).unwrap();
    fs::create_dir_all(inactive_project.join(".git")).unwrap();
    let active_project = active_project.canonicalize().unwrap();
    let inactive_project = inactive_project.canonicalize().unwrap();
    let (active_home, activity) =
        crate::security::sandbox::prepare_bubblewrap_managed_home_for_workload(
            &config_root,
            &active_project,
        )
        .unwrap();
    let inactive_home =
        crate::security::sandbox::prepare_bubblewrap_managed_home(&config_root, &inactive_project)
            .unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let exit_code = block_on_cli_code(crate::cli::run_with(
        with_json_output(vec![
            "mez".to_string(),
            "sandbox".to_string(),
            "cache".to_string(),
            "prune".to_string(),
            "--yes".to_string(),
        ]),
        env,
        false,
        &mut stdout,
        &mut stderr,
    ))
    .unwrap();

    assert_eq!(exit_code, 0);
    let result: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(result["active_homes"], 1);
    assert_eq!(result["removed_homes"], 1);
    assert!(active_home.host_path.exists());
    assert!(!inactive_home.host_path.exists());
    assert!(stderr.is_empty());

    drop(activity);
    let _ = fs::remove_dir_all(home);
}
