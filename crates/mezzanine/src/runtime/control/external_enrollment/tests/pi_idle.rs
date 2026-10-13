//! Bare-installed ordinary Pi idle renewal, real observer EOF and producer exit.
//!
//! Actual CLI publication, child package discovery and kernel writer/root proof
//! precede every assertion. The concrete UnixOriginStream remains held across
//! actor stop/returned-service inspection so no clone fabricates observer health.
//! Only this fixture's registration deadline is aged; origin/clock/identity and
//! accounting are never rewritten. Normal read-only commands emit the real idle
//! key, and manually delivering that emitted matching timer after its actual
//! delay runs production reconciliation (not the full daemon timer adapter).
//! Private gates sequence the synthetic vendor only. Separate shell exit status
//! and post-assert result qualify child completion, not socket EOF. This is not
//! real SDK, paid-provider, OS-root replacement, spool or zero-invoice evidence.

use super::*;
use crate::host::async_runtime::{
    AsyncRuntimeActorConfig, AsyncRuntimeControlConnectionConfig, AsyncRuntimeSessionActor,
    serve_authenticated_async_runtime_control_connection_loop_with_snapshots,
};
use crate::runtime::{
    RuntimeEvent, RuntimeEventBatch, RuntimeSideEffect, RuntimeTimerKind, TimerEvent,
};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

/// Bounded private actual wire, never printed as a failure diagnostic.
#[derive(Default)]
struct Wire {
    requests: Vec<u8>,
    replies: Vec<u8>,
}

/// Audits only bytes read/written by a concrete native-qualified adapter.
struct AuditIo<S> {
    inner: S,
    wire: Arc<std::sync::Mutex<Wire>>,
}

impl<S: AsyncRead + Unpin> AsyncRead for AuditIo<S> {
    /// Captures actual kernel-qualified reads within a fixed memory bound.
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buffer.filled().len();
        let result = Pin::new(&mut this.inner).poll_read(cx, buffer);
        if matches!(result, Poll::Ready(Ok(()))) {
            let mut wire = this.wire.lock().unwrap();
            assert!(wire.requests.len() + buffer.filled().len() - before <= 32768);
            wire.requests.extend_from_slice(&buffer.filled()[before..]);
        }
        result
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for AuditIo<S> {
    /// Captures only successfully written real replies, never fabricating ACKs.
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.inner).poll_write(cx, bytes);
        if let Poll::Ready(Ok(written)) = result {
            let mut wire = this.wire.lock().unwrap();
            assert!(wire.replies.len() + written <= 32768);
            wire.replies.extend_from_slice(&bytes[..written]);
        }
        result
    }
    /// Delegates the real transport flush owner.
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }
    /// Delegates actual socket shutdown rather than marking a clone disconnected.
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

/// Best-effort panic cleanup rechecks owned process incarnations before numeric
/// signalling; it is not a race-free kernel PID-handle kill guarantee.
#[derive(Default)]
struct Processes(Vec<mez_mux::process::ProcessParentIdentity>);
impl Drop for Processes {
    /// Birth/parent checks prevent PID-reuse cleanup from targeting another run.
    fn drop(&mut self) {
        for identity in self.0.iter().rev() {
            if mez_mux::process::process_parent_identity_for_pid(identity.process_id)
                == Some(*identity)
            {
                // SAFETY: this exact owned fixture incarnation was just reobserved.
                unsafe {
                    libc::kill(identity.process_id as i32, libc::SIGKILL);
                }
            }
        }
    }
}

/// Owns only this private fixture directory, never real user configuration.
struct Directory(std::path::PathBuf);
impl Drop for Directory {
    /// Cleanup remains bounded to the unique test-owned root.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Metadata that idle maintenance must retain, excluding private credentials.
#[derive(PartialEq, Eq)]
struct Registration {
    generation: u64,
    epoch: u64,
    sequence: u64,
    state: String,
    owner: String,
}

/// Reads the actual single registration without granting any observer rights.
fn registration(service: &RuntimeSessionService) -> Registration {
    assert_eq!(service.control.external_agents().bindings.len(), 1);
    let binding = service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap();
    let presentation = binding
        .registration
        .as_ref()
        .unwrap()
        .presentation
        .as_ref()
        .unwrap();
    Registration {
        generation: binding.generation,
        epoch: binding.enrollment.as_ref().unwrap().epoch,
        sequence: presentation.sequence,
        state: presentation.state.clone(),
        owner: binding.accounting_owner.clone(),
    }
}

/// Returns actual lease metadata; read-only status/list must not update it.
fn expiry(service: &RuntimeSessionService) -> u64 {
    service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap()
        .expires
}

/// Ages ONLY the fixture registration deadline, never native evidence or billing UTC.
fn age_lease(service: &mut RuntimeSessionService) -> u64 {
    let aged = crate::runtime::current_unix_seconds().saturating_sub(1);
    service
        .control
        .external_agents_mut()
        .bindings
        .values_mut()
        .next()
        .unwrap()
        .expires = aged;
    aged
}

/// Original owner/request/project/admission/link fields, retained even on exit.
type RequestRow = (String, String, Option<String>, i64, Option<String>);

/// Durable metadata/expense snapshots keep unreported vendor cost unknown.
#[derive(PartialEq, Eq)]
struct Ledger {
    requests: Vec<RequestRow>,
    sources: Vec<(String, String)>,
    observations: i64,
    streams: i64,
    expenses: i64,
}

/// Inspects only the owned SQLite store; no parent completes or reports usage.
fn ledger(path: &std::path::Path) -> Ledger {
    let db = rusqlite::Connection::open(path).unwrap();
    let requests = db.prepare("SELECT owner,request_id,project_id,admitted_at,observation_id FROM external_usage_requests ORDER BY request_id").unwrap().query_map([], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
    let sources = db
        .prepare("SELECT witness,owner FROM external_usage_sources ORDER BY owner")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let count = |table: &str| {
        db.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap()
    };
    Ledger {
        requests,
        sources,
        observations: count("external_usage_observations"),
        streams: count("external_usage_streams"),
        expenses: count("token_usage_events"),
    }
}

/// Decodes actual bounded control frames without logging private reply bodies.
fn frames(mut bytes: &[u8]) -> Vec<serde_json::Value> {
    let mut result = Vec::new();
    while !bytes.is_empty() {
        let (body, used) = crate::control_client::decode_control_frame(bytes, 8192).unwrap();
        result.push(serde_json::from_str(&body).unwrap());
        bytes = &bytes[used..];
    }
    result
}

/// Waits for a scalar child phase or real shell status, with content-free errors.
async fn wait_file(home: &std::path::Path, name: &str, expected: Option<&str>) -> String {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(value) = std::fs::read_to_string(home.join(name))
                && expected.is_none_or(|expected| value.trim() == expected)
            {
                return value;
            }
            if let Ok(bytes) = std::fs::read(home.join("idle-result")) {
                let result: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                assert_ne!(
                    result["complete"], false,
                    "ordinary child failed its assertions"
                );
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("ordinary child did not publish expected {name}"))
}

/// Releases only synthetic vendor sequencing; no SDK facts or capabilities are supplied.
fn release(home: &std::path::Path, phase: &str) {
    std::fs::write(home.join("idle-next.tmp"), phase).unwrap();
    std::fs::rename(home.join("idle-next.tmp"), home.join("idle-next")).unwrap();
}

/// Normal read-only actor status/list calls arm a real idle timer but do not run
/// lease renewal. Optionally deliver exactly its emitted key after its bounded
/// real delay; the actor's matching-key handler owns production maintenance.
async fn inspect_timer(
    service: RuntimeSessionService,
    primary: &mez_core::ids::ClientId,
    apply: bool,
    present: bool,
) -> RuntimeSessionService {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let task = tokio::spawn(actor.run());
    let status = handle
        .execute_agent_shell_command(primary.clone(), "/status".into())
        .await
        .unwrap();
    assert!(!status.contains("PRIVATE_IDLE_CONTENT"));
    if present {
        assert!(status.contains("### External Telemetry"));
        assert!(status.contains("unavailable-source-continuity"));
    }
    let list = handle
        .execute_terminal_command(primary.clone(), "list-agents".into())
        .await
        .unwrap();
    assert!(!list.contains("PRIVATE_IDLE_CONTENT"));
    if present {
        assert!(list.contains("pi") || list.contains("Pi"));
    }
    assert!(
        handle
            .pending_agent_provider_tasks()
            .await
            .unwrap()
            .is_empty()
    );
    let timers = handle.drain_timer_side_effects(64).await.unwrap();
    let matching = timers
        .into_iter()
        .filter_map(|effect| match effect {
            RuntimeSideEffect::ScheduleTimer { key, delay_ms }
                if key.kind == RuntimeTimerKind::IdleCleanup =>
            {
                Some((key, delay_ms))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "external-only idle maintenance must emit one active key"
    );
    if apply {
        let (key, delay) = matching.into_iter().next().unwrap();
        assert!(
            delay > 0 && delay <= 5000,
            "maintenance delay is bounded, never a lease-length sleep"
        );
        tokio::time::sleep(Duration::from_millis(delay)).await;
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Timer(TimerEvent {
            key,
            now_ms: crate::runtime::current_unix_millis(),
        }));
        assert_eq!(
            handle.submit_runtime_events(batch).await.unwrap().accepted,
            1
        );
    }
    handle.shutdown().await.unwrap();
    task.await.unwrap().service
}

/// Runs actual bare default-root installation twice, never a manual artifact renderer.
fn bootstrap(helper: &std::path::Path, home: &std::path::Path) {
    for expected in ["installed", "unchanged"] {
        let output = std::process::Command::new(helper)
            .args(["--json", "bootstrap", "pi"])
            .env_clear()
            .env("HOME", home)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(output.status.success(), "bare installation failed");
        assert!(output.stderr.is_empty());
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["root_source"], "vendor-default");
        assert_eq!(report["result"], expected);
        assert_eq!(report["lifecycle"], "unverified");
        assert_eq!(report["usage"], "unverified");
    }
}

/// Composes CLI, native callback delivery, exact timer ownership and lifetime
/// changes while retaining one concrete observer adapter until the tested EOF.
async fn qualify(eof: bool) {
    use std::os::unix::fs::PermissionsExt;
    let mode = if eof { "eof" } else { "exit" };
    let node = std::path::PathBuf::from(
        std::env::var_os("MEZ_TEST_NODE_BINARY").expect("explicit already-installed Node path"),
    );
    let helper = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("mez");
    assert!(node.is_absolute() && node.is_file() && helper.is_file());
    let directory = std::fs::canonicalize("/tmp").unwrap().join(format!(
        "mez-pi-idle-{}-{:x}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir(&directory).unwrap();
    let _directory = Directory(directory.clone());
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = directory.join("home");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(home.join("idle-mode"), mode).unwrap();
    bootstrap(&helper, &home);
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/pi-native-idle-fixture.mjs")
        .canonicalize()
        .unwrap();
    let bin = directory.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let pi = bin.join("pi");
    std::fs::write(
        &pi,
        format!(
            "#!/bin/sh\n{} {}\nstatus=$?\nprintf '%s\\n' \"$status\" > {}\nexit \"$status\"\n",
            shlex::try_quote(node.to_str().unwrap()).unwrap(),
            shlex::try_quote(script.to_str().unwrap()).unwrap(),
            shlex::try_quote(home.join("idle-exit-status").to_str().unwrap()).unwrap()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&pi, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = directory.join("control.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    crate::runtime::enable_unix_writer_credentials(listener.as_raw_fd()).unwrap();
    let usage = directory.join("usage.sqlite");
    let mut service = RuntimeServiceFixture::new().control_socket(&path).build();
    let primary = service
        .attach_primary(
            "idle-user",
            true,
            mez_mux::layout::Size::new(100, 40).unwrap(),
            120,
        )
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    let root = mez_mux::process::process_parent_identity_for_pid(
        service.primary_pid_for_live_pane_process("%1").unwrap(),
    )
    .unwrap();
    let mut processes = Processes(vec![root]);
    service
        .session
        .set_pane_title_explicit("%1", "Pinned idle title")
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let native = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let clients = service.session().clients().len();
    service.toggle_active_agent_shell().unwrap();
    service.set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&usage));
    service
        .write_runtime_pane_input(
            "%1",
            format!(
                "HOME={} PATH={}:$PATH pi\n",
                shlex::try_quote(home.to_str().unwrap()).unwrap(),
                shlex::try_quote(bin.to_str().unwrap()).unwrap()
            )
            .as_bytes(),
        )
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let task = tokio::spawn(actor.run());
    let (mut socket, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let uid = crate::runtime::current_effective_uid();
    let origin = Arc::new(crate::runtime::capture_unix_origin(socket.as_raw_fd(), uid).unwrap());
    assert_eq!(
        mez_mux::process::process_executable_identity_for_pid(origin.identity.process_id)
            .unwrap()
            .executable_path,
        node.canonicalize().unwrap()
    );
    processes.0.push(origin.identity);
    processes.0.push(
        mez_mux::process::process_parent_identity_for_pid(origin.identity.parent_process_id)
            .unwrap(),
    );
    let mut connection = ControlConnectionState::new(true, false);
    connection
        .bind_authenticated_peer(AuthenticatedPeer::unix_user(origin.uid()))
        .unwrap();
    connection.bind_unix_origin(origin.clone()).unwrap();
    let wire = Arc::new(std::sync::Mutex::new(Wire::default()));
    let qualified = crate::runtime::UnixOriginStream::new(&mut socket, Some(origin.clone()));
    let mut io = AuditIo {
        inner: qualified,
        wire: wire.clone(),
    };
    let expected_frames = if eof { 2 } else { 4 };
    let served = serve_authenticated_async_runtime_control_connection_loop_with_snapshots(
        &mut io,
        AuthenticatedPeer::unix_user(uid),
        &handle,
        &mut connection,
        AsyncRuntimeControlConnectionConfig::new(8192, uid).unwrap(),
        None,
        |served, _| served >= expected_frames,
    )
    .await
    .unwrap();
    assert_eq!(served, expected_frames);
    assert!(!connection.initialized());
    assert!(connection.caller_client_id().is_none());
    assert!(origin.writer_confirmed() && origin.observer_connected());
    wait_file(&home, "idle-phase", Some("silent")).await;
    handle.shutdown().await.unwrap();
    let mut service = task.await.unwrap().service;
    // The actual adapter remains held, not replaced by a connected-state clone.
    assert!(origin.is_live() && origin.observer_connected());
    let original = registration(&service);
    let initial_ledger = ledger(&usage);
    assert_eq!(
        (
            initial_ledger.expenses,
            initial_ledger.observations,
            initial_ledger.streams
        ),
        (0, 0, 0)
    );
    assert_eq!(initial_ledger.requests.len(), if eof { 0 } else { 1 });
    assert!(initial_ledger.requests.iter().all(|row| row.4.is_none()));
    let initial_wire = wire.lock().unwrap().requests.clone();
    service.toggle_active_agent_shell().unwrap();
    let aged = age_lease(&mut service);
    service = inspect_timer(service, &primary, false, true).await;
    assert_eq!(
        expiry(&service),
        aged,
        "read-only status/list changed the lease"
    );
    assert!(
        registration(&service) == original,
        "read-only discovery changed registration metadata"
    );
    assert!(
        ledger(&usage) == initial_ledger,
        "read-only discovery changed durable metadata"
    );
    // Poll the SAME native-qualified adapter during production maintenance.
    // Audit snapshots alone cannot detect unread traffic queued by the child;
    // any byte, EOF or read failure before the intentional boundary rejects.
    let mut unexpected = [0];
    service = tokio::select! {
        biased;
        _ = io.read(&mut unexpected) => panic!("silent producer emitted traffic or lost its observer"),
        service = inspect_timer(service,&primary,true,true) => service,
    };
    assert!(expiry(&service) > crate::runtime::current_unix_seconds());
    let renewed = expiry(&service);
    assert!(
        registration(&service) == original,
        "idle maintenance changed registration metadata"
    );
    assert!(
        ledger(&usage) == initial_ledger,
        "idle maintenance changed durable metadata"
    );
    assert!(
        wire.lock().unwrap().requests == initial_wire,
        "idle required a producer SDK/usage heartbeat"
    );
    assert!(service.agent_turn_ledger().turns().is_empty());
    release(&home, "silent");
    wait_file(&home, "idle-phase", Some("renewed")).await;
    // Bound a final actual read through the retained adapter before deliberately
    // dropping it (EOF mode) or releasing actual producer exit (exit mode).
    // Timeout proves only this quiet interval, never continuous/no-gap delivery.
    assert!(
        tokio::time::timeout(Duration::from_millis(100), io.read(&mut unexpected))
            .await
            .is_err(),
        "silent producer queued traffic or closed before the intentional boundary"
    );
    if eof {
        // Actual adapter drop/transport shutdown: a retained origin is NOT an
        // observer. Keep producer/root alive to isolate this lifetime boundary.
        io.shutdown().await.unwrap();
        drop(io);
        drop(socket);
        release(&home, "renewed");
        wait_file(&home, "idle-phase", Some("disconnected")).await;
        assert!(origin.is_live());
        assert!(!origin.observer_connected());
        assert_eq!(
            mez_mux::process::process_parent_identity_for_pid(root.process_id).unwrap(),
            root
        );
        let status = service
            .runtime_agent_status_display_with_options("%1", false)
            .unwrap();
        assert!(status.contains("connection-lost"));
        assert_eq!(expiry(&service), renewed);
        assert_eq!(service.external_agent_rows().len(), 1);
        service = inspect_timer(service, &primary, true, true).await;
        assert_eq!(
            expiry(&service),
            renewed,
            "idle renewed a disconnected adapter from producer/shell survival"
        );
        age_lease(&mut service);
        service = inspect_timer(service, &primary, true, false).await;
        assert!(service.external_agent_rows().is_empty());
        assert!(
            service
                .control
                .external_agents()
                .bindings
                .values()
                .all(|binding| binding.retired)
        );
        assert!(origin.is_live());
        assert!(
            ledger(&usage) == initial_ledger,
            "observer loss changed durable metadata"
        );
        release(&home, "disconnected");
    } else {
        release(&home, "renewed");
        // Consuming real EOF releases adapter observer ownership. The distinct
        // producer-death proof is native death under a still-live root with an
        // explicitly unexpired lease, never a fabricated disconnected marker.
        let mut byte = [0];
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(10), io.read(&mut byte))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        wait_file(&home, "idle-exit-status", Some("0")).await;
        assert!(!origin.is_live());
        assert_eq!(
            mez_mux::process::process_parent_identity_for_pid(root.process_id).unwrap(),
            root
        );
        assert!(
            renewed > crate::runtime::current_unix_seconds(),
            "producer death must be observed before its remaining lease expires"
        );
        assert!(service.external_agent_rows().is_empty());
        assert_eq!(expiry(&service), renewed);
        assert!(
            service
                .control
                .external_agents()
                .bindings
                .values()
                .all(|binding| !binding.retired),
            "read-only discovery performed retirement"
        );
        service = inspect_timer(service, &primary, true, false).await;
        assert!(
            service
                .control
                .external_agents()
                .bindings
                .values()
                .all(|binding| binding.retired)
        );
        drop(io);
        drop(socket);
    }
    assert_eq!(
        wait_file(&home, "idle-exit-status", Some("0")).await.trim(),
        "0",
        "shell must prove actual Node completion status"
    );
    let result: serde_json::Value =
        serde_json::from_str(&wait_file(&home, "idle-result", None).await).unwrap();
    assert_eq!(result["complete"], true);
    assert_eq!(result["mode"], mode);
    assert_eq!(result["callbacks"], if eof { 1 } else { 3 });
    assert_eq!(result["sdk_shutdown"], false);
    assert_eq!(result["usage"], "unavailable-source-continuity");
    assert_eq!(service.session().clients().len(), clients);
    assert_eq!(
        service.agent_shell_store().get("%1").unwrap().session_id,
        native
    );
    assert_eq!(
        service.session().pane_title_state("%1").unwrap().0,
        "Pinned idle title"
    );
    assert!(service.agent_turn_ledger().turns().is_empty());
    assert!(
        ledger(&usage) == initial_ledger,
        "missing finalized usage must remain unreported, never invented zero cost"
    );
    let recorded = wire.lock().unwrap();
    assert!(
        recorded.requests == initial_wire,
        "unexpected producer traffic must not dump private protocol bytes"
    );
    let requests = frames(&recorded.requests);
    let replies = frames(&recorded.replies);
    assert_eq!(
        requests
            .iter()
            .filter(|row| row["method"] == "agent/external/enroll")
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|row| row["method"] == "agent/external/pi-observation")
            .count(),
        if eof { 1 } else { 2 }
    );
    assert!(
        !requests
            .iter()
            .any(|row| row["method"] == "agent/external/usage-end"
                || row["method"] == "agent/external/renew")
    );
    assert!(!String::from_utf8_lossy(&recorded.requests).contains("PRIVATE_IDLE_CONTENT"));
    let enrolled = replies
        .iter()
        .find(|row| row["result"]["registered"] == true)
        .unwrap();
    assert!(enrolled["result"]["controls"] == serde_json::json!([]));
    assert!(
        result["run"] == enrolled["result"]["run_id"],
        "child run differs from actual enrollment"
    );
    assert!(
        result["epoch"] == enrolled["result"]["observer_epoch"],
        "child epoch differs from actual enrollment"
    );
    drop(recorded);
    service.terminate_all_pane_processes().unwrap();
}

/// A silent installed producer keeps a genuine observer alive while matching
/// daemon idle work renews without SDK callbacks or expense. Actual observer
/// EOF stops renewal despite live producer/shell, then finite expiry retires.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit already-installed Node and fresh sibling mez required; actual native installed idle/EOF acceptance"]
async fn external_pi_installed_idle_timer_renews_and_real_eof_expires() {
    tokio::time::timeout(Duration::from_secs(60), qualify(true))
        .await
        .unwrap();
}

/// A silent installed producer with an unfinished native request exits without
/// SDK shutdown/finalized usage. The still-live shell cannot retain its row;
/// reads hide it without retirement/renewal and matching maintenance retires
/// exactly that source while the original request remains unreported.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit already-installed Node and fresh sibling mez required; actual native installed producer-exit acceptance"]
async fn external_pi_installed_producer_exit_without_sdk_end_retires_under_live_shell() {
    tokio::time::timeout(Duration::from_secs(60), qualify(false))
        .await
        .unwrap();
}

/// Abnormal setup must reject prohibited activation strings and unexpected
/// owner objects without Node assertion diffs exposing their values. No SDK,
/// daemon, artifact stub or actual private user input is needed: unique owned
/// sentinels fail before filesystem discovery, and all child output is captured
/// privately. Static messages and genuine nonzero exit qualify rejection.
#[test]
#[ignore = "explicit already-installed Node required; offline fixture privacy qualification"]
fn external_pi_idle_setup_rejects_private_values_without_diagnostic_exposure() {
    let node = std::path::PathBuf::from(
        std::env::var_os("MEZ_TEST_NODE_BINARY").expect("explicit already-installed Node path"),
    );
    assert!(node.is_absolute() && node.is_file());
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/pi-native-idle-fixture.mjs")
        .canonicalize()
        .unwrap();
    let home = std::fs::canonicalize("/tmp").unwrap().join(format!(
        "mez-pi-idle-privacy-{}-{:x}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir(&home).unwrap();
    let _directory = Directory(home.clone());
    let sentinel = "SYNTHETIC_IDLE_PRIVATE_VALUE_NOT_A_CREDENTIAL";
    for key in [
        "PI_CODING_AGENT_DIR",
        "MEZ_HARNESS_SESSION",
        "MEZ_HARNESS_OBSERVER_FD",
        "MEZ_HARNESS_HELPER",
        "MEZ_EXTERNAL_LAUNCH_TOKEN",
    ] {
        let output = std::process::Command::new("timeout")
            .arg("5s")
            .arg(&node)
            .arg(&script)
            .env_clear()
            .env("HOME", &home)
            .env(key, sentinel)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(
            output
                .status
                .code()
                .is_some_and(|code| code != 0 && code != 124),
            "setup must actually reject, not timeout"
        );
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("activation input supplied"));
        assert!(
            !stderr.contains(sentinel),
            "setup diagnostic exposed a synthetic activation value"
        );
    }
    let preload = format!(
        "data:text/javascript,globalThis[Symbol.for(%22mezzanine.pi.ordinary-owner.v1%22)]={{privateToken:%22{sentinel}%22}}"
    );
    let output = std::process::Command::new("timeout")
        .arg("5s")
        .arg(&node)
        .args(["--import", &preload])
        .arg(&script)
        .env_clear()
        .env("HOME", &home)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        output
            .status
            .code()
            .is_some_and(|code| code != 0 && code != 124)
    );
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("factory opened resources"));
    assert!(
        !stderr.contains(sentinel),
        "setup diagnostic exposed an unexpected owner object"
    );
}
