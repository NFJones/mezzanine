//! Standalone fixed-helper startup, distinct from full CLI/config initialization.
//!
//! Observation helpers must remain neutral outside Mez with no HOME/default
//! configuration. Exact routing and peer authorization stay inside the helper;
//! this fixture never installs a vendor or supplies daemon credentials.

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
