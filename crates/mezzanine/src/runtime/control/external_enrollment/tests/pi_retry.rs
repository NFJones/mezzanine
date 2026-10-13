//! Actual installed Pi finalized-callback retry after two lost durable replies.
//!
//! Bare bootstrap and a normally invoked Pi-shaped Node process supply genuine
//! Unix writer/ancestor evidence to the production actor. Both actual usage-end
//! replies (applied=true then applied=false) are lost before any bytes escape.
//! Only a later exact child-originated finalized callback retries the original
//! ticket/report. Private audits are bounded and never printed. No registry
//! reset, provider replay, failure-latch clearing, synthetic ACK or spool occurs.

use super::*;
use crate::host::async_runtime::{
    AsyncRuntimeActorConfig, AsyncRuntimeControlConnectionConfig, AsyncRuntimeSessionActor,
    AsyncRuntimeSessionHandle,
    serve_authenticated_async_runtime_control_connection_loop_with_snapshots,
};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// Private, bounded wire evidence. Credentials/witnesses never enter diagnostics.
#[derive(Default)]
struct Audit {
    requests: Vec<u8>,
    replies: Vec<u8>,
    dropped: Vec<serde_json::Value>,
}

/// Transparent native stream wrapper with one selected real durable reply loss.
struct DoubleAckIo<S> {
    inner: S,
    audit: Arc<std::sync::Mutex<Audit>>,
    drop_reply: bool,
}

impl<S: AsyncRead + Unpin> AsyncRead for DoubleAckIo<S> {
    /// Captures bytes from the real UnixOriginStream read, not a payload PID.
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let result = Pin::new(&mut this.inner).poll_read(cx, buf);
        if matches!(result, Poll::Ready(Ok(()))) {
            let mut audit = this.audit.lock().unwrap();
            assert!(audit.requests.len() + buf.filled().len() - before <= 65536);
            audit.requests.extend_from_slice(&buf.filled()[before..]);
        }
        result
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for DoubleAckIo<S> {
    /// Matches the real usage-end request ID and durable result before writing
    /// ANY reply byte. The actor has already dispatched/committed the operation.
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        if this.drop_reply
            && let Ok((body, consumed)) = crate::control_client::decode_control_frame(bytes, 8192)
            && consumed == bytes.len()
            && let Ok(reply) = serde_json::from_str::<serde_json::Value>(&body)
            && reply["result"]["durable"] == true
        {
            let mut audit = this.audit.lock().unwrap();
            let sent = frames(&audit.requests);
            let request = sent.iter().find(|row| row["id"] == reply["id"]).unwrap();
            assert!(
                request["method"] == "agent/external/usage-end",
                "fault selected a non-report reply"
            );
            assert!(reply["result"]["accepted"] == true);
            assert!(audit.dropped.len() < 2);
            assert!(
                reply["result"]["applied"] == (audit.dropped.is_empty()),
                "fault did not observe original commit then replay"
            );
            audit.dropped.push(reply);
            this.drop_reply = false;
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "fixture drops actual durable reply before bytes",
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

    /// Preserves native flush semantics without creating an acknowledgment.
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    /// Preserves socket shutdown and origin-owner release behavior.
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

/// Panic-safe cleanup for exact kernel-captured fixture process incarnations.
#[derive(Default)]
struct Processes(std::sync::Mutex<Vec<mez_mux::process::ProcessParentIdentity>>);

impl Processes {
    /// Retains only independently observed owned native fixture identities.
    fn track(&self, identity: mez_mux::process::ProcessParentIdentity) {
        self.0.lock().unwrap().push(identity);
    }
}

impl Drop for Processes {
    /// PID reuse cannot signal an unrelated incarnation; no peer hint is used.
    fn drop(&mut self) {
        for identity in self.0.lock().unwrap().iter().rev() {
            if mez_mux::process::process_parent_identity_for_pid(identity.process_id)
                == Some(*identity)
            {
                // SAFETY: the exact owned native birth/parent identity was just reobserved.
                unsafe {
                    libc::kill(identity.process_id as i32, libc::SIGKILL);
                }
            }
        }
    }
}

/// Removes only the unique private install/database/socket fixture directory.
struct Directory(std::path::PathBuf);
impl Drop for Directory {
    /// Never removes user HOME, vendor configuration or other workers' outputs.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Decodes private complete frames for fixed boolean comparisons, never logs.
fn frames(mut bytes: &[u8]) -> Vec<serde_json::Value> {
    let mut rows = Vec::new();
    while !bytes.is_empty() {
        let (body, consumed) = crate::control_client::decode_control_frame(bytes, 8192).unwrap();
        rows.push(serde_json::from_str(&body).unwrap());
        bytes = &bytes[consumed..];
    }
    rows
}

/// Original owner/request/project/admission clock and linked observation.
type RequestRow = (String, String, Option<String>, i64, Option<String>);
/// Original expense identity/report clock and normalized token partitions.
type ExpenseRow = (String, i64, i64, i64, i64, Option<i64>, Option<i64>);

/// Exact durable conservation witness; deliberately has NO Debug implementation
/// because source/owner identifiers must not escape on assertion failure.
#[derive(PartialEq, Eq)]
struct Ledger {
    requests: Vec<RequestRow>,
    events: Vec<ExpenseRow>,
    streams: Vec<(String, i64)>,
    sources: Vec<(String, String)>,
    observations: i64,
}

/// Reads only this fixture's real committed SQLite, not a simulated receipt.
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

/// Waits for a scalar child-owned stage; assertion failure is never recovery.
async fn wait_phase(home: &std::path::Path, expected: &str) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let phase = tokio::fs::read_to_string(home.join("retry-phase"))
                .await
                .unwrap_or_default();
            assert!(
                phase != "unavailable",
                "installed retry child failed an assertion"
            );
            if phase == expected {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("installed retry child did not reach {expected}"));
}

/// Orders the child's own callbacks using only an inert fixed phase name.
fn release(home: &std::path::Path, phase: &str) {
    std::fs::write(home.join("retry-next.tmp"), phase).unwrap();
    std::fs::rename(home.join("retry-next.tmp"), home.join("retry-next")).unwrap();
}

/// Requires the pane shell's actual post-process numeric result within five
/// seconds; EOF, phase publication, missing and malformed status are not success.
async fn child_status(home: &std::path::Path) -> Option<u8> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let path = home.join("retry-exit");
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

/// Serves only a real Node peer through native origin/writer collection and
/// production actor ingress. Authenticated transport grants no initialized RPC.
async fn connection(
    mut socket: tokio::net::UnixStream,
    handle: AsyncRuntimeSessionHandle,
    audit: Arc<std::sync::Mutex<Audit>>,
    drop_reply: bool,
    node: &std::path::Path,
    processes: &Processes,
) -> (ControlConnectionState, Result<u64>) {
    let uid = crate::runtime::current_effective_uid();
    let origin = crate::runtime::capture_unix_origin(socket.as_raw_fd(), uid).unwrap();
    assert_ne!(origin.identity.process_id, std::process::id());
    assert!(
        mez_mux::process::process_executable_identity_for_pid(origin.identity.process_id)
            .unwrap()
            .executable_path
            == node.canonicalize().unwrap()
    );
    processes.track(origin.identity);
    let mut state = ControlConnectionState::new(true, false);
    state
        .bind_authenticated_peer(AuthenticatedPeer::unix_user(origin.uid()))
        .unwrap();
    state.bind_unix_origin(Arc::new(origin)).unwrap();
    let qualified =
        crate::runtime::UnixOriginStream::new(&mut socket, state.unix_origin().cloned());
    let mut io = DoubleAckIo {
        inner: qualified,
        audit,
        drop_reply,
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

/// Verifies child-originated immutable report/readiness plus both genuine lost
/// reply IDs, successful third replay and absence of extra expense markers.
fn assert_wire(audit: &Audit, revision: i64) {
    let sent = frames(&audit.requests);
    let received = frames(&audit.replies);
    assert_eq!(audit.dropped.len(), 2);
    let ends = sent
        .iter()
        .filter(|row| row["method"] == "agent/external/usage-end")
        .collect::<Vec<_>>();
    assert_eq!(
        ends.len(),
        4,
        "two immediate attempts, later callback replay, then new request"
    );
    for later in &ends[1..3] {
        for field in [
            "request_id",
            "event_id",
            "provider",
            "model",
            "counters",
            "observed_at",
        ] {
            assert!(
                later["params"][field] == ends[0]["params"][field],
                "original report metadata changed"
            );
        }
    }
    assert!(ends[3]["params"]["request_id"] != ends[0]["params"]["request_id"]);
    let original = &ends[0]["params"]["request_id"];
    let begins = sent
        .iter()
        .filter(|row| {
            row["method"] == "agent/external/usage-begin"
                && row["params"]["request_id"] == *original
        })
        .collect::<Vec<_>>();
    assert_eq!(begins.len(), 4);
    let readiness = received
        .iter()
        .filter(|row| begins.iter().any(|begin| begin["id"] == row["id"]))
        .collect::<Vec<_>>();
    assert_eq!(readiness.len(), 4);
    for later in &readiness[1..] {
        for field in ["source_witness", "admitted_at", "origin"] {
            assert!(
                later["result"][field] == readiness[0]["result"][field],
                "original readiness changed"
            );
        }
    }
    for (index, lost) in audit.dropped.iter().enumerate() {
        assert!(lost["id"] == ends[index]["id"]);
        assert!(
            !received.iter().any(|row| row["id"] == lost["id"]),
            "a lost ACK byte reached the child"
        );
        assert!(lost["result"]["revision"] == revision);
    }
    let replay = received
        .iter()
        .find(|row| row["id"] == ends[2]["id"])
        .unwrap();
    assert!(replay["result"]["durable"] == true && replay["result"]["applied"] == false);
    assert!(replay["result"]["revision"] == revision);
    let enrollments = received
        .iter()
        .filter(|row| row["result"]["registered"] == true)
        .collect::<Vec<_>>();
    assert_eq!(enrollments.len(), 3);
    for later in &enrollments[1..] {
        for field in [
            "launch_token",
            "observer_witness",
            "generation",
            "run_id",
            "observer_epoch",
            "observer_instance",
            "external_session_id",
        ] {
            assert!(
                later["result"][field] == enrollments[0]["result"][field],
                "connected retry changed its accepted handle/source"
            );
        }
        assert!(
            later["result"].get("recovery").is_none(),
            "connected retry fabricated projection recovery"
        );
        assert!(later["result"]["controls"] == serde_json::json!([]));
    }
    let gaps = sent
        .iter()
        .filter(|row| {
            row["method"] == "agent/external/pi-observation"
                && row["params"]["event"]["type"] == "usage_gap"
        })
        .collect::<Vec<_>>();
    assert_eq!(gaps.len(), 1);
    assert!(
        gaps[0]["params"]["event"]
            == serde_json::json!({"type":"usage_gap","reason":"durable-acknowledgment"})
    );
    assert!(!String::from_utf8_lossy(&audit.requests).contains("PRIVATE_RETRY_CONTENT"));
}

/// Qualifies existing callback-triggered same-owner retry through actual bare
/// install, real producer/actor commitment, preserved ledger and truthful status.
async fn qualify() {
    use std::os::unix::fs::PermissionsExt;
    let node = std::path::PathBuf::from(
        std::env::var_os("MEZ_TEST_NODE_BINARY").expect("explicit existing Node"),
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
        "mez-pi-retry-{}-{:x}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir(&directory).unwrap();
    let _directory = Directory(directory.clone());
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = directory.join("home");
    std::fs::create_dir(&home).unwrap();
    for expected in ["installed", "unchanged"] {
        let output = std::process::Command::new(&helper)
            .args(["--json", "bootstrap", "pi"])
            .env_clear()
            .env("HOME", &home)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(
            output.status.success() && output.stderr.is_empty(),
            "bare bootstrap failed"
        );
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(report["result"] == expected && report["root_source"] == "vendor-default");
    }
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/pi-native-retry-fixture.mjs")
        .canonicalize()
        .unwrap();
    let bin = directory.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let pi = bin.join("pi");
    std::fs::write(&pi, format!("#!/bin/sh\n{} {}\nresult=$?\nprintf '%s\\n' \"$result\" > \"$HOME/retry-exit.tmp\"\nmv \"$HOME/retry-exit.tmp\" \"$HOME/retry-exit\"\nexit \"$result\"\n", shlex::try_quote(node.to_str().unwrap()).unwrap(), shlex::try_quote(script.to_str().unwrap()).unwrap())).unwrap();
    std::fs::set_permissions(&pi, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = directory.join("control.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    crate::runtime::enable_unix_writer_credentials(listener.as_raw_fd()).unwrap();
    let usage = directory.join("usage.sqlite");
    let mut service = RuntimeServiceFixture::new().control_socket(&path).build();
    let primary = service
        .attach_primary(
            "retry-user",
            true,
            mez_mux::layout::Size::new(100, 40).unwrap(),
            120,
        )
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    let processes = Processes::default();
    processes.track(
        mez_mux::process::process_parent_identity_for_pid(
            service.primary_pid_for_live_pane_process("%1").unwrap(),
        )
        .unwrap(),
    );
    service
        .session
        .set_pane_title_explicit("%1", "Pinned retry title")
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
    service.toggle_active_agent_shell().unwrap();
    let clients = service.session().clients().len();
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
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let actor_task = tokio::spawn(actor.run());
    let (socket, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let (first, ()) = tokio::join!(
        connection(
            socket,
            handle.clone(),
            audit.clone(),
            true,
            &node,
            &processes
        ),
        async {
            wait_phase(&home, "registered").await;
            release(&home, "registered");
        }
    );
    let (state, failed) = first;
    assert_eq!(
        failed.unwrap_err().io_kind(),
        Some(std::io::ErrorKind::BrokenPipe)
    );
    let producer = state.unix_origin().unwrap().identity;
    assert!(state.unix_origin().unwrap().is_live());
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
    let revision = committed.streams[0].1;
    let (socket, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let (state, failed) = connection(
        socket,
        handle.clone(),
        audit.clone(),
        true,
        &node,
        &processes,
    )
    .await;
    assert_eq!(
        failed.unwrap_err().io_kind(),
        Some(std::io::ErrorKind::BrokenPipe)
    );
    assert!(state.unix_origin().unwrap().identity == producer);
    assert!(
        ledger(&usage) == committed,
        "second actual lost replay changed expense"
    );
    let (socket, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let (third, ()) = tokio::join!(
        connection(
            socket,
            handle.clone(),
            audit.clone(),
            false,
            &node,
            &processes
        ),
        async {
            wait_phase(&home, "exhausted").await;
            assert!(
                ledger(&usage) == committed,
                "exhausted immediate retries changed expense"
            );
            handle
                .execute_terminal_command(primary.clone(), "agent-shell".into())
                .await
                .unwrap();
            let status = handle
                .execute_agent_shell_command(primary.clone(), "/status".into())
                .await
                .unwrap();
            assert!(status.contains("unavailable-pi-usage-durable-acknowledgment"));
            assert!(!status.contains("PRIVATE_RETRY_CONTENT"));
            // Release the later callback only in a genuinely later UTC second.
            // Equality then proves a frozen report clock rather than timing luck.
            tokio::time::timeout(Duration::from_secs(5), async {
                while crate::runtime::current_unix_seconds() <= committed.events[0].1 as u64 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            release(&home, "exhausted");
            wait_phase(&home, "replayed").await;
            assert!(
                ledger(&usage) == committed,
                "later callback replay changed durable identity/amount/revision"
            );
            let status = handle
                .execute_agent_shell_command(primary.clone(), "/status".into())
                .await
                .unwrap();
            // Native coverage selects the sticky epoch gap instead of the
            // generic reported label. The child's durable ACK and conserved
            // ledger establish reporting; neither erases the truthful loss.
            assert!(
                status.contains("actual-model")
                    && status.contains("unavailable-pi-usage-durable-acknowledgment")
            );
            let reports = frames(&audit.lock().unwrap().requests)
                .iter()
                .filter(|row| row["method"] == "agent/external/usage-end")
                .count();
            assert_eq!(reports, 3);
            release(&home, "replayed");
            wait_phase(&home, "known").await;
            assert_eq!(
                frames(&audit.lock().unwrap().requests)
                    .iter()
                    .filter(|row| row["method"] == "agent/external/usage-end")
                    .count(),
                reports
            );
            assert!(ledger(&usage) == committed);
            release(&home, "known");
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
            assert!(
                next.sources == committed.sources
                    && next.requests.contains(&committed.requests[0])
                    && next.events.contains(&committed.events[0])
            );
            assert_eq!(next.events.iter().map(|row| row.2).sum::<i64>(), 20);
            assert!(
                handle
                    .pending_agent_provider_tasks()
                    .await
                    .unwrap()
                    .is_empty()
            );
            release(&home, "subsequent");
        }
    );
    let (state, served) = third;
    assert!(served.unwrap() > 0);
    assert!(state.unix_origin().unwrap().identity == producer);
    // Completion is required after EVERY child callback/teardown assertion,
    // independently of the last native socket's EOF or phase publication.
    let status = child_status(&home).await;
    handle.shutdown().await.unwrap();
    let mut returned = actor_task.await.unwrap().service;
    if status != Some(0) {
        returned.terminate_all_pane_processes().unwrap();
        panic!("installed retry child exited unsuccessfully: {status:?}");
    }
    wait_phase(&home, "complete").await;
    assert_eq!(returned.session().clients().len(), clients);
    assert!(returned.session().pane_title_state("%1").unwrap().0 == "Pinned retry title");
    assert!(returned.agent_shell_store().get("%1").unwrap().session_id == native);
    assert!(returned.agent_turn_ledger().turns().is_empty());
    assert_eq!(returned.control.external_agents().bindings.len(), 1);
    assert!(
        returned
            .control
            .external_agents()
            .bindings
            .values()
            .all(|binding| {
                binding.retired
                    && binding.enrollment.as_ref().unwrap().request_reports_seen
                    && binding
                        .pi_lifecycle
                        .as_ref()
                        .and_then(|projection| projection.usage_gap_coverage())
                        == Some("unavailable-pi-usage-durable-acknowledgment")
            })
    );
    assert!(returned.external_agent_rows().is_empty());
    assert_eq!(
        returned.external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
    assert_wire(&audit.lock().unwrap(), revision);
    returned.terminate_all_pane_processes().unwrap();
}

/// Two actual durable replies are dropped before bytes. The installed ordinary
/// producer publishes its real failure diagnostic, stays in its original epoch,
/// and a later exact finalized callback retries only the original report once.
/// Existing charge/owner/origin/clock/revision are conserved, a known ACK is inert,
/// the next genuine request charges once, and final successful child exit is mandatory.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit existing Node and freshly built sibling mez required; offline native double-ACK qualification"]
async fn external_pi_native_exact_callback_retries_after_two_lost_durable_acks() {
    tokio::time::timeout(Duration::from_secs(45), qualify())
        .await
        .unwrap();
}
