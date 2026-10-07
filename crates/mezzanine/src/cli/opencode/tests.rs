//! Exact local-session admission and public observer delivery fixtures.

use super::*;

/// The public dispatcher path privately issues an OpenCode capability, passes
/// only observation hints to its child, preserves --pure silence, and returns
/// the child's own exit status without relaunch or daemon-token exposure.
#[tokio::test(flavor = "current_thread")]
async fn opencode_public_launch_private_binding_and_pure_silence() {
    use std::os::unix::fs::PermissionsExt;
    use tokio::io::AsyncWriteExt;
    for pure in [false, true] {
        let root = Root::new();
        let socket = root.0.join("daemon");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let executable = root.0.join("opencode-fixture");
        std::fs::write(&executable,b"#!/bin/sh\n[ -z \"${MEZ_PANE:-}${MEZ_CONTROL_TOKEN:-}\" ] || exit 99\n[ \"$MEZ_OPENCODE_OBSERVER_FD\" = 3 ] || exit 98\n[ \"$1\" = --pure ] && exit 11\nprintf '%s\\n' '{\"session\":\"fresh\",\"kind\":\"start\"}' '{\"session\":\"fresh\",\"kind\":\"status\",\"state\":\"running\"}' '{\"session\":\"fresh\",\"kind\":\"status\",\"state\":\"retire\"}' >&3\nexit 11\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let server = async {
            let (mut stream, _) = listener.accept().await.unwrap();
            let init = request(&mut stream).await;
            assert_eq!(init["method"], "control/initialize");
            assert_ne!(init["params"]["client_name"], "primary");
            let reply = serde_json::json!({"jsonrpc":"2.0","id":init["id"],"result":{}});
            stream
                .write_all(&crate::control::encode_control_body(&reply.to_string()))
                .await
                .unwrap();
            let launch = request(&mut stream).await;
            assert_eq!(launch["params"]["harness"], "opencode");
            let reply = serde_json::json!({"jsonrpc":"2.0","id":launch["id"],"result":{"protocol":"external-agent/1","launch_token":"x".repeat(43),"generation":1,"expires_at_unix_seconds":super::super::current_unix_seconds().unwrap()+120,"lease_seconds":60}});
            stream
                .write_all(&crate::control::encode_control_body(&reply.to_string()))
                .await
                .unwrap();
            drop(stream);
            if pure {
                assert!(
                    tokio::time::timeout(Duration::from_millis(100), listener.accept())
                        .await
                        .is_err()
                );
                return;
            }
            for method in [
                "agent/external/register",
                "agent/external/presentation",
                "agent/external/deregister",
            ] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let req = request(&mut stream).await;
                assert_eq!(req["method"], method);
                assert_eq!(req["params"]["external_session_id"], "fresh");
                let result = match method {
                    "agent/external/register" => {
                        serde_json::json!({"registered":true,"agent_id":"agent","generation":1,"expires_at_unix_seconds":super::super::current_unix_seconds().unwrap()+60})
                    }
                    "agent/external/presentation" => {
                        serde_json::json!({"sequence":1,"changed":true})
                    }
                    _ => serde_json::json!({"retired":true,"changed":true}),
                };
                let reply = serde_json::json!({"jsonrpc":"2.0","id":req["id"],"result":result});
                stream
                    .write_all(&crate::control::encode_control_body(&reply.to_string()))
                    .await
                    .unwrap();
            }
        };
        let args = OpenCodeCliArgs {
            executable,
            pane: "%1".into(),
            session: None,
            vendor_version: "any-local".into(),
            arguments: if pure { vec!["--pure".into()] } else { vec![] },
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            let selection = SocketSelection::Explicit(socket);
            tokio::join!(run(args, &selection), server)
        })
        .await
        .unwrap();
        assert_eq!(result.unwrap(), 11);
    }
}

/// Test namespace owns only its local socket/database and removes it on drop.
struct Root(PathBuf);
impl Root {
    /// Allocates one collision-resistant private fixture directory.
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "mez-opencode-parent-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Strict one-request test decoder, never a process-visible bridge interface.
async fn request(stream: &mut tokio::net::UnixStream) -> serde_json::Value {
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::new();
    loop {
        let mut buffer = [0; 1024];
        let count = stream.read(&mut buffer).await.unwrap();
        assert!(count > 0 && bytes.len() + count < 65536);
        bytes.extend_from_slice(&buffer[..count]);
        if let Ok((body, _)) = crate::control::decode_control_frame(&bytes, 65536) {
            return serde_json::from_str(&body).unwrap();
        }
    }
}

/// Content-free parent observations round-trip through actual durable storage.
/// Identical completed snapshots charge once, history before cutoff is omitted,
/// and a lost/undurable acknowledgment stops telemetry without retrying expense.
#[tokio::test(flavor = "current_thread")]
async fn opencode_parent_acknowledges_durable_usage_replay_and_loss() {
    use crate::storage::token_usage::{
        ExternalCounters, ExternalUsageReport, TokenHistoryScope, TokenUsageStore,
    };
    use tokio::io::AsyncWriteExt;
    for mode in ["success", "undurable", "lost"] {
        let root = Root::new();
        let socket = root.0.join("daemon");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let grant = serde_json::from_value::<super::super::pi::Grant>(serde_json::json!({
            "protocol":"external-agent/1","launch_token":"x".repeat(43),"generation":1,
            "expires_at_unix_seconds":super::super::current_unix_seconds().unwrap()+120,"lease_seconds":60,
        })).unwrap();
        let store = TokenUsageStore::new(root.0.join("usage.sqlite"));
        let (stream, mut writer) = tokio::net::UnixStream::pair().unwrap();
        let (_stop, cancellation) = watch::channel(false);
        let mut held = None;
        let retired = std::cell::Cell::new(false);
        let producer = async {
            let message = serde_json::json!({"role":"assistant","sessionID":"bound","id":"message","providerID":"provider","modelID":"model","time":{"completed":1000},"tokens":{"input":10,"output":4,"reasoning":2,"cache":{"read":3,"write":5}}});
            let mut old = message.clone();
            old["id"] = "old".into();
            old["time"]["completed"] = 999.into();
            for item in [
                serde_json::json!({"session":"bound","kind":"start"}),
                serde_json::json!({"session":"bound","kind":"status","state":"running"}),
                serde_json::json!({"session":"bound","kind":"usage","message":old}),
                serde_json::json!({"session":"bound","kind":"usage","message":message}),
                serde_json::json!({"session":"bound","kind":"usage","message":message}),
                serde_json::json!({"session":"bound","kind":"status","state":"retire"}),
            ] {
                writer
                    .write_all((item.to_string() + "\n").as_bytes())
                    .await
                    .unwrap();
            }
            writer.shutdown().await.unwrap();
        };
        let server = async {
            let mut reports = 0;
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let req = request(&mut stream).await;
                assert_eq!(req["params"]["launch_token"], "x".repeat(43));
                assert_eq!(req["params"]["external_session_id"], "bound");
                let result = match req["method"].as_str().unwrap() {
                    "agent/external/register" => {
                        serde_json::json!({"registered":true,"agent_id":"agent","generation":1,"expires_at_unix_seconds":super::super::current_unix_seconds().unwrap()+60})
                    }
                    "agent/external/presentation" => {
                        serde_json::json!({"sequence":req["params"]["sequence"],"changed":true})
                    }
                    "agent/external/usage" => {
                        reports += 1;
                        let p = &req["params"];
                        let report = ExternalUsageReport {
                            owner: "server-owner".into(),
                            project: None,
                            harness: "opencode".into(),
                            epoch: p["epoch"].as_str().unwrap().into(),
                            event_id: p["event_id"].as_str().unwrap().into(),
                            sequence: p["sequence"].as_u64().unwrap(),
                            mode: p["mode"].as_str().unwrap().into(),
                            baseline: p["baseline"].as_bool().unwrap(),
                            observed_at: p["observed_at"].as_u64().unwrap(),
                            model: mez_agent::ModelTokenUsageKey::new(
                                p["provider"].as_str().unwrap(),
                                p["model"].as_str().unwrap(),
                            ),
                            counters: serde_json::from_value::<ExternalCounters>(
                                p["counters"].clone(),
                            )
                            .unwrap(),
                        };
                        assert!(!p.as_object().unwrap().contains_key("owner"));
                        assert!(!p.as_object().unwrap().contains_key("harness"));
                        let commit = store.ingest_external(&report, 1).unwrap();
                        assert_eq!(commit.applied, reports == 1);
                        if mode == "lost" {
                            return reports;
                        }
                        serde_json::json!({"accepted":true,"durable":mode!="undurable","applied":commit.applied,"revision":commit.revision})
                    }
                    "agent/external/deregister" => {
                        serde_json::json!({"retired":true,"changed":true})
                    }
                    other => panic!("unexpected {other}"),
                };
                let end = req["method"] == "agent/external/deregister"
                    || mode == "undurable" && req["method"] == "agent/external/usage";
                let body =
                    serde_json::json!({"jsonrpc":"2.0","id":req["id"],"result":result}).to_string();
                stream
                    .write_all(&crate::control::encode_control_body(&body))
                    .await
                    .unwrap();
                if end {
                    return reports;
                }
            }
        };
        let (result, (), reports) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(
                run_observer(
                    stream,
                    &socket,
                    grant,
                    Some("bound"),
                    1000,
                    &mut held,
                    &retired,
                    cancellation
                ),
                producer,
                server
            )
        })
        .await
        .unwrap();
        assert_eq!(result.is_ok(), mode == "success");
        assert_eq!(reports, if mode == "success" { 2 } else { 1 });
        let history = store
            .history_snapshot(1, &[1], &TokenHistoryScope::default())
            .unwrap();
        let usage = history.windows[&1].values().next().unwrap();
        assert_eq!(usage.usage.input_tokens, 18);
        assert_eq!(usage.usage.output_tokens, 6);
    }
}

/// Explicit local session selection cannot be silently replaced by implicit
/// continuation, fork, remote attach or policy bypass. Pure mode is preserved.
#[test]
fn opencode_cli_admission_keeps_local_session_and_disabled_plugins() {
    let mut args = OpenCodeCliArgs {
        executable: "/existing/opencode".into(),
        pane: "%1".into(),
        session: Some("ses_root".into()),
        vendor_version: "any-local".into(),
        arguments: vec!["--pure".into()],
    };
    validate(&args).unwrap();
    for value in [
        "--mini",
        "--no-replay",
        "--model=provider/model",
        "--agent=build",
        "--log-level=INFO",
        "--replay-limit=50",
    ] {
        args.arguments = vec![value.into()];
        validate(&args).unwrap();
    }
    for value in [
        "attach",
        "--continue",
        "--fork",
        "--session=other",
        "--hostname=remote",
        "--auto",
        "-cc",
        "-cs=ses_other",
        "-sother",
        "--unknown-option",
        "--pure=false",
        "--log-level=unknown",
    ] {
        args.arguments = vec![value.into()];
        assert!(validate(&args).is_err());
    }
}
