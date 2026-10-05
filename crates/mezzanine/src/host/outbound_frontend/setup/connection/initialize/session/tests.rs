//! Exact session settlement and shared-owner real-host creation qualification.
//!
//! Local setup retains protected profile evidence. Real loopback host fixtures
//! allocate shell-backed sessions only; no provider calls or user input replay.

use super::*;
use crate::host::async_runtime::AsyncAttachedTerminalIo;

/// Supplies one valid correlated active-lease response for validator probes.
fn response(server: iroh::EndpointId) -> serde_json::Value {
    serde_json::json!({"jsonrpc":"2.0","id":REQUEST_ID,"result":{
        "selected_version":3,"granted_role":"primary","host":{"endpoint_id":server.to_string()},
        "session":{"id":"$1"},"client":{"id":"c1"},
        "lease":{"lease_id":"lease-one","session_id":"$1","state":"active"},
        "x11_forwarding":null
    }})
}

/// Session/client/lease facts must match one correlated response and explicit
/// stable targets. Returned proof, role drift and mismatched lease ownership
/// cannot become a valid local summary, and diagnostics omit raw peer payload.
#[test]
fn outbound_session_settlement_requires_exact_client_and_lease() {
    let server = iroh::SecretKey::generate().public();
    let params = initialize_params_from_json(
        &serde_json::json!({
            "client_name":"frontend","requested_version":3,"requested_role":"primary",
            "session_intent":"attach","session_target":{"session_id":"$1"}
        })
        .to_string(),
    )
    .unwrap();
    let original = response(server);
    let summary = validate_session_response(&original.to_string(), server, &params).unwrap();
    assert_eq!(summary["session_id"], "$1");
    assert_eq!(summary["client_id"], "c1");
    for target in [
        serde_json::json!({"session_id":"$1","lease_id":null}),
        serde_json::json!({"lease_id":"lease-one","session_id":null}),
        serde_json::json!({"name":"work","session_id":null,"lease_id":null}),
    ] {
        let nullable = initialize_params_from_json(
            &serde_json::json!({
                "client_name":"frontend","requested_version":3,"requested_role":"primary",
                "session_intent":"attach","session_target":target
            })
            .to_string(),
        )
        .unwrap();
        assert!(validate_session_response(&original.to_string(), server, &nullable).is_ok());
    }
    for (pointer, value) in [
        ("/id", serde_json::json!("other")),
        ("/result/granted_role", serde_json::json!("observer")),
        ("/result/client/id", serde_json::json!("agent-%1")),
        ("/result/session/id", serde_json::json!("$2")),
        ("/result/lease/session_id", serde_json::json!("$2")),
        ("/result/lease/state", serde_json::json!("pending")),
        ("/result/lease/lease_id", serde_json::json!("")),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert!(
            validate_session_response(&changed.to_string(), server, &params).is_err(),
            "{pointer}"
        );
    }
    let mut proof = original;
    proof["result"]["device_credential"] = serde_json::json!("private-proof");
    let error = validate_session_response(&proof.to_string(), server, &params).unwrap_err();
    assert!(!error.message().contains("private-proof"));
}

/// Prepares one primary Create through real local hello and profile resolution.
/// Invocation keys are retained exactly; profile proof is never local input.
async fn create_frontend(
    admission: &OutboundFrontendAdmission,
    name: &str,
) -> (
    PreparedFrontend,
    Framed<tokio::net::UnixStream, ProtocolFrameCodec>,
) {
    let (server, client) = tokio::net::UnixStream::pair().unwrap();
    let mut client = Framed::new(client, ProtocolFrameCodec::new(HELLO_LIMIT).unwrap());
    client
        .send(ProtocolFrame::new(
            CONTENT_TYPE,
            serde_json::json!({"protocol":PROTOCOL}).to_string(),
        ))
        .await
        .unwrap();
    let frontend = admission.admit(server).await.unwrap();
    client.next().await.unwrap().unwrap();
    client.send(ProtocolFrame::new(CONTENT_TYPE, serde_json::json!({
        "handle":frontend.handle(),"profile":"creator","initialize":{
            "client_name":name,"requested_version":3,"requested_role":"primary",
            "session_intent":"create","idempotency_key":format!("create-{name}"),
            "detach_primary_on_disconnect":true,
            "event_stream_version":1,
            "client":{"name":name,"interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm"},
                "metadata":{"session_name":name}}
        }
    }).to_string())).await.unwrap();
    (
        frontend.prepare(Duration::from_secs(2)).await.unwrap(),
        client,
    )
}

/// Two independently initialized connections using the same paired endpoint
/// create distinct sessions while the first remains attached. Retiring one
/// owner leaves the sibling control stream usable. This is an in-process
/// broker-component fixture, not two CLI processes or event/X11 qualification.
#[tokio::test]
async fn outbound_session_initialization_creates_distinct_live_siblings() {
    use crate::host::iroh::HostIrohRuntime;
    use crate::host::router::{
        HostDefaultSessionPolicy, HostSessionRouter, HostSessionRouterConfig,
    };
    use crate::host::shell::{ResolvedShell, ShellSource};
    use crate::runtime::{RuntimeIrohCompressionCodec, RuntimeIrohTransportPolicy};
    use crate::security::remote::{
        RemoteHostRoutingAuthority, RemoteRoleCeiling, RemoteSessionAttachScope, RemoteTrustStore,
    };
    use std::os::unix::fs::PermissionsExt;

    let root = std::env::temp_dir().join(format!(
        "mez-owner-sessions-{:032x}",
        rand::random::<u128>()
    ));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let host_root = root.join("host");
    let client_root = root.join("client");
    let policy = RuntimeIrohTransportPolicy {
        compression_codecs: vec![RuntimeIrohCompressionCodec::None],
        ..Default::default()
    };
    let host = HostIrohRuntime::bind(
        &host_root,
        RuntimeIrohTransportPolicy {
            enabled: true,
            ..policy.clone()
        },
    )
    .await
    .unwrap()
    .unwrap();
    let endpoint = OutboundEndpointOwner::bind(&client_root, &policy)
        .await
        .unwrap();
    let admission =
        OutboundFrontendAdmission::new(endpoint.clone(), 2, Duration::from_secs(2)).unwrap();
    let router = HostSessionRouter::new(HostSessionRouterConfig {
        runtime_root: root.join("runtime"),
        owner_uid: crate::runtime::current_effective_uid(),
        config_root: host_root.clone(),
        config_layers: vec![crate::config::ConfigLayer {
            name: "outbound-receipts".into(),
            path: None,
            format: crate::config::ConfigFormat::Toml,
            scope: crate::config::ConfigScope::Primary,
            trusted: true,
            text: "[terminal]\nzen_mode = true\nzen_focus_label_duration_ms = 60000\n[agents]\nshell_mode = \"pane\"\n[permissions]\nsandbox = \"policy-only\"\n".into(),
        }],
        shell: ResolvedShell::new(
            std::path::PathBuf::from("/bin/sh"),
            ShellSource::FallbackBinSh,
        ),
        max_sessions: 4,
        max_live_sessions: 4,
        default_session_policy: HostDefaultSessionPolicy::MostRecentAttachable,
        default_lease_lifetime_seconds: 0,
    });
    let trust = RemoteTrustStore::under_host_config_root(&host_root).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let invitation = trust
        .create_host_invitation(
            host.endpoint_id(),
            RemoteRoleCeiling::Primary,
            RemoteHostRoutingAuthority {
                session_create: true,
                session_kill: false,
                session_list: true,
                session_attach_scope: RemoteSessionAttachScope::Own,
                max_active_leases: 4,
                max_live_sessions: 4,
                lease_lifetime_ceiling_seconds: None,
            },
            600,
            now,
        )
        .unwrap();
    let redemption = trust
        .redeem_invitation(
            &invitation.token,
            host.endpoint_id(),
            &endpoint.endpoint_id().to_string(),
            "creator",
            RequestedRole::Primary,
            now,
        )
        .unwrap();
    RemoteClientProfileStore::under_config_root(&client_root)
        .save(&RemoteClientProfile {
            name: "creator".into(),
            server_addr: host.endpoint_addr().unwrap(),
            role: RemoteRoleCeiling::Primary,
            scope: RemoteClientProfileScope::Host,
            device_credential: redemption.device_credential,
        })
        .unwrap();
    let stop = std::sync::Arc::new(tokio::sync::Notify::new());
    let server_stop = stop.clone();
    let serve = host.serve_routed(router.clone(), async move { server_stop.notified().await });
    let listener = crate::host::outbound_frontend::OutboundFrontendListener::bind(
        endpoint.clone(),
        2,
        Duration::from_secs(2),
    )
    .unwrap();
    let socket = listener.socket_path().unwrap().to_path_buf();
    let client_work = async {
        let (first, first_local) = create_frontend(&admission, "first").await;
        let mut first = first
            .connect_pinned()
            .await
            .unwrap()
            .initialize_session()
            .await
            .unwrap();
        let (second, second_local) = create_frontend(&admission, "second").await;
        let mut second = second
            .connect_pinned()
            .await
            .unwrap()
            .initialize_session()
            .await
            .unwrap();
        assert_ne!(first.summary["session_id"], second.summary["session_id"]);
        assert_ne!(first.summary["lease_id"], second.summary["lease_id"]);
        assert_eq!(router.snapshots().await.unwrap().len(), 2);
        assert_eq!(
            first.connected.prepared.initialize["idempotency_key"],
            "create-first"
        );
        let first_event = tokio::time::timeout(Duration::from_secs(2), first.next_event())
            .await
            .unwrap()
            .unwrap()
            .expect("negotiated first-session event");
        assert!(first_event.1.is_some());
        drop(first);
        let second_event = tokio::time::timeout(Duration::from_secs(2), second.next_event())
            .await
            .unwrap()
            .unwrap()
            .expect("sibling event remains available");
        assert!(second_event.1.is_some());
        assert!(
            second
                .connected
                .connection
                .connection()
                .close_reason()
                .is_none()
        );
        let handle = second.connected.prepared.frontend.handle().clone();
        let mut second_local = Framed::new(
            second_local.into_inner(),
            ProtocolFrameCodec::new(BODY_LIMIT).unwrap(),
        );
        second_local
            .send(ProtocolFrame::new(
                CONTENT_TYPE,
                serde_json::json!({
                    "handle":handle,"columns":80,"rows":24
                })
                .to_string(),
            ))
            .await
            .unwrap();
        let local_response = async {
            let frame = second_local.next().await.transpose().unwrap()?;
            assert_eq!(frame.content_type, CONTENT_TYPE);
            Some(serde_json::from_str::<serde_json::Value>(&frame.body).unwrap())
        };
        let (delivered, view) = tokio::join!(second.deliver_view(), local_response);
        second = delivered.unwrap();
        let view = view.unwrap();
        assert_eq!(view["session"], second.summary);
        assert_eq!(view["handle"], serde_json::to_value(&handle).unwrap());
        assert!(view["lines"].is_array());
        assert_eq!(
            view["line_style_spans"].as_array().unwrap().len(),
            view["lines"].as_array().unwrap().len()
        );
        assert!(view["cursor"].is_object());
        assert!(view["output_modes"].is_object());
        assert!(view["presentation_ids"].is_array());
        assert_eq!(
            second.delivered_receipts,
            serde_json::from_value::<Vec<u64>>(view["presentation_ids"].clone()).unwrap()
        );
        assert_eq!(view.as_object().unwrap().len(), 7);
        drop(second);
        drop(first_local);
        drop(second_local);
        assert_eq!(admission.slots.available_permits(), 2);
        // Also drive the actual supervising listener, not only direct owner APIs.
        let cancel = std::sync::Arc::new(tokio::sync::Notify::new());
        let server_cancel = cancel.clone();
        let supervised = listener.serve(async move { server_cancel.notified().await });
        let clients = async {
            let (first, first_view) = supervised_create(&socket, "supervised-first").await;
            let (second, second_view) = supervised_create(&socket, "supervised-second").await;
            assert_ne!(
                first_view["session"]["session_id"],
                second_view["session"]["session_id"]
            );
            assert_ne!(
                first_view["session"]["lease_id"],
                second_view["session"]["lease_id"]
            );
            assert_eq!(router.snapshots().await.unwrap().len(), 4);
            let (first, _, first_event) =
                first.poll_events(25, Duration::from_secs(2)).await.unwrap();
            assert!(
                first_event.is_some(),
                "negotiated events must cross local IPC"
            );
            drop(first);
            let (second, _, second_event) = second
                .poll_events(25, Duration::from_secs(2))
                .await
                .unwrap();
            assert!(second_event.is_some(), "sibling events survive retirement");
            assert_eq!(
                serde_json::to_value(second.summary()).unwrap(),
                second_view["session"]
            );
            // Literal bytes without a line terminator cannot execute a shell
            // command; acceptance is qualified independently of physical echo.
            let (second, acknowledgement) = second
                .step(
                    80,
                    24,
                    b"fixture",
                    "exact-fixture-input",
                    Duration::from_secs(2),
                )
                .await
                .unwrap();
            assert_eq!(acknowledgement.input_bytes, 7);
            assert!(!acknowledgement.client_detached);
            assert!(!acknowledgement.session_terminated);
            let (second, lines) = second
                .snapshot(80, 24, Duration::from_secs(2))
                .await
                .unwrap();
            assert_eq!(
                serde_json::to_value(second.summary()).unwrap(),
                second_view["session"]
            );
            assert!(lines.len() <= 24);
            assert_eq!(second.line_style_spans().len(), lines.len());
            assert!(second.output_modes().cursor_row < 24);
            assert!(second.output_modes().cursor_column < 80);
            let settlement = serde_json::to_value(second.summary()).unwrap();
            let runtime = router
                .runtime_for_tests(settlement["session_id"].as_str().unwrap())
                .unwrap();
            let client_id =
                ClientId::parse('c', settlement["client_id"].as_str().unwrap().to_string())
                    .unwrap();
            runtime
                .actor()
                .execute_terminal_command(client_id.clone(), "new-window receipt-focus".into())
                .await
                .unwrap();
            let (second, _) = second
                .snapshot(100, 30, Duration::from_secs(2))
                .await
                .unwrap();
            assert_eq!(
                serde_json::to_value(second.summary()).unwrap(),
                second_view["session"]
            );
            let receipts = second.presentation_ids().to_vec();
            assert!(
                !receipts.is_empty(),
                "focus transition must produce real receipts"
            );
            let pending = runtime
                .actor()
                .render_iroh_client_snapshot(client_id.clone(), false)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(pending.presentation_ids, receipts);
            // Commit the exact retained snapshot through the production fd
            // writer before forwarding its receipt. A disposable Unix endpoint
            // qualifies byte commitment, not visibility in a physical terminal.
            use std::os::fd::AsRawFd;
            let (output, mut output_peer) = std::os::unix::net::UnixStream::pair().unwrap();
            let output_clone = output.try_clone().unwrap();
            output_peer
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut terminal = crate::host::async_runtime::AsyncAttachedTerminalFdLoopIo::new(
                output.as_raw_fd(),
                output_clone.as_raw_fd(),
                None,
            )
            .unwrap();
            let (second, acknowledged) = second
                .present(&mut terminal, "exact-output-commit", Duration::from_secs(2))
                .await
                .unwrap();
            assert_eq!(terminal.pending_output_bytes(), 0);
            drop(terminal);
            drop(output_clone);
            drop(output);
            let mut committed_bytes = Vec::new();
            std::io::Read::read_to_end(&mut output_peer, &mut committed_bytes).unwrap();
            assert!(!committed_bytes.is_empty());
            assert!(
                acknowledged,
                "delivery alone must leave the receipt unarmed"
            );
            assert!(second.presentation_ids().is_empty());
            assert_eq!(
                runtime
                    .actor()
                    .acknowledge_zen_focus_label_presentations(client_id, receipts, 1)
                    .await
                    .unwrap(),
                0,
                "explicit acknowledgement must arm once, without renewing on duplicate"
            );
            // A completed snapshot with cleared receipts can enter the internal
            // foreground and exit on local EOF without recreating session work.
            // The fake records explicit presentation entry and restoration; the
            // production fd commitment was exercised immediately above.
            let mut foreground = crate::host::async_runtime::AsyncFakeAttachedTerminalIo::default();
            second
                .run_snapshot_foreground(
                    &mut foreground,
                    mez_mux::layout::Size::new(100, 30).unwrap(),
                    Duration::from_secs(2),
                    std::future::pending(),
                )
                .await
                .unwrap();
            assert_eq!(foreground.presentation_entries, 1);
            assert_eq!(foreground.presentation_restores, 1);
            assert_eq!(foreground.written_frames.len(), 1);
            assert_eq!(router.snapshots().await.unwrap().len(), 4);
            cancel.notify_one();
        };
        let (accepted, ()) = tokio::join!(supervised, clients);
        assert_eq!(accepted.unwrap(), 2);
        stop.notify_one();
    };
    let (served, ()) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(serve, client_work)
    })
    .await
    .unwrap();
    assert_eq!(served.unwrap(), 4);
    router
        .shutdown_all(true, Duration::from_secs(5))
        .await
        .unwrap();
    drop(listener);
    assert!(!socket.exists());
    drop(admission);
    endpoint.begin_shutdown().unwrap().finish().await.unwrap();
    drop(host);
    std::fs::remove_dir_all(root).unwrap();
}

/// Sends real listener hello/setup/view frames without direct admission calls.
/// The response binds one fresh session to the exact local handle; no proof is
/// supplied by or returned to this frontend.
async fn supervised_create(
    path: &std::path::Path,
    name: &str,
) -> (
    crate::host::outbound_frontend::client::OutboundSessionClient,
    serde_json::Value,
) {
    let client = crate::host::outbound_frontend::client::OutboundFrontendClient::connect(
        path.parent().unwrap(),
        Duration::from_secs(2),
    )
    .await
    .unwrap();
    let (client, lines) = client.start_session("creator", serde_json::json!({
            "client_name":name,"requested_version":3,"requested_role":"primary",
            "session_intent":"create","idempotency_key":format!("create-{name}"),
            "detach_primary_on_disconnect":true,
            "event_stream_version":1,
            "client":{"name":name,"interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm"},
                "metadata":{"session_name":name}}
    }), 80, 24, Duration::from_secs(2)).await.unwrap();
    let view = serde_json::json!({"session":client.summary(), "lines":lines});
    (client, view)
}
