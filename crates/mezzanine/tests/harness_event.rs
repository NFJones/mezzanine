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
