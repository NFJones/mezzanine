//! Standalone fixed-helper startup, distinct from full CLI/config initialization.
//!
//! Observation helpers must remain neutral outside Mez with no HOME/default
//! configuration. Exact routing and peer authorization stay inside the helper;
//! this fixture never installs a vendor or supplies daemon credentials.

/// Production Cursor bootstrap must create version1 only for an absent shared
/// document, preserve authored callbacks/metadata/CLI policy and leave previews
/// unchanged. Fixed shell commands execute a literal difficult helper pathname
/// and emit no context, env, permission or follow-up output. This is deployment
/// proof, not vendor loading, native association, token accounting or IDE parity.
#[test]
fn standalone_cursor_bootstrap_shared_members_and_commands_remain_neutral() {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!(
        "mez-cursor-install-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let helper = root.join("owned 'quote' $LITERAL; mez");
    std::fs::copy(env!("CARGO_BIN_EXE_mez"), &helper).unwrap();
    let home = root.join("home");
    std::fs::create_dir(&home).unwrap();
    let vendor = root.join("custom-cursor");
    let bootstrap = |flags: &[&str]| {
        let output = std::process::Command::new(&helper)
            .args(["--json", "bootstrap", "cursor"])
            .args(flags)
            .env_clear()
            .env("HOME", &home)
            .env("CURSOR_CONFIG_DIR", &vendor)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty());
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    assert_eq!(bootstrap(&["--dry-run"])["result"], "preview");
    assert!(!vendor.exists());
    assert!(!home.join(".config").exists());
    assert_eq!(bootstrap(&[])["result"], "installed");
    let file = vendor.join("hooks.json");
    let mut document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(document["version"], 1);
    document["authored"] = true.into();
    document["hooks"]["stop"].as_array_mut().unwrap().insert(
        0,
        serde_json::json!({"command":"AUTHORED COMMAND","matcher":".*","timeout":17}),
    );
    std::fs::write(&file, document.to_string()).unwrap();
    std::fs::write(vendor.join("cli-config.json"), b"PRIVATE USER POLICY").unwrap();
    let before = std::fs::read(&file).unwrap();
    assert_eq!(bootstrap(&["--dry-run"])["result"], "preview");
    assert_eq!(std::fs::read(&file).unwrap(), before);
    let repeated = bootstrap(&[]);
    assert_eq!(repeated["result"], "unchanged");
    assert_eq!(repeated["root_source"], "CURSOR_CONFIG_DIR");
    assert_eq!(repeated["lifecycle"], "unavailable-source-association");
    assert_eq!(repeated["usage"], "unavailable-source-association");
    for event in ["sessionStart", "stop", "sessionEnd"] {
        let hook = document["hooks"][event]
            .as_array()
            .unwrap()
            .iter()
            .find(|hook| hook["command"].as_str().unwrap().contains("cursor-hook"))
            .unwrap();
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", hook["command"].as_str().unwrap()])
            .env_clear()
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"PRIVATE INPUT NOT A SOURCE ID")
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"{}\n");
        assert!(output.stderr.is_empty());
    }
    assert_eq!(bootstrap(&["--uninstall"])["result"], "uninstalled");
    let document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(document["version"], 1);
    assert_eq!(document["authored"], true);
    assert_eq!(
        document["hooks"]["stop"],
        serde_json::json!([{"command":"AUTHORED COMMAND","matcher":".*","timeout":17}])
    );
    assert_eq!(
        std::fs::read(vendor.join("cli-config.json")).unwrap(),
        b"PRIVATE USER POLICY"
    );
    std::fs::remove_dir_all(root).unwrap();
}

/// Bare production Copilot bootstrap resolves COPILOT_HOME, creates only its
/// owned documented hook file, and leaves authored hook/settings/exporter data
/// untouched. Direct exec argv containing shell metacharacters is inert; every
/// helper outcome stays neutral without HOME/PATH/routing or session guessing.
/// Installation remains explicitly distinct from runtime/usage availability.
#[test]
fn standalone_copilot_bootstrap_and_direct_exec_are_neutral_and_preserving() {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!(
        "mez-copilot-install-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let helper = root.join("owned 'quote' $LITERAL; mez");
    std::fs::copy(env!("CARGO_BIN_EXE_mez"), &helper).unwrap();
    let home = root.join("home");
    std::fs::create_dir(&home).unwrap();
    let vendor = root.join("custom-copilot");
    let bootstrap = |flags: &[&str]| {
        let output = std::process::Command::new(&helper)
            .args(["--json", "bootstrap", "copilot"])
            .args(flags)
            .env_clear()
            .env("HOME", &home)
            .env("COPILOT_HOME", &vendor)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty());
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    assert_eq!(bootstrap(&["--dry-run"])["result"], "preview");
    assert!(!vendor.exists());
    assert!(!home.join(".config").exists());
    std::fs::create_dir_all(vendor.join("hooks")).unwrap();
    std::fs::write(
        vendor.join("settings.json"),
        b"{\"disableAllHooks\":true,\"authoredExporter\":true}",
    )
    .unwrap();
    std::fs::write(vendor.join("hooks/authored.json"), b"AUTHORED HOOK").unwrap();
    let installed = bootstrap(&[]);
    assert_eq!(installed["root_source"], "COPILOT_HOME");
    assert_eq!(installed["result"], "installed");
    assert_eq!(installed["lifecycle"], "unverified");
    assert_eq!(installed["usage"], "unavailable-safe-activation");
    assert_eq!(installed["manifest_revision"], 2);
    assert_eq!(bootstrap(&[])["result"], "unchanged");
    let hooks: serde_json::Value =
        serde_json::from_slice(&std::fs::read(vendor.join("hooks/mezzanine.json")).unwrap())
            .unwrap();
    for event in [
        "sessionStart",
        "userPromptSubmitted",
        "preToolUse",
        "postToolUse",
        "notification",
        "agentStop",
        "errorOccurred",
        "sessionEnd",
    ] {
        let hook = &hooks["hooks"][event][0];
        assert_eq!(hook["exec"], helper.to_str().unwrap());
        let arguments = hook["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect::<Vec<_>>();
        let mut child = std::process::Command::new(hook["exec"].as_str().unwrap())
            .args(arguments)
            .env_clear()
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"PRIVATE INPUT, NOT A SESSION ID OR VALID JSON")
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"{}\n");
        assert!(output.stderr.is_empty());
    }
    assert_eq!(bootstrap(&["--check"])["result"], "checked");
    assert_eq!(bootstrap(&["--uninstall"])["result"], "uninstalled");
    assert!(!vendor.join("hooks/mezzanine.json").exists());
    assert_eq!(
        std::fs::read(vendor.join("hooks/authored.json")).unwrap(),
        b"AUTHORED HOOK"
    );
    assert_eq!(
        std::fs::read(vendor.join("settings.json")).unwrap(),
        b"{\"disableAllHooks\":true,\"authoredExporter\":true}"
    );
    std::fs::remove_dir_all(root).unwrap();
}

/// Bare production bootstrap pins its actual executable even when the path
/// needs shell quoting. Authored hooks/disabled configuration survive, dry-run
/// writes no vendor root, repeats are unchanged, and the installed callback is
/// neutral with empty routing/PATH/launcher environment and after helper loss.
/// This tests deployment, not actual Codex loader trust, enrollment or billing.
#[test]
fn standalone_codex_bootstrap_pins_literal_helper_and_preserves_authored_hooks() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!(
        "mez-codex-deploy-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let helper = root.join("owned 'quote' $NOT_A_VARIABLE; mez");
    std::fs::copy(env!("CARGO_BIN_EXE_mez"), &helper).unwrap();
    let home = root.join("home");
    std::fs::create_dir(&home).unwrap();
    let bootstrap = |flags: &[&str]| {
        let result = std::process::Command::new(&helper)
            .args(["--json", "bootstrap", "codex"])
            .args(flags)
            .env_clear()
            .env("HOME", &home)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        assert!(result.stderr.is_empty(), "{result:?}");
        serde_json::from_slice::<serde_json::Value>(&result.stdout).unwrap()
    };
    assert_eq!(bootstrap(&["--dry-run"])["result"], "preview");
    let vendor = home.join(".codex");
    assert!(!vendor.exists());
    assert!(!home.join(".config").exists());
    std::fs::create_dir(&vendor).unwrap();
    let authored = serde_json::json!({"matcher":"authored","hooks":[{"type":"command","command":"user hook","timeout":7}]});
    std::fs::write(
        vendor.join("hooks.json"),
        serde_json::json!({"custom":"preserve","hooks":{"SessionStart":[authored]}}).to_string(),
    )
    .unwrap();
    std::fs::write(vendor.join("config.toml"), b"[features]\nhooks = false\n").unwrap();
    let installed = bootstrap(&[]);
    assert_eq!(installed["manifest_revision"], 4);
    assert_eq!(installed["result"], "installed");
    assert_eq!(installed["runtime_verification"], "not-performed");
    assert_eq!(bootstrap(&[])["result"], "unchanged");
    let document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(vendor.join("hooks.json")).unwrap()).unwrap();
    assert_eq!(document["custom"], "preserve");
    assert_eq!(document["hooks"]["SessionStart"][0], authored);
    assert_eq!(
        document["hooks"]["SessionStart"].as_array().unwrap().len(),
        2
    );
    assert_eq!(
        std::fs::read(vendor.join("config.toml")).unwrap(),
        b"[features]\nhooks = false\n"
    );
    let command = document["hooks"]["SessionStart"][1]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(!command.contains("MEZ_CODEX_HELPER"));
    for state in ["executable", "nonexecutable", "missing"] {
        if state == "nonexecutable" {
            std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o600)).unwrap();
        } else if state == "missing" {
            std::fs::remove_file(&helper).unwrap();
        }
        for (shell, flags) in [
            ("/bin/sh", vec!["-c"]),
            ("/usr/bin/bash", vec!["--noprofile", "--norc", "-c"]),
            ("/bin/zsh", vec!["-f", "-c"]),
            ("/usr/bin/zsh", vec!["-f", "-c"]),
            ("/usr/bin/fish", vec!["--no-config", "-c"]),
            ("/opt/homebrew/bin/fish", vec!["--no-config", "-c"]),
            ("/usr/local/bin/fish", vec!["--no-config", "-c"]),
        ] {
            if !std::path::Path::new(shell).is_file() {
                continue;
            }
            let result = std::process::Command::new(shell)
                .args(flags)
                .arg(command)
                .env_clear()
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap();
            assert!(result.status.success(), "{shell}/{state}: {result:?}");
            assert_eq!(result.stdout, b"{}\n", "{shell}/{state}");
            assert!(result.stderr.is_empty(), "{shell}/{state}: {result:?}");
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}

/// Malformed/child/compact inputs, missing pane hints and unavailable Unix routes
/// cannot leak vendor content or daemon errors. This actual binary never falls
/// back to a primary, inherited descriptor or default socket on telemetry loss.
#[test]
fn standalone_codex_hook_invalid_callbacks_and_outage_remain_neutral() {
    use std::io::Write;
    let valid = r#"{"hook_event_name":"SessionStart","session_id":"a","source":"startup","prompt":"PRIVATE"}"#;
    for (input, route, pane) in [
        (valid, "relative", "%1"),
        (valid, "/tmp/mez-absent-codex-helper.sock", "%1"),
        (valid, "/tmp/mez-absent-codex-helper.sock", ""),
        (
            r#"{"hook_event_name":"SessionStart","session_id":"a","source":"startup","source":"fork"}"#,
            "/tmp/mez-absent-codex-helper.sock",
            "%1",
        ),
        (
            r#"{"hook_event_name":"SessionStart","session_id":"a","source":"startup","agent_id":"child"}"#,
            "/tmp/mez-absent-codex-helper.sock",
            "%1",
        ),
        (
            r#"{"hook_event_name":"SessionStart","session_id":"a","source":"compact"}"#,
            "/tmp/mez-absent-codex-helper.sock",
            "%1",
        ),
        (
            "not json PRIVATE",
            "/tmp/mez-absent-codex-helper.sock",
            "%1",
        ),
    ] {
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_mez"))
            .arg("codex-hook")
            .env_clear()
            .env("MEZ", route)
            .env("MEZ_PANE", pane)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let result = child.wait_with_output().unwrap();
        assert!(result.status.success(), "{result:?}");
        assert_eq!(result.stdout, b"{}\n");
        assert!(result.stderr.is_empty());
    }
}

/// The fixed Codex callback helper must return neutral output before global
/// HOME/config/CPU discovery. Neither absent configuration nor inherited legacy
/// observer markers may turn unavailable telemetry into a vendor hook failure.
/// No vendor, provider or daemon is started by this real-binary regression.
#[test]
fn standalone_codex_hook_is_neutral_without_home_or_configuration() {
    for marked in [false, true] {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_mez"));
        command
            .arg("codex-hook")
            .env_clear()
            .stdin(std::process::Stdio::null());
        if marked {
            command.env("MEZ_CODEX_OBSERVER_FD", "3");
        }
        let result = command.output().unwrap();
        assert!(result.status.success(), "{result:?}");
        assert_eq!(result.stdout, b"{}\n");
        assert!(result.stderr.is_empty());
    }
}

/// The real binary's fixed harness-event mode must not resolve global runtime
/// CPU config before reading bounded stdin and emitting neutral output. Empty
/// environment/no route is unavailable telemetry, not a vendor failure.
#[test]
fn standalone_harness_event_is_neutral_without_home_or_configuration() {
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_mez"))
        .arg("harness-event")
        .env_clear()
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(result.status.success());
    assert_eq!(result.stdout, b"{}\n");
    assert!(result.stderr.is_empty());
}

/// Curated one-shot APIs provide no stdin transport. The public argv capsule
/// helper must avoid ordinary HOME/config startup and leave inherited stdin
/// unread; no route is a typed neutral unavailable result, not vendor failure.
#[test]
fn standalone_harness_source_is_public_unavailable_without_home_or_stdin() {
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_mez"))
        .args(["harness-source", r#"{"external_session_id":"session-a","observer_instance":"module-a","session_boundary":"startup"}"#])
        .env_clear().stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
    let _silent_stdin = child.stdin.take().unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(result.status.success());
    assert_eq!(result.stdout, b"{\"registered\":false}\n");
    assert!(result.stderr.is_empty());
}

/// Invalid public capsules, malformed routes and unavailable sockets must never
/// echo credential/content-shaped fields, consume stdin or invoke default config.
/// Exact source mode keeps fixed neutral output under a cleared environment.
#[test]
fn standalone_harness_source_invalid_capsule_and_route_are_neutral() {
    for (capsule, route) in [
        (
            r#"{"external_session_id":"a","observer_instance":"b","session_boundary":"startup","launch_token":"PRIVATE"}"#,
            "relative",
        ),
        (
            r#"{"external_session_id":"a","observer_instance":"b","session_boundary":"startup"}"#,
            "/tmp/mez-absent-source-helper-test.sock",
        ),
    ] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_mez"))
            .args(["harness-source", capsule])
            .env_clear()
            .env("MEZ", route)
            .env("MEZ_PANE", "%1")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"{\"registered\":false}\n");
        assert!(output.stderr.is_empty());
    }
}
