//! Parent-owned handoff uses private loopback IPC and inert synthetic callbacks.
//! No vendor process, session file, user configuration or provider work is used.

use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Canceled or malformed proposals cannot activate initial registration or
/// primary reauthorization. Exact session/reason/epoch ownership remains a
/// parent predicate even when all input bytes are already available.
#[tokio::test(flavor = "current_thread")]
async fn pi_binding_parent_rejects_unowned_initial_and_precanceled_input() {
    for (session, epoch, reason, canceled) in [
        ("foreign", 1, "startup", false),
        ("initial", 2, "startup", false),
        ("initial", 1, "reload", false),
        ("initial", 1, "startup", true),
    ] {
        let root = Root::new();
        let socket = root.0.join("daemon");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let grant = Grant {
            protocol: "external-agent/1".into(),
            launch_token: SecretString::from("x".repeat(43)),
            generation: 1,
            expires_at_unix_seconds: super::super::super::current_unix_seconds().unwrap() + 120,
            lease_seconds: 60,
        };
        let mut context =
            Context::new(socket, "%1".into(), "any".into(), "initial", grant).unwrap();
        let (stream, mut child) = tokio::net::UnixStream::pair().unwrap();
        child
            .write_all(
                frame(
                    session,
                    epoch,
                    serde_json::json!({"type":"session_start","reason":reason}),
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let (_stop, cancellation) = tokio::sync::watch::channel(canceled);
        assert_eq!(context.run(stream, cancellation).await.is_ok(), canceled);
        assert!(!context.activated);
        assert!(
            tokio::time::timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
    }
}

/// The current public supervisor neither registers a silent extension nor
/// changes a child's exit behavior when activated telemetry encounters outage.
/// This uses v2 frames and the production Context, not the legacy component.
#[tokio::test(flavor = "current_thread")]
async fn pi_binding_current_supervisor_preserves_silent_and_outage_children() {
    for active in [false, true] {
        let root = Root::new();
        let socket = root.0.join("daemon");
        let listener = if active {
            None
        } else {
            Some(tokio::net::UnixListener::bind(&socket).unwrap())
        };
        let grant = Grant {
            protocol: "external-agent/1".into(),
            launch_token: SecretString::from("x".repeat(43)),
            generation: 1,
            expires_at_unix_seconds: super::super::super::current_unix_seconds().unwrap() + 120,
            lease_seconds: 60,
        };
        let mut context =
            Context::new(socket, "%1".into(), "any".into(), "initial", grant).unwrap();
        let command = if active {
            "printf '%s\\n' '{\"session\":\"initial\",\"epoch\":1,\"event\":{\"type\":\"session_start\",\"reason\":\"startup\"}}' >&3; sleep 0.1; exit 9"
        } else {
            "sleep 0.1; exit 9"
        };
        let child = pi_launch::spawn(pi_launch::LaunchSpec {
            executable: "/bin/sh".into(),
            directory: root.0.clone(),
            arguments: vec!["-c".into(), command.into()],
            environment: vec![],
            stdin: Stdio::null(),
            stdout: Stdio::null(),
            stderr: Stdio::null(),
        })
        .unwrap();
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(5),
                supervise_binding(child, &mut context)
            )
            .await
            .unwrap()
            .unwrap(),
            9
        );
        if let Some(listener) = listener {
            assert!(
                tokio::time::timeout(Duration::from_millis(30), listener.accept())
                    .await
                    .is_err()
            );
        }
    }
}

/// Fixture root/socket lifetime never touches a user installation.
struct Root(PathBuf);
impl Root {
    /// Creates one private uniquely named socket directory.
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "mez-pi-bind-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
}
impl Drop for Root {
    /// Removes only the test's owned namespace.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Reads one finite framed request from a local test connection.
async fn request(stream: &mut tokio::net::UnixStream) -> serde_json::Value {
    let mut bytes = Vec::new();
    loop {
        let mut buffer = [0; 1024];
        let count = stream.read(&mut buffer).await.unwrap();
        assert!(count > 0 && bytes.len() + count <= 65536);
        bytes.extend_from_slice(&buffer[..count]);
        if let Ok((body, _)) = crate::control::decode_control_frame(&bytes, 65536) {
            return serde_json::from_str(&body).unwrap();
        }
    }
}

/// Generates allowlisted wire facts only, without transcript or credential data.
fn frame(session: &str, epoch: u64, event: serde_json::Value) -> String {
    serde_json::json!({"session":session,"epoch":epoch,"event":event}).to_string() + "\n"
}

/// Startup, reload, new/resume/fork handoffs retain one child stream. Reload uses
/// the same registration/sequence; session transitions first acknowledge exact
/// retirement, then issue a distinct generation fenced to the predecessor root.
/// Lost retirement or rejected reauthorization must not register a new session.
#[tokio::test(flavor = "current_thread")]
async fn pi_binding_parent_orders_retirement_and_fresh_authorization() {
    for mode in ["success", "denied", "lost-retirement"] {
        let denied = mode == "denied";
        let root = Root::new();
        let socket = root.0.join("daemon");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let grant = Grant {
            protocol: "external-agent/1".into(),
            launch_token: SecretString::from("x".repeat(43)),
            generation: 1,
            expires_at_unix_seconds: super::super::super::current_unix_seconds().unwrap() + 120,
            lease_seconds: 60,
        };
        let mut context =
            Context::new(socket, "%1".into(), "any-version".into(), "initial", grant).unwrap();
        let (stream, mut child) = tokio::net::UnixStream::pair().unwrap();
        let (_stop, cancellation) = tokio::sync::watch::channel(false);
        let producer = async {
            let mut bytes = frame(
                "initial",
                1,
                serde_json::json!({"type":"session_start","reason":"startup"}),
            );
            bytes += &frame("initial", 1, serde_json::json!({"type":"agent_start"}));
            bytes += &frame(
                "initial",
                1,
                serde_json::json!({"type":"session_shutdown","reason":"reload"}),
            );
            bytes += &frame("initial", 1, serde_json::json!({"type":"agent_settled"})); // stale
            bytes += &frame(
                "initial",
                2,
                serde_json::json!({"type":"session_start","reason":"reload"}),
            );
            let mut session = "initial";
            for (i, (reason, next)) in
                [("new", "second"), ("resume", "initial"), ("fork", "branch")]
                    .into_iter()
                    .enumerate()
            {
                bytes += &frame(
                    session,
                    (i + 2) as u64,
                    serde_json::json!({"type":"session_shutdown","reason":reason}),
                );
                bytes += &frame(
                    next,
                    (i + 3) as u64,
                    serde_json::json!({"type":"session_start","reason":reason}),
                );
                bytes += &frame(
                    next,
                    (i + 3) as u64,
                    serde_json::json!({"type":"agent_start"}),
                );
                session = next;
            }
            bytes += &frame(
                "branch",
                5,
                serde_json::json!({"type":"session_shutdown","reason":"quit"}),
            );
            child.write_all(bytes.as_bytes()).await.unwrap();
            child.shutdown().await.unwrap();
        };
        let server = async {
            let mut generation = 1;
            let mut retired = false;
            let mut sequence = 0;
            let mut registrations = Vec::new();
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut req = request(&mut stream).await;
                if req["method"] == "control/initialize" {
                    let result = serde_json::json!({"jsonrpc":"2.0","id":req["id"],"result":{}});
                    stream
                        .write_all(&crate::control::encode_control_body(&result.to_string()))
                        .await
                        .unwrap();
                    req = request(&mut stream).await;
                }
                let method = req["method"].as_str().unwrap();
                let result = match method {
                    "agent/external/launch" => {
                        assert!(retired, "new authority allocated before retirement");
                        assert_eq!(req["params"]["root_generation"], generation);
                        if denied {
                            let error = serde_json::json!({"jsonrpc":"2.0","id":req["id"],"error":{"message":"root changed"}});
                            stream
                                .write_all(&crate::control::encode_control_body(&error.to_string()))
                                .await
                                .unwrap();
                            return registrations;
                        }
                        generation += 1;
                        sequence = 0;
                        retired = false;
                        serde_json::json!({"protocol":"external-agent/1","launch_token":"x".repeat(43),"generation":generation,"expires_at_unix_seconds":super::super::super::current_unix_seconds().unwrap()+120,"lease_seconds":60})
                    }
                    "agent/external/register" => {
                        assert!(!retired);
                        assert_eq!(req["params"]["generation"], generation);
                        registrations.push(
                            req["params"]["external_session_id"]
                                .as_str()
                                .unwrap()
                                .to_string(),
                        );
                        serde_json::json!({"registered":true,"agent_id":format!("agent{generation}"),"generation":generation,"expires_at_unix_seconds":super::super::super::current_unix_seconds().unwrap()+60})
                    }
                    "agent/external/presentation" => {
                        assert!(!retired);
                        sequence += 1;
                        assert_eq!(req["params"]["sequence"], sequence);
                        serde_json::json!({"sequence":sequence,"changed":true})
                    }
                    "agent/external/deregister" => {
                        assert!(!retired);
                        retired = true;
                        if mode == "lost-retirement" {
                            // Drop after accepting the request. No new primary
                            // issuance may follow this ambiguous acknowledgment.
                            return registrations;
                        }
                        serde_json::json!({"retired":true,"changed":true})
                    }
                    other => panic!("unexpected request {other}"),
                };
                let result = serde_json::json!({"jsonrpc":"2.0","id":req["id"],"result":result});
                stream
                    .write_all(&crate::control::encode_control_body(&result.to_string()))
                    .await
                    .unwrap();
                if generation == 4 && retired {
                    return registrations;
                }
            }
        };
        let (result, (), registrations) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(context.run(stream, cancellation), producer, server)
        })
        .await
        .unwrap();
        assert_eq!(result.is_err(), mode != "success");
        assert_eq!(
            registrations,
            if mode != "success" {
                vec!["initial"]
            } else {
                vec!["initial", "second", "initial", "branch"]
            }
        );
    }
}
