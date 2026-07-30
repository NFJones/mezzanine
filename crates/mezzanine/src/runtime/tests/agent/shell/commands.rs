//! Agent shell commands tests.

use super::*;
use crate::control::{decode_control_frame, encode_control_body};
use crate::runtime::SandboxConfig;

/// Builds active-pane bootstrap evidence with optional Rust toolchain roots.
fn toolchain_environment(environment_managers: Vec<String>) -> mez_agent::EnvironmentSignature {
    mez_agent::EnvironmentSignature::new(
        "linux",
        "x86_64",
        None,
        "test-host",
        "test-user",
        "/bin/sh",
        mez_agent::ShellClassification::PosixSh,
        None,
        Some("/usr/bin:/bin".to_string()),
        "/workspace",
        Some("/workspace".to_string()),
        true,
        None,
        environment_managers,
    )
    .unwrap()
}

/// Creates a visible primary agent shell backed by one disk config layer.
fn toolchain_command_service(
    name: &str,
    config_text: &str,
) -> (RuntimeSessionService, mez_core::ids::ClientId, PathBuf) {
    let root = temp_root(name);
    fs::create_dir_all(&root).unwrap();
    let path = root.join("config.toml");
    fs::write(&path, config_text).unwrap();
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: Some(path.clone()),
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: config_text.to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    (service, primary, path)
}

/// Verifies `/sandbox toolchains` status and detection consume only active-pane
/// bootstrap evidence and do not mutate config text or generation state.
#[test]
fn runtime_agent_shell_toolchain_status_and_detect_are_read_only() {
    let config =
        "[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = []\n";
    let (mut service, primary, path) =
        toolchain_command_service("runtime-toolchain-read-only", config);
    service.set_pane_environment_signature_for_tests(
        "%1",
        toolchain_environment(vec![
            "cargo-bin:/home/test-user/.cargo/bin".to_string(),
            "rustup:/home/test-user/.rustup".to_string(),
        ]),
    );
    let generation = service.session.config_generation;
    let before = fs::read_to_string(&path).unwrap();

    let status = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"toolchain-status","method":"agent/shell/command","params":{"idempotency_key":"toolchain-status","input":"/sandbox toolchains"}}"#,
        &primary,
    );
    let list = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"toolchain-list","method":"agent/shell/command","params":{"idempotency_key":"toolchain-list","input":"/sandbox toolchains list"}}"#,
        &primary,
    );
    let detect = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"toolchain-detect","method":"agent/shell/command","params":{"idempotency_key":"toolchain-detect","input":"/sandbox toolchains detect rust"}}"#,
        &primary,
    );

    assert!(status.contains(r#""command":"toolchain""#), "{status}");
    assert!(status.contains(r#""presentation":"pager""#), "{status}");
    assert!(status.contains("# Toolchains"), "{status}");
    assert!(status.contains("available-disabled"), "{status}");
    assert!(status.contains("| `rust` | no | yes |"), "{status}");
    assert!(list.contains(r#""presentation":"pager""#), "{list}");
    assert!(list.contains("# Supported Toolchains"), "{list}");
    assert!(list.contains("| `python` | any |"), "{list}");
    assert!(detect.contains(r#""presentation":"pager""#), "{detect}");
    assert!(detect.contains("# Toolchain Detection"), "{detect}");
    assert!(detect.contains("| Available | yes |"), "{detect}");
    assert!(detect.contains("active-pane bootstrap"), "{detect}");
    assert_eq!(service.session.config_generation, generation);
    assert_eq!(fs::read_to_string(&path).unwrap(), before);
    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// Verifies missing active-pane bootstrap evidence is summarized through the
/// transient error-notice route rather than returned as pane-history output.
#[test]
fn runtime_agent_shell_toolchain_detection_without_evidence_is_transient_error() {
    let config =
        "[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = []\n";
    let (mut service, primary, path) =
        toolchain_command_service("runtime-toolchain-missing-evidence", config);

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"toolchain-missing-evidence","method":"agent/shell/command","params":{"idempotency_key":"toolchain-missing-evidence","input":"/sandbox toolchains detect rust"}}"#,
        &primary,
    );

    assert!(
        response.contains(r#""presentation":"error_notice""#),
        "{response}"
    );
    assert!(
        response.contains("requires active-pane bootstrap evidence"),
        "{response}"
    );
    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// Verifies confirmed enable and disable persist only the typed kind, hot-apply
/// to subsequent actions, and advance generation exactly once per real change.
#[test]
fn runtime_agent_shell_toolchain_enable_disable_and_no_op_are_generation_exact() {
    let config =
        "[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = []\n";
    let (mut service, primary, path) =
        toolchain_command_service("runtime-toolchain-mutation", config);
    service.set_pane_environment_signature_for_tests(
        "%1",
        toolchain_environment(vec![
            "cargo-bin:/home/test-user/.cargo/bin".to_string(),
            "rustup:/home/test-user/.rustup".to_string(),
        ]),
    );
    let initial_generation = service.session.config_generation;

    let missing_confirmation = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"toolchain-unconfirmed","method":"agent/shell/command","params":{"idempotency_key":"toolchain-unconfirmed","input":"/sandbox toolchains enable rust"}}"#,
        &primary,
    );
    assert!(
        missing_confirmation.contains("expects status, list, detect"),
        "{missing_confirmation}"
    );
    assert!(
        missing_confirmation.contains(r#""presentation":"error_notice""#),
        "{missing_confirmation}"
    );
    assert_eq!(service.session.config_generation, initial_generation);

    let control_attempt = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"toolchain-control-enable","method":"agent/shell/command","params":{"idempotency_key":"toolchain-control-enable","input":"/sandbox toolchains enable rust --yes"}}"#,
        &primary,
    );
    assert!(
        control_attempt.contains("require authenticated primary-client input"),
        "{control_attempt}"
    );
    assert_eq!(service.session.config_generation, initial_generation);

    let enabled = service
        .execute_agent_shell_command(&primary, "/sandbox toolchains enable rust --yes")
        .unwrap();
    assert!(enabled.contains(r#""presentation":"notice""#), "{enabled}");
    assert!(enabled.contains("Enabled rust; updated"), "{enabled}");
    assert!(enabled.contains("changed=true"), "{enabled}");
    assert_eq!(service.session.config_generation, initial_generation + 1);
    let persisted = fs::read_to_string(&path).unwrap();
    assert!(persisted.contains("toolchains = [\"rust\"]"), "{persisted}");
    assert!(!persisted.contains("/home/test-user"), "{persisted}");

    let enabled_again = service
        .execute_agent_shell_command(&primary, "/sandbox toolchains enable rust --yes")
        .unwrap();
    assert!(
        enabled_again.contains("Enabled rust; no-op"),
        "{enabled_again}"
    );
    assert!(enabled_again.contains("changed=false"), "{enabled_again}");
    assert_eq!(service.session.config_generation, initial_generation + 1);

    let disabled = service
        .execute_agent_shell_command(&primary, "/sandbox toolchains disable rust --yes")
        .unwrap();
    assert!(disabled.contains("Disabled rust; updated"), "{disabled}");
    assert!(disabled.contains("changed=true"), "{disabled}");
    assert_eq!(service.session.config_generation, initial_generation + 2);
    assert!(
        fs::read_to_string(&path)
            .unwrap()
            .contains("toolchains = []")
    );
    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// Confirmed Zig detection and enablement consume only active-pane bootstrap
/// evidence and persist no discovered host path into runtime configuration.
#[test]
fn runtime_agent_shell_zig_toolchain_detects_and_persists_only_kind() {
    let config =
        "[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = []\n";
    let (mut service, primary, path) =
        toolchain_command_service("runtime-zig-toolchain-mutation", config);
    let zig_root = path.parent().unwrap().join("zig-0.14.0");
    fs::create_dir_all(zig_root.join("lib")).unwrap();
    fs::write(zig_root.join("zig"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(zig_root.join("zig"), fs::Permissions::from_mode(0o755)).unwrap();
    let zig_root = zig_root.canonicalize().unwrap();
    service.set_pane_environment_signature_for_tests(
        "%1",
        toolchain_environment(vec![format!("zig:{}", zig_root.display())]),
    );

    let detect = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"zig-detect","method":"agent/shell/command","params":{"idempotency_key":"zig-detect","input":"/sandbox toolchains detect zig"}}"#,
        &primary,
    );
    assert!(detect.contains(r#""presentation":"pager""#), "{detect}");
    assert!(detect.contains("| Kind | `zig` |"), "{detect}");
    assert!(detect.contains("| Available | yes |"), "{detect}");

    let enabled = service
        .execute_agent_shell_command(&primary, "/sandbox toolchains enable zig --yes")
        .unwrap();
    assert!(enabled.contains(r#""presentation":"notice""#), "{enabled}");
    assert!(enabled.contains("Enabled zig; updated"), "{enabled}");
    assert!(enabled.contains("changed=true"), "{enabled}");
    let persisted = fs::read_to_string(&path).unwrap();
    assert!(persisted.contains("toolchains = [\"zig\"]"), "{persisted}");
    assert!(
        !persisted.contains(&zig_root.to_string_lossy().into_owned()),
        "{persisted}"
    );

    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// Confirmed Go detection and enablement consume only active-pane SDK
/// evidence and never persist the discovered host root, GOPATH, or GOBIN.
#[test]
fn runtime_agent_shell_go_toolchain_detects_and_persists_only_kind() {
    let config =
        "[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = []\n";
    let (mut service, primary, path) =
        toolchain_command_service("runtime-go-toolchain-mutation", config);
    let go_root = path.parent().unwrap().join("go-sdk");
    fs::create_dir_all(go_root.join("bin")).unwrap();
    fs::create_dir_all(go_root.join("src")).unwrap();
    fs::write(go_root.join("bin/go"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(go_root.join("bin/go"), fs::Permissions::from_mode(0o755)).unwrap();
    let go_root = go_root.canonicalize().unwrap();
    service.set_pane_environment_signature_for_tests(
        "%1",
        toolchain_environment(vec![format!("go:{}", go_root.display())]),
    );

    let detect = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"go-detect","method":"agent/shell/command","params":{"idempotency_key":"go-detect","input":"/sandbox toolchains detect go"}}"#,
        &primary,
    );
    assert!(detect.contains(r#""presentation":"pager""#), "{detect}");
    assert!(detect.contains("| Kind | `go` |"), "{detect}");
    assert!(detect.contains("| Available | yes |"), "{detect}");

    let enabled = service
        .execute_agent_shell_command(&primary, "/sandbox toolchains enable go --yes")
        .unwrap();
    assert!(enabled.contains(r#""presentation":"notice""#), "{enabled}");
    assert!(enabled.contains("Enabled go; updated"), "{enabled}");
    assert!(enabled.contains("changed=true"), "{enabled}");
    let persisted = fs::read_to_string(&path).unwrap();
    assert!(persisted.contains("toolchains = [\"go\"]"), "{persisted}");
    assert!(
        !persisted.contains(&go_root.to_string_lossy().into_owned()),
        "{persisted}"
    );

    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// Confirmed Deno detection and enablement consume only active-pane runtime
/// evidence and never persist the discovered host root or host cache state.
#[test]
fn runtime_agent_shell_deno_toolchain_detects_and_persists_only_kind() {
    let config =
        "[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = []\n";
    let (mut service, primary, path) =
        toolchain_command_service("runtime-deno-toolchain-mutation", config);
    let deno_root = path.parent().unwrap().join("deno-runtime");
    fs::create_dir_all(&deno_root).unwrap();
    fs::write(deno_root.join("deno"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(deno_root.join("deno"), fs::Permissions::from_mode(0o755)).unwrap();
    let deno_root = deno_root.canonicalize().unwrap();
    service.set_pane_environment_signature_for_tests(
        "%1",
        toolchain_environment(vec![format!("deno:{}", deno_root.display())]),
    );

    let detect = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"deno-detect","method":"agent/shell/command","params":{"idempotency_key":"deno-detect","input":"/sandbox toolchains detect deno"}}"#,
        &primary,
    );
    assert!(detect.contains(r#""presentation":"pager""#), "{detect}");
    assert!(detect.contains("| Kind | `deno` |"), "{detect}");
    assert!(detect.contains("| Available | yes |"), "{detect}");

    let enabled = service
        .execute_agent_shell_command(&primary, "/sandbox toolchains enable deno --yes")
        .unwrap();
    assert!(enabled.contains(r#""presentation":"notice""#), "{enabled}");
    assert!(enabled.contains("Enabled deno; updated"), "{enabled}");
    assert!(enabled.contains("changed=true"), "{enabled}");
    let persisted = fs::read_to_string(&path).unwrap();
    assert!(persisted.contains("toolchains = [\"deno\"]"), "{persisted}");
    assert!(
        !persisted.contains(&deno_root.to_string_lossy().into_owned()),
        "{persisted}"
    );

    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// Confirmed Bun detection and enablement consume only active-pane
/// distribution evidence and never persist its host root or package state.
#[test]
fn runtime_agent_shell_bun_toolchain_detects_and_persists_only_kind() {
    let config =
        "[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = []\n";
    let (mut service, primary, path) =
        toolchain_command_service("runtime-bun-toolchain-mutation", config);
    let bun_root = path.parent().unwrap().join("bun-runtime");
    fs::create_dir_all(bun_root.join("bin")).unwrap();
    fs::write(bun_root.join("bin/bun"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(bun_root.join("bin/bun"), fs::Permissions::from_mode(0o755)).unwrap();
    let bun_root = bun_root.canonicalize().unwrap();
    service.set_pane_environment_signature_for_tests(
        "%1",
        toolchain_environment(vec![format!("bun:{}", bun_root.display())]),
    );

    let detect = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"bun-detect","method":"agent/shell/command","params":{"idempotency_key":"bun-detect","input":"/sandbox toolchains detect bun"}}"#,
        &primary,
    );
    assert!(detect.contains(r#""presentation":"pager""#), "{detect}");
    assert!(detect.contains("| Kind | `bun` |"), "{detect}");
    assert!(detect.contains("| Available | yes |"), "{detect}");

    let enabled = service
        .execute_agent_shell_command(&primary, "/sandbox toolchains enable bun --yes")
        .unwrap();
    assert!(enabled.contains(r#""presentation":"notice""#), "{enabled}");
    assert!(enabled.contains("Enabled bun; updated"), "{enabled}");
    assert!(enabled.contains("changed=true"), "{enabled}");
    let persisted = fs::read_to_string(&path).unwrap();
    assert!(persisted.contains("toolchains = [\"bun\"]"), "{persisted}");
    assert!(
        !persisted.contains(&bun_root.to_string_lossy().into_owned()),
        "{persisted}"
    );

    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// Confirmed Node.js detection and enablement consume only active-pane
/// distribution evidence and never persist its host root or package state.
#[test]
fn runtime_agent_shell_node_toolchain_detects_and_persists_only_kind() {
    let config =
        "[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = []\n";
    let (mut service, primary, path) =
        toolchain_command_service("runtime-node-toolchain-mutation", config);
    let node_root = path.parent().unwrap().join("node-runtime");
    fs::create_dir_all(node_root.join("bin")).unwrap();
    fs::create_dir_all(node_root.join("lib")).unwrap();
    fs::write(node_root.join("bin/node"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(
        node_root.join("bin/node"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let node_root = node_root.canonicalize().unwrap();
    service.set_pane_environment_signature_for_tests(
        "%1",
        toolchain_environment(vec![format!("node-runtime:{}", node_root.display())]),
    );

    let detect = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"node-detect","method":"agent/shell/command","params":{"idempotency_key":"node-detect","input":"/sandbox toolchains detect node"}}"#,
        &primary,
    );
    assert!(detect.contains(r#""presentation":"pager""#), "{detect}");
    assert!(detect.contains("| Kind | `node` |"), "{detect}");
    assert!(detect.contains("| Available | yes |"), "{detect}");

    let enabled = service
        .execute_agent_shell_command(&primary, "/sandbox toolchains enable node --yes")
        .unwrap();
    assert!(enabled.contains(r#""presentation":"notice""#), "{enabled}");
    assert!(enabled.contains("Enabled node; updated"), "{enabled}");
    assert!(enabled.contains("changed=true"), "{enabled}");
    let persisted = fs::read_to_string(&path).unwrap();
    assert!(persisted.contains("toolchains = [\"node\"]"), "{persisted}");
    assert!(
        !persisted.contains(&node_root.to_string_lossy().into_owned()),
        "{persisted}"
    );

    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// Confirmed Python detection and enablement consume only active-pane base
/// runtime evidence and persist the typed kind without host paths or caches.
#[test]
fn runtime_agent_shell_python_toolchain_detects_and_persists_only_kind() {
    let config =
        "[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = []\n";
    let (mut service, primary, path) =
        toolchain_command_service("runtime-python-toolchain-mutation", config);
    let python_root = path.parent().unwrap().join("python-runtime");
    fs::create_dir_all(python_root.join("bin")).unwrap();
    fs::create_dir_all(python_root.join("lib")).unwrap();
    fs::write(python_root.join("bin/python3"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(
        python_root.join("bin/python3"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let python_root = python_root.canonicalize().unwrap();
    service.set_pane_environment_signature_for_tests(
        "%1",
        toolchain_environment(vec![format!("python-runtime:{}", python_root.display())]),
    );

    let detect = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"python-detect","method":"agent/shell/command","params":{"idempotency_key":"python-detect","input":"/sandbox toolchains detect python"}}"#,
        &primary,
    );
    assert!(detect.contains(r#""presentation":"pager""#), "{detect}");
    assert!(detect.contains("| Kind | `python` |"), "{detect}");
    assert!(detect.contains("| Available | yes |"), "{detect}");

    let enabled = service
        .execute_agent_shell_command(&primary, "/sandbox toolchains enable python --yes")
        .unwrap();
    assert!(enabled.contains(r#""presentation":"notice""#), "{enabled}");
    assert!(enabled.contains("Enabled python; updated"), "{enabled}");
    assert!(enabled.contains("changed=true"), "{enabled}");
    let persisted = fs::read_to_string(&path).unwrap();
    assert!(
        persisted.contains("toolchains = [\"python\"]"),
        "{persisted}"
    );
    assert!(
        !persisted.contains(&python_root.to_string_lossy().into_owned()),
        "{persisted}"
    );

    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// Confirmed JDK detection and enablement consume only exact active-pane SDK
/// evidence and persist the typed kind without host paths or manager state.
#[test]
fn runtime_agent_shell_jdk_toolchain_detects_and_persists_only_kind() {
    let config =
        "[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = []\n";
    let (mut service, primary, path) =
        toolchain_command_service("runtime-jdk-toolchain-mutation", config);
    let jdk_root = path.parent().unwrap().join("jdk-runtime");
    fs::create_dir_all(jdk_root.join("bin")).unwrap();
    fs::create_dir_all(jdk_root.join("lib")).unwrap();
    for executable in ["java", "javac", "jar"] {
        let executable_path = jdk_root.join("bin").join(executable);
        fs::write(&executable_path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(executable_path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let jdk_root = jdk_root.canonicalize().unwrap();
    service.set_pane_environment_signature_for_tests(
        "%1",
        toolchain_environment(vec![format!("jdk-runtime:{}", jdk_root.display())]),
    );

    let detect = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"jdk-detect","method":"agent/shell/command","params":{"idempotency_key":"jdk-detect","input":"/sandbox toolchains detect jdk"}}"#,
        &primary,
    );
    assert!(detect.contains(r#""presentation":"pager""#), "{detect}");
    assert!(detect.contains("| Kind | `jdk` |"), "{detect}");
    assert!(detect.contains("| Available | yes |"), "{detect}");

    let enabled = service
        .execute_agent_shell_command(&primary, "/sandbox toolchains enable jdk --yes")
        .unwrap();
    assert!(enabled.contains(r#""presentation":"notice""#), "{enabled}");
    assert!(enabled.contains("Enabled jdk; updated"), "{enabled}");
    assert!(enabled.contains("changed=true"), "{enabled}");
    let persisted = fs::read_to_string(&path).unwrap();
    assert!(persisted.contains("toolchains = [\"jdk\"]"), "{persisted}");
    assert!(
        !persisted.contains(&jdk_root.to_string_lossy().into_owned()),
        "{persisted}"
    );

    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// Confirmed .NET SDK detection and enablement consume exact active-pane
/// evidence and persist only the typed kind, never host paths or NuGet state.
#[test]
fn runtime_agent_shell_dotnet_toolchain_detects_and_persists_only_kind() {
    let config =
        "[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = []\n";
    let (mut service, primary, path) =
        toolchain_command_service("runtime-dotnet-toolchain-mutation", config);
    let dotnet_root = path.parent().unwrap().join("dotnet-sdk");
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
    service.set_pane_environment_signature_for_tests(
        "%1",
        toolchain_environment(vec![format!("dotnet-sdk:{}", dotnet_root.display())]),
    );

    let detect = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"dotnet-detect","method":"agent/shell/command","params":{"idempotency_key":"dotnet-detect","input":"/sandbox toolchains detect dotnet"}}"#,
        &primary,
    );
    assert!(detect.contains(r#""presentation":"pager""#), "{detect}");
    assert!(detect.contains("| Kind | `dotnet` |"), "{detect}");
    assert!(detect.contains("| Available | yes |"), "{detect}");

    let enabled = service
        .execute_agent_shell_command(&primary, "/sandbox toolchains enable dotnet --yes")
        .unwrap();
    assert!(enabled.contains(r#""presentation":"notice""#), "{enabled}");
    assert!(enabled.contains("Enabled dotnet; updated"), "{enabled}");
    assert!(enabled.contains("changed=true"), "{enabled}");
    let persisted = fs::read_to_string(&path).unwrap();
    assert!(
        persisted.contains("toolchains = [\"dotnet\"]"),
        "{persisted}"
    );
    assert!(
        !persisted.contains(&dotnet_root.to_string_lossy().into_owned()),
        "{persisted}"
    );

    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// Confirmed Dart SDK detection and enablement consume exact active-pane
/// evidence and persist only the typed kind, never host paths or Pub state.
#[test]
fn runtime_agent_shell_dart_toolchain_detects_and_persists_only_kind() {
    let config =
        "[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = []\n";
    let (mut service, primary, path) =
        toolchain_command_service("runtime-dart-toolchain-mutation", config);
    let dart_root = path.parent().unwrap().join("dart-sdk");
    fs::create_dir_all(dart_root.join("bin")).unwrap();
    fs::create_dir_all(dart_root.join("lib")).unwrap();
    fs::write(dart_root.join("bin/dart"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(
        dart_root.join("bin/dart"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let dart_root = dart_root.canonicalize().unwrap();
    service.set_pane_environment_signature_for_tests(
        "%1",
        toolchain_environment(vec![format!("dart-sdk:{}", dart_root.display())]),
    );

    let detect = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"dart-detect","method":"agent/shell/command","params":{"idempotency_key":"dart-detect","input":"/sandbox toolchains detect dart"}}"#,
        &primary,
    );
    assert!(detect.contains(r#""presentation":"pager""#), "{detect}");
    assert!(detect.contains("| Kind | `dart` |"), "{detect}");
    assert!(detect.contains("| Available | yes |"), "{detect}");

    let enabled = service
        .execute_agent_shell_command(&primary, "/sandbox toolchains enable dart --yes")
        .unwrap();
    assert!(enabled.contains(r#""presentation":"notice""#), "{enabled}");
    assert!(enabled.contains("Enabled dart; updated"), "{enabled}");
    assert!(enabled.contains("changed=true"), "{enabled}");
    let persisted = fs::read_to_string(&path).unwrap();
    assert!(persisted.contains("toolchains = [\"dart\"]"), "{persisted}");
    assert!(
        !persisted.contains(&dart_root.to_string_lossy().into_owned()),
        "{persisted}"
    );

    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// Kotlin/JVM detection requires exact compiler and JDK evidence, and
/// enablement persists only both typed selections rather than either host root.
#[test]
fn runtime_agent_shell_kotlin_toolchain_requires_jdk_and_persists_only_kinds() {
    let config = "[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = [\"jdk\"]\n";
    let (mut service, primary, path) =
        toolchain_command_service("runtime-kotlin-toolchain-mutation", config);
    let jdk_root = path.parent().unwrap().join("jdk-runtime");
    fs::create_dir_all(jdk_root.join("bin")).unwrap();
    fs::create_dir_all(jdk_root.join("lib")).unwrap();
    for executable in ["java", "javac", "jar"] {
        let executable_path = jdk_root.join("bin").join(executable);
        fs::write(&executable_path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(executable_path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let kotlin_root = path.parent().unwrap().join("kotlin-compiler");
    fs::create_dir_all(kotlin_root.join("bin")).unwrap();
    fs::create_dir_all(kotlin_root.join("lib")).unwrap();
    for executable in ["kotlinc", "kotlin"] {
        let executable_path = kotlin_root.join("bin").join(executable);
        fs::write(&executable_path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(executable_path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let jdk_root = jdk_root.canonicalize().unwrap();
    let kotlin_root = kotlin_root.canonicalize().unwrap();
    service.set_pane_environment_signature_for_tests(
        "%1",
        toolchain_environment(vec![
            format!("jdk-runtime:{}", jdk_root.display()),
            format!("kotlin-jvm:{}", kotlin_root.display()),
        ]),
    );

    let detect = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"kotlin-detect","method":"agent/shell/command","params":{"idempotency_key":"kotlin-detect","input":"/sandbox toolchains detect kotlin"}}"#,
        &primary,
    );
    assert!(detect.contains(r#""presentation":"pager""#), "{detect}");
    assert!(detect.contains("| Kind | `kotlin` |"), "{detect}");
    assert!(detect.contains("| Available | yes |"), "{detect}");

    let enabled = service
        .execute_agent_shell_command(&primary, "/sandbox toolchains enable kotlin --yes")
        .unwrap();
    assert!(enabled.contains(r#""presentation":"notice""#), "{enabled}");
    assert!(enabled.contains("Enabled kotlin; updated"), "{enabled}");
    assert!(enabled.contains("changed=true"), "{enabled}");
    let persisted = fs::read_to_string(&path).unwrap();
    assert!(
        persisted.contains("toolchains = [\"jdk\", \"kotlin\"]"),
        "{persisted}"
    );
    assert!(
        !persisted.contains(&jdk_root.to_string_lossy().into_owned()),
        "{persisted}"
    );
    assert!(
        !persisted.contains(&kotlin_root.to_string_lossy().into_owned()),
        "{persisted}"
    );

    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// Ruby detection consumes exact active-pane runtime evidence and enablement
/// persists only the typed kind, never host roots, gemsets, or Bundler state.
#[test]
fn runtime_agent_shell_ruby_toolchain_detects_and_persists_only_kind() {
    let config =
        "[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = []\n";
    let (mut service, primary, path) =
        toolchain_command_service("runtime-ruby-toolchain-mutation", config);
    let ruby_root = path.parent().unwrap().join("ruby-runtime");
    fs::create_dir_all(ruby_root.join("bin")).unwrap();
    fs::create_dir_all(ruby_root.join("lib/ruby")).unwrap();
    for executable in ["ruby", "gem", "bundle"] {
        let executable_path = ruby_root.join("bin").join(executable);
        fs::write(&executable_path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(executable_path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let ruby_root = ruby_root.canonicalize().unwrap();
    service.set_pane_environment_signature_for_tests(
        "%1",
        toolchain_environment(vec![format!("ruby-runtime:{}", ruby_root.display())]),
    );

    let detect = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"ruby-detect","method":"agent/shell/command","params":{"idempotency_key":"ruby-detect","input":"/sandbox toolchains detect ruby"}}"#,
        &primary,
    );
    assert!(detect.contains(r#""presentation":"pager""#), "{detect}");
    assert!(detect.contains("| Kind | `ruby` |"), "{detect}");
    assert!(detect.contains("| Available | yes |"), "{detect}");

    let enabled = service
        .execute_agent_shell_command(&primary, "/sandbox toolchains enable ruby --yes")
        .unwrap();
    assert!(enabled.contains(r#""presentation":"notice""#), "{enabled}");
    assert!(enabled.contains("Enabled ruby; updated"), "{enabled}");
    assert!(enabled.contains("changed=true"), "{enabled}");
    let persisted = fs::read_to_string(&path).unwrap();
    assert!(persisted.contains("toolchains = [\"ruby\"]"), "{persisted}");
    assert!(
        !persisted.contains(&ruby_root.to_string_lossy().into_owned()),
        "{persisted}"
    );

    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// Composer detection requires exact PHP and Composer evidence, while
/// enablement preserves both typed selections without persisting host roots.
#[test]
fn runtime_agent_shell_composer_requires_php_and_persists_only_kinds() {
    let config = "[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = [\"php\"]\n";
    let (mut service, primary, path) =
        toolchain_command_service("runtime-php-composer-toolchain-mutation", config);
    let php_root = path.parent().unwrap().join("php-runtime");
    fs::create_dir_all(php_root.join("bin")).unwrap();
    fs::create_dir_all(php_root.join("lib/php")).unwrap();
    fs::write(php_root.join("bin/php"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(php_root.join("bin/php"), fs::Permissions::from_mode(0o755)).unwrap();
    let composer_root = path.parent().unwrap().join("composer-runtime");
    fs::create_dir_all(composer_root.join("bin")).unwrap();
    fs::write(composer_root.join("bin/composer"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(
        composer_root.join("bin/composer"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let php_root = php_root.canonicalize().unwrap();
    let composer_root = composer_root.canonicalize().unwrap();
    service.set_pane_environment_signature_for_tests(
        "%1",
        toolchain_environment(vec![
            format!("php-runtime:{}", php_root.display()),
            format!("composer-runtime:{}", composer_root.display()),
        ]),
    );

    let detect = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"composer-detect","method":"agent/shell/command","params":{"idempotency_key":"composer-detect","input":"/sandbox toolchains detect composer"}}"#,
        &primary,
    );
    assert!(detect.contains(r#""presentation":"pager""#), "{detect}");
    assert!(detect.contains("| Kind | `composer` |"), "{detect}");
    assert!(detect.contains("| Available | yes |"), "{detect}");

    let enabled = service
        .execute_agent_shell_command(&primary, "/sandbox toolchains enable composer --yes")
        .unwrap();
    assert!(enabled.contains(r#""presentation":"notice""#), "{enabled}");
    assert!(enabled.contains("Enabled composer; updated"), "{enabled}");
    assert!(enabled.contains("changed=true"), "{enabled}");
    let persisted = fs::read_to_string(&path).unwrap();
    assert!(
        persisted.contains("toolchains = [\"php\", \"composer\"]"),
        "{persisted}"
    );
    assert!(
        !persisted.contains(&php_root.to_string_lossy().into_owned()),
        "{persisted}"
    );
    assert!(
        !persisted.contains(&composer_root.to_string_lossy().into_owned()),
        "{persisted}"
    );

    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// Elixir detection requires exact Erlang and Elixir evidence, while
/// enablement preserves both typed selections without persisting host roots.
#[test]
fn runtime_agent_shell_elixir_requires_erlang_and_persists_only_kinds() {
    let config = "[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = [\"erlang\"]\n";
    let (mut service, primary, path) =
        toolchain_command_service("runtime-erlang-elixir-toolchain-mutation", config);
    let erlang_root = path.parent().unwrap().join("erlang-runtime");
    fs::create_dir_all(erlang_root.join("bin")).unwrap();
    fs::create_dir_all(erlang_root.join("lib/erlang")).unwrap();
    for executable in ["erl", "erlc", "escript"] {
        let executable_path = erlang_root.join("bin").join(executable);
        fs::write(&executable_path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(executable_path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let elixir_root = path.parent().unwrap().join("elixir-runtime");
    fs::create_dir_all(elixir_root.join("bin")).unwrap();
    fs::create_dir_all(elixir_root.join("lib/elixir")).unwrap();
    for executable in ["elixir", "elixirc", "mix"] {
        let executable_path = elixir_root.join("bin").join(executable);
        fs::write(&executable_path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(executable_path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let erlang_root = erlang_root.canonicalize().unwrap();
    let elixir_root = elixir_root.canonicalize().unwrap();
    service.set_pane_environment_signature_for_tests(
        "%1",
        toolchain_environment(vec![
            format!("erlang-otp:{}", erlang_root.display()),
            format!("elixir-runtime:{}", elixir_root.display()),
        ]),
    );

    let detect = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"elixir-detect","method":"agent/shell/command","params":{"idempotency_key":"elixir-detect","input":"/sandbox toolchains detect elixir"}}"#,
        &primary,
    );
    assert!(detect.contains(r#""presentation":"pager""#), "{detect}");
    assert!(detect.contains("| Kind | `elixir` |"), "{detect}");
    assert!(detect.contains("| Available | yes |"), "{detect}");

    let enabled = service
        .execute_agent_shell_command(&primary, "/sandbox toolchains enable elixir --yes")
        .unwrap();
    assert!(enabled.contains(r#""presentation":"notice""#), "{enabled}");
    assert!(enabled.contains("Enabled elixir; updated"), "{enabled}");
    assert!(enabled.contains("changed=true"), "{enabled}");
    let persisted = fs::read_to_string(&path).unwrap();
    assert!(
        persisted.contains("toolchains = [\"erlang\", \"elixir\"]"),
        "{persisted}"
    );
    assert!(
        !persisted.contains(&erlang_root.to_string_lossy().into_owned()),
        "{persisted}"
    );
    assert!(
        !persisted.contains(&elixir_root.to_string_lossy().into_owned()),
        "{persisted}"
    );

    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// Cabal and Stack detection requires exact GHC companion evidence, while
/// enablement preserves typed selections without persisting any host roots.
#[test]
fn runtime_agent_shell_haskell_companions_require_ghc_and_persist_only_kinds() {
    let config = "[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = [\"ghc\"]\n";
    let (mut service, primary, path) =
        toolchain_command_service("runtime-haskell-toolchain-mutation", config);
    let ghc_root = path.parent().unwrap().join("ghc-compiler");
    fs::create_dir_all(ghc_root.join("bin")).unwrap();
    fs::create_dir_all(ghc_root.join("lib/ghc")).unwrap();
    for executable in ["ghc", "ghci", "runghc", "ghc-pkg"] {
        let executable_path = ghc_root.join("bin").join(executable);
        fs::write(&executable_path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(executable_path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let cabal_root = path.parent().unwrap().join("cabal-companion");
    fs::create_dir_all(cabal_root.join("bin")).unwrap();
    fs::write(cabal_root.join("bin/cabal"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(
        cabal_root.join("bin/cabal"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let stack_root = path.parent().unwrap().join("stack-companion");
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
    service.set_pane_environment_signature_for_tests(
        "%1",
        toolchain_environment(vec![
            format!("ghc-compiler:{}", ghc_root.display()),
            format!("cabal-companion:{}", cabal_root.display()),
            format!("stack-companion:{}", stack_root.display()),
        ]),
    );

    for kind in ["cabal", "stack"] {
        let detect = service.dispatch_runtime_control_body(
            &format!(
                r#"{{"jsonrpc":"2.0","id":"{kind}-detect","method":"agent/shell/command","params":{{"idempotency_key":"{kind}-detect","input":"/sandbox toolchains detect {kind}"}}}}"#
            ),
            &primary,
        );
        assert!(detect.contains(r#""presentation":"pager""#), "{detect}");
        assert!(detect.contains(&format!("| Kind | `{kind}` |")), "{detect}");
        assert!(detect.contains("| Available | yes |"), "{detect}");
    }

    let enabled = service
        .execute_agent_shell_command(&primary, "/sandbox toolchains enable cabal stack --yes")
        .unwrap();
    assert!(enabled.contains(r#""presentation":"notice""#), "{enabled}");
    assert!(enabled.contains("changed=true"), "{enabled}");
    let persisted = fs::read_to_string(&path).unwrap();
    assert!(
        persisted.contains("toolchains = [\"ghc\", \"cabal\", \"stack\"]"),
        "{persisted}"
    );
    for root in [&ghc_root, &cabal_root, &stack_root] {
        assert!(
            !persisted.contains(&root.to_string_lossy().into_owned()),
            "{persisted}"
        );
    }

    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// OCaml detection and enablement use only the active pane's trusted project
/// `_opam` switch while persisting the kind rather than any canonical host path.
#[test]
fn runtime_agent_shell_ocaml_uses_trusted_project_local_switch() {
    let config =
        "[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = []\n";
    let (mut service, primary, path) =
        toolchain_command_service("runtime-ocaml-toolchain-mutation", config);
    let project = path.parent().unwrap().join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    let environment = project.join("_opam");
    for directory in ["bin", "lib", "share"] {
        fs::create_dir_all(environment.join(directory)).unwrap();
    }
    for executable in ["ocaml", "ocamlc", "ocamlopt", "dune"] {
        let executable_path = environment.join("bin").join(executable);
        fs::write(&executable_path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(executable_path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let project = project.canonicalize().unwrap();
    let environment = environment.canonicalize().unwrap();
    let mut trust_store = ProjectTrustStore::default();
    trust_store
        .decide_at(
            project.clone(),
            TrustDecision::Trusted,
            Some(project.join(".git")),
            1,
        )
        .unwrap();
    service.set_project_trust_store(trust_store, None);
    service.set_pane_current_working_directory("%1".to_string(), project.clone());
    service.set_pane_environment_signature_for_tests("%1", toolchain_environment(Vec::new()));

    let detect = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"ocaml-detect","method":"agent/shell/command","params":{"idempotency_key":"ocaml-detect","input":"/sandbox toolchains detect ocaml"}}"#,
        &primary,
    );
    assert!(detect.contains(r#""presentation":"pager""#), "{detect}");
    assert!(detect.contains("| Kind | `ocaml` |"), "{detect}");
    assert!(detect.contains("| Available | yes |"), "{detect}");
    assert!(
        detect.contains(&environment.to_string_lossy().into_owned()),
        "{detect}"
    );

    let enabled = service
        .execute_agent_shell_command(&primary, "/sandbox toolchains enable ocaml --yes")
        .unwrap();
    assert!(enabled.contains(r#""presentation":"notice""#), "{enabled}");
    assert!(enabled.contains("changed=true"), "{enabled}");
    let persisted = fs::read_to_string(&path).unwrap();
    assert!(
        persisted.contains("toolchains = [\"ocaml\"]"),
        "{persisted}"
    );
    assert!(
        !persisted.contains(&environment.to_string_lossy().into_owned()),
        "{persisted}"
    );

    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// Native toolchain detection consumes exact active-pane bootstrap roots, and
/// enabling the explicit bundle persists only ordered kinds rather than any
/// canonical compiler or build-tool path.
#[test]
fn runtime_agent_shell_native_toolchains_persist_only_kinds() {
    let config =
        "[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = []\n";
    let (mut service, primary, path) =
        toolchain_command_service("runtime-native-toolchain-mutation", config);
    let specifications = [
        (
            "llvm",
            "llvm-toolchain",
            ["clang", "clang++", "llvm-ar", "llvm-config"].as_slice(),
            ["lib/clang"].as_slice(),
        ),
        (
            "gcc",
            "gcc-toolchain",
            ["gcc", "g++", "gcc-ar"].as_slice(),
            ["lib/gcc"].as_slice(),
        ),
        (
            "cmake",
            "cmake-toolchain",
            ["cmake", "ctest"].as_slice(),
            ["share/cmake"].as_slice(),
        ),
        (
            "ninja",
            "ninja-toolchain",
            ["ninja"].as_slice(),
            [].as_slice(),
        ),
        (
            "meson",
            "meson-toolchain",
            ["meson"].as_slice(),
            [].as_slice(),
        ),
    ];
    let mut roots = Vec::new();
    let mut managers = Vec::new();
    for (kind, evidence, executables, directories) in specifications {
        let root = path.parent().unwrap().join(kind);
        fs::create_dir_all(root.join("bin")).unwrap();
        for directory in directories {
            fs::create_dir_all(root.join(directory)).unwrap();
        }
        for executable in executables {
            let executable_path = root.join("bin").join(executable);
            fs::write(&executable_path, "#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(executable_path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let root = root.canonicalize().unwrap();
        managers.push(format!("{evidence}:{}", root.display()));
        roots.push((kind, root));
    }
    service.set_pane_environment_signature_for_tests("%1", toolchain_environment(managers));

    for (kind, root) in &roots {
        let detect = service.dispatch_runtime_control_body(
            &format!(
                r#"{{"jsonrpc":"2.0","id":"{kind}-detect","method":"agent/shell/command","params":{{"idempotency_key":"{kind}-detect","input":"/sandbox toolchains detect {kind}"}}}}"#
            ),
            &primary,
        );
        assert!(detect.contains(r#""presentation":"pager""#), "{detect}");
        assert!(detect.contains(&format!("| Kind | `{kind}` |")), "{detect}");
        assert!(detect.contains("| Available | yes |"), "{detect}");
        assert!(
            detect.contains(&root.to_string_lossy().into_owned()),
            "{detect}"
        );
    }

    let enabled = service
        .execute_agent_shell_command(
            &primary,
            "/sandbox toolchains enable llvm gcc cmake ninja meson --yes",
        )
        .unwrap();
    assert!(enabled.contains(r#""presentation":"notice""#), "{enabled}");
    assert!(enabled.contains("changed=true"), "{enabled}");
    let persisted = fs::read_to_string(&path).unwrap();
    assert!(
        persisted.contains("toolchains = [\"llvm\", \"gcc\", \"cmake\", \"ninja\", \"meson\"]"),
        "{persisted}"
    );
    for (_, root) in &roots {
        assert!(
            !persisted.contains(&root.to_string_lossy().into_owned()),
            "{persisted}"
        );
    }

    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// Swift detection consumes only exact active-pane bootstrap evidence, while
/// enablement persists the typed kind without serializing the discovered root.
#[test]
fn runtime_agent_shell_swift_toolchain_persists_only_kind() {
    let config =
        "[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = []\n";
    let (mut service, primary, path) =
        toolchain_command_service("runtime-swift-toolchain-mutation", config);
    let root = path.parent().unwrap().join("swift");
    fs::create_dir_all(root.join("bin")).unwrap();
    fs::create_dir_all(root.join("lib/swift/linux")).unwrap();
    for executable in ["swift", "swiftc", "swift-package", "sourcekit-lsp"] {
        let executable_path = root.join("bin").join(executable);
        fs::write(&executable_path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(executable_path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let root = root.canonicalize().unwrap();
    service.set_pane_environment_signature_for_tests(
        "%1",
        toolchain_environment(vec![format!("swift-toolchain:{}", root.display())]),
    );

    let detect = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"swift-detect","method":"agent/shell/command","params":{"idempotency_key":"swift-detect","input":"/sandbox toolchains detect swift"}}"#,
        &primary,
    );
    assert!(detect.contains(r#""presentation":"pager""#), "{detect}");
    assert!(detect.contains("| Kind | `swift` |"), "{detect}");
    assert!(detect.contains("| Available | yes |"), "{detect}");
    assert!(
        detect.contains(&root.to_string_lossy().into_owned()),
        "{detect}"
    );
    assert!(
        detect.contains("/opt/mez/toolchains/swift/root/bin:/usr/bin:/bin"),
        "{detect}"
    );

    let enabled = service
        .execute_agent_shell_command(&primary, "/sandbox toolchains enable swift --yes")
        .unwrap();
    assert!(enabled.contains(r#""presentation":"notice""#), "{enabled}");
    assert!(enabled.contains("changed=true"), "{enabled}");
    let persisted = fs::read_to_string(&path).unwrap();
    assert!(
        persisted.contains("toolchains = [\"swift\"]"),
        "{persisted}"
    );
    assert!(
        !persisted.contains(&root.to_string_lossy().into_owned()),
        "{persisted}"
    );

    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// Maven and Gradle detection prefers trusted-project wrappers and enabling
/// the explicit JDK bundle persists only its typed kinds.
#[test]
fn runtime_agent_shell_jvm_wrappers_persist_only_kinds() {
    let config =
        "[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = []\n";
    let (mut service, primary, path) =
        toolchain_command_service("runtime-jvm-wrapper-toolchain-mutation", config);
    let jdk = path.parent().unwrap().join("jdk");
    fs::create_dir_all(jdk.join("bin")).unwrap();
    fs::create_dir_all(jdk.join("lib")).unwrap();
    for executable in ["java", "javac", "jar"] {
        let executable_path = jdk.join("bin").join(executable);
        fs::write(&executable_path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(executable_path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let project = path.parent().unwrap().join("project");
    fs::create_dir_all(project.join(".git")).unwrap();
    fs::create_dir_all(project.join(".mvn/wrapper")).unwrap();
    fs::create_dir_all(project.join("gradle/wrapper")).unwrap();
    for wrapper in ["mvnw", "gradlew"] {
        let executable_path = project.join(wrapper);
        fs::write(&executable_path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(executable_path, fs::Permissions::from_mode(0o755)).unwrap();
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
    let jdk = jdk.canonicalize().unwrap();
    let project = project.canonicalize().unwrap();
    let mut trust_store = ProjectTrustStore::default();
    trust_store
        .decide_at(
            project.clone(),
            TrustDecision::Trusted,
            Some(project.join(".git")),
            1,
        )
        .unwrap();
    service.set_project_trust_store(trust_store, None);
    service.set_pane_current_working_directory("%1".to_string(), project.clone());
    service.set_pane_environment_signature_for_tests(
        "%1",
        toolchain_environment(vec![format!("jdk-runtime:{}", jdk.display())]),
    );

    for kind in ["maven", "gradle"] {
        let detect = service.dispatch_runtime_control_body(
            &format!(
                r#"{{"jsonrpc":"2.0","id":"{kind}-detect","method":"agent/shell/command","params":{{"idempotency_key":"{kind}-detect","input":"/sandbox toolchains detect {kind}"}}}}"#
            ),
            &primary,
        );
        assert!(detect.contains(r#""presentation":"pager""#), "{detect}");
        assert!(detect.contains(&format!("| Kind | `{kind}` |")), "{detect}");
        assert!(detect.contains("| Available | yes |"), "{detect}");
        assert!(
            detect.contains(&project.to_string_lossy().into_owned()),
            "{detect}"
        );
    }

    let enabled = service
        .execute_agent_shell_command(
            &primary,
            "/sandbox toolchains enable jdk maven gradle --yes",
        )
        .unwrap();
    assert!(enabled.contains(r#""presentation":"notice""#), "{enabled}");
    assert!(enabled.contains("changed=true"), "{enabled}");
    let persisted = fs::read_to_string(&path).unwrap();
    assert!(
        persisted.contains("toolchains = [\"jdk\", \"maven\", \"gradle\"]"),
        "{persisted}"
    );
    for root in [&jdk, &project] {
        assert!(
            !persisted.contains(&root.to_string_lossy().into_owned()),
            "{persisted}"
        );
    }

    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// Verifies `/sandbox toolchains reload` invokes the full disk-backed config reload and
/// reports before/after typed state rather than applying only one field.
#[test]
fn runtime_agent_shell_toolchain_reload_reapplies_full_disk_config() {
    let config = "[history]\nlines = 7\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = []\n";
    let (mut service, primary, path) =
        toolchain_command_service("runtime-toolchain-reload", config);
    fs::write(
        &path,
        "[history]\nlines = 13\n[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = [\"rust\"]\n",
    )
    .unwrap();

    let reload = service
        .execute_agent_shell_command(&primary, "/sandbox toolchains reload")
        .unwrap();

    assert!(reload.contains(r#""presentation":"notice""#), "{reload}");
    assert!(
        reload.contains("Reloaded the full configuration"),
        "{reload}"
    );
    assert!(
        reload.contains("changes apply to subsequent actions"),
        "{reload}"
    );
    assert_eq!(service.terminal_history_limit(), 13);
    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// Verifies the runtime executor defensively rejects a non-primary caller even
/// when the caller bypasses the ordinary JSON-RPC authorization boundary.
#[test]
fn runtime_agent_shell_toolchain_rejects_non_primary_client() {
    let config =
        "[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = []\n";
    let (mut service, _primary, path) =
        toolchain_command_service("runtime-toolchain-non-primary", config);
    let non_primary = mez_core::ids::ClientId::opaque("c-observer").unwrap();

    let error = service
        .execute_agent_shell_command(&non_primary, "/sandbox toolchains status")
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
    assert!(error.message().contains("primary client"), "{error}");
    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// Verifies an authenticated automation client may queue one exact mutation,
/// but cannot apply it; only matching direct primary input settles it once.
#[test]
fn runtime_toolchain_mutation_submission_requires_exact_primary_settlement() {
    let config =
        "[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = []\n";
    let (mut service, primary, path) =
        toolchain_command_service("runtime-toolchain-pending-settlement", config);
    service.set_pane_environment_signature_for_tests(
        "%1",
        toolchain_environment(vec![
            "cargo-bin:/home/test-user/.cargo/bin".to_string(),
            "rustup:/home/test-user/.rustup".to_string(),
        ]),
    );
    let digest = crate::runtime::normalized_toolchain_mutation_digest(
        "enable",
        crate::runtime::SandboxToolchainKind::Rust,
    );
    let mut connection = ControlConnectionState::new(true, false);
    let initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"automation-init","method":"control/initialize","params":{"client_name":"toolchain-cli","requested_version":1,"requested_role":"automation","client":{"name":"toolchain-cli","interactive":false}}}"#,
    );
    let submit = encode_control_body(&format!(
        r#"{{"jsonrpc":"2.0","id":"submit","method":"toolchain/mutation/submit","params":{{"operation":"enable","kind":"rust","request_digest":"{digest}","idempotency_key":"submit-rust"}}}}"#,
    ));
    let mut input = initialize;
    input.extend_from_slice(&submit);

    let (output, consumed) = service
        .handle_control_input_for_connection(&input, 16 * 1024, &mut connection)
        .unwrap();
    let (_, initialized_bytes) = decode_control_frame(&output, 16 * 1024).unwrap();
    let (submitted, _) = decode_control_frame(&output[initialized_bytes..], 16 * 1024).unwrap();
    assert_eq!(consumed, input.len());
    assert!(submitted.contains(r#""state":"pending""#), "{submitted}");
    assert!(submitted.contains(r#""operation":"enable""#), "{submitted}");
    assert!(submitted.contains(r#""selectors":["rust"]"#), "{submitted}");
    assert!(!fs::read_to_string(&path).unwrap().contains("\"rust\""));

    let submitted_json: serde_json::Value = serde_json::from_str(&submitted).unwrap();
    let request_id = submitted_json["result"]["request_id"]
        .as_str()
        .unwrap()
        .to_string();
    let tampered = service
        .execute_agent_shell_command(
            &primary,
            &format!(
                "/sandbox toolchains confirm {request_id} {} --yes",
                "0".repeat(64)
            ),
        )
        .unwrap();
    assert!(tampered.contains("digest does not match"), "{tampered}");
    assert!(!fs::read_to_string(&path).unwrap().contains("\"rust\""));

    let confirmed = service
        .execute_agent_shell_command(
            &primary,
            &format!("/sandbox toolchains confirm {request_id} {digest} --yes"),
        )
        .unwrap();
    assert!(confirmed.contains("Enabled rust; updated"), "{confirmed}");
    assert!(
        fs::read_to_string(&path)
            .unwrap()
            .contains("toolchains = [\"rust\"]")
    );

    let replay = service
        .execute_agent_shell_command(
            &primary,
            &format!("/sandbox toolchains confirm {request_id} {digest} --yes"),
        )
        .unwrap();
    assert!(replay.contains("missing or already settled"), "{replay}");
    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// Verifies a typed custom definition submitted by automation remains pending
/// until matching authenticated primary input applies the exact definition.
#[test]
fn runtime_custom_toolchain_definition_requires_exact_primary_settlement() {
    let custom_root = temp_root("runtime-custom-toolchain-root").join("acme-sdk");
    fs::create_dir_all(custom_root.join("bin")).unwrap();
    fs::write(custom_root.join("bin/acme"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(
        custom_root.join("bin/acme"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let custom_root = custom_root.canonicalize().unwrap();
    let config = format!(
        "version = 32\n[permissions]\nsandbox = \"bubblewrap\"\nread_scopes = [\"{}\"]\n[permissions.bubblewrap]\ntoolchains = []\n",
        custom_root.display(),
    );
    let (mut service, primary, path) =
        toolchain_command_service("runtime-custom-toolchain-settlement", &config);
    service.set_pane_environment_signature_for_tests("%1", toolchain_environment(Vec::new()));
    let authority_request = service
        .primary_path_resolution_request("%1")
        .unwrap()
        .unwrap();
    let authority_command = mez_agent::shell::pane_path_resolution_command(
        &authority_request,
        mez_agent::ShellClassification::PosixSh,
    )
    .unwrap();
    let authority_output = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(authority_command)
        .current_dir(&custom_root)
        .output()
        .unwrap();
    assert!(authority_output.status.success(), "{authority_output:?}");
    let authority_key = service
        .path_resolution_cache_key("%1", &authority_request)
        .unwrap();
    service
        .observe_path_resolution_transaction_end(
            "custom-toolchain-authority",
            "%1",
            0,
            authority_key,
            &String::from_utf8(authority_output.stdout).unwrap(),
            false,
        )
        .unwrap();
    let name = crate::runtime::CustomToolchainName::parse("acme").unwrap();
    let definition = crate::runtime::CustomToolchainDefinition::new(
        Some("Acme SDK".to_string()),
        vec![custom_root.to_string_lossy().into_owned()],
        vec!["0:bin".to_string()],
        vec!["0:bin/acme".to_string()],
        std::collections::BTreeMap::from([("ACME_HOME".to_string(), "0:.".to_string())]),
    )
    .unwrap();
    let digest = crate::runtime::normalized_custom_toolchain_mutation_digest(
        "define",
        &name,
        Some(&definition),
        false,
    );
    let mut connection = ControlConnectionState::new(true, false);
    let initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"automation-init","method":"control/initialize","params":{"client_name":"toolchain-cli","requested_version":1,"requested_role":"automation","client":{"name":"toolchain-cli","interactive":false}}}"#,
    );
    let submit = encode_control_body(
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": "submit-custom",
            "method": "toolchain/mutation/submit",
            "params": {
                "operation": "define",
                "name": "acme",
                "description": "Acme SDK",
                "roots": [custom_root],
                "path_entries": ["0:bin"],
                "required_executables": ["0:bin/acme"],
                "environment": {"ACME_HOME": "0:."},
                "request_digest": digest,
                "idempotency_key": "submit-custom-acme",
            },
        })
        .to_string(),
    );
    let mut input = initialize;
    input.extend_from_slice(&submit);

    let (output, consumed) = service
        .handle_control_input_for_connection(&input, 16 * 1024, &mut connection)
        .unwrap();
    let (_, initialized_bytes) = decode_control_frame(&output, 16 * 1024).unwrap();
    let (submitted, _) = decode_control_frame(&output[initialized_bytes..], 16 * 1024).unwrap();
    assert_eq!(consumed, input.len());
    assert!(submitted.contains(r#""state":"pending""#), "{submitted}");
    assert!(submitted.contains(r#""operation":"define""#), "{submitted}");
    assert!(
        !fs::read_to_string(&path)
            .unwrap()
            .contains("custom_toolchains")
    );

    let submitted_json: serde_json::Value = serde_json::from_str(&submitted).unwrap();
    let request_id = submitted_json["result"]["request_id"].as_str().unwrap();
    let confirmed = service
        .execute_agent_shell_command(
            &primary,
            &format!("/sandbox toolchains confirm {request_id} {digest} --yes"),
        )
        .unwrap();
    assert!(confirmed.contains("Defined custom:acme"), "{confirmed}");
    let persisted = fs::read_to_string(&path).unwrap();
    assert!(persisted.contains("[permissions.bubblewrap.custom_toolchains.acme]"));
    assert!(persisted.contains(&format!("roots = [\"{}\"]", custom_root.display())));
    assert!(persisted.contains("ACME_HOME = \"0:.\""));
    let _ = fs::remove_dir_all(path.parent().unwrap());
    let _ = fs::remove_dir_all(custom_root.parent().unwrap());
}

/// Verifies pending mutation settlement fails closed after configuration
/// generation changes, and submission itself fails without a live primary.
#[test]
fn runtime_toolchain_mutation_submission_rejects_stale_and_absent_primary() {
    let config =
        "[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = []\n";
    let (mut service, primary, path) =
        toolchain_command_service("runtime-toolchain-pending-stale", config);
    service.set_pane_environment_signature_for_tests(
        "%1",
        toolchain_environment(vec![
            "cargo-bin:/home/test-user/.cargo/bin".to_string(),
            "rustup:/home/test-user/.rustup".to_string(),
        ]),
    );
    let digest = crate::runtime::normalized_toolchain_mutation_digest(
        "disable",
        crate::runtime::SandboxToolchainKind::Rust,
    );
    let submitted = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"submit-stale","method":"toolchain/mutation/submit","params":{{"operation":"disable","kind":"rust","request_digest":"{digest}","idempotency_key":"submit-stale"}}}}"#,
        ),
        &primary,
    );
    assert!(submitted.contains(r#""state":"pending""#), "{submitted}");
    let submitted_json: serde_json::Value = serde_json::from_str(&submitted).unwrap();
    let request_id = submitted_json["result"]["request_id"]
        .as_str()
        .unwrap()
        .to_string();

    let changed = service
        .execute_agent_shell_command(&primary, "/sandbox toolchains enable rust --yes")
        .unwrap();
    assert!(changed.contains("Enabled rust; updated"), "{changed}");
    let stale = service
        .execute_agent_shell_command(
            &primary,
            &format!("/sandbox toolchains confirm {request_id} {digest} --yes"),
        )
        .unwrap();
    assert!(stale.contains("configuration changed"), "{stale}");
    assert!(
        fs::read_to_string(&path)
            .unwrap()
            .contains("toolchains = [\"rust\"]")
    );
    let _ = fs::remove_dir_all(path.parent().unwrap());

    let mut detached = test_runtime_service();
    let mut connection = ControlConnectionState::new(true, false);
    let initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"automation-init","method":"control/initialize","params":{"client_name":"toolchain-cli","requested_version":1,"requested_role":"automation","client":{"name":"toolchain-cli","interactive":false}}}"#,
    );
    let submit = encode_control_body(&format!(
        r#"{{"jsonrpc":"2.0","id":"absent-primary","method":"toolchain/mutation/submit","params":{{"operation":"disable","kind":"rust","request_digest":"{digest}","idempotency_key":"absent-primary"}}}}"#,
    ));
    let mut input = initialize;
    input.extend_from_slice(&submit);
    let (output, _) = detached
        .handle_control_input_for_connection(&input, 16 * 1024, &mut connection)
        .unwrap();
    let (_, initialized_bytes) = decode_control_frame(&output, 16 * 1024).unwrap();
    let (absent, _) = decode_control_frame(&output[initialized_bytes..], 16 * 1024).unwrap();
    assert!(
        absent.contains(r#""mezzanine_code":"invalid_state""#),
        "{absent}"
    );
    assert!(absent.contains("attached primary client"), "{absent}");
}

/// Verifies durable toolchain audit records retain typed operation and
/// generation metadata without persisting bootstrap-derived host roots.
#[test]
fn runtime_agent_shell_toolchain_audit_redacts_discovered_roots() {
    let config =
        "[permissions]\nsandbox = \"bubblewrap\"\n[permissions.bubblewrap]\ntoolchains = []\n";
    let (mut service, primary, path) = toolchain_command_service("runtime-toolchain-audit", config);
    let audit_path = path.parent().unwrap().join("audit.jsonl");
    service.set_audit_log(AuditLog::new(crate::security::audit::AuditConfig {
        enabled: true,
        path: audit_path.clone(),
        hash_chain: false,
        required: true,
    }));
    service.set_pane_environment_signature_for_tests(
        "%1",
        toolchain_environment(vec![
            "cargo-bin:/private/toolchains/.cargo/bin".to_string(),
            "rustup:/private/toolchains/.rustup".to_string(),
        ]),
    );

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"toolchain-audit","method":"agent/shell/command","params":{"idempotency_key":"toolchain-audit","input":"/sandbox toolchains detect rust"}}"#,
        &primary,
    );

    assert!(response.contains(r#""presentation":"pager""#), "{response}");
    assert!(response.contains("| Available | yes |"), "{response}");
    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(audit.contains(r#""event_type":"toolchain""#), "{audit}");
    assert!(audit.contains(r#""action":"detect""#), "{audit}");
    assert!(audit.contains(r#""kind":"rust""#), "{audit}");
    assert!(audit.contains("config_generation"), "{audit}");
    assert!(!audit.contains("/private/toolchains"), "{audit}");
    assert!(!audit.contains("cargo_bin"), "{audit}");
    assert!(!audit.contains("rustup_home"), "{audit}");
    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// Verifies that the runtime `agent/shell/command` `/list-mcp` path uses the live
/// MCP registry and exposes unavailable or session-blacklisted details. This
/// protects the spec requirement that agent-shell MCP visibility match control
/// and command surfaces instead of returning a generic runtime placeholder.
#[test]
fn runtime_agent_shell_mcp_command_reports_live_registry_detail() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .mcp_registry_mut()
        .add_server(mez_agent::mcp::McpServerConfig::stdio(
            "fs",
            "filesystem",
            "mcp-fs",
            Vec::new(),
        ))
        .unwrap();
    service
        .mcp_registry_mut()
        .mark_available(
            "fs",
            vec![mez_agent::mcp::McpToolState {
                server_id: String::new(),
                name: "read_file".to_string(),
                available: true,
                blacklisted: false,
                permission_required: true,
                effects: mez_agent::mcp::McpToolEffects::none(),
                approval: mez_agent::mcp::McpApprovalSetting::Inherit,
                description: "read a file".to_string(),
                input_schema_json: "{}".to_string(),
            }],
            1,
        )
        .unwrap();
    service
        .mcp_registry_mut()
        .blacklist_for_session("fs", "failed handshake", 1)
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-mcp","method":"agent/shell/command","params":{"idempotency_key":"agent-mcp","input":"/list-mcp"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(response.contains(r#""command":"list-mcp""#), "{response}");
    assert!(response.contains("## MCP Servers"), "{response}");
    assert!(response.contains("Servers: 1"), "{response}");
    assert!(response.contains("Tools: 1"), "{response}");
    assert!(response.contains("Source: runtime-mcp"), "{response}");
    assert!(response.contains("### `fs` - filesystem"), "{response}");
    assert!(response.contains("- State: blacklisted"), "{response}");
    assert!(
        response.contains("- Session blacklisted: true"),
        "{response}"
    );
    assert!(response.contains("- Retryable: true"), "{response}");
    assert!(
        response.contains("- Reason: failed handshake"),
        "{response}"
    );
    assert!(
        response.contains("| `read_file` | blacklisted |"),
        "{response}"
    );
    assert!(!response.contains("requires_runtime"), "{response}");
}

/// Verifies that `/status` is backed by live runtime state rather than only
/// the shell session fallback. The status view is a user-visible conformance
/// surface, so it must include model selection, policy, identity, writable
/// scope state, current context tracking, and provider token counters in one
/// response.
#[test]
fn runtime_agent_shell_status_reports_live_runtime_state() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-fast\"]\ndefault_model = \"gpt-fast\"\n\n[permissions]\npreset = \"auto\"\napproval_policy = \"full-access\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let second_pane = service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some("cat >/dev/null"))
        .unwrap()
        .pane_id;
    service.session.select_pane(&primary, "%1").unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume(second_pane.as_str())
        .unwrap();
    service.record_agent_provider_token_usage(
        "%1",
        mez_agent::ModelTokenUsage {
            input_tokens: 120,
            output_tokens: 34,
            reasoning_tokens: 9,
            cached_input_tokens: Some(80),
            cache_write_input_tokens: None,
        },
    );
    service.record_agent_provider_token_usage(
        "%1",
        mez_agent::ModelTokenUsage {
            input_tokens: 40,
            output_tokens: 0,
            reasoning_tokens: 0,
            cached_input_tokens: None,
            cache_write_input_tokens: None,
        },
    );
    let deepseek_profile = runtime_model_profile("deepseek", "deepseek-chat");
    service.record_agent_provider_token_usage_with_profile(
        "%1",
        mez_agent::ModelTokenUsage {
            input_tokens: 200,
            output_tokens: 50,
            reasoning_tokens: 20,
            cached_input_tokens: Some(100),
            cache_write_input_tokens: None,
        },
        mez_agent::ModelTokenUsage {
            input_tokens: 200,
            output_tokens: 50,
            reasoning_tokens: 20,
            cached_input_tokens: Some(100),
            cache_write_input_tokens: None,
        },
        Some(&deepseek_profile),
    );
    service.record_agent_provider_token_usage(
        second_pane.as_str(),
        mez_agent::ModelTokenUsage {
            input_tokens: 60,
            output_tokens: 10,
            reasoning_tokens: 4,
            cached_input_tokens: Some(30),
            cache_write_input_tokens: None,
        },
    );
    service
        .integration
        .runtime_metrics_mut()
        .record_provider_token_usage(
            mez_agent::ModelTokenUsage {
                input_tokens: 300,
                output_tokens: 75,
                reasoning_tokens: 15,
                cached_input_tokens: Some(120),
                cache_write_input_tokens: None,
            },
            mez_agent::ModelTokenUsage {
                input_tokens: 300,
                output_tokens: 75,
                reasoning_tokens: 15,
                cached_input_tokens: Some(120),
                cache_write_input_tokens: None,
            },
            &mez_agent::ModelTokenUsageKey::new("runtime-metrics", "metrics-only"),
        );
    service
        .register_subagent_write_scopes_for_tests(
            "agent-%1",
            CooperationMode::OwnedWrite,
            &["src".to_string()],
            None,
        )
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "summarize the pane")
        .unwrap();
    assert_eq!(started.state, AgentTurnState::Running);

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-status","method":"agent/shell/command","params":{"idempotency_key":"agent-status","input":"/status"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(response.contains(r#""command":"status""#), "{response}");
    assert!(
        response.contains(r#""content_type":"text/markdown; charset=utf-8""#),
        "{response}"
    );
    assert!(response.contains("## Agent Status"), "{response}");
    assert!(response.contains("| Field | Value |"), "{response}");
    assert!(response.contains("| Agent id | agent-%1 |"), "{response}");
    assert!(response.contains("| Window id | @1 |"), "{response}");
    assert!(
        response.contains("| Model | gpt-fast via openai (profile: default"),
        "{response}"
    );
    assert!(
        response.contains("| Prompt profile | default v32 |"),
        "{response}"
    );
    assert!(
        response.contains(
            "| Permissions | preset auto (session-config; owner none), approval full-access (session-config; owner none), bypass false (session) |"
        ),
        "{response}"
    );
    assert!(
        response.contains("| src | agent-%1 | owned-write |"),
        "{response}"
    );
    assert!(response.contains("| Context | 1 blocks"), "{response}");
    assert!(
        response.contains("| Pane agent tokens | 2 models; see Pane Agent Token Usage |"),
        "{response}"
    );
    assert!(
        response.contains("### Pane Agent Token Usage"),
        "{response}"
    );
    assert!(
        response.contains("| Cumulative cache hit | unknown |"),
        "{response}"
    );
    assert!(
        response.contains(
            "| Latest request cache hit | 50.00% (deepseek-chat via deepseek; cached_input=100 input=200) |"
        ),
        "{response}"
    );
    assert!(
        response.contains("| Cumulative Cache Hit % |"),
        "{response}"
    );
    let session_heading = response
        .find("### Pane Agent Token Usage")
        .expect("session token usage heading should be present");
    let instance_heading = response
        .find("### Mez Session Token Usage")
        .expect("instance token usage heading should be present");
    assert!(session_heading < instance_heading, "{response}");
    assert!(
        response.contains("| openai | gpt-fast | 160 | unknown | 34 | 9 | unknown |"),
        "{response}"
    );
    assert!(
        response.contains("| deepseek | deepseek-chat | 100 | 100 | 50 | 20 | 50.00% |"),
        "{response}"
    );
    assert!(
        response.contains("| openai | gpt-fast | 220 | unknown | 44 | 13 | unknown |"),
        "{response}"
    );
    assert!(
        !response.contains("| runtime-metrics | metrics-only |"),
        "{response}"
    );
    assert!(!response.contains("Provider rate limits"), "{response}");
    assert!(!response.contains("### Quota Usage"), "{response}");
    assert!(
        response.contains("| Latest turn | turn-1 (running) |"),
        "{response}"
    );
    assert!(!response.contains("requires_runtime"), "{response}");

    let session_usage_before_reset = service.total_agent_token_usage_by_model();
    let second_pane_usage_before_reset = service.agent_token_usage_for_pane(second_pane.as_str());
    let reset_response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-reset-status","method":"agent/shell/command","params":{"idempotency_key":"agent-reset-status","input":"/reset-status"}}"#,
        &primary,
    );

    assert!(
        reset_response.contains(r#""kind":"mutated""#),
        "{reset_response}"
    );
    assert!(
        reset_response.contains(r#""command":"reset-status""#),
        "{reset_response}"
    );
    assert!(
        reset_response.contains("pane_token_usage_reset=true changed=true"),
        "{reset_response}"
    );
    assert!(service.agent_token_usage_for_pane("%1").is_empty());
    assert_eq!(
        service.agent_token_usage_for_pane(second_pane.as_str()),
        second_pane_usage_before_reset
    );
    assert_eq!(
        service.total_agent_token_usage_by_model(),
        session_usage_before_reset
    );
}

/// Verifies that `/init` creates a project instruction scaffold in the active
/// pane's working directory and leaves an existing scaffold intact. This covers
/// the baseline file-mutation slash command without writing to the repository
/// root used by the test harness.
#[test]
fn runtime_agent_shell_init_creates_project_instruction_scaffold() {
    let root = temp_root("runtime-agent-init");
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let descriptor = service.initial_pane_descriptor().unwrap();
    service
        .start_pane_process_with_start_directory(descriptor, Some("sleep 30"), Some(&root))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-init","method":"agent/shell/command","params":{"idempotency_key":"agent-init","input":"/init"}}"#,
        &primary,
    );

    let scaffold = root.join("AGENTS.md");
    assert!(response.contains(r#""kind":"mutated""#), "{response}");
    assert!(response.contains(r#""command":"init""#), "{response}");
    assert!(response.contains("created=true"), "{response}");
    assert!(response.contains("source=runtime-init"), "{response}");
    assert!(!response.contains("requires_runtime"), "{response}");
    let text = fs::read_to_string(&scaffold).unwrap();
    assert!(text.contains("# Repository Guidelines"), "{text}");
    assert!(
        text.contains("## Build, Test, and Development Commands"),
        "{text}"
    );

    let existing = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-init-existing","method":"agent/shell/command","params":{"idempotency_key":"agent-init-existing","input":"/init"}}"#,
        &primary,
    );

    assert!(existing.contains(r#""kind":"display""#), "{existing}");
    assert!(existing.contains(r#""command":"init""#), "{existing}");
    assert!(existing.contains("created=false"), "{existing}");
    assert!(existing.contains("existing=true"), "{existing}");
    assert!(!existing.contains("requires_runtime"), "{existing}");
    service.kill_session(&primary, true).unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies `/auth-status` renders one ordered, secret-safe row for every
/// configured provider, including providers without stored credentials.
#[test]
fn runtime_agent_shell_auth_status_lists_configured_provider_rows() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.deepseek]\nkind = \"deepseek\"\nmodels = [\"deepseek-v4-pro\"]\ndefault_model = \"deepseek-v4-pro\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\"]\ndefault_model = \"gpt-5.5\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-agent-auth-status-table");
    let auth_store = AuthStore::new(crate::security::auth::AuthPaths::under_config_root(&root));
    auth_store
        .login_provider_api_key_with_selected_store(
            "openai",
            "work",
            "sk-runtime-secret",
            Some("file"),
        )
        .unwrap();
    service.set_auth_store(auth_store);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let status = service
        .execute_agent_shell_command(&primary, "/auth-status")
        .unwrap();

    assert!(status.contains("## Authentication Status"), "{status}");
    assert!(
        status.contains("| Provider | Authenticated | Profile | Credential store | State |"),
        "{status}"
    );
    assert!(
        status.contains("| deepseek | false | none | none | logged-out |"),
        "{status}"
    );
    assert!(
        status.contains("| openai | true | work | file | available |"),
        "{status}"
    );
    assert!(
        status.find("| deepseek |").unwrap() < status.find("| openai |").unwrap(),
        "{status}"
    );
    assert!(!status.contains("sk-runtime-secret"), "{status}");

    let _ = fs::remove_dir_all(root);
}

/// Verifies `/auth-status` retains every configured provider row when no auth
/// store is attached, making unavailable credential storage explicit instead
/// of omitting configured providers or selecting an unrelated default status.
#[test]
fn runtime_agent_shell_auth_status_marks_unavailable_auth_store_per_provider() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.deepseek]\nkind = \"deepseek\"\nmodels = [\"deepseek-v4-pro\"]\ndefault_model = \"deepseek-v4-pro\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\"]\ndefault_model = \"gpt-5.5\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let status = service
        .execute_agent_shell_command(&primary, "/auth-status")
        .unwrap();

    assert!(
        status.contains("| deepseek | unknown | none | unavailable | auth-store-unavailable |"),
        "{status}"
    );
    assert!(
        status.contains("| openai | unknown | none | unavailable | auth-store-unavailable |"),
        "{status}"
    );
}

/// Verifies `/approval` changes only the issuing pane while preserving the
/// configured session policy as the baseline for unrelated panes.
#[test]
fn runtime_agent_shell_approval_command_mutates_live_policy() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-permissions","method":"agent/shell/command","params":{"idempotency_key":"agent-permissions","input":"/approval full-access"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"mutated""#), "{response}");
    assert!(response.contains(r#""command":"approval""#), "{response}");
    assert!(response.contains("field=approval_policy"), "{response}");
    assert!(response.contains("requested=full-access"), "{response}");
    assert!(response.contains("changed=true"), "{response}");
    assert!(!response.contains("requires_runtime"), "{response}");
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::Ask
    );
    assert_eq!(
        service.permission_policy_for_pane("%1").approval_policy,
        ApprovalPolicy::FullAccess
    );
}

/// Verifies sandbox enable and disable default to one exact pane, leave the
/// persisted global backend and generation unchanged, report provenance, and
/// discard the override when the pane's runtime state is removed.
#[test]
fn runtime_agent_shell_sandbox_mutations_are_pane_local_by_default() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let second_pane = service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some("cat >/dev/null"))
        .unwrap()
        .pane_id;
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume(second_pane.as_str())
        .unwrap();
    service.session.select_pane(&primary, "%1").unwrap();
    let generation = service.session.config_generation;

    let enabled = service
        .execute_agent_shell_command(&primary, "/sandbox enable --yes")
        .unwrap();
    assert!(enabled.contains(r#""command":"sandbox""#), "{enabled}");
    assert!(enabled.contains("scope=pane"), "{enabled}");
    assert!(matches!(
        service.sandbox_config_for_pane("%1"),
        SandboxConfig::Bubblewrap(_)
    ));
    assert!(matches!(
        service.sandbox_config_for_pane(second_pane.as_str()),
        SandboxConfig::PolicyOnly
    ));
    assert!(matches!(
        service.configured_permissions().sandbox,
        SandboxConfig::PolicyOnly
    ));
    assert_eq!(service.session.config_generation, generation);

    let status = service
        .execute_agent_shell_command(&primary, "/sandbox status")
        .unwrap();
    assert!(
        status.contains("Effective backend | `bubblewrap`"),
        "{status}"
    );
    assert!(status.contains("Source | pane override"), "{status}");
    assert_eq!(service.session.config_generation, generation);

    service.cleanup_removed_pane_runtime_state("%1").unwrap();
    assert!(matches!(
        service.sandbox_config_for_pane("%1"),
        SandboxConfig::PolicyOnly
    ));
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies `--global` persists and hot-applies the default backend while an
/// exact pane override continues to win over later global changes.
#[test]
fn runtime_agent_shell_sandbox_global_mutation_preserves_pane_override() {
    let config = "[permissions]\nsandbox = \"policy-only\"\n";
    let (mut service, primary, path) = toolchain_command_service("runtime-sandbox-global", config);
    let initial_generation = service.session.config_generation;

    let enabled = service
        .execute_agent_shell_command(&primary, "/sandbox enable --global --yes")
        .unwrap();
    assert!(enabled.contains("scope=global"), "{enabled}");
    assert!(matches!(
        service.configured_permissions().sandbox,
        SandboxConfig::Bubblewrap(_)
    ));
    assert_eq!(service.session.config_generation, initial_generation + 1);
    assert!(
        fs::read_to_string(&path)
            .unwrap()
            .contains("sandbox = \"bubblewrap\"")
    );

    service
        .execute_agent_shell_command(&primary, "/sandbox disable --yes")
        .unwrap();
    let global_status = service
        .execute_agent_shell_command(&primary, "/sandbox status --global")
        .unwrap();
    assert!(
        global_status.contains("Backend | `bubblewrap`"),
        "{global_status}"
    );
    assert!(matches!(
        service.sandbox_config_for_pane("%1"),
        SandboxConfig::PolicyOnly
    ));
    assert!(service.pane_has_sandbox_override("%1"));
    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// Verifies pane-local permission commands do not leak preset or approval
/// changes into an unrelated root pane or the configured session baseline.
#[test]
fn runtime_agent_shell_permission_commands_isolate_root_panes() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let second_pane = service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some("cat >/dev/null"))
        .unwrap()
        .pane_id;
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume(second_pane.as_str())
        .unwrap();
    service.session.select_pane(&primary, "%1").unwrap();

    let approval = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"pane-approval","method":"agent/shell/command","params":{"idempotency_key":"pane-approval","input":"/approval full-access"}}"#,
        &primary,
    );
    let preset = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"pane-preset","method":"agent/shell/command","params":{"idempotency_key":"pane-preset","input":"/permissions preset auto"}}"#,
        &primary,
    );

    assert!(approval.contains("changed=true"), "{approval}");
    assert!(preset.contains("changed=true"), "{preset}");
    assert_eq!(
        service.permission_policy_for_pane("%1").approval_policy,
        ApprovalPolicy::FullAccess
    );
    assert_eq!(
        service.permission_policy_for_pane("%1").preset,
        mez_agent::PermissionPreset::Auto
    );
    assert_eq!(
        service
            .permission_policy_for_pane(second_pane.as_str())
            .approval_policy,
        ApprovalPolicy::Ask
    );
    assert_eq!(
        service
            .permission_policy_for_pane(second_pane.as_str())
            .preset,
        mez_agent::PermissionPreset::ReadOnly
    );
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::Ask
    );
    assert_eq!(
        service.permission_policy().preset,
        mez_agent::PermissionPreset::ReadOnly
    );
    let frame_context = service.terminal_frame_context();
    assert_eq!(
        frame_context
            .panes
            .get("%1")
            .and_then(|context| context.policy_mode.as_deref()),
        Some("full-access")
    );
    assert_eq!(
        frame_context
            .panes
            .get(second_pane.as_str())
            .and_then(|context| context.policy_mode.as_deref()),
        Some("ask")
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies active descendants resolve parent changes dynamically, child
/// field overrides shadow only their subtree, and clearing restores inheritance.
#[test]
fn runtime_pane_permission_overrides_inherit_and_shadow_by_field() {
    let mut service = test_runtime_service();
    service.set_subagent_lineage(
        "agent-%2",
        RuntimeSubagentLineage {
            parent_agent_id: "agent-%1".to_string(),
            root_agent_id: "agent-%1".to_string(),
            depth: 1,
            display_name: "child".to_string(),
        },
    );
    service.set_subagent_lineage(
        "agent-%3",
        RuntimeSubagentLineage {
            parent_agent_id: "agent-%2".to_string(),
            root_agent_id: "agent-%1".to_string(),
            depth: 2,
            display_name: "grandchild".to_string(),
        },
    );

    service.set_pane_permission_preset_override("%1", Some(mez_agent::PermissionPreset::Auto));
    service.set_pane_approval_policy_override("%1", Some(ApprovalPolicy::FullAccess));
    assert_eq!(
        service.permission_policy_for_agent("agent-%3").preset,
        mez_agent::PermissionPreset::Auto
    );
    assert_eq!(
        service
            .permission_policy_for_agent("agent-%3")
            .approval_policy,
        ApprovalPolicy::FullAccess
    );

    service.set_pane_approval_policy_override("%2", Some(ApprovalPolicy::Ask));
    let child = service.permission_policy_for_agent("agent-%2");
    let grandchild = service.permission_policy_for_agent("agent-%3");
    assert_eq!(child.preset, mez_agent::PermissionPreset::Auto);
    assert_eq!(child.approval_policy, ApprovalPolicy::Ask);
    assert_eq!(grandchild.preset, mez_agent::PermissionPreset::Auto);
    assert_eq!(grandchild.approval_policy, ApprovalPolicy::Ask);
    let grandchild_status = service.permission_policy_status_for_pane("%3");
    assert_eq!(
        grandchild_status.preset_source.source,
        "ancestor-pane-override"
    );
    assert_eq!(
        grandchild_status.preset_source.owner_pane_id.as_deref(),
        Some("%1")
    );
    assert_eq!(
        grandchild_status.approval_policy_source.source,
        "ancestor-pane-override"
    );
    assert_eq!(
        grandchild_status
            .approval_policy_source
            .owner_pane_id
            .as_deref(),
        Some("%2")
    );
    assert_eq!(
        service.permission_policy_for_pane("%1").approval_policy,
        ApprovalPolicy::FullAccess
    );

    service.set_pane_approval_policy_override("%2", None);
    assert_eq!(
        service
            .permission_policy_for_agent("agent-%3")
            .approval_policy,
        ApprovalPolicy::FullAccess
    );
    service.set_pane_permission_preset_override("%1", None);
    assert_eq!(
        service.permission_policy_for_agent("agent-%3").preset,
        mez_agent::PermissionPreset::ReadOnly
    );
    let grandchild_status = service.permission_policy_status_for_pane("%3");
    assert_eq!(grandchild_status.preset_source.source, "session-config");
    assert_eq!(grandchild_status.preset_source.owner_pane_id, None);
}

/// Verifies pane cleanup removes only that pane's explicit permission fields.
///
/// Closing one descendant must not erase an ancestor's independently owned
/// override or leave the removed pane carrying stale authority if its id is
/// queried before reuse.
#[test]
fn runtime_pane_permission_cleanup_isolated_to_removed_pane() {
    let mut service = test_runtime_service();
    service.set_pane_approval_policy_override("%1", Some(ApprovalPolicy::FullAccess));
    service.set_pane_approval_policy_override("%2", Some(ApprovalPolicy::HostAccess));

    service.cleanup_removed_pane_runtime_state("%2").unwrap();

    assert_eq!(
        service.permission_policy_for_pane("%1").approval_policy,
        ApprovalPolicy::FullAccess
    );
    assert_eq!(
        service.permission_policy_for_pane("%2").approval_policy,
        ApprovalPolicy::Ask
    );
}

/// Verifies pane permission slash commands can clear explicit fields and
/// restore dynamic inheritance from the configured session baseline.
#[test]
fn runtime_agent_shell_permission_commands_clear_to_inherit() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    for (id, input) in [
        ("set-approval", "/approval full-access"),
        ("set-preset", "/permissions preset auto"),
        ("clear-approval", "/approval inherit"),
        ("clear-preset", "/permissions preset clear"),
    ] {
        let response = service.dispatch_runtime_control_body(
            &format!(
                r#"{{"jsonrpc":"2.0","id":"{id}","method":"agent/shell/command","params":{{"idempotency_key":"{id}","input":"{input}"}}}}"#
            ),
            &primary,
        );
        assert!(response.contains("changed=true"), "{response}");
    }

    let policy = service.permission_policy_for_pane("%1");
    assert_eq!(policy.approval_policy, ApprovalPolicy::Ask);
    assert_eq!(policy.preset, mez_agent::PermissionPreset::ReadOnly);
}

/// Verifies a configured subagent profile preset remains a non-broadenable
/// restriction after pane-subtree policy composition.
#[test]
fn runtime_subagent_profile_preset_restricts_pane_override() {
    let mut service = test_runtime_service();
    service.set_subagent_lineage(
        "agent-%2",
        RuntimeSubagentLineage {
            parent_agent_id: "agent-%1".to_string(),
            root_agent_id: "agent-%1".to_string(),
            depth: 1,
            display_name: "child".to_string(),
        },
    );
    service.set_subagent_scope_declaration(
        "agent-%2",
        mez_agent::SubagentScopeDeclaration {
            cooperation_mode: CooperationMode::ExploreOnly,
            current_directory: "/repo".to_string(),
            read_scopes: vec!["/repo".to_string()],
            write_scopes: Vec::new(),
            permission_preset: Some(mez_agent::PermissionPreset::ReadOnly),
        },
    );
    service.set_pane_permission_preset_override("%1", Some(mez_agent::PermissionPreset::Auto));
    service.set_pane_permission_preset_override("%2", Some(mez_agent::PermissionPreset::Auto));
    let turn = mez_agent::AgentTurnRecord {
        turn_id: "profile-restriction".to_string(),
        conversation_id: "conversation-1".to_string(),
        agent_id: "agent-%2".to_string(),
        pane_id: "%2".to_string(),
        trigger: mez_agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 1,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        state: mez_agent::AgentTurnState::Running,
        cooperation_mode: Some("explore-only".to_string()),
        initial_capability: None,
    };

    assert_eq!(
        service.permission_policy_for_turn(&turn).preset,
        mez_agent::PermissionPreset::ReadOnly
    );
}

/// Verifies malformed cyclic delegation lineage fails closed instead of
/// looping or retaining a broader pane override.
#[test]
fn runtime_pane_permission_override_cycle_fails_closed() {
    let mut service = test_runtime_service();
    service.set_pane_permission_preset_override("%1", Some(mez_agent::PermissionPreset::Auto));
    service.set_pane_approval_policy_override("%1", Some(ApprovalPolicy::HostAccess));
    service.set_subagent_lineage(
        "agent-%1",
        RuntimeSubagentLineage {
            parent_agent_id: "agent-%2".to_string(),
            root_agent_id: "agent-%1".to_string(),
            depth: 1,
            display_name: "first".to_string(),
        },
    );
    service.set_subagent_lineage(
        "agent-%2",
        RuntimeSubagentLineage {
            parent_agent_id: "agent-%1".to_string(),
            root_agent_id: "agent-%1".to_string(),
            depth: 2,
            display_name: "second".to_string(),
        },
    );

    let policy = service.permission_policy_for_agent("agent-%2");
    assert_eq!(policy.preset, mez_agent::PermissionPreset::ReadOnly);
    assert_eq!(policy.approval_policy, ApprovalPolicy::Ask);
}

/// Verifies only the attached primary user's pane command can select host
/// access without broadening the configured session baseline.
#[test]
fn runtime_agent_shell_approval_command_selects_host_access() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-host-access","method":"agent/shell/command","params":{"idempotency_key":"agent-host-access","input":"/approval host-access"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"mutated""#), "{response}");
    assert!(response.contains("requested=host-access"), "{response}");
    assert!(
        response.contains("authority_change=broadening"),
        "{response}"
    );
    assert!(
        response.contains("approved_by=primary-command"),
        "{response}"
    );
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::Ask
    );
    assert_eq!(
        service.permission_policy_for_pane("%1").approval_policy,
        ApprovalPolicy::HostAccess
    );
}

/// Verifies terse slash-command display output uses transient status feedback.
///
/// One-line status acknowledgements should stay out of the durable agent pane
/// transcript while still giving brief feedback in the window status bar.
#[test]
fn runtime_agent_shell_single_line_display_uses_transient_status_without_overlay() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"/approval\r".to_vec(),
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    assert!(service.primary_display_overlay().is_none());
    assert!(
        service
            .primary_error_status_overlay()
            .is_some_and(|message| message.contains("approval policy: ask")),
        "{:?}",
        service.primary_error_status_overlay()
    );
    let pane_text = service
        .pane_screen("%1")
        .map(|screen| screen.normal_content_lines().join("\n"))
        .unwrap_or_default();
    assert!(!pane_text.contains("approval policy: ask"), "{pane_text}");
    assert!(!pane_text.contains("source: runtime-policy"), "{pane_text}");
}

/// Verifies an explicit pane `/approval` choice survives unrelated configured
/// baseline reloads without becoming a session-global override.
///
/// This protects full-access mode from being silently reset when a config
/// reload reapplies an older `permissions.approval_policy` value.
#[test]
fn runtime_agent_shell_approval_command_survives_config_reload() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-approval-live-override");
    let path = root.join("config.toml");
    fs::write(
        &path,
        "[history]\nlines = 7\n[permissions]\napproval_policy = \"ask\"\n",
    )
    .unwrap();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: Some(path.clone()),
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: fs::read_to_string(&path).unwrap(),
        }])
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-approval","method":"agent/shell/command","params":{"idempotency_key":"agent-approval-live","input":"/approval full-access"}}"#,
        &primary,
    );

    assert!(response.contains("requested=full-access"), "{response}");
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::Ask
    );
    assert_eq!(
        service.permission_policy_for_pane("%1").approval_policy,
        ApprovalPolicy::FullAccess
    );

    fs::write(
        &path,
        "[history]\nlines = 11\n[permissions]\napproval_policy = \"ask\"\n",
    )
    .unwrap();
    let reload = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"reload-approval","method":"config/reload","params":{"idempotency_key":"reload-approval-live"}}"#,
        &primary,
    );

    assert!(reload.contains(r#""operation":"reload""#), "{reload}");
    assert_eq!(service.terminal_history_limit(), 11);
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::Ask
    );
    assert_eq!(
        service.permission_policy_for_pane("%1").approval_policy,
        ApprovalPolicy::FullAccess
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies that the removed `/statusline` command is rejected without
/// mutating the live pane status-line rendering fields.
#[test]
fn runtime_agent_shell_statusline_is_rejected_without_mutating_pane_frame_fields() {
    let mut service = test_runtime_service();
    let expected_frame_fields = service.pane_frame_visible_fields().to_vec();
    let expected_frame_template = service.pane_frame_template().to_string();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-statusline","method":"agent/shell/command","params":{"idempotency_key":"agent-statusline","input":"/statusline agent.status agent.model pane.mode"}}"#,
        &primary,
    );

    assert!(response.contains("unknown slash command"), "{response}");
    assert!(!response.contains(r#""kind":"mutated""#), "{response}");
    assert!(service.pane_frames_enabled());
    assert_eq!(service.pane_frame_visible_fields(), expected_frame_fields);
    assert_eq!(service.pane_frame_template(), expected_frame_template);
}

/// Verifies that `/debug-config` reports live effective configuration, layer
/// order, and policy diagnostics from runtime state instead of the generic
/// runtime-required slash placeholder.
#[test]
fn runtime_agent_shell_debug_config_reports_live_runtime_config() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[history]\nlines = 7\n[permissions]\npreset = \"auto\"\napproval_policy = \"full-access\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"debug-config","method":"agent/shell/command","params":{"idempotency_key":"debug-config","input":"/debug-config history.lines"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(
        response.contains(r#""command":"debug-config""#),
        "{response}"
    );
    assert!(response.contains("source=runtime-config"), "{response}");
    assert!(response.contains("layers=1"), "{response}");
    assert!(response.contains("applied_layers=1"), "{response}");
    assert!(response.contains("permission_preset=auto"), "{response}");
    assert!(
        response.contains("approval_policy=full-access"),
        "{response}"
    );
    assert!(response.contains("layer=primary"), "{response}");
    assert!(response.contains("scope=primary"), "{response}");
    assert!(response.contains("format=toml"), "{response}");
    assert!(response.contains("value path=history.lines"), "{response}");
    assert!(response.contains("value=7"), "{response}");
    assert!(!response.contains("requires_runtime"), "{response}");
}

/// Verifies that planning-time shell action failures stay visible without
/// exposing the exact command in the default pane buffer. The user still sees
/// the policy failure, while command details remain reserved for verbose or
/// trace mode.
#[test]
fn runtime_agent_shell_planning_failure_hides_command_by_default() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 12).unwrap(), 20).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().add_rule(
        mez_agent::permissions::CommandRule::new(["ls"], RuleDecision::Forbid, RuleMatch::Prefix)
            .unwrap(),
    );

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-failed-command","input":"list files"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap shell response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "shell-1".to_string(),
                    rationale: "list files".to_string(),
                    payload: mez_agent::AgentActionPayload::ShellCommand {
                        summary: "List files".to_string(),
                        command: "ls".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert_eq!(execution.action_results[0].status, ActionStatus::Denied);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: List files (shell command denied before execution"),
        "{pane_text}"
    );
    assert!(!pane_text.contains("before execution: ls"), "{pane_text}");
    assert!(!pane_text.contains("$ ls"), "{pane_text}");
    service.terminate_all_pane_processes().unwrap();
}
