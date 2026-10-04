//! Synthetic child tests for explicit launch and private descriptor ownership.
//! No vendor, user daemon, provider or user configuration is accessed.

use super::*;
use tokio::io::AsyncReadExt;

/// Builds an explicit shell fixture, not a product shell-selection fallback.
fn spec(script: &str) -> LaunchSpec {
    LaunchSpec {
        executable: "/bin/sh".into(),
        directory: std::env::temp_dir(),
        arguments: vec!["-c".into(), script.into()],
        environment: vec![("ONLY_EXPLICIT".into(), "fixture".into())],
        stdin: Stdio::null(),
        stdout: Stdio::null(),
        stderr: Stdio::null(),
    }
}

/// The inherited stream works without passing capability authority, daemon
/// routing or ambient environment. EOF proves no parent-held child endpoint
/// leaked past spawn; the caller reaps the actual process independently.
#[tokio::test]
async fn pi_launch_stream_and_environment_are_explicit() {
    let mut launched = spawn(spec(
        "test \"$ONLY_EXPLICIT\" = fixture && test -z \"${HOME+x}${MEZ_PANE+x}${MEZ_CONTROL_TOKEN+x}\" && printf '%s\\n' '{\"type\":\"agent_start\"}' >&3",
    )).unwrap();
    let mut bytes = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        launched.observer.read_to_end(&mut bytes),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(bytes, b"{\"type\":\"agent_start\"}\n");
    assert!(launched.child.wait().await.unwrap().success());
}

/// Telemetry can close without killing the child. The explicit process owner
/// retains normal stdin and reaping authority, and can finish the fixture after
/// observer disposal without a retry, replacement launch or daemon exchange.
#[tokio::test]
async fn pi_launch_observer_disposal_does_not_terminate_child() {
    use tokio::io::AsyncWriteExt;
    let mut input = spec("read answer; test \"$answer\" = finish");
    input.stdin = Stdio::piped();
    let launched = spawn(input).unwrap();
    drop(launched.observer);
    let mut child = launched.child;
    assert!(child.try_wait().unwrap().is_none());
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"finish\n")
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}

/// Relative selection and a nonexistent exact executable fail before any
/// usable launch is returned. Diagnostics must not echo sensitive argv/env.
#[tokio::test]
async fn pi_launch_invalid_selection_is_neutral_and_redacted() {
    for executable in ["sh", "/definitely-unavailable-mez-pi-executable"] {
        let mut input = spec("PRIVATE_ARG");
        input.executable = executable.into();
        input
            .environment
            .push(("PRIVATE_KEY".into(), "PRIVATE_VALUE".into()));
        let error = match spawn(input) {
            Ok(_) => panic!("invalid fixture executable launched"),
            Err(error) => error,
        };
        assert_eq!(
            error.to_string(),
            "InvalidState: Pi observer launch unavailable"
        );
    }
}
