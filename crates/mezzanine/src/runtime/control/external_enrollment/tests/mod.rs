//! Ordinary pane-process admission tests, with no supplied PID/token/fd3 proof.

use super::*;
use crate::test_support::runtime::RuntimeServiceFixture;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;

/// Owns the real shell, unique socket directory and accepted child connection.
/// Every exit closes the child socket and terminates only fixture pane roots.
struct Fixture {
    service: RuntimeSessionService,
    directory: std::path::PathBuf,
    socket: tokio::net::UnixStream,
    connection: ControlConnectionState,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.service.terminate_all_pane_processes();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// Starts a normal shell, writes a normally invoked Rust fake vendor command,
/// and accepts its own socket. Only nonsecret standard MEZ discovery is inherited.
async fn fixture(mode: &str) -> Option<Fixture> {
    let directory = std::path::Path::new("/tmp").join(format!(
        "mez-enroll-{}-{:x}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("control.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    crate::runtime::enable_unix_writer_credentials(listener.as_raw_fd()).unwrap();
    let mut service = RuntimeServiceFixture::new().control_socket(&path).build();
    service.start_initial_pane_process(None).unwrap();
    let executable = std::env::current_exe().unwrap();
    let command = format!(
        "MEZ_TEST_ENROLL_MODE={} {} --exact runtime::control::external_enrollment::tests::external_enrollment_ordinary_child_fixture --ignored --quiet\n",
        shlex::try_quote(mode).unwrap(),
        shlex::try_quote(executable.to_str().unwrap()).unwrap()
    );
    service
        .write_runtime_pane_input("%1", command.as_bytes())
        .unwrap();
    let (mut socket, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let uid = crate::runtime::current_effective_uid();
    let captured = crate::runtime::capture_unix_origin(socket.as_raw_fd(), uid);
    let origin = match captured {
        Ok(origin) => origin,
        Err(error) if error.raw_os_error() == Some(libc::ENOPROTOOPT) => {
            service.terminate_all_pane_processes().unwrap();
            std::fs::remove_dir_all(&directory).unwrap();
            return None;
        }
        Err(error) => panic!("native origin capture failed: {error}"),
    };
    assert_ne!(origin.identity.process_id, std::process::id());
    let mut connection = ControlConnectionState::new(true, false);
    connection
        .bind_authenticated_peer(AuthenticatedPeer::unix_user(uid))
        .unwrap();
    connection.bind_unix_origin(Arc::new(origin)).unwrap();
    if mode == "hold" {
        use tokio::io::AsyncReadExt;
        let mut qualified =
            crate::runtime::UnixOriginStream::new(&mut socket, connection.unix_origin().cloned());
        let mut ready = [0];
        tokio::time::timeout(Duration::from_secs(10), qualified.read_exact(&mut ready))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ready, [1]);
        assert!(connection.unix_origin().unwrap().writer_confirmed());
    }
    Some(Fixture {
        service,
        directory,
        socket,
        connection,
    })
}

/// Produces strict inert lifecycle metadata; caller-supplied PIDs never occur.
fn request() -> JsonRpcRequest {
    crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"enroll","method":"agent/external/enroll",
        "params":{"pane_id":"%1","harness":"pi","version":"fixture","external_session_id":"session-a",
        "display_name":"ordinary fixture","observer_kind":"persistent","observer_instance":"fixture-instance-a"}}).to_string()).unwrap()
}

/// Runs a real ordinary process in the pane. Hold mode supports native owner
/// tests; flow mode exercises framed enrollment, retry and producer-bound renew.
/// No provider, launcher, inherited observer FD, native PID override or token is used.
#[test]
#[ignore = "self-executing ordinary producer fixture"]
fn external_enrollment_ordinary_child_fixture() {
    let Some(mode) = std::env::var_os("MEZ_TEST_ENROLL_MODE") else {
        return;
    };
    let discovery = std::env::var("MEZ").unwrap();
    let path = discovery
        .split(crate::runtime::MEZ_ENV_FIELD_SEPARATOR)
        .next()
        .unwrap();
    let mut socket = std::os::unix::net::UnixStream::connect(path).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    socket
        .set_write_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    if mode == "hold" {
        socket.write_all(&[1]).unwrap();
        loop {
            let mut release = [0];
            socket.read_exact(&mut release).unwrap();
            if release == [2] {
                return;
            }
            assert_eq!(release, [3]);
            socket.write_all(&[1]).unwrap();
        }
    }
    assert_eq!(mode, "flow");
    let enroll = serde_json::json!({"jsonrpc":"2.0","id":"enroll","method":"agent/external/enroll",
        "params":{"pane_id":std::env::var("MEZ_PANE").unwrap(),"harness":"pi","version":"fixture",
        "external_session_id":"session-a","display_name":"ordinary fixture","observer_kind":"persistent","observer_instance":"fixture-instance-a"}});
    let initial = exchange(&mut socket, &enroll);
    assert!(
        initial.get("error").is_none(),
        "ordinary enrollment rejected"
    );
    assert_eq!(initial["result"]["registered"], true);
    assert_eq!(initial["result"]["controls"], serde_json::json!([]));
    assert_eq!(initial["result"]["usage"], "unavailable-source-continuity");
    let retry = exchange(&mut socket, &enroll);
    assert!(retry.get("error").is_none(), "retry rejected");
    assert_eq!(
        retry["result"]["generation"],
        initial["result"]["generation"]
    );
    assert!(
        retry["result"]["launch_token"] == initial["result"]["launch_token"],
        "retry returned a different private handle"
    );
    let renew = serde_json::json!({"jsonrpc":"2.0","id":"renew","method":"agent/external/renew",
        "params":{"launch_token":initial["result"]["launch_token"],"generation":initial["result"]["generation"],"external_session_id":"session-a"}});
    assert!(
        exchange(&mut socket, &renew).get("error").is_none(),
        "renew rejected"
    );
    let denied = exchange(
        &mut socket,
        &serde_json::json!({"jsonrpc":"2.0","id":"role","method":"pane/list","params":{}}),
    );
    assert!(
        denied.get("error").is_some(),
        "enrollment granted a control role"
    );
}

/// Exchanges a finite frame on the fixture's own connection, never retaining or
/// printing credentials. Socket deadlines bound fragmented reply accumulation.
fn exchange(
    socket: &mut std::os::unix::net::UnixStream,
    body: &serde_json::Value,
) -> serde_json::Value {
    socket
        .write_all(&crate::control::encode_control_body(&body.to_string()))
        .unwrap();
    let mut output = Vec::new();
    loop {
        let mut chunk = [0; 1024];
        let n = socket.read(&mut chunk).unwrap();
        assert_ne!(n, 0, "fixture reply unexpectedly closed");
        output.extend_from_slice(&chunk[..n]);
        assert!(output.len() <= 8192);
        if let Ok((body, consumed)) = crate::control::decode_control_frame(&output, 8192) {
            assert_eq!(consumed, output.len());
            return serde_json::from_str(&body).unwrap();
        }
    }
}

/// Genuine kernel-origin/ancestry evidence admits an ordinary local descendant,
/// retries preserve handle/run, and client membership never changes. An unrelated
/// same-user holder of the token or unimplemented accounting source is rejected.
#[tokio::test(flavor = "current_thread")]
async fn external_enrollment_native_descendant_registers_without_primary() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let clients = fixture.service.session().clients().len();
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    let response =
        fixture
            .service
            .complete_external_enrollment(work, observed, &fixture.connection);
    let initial: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert!(initial.get("error").is_none(), "{response}");
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    let retry: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, &fixture.connection),
    )
    .unwrap();
    assert!(
        initial["result"]["launch_token"] == retry["result"]["launch_token"],
        "retry returned a different private handle"
    );
    assert_eq!(
        initial["result"]["generation"],
        retry["result"]["generation"]
    );
    assert_eq!(fixture.service.control.external_agents().bindings.len(), 1);
    assert_eq!(fixture.service.session().clients().len(), clients);
    assert!(!fixture.connection.initialized());
    assert!(fixture.connection.caller_client_id().is_none());

    let params = serde_json::json!({"launch_token":initial["result"]["launch_token"],"generation":initial["result"]["generation"],"external_session_id":"session-a"});
    let renew = crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"renew","method":"agent/external/renew","params":params}).to_string()).unwrap();
    let (socket, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
    let uid = crate::runtime::current_effective_uid();
    let mut stolen = ControlConnectionState::new(true, false);
    stolen
        .bind_authenticated_peer(AuthenticatedPeer::unix_user(uid))
        .unwrap();
    stolen
        .bind_unix_origin(Arc::new(
            crate::runtime::capture_unix_origin(socket.as_raw_fd(), uid).unwrap(),
        ))
        .unwrap();
    assert!(
        fixture
            .service
            .dispatch_external_agent_request(&renew, &stolen)
            .is_err()
    );
    assert!(
        fixture
            .service
            .dispatch_external_agent_request(&renew, &fixture.connection)
            .is_ok()
    );
    let usage = crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"usage","method":"agent/external/usage","params":params}).to_string()).unwrap();
    assert!(
        fixture
            .service
            .prepare_external_usage(&usage, &fixture.connection)
            .is_err()
    );
}

/// Authority and grammar failures happen before reservation. Expired observation
/// and duplicate completion settle without creating a registration; finite
/// pending native work capacity is reclaimed only when completion is consumed.
#[tokio::test(flavor = "current_thread")]
async fn external_enrollment_fences_bounds_and_reclaims_reservations() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    for extra in ["pid", "ppid", "launch_token", "target", "idempotency_key"] {
        let mut params: serde_json::Value =
            serde_json::from_str(request().params.as_deref().unwrap()).unwrap();
        params[extra] = serde_json::json!(1);
        let bad = crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"agent/external/enroll","params":params}).to_string()).unwrap();
        assert!(
            fixture
                .service
                .prepare_external_enrollment(&bad, &fixture.connection)
                .is_err()
        );
    }
    let mut remote = ControlConnectionState::new(true, true);
    remote
        .bind_authenticated_peer(AuthenticatedPeer::iroh_endpoint("remote"))
        .unwrap();
    assert!(
        fixture
            .service
            .prepare_external_enrollment(&request(), &remote)
            .is_err()
    );
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .enrollments
            .pending
            .is_empty()
    );
    let mut pending = Vec::new();
    for _ in 0..MAX_PENDING {
        pending.push(
            fixture
                .service
                .prepare_external_enrollment(&request(), &fixture.connection)
                .unwrap(),
        );
    }
    assert!(
        fixture
            .service
            .prepare_external_enrollment(&request(), &fixture.connection)
            .is_err()
    );
    for work in pending {
        fixture.service.complete_external_enrollment(
            work,
            Err(MezError::conflict("test worker failure")),
            &fixture.connection,
        );
    }
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .enrollments
            .pending
            .is_empty()
    );
    let mut expired = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    expired.deadline = Instant::now();
    assert!(expired.observe().is_err());
    assert!(
        fixture
            .service
            .complete_external_enrollment(expired, Ok(()), &fixture.connection)
            .contains("error")
    );
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let replay = work.clone();
    fixture.service.complete_external_enrollment(
        work,
        Err(MezError::conflict("test worker failure")),
        &fixture.connection,
    );
    assert!(
        fixture
            .service
            .complete_external_enrollment(replay, Ok(()), &fixture.connection)
            .contains("already settled")
    );
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .is_empty()
    );
}

/// Strict metadata rejects unsupported helper/retired harness contracts and
/// enforces the exact 4096-byte boundary before reserving native observation.
#[tokio::test(flavor = "current_thread")]
async fn external_enrollment_metadata_boundary_and_contracts_are_strict() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let raw = request().params.unwrap();
    let mut exact = request();
    exact.params = Some(format!("{}{}", raw, " ".repeat(4096 - raw.len())));
    let work = fixture
        .service
        .prepare_external_enrollment(&exact, &fixture.connection)
        .unwrap();
    fixture.service.complete_external_enrollment(
        work,
        Err(MezError::conflict("test discarded observation")),
        &fixture.connection,
    );
    exact.params.as_mut().unwrap().push(' ');
    assert!(
        fixture
            .service
            .prepare_external_enrollment(&exact, &fixture.connection)
            .unwrap_err()
            .message()
            .contains("metadata exceeds limit")
    );
    for (field, value) in [
        ("observer_kind", serde_json::json!("helper")),
        ("harness", serde_json::json!("gemini")),
        ("harness", serde_json::json!("codex")),
        ("display_name", serde_json::json!(false)),
        ("observer_instance", serde_json::json!("")),
        ("observer_instance", serde_json::json!(false)),
        ("predecessor_generation", serde_json::json!(0)),
        ("predecessor_generation", serde_json::json!("1")),
        ("predecessor_generation", serde_json::Value::Null),
        ("external_session_id", serde_json::json!("line\nfeed")),
    ] {
        let mut changed = request();
        let mut params: serde_json::Value = serde_json::from_str(&raw).unwrap();
        params[field] = value;
        changed.params = Some(params.to_string());
        assert!(
            fixture
                .service
                .prepare_external_enrollment(&changed, &fixture.connection)
                .is_err()
        );
    }
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .is_empty()
    );
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .enrollments
            .pending
            .is_empty()
    );
}

/// The actor admits exactly one bounded 8192-byte enrollment body. Oversized
/// bodies and batched frames reject before reservation; the exact boundary still
/// traverses native observation and returns an ordinary producer registration.
#[tokio::test(flavor = "current_thread")]
async fn external_enrollment_actor_frame_boundary_is_exact() {
    use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let service = std::mem::replace(&mut fixture.service, RuntimeServiceFixture::new().build());
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let actor_task = tokio::spawn(actor.run());
    let base = serde_json::json!({"jsonrpc":"2.0","id":"bound","method":"agent/external/enroll",
        "params":serde_json::from_str::<serde_json::Value>(request().params.as_deref().unwrap()).unwrap()}).to_string();
    let exact = format!("{}{}", base, " ".repeat(8192 - base.len()));
    let large = crate::control::encode_control_body(&format!("{exact} "));
    let response = handle
        .handle_control_input_for_connection(large, 16384, fixture.connection.clone())
        .await
        .unwrap();
    let (body, _) = crate::control::decode_control_frame(&response.output, 16384).unwrap();
    assert!(body.contains("one bounded control frame"));
    let mut batched = crate::control::encode_control_body(&base);
    batched.extend_from_slice(&crate::control::encode_control_body(&base));
    let response = handle
        .handle_control_input_for_connection(batched, 16384, fixture.connection.clone())
        .await
        .unwrap();
    let (body, _) = crate::control::decode_control_frame(&response.output, 16384).unwrap();
    assert!(body.contains("one bounded control frame"));
    let response = handle
        .handle_control_input_for_connection(
            crate::control::encode_control_body(&exact),
            16384,
            fixture.connection.clone(),
        )
        .await
        .unwrap();
    let (body, _) = crate::control::decode_control_frame(&response.output, 16384).unwrap();
    assert!(
        serde_json::from_str::<serde_json::Value>(&body)
            .unwrap()
            .get("error")
            .is_none()
    );
    handle.shutdown().await.unwrap();
    let mut exit = actor_task.await.unwrap();
    assert_eq!(exit.service.control.external_agents().bindings.len(), 1);
    assert!(
        exit.service
            .control
            .external_agents()
            .enrollments
            .pending
            .is_empty()
    );
    exit.service.terminate_all_pane_processes().unwrap();
}

/// An identical enrolled run needs only bounded observation/reply capacity, not
/// another registration slot. Saturation cannot prevent recovering its original
/// private handle after a lost reply, and conflicting metadata cannot allocate.
#[tokio::test(flavor = "current_thread")]
async fn external_enrollment_identical_retry_survives_registry_saturation() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let primary = fixture
        .service
        .attach_primary(
            "fixture",
            true,
            mez_mux::layout::Size::new(80, 24).unwrap(),
            120,
        )
        .unwrap();
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    let initial: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, &fixture.connection),
    )
    .unwrap();
    assert!(initial.get("error").is_none());
    for _ in 1..super::super::external_agents::MAX_BINDINGS {
        // Existing primary-owned launches consume real production slots without
        // fabricating native producer identities or expanding enrollment rights.
        let issued = fixture.service.dispatch_runtime_control_body(
            r#"{"jsonrpc":"2.0","id":"fill","method":"agent/external/launch","params":{"pane_id":"%1","harness":"pi","version":"fixture"}}"#, &primary);
        assert!(
            serde_json::from_str::<serde_json::Value>(&issued)
                .unwrap()
                .get("error")
                .is_none()
        );
    }
    assert_eq!(
        fixture.service.control.external_agents().bindings.len(),
        super::super::external_agents::MAX_BINDINGS
    );
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    let retry: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, &fixture.connection),
    )
    .unwrap();
    assert!(
        retry["result"]["launch_token"] == initial["result"]["launch_token"],
        "saturated retry changed private handle"
    );
    let mut changed = request();
    let mut params: serde_json::Value =
        serde_json::from_str(changed.params.as_deref().unwrap()).unwrap();
    params["display_name"] = serde_json::json!("different metadata");
    changed.params = Some(params.to_string());
    let work = fixture
        .service
        .prepare_external_enrollment(&changed, &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(
        fixture
            .service
            .complete_external_enrollment(work, observed, &fixture.connection)
            .contains("metadata differs")
    );
    assert_eq!(
        fixture.service.control.external_agents().bindings.len(),
        super::super::external_agents::MAX_BINDINGS
    );
}

/// A real unrelated same-user socket origin cannot use a forged pane hint to
/// enroll. Native proof also becomes unusable when a root is replaced after the
/// worker finished, or when the original connection changes before settlement.
#[tokio::test(flavor = "current_thread")]
async fn external_enrollment_unrelated_origin_and_stale_root_fail_closed() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let (mut socket, mut peer) = tokio::net::UnixStream::pair().unwrap();
    let uid = crate::runtime::current_effective_uid();
    let mut unrelated = ControlConnectionState::new(true, false);
    unrelated
        .bind_authenticated_peer(AuthenticatedPeer::unix_user(uid))
        .unwrap();
    unrelated
        .bind_unix_origin(Arc::new(
            crate::runtime::capture_unix_origin(socket.as_raw_fd(), uid).unwrap(),
        ))
        .unwrap();
    {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut qualified =
            crate::runtime::UnixOriginStream::new(&mut socket, unrelated.unix_origin().cloned());
        peer.write_all(&[1]).await.unwrap();
        let mut ready = [0];
        qualified.read_exact(&mut ready).await.unwrap();
        assert!(unrelated.unix_origin().unwrap().writer_confirmed());
    }
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &unrelated)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(observed.is_err());
    assert!(
        fixture
            .service
            .complete_external_enrollment(work, observed, &unrelated)
            .contains("error")
    );
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(observed.is_ok());
    assert!(
        fixture
            .service
            .complete_external_enrollment(work, observed, &unrelated)
            .contains("connection changed")
    );
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(observed.is_ok());
    fixture.service.terminate_all_pane_processes().unwrap();
    fixture.service.start_initial_pane_process(None).unwrap();
    assert!(
        fixture
            .service
            .complete_external_enrollment(work, observed, &fixture.connection)
            .contains("root or deadline changed")
    );
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .is_empty()
    );
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .enrollments
            .pending
            .is_empty()
    );
}

/// Native observation must leave the actor responsive and retain reservation
/// settlement after the caller abandons its reply. Release yields one original
/// registration, not replayed provider work or an initialized control client.
#[tokio::test(flavor = "current_thread")]
async fn external_enrollment_worker_is_off_actor_and_settles_lost_reply() {
    use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let settled = Arc::new(tokio::sync::Notify::new());
    fixture
        .service
        .control
        .external_agents_mut()
        .enrollments
        .worker_gate = Some((started.clone(), release.clone()));
    fixture
        .service
        .control
        .external_agents_mut()
        .enrollments
        .settled = Some(settled.clone());
    let service = std::mem::replace(&mut fixture.service, RuntimeServiceFixture::new().build());
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let actor_task = tokio::spawn(actor.run());
    let caller = handle.clone();
    let connection = fixture.connection.clone();
    let body = serde_json::json!({"jsonrpc":"2.0","id":"lost","method":"agent/external/enroll", "params":serde_json::from_str::<serde_json::Value>(request().params.as_deref().unwrap()).unwrap()}).to_string();
    let pending = tokio::spawn(async move {
        caller
            .handle_control_input_for_connection(
                crate::control::encode_control_body(&body),
                8192,
                connection,
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), handle.lifecycle_state())
        .await
        .unwrap()
        .unwrap();
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), settled.notified())
        .await
        .unwrap();
    let timers = handle.drain_timer_side_effects(8).await.unwrap();
    assert!(
        timers.iter().any(|effect| matches!(effect,
        crate::runtime::RuntimeSideEffect::ScheduleTimer { key, .. }
        if key.kind == crate::runtime::RuntimeTimerKind::IdleCleanup)),
        "first enrollment must arm idle cleanup even after reply loss"
    );
    let key = timers
        .iter()
        .find_map(|effect| match effect {
            crate::runtime::RuntimeSideEffect::ScheduleTimer { key, .. }
                if key.kind == crate::runtime::RuntimeTimerKind::IdleCleanup =>
            {
                Some(key.clone())
            }
            _ => None,
        })
        .unwrap();
    // No later vendor callback: the ordinary child exits after its fixture-only
    // release, and the originally scheduled maintenance event must retire it.
    tokio::io::AsyncWriteExt::write_all(&mut fixture.socket, &[2])
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while fixture.connection.unix_origin().unwrap().is_live() {
        assert!(Instant::now() < deadline, "ordinary producer did not exit");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let mut tick = crate::runtime::RuntimeEventBatch::new();
    tick.push(crate::runtime::RuntimeEvent::Timer(
        crate::runtime::TimerEvent { key, now_ms: 1000 },
    ));
    assert!(handle.submit_runtime_events(tick).await.unwrap().applied > 0);
    handle.shutdown().await.unwrap();
    let mut exit = actor_task.await.unwrap();
    assert!(
        exit.service
            .control
            .external_agents()
            .enrollments
            .pending
            .is_empty()
    );
    assert_eq!(exit.service.control.external_agents().bindings.len(), 1);
    assert!(
        exit.service
            .control
            .external_agents()
            .bindings
            .values()
            .all(|binding| binding.retired)
    );
    assert!(!fixture.connection.initialized());
    exit.service.terminate_all_pane_processes().unwrap();
}

/// A verified connected observer must renew from daemon maintenance without
/// callback traffic. Losing that transport must stop renewal even though the
/// stable producer remains alive; expiry is unavailable telemetry, not death.
#[tokio::test(flavor = "current_thread")]
async fn external_enrollment_daemon_renews_idle_connected_observer_only() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let origin = fixture.connection.unix_origin().unwrap().clone();
    let qualified =
        crate::runtime::UnixOriginStream::new(&mut fixture.socket, Some(origin.clone()));
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    let response =
        fixture
            .service
            .complete_external_enrollment(work, observed, &fixture.connection);
    assert!(
        serde_json::from_str::<serde_json::Value>(&response)
            .unwrap()
            .get("error")
            .is_none()
    );
    fixture
        .service
        .control
        .external_agents_mut()
        .bindings
        .values_mut()
        .next()
        .unwrap()
        .expires = current_unix_seconds() - 1;
    fixture
        .service
        .reconcile_agent_runtime_progress_paths_with_actor_progress(&BTreeSet::new())
        .unwrap();
    assert_eq!(fixture.service.reconcile_external_agent_registrations(), 0);
    let binding = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap();
    assert!(!binding.retired);
    assert!(binding.expires > current_unix_seconds());
    drop(qualified);
    assert!(
        origin.is_live(),
        "producer must remain distinct from lost observer"
    );
    fixture
        .service
        .control
        .external_agents_mut()
        .bindings
        .values_mut()
        .next()
        .unwrap()
        .expires = current_unix_seconds() - 1;
    assert_eq!(fixture.service.reconcile_external_agent_registrations(), 1);
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .all(|binding| binding.retired)
    );
}

/// Requalified observer sockets may replace lost connections without a new run.
/// Dead weak links and stale old adapter closure cannot erase a healthy current
/// observer, and the retained set never exceeds its finite 16-endpoint bound.
#[tokio::test(flavor = "current_thread")]
async fn external_enrollment_requalified_observer_preserves_idle_run() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    let initial: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, &fixture.connection),
    )
    .unwrap();
    assert!(initial.get("error").is_none());
    let uid = crate::runtime::current_effective_uid();
    let origin =
        Arc::new(crate::runtime::capture_unix_origin(fixture.socket.as_raw_fd(), uid).unwrap());
    let mut replacement = ControlConnectionState::new(true, false);
    replacement
        .bind_authenticated_peer(AuthenticatedPeer::unix_user(uid))
        .unwrap();
    replacement.bind_unix_origin(origin.clone()).unwrap();
    let mut qualified =
        crate::runtime::UnixOriginStream::new(&mut fixture.socket, Some(origin.clone()));
    qualified.write_all(&[3]).await.unwrap();
    let mut ready = [0];
    qualified.read_exact(&mut ready).await.unwrap();
    assert!(origin.writer_confirmed());
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &replacement)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    let retry: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, &replacement),
    )
    .unwrap();
    assert!(
        retry["result"]["launch_token"] == initial["result"]["launch_token"],
        "requalified observer changed run handle"
    );
    assert_eq!(retry["result"]["agent_id"], initial["result"]["agent_id"]);
    // Old connection data may remain retained, but its adapter is already gone.
    assert!(
        !fixture
            .connection
            .unix_origin()
            .unwrap()
            .observer_connected()
    );
    fixture
        .service
        .control
        .external_agents_mut()
        .bindings
        .values_mut()
        .next()
        .unwrap()
        .expires = current_unix_seconds() - 1;
    fixture
        .service
        .reconcile_agent_runtime_progress_paths_with_actor_progress(&BTreeSet::new())
        .unwrap();
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .all(|binding| !binding.retired && binding.expires > current_unix_seconds())
    );
    let binding = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap();
    assert!(binding.enrollment.as_ref().unwrap().observers.len() <= 16);
    drop(qualified);
    fixture
        .service
        .control
        .external_agents_mut()
        .bindings
        .values_mut()
        .next()
        .unwrap()
        .expires = current_unix_seconds() - 1;
    fixture
        .service
        .reconcile_agent_runtime_progress_paths_with_actor_progress(&BTreeSet::new())
        .unwrap();
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .all(|binding| binding.retired)
    );
}

/// A normally invoked producer speaks real framed Unix ingress through the actor
/// and native worker, enrolls/retries/renews, and cannot obtain an initialized
/// client. Its exact death retires telemetry even while the pane shell survives.
#[tokio::test(flavor = "current_thread")]
async fn external_enrollment_ordinary_unix_actor_roundtrip() {
    use crate::host::async_runtime::{
        AsyncRuntimeActorConfig, AsyncRuntimeControlConnectionConfig, AsyncRuntimeSessionActor,
        serve_async_runtime_control_connection_loop,
    };
    let Some(mut fixture) = fixture("flow").await else {
        return;
    };
    let service = std::mem::replace(&mut fixture.service, RuntimeServiceFixture::new().build());
    let clients = service.session().clients().len();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let server = async {
        serve_async_runtime_control_connection_loop(
            &mut fixture.socket,
            &handle,
            &mut fixture.connection,
            AsyncRuntimeControlConnectionConfig::new(8192, crate::runtime::current_effective_uid())
                .unwrap(),
            |served, _| served >= 4,
        )
        .await
        .unwrap();
        assert!(!fixture.connection.initialized());
        assert!(fixture.connection.caller_client_id().is_none());
        let origin = fixture.connection.unix_origin().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while origin.is_live() {
            assert!(Instant::now() < deadline, "ordinary fixture did not exit");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        handle.shutdown().await.unwrap();
    };
    let ((), mut exit) = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(server, actor.run())
    })
    .await
    .unwrap();
    assert_eq!(exit.service.session().clients().len(), clients);
    assert_eq!(exit.service.control.external_agents().bindings.len(), 1);
    exit.service.reconcile_external_agent_registrations();
    assert!(
        exit.service
            .control
            .external_agents()
            .bindings
            .values()
            .all(|binding| binding.retired)
    );
    exit.service.terminate_all_pane_processes().unwrap();
}

mod observer_epochs;
mod persistent_client;
mod pi_observation;

/// Programmatic requests still undergo strict selector decoding before native
/// reservations. Wire parsing is not the only boundary: escaped/duplicate
/// session, kind and predecessor values must leave admission state untouched.
#[tokio::test(flavor = "current_thread")]
async fn external_enrollment_direct_metadata_rejects_duplicates_before_reservation() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    for suffix in [
        r#", "harness":"pi""#,
        r#", "observer_kind":"persistent""#,
        r#", "external_session_id":"session-b""#,
        r#", "\u0070ane_id":"%2""#,
    ] {
        let mut candidate = request();
        let raw = candidate.params.as_ref().unwrap();
        candidate.params = Some(format!("{}{suffix}}}", &raw[..raw.len() - 1]));
        assert!(
            fixture
                .service
                .prepare_external_enrollment(&candidate, &fixture.connection)
                .is_err()
        );
        assert!(
            fixture
                .service
                .control
                .external_agents()
                .enrollments
                .pending
                .is_empty()
        );
        assert!(
            fixture
                .service
                .control
                .external_agents()
                .bindings
                .is_empty()
        );
    }
}
