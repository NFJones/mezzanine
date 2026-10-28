//! Actual optional endpoint loss against the existing local host and native pane.
//!
//! The fixture closes only its own Iroh endpoint, waits for remote task draining,
//! then requires fresh shell execution and owner-authenticated Unix management.
//! All observations are captured before exact host cleanup and assertions, so a
//! regression failure cannot abandon the fixture's original native owner.

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
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let read = tokio::io::AsyncReadExt::read(&mut stream, &mut buffer).await?;
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
