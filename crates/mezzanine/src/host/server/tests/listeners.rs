//! Host listener isolation, shared-authority failure and exact fixture cleanup.
//!
//! Offline endpoints and private stores exercise local management, real native
//! execution and routed attachment retirement. Faults touch only fixture-owned
//! endpoints and stores. Product observations precede independent cleanup, so
//! cleanup cannot manufacture a successful closure or native-retirement result.

use super::*;
use crate::control::{ControlConnectionState, encode_control_body};
use crate::host::iroh::HostIrohRuntime;
use crate::runtime::{
    RuntimeIrohIdentityPolicy, RuntimeIrohRelayPolicy, RuntimeIrohTransportPolicy,
};

/// Exchanges one bounded real management request without panicking on the
/// historical closed-socket regression, allowing fixture teardown first.
async fn management_request(socket: &Path, method: &str, params: Value) -> Result<Value> {
    let mut stream = tokio::net::UnixStream::connect(socket).await?;
    let request = json!({"jsonrpc":"2.0","id":"endpoint-loss","method":method,"params":params});
    stream
        .write_all(&encode_control_body(&request.to_string()))
        .await?;
    read_fixture_response(&mut stream).await
}

/// Reads one bounded response for local and Iroh fixture peers. Callers retain
/// their real connection and operation deadline; EOF remains an observed error.
async fn read_fixture_response<S: tokio::io::AsyncRead + Unpin>(stream: &mut S) -> Result<Value> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let read = tokio::io::AsyncReadExt::read(stream, &mut buffer).await?;
        if read == 0 {
            return Err(MezError::invalid_state("management closed before response"));
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.len() > HOST_CONTROL_MAX_CONTENT_LENGTH + 8192 {
            return Err(MezError::invalid_state(
                "management fixture response exceeded limit",
            ));
        }
        if let Ok((body, _)) =
            crate::control::decode_control_frame(&bytes, HOST_CONTROL_MAX_CONTENT_LENGTH)
        {
            return serde_json::from_str(&body)
                .map_err(|error| MezError::invalid_state(error.to_string()));
        }
    }
}

/// Sends a new command through the actual pane input owner. Its output is a
/// fresh file containing shell PID and nonce; terminal echo cannot forge it.
async fn shell_heartbeat(
    actor: &crate::host::async_runtime::AsyncRuntimeSessionHandle,
    primary: &mez_core::ids::ClientId,
    path: &Path,
    nonce: &str,
) -> Result<u32> {
    let command = format!("printf '%s {nonce}' \"$$\" > '{}'\n", path.display());
    actor
        .write_input_to_pane(primary.clone(), "%1", command.into_bytes())
        .await?;
    let text = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Ok(text) = fs::read_to_string(path)
                && text.ends_with(nonce)
            {
                break text;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .map_err(|_| MezError::invalid_state("fresh native shell heartbeat absent"))?;
    text.split_whitespace()
        .next()
        .and_then(|pid| pid.parse().ok())
        .ok_or_else(|| MezError::invalid_state("shell heartbeat PID missing"))
}

/// A real endpoint close must finish its remote listener without ending local
/// management, replacing the session or losing its original responsive native
/// pane. The first and second file nonces prove separate shell commands rather
/// than a cached snapshot, PID existence check, or PTY echo. Cleanup uses the
/// original host native owner before any assertion of the captured observations.
#[tokio::test(flavor = "current_thread")]
async fn optional_endpoint_loss_preserves_local_management_and_native_pane() {
    let root = test_root("optional-endpoint-loss");
    let policy = RuntimeIrohTransportPolicy {
        enabled: true,
        identity: RuntimeIrohIdentityPolicy::Host,
        relay: RuntimeIrohRelayPolicy::Disabled,
        setup_timeout: Duration::from_secs(3),
        ..RuntimeIrohTransportPolicy::default()
    };
    let remote = std::sync::Arc::new(
        HostIrohRuntime::bind(&root, policy.clone())
            .await
            .unwrap()
            .unwrap(),
    );
    let endpoint = remote.endpoint_for_test().endpoint().clone();
    let diagnostics = remote.endpoint_for_test().diagnostics();
    let mut settings = config(root.clone());
    settings.iroh_invitation_issuer = Some(remote.invitation_issuer());
    let host = std::sync::Arc::new(HostServer::bind(settings).unwrap());
    let native = host.router().native_cleanup_owner();
    let session =
        Box::pin(host.create_session(Some("local-survivor".into()), Size::new(80, 24).unwrap()))
            .await
            .unwrap();
    let actor = host
        .router()
        .runtime_for_tests(&session.session_id)
        .unwrap()
        .actor()
        .clone();
    let serving_host = host.clone();
    let serving_remote = remote.clone();
    let (cancel, cancelled) = tokio::sync::oneshot::channel();
    let mut serving = tokio::spawn(Box::pin(async move {
        serving_host
            .serve_with_iroh(&serving_remote, async {
                let _ = cancelled.await;
            })
            .await
    }));
    let observations: Result<()> = async {
        let initialized = actor.handle_control_input_for_connection(
            encode_control_body(r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"requested_role":"primary","requested_version":2,"client_name":"local-survivor","client":{"name":"local-survivor","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm"}}}}"#),
            HOST_CONTROL_MAX_CONTENT_LENGTH, ControlConnectionState::new(true,true),
        ).await?;
        let primary = initialized.connection.caller_client_id().cloned()
            .ok_or_else(|| MezError::invalid_state("local fixture primary was not initialized"))?;
        let pid = shell_heartbeat(&actor, &primary, &root.join("before"), "before-close").await?;
        let birth = mez_mux::process::process_start_token_for_pid(pid);
        if birth.is_none() {
            return Err(MezError::invalid_state("initial native identity absent"));
        }
        tokio::time::timeout(Duration::from_secs(3), async {
            while !diagnostics.snapshot().listener_active { tokio::task::yield_now().await; }
        }).await.map_err(|_| MezError::invalid_state("remote listener did not start"))?;
        endpoint.close().await;
        tokio::time::timeout(Duration::from_secs(3), async {
            while diagnostics.snapshot().listener_active { tokio::task::yield_now().await; }
        }).await.map_err(|_| MezError::invalid_state("remote listener did not drain"))?;
        let status = tokio::time::timeout(Duration::from_secs(3), management_request(host.socket_path(), "host/get", json!({})))
            .await.map_err(|_| MezError::invalid_state("local management stopped after endpoint loss"))??;
        if status["result"]["ready"] != true {
            return Err(MezError::invalid_state("local management is not ready after endpoint loss"));
        }
        if status["result"]["iroh"]["enabled"] != true || status["result"]["iroh"]["available"] != false {
            return Err(MezError::invalid_state("remote status hid the unavailable endpoint"));
        }
        if remote.endpoint_addr().is_some() || remote.invitation_issuer().available() {
            return Err(MezError::invalid_state("closed endpoint retained a dialable projection"));
        }
        if remote.invitation_issuer().create_invitation("unavailable", RemoteRoleCeiling::Observer, RemoteHostRoutingAuthority::default(), 60, 1).is_ok() {
            return Err(MezError::invalid_state("unavailable endpoint issued an invitation"));
        }
        let duplicate = HostIrohRuntime::bind(&root, policy).await;
        if !duplicate.is_err_and(|error| error.kind() == MezErrorKind::Conflict) {
            return Err(MezError::invalid_state("endpoint loss released the original host identity owner"));
        }
        let after_pid = shell_heartbeat(&actor, &primary, &root.join("after"), "after-close").await?;
        if after_pid != pid || mez_mux::process::process_start_token_for_pid(pid) != birth {
            return Err(MezError::invalid_state("local native pane was replaced after endpoint loss"));
        }
        let created = tokio::time::timeout(Duration::from_secs(5), management_request(host.socket_path(), "host/session/create", json!({"name":"after-loss","cwd":root,"shell":"/bin/sh"})))
            .await.map_err(|_| MezError::invalid_state("new local admission stalled after endpoint loss"))??;
        if created.get("error").is_some() || created["result"].get("session_id").and_then(Value::as_str).filter(|id| *id != session.session_id.as_str()).is_none() {
            return Err(MezError::invalid_state("new local session was not independently admitted"));
        }
        let issuer = remote.invitation_issuer();
        let replay = issuer.restore_invitation_response(json!({}), "unavailable-replay", &serde_json::Map::new());
        if !replay.is_err_and(|error| error.message().contains("transport is unavailable")) {
            return Err(MezError::invalid_state("invitation replay did not fence unavailable transport"));
        }
        issuer.list_clients()?;
        let snapshots = host.router().snapshots().await?;
        if !snapshots.iter().any(|entry| entry.session_id == session.session_id && entry.state == SessionSupervisorState::Running) {
            return Err(MezError::invalid_state("original local session is not running"));
        }
        let stopped = tokio::time::timeout(Duration::from_secs(5), management_request(host.socket_path(), "host/shutdown", json!({"force":false,"wait_for_completion":true})))
            .await.map_err(|_| MezError::invalid_state("local shutdown receipt stalled after endpoint loss"))??;
        if stopped["result"]["shutdown_completed"] != true {
            return Err(MezError::invalid_state("local shutdown did not complete after endpoint loss"));
        }
        Ok(())
    }.await;
    let _ = cancel.send(());
    let served = tokio::time::timeout(Duration::from_secs(8), &mut serving).await;
    if served.is_err() {
        // Retire only this fixture's async task; native custody remains with
        // the original host owner and is settled separately below.
        serving.abort();
        let _ = serving.await;
    }
    let cleanup = host
        .router()
        .shutdown_all(true, Duration::from_secs(3))
        .await;
    let pending = native.native_pending_count();
    drop(actor);
    drop(host);
    drop(remote);
    cleanup.unwrap();
    assert_eq!(pending.unwrap(), 0, "fixture retained native children");
    let _ = fs::remove_dir_all(root);
    observations.unwrap();
    let error = served.unwrap().unwrap().unwrap_err();
    assert!(
        error
            .message()
            .contains("Iroh listener closed unexpectedly"),
        "{error}"
    );
}

/// A real required UDP bind conflict must leave optional remote transport
/// unavailable while local administration and actual native execution work.
/// Releasing the owned conflicting socket does not authorize an automatic
/// restart or a second key owner. Shutdown observations precede final assertions.
#[tokio::test(flavor = "current_thread")]
async fn optional_startup_bind_failure_preserves_local_service_and_identity() {
    let root = test_root("optional-startup-bind");
    let occupied = std::net::UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, 0)).unwrap();
    let policy = RuntimeIrohTransportPolicy {
        enabled: true,
        identity: RuntimeIrohIdentityPolicy::Host,
        relay: RuntimeIrohRelayPolicy::Disabled,
        bind_port: occupied.local_addr().unwrap().port(),
        setup_timeout: Duration::from_secs(3),
        ..RuntimeIrohTransportPolicy::default()
    };
    let remote = match HostIrohRuntime::bind(&root, policy.clone()).await {
        Ok(Some(remote)) => std::sync::Arc::new(remote),
        other => {
            let _ = fs::remove_dir_all(root);
            panic!("optional network bind failure prevented local startup: {other:?}");
        }
    };
    let mut settings = config(root.clone());
    settings.iroh_invitation_issuer = Some(remote.invitation_issuer());
    let host = std::sync::Arc::new(HostServer::bind(settings).unwrap());
    let native = host.router().native_cleanup_owner();
    let session =
        Box::pin(host.create_session(Some("startup-local".into()), Size::new(80, 24).unwrap()))
            .await
            .unwrap();
    let actor = host
        .router()
        .runtime_for_tests(&session.session_id)
        .unwrap()
        .actor()
        .clone();
    let serving_host = host.clone();
    let serving_remote = remote.clone();
    let (cancel, cancelled) = tokio::sync::oneshot::channel();
    let mut serving = tokio::spawn(Box::pin(async move {
        serving_host
            .serve_with_iroh(&serving_remote, async {
                let _ = cancelled.await;
            })
            .await
    }));
    let observations: Result<()> = async {
        drop(occupied);
        let status = tokio::time::timeout(Duration::from_secs(3), management_request(host.socket_path(), "host/get", json!({})))
            .await.map_err(|_| MezError::invalid_state("local management unavailable after optional bind failure"))??;
        if status["result"]["ready"] != true || status["result"]["iroh"]["enabled"] != true
            || status["result"]["iroh"]["available"] != false
            || status["result"]["iroh"]["endpoint_id"].as_str() != Some(remote.endpoint_id())
        {
            return Err(MezError::invalid_state("startup status hid configured transport unavailability"));
        }
        let initialized = actor.handle_control_input_for_connection(
            encode_control_body(r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"requested_role":"primary","requested_version":2,"client_name":"startup-local","client":{"name":"startup-local","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm"}}}}"#),
            HOST_CONTROL_MAX_CONTENT_LENGTH, ControlConnectionState::new(true,true),
        ).await?;
        let primary = initialized.connection.caller_client_id().cloned()
            .ok_or_else(|| MezError::invalid_state("startup local primary was not initialized"))?;
        let pid = shell_heartbeat(&actor, &primary, &root.join("startup-heartbeat"), "startup-local-ready").await?;
        if mez_mux::process::process_start_token_for_pid(pid).is_none() {
            return Err(MezError::invalid_state("startup local native identity absent"));
        }
        let duplicate = HostIrohRuntime::bind(&root, policy).await;
        if !duplicate.is_err_and(|error| error.kind() == MezErrorKind::Conflict) {
            return Err(MezError::invalid_state("unavailable startup transport lost its original identity lock"));
        }
        let issuer = remote.invitation_issuer();
        if issuer.available() || remote.endpoint_addr().is_some() {
            return Err(MezError::invalid_state("unavailable startup transport silently restarted"));
        }
        issuer.list_clients()?;
        if issuer.create_invitation("startup", RemoteRoleCeiling::Observer, RemoteHostRoutingAuthority::default(), 60, 1).is_ok() {
            return Err(MezError::invalid_state("unavailable startup transport issued an invitation"));
        }
        Ok(())
    }.await;
    let _ = cancel.send(());
    let served = tokio::time::timeout(Duration::from_secs(8), &mut serving).await;
    if served.is_err() {
        serving.abort();
        let _ = serving.await;
    }
    let cleanup = host
        .router()
        .shutdown_all(true, Duration::from_secs(3))
        .await;
    let pending = native.native_pending_count();
    drop(actor);
    drop(host);
    drop(remote);
    cleanup.unwrap();
    assert_eq!(pending.unwrap(), 0);
    let _ = fs::remove_dir_all(root);
    observations.unwrap();
    let error = served.unwrap().unwrap().unwrap_err();
    assert!(
        error.message().contains("failed to bind Iroh endpoint"),
        "{error}"
    );
}

/// Runs an actual listener pair without native sessions. A temporary owned
/// lease-directory type fault exercises critical shared-authority I/O. Product
/// outcomes and peer closure are observed before restoring paths or performing
/// independent fixture cleanup; a failed observation cannot become a pass merely
/// because the cleanup closes the endpoint later.
async fn observe_listener_pair_disposition(mode: &str) -> Result<()> {
    let root = test_root(mode);
    let policy = RuntimeIrohTransportPolicy {
        enabled: true,
        identity: RuntimeIrohIdentityPolicy::Host,
        relay: RuntimeIrohRelayPolicy::Disabled,
        setup_timeout: Duration::from_secs(3),
        ..RuntimeIrohTransportPolicy::default()
    };
    let remote = std::sync::Arc::new(HostIrohRuntime::bind(&root, policy).await.unwrap().unwrap());
    let endpoint = remote.endpoint_for_test().endpoint().clone();
    let diagnostics = remote.endpoint_for_test().diagnostics();
    let mut settings = config(root.clone());
    settings.iroh_invitation_issuer = Some(remote.invitation_issuer());
    let host = std::sync::Arc::new(HostServer::bind(settings).unwrap());
    let native = host.router().native_cleanup_owner();
    let serving_host = host.clone();
    let serving_remote = remote.clone();
    let (cancel, cancelled) = tokio::sync::oneshot::channel();
    let mut cancel = Some(cancel);
    let mut serving = tokio::spawn(Box::pin(async move {
        serving_host
            .serve_with_iroh(&serving_remote, async {
                let _ = cancelled.await;
            })
            .await
    }));
    let leases = root.join("host/leases");
    let retained = root.join("retained-leases");
    let mut moved_leases = false;
    let mut joined = false;
    let observations: Result<()> = async {
        let ready = tokio::time::timeout(
            Duration::from_secs(3),
            management_request(host.socket_path(), "host/get", json!({})),
        )
        .await
        .map_err(|_| MezError::invalid_state("listener fixture local readiness timed out"))??;
        if ready["result"]["ready"] != true {
            return Err(MezError::invalid_state(
                "listener fixture local readiness absent",
            ));
        }
        tokio::time::timeout(Duration::from_secs(3), async {
            while !diagnostics.snapshot().listener_active {
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| MezError::invalid_state("listener fixture remote readiness absent"))?;
        if mode == "after-remote" {
            endpoint.close().await;
            tokio::time::timeout(Duration::from_secs(3), async {
                while diagnostics.snapshot().listener_active {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .map_err(|_| {
                MezError::invalid_state("remote loss was not settled before local fault")
            })?;
            let degraded = tokio::time::timeout(
                Duration::from_secs(3),
                management_request(host.socket_path(), "host/get", json!({})),
            )
            .await
            .map_err(|_| {
                MezError::invalid_state("degraded local listener did not remain active")
            })??;
            if degraded["result"]["iroh"]["available"] != false {
                return Err(MezError::invalid_state(
                    "degraded transport was not observed",
                ));
            }
        }
        if mode == "cancel" {
            let _ = cancel.take().unwrap().send(());
        } else {
            fs::rename(&leases, &retained)?;
            moved_leases = true;
            fs::write(&leases, b"fixture-owned authority destination fault")?;
            if mode == "simultaneous" {
                endpoint.close().await;
            }
            // Wake the real accept loop. No request or uncertain operation is
            // replayed; the next owner loop observes its actual lease-store I/O.
            drop(tokio::net::UnixStream::connect(host.socket_path()).await?);
        }
        let result = tokio::time::timeout(Duration::from_secs(8), &mut serving)
            .await
            .map_err(|_| {
                MezError::invalid_state("listener owner did not finish its critical/cancel path")
            })?;
        joined = true;
        let result = result.map_err(|error| {
            MezError::invalid_state(format!("listener owner task failed: {error}"))
        })?;
        if !endpoint.is_closed() || diagnostics.snapshot().listener_active {
            return Err(MezError::invalid_state(
                "peer endpoint remained live after owner completion",
            ));
        }
        if mode == "cancel" {
            result?;
        } else {
            let error = result.err().ok_or_else(|| {
                MezError::invalid_state("critical authority I/O was reported as success")
            })?;
            if error.io_kind().is_none() || !error.message().contains("host serving") {
                return Err(MezError::invalid_state(format!(
                    "critical local cause was replaced: {error}"
                )));
            }
            if mode == "after-remote" && !error.message().contains("remote transport unavailable") {
                return Err(MezError::invalid_state(
                    "later critical error erased observed remote degradation",
                ));
            }
        }
        Ok(())
    }
    .await;
    if moved_leases {
        if leases.is_file() {
            fs::remove_file(&leases).unwrap();
        }
        fs::rename(&retained, &leases).unwrap();
    }
    if !joined {
        if let Some(cancel) = cancel.take() {
            let _ = cancel.send(());
        }
        if tokio::time::timeout(Duration::from_secs(8), &mut serving)
            .await
            .is_err()
        {
            serving.abort();
            let _ = serving.await;
        }
    }
    let endpoint_cleanup = tokio::time::timeout(Duration::from_secs(3), endpoint.close()).await;
    let runtime_cleanup = host
        .router()
        .shutdown_all(true, Duration::from_secs(3))
        .await;
    let pending = native.native_pending_count();
    drop(host);
    drop(remote);
    runtime_cleanup?;
    endpoint_cleanup.map_err(|_| MezError::invalid_state("fixture endpoint cleanup timed out"))?;
    if pending? != 0 {
        return Err(MezError::invalid_state(
            "native-free fixture acquired native debt",
        ));
    }
    let _ = fs::remove_dir_all(root);
    observations
}

/// Critical shared-authority failure remains fatal before, after and alongside
/// optional remote loss. The real endpoint must be closed before the owner
/// returns, and a previously observed remote outage cannot replace local cause.
#[tokio::test(flavor = "current_thread")]
async fn critical_local_authority_failure_still_settles_optional_remote_peer() {
    for mode in ["local-first", "after-remote", "simultaneous"] {
        observe_listener_pair_disposition(mode).await.unwrap();
    }
}

/// The cancellation future used by CLI shutdown signals still settles a real
/// healthy local/remote pair successfully. This exercises the shared owner path;
/// it does not send an operating-system signal to the test runner process.
#[tokio::test(flavor = "current_thread")]
async fn host_signal_cancellation_settles_both_healthy_listeners() {
    observe_listener_pair_disposition("cancel").await.unwrap();
}

/// One fixture-owned authenticated control connection. Kept outside the
/// observation future so all pumps can be explicitly joined even on failure.
struct RemoteProbe {
    /// Actual SDK connection whose closure is observed before fixture cleanup.
    connection: iroh::endpoint::Connection,
    /// Owned bounded control-frame bridge, never a simulated protocol peer.
    bridge: crate::runtime::IrohCompressionBridge,
}

impl RemoteProbe {
    /// Connects once over the real endpoint and uncompressed transport contract.
    /// Returns setup failure without issuing any session operation or retry.
    async fn connect(client: &iroh::Endpoint, address: &iroh::EndpointAddr) -> Result<Self> {
        tokio::time::timeout(Duration::from_secs(3), async {
            let connection = client
                .connect(address.clone(), crate::runtime::MEZZANINE_IROH_ALPN)
                .await
                .map_err(|error| {
                    MezError::invalid_state(format!("fixture Iroh connect failed: {error}"))
                })?;
            let (send, recv) = connection.open_bi().await.map_err(|error| {
                MezError::invalid_state(format!("fixture Iroh stream failed: {error}"))
            })?;
            let compression = crate::runtime::IrohCompressionPolicy::new(
                crate::runtime::RuntimeIrohCompressionCodec::None,
                512,
                3,
                HOST_CONTROL_MAX_CONTENT_LENGTH + 1024,
            )?;
            let bridge = crate::runtime::IrohCompressionBridge::spawn(
                recv,
                send,
                compression,
                HOST_CONTROL_MAX_CONTENT_LENGTH,
            )?;
            Ok(Self { connection, bridge })
        })
        .await
        .map_err(|_| MezError::invalid_state("fixture Iroh connect timed out"))?
    }

    /// Exchanges one real request without FIN or replay, keeping attachment
    /// ownership alive until actual endpoint closure is injected.
    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        tokio::time::timeout(Duration::from_secs(3), async {
            let request =
                json!({"jsonrpc":"2.0","id":"active-probe","method":method,"params":params});
            self.bridge
                .stream_mut()
                .write_all(&encode_control_body(&request.to_string()))
                .await?;
            self.bridge.stream_mut().flush().await?;
            read_fixture_response(self.bridge.stream_mut()).await
        })
        .await
        .map_err(|_| MezError::invalid_state("fixture Iroh response timed out"))?
    }

    /// Settles the owned pump even when endpoint loss already made flushing
    /// fail. Bridge shutdown joins on success and on its abort/error branches;
    /// this cleanup result cannot rewrite prior product observations.
    async fn finish(self) {
        let _ = self.bridge.shutdown(Duration::from_secs(3)).await;
        self.connection.close(
            iroh::endpoint::VarInt::from_u32(0),
            b"owned fixture complete",
        );
    }
}

/// Actual endpoint loss must disconnect a live routed primary and its transport
/// tasks, retain the existing durable lease/session and original native pane,
/// and remove the old client's input authority. A later local primary supplies
/// a fresh command/nonce barrier proving both native continuity and rejection of
/// stale-client input. Local trust revocation must remain usable while remote
/// transport is unavailable. All owners are retained before remote creation.
#[tokio::test(flavor = "current_thread")]
async fn endpoint_loss_settles_active_attachment_without_replaying_or_losing_lease() {
    use crate::security::remote::{RemoteSessionAttachScope, RemoteTrustStore};
    use crate::storage::lease::RemoteSessionLeaseState;
    use secrecy::ExposeSecret;

    let root = test_root("active-endpoint-loss");
    let policy = RuntimeIrohTransportPolicy {
        enabled: true,
        identity: RuntimeIrohIdentityPolicy::Host,
        relay: RuntimeIrohRelayPolicy::Disabled,
        compression_codecs: vec![crate::runtime::RuntimeIrohCompressionCodec::None],
        setup_timeout: Duration::from_secs(3),
        ..RuntimeIrohTransportPolicy::default()
    };
    let remote = std::sync::Arc::new(
        HostIrohRuntime::bind(&root, policy.clone())
            .await
            .unwrap()
            .unwrap(),
    );
    let endpoint = remote.endpoint_for_test().endpoint().clone();
    let address = remote.endpoint_addr().unwrap();
    let diagnostics = remote.endpoint_for_test().diagnostics();
    let trust = RemoteTrustStore::under_host_config_root(&root).unwrap();
    let now = current_unix_seconds().unwrap();
    let invitation = trust
        .create_host_invitation(
            remote.endpoint_id(),
            RemoteRoleCeiling::Primary,
            RemoteHostRoutingAuthority {
                session_create: true,
                session_list: true,
                session_attach_scope: RemoteSessionAttachScope::Own,
                max_active_leases: 2,
                max_live_sessions: 2,
                ..RemoteHostRoutingAuthority::default()
            },
            600,
            now,
        )
        .unwrap();
    let mut settings = config(root.clone());
    settings.iroh_invitation_issuer = Some(remote.invitation_issuer());
    let host = std::sync::Arc::new(HostServer::bind(settings).unwrap());
    let native = host.router().native_cleanup_owner();
    let client =
        crate::runtime::bind_runtime_iroh_client_endpoint(&policy, iroh::SecretKey::generate())
            .await
            .unwrap();
    let serving_host = host.clone();
    let serving_remote = remote.clone();
    let (cancel, cancelled) = tokio::sync::oneshot::channel();
    let mut serving = tokio::spawn(Box::pin(async move {
        serving_host
            .serve_with_iroh(&serving_remote, async {
                let _ = cancelled.await;
            })
            .await
    }));
    let mut probes = Vec::new();
    let observations: Result<()> = async {
        probes.push(RemoteProbe::connect(&client, &address).await?);
        let paired = probes[0].request("control/initialize", json!({
            "client_name":"pair-probe", "requested_version":3, "requested_role":"observer", "session_intent":"host_only",
            "authentication":{"mechanism":"extension:iroh_invitation","token":invitation.token.expose_secret()}
        })).await?;
        let credential = paired["result"]["device_credential"].as_str()
            .ok_or_else(|| MezError::invalid_state("fixture pairing did not return a credential"))?.to_string();
        probes.pop().unwrap().finish().await;
        probes.push(RemoteProbe::connect(&client, &address).await?);
        let created = probes[0].request("control/initialize", json!({
            "client_name":"active-probe", "requested_version":3, "requested_role":"primary", "session_intent":"create",
            "idempotency_key":"active-create-once", "detach_primary_on_disconnect":true,
            "client":{"name":"active-probe","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm"}},
            "authentication":{"mechanism":"extension:iroh_device","token":credential}
        })).await?;
        let lease_id = created["result"]["lease"]["lease_id"].as_str()
            .ok_or_else(|| MezError::invalid_state("routed fixture lease absent"))?.to_string();
        let session_id = created["result"]["lease"]["session_id"].as_str()
            .ok_or_else(|| MezError::invalid_state("routed fixture session absent"))?.to_string();
        let lease_before = host.router().get_lease(&lease_id)?;
        let state = probes[0].request("session/get", json!({})).await?;
        if state["result"]["session"]["attached_primary_count"] != 1 {
            return Err(MezError::invalid_state("routed fixture did not own exactly one active primary"));
        }
        let old_primary = state["result"]["session"]["primary_client_ids"][0].as_str()
            .and_then(mez_core::ids::ClientId::opaque)
            .ok_or_else(|| MezError::invalid_state("routed fixture primary absent"))?;
        let panes = probes[0].request("pane/list", json!({})).await?;
        if panes.get("error").is_some() {
            return Err(MezError::invalid_state("active routed control request failed"));
        }
        let runtime = host.router().runtime_for_tests(&session_id)?;
        let actor = runtime.actor();
        let pid = shell_heartbeat(actor, &old_primary, &root.join("remote-before"), "remote-before-loss").await?;
        let birth = mez_mux::process::process_start_token_for_pid(pid)
            .ok_or_else(|| MezError::invalid_state("routed native identity absent"))?;
        endpoint.close().await;
        tokio::time::timeout(Duration::from_secs(3), probes[0].connection.closed()).await
            .map_err(|_| MezError::invalid_state("active routed connection did not close"))?;
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let snapshot = diagnostics.snapshot();
                if !snapshot.listener_active && snapshot.active_connections == 0
                    && actor.lifecycle_state().await? == RuntimeLifecycleState::Detached { break Ok::<(), MezError>(()); }
                tokio::task::yield_now().await;
            }
        }).await.map_err(|_| MezError::invalid_state("remote attachment ownership did not settle"))??;
        let lease_after = host.router().get_lease(&lease_id)?;
        if lease_after.state != RemoteSessionLeaseState::Active || lease_after.session_id != session_id
            || lease_after.lease_generation != lease_before.lease_generation
            || host.router().snapshots().await?.len() != 1 {
            return Err(MezError::invalid_state("endpoint loss changed/replayed durable session assignment"));
        }
        let blocked = root.join("stale-input-effect");
        let stale = format!("printf stale > '{}'\n", blocked.display());
        if actor.write_input_to_pane(old_primary, "%1", stale.into_bytes()).await.is_ok() {
            return Err(MezError::invalid_state("disconnected remote primary retained input authority"));
        }
        let local = actor.handle_control_input_for_connection(
            encode_control_body(r#"{"jsonrpc":"2.0","id":"local","method":"control/initialize","params":{"requested_version":2,"requested_role":"primary","client_name":"after-remote","client":{"name":"after-remote","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm"}}}}"#),
            HOST_CONTROL_MAX_CONTENT_LENGTH, ControlConnectionState::new(true,true),
        ).await?;
        let local_primary = local.connection.caller_client_id().cloned()
            .ok_or_else(|| MezError::invalid_state("replacement local primary absent"))?;
        let after_pid = shell_heartbeat(actor, &local_primary, &root.join("remote-after"), "local-after-loss").await?;
        if after_pid != pid || mez_mux::process::process_start_token_for_pid(pid) != Some(birth) || blocked.exists() {
            return Err(MezError::invalid_state("routed pane was replaced or stale input produced an effect"));
        }
        let status = tokio::time::timeout(Duration::from_secs(3), management_request(host.socket_path(), "host/get", json!({})))
            .await.map_err(|_| MezError::invalid_state("local administration lost after active transport loss"))??;
        if status["result"]["ready"] != true || status["result"]["iroh"]["available"] != false {
            return Err(MezError::invalid_state("active loss was reported as healthy remote transport"));
        }
        let records = remote.invitation_issuer().list_clients()?;
        let record = records.first().ok_or_else(|| MezError::invalid_state("paired trust record absent"))?;
        let revoked = tokio::time::timeout(Duration::from_secs(3), management_request(host.socket_path(), "remote/client/revoke",
            json!({"client_id":record.id,"reason":"owned outage test","idempotency_key":"revoke-after-outage"})))
            .await.map_err(|_| MezError::invalid_state("local revocation stalled after outage"))??;
        if revoked["result"]["revoked"] != true {
            return Err(MezError::invalid_state("local trust revocation unavailable during outage"));
        }
        Ok(())
    }.await;
    for probe in probes {
        probe.finish().await;
    }
    client.close().await;
    let _ = cancel.send(());
    let served = tokio::time::timeout(Duration::from_secs(8), &mut serving).await;
    if served.is_err() {
        serving.abort();
        let _ = serving.await;
    }
    let cleanup = host
        .router()
        .shutdown_all(true, Duration::from_secs(3))
        .await;
    let pending = native.native_pending_count();
    drop(host);
    drop(remote);
    cleanup.unwrap();
    assert_eq!(
        pending.unwrap(),
        0,
        "active fixture retained native ownership"
    );
    let _ = fs::remove_dir_all(root);
    observations.unwrap();
    let error = served.unwrap().unwrap().unwrap_err();
    assert!(
        error
            .message()
            .contains("Iroh listener closed unexpectedly"),
        "{error}"
    );
}
