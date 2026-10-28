//! Actual installed Pi ACK-loss and same-live registration-index recreation.
//!
//! The production CLI installs into isolated HOME and an ordinary Pi-shaped
//! process discovers its default factory. Native Unix writer/ancestor evidence
//! and the actor own every request/commit. A transport wrapper fails the first
//! real durable/applied reply before writing any bytes, never fabricating ACKs.
//! Reset stops the actor, retains its same service/control/root/store, clears only
//! the registration facade, then restarts actor processing. It is not an OS
//! daemon/root replacement, durable spool, vendor SDK, or invoice qualification.
//! Unknown warm recovery also checks the shared client's explicit cold attempt:
//! only the original checkpoint is sent, and retained warm authority must refuse.
//!
//! Final socket EOF is only transport evidence. Before final actor shutdown or
//! process cleanup, each path collects the pane shell's atomic actual Node exit
//! status and the child's post-assertion complete phase. Neither alone suffices.
//! A fixed scalar post-EOF assertion gate proves late failures are rejected on
//! all three paths without injecting callbacks, credentials or ledger reports.
//! Completion errors contain only fixed stages and numeric status; panic cleanup
//! still reobserves exact native PID/birth/parent identity before signalling.

use super::*;
use crate::host::async_runtime::{
    AsyncRuntimeActorConfig, AsyncRuntimeControlConnectionConfig, AsyncRuntimeSessionActor,
    AsyncRuntimeSessionHandle,
    serve_authenticated_async_runtime_control_connection_loop_with_snapshots,
};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// Bounded private request/reply audit; private credentials never enter output.
#[derive(Default)]
struct Audit {
    requests: Vec<u8>,
    replies: Vec<u8>,
    dropped: Option<serde_json::Value>,
    dropped_resume: Option<serde_json::Value>,
}

/// Exact real reply boundary to fault; none permits ordinary transport.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReplyFault {
    /// Actual durable applied usage reply after ledger commitment.
    Commit,
    /// Actual registered same-live recovery reply after native admission.
    Resume,
    /// No synthetic fault or replacement response.
    None,
}

/// Wraps a genuinely native-qualified stream and faults only a selected real reply.
struct FaultIo<S> {
    inner: S,
    audit: Arc<std::sync::Mutex<Audit>>,
    fault: ReplyFault,
}

impl<S: AsyncRead + Unpin> AsyncRead for FaultIo<S> {
    /// Records only bytes supplied by the real native writer-qualified owner.
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buffer.filled().len();
        let result = Pin::new(&mut this.inner).poll_read(cx, buffer);
        if matches!(result, Poll::Ready(Ok(()))) {
            let mut audit = this.audit.lock().unwrap();
            assert!(audit.requests.len() + buffer.filled().len() - before <= 65536);
            audit.requests.extend_from_slice(&buffer.filled()[before..]);
        }
        result
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for FaultIo<S> {
    /// The actor has already completed real dispatch/commit before this write.
    /// Decode its actual full response and fail before delegating ANY ACK bytes.
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        if this.fault != ReplyFault::None
            && let Ok((body, consumed)) = crate::control_client::decode_control_frame(bytes, 8192)
            && consumed == bytes.len()
            && let Ok(reply) = serde_json::from_str::<serde_json::Value>(&body)
            && ((this.fault == ReplyFault::Commit
                && reply["result"]["durable"] == true
                && reply["result"]["applied"] == true)
                || (this.fault == ReplyFault::Resume
                    && reply["result"]["registered"] == true
                    && reply["result"]["recovery"]
                        == serde_json::json!({"kind":"same-live-source","next_sequence":1})))
        {
            let mut audit = this.audit.lock().unwrap();
            let lost = if this.fault == ReplyFault::Commit {
                &mut audit.dropped
            } else {
                &mut audit.dropped_resume
            };
            assert!(
                lost.is_none(),
                "selected real reply may be dropped only once"
            );
            *lost = Some(reply);
            this.fault = ReplyFault::None;
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "private fixture drops an actual committed/admitted reply",
            )));
        }
        let result = Pin::new(&mut this.inner).poll_write(cx, bytes);
        if let Poll::Ready(Ok(written)) = result {
            let mut audit = this.audit.lock().unwrap();
            assert!(audit.replies.len() + written <= 65536);
            audit.replies.extend_from_slice(&bytes[..written]);
        }
        result
    }

    /// Delegates real flush without completing or replaying any action.
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    /// Delegates real shutdown without changing process or actor authority.
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

/// Reaps only this fixture's captured native process incarnations on panic.
/// Rechecking birth/parent identity prevents PID-reuse cleanup from signalling
/// an unrelated process; ordinary success still uses service-owned termination.
#[derive(Default)]
struct Processes(Arc<std::sync::Mutex<Vec<mez_mux::process::ProcessParentIdentity>>>);

impl Processes {
    /// Tracks a kernel-observed owned fixture process, not a payload PID hint.
    fn track(&self, identity: mez_mux::process::ProcessParentIdentity) {
        self.0.lock().unwrap().push(identity);
    }
}

impl Drop for Processes {
    /// Best-effort terminates only surviving exact fixture incarnations.
    fn drop(&mut self) {
        for identity in self.0.lock().unwrap().iter().rev() {
            if mez_mux::process::process_parent_identity_for_pid(identity.process_id)
                == Some(*identity)
            {
                // SAFETY: this is an owned fixture PID whose complete native
                // birth/parent identity was just reobserved, never a peer hint.
                unsafe {
                    libc::kill(identity.process_id as i32, libc::SIGKILL);
                }
            }
        }
    }
}

/// Removes only the isolated fixture root on success or assertion unwind.
struct Directory(std::path::PathBuf);
impl Drop for Directory {
    /// Leaves real HOME/vendor files untouched.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Decodes complete audit frames without printing private wire bodies.
fn frames(mut bytes: &[u8]) -> Vec<serde_json::Value> {
    let mut result = Vec::new();
    while !bytes.is_empty() {
        let (body, used) = crate::control_client::decode_control_frame(bytes, 8192).unwrap();
        result.push(serde_json::from_str(&body).unwrap());
        bytes = &bytes[used..];
    }
    result
}

/// Original owner, request ID, project origin, admitted UTC and linked observation.
type RequestRow = (String, String, Option<String>, i64, Option<String>);
/// Expense ID, reporting UTC, input/output/reasoning and optional cache partitions.
type ExpenseRow = (String, i64, i64, i64, i64, Option<i64>, Option<i64>);

/// Conserves the actual original durable IDs/origin/clock and stream revision.
/// Only fixture-owned SQLite is inspected; the parent never reports these values.
#[derive(Debug, PartialEq, Eq)]
struct Ledger {
    requests: Vec<RequestRow>,
    events: Vec<ExpenseRow>,
    streams: Vec<(String, i64)>,
    sources: Vec<(String, String)>,
    observations: i64,
}

/// Reads committed rows, not an injected success or a matching fake receipt.
fn ledger(path: &std::path::Path) -> Ledger {
    let db = rusqlite::Connection::open(path).unwrap();
    let requests = db.prepare("SELECT owner,request_id,project_id,admitted_at,observation_id FROM external_usage_requests ORDER BY request_id").unwrap().query_map([], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
    let events = db.prepare("SELECT id,observed_at,input_tokens,output_tokens,reasoning_tokens,cached_input_tokens,cache_write_input_tokens FROM token_usage_events ORDER BY id").unwrap().query_map([], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?))).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
    let streams = db
        .prepare("SELECT id,revision FROM external_usage_streams ORDER BY id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let sources = db
        .prepare("SELECT witness,owner FROM external_usage_sources ORDER BY owner")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let observations = db
        .query_row(
            "SELECT count(*) FROM external_usage_observations",
            [],
            |row| row.get(0),
        )
        .unwrap();
    Ledger {
        requests,
        events,
        streams,
        sources,
        observations,
    }
}

/// Waits on a scalar synthetic phase only; unavailable is a real failing
/// installed-producer outcome, not permission to manufacture recovery evidence.
async fn wait_phase(home: &std::path::Path, expected: &str) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let phase = std::fs::read_to_string(home.join("replay-phase")).unwrap_or_default();
            assert_ne!(
                phase, "unavailable",
                "installed native producer could not recover its original request"
            );
            if phase == expected {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("installed producer did not reach phase {expected}"));
}

/// Releases only synthetic callback sequencing, never ticket/capability data.
fn release(home: &std::path::Path, phase: &str) {
    std::fs::write(home.join("replay-next.tmp"), phase).unwrap();
    std::fs::rename(home.join("replay-next.tmp"), home.join("replay-next")).unwrap();
}

/// Fixed completion failure data contains no phase contents or private wire IDs.
#[derive(Debug, PartialEq, Eq)]
enum ChildFailure {
    /// The actual parent-shell result is unavailable or unsuccessful.
    Status(Option<u8>),
    /// A successfully exited producer did not publish its final assertion phase.
    Phase,
}

/// Collects only the atomically published actual shell result, not socket EOF.
/// Missing, malformed and oversized values fail closed within five seconds.
async fn child_status(home: &std::path::Path) -> Option<u8> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let path = home.join("replay-exit");
            if let Ok(metadata) = tokio::fs::metadata(&path).await {
                if metadata.len() > 4 {
                    return None;
                }
                return tokio::fs::read_to_string(path)
                    .await
                    .ok()
                    .and_then(|value| value.trim().parse::<u8>().ok());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .ok()
    .flatten()
}

/// Requires independent process and post-assertion phase evidence. All errors
/// use fixed stages/status only; no raw marker contents enter a failure message.
fn completion_result(status: Option<u8>, complete: bool) -> std::result::Result<(), ChildFailure> {
    if status != Some(0) {
        Err(ChildFailure::Status(status))
    } else if !complete {
        Err(ChildFailure::Phase)
    } else {
        Ok(())
    }
}

/// Collects the real process result while actor/service/process owners remain
/// alive, then checks the child's bounded final marker. Actual exit precedes
/// the phase read, so a missing/wrong/early-error phase cannot become success
/// later; there is no need to wait beyond the bounded process-status collector.
async fn child_completion(home: &std::path::Path) -> std::result::Result<(), ChildFailure> {
    let status = child_status(home).await;
    let path = home.join("replay-phase");
    let complete = if tokio::fs::metadata(&path)
        .await
        .is_ok_and(|metadata| metadata.len() == 8)
    {
        tokio::fs::read_to_string(path).await.ok().as_deref() == Some("complete")
    } else {
        false
    };
    completion_result(status, complete)
}

/// Releases the late assertion only after the native socket loop has finished.
/// Separately proves the child's fault site and actual exit1, so an unrelated
/// producer error cannot satisfy the regression's expected completion failure.
async fn observe_late_failure(home: &std::path::Path, fail_child: bool) {
    if fail_child {
        // Refusal may close its socket before consuming the previous release.
        // Do not overwrite that gate; require the child's next fixed phase.
        wait_phase(home, "post-eof").await;
        release(home, "post-eof");
        assert_eq!(child_status(home).await, Some(1));
        assert!(
            std::fs::read_to_string(home.join("replay-failure-stage"))
                .ok()
                .as_deref()
                == Some("assertion"),
            "fixed post-EOF assertion stage not reached"
        );
    }
}

/// Serves an actual Node socket over genuine UnixOriginStream writer capture.
/// Authentication and all actor admission/usage checks remain production code.
async fn connection(
    mut socket: tokio::net::UnixStream,
    handle: AsyncRuntimeSessionHandle,
    audit: Arc<std::sync::Mutex<Audit>>,
    fault: ReplyFault,
    node: &std::path::Path,
    processes: &Processes,
) -> (ControlConnectionState, Result<u64>) {
    let uid = crate::runtime::current_effective_uid();
    let origin = crate::runtime::capture_unix_origin(socket.as_raw_fd(), uid).unwrap();
    assert_ne!(origin.identity.process_id, std::process::id());
    assert_eq!(
        mez_mux::process::process_executable_identity_for_pid(origin.identity.process_id)
            .unwrap()
            .executable_path,
        node.canonicalize().unwrap()
    );
    processes.track(origin.identity);
    let mut state = ControlConnectionState::new(true, false);
    state
        .bind_authenticated_peer(AuthenticatedPeer::unix_user(origin.uid()))
        .unwrap();
    state.bind_unix_origin(Arc::new(origin)).unwrap();
    let qualified =
        crate::runtime::UnixOriginStream::new(&mut socket, state.unix_origin().cloned());
    let mut io = FaultIo {
        inner: qualified,
        audit,
        fault,
    };
    let result = serve_authenticated_async_runtime_control_connection_loop_with_snapshots(
        &mut io,
        AuthenticatedPeer::unix_user(uid),
        &handle,
        &mut state,
        AsyncRuntimeControlConnectionConfig::new(8192, uid).unwrap(),
        None,
        |_, _| false,
    )
    .await;
    assert!(!state.initialized());
    assert!(state.caller_client_id().is_none());
    assert!(state.unix_origin().unwrap().writer_confirmed());
    (state, result)
}

/// Establishes exact source/readiness/report invariants from the real wire,
/// excluding changing credentials from immutable expense comparisons. Booleans
/// avoid printing private tokens/witnesses on a failed comparison.
fn assert_replay(audit: &Audit, registry: bool, revision: u64) {
    let requests = frames(&audit.requests);
    let replies = frames(&audit.replies);
    let enrolls = requests
        .iter()
        .filter(|row| row["method"] == "agent/external/enroll")
        .collect::<Vec<_>>();
    assert_eq!(enrolls.len(), 2);
    let handles = replies
        .iter()
        .filter(|row| row["result"]["registered"] == true)
        .collect::<Vec<_>>();
    assert_eq!(handles.len(), 2);
    let old = &handles[0]["result"];
    let current = &handles[1]["result"];
    assert!(
        old["controls"] == serde_json::json!([]) && current["controls"] == serde_json::json!([])
    );
    for field in [
        "pane_id",
        "harness",
        "version",
        "external_session_id",
        "display_name",
        "observer_kind",
        "observer_instance",
        "predecessor_generation",
    ] {
        assert!(
            enrolls[0]["params"][field] == enrolls[1]["params"][field],
            "original enrollment metadata changed"
        );
    }
    if registry {
        assert!(
            enrolls[1]["params"]["source_resume"]["observer_witness"] == old["observer_witness"]
        );
        assert!(enrolls[1]["params"]["source_resume"]["generation"] == old["generation"]);
        assert!(
            current["launch_token"] != old["launch_token"]
                && current["observer_witness"] != old["observer_witness"]
        );
        assert!(current["run_id"] == old["run_id"]);
        assert_eq!(
            current["observer_epoch"].as_u64().unwrap(),
            old["observer_epoch"].as_u64().unwrap() + 1
        );
        assert!(
            current["recovery"] == serde_json::json!({"kind":"same-live-source","next_sequence":1})
        );
        let facts = requests
            .iter()
            .skip_while(|row| row["id"] != enrolls[1]["id"])
            .skip(1)
            .filter(|row| row["method"] == "agent/external/pi-observation")
            .collect::<Vec<_>>();
        // Ordinary LifecycleOwner accepts a genuine candidate directly. Resume
        // permits diagnostic gaps, but a successful retry invents neither a
        // missing-accounting gap nor an SDK SessionStart for this new epoch.
        assert_eq!(facts.len(), 3);
        assert_eq!(facts[0]["params"]["sequence"], 1);
        assert!(
            facts[0]["params"]["event"]
                == serde_json::json!({"type":"agent_before_settle","outcome":"completed"})
        );
        assert_eq!(facts[1]["params"]["sequence"], 2);
        assert!(facts[1]["params"]["event"] == serde_json::json!({"type":"agent_settled"}));
        assert_eq!(facts[2]["params"]["sequence"], 3);
        assert!(
            facts[2]["params"]["event"]
                == serde_json::json!({"type":"session_shutdown","reason":"quit"})
        );
    } else {
        assert!(current["launch_token"] == old["launch_token"]);
        assert!(current["observer_epoch"] == old["observer_epoch"]);
    }
    let ends = requests
        .iter()
        .filter(|row| row["method"] == "agent/external/usage-end")
        .collect::<Vec<_>>();
    assert_eq!(
        ends.len(),
        3,
        "one original attempt, one telemetry replay, one subsequent genuine request"
    );
    for field in [
        "request_id",
        "event_id",
        "provider",
        "model",
        "counters",
        "observed_at",
    ] {
        assert!(
            ends[0]["params"][field] == ends[1]["params"][field],
            "frozen original report changed"
        );
    }
    assert!(ends[2]["params"]["request_id"] != ends[0]["params"]["request_id"]);
    let original = &ends[0]["params"]["request_id"];
    let ready_ids = requests
        .iter()
        .filter(|row| {
            row["method"] == "agent/external/usage-begin"
                && row["params"]["request_id"] == *original
        })
        .map(|row| &row["id"])
        .collect::<Vec<_>>();
    let readiness = replies
        .iter()
        .filter(|reply| ready_ids.contains(&&reply["id"]) && reply["result"]["accepted"] == true)
        .collect::<Vec<_>>();
    assert!(readiness.len() >= 3);
    for reply in &readiness[1..] {
        for field in ["source_witness", "admitted_at", "origin"] {
            assert!(
                reply["result"][field] == readiness[0]["result"][field],
                "original native readiness changed"
            );
        }
    }
    let replay = replies
        .iter()
        .find(|reply| reply["id"] == ends[1]["id"])
        .unwrap();
    assert_eq!(replay["result"]["durable"], true);
    assert_eq!(replay["result"]["applied"], false);
    assert_eq!(replay["result"]["revision"], revision);
    assert!(
        !replies
            .iter()
            .any(|reply| reply["id"] == audit.dropped.as_ref().unwrap()["id"]),
        "dropped ACK leaked to the child"
    );
    assert!(!String::from_utf8_lossy(&audit.requests).contains("PRIVATE_REPLAY_CONTENT"));
}

/// Runs only actual bare bootstrap against the fresh built sibling product.
/// No root/version/apply/manifest/helper selection reaches installation.
fn bootstrap(helper: &std::path::Path, home: &std::path::Path) {
    for expected in ["installed", "unchanged"] {
        let output = std::process::Command::new(helper)
            .args(["--json", "bootstrap", "pi"])
            .env_clear()
            .env("HOME", home)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(output.status.success(), "bare bootstrap failed");
        assert!(output.stderr.is_empty());
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["root_source"], "vendor-default");
        assert_eq!(report["result"], expected);
        assert_eq!(report["lifecycle"], "unverified");
        assert_eq!(report["usage"], "unverified");
    }
}

/// Shared local fixture composes the genuine CLI/producer/actor/ledger chain.
/// Connected mode isolates ACK loss; registry mode additionally clears ONLY
/// ephemeral registrations after disconnect, retaining the same control owner.
/// The final completion outcome is captured before cleanup, but all inherited
/// wire/ledger/service assertions still run even for the intentional failure.
async fn qualify(
    registry: bool,
    unknown: bool,
    fail_child: bool,
) -> std::result::Result<(), ChildFailure> {
    use std::os::unix::fs::PermissionsExt;
    let node = std::path::PathBuf::from(
        std::env::var_os("MEZ_TEST_NODE_BINARY").expect("explicit already-installed Node path"),
    );
    // This test-only selector chooses our freshly built product, never an
    // installed-client activation input or a parent-supplied capability.
    let helper = std::env::var_os("MEZ_TEST_HELPER_BINARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("mez")
        });
    assert!(node.is_absolute() && node.is_file() && helper.is_absolute() && helper.is_file());
    let directory = std::fs::canonicalize("/tmp").unwrap().join(format!(
        "mez-pi-replay-{}-{:x}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir(&directory).unwrap();
    let _directory = Directory(directory.clone());
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = directory.join("home");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(
        home.join("replay-mode"),
        if unknown {
            "unknown"
        } else if registry {
            "registry"
        } else {
            "connected"
        },
    )
    .unwrap();
    bootstrap(&helper, &home);
    if fail_child {
        std::fs::write(home.join("replay-failure"), "post-eof-assertion").unwrap();
    }
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/pi-native-replay-fixture.mjs")
        .canonicalize()
        .unwrap();
    let bin = directory.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let pi = bin.join("pi");
    std::fs::write(
        &pi,
        format!(
            "#!/bin/sh\n{} {}\nresult=$?\nprintf '%s\\n' \"$result\" > \"$HOME/replay-exit.tmp\"\nmv \"$HOME/replay-exit.tmp\" \"$HOME/replay-exit\"\nexit \"$result\"\n",
            shlex::try_quote(node.to_str().unwrap()).unwrap(),
            shlex::try_quote(script.to_str().unwrap()).unwrap()
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
            "replay-user",
            true,
            mez_mux::layout::Size::new(100, 40).unwrap(),
            120,
        )
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    let processes = Processes::default();
    let root = mez_mux::process::process_parent_identity_for_pid(
        service.primary_pid_for_live_pane_process("%1").unwrap(),
    )
    .unwrap();
    processes.track(root);
    service
        .session
        .set_pane_title_explicit("%1", "Pinned replay title")
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let native_conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    service.toggle_active_agent_shell().unwrap();
    let clients = service.session().clients().len();
    let session = service.session().id.to_string();
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
    let audit = Arc::new(std::sync::Mutex::new(Audit::default()));
    let (mut handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let mut actor_task = tokio::spawn(actor.run());
    let (socket, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let (first, ()) = tokio::join!(
        connection(
            socket,
            handle.clone(),
            audit.clone(),
            ReplyFault::Commit,
            &node,
            &processes
        ),
        async {
            wait_phase(&home, "registered").await;
            release(&home, "registered");
        }
    );
    let (first_connection, failed_write) = first;
    assert_eq!(
        failed_write.unwrap_err().io_kind(),
        Some(std::io::ErrorKind::BrokenPipe)
    );
    assert!(
        first_connection.unix_origin().unwrap().is_live(),
        "producer died instead of reconnecting"
    );
    let producer = first_connection.unix_origin().unwrap().identity;
    let committed = ledger(&usage);
    assert_eq!(
        (
            committed.requests.len(),
            committed.events.len(),
            committed.streams.len(),
            committed.sources.len(),
            committed.observations
        ),
        (1, 1, 1, 1, 1)
    );
    assert_eq!(
        (
            committed.events[0].2,
            committed.events[0].3,
            committed.events[0].4,
            committed.events[0].5,
            committed.events[0].6
        ),
        (10, 7, 4, Some(2), Some(3))
    );
    let revision = audit.lock().unwrap().dropped.as_ref().unwrap()["result"]["revision"]
        .as_u64()
        .unwrap();
    assert_eq!(committed.streams[0].1 as u64, revision);
    if registry {
        handle.shutdown().await.unwrap();
        let mut returned = actor_task.await.unwrap().service;
        assert_eq!(returned.session().id.to_string(), session);
        assert_eq!(
            mez_mux::process::process_parent_identity_for_pid(root.process_id).unwrap(),
            root
        );
        assert_eq!(
            mez_mux::process::process_parent_identity_for_pid(producer.process_id).unwrap(),
            producer
        );
        assert_eq!(
            returned
                .external_token_usage(None)
                .values()
                .map(|value| value.input_tokens)
                .sum::<u64>(),
            10
        );
        // The ONLY reset: preserve control/component owner, source anchors,
        // accounting projection/begins, root process and durable store.
        *returned.control.external_agents_mut() = Default::default();
        returned.set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&usage));
        assert_eq!(
            returned
                .external_token_usage(None)
                .values()
                .map(|value| value.input_tokens)
                .sum::<u64>(),
            10,
            "registration reset erased committed accounting projection"
        );
        assert_eq!(ledger(&usage), committed);
        let (next_handle, next_actor) =
            AsyncRuntimeSessionActor::new(returned, AsyncRuntimeActorConfig::default()).unwrap();
        handle = next_handle;
        actor_task = tokio::spawn(next_actor.run());
    }
    if unknown {
        // R1 is genuinely admitted but its actual reply never reaches Node.
        let (socket, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let (resumed_connection, lost_resume) = connection(
            socket,
            handle.clone(),
            audit.clone(),
            ReplyFault::Resume,
            &node,
            &processes,
        )
        .await;
        assert_eq!(
            lost_resume.unwrap_err().io_kind(),
            Some(std::io::ErrorKind::BrokenPipe)
        );
        assert_eq!(resumed_connection.unix_origin().unwrap().identity, producer);
        assert_eq!(ledger(&usage), committed);
        {
            let recorded = audit.lock().unwrap();
            let admitted = recorded
                .dropped_resume
                .as_ref()
                .expect("actual R1 admission reply must have been lost before bytes");
            assert_eq!(admitted["result"]["registered"], true);
            assert!(
                admitted["result"]["recovery"]
                    == serde_json::json!({"kind":"same-live-source","next_sequence":1})
            );
        }
        handle.shutdown().await.unwrap();
        let mut retained = actor_task.await.unwrap().service;
        assert_eq!(retained.session().id.to_string(), session);
        assert_eq!(
            mez_mux::process::process_parent_identity_for_pid(root.process_id).unwrap(),
            root
        );
        assert_eq!(
            mez_mux::process::process_parent_identity_for_pid(producer.process_id).unwrap(),
            producer
        );
        assert_eq!(retained.control.external_agents().bindings.len(), 1);
        *retained.control.external_agents_mut() = Default::default();
        retained.set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&usage));
        assert_eq!(
            retained
                .external_token_usage(None)
                .values()
                .map(|value| value.input_tokens)
                .sum::<u64>(),
            10
        );
        assert_eq!(ledger(&usage), committed);
        let (next, actor) =
            AsyncRuntimeSessionActor::new(retained, AsyncRuntimeActorConfig::default()).unwrap();
        handle = next;
        actor_task = tokio::spawn(actor.run());
        let (socket, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let (refused, ()) = tokio::join!(
            connection(
                socket,
                handle.clone(),
                audit.clone(),
                ReplyFault::None,
                &node,
                &processes
            ),
            async {
                wait_phase(&home, "refused").await;
                assert_eq!(
                    ledger(&usage),
                    committed,
                    "failed old-S0 retry must not create any replacement expense or namespace"
                );
                release(&home, "refused");
            }
        );
        let (refused_connection, finished) = refused;
        assert!(finished.unwrap() > 0);
        assert_eq!(refused_connection.unix_origin().unwrap().identity, producer);
        observe_late_failure(&home, fail_child).await;
        let completion = child_completion(&home).await;
        handle.shutdown().await.unwrap();
        let mut retained = actor_task.await.unwrap().service;
        assert_eq!(retained.session().clients().len(), clients);
        assert_eq!(
            retained.session().pane_title_state("%1").unwrap().0,
            "Pinned replay title"
        );
        assert_eq!(
            retained.agent_shell_store().get("%1").unwrap().session_id,
            native_conversation
        );
        assert!(retained.agent_turn_ledger().turns().is_empty());
        assert!(
            retained.control.external_agents().bindings.is_empty(),
            "denied S0 must not allocate R2"
        );
        assert!(
            retained
                .control
                .external_agents()
                .enrollments
                .pending
                .is_empty()
        );
        assert_eq!(
            retained
                .external_token_usage(None)
                .values()
                .map(|value| value.input_tokens)
                .sum::<u64>(),
            10
        );
        assert_eq!(ledger(&usage), committed);
        {
            let recorded = audit.lock().unwrap();
            let sent = frames(&recorded.requests);
            let received = frames(&recorded.replies);
            let enrolls = sent
                .iter()
                .filter(|row| row["method"] == "agent/external/enroll")
                .collect::<Vec<_>>();
            // The shared client now makes one explicit durable-native attempt
            // after an answered warm refusal. The same service still retains
            // its warm source authority, so that fourth attempt must refuse
            // without selecting a newer receipt or initial enrollment.
            assert_eq!(enrolls.len(), 4);
            assert!(
                enrolls[1]["params"] == enrolls[2]["params"],
                "unknown R1 must retain original S0/P metadata, not newest/bare fallback"
            );
            let original = received
                .iter()
                .find(|row| row["result"]["registered"] == true)
                .unwrap();
            assert!(
                enrolls[2]["params"]["source_resume"]["observer_witness"]
                    == original["result"]["observer_witness"]
            );
            assert!(
                enrolls[2]["params"]["source_resume"]["generation"]
                    == original["result"]["generation"]
            );
            let rejected = received
                .iter()
                .find(|row| row["id"] == enrolls[2]["id"])
                .unwrap();
            assert_eq!(rejected["error"]["code"], -32006);
            assert_eq!(
                rejected["error"]["message"],
                "source resume retry unavailable"
            );
            let cold = &enrolls[3]["params"];
            let selector = &cold["durable_source_resume"];
            assert!(
                selector["receipt"] == original["result"]["source_receipt"],
                "cold recovery must retain the original public checkpoint"
            );
            let operation = selector["operation_id"].as_str().unwrap();
            assert!(
                operation.len() == 64 && operation.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "cold recovery requires one bounded explicit operation identity"
            );
            let mut expected_cold = enrolls[0]["params"].clone();
            expected_cold["durable_source_resume"] = selector.clone();
            assert!(
                *cold == expected_cold,
                "cold recovery must preserve metadata without warm/newest selectors"
            );
            let cold_refused = received
                .iter()
                .find(|row| row["id"] == enrolls[3]["id"])
                .unwrap();
            assert_eq!(cold_refused["error"]["code"], -32006);
            assert_eq!(
                cold_refused["error"]["message"],
                "same-live source authority retained; cold replacement unavailable"
            );
            assert_eq!(
                sent.iter()
                    .filter(|row| row["method"] == "agent/external/usage-end")
                    .count(),
                1
            );
            assert_eq!(
                sent.iter()
                    .filter(|row| row["method"] == "agent/external/usage-begin")
                    .count(),
                2
            );
            assert_eq!(
                received
                    .iter()
                    .filter(|row| row["result"]["registered"] == true)
                    .count(),
                1
            );
            assert!(
                !received.iter().any(|row| row["result"]["durable"] == true),
                "no lost durable reply was delivered"
            );
            assert!(
                !received
                    .iter()
                    .any(|row| row["id"] == recorded.dropped.as_ref().unwrap()["id"]
                        || row["id"] == recorded.dropped_resume.as_ref().unwrap()["id"])
            );
            let after = sent
                .iter()
                .rposition(|row| row["method"] == "agent/external/enroll")
                .unwrap();
            assert_eq!(
                after + 1,
                sent.len(),
                "failed resume must not send a manufactured gap or follow-on report"
            );
            assert!(
                !String::from_utf8_lossy(&recorded.requests).contains("PRIVATE_REPLAY_CONTENT")
            );
        }
        retained.terminate_all_pane_processes().unwrap();
        return completion;
    }
    let (socket, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let (second, ()) = tokio::join!(
        connection(
            socket,
            handle.clone(),
            audit.clone(),
            ReplyFault::None,
            &node,
            &processes
        ),
        async {
            wait_phase(&home, "recovered").await;
            assert_eq!(
                ledger(&usage),
                committed,
                "replay changed original durable expense/origin/revision"
            );
            handle
                .execute_terminal_command(primary.clone(), "agent-shell".into())
                .await
                .unwrap();
            let status = handle
                .execute_agent_shell_command(primary.clone(), "/status".into())
                .await
                .unwrap();
            assert!(
                status.contains("actual-model")
                    && status.contains("request-bound-external-reported")
            );
            if registry {
                // start() restores credentials/ordinal but emits no SDK fact.
                // The producer is paused before its next lifecycle callback:
                // do not manufacture a gap from successful durable replay.
                let recorded = frames(&audit.lock().unwrap().requests);
                let resumed = recorded
                    .iter()
                    .rposition(|row| row["method"] == "agent/external/enroll")
                    .unwrap();
                assert!(
                    !recorded[resumed + 1..]
                        .iter()
                        .any(|row| row["method"] == "agent/external/pi-observation")
                );
                assert!(!status.contains("unavailable-pi-usage-"));
            }
            assert!(!status.contains("PRIVATE_REPLAY_CONTENT"));
            let list = handle
                .execute_terminal_command(primary.clone(), "list-agents".into())
                .await
                .unwrap();
            assert!(list.contains("pi") || list.contains("Pi"));
            assert!(
                handle
                    .pending_agent_provider_tasks()
                    .await
                    .unwrap()
                    .is_empty()
            );
            handle
                .execute_terminal_command(primary.clone(), "agent-shell".into())
                .await
                .unwrap();
            release(&home, "recovered");
            wait_phase(&home, "subsequent").await;
            let next = ledger(&usage);
            assert_eq!(
                (
                    next.requests.len(),
                    next.events.len(),
                    next.streams.len(),
                    next.sources.len(),
                    next.observations
                ),
                (2, 2, 1, 1, 2)
            );
            assert_eq!(next.sources, committed.sources);
            assert_eq!(next.events.iter().map(|row| row.2).sum::<i64>(), 20);
            assert!(next.events.contains(&committed.events[0]));
            assert!(next.requests.contains(&committed.requests[0]));
            release(&home, "subsequent");
        }
    );
    let (second_connection, served) = second;
    assert!(served.unwrap() > 0);
    assert_eq!(second_connection.unix_origin().unwrap().identity, producer);
    observe_late_failure(&home, fail_child).await;
    let completion = child_completion(&home).await;
    handle.shutdown().await.unwrap();
    let mut returned = actor_task.await.unwrap().service;
    assert_eq!(returned.session().clients().len(), clients);
    assert_eq!(
        returned.session().pane_title_state("%1").unwrap().0,
        "Pinned replay title"
    );
    assert_eq!(
        returned.agent_shell_store().get("%1").unwrap().session_id,
        native_conversation
    );
    assert!(returned.agent_turn_ledger().turns().is_empty());
    assert!(
        returned
            .control
            .external_agents()
            .bindings
            .values()
            .all(|binding| binding.retired)
    );
    assert!(returned.external_agent_rows().is_empty());
    assert_eq!(
        returned.external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
    assert_replay(&audit.lock().unwrap(), registry, revision);
    returned.terminate_all_pane_processes().unwrap();
    completion
}

/// Completion is conjunctive: a complete marker cannot excuse missing/nonzero
/// exit, and a neutral exit cannot excuse absent/wrong final assertions. Fixed
/// enums ensure neither raw phase strings nor private wire values reach Debug.
#[test]
fn pi_replay_completion_requires_both_phase_and_actual_exit_zero() {
    assert_eq!(completion_result(Some(0), true), Ok(()));
    assert_eq!(completion_result(Some(0), false), Err(ChildFailure::Phase));
    for status in [None, Some(1), Some(137), Some(255)] {
        for complete in [false, true] {
            assert_eq!(
                completion_result(status, complete),
                Err(ChildFailure::Status(status))
            );
        }
    }
    assert_eq!(format!("{:?}", ChildFailure::Phase), "Phase");
    assert_eq!(
        format!("{:?}", ChildFailure::Status(Some(1))),
        "Status(Some(1))"
    );
}

/// Synthetic scalar-file tests qualify only the collector, never native RPC
/// or successful producer acceptance. Missing, malformed/overflow, oversized,
/// wrong-phase and early-error values remain bounded failures; actual native
/// success and late exit1 are exercised separately through real producers.
#[tokio::test(flavor = "current_thread")]
async fn pi_replay_completion_collector_rejects_invalid_status_and_phase() {
    let directory = Directory(std::fs::canonicalize("/tmp").unwrap().join(format!(
        "mez-pi-completion-{}-{:x}",
        std::process::id(),
        rand::random::<u64>()
    )));
    std::fs::create_dir(&directory.0).unwrap();
    let home = directory.0.as_path();
    std::fs::write(home.join("replay-phase"), "complete").unwrap();
    assert_eq!(
        child_completion(home).await,
        Err(ChildFailure::Status(None))
    );
    for value in ["", "bad", "256", "-1", "00000"] {
        std::fs::write(home.join("replay-exit"), value).unwrap();
        assert_eq!(
            child_completion(home).await,
            Err(ChildFailure::Status(None))
        );
    }
    std::fs::write(home.join("replay-exit"), "1\n").unwrap();
    assert_eq!(
        child_completion(home).await,
        Err(ChildFailure::Status(Some(1)))
    );
    std::fs::write(home.join("replay-exit"), "0\n").unwrap();
    for phase in [
        "refused",
        "post-eof",
        "unavailable",
        "PRIVATE_REPLAY_CONTENT",
        "",
    ] {
        std::fs::write(home.join("replay-phase"), phase).unwrap();
        assert_eq!(child_completion(home).await, Err(ChildFailure::Phase));
    }
    std::fs::remove_file(home.join("replay-phase")).unwrap();
    assert_eq!(child_completion(home).await, Err(ChildFailure::Phase));
    std::fs::write(home.join("replay-phase"), "complete").unwrap();
    assert_eq!(child_completion(home).await, Ok(()));
}

/// Dropping a real durable reply without registry loss must retransmit only the
/// original expense. No SDK callback/provider is replayed and no new epoch is
/// fabricated, while a subsequent genuine request can still be accounted once.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit already-installed Node and freshly built sibling mez required; real native ACK-loss acceptance"]
async fn external_pi_native_committed_ack_loss_without_registry_reset() {
    tokio::time::timeout(Duration::from_secs(45), qualify(false, false, false))
        .await
        .unwrap()
        .unwrap();
}

/// Actual commit-before-lost-ACK plus same-service registry recreation requires
/// child-originated same-live proof and fresh transport handles. The first real
/// lifecycle candidate has ordinal one without a synthetic start or gap.
/// Original owner/request/origin/clock/revision remain unchanged,
/// replay applies zero delta, and the next genuine request charges exactly once.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit already-installed Node and freshly built sibling mez required; real native registry-reset replay acceptance"]
async fn external_pi_native_committed_ack_loss_and_registry_reset_replays_once() {
    tokio::time::timeout(Duration::from_secs(45), qualify(true, false, false))
        .await
        .unwrap()
        .unwrap();
}

/// A genuine R1 native admission reply lost before bytes cannot authorize a
/// later retry after R1 registrations disappear again. The same live child
/// knows only original S0/P and must receive the named refusal, latch its real
/// failed/gap path, and preserve exactly one original committed expense/source.
/// Neither parent nor child guesses R1/R2, changes frozen report data, or falls
/// back to bare enrollment; the lost replies are never treated as delivered.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit already-installed Node and freshly built sibling mez required; actual unknown-R1 reply loss negative"]
async fn external_pi_native_unknown_recovery_reply_second_reset_refuses_old_proof() {
    tokio::time::timeout(Duration::from_secs(45), qualify(true, true, false))
        .await
        .unwrap()
        .unwrap();
}

/// Every real native recovery path must reject an assertion which runs after
/// parent-observed socket EOF. The child records its fixed fault stage and the
/// shell publishes its genuine exit1 before cleanup; neither an unrelated
/// error, a compile failure, a timeout nor an EOF can satisfy this regression.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit already-installed Node and freshly built sibling mez required; real post-EOF child failure"]
async fn external_pi_native_post_eof_assertion_failure_is_rejected() {
    let mut outcomes = Vec::new();
    for (registry, unknown) in [(false, false), (true, false), (true, true)] {
        let outcome =
            tokio::time::timeout(Duration::from_secs(45), qualify(registry, unknown, true))
                .await
                .unwrap();
        outcomes.push(outcome);
    }
    assert_eq!(
        outcomes,
        vec![
            Err(ChildFailure::Status(Some(1))),
            Err(ChildFailure::Status(Some(1))),
            Err(ChildFailure::Status(Some(1)))
        ]
    );
}
