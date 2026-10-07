//! Explicit launcher admission, private issuance and independently reaped children.

use super::*;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;

/// A disabled or unloaded extension leaves its inherited descriptor silent.
/// Child existence alone must not publish an Available vendor session, start
/// idle renewal or send retirement for an unregistered binding.
#[tokio::test]
async fn pi_cli_silent_extension_does_not_register() {
    let root = Fixture::new();
    let socket = root.0.join("control.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let mut owner = pi_owner::LifecycleOwner::new("bound").unwrap();
    let transport = pi_transport::CapabilityTransport::new(
        &socket,
        SecretString::from("x".repeat(43)),
        1,
        &owner,
    )
    .unwrap();
    let launched = pi_launch::spawn(pi_launch::LaunchSpec {
        executable: "/bin/sh".into(),
        directory: root.0.clone(),
        arguments: vec!["-c".into(), "sleep 0.1; exit 9".into()],
        environment: vec![],
        stdin: Stdio::null(),
        stdout: Stdio::null(),
        stderr: Stdio::null(),
    })
    .unwrap();
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(5),
            supervise(launched, &mut owner, &transport)
        )
        .await
        .unwrap()
        .unwrap(),
        9
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err(),
        "silent extension attempted daemon work"
    );
}

/// Drives public launch from primary issuance through child exec, observation,
/// registration and exact retirement using a local synthetic daemon. Neither
/// the argv nor inherited environment may contain its private grant. The child
/// receives the fresh session selector once and reports its own exit status.
#[tokio::test]
async fn pi_cli_public_launch_keeps_grant_in_parent() {
    let root = Fixture::new();
    let executable = root.0.join("pi-fixture");
    std::fs::write(&executable, "#!/bin/sh\n[ \"$1\" = '--session-id' ] || exit 99\n[ \"$2\" = \"$MEZ_PI_OBSERVER_SESSION\" ] || exit 98\n[ \"$MEZ_PI_OBSERVER_FD\" = 3 ] || exit 97\n[ -z \"${MEZ_PANE:-}\" ] || exit 96\nprintf '%s\\n' '{\"type\":\"session_start\",\"reason\":\"startup\"}' '{\"type\":\"agent_start\"}' >&3\nexit 13\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = root.0.join("daemon");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut methods = Vec::new();
        let mut bound = None;
        loop {
            let (mut stream, _) = loop {
                assert!(
                    std::time::Instant::now() < deadline,
                    "launcher did not retire"
                );
                match listener.accept() {
                    Ok(value) => break value,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(1))
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            loop {
                let bytes =
                    super::super::read_control_response_frames(&mut stream, 65536, 1).unwrap();
                let (body, _) = crate::control::decode_control_frame(&bytes, 65536).unwrap();
                let request: serde_json::Value = serde_json::from_str(&body).unwrap();
                let method = request["method"].as_str().unwrap();
                methods.push(method.to_string());
                let result = match method {
                    "control/initialize" => {
                        assert_ne!(request["params"]["client_name"], "primary");
                        serde_json::json!({})
                    }
                    "agent/external/launch" => {
                        serde_json::json!({"protocol":"external-agent/1","launch_token":"x".repeat(43),"generation":1,"expires_at_unix_seconds":super::super::current_unix_seconds().unwrap()+120,"lease_seconds":60})
                    }
                    "agent/external/register" => {
                        let session = request["params"]["external_session_id"].as_str().unwrap();
                        assert_ne!(session, "bound");
                        bound = Some(session.to_string());
                        serde_json::json!({"registered":true,"agent_id":"agent","generation":1,"expires_at_unix_seconds":super::super::current_unix_seconds().unwrap()+60})
                    }
                    "agent/external/presentation" => {
                        assert_eq!(
                            request["params"]["external_session_id"],
                            bound.as_deref().unwrap()
                        );
                        serde_json::json!({"sequence":request["params"]["sequence"],"changed":true})
                    }
                    "agent/external/deregister" => {
                        serde_json::json!({"retired":true,"changed":true})
                    }
                    _ => panic!("unexpected launcher work"),
                };
                let response =
                    serde_json::json!({"jsonrpc":"2.0","id":request["id"],"result":result})
                        .to_string();
                stream
                    .write_all(&crate::control::encode_control_body(&response))
                    .unwrap();
                if method == "agent/external/deregister" {
                    return methods;
                }
                if method != "control/initialize" {
                    break;
                }
            }
        }
    });
    let args = PiCliArgs {
        executable,
        pane: "%1".into(),
        vendor_version: "any-local-version".into(),
        arguments: vec![],
    };
    assert_eq!(
        run(args, &SocketSelection::Explicit(socket)).await.unwrap(),
        13
    );
    let methods = server.join().unwrap();
    assert_eq!(
        methods,
        [
            "control/initialize",
            "agent/external/launch",
            "agent/external/register",
            "agent/external/presentation",
            "agent/external/presentation",
            "agent/external/deregister"
        ]
    );
    let env = environment("bound");
    assert_eq!(
        env.iter()
            .filter(|(key, _)| key.to_string_lossy().starts_with("MEZ_"))
            .count(),
        2
    );
    assert!(
        !env.iter()
            .any(|(_, value)| value == &OsString::from("x".repeat(43)))
    );
}

/// Temporary sockets/scripts belong only to one fixture and never touch user
/// installations or provider credentials. Drop removes only this fixture root.
struct Fixture(PathBuf);
impl Fixture {
    /// Creates one private root with a collision-resistant suffix.
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "mez-pi-cli-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    /// Cleanup cannot escape the fixture's randomly allocated directory.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Session selectors reject before socket work, but disabled extensions remain
/// unchanged. Arbitrary inert versions do not require local vendor probing.
#[test]
fn pi_cli_admission_owns_fresh_session_and_respects_disabled_extensions() {
    use clap::Parser;
    #[derive(Parser)]
    struct Parse {
        #[command(flatten)]
        args: PiCliArgs,
    }
    let args = Parse::try_parse_from([
        "fixture",
        "--executable",
        "/existing/pi",
        "--pane",
        "%1",
        "--vendor-version",
        "future-local",
        "--",
        "--no-extensions",
    ])
    .unwrap()
    .args;
    validate(&args).unwrap();
    assert_eq!(args.arguments, [OsString::from("--no-extensions")]);
    for option in [
        "--session",
        "--session-id=other",
        "--continue",
        "--resume",
        "-c",
        "-r",
    ] {
        let mut rejected = args.clone();
        rejected.arguments.push(option.into());
        assert!(validate(&rejected).is_err());
    }
    let mut relative = args;
    relative.executable = "pi".into();
    assert!(validate(&relative).is_err());
}

/// Explicit-user initialization precedes issuance and never exposes the
/// credential in the request. Invalid or credential-containing peer errors are
/// redacted before they can become terminal diagnostics.
#[tokio::test]
async fn pi_cli_authorization_validates_and_redacts_private_grant() {
    for valid in [true, false] {
        let root = Fixture::new();
        let socket = root.0.join("control.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut read = || {
                let bytes =
                    super::super::read_control_response_frames(&mut stream, 65536, 1).unwrap();
                let (body, _) = crate::control::decode_control_frame(&bytes, 65536).unwrap();
                serde_json::from_str::<serde_json::Value>(&body).unwrap()
            };
            let init = read();
            assert_eq!(init["method"], "control/initialize");
            assert_ne!(init["params"]["client_name"], "primary");
            stream
                .write_all(&crate::control::encode_control_body(
                    r#"{"jsonrpc":"2.0","id":"pi-init","result":{}}"#,
                ))
                .unwrap();
            let bytes = super::super::read_control_response_frames(&mut stream, 65536, 1).unwrap();
            let (body, _) = crate::control::decode_control_frame(&bytes, 65536).unwrap();
            let request: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(request["method"], "agent/external/launch");
            assert_eq!(
                request["params"],
                serde_json::json!({"pane_id":"%1","harness":"pi","version":"future"})
            );
            let body = if valid {
                serde_json::json!({"jsonrpc":"2.0","id":"cli","result":{
                    "protocol":"external-agent/1","launch_token":"x".repeat(43),"generation":1,
                    "expires_at_unix_seconds":super::super::current_unix_seconds().unwrap()+120,"lease_seconds":60}}).to_string()
            } else {
                r#"{"jsonrpc":"2.0","id":"cli","error":{"message":"PRIVATE_TOKEN"}}"#.into()
            };
            stream
                .write_all(&crate::control::encode_control_body(&body))
                .unwrap();
        });
        let result = authorize(&socket, "%1", "future").await;
        if valid {
            assert_eq!(result.unwrap().generation, 1);
        } else {
            assert!(!result.err().unwrap().message().contains("PRIVATE"));
        }
        server.join().unwrap();
    }
}

/// A synthetic child writes only content-free facts. The production supervisor
/// forwards them through acknowledged lifecycle transport and returns the
/// child's exit code; daemon tokens never reach argv or its explicit environment.
#[tokio::test]
async fn pi_cli_supervisor_delivers_lifecycle_and_reaps_child() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let root = Fixture::new();
    let socket = root.0.join("control.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let now = super::super::current_unix_seconds().unwrap();
    let server = async {
        let mut states = Vec::new();
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let request = loop {
                let mut buffer = [0; 512];
                let count = stream.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&buffer[..count]);
                if let Ok((body, _)) = crate::control::decode_control_frame(&bytes, 65536) {
                    break serde_json::from_str::<serde_json::Value>(&body).unwrap();
                }
            };
            assert_eq!(request["params"]["launch_token"], "x".repeat(43));
            let result = match request["method"].as_str().unwrap() {
                "agent/external/register" => {
                    serde_json::json!({"registered":true,"agent_id":"agent","generation":1,"expires_at_unix_seconds":now+60})
                }
                "agent/external/presentation" => {
                    states.push(request["params"]["state"].as_str().unwrap().to_string());
                    serde_json::json!({"sequence":request["params"]["sequence"],"changed":true})
                }
                "agent/external/deregister" => serde_json::json!({"retired":true,"changed":true}),
                other => panic!("unexpected method {other}"),
            };
            let retired = request["method"] == "agent/external/deregister";
            let body = serde_json::json!({"jsonrpc":"2.0","id":"pi-lifecycle","result":result})
                .to_string();
            stream
                .write_all(&crate::control::encode_control_body(&body))
                .await
                .unwrap();
            if retired {
                break states;
            }
        }
    };
    let mut owner = pi_owner::LifecycleOwner::new("bound").unwrap();
    let transport = pi_transport::CapabilityTransport::new(
        &socket,
        SecretString::from("x".repeat(43)),
        1,
        &owner,
    )
    .unwrap();
    let launched = pi_launch::spawn(pi_launch::LaunchSpec {
        executable: "/bin/sh".into(), directory: root.0.clone(),
        arguments: vec!["-c".into(), "sleep 0.1; printf '%s\n' '{\"type\":\"session_start\",\"reason\":\"startup\"}' '{\"type\":\"agent_start\"}' >&3; exit 7".into()],
        environment: vec![], stdin: Stdio::null(), stdout: Stdio::null(), stderr: Stdio::null(),
    }).unwrap();
    let (result, states) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(supervise(launched, &mut owner, &transport), server)
    })
    .await
    .unwrap();
    assert_eq!(result.unwrap(), 7);
    assert_eq!(states, ["ready", "running"]);
}

/// Daemon outage only drops the observer. The child still exits normally and
/// its nonzero result is not replaced with a telemetry success or relaunch.
#[tokio::test]
async fn pi_cli_telemetry_failure_does_not_terminate_child() {
    let root = Fixture::new();
    let mut owner = pi_owner::LifecycleOwner::new("bound").unwrap();
    let transport = pi_transport::CapabilityTransport::new(
        &root.0.join("missing"),
        SecretString::from("x".repeat(43)),
        1,
        &owner,
    )
    .unwrap();
    let launched = pi_launch::spawn(pi_launch::LaunchSpec {
        executable: "/bin/sh".into(),
        directory: root.0.clone(),
        arguments: vec!["-c".into(), "sleep 0.1; exit 9".into()],
        environment: vec![],
        stdin: Stdio::null(),
        stdout: Stdio::null(),
        stderr: Stdio::null(),
    })
    .unwrap();
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(5),
            supervise(launched, &mut owner, &transport)
        )
        .await
        .unwrap()
        .unwrap(),
        9
    );
}
