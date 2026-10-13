//! Installed ordinary Pi finite recovery after both immediate ACKs are lost.
//!
//! Bare bootstrap supplies the actual default package to an isolated HOME. One
//! ordinary Node producer emits one current request/finalized callback, then the
//! production telemetry timer retries its retained report. Every admission and
//! acknowledgment comes from the real actor and native Unix writer/root proof.
//! The fixture loses the actual applied:true and applied:false replies before
//! writing any bytes; it never injects capabilities, ACKs, reports or SDK facts.
//! The same live service/root/source and original SQLite obligation survive.
//! Socket EOF is not acceptance: the pane shell's actual Node exit0 and child's final
//! post-assertion complete marker are required before service/process teardown.
//! This Linux/offline source-shaped qualifier is not vendor SDK, macOS, daemon
//! restart, durable outbox, invoice completeness or failed-owner recovery proof.

use super::*;
use crate::host::async_runtime::{
    AsyncRuntimeActorConfig, AsyncRuntimeControlConnectionConfig, AsyncRuntimeSessionActor,
    AsyncRuntimeSessionHandle,
    serve_authenticated_async_runtime_control_connection_loop_with_snapshots,
};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// Private bounded wire audit; contents are compared only with fixed diagnostics.
#[derive(Default)]
struct Audit {
    requests: Vec<u8>,
    replies: Vec<u8>,
    dropped: Vec<serde_json::Value>,
}

/// Faults one specified actual durable reply after commit but before ACK bytes.
struct FaultIo<S> {
    inner: S,
    audit: Arc<std::sync::Mutex<Audit>>,
    drop_applied: Option<bool>,
}

impl<S: AsyncRead + Unpin> AsyncRead for FaultIo<S> {
    /// Records only bytes obtained from the actual native writer-qualified stream.
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
    /// Identifies a full real actor reply; fails before delegating any bytes.
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        if let Some(applied) = this.drop_applied
            && let Ok((body, used)) = crate::control_client::decode_control_frame(bytes, 8192)
            && used == bytes.len()
            && let Ok(reply) = serde_json::from_str::<serde_json::Value>(&body)
            && reply["result"]["durable"] == true
            && reply["result"]["applied"] == applied
        {
            this.audit.lock().unwrap().dropped.push(reply);
            this.drop_applied = None;
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "fixture drops real durable ACK before bytes",
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

    /// Delegates real flush; this operation does not manufacture delivery.
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    /// Delegates shutdown without treating EOF as accounting or producer success.
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

/// Removes only this generated private root after service/process cleanup.
struct Directory(std::path::PathBuf);

impl Drop for Directory {
    /// Never touches real user HOME or vendor configuration.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Owns kernel-observed process incarnations for bounded panic cleanup.
#[derive(Default)]
struct Processes(Arc<std::sync::Mutex<Vec<mez_mux::process::ProcessParentIdentity>>>);

impl Drop for Processes {
    /// Reobserves exact birth/parent identity before signalling only owned PIDs.
    fn drop(&mut self) {
        for identity in self.0.lock().unwrap().iter().rev() {
            if mez_mux::process::process_parent_identity_for_pid(identity.process_id)
                == Some(*identity)
            {
                // SAFETY: complete native incarnation was just reobserved.
                unsafe {
                    libc::kill(identity.process_id as i32, libc::SIGKILL);
                }
            }
        }
    }
}

/// Original source-owned request/origin/admission/link, never printed on failure.
type RequestRow = (String, String, Option<String>, i64, Option<String>);
/// Original expense/reporting UTC and synthetic normalized counters.
type ExpenseRow = (String, i64, i64, i64, i64, Option<i64>, Option<i64>);

/// Exact fixture-owned durable evidence; no Debug derives can expose private IDs.
#[derive(PartialEq, Eq)]
struct Ledger {
    requests: Vec<RequestRow>,
    events: Vec<ExpenseRow>,
    sources: Vec<(String, String)>,
    revisions: Vec<i64>,
    observations: i64,
}

/// Reads only generated fixture SQLite after the actor committed real reports.
fn ledger(path: &std::path::Path) -> Ledger {
    let db = rusqlite::Connection::open(path).unwrap();
    let requests = db.prepare("SELECT owner,request_id,project_id,admitted_at,observation_id FROM external_usage_requests ORDER BY request_id").unwrap().query_map([], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
    let events = db.prepare("SELECT id,observed_at,input_tokens,output_tokens,reasoning_tokens,cached_input_tokens,cache_write_input_tokens FROM token_usage_events ORDER BY id").unwrap().query_map([], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?))).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
    let sources = db
        .prepare("SELECT witness,owner FROM external_usage_sources ORDER BY owner")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let revisions = db
        .prepare("SELECT revision FROM external_usage_streams ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
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
        sources,
        revisions,
        observations,
    }
}

/// Decodes complete private audit frames, never rendering raw wire diagnostics.
fn frames(mut bytes: &[u8]) -> Vec<serde_json::Value> {
    let mut result = Vec::new();
    while !bytes.is_empty() {
        let (body, used) = crate::control_client::decode_control_frame(bytes, 8192).unwrap();
        result.push(serde_json::from_str(&body).unwrap());
        bytes = &bytes[used..];
    }
    result
}

/// Executes actual bare bootstrap twice with no root/version/accounting enable.
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
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["root_source"], "vendor-default");
        assert_eq!(result["result"], expected);
    }
}

/// Waits only on a fixed scalar child phase; it carries no reporting authority.
async fn wait_recovered(home: &std::path::Path) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let phase = std::fs::read_to_string(home.join("pending-phase")).unwrap_or_default();
            assert!(phase != "unavailable", "installed pending recovery failed");
            if phase == "recovered" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("installed pending recovery phase expired");
}

/// Conjunctive completion failure includes fixed stage/scalar status only.
#[derive(Debug, PartialEq, Eq)]
enum ChildFailure {
    /// Actual shell exit is missing, malformed or unsuccessful.
    Status(Option<u8>),
    /// Successful exit lacks the child's exact post-assertion complete marker.
    Phase,
}

/// Pure conjunction is deliberately independent of socket EOF and ledger state.
fn completion(status: Option<u8>, complete: bool) -> std::result::Result<(), ChildFailure> {
    if status != Some(0) {
        Err(ChildFailure::Status(status))
    } else if !complete {
        Err(ChildFailure::Phase)
    } else {
        Ok(())
    }
}

/// Collects real producer completion while its actor/root owners remain alive.
async fn child_completion(home: &std::path::Path) -> std::result::Result<(), ChildFailure> {
    let status = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let path = home.join("pending-exit");
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
    .flatten();
    let path = home.join("pending-phase");
    let complete = tokio::fs::metadata(&path)
        .await
        .is_ok_and(|metadata| metadata.len() == 8)
        && tokio::fs::read_to_string(path).await.ok().as_deref() == Some("complete");
    completion(status, complete)
}

/// Serves a real Node writer over production native proof and actor dispatch.
async fn connection(
    mut socket: tokio::net::UnixStream,
    handle: AsyncRuntimeSessionHandle,
    audit: Arc<std::sync::Mutex<Audit>>,
    drop_applied: Option<bool>,
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
            == node.canonicalize().unwrap(),
        "native writer is not selected Node"
    );
    processes.0.lock().unwrap().push(origin.identity);
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
        drop_applied,
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

/// Proves immutable original report/readiness, healthy gap and actual inert ACK.
fn assert_recovery(audit: &Audit) {
    let requests = frames(&audit.requests);
    let replies = frames(&audit.replies);
    assert_eq!(audit.dropped.len(), 2);
    assert!(
        audit.dropped[0]["result"]["applied"] == true
            && audit.dropped[1]["result"]["applied"] == false
    );
    let ends = requests
        .iter()
        .filter(|row| row["method"] == "agent/external/usage-end")
        .collect::<Vec<_>>();
    assert_eq!(ends.len(), 3);
    for field in [
        "request_id",
        "event_id",
        "provider",
        "model",
        "counters",
        "observed_at",
    ] {
        assert!(
            ends.iter()
                .all(|row| row["params"][field] == ends[0]["params"][field]),
            "original incurred report changed"
        );
    }
    let begins = requests
        .iter()
        .filter(|row| row["method"] == "agent/external/usage-begin")
        .collect::<Vec<_>>();
    assert_eq!(begins.len(), 4);
    assert!(
        begins
            .iter()
            .all(|row| row["params"]["request_id"] == ends[0]["params"]["request_id"]),
        "new request marker was minted"
    );
    let readiness = replies
        .iter()
        .filter(|row| begins.iter().any(|begin| begin["id"] == row["id"]))
        .collect::<Vec<_>>();
    assert_eq!(readiness.len(), 4);
    for field in ["request_id", "admitted_at", "source_witness", "origin"] {
        assert!(
            readiness
                .iter()
                .all(|row| row["result"][field] == readiness[0]["result"][field]),
            "original native readiness changed"
        );
    }
    let recovered = replies
        .iter()
        .find(|row| row["id"] == ends[2]["id"])
        .unwrap();
    assert!(recovered["result"]["durable"] == true && recovered["result"]["applied"] == false);
    assert!(recovered["result"]["revision"] == audit.dropped[0]["result"]["revision"]);
    assert!(
        audit
            .dropped
            .iter()
            .all(|lost| !replies.iter().any(|reply| reply["id"] == lost["id"])),
        "pre-byte ACK loss leaked reply"
    );
    let gap = requests
        .iter()
        .filter(|row| {
            row["method"] == "agent/external/pi-observation"
                && row["params"]["event"]["type"] == "usage_gap"
        })
        .collect::<Vec<_>>();
    assert_eq!(gap.len(), 1);
    assert!(
        gap[0]["params"]["event"]
            == serde_json::json!({"type":"usage_gap","reason":"durable-acknowledgment"})
    );
    assert!(
        replies.iter().any(|reply| reply["id"] == gap[0]["id"]
            && reply["result"]["accepted"] == true
            && reply["result"]["sequence"] == gap[0]["params"]["sequence"]
            && reply["result"]["retired"] == false),
        "truthful gap lacked a typed healthy receipt"
    );
    let enrolls = requests
        .iter()
        .filter(|row| row["method"] == "agent/external/enroll")
        .collect::<Vec<_>>();
    assert_eq!(enrolls.len(), 3);
    assert!(
        enrolls
            .iter()
            .all(|row| row["params"]["observer_instance"]
                == enrolls[0]["params"]["observer_instance"]),
        "observer instance replaced"
    );
    let handles = replies
        .iter()
        .filter(|row| row["result"]["registered"] == true)
        .collect::<Vec<_>>();
    assert_eq!(handles.len(), 3);
    for field in [
        "observer_witness",
        "generation",
        "run_id",
        "observer_epoch",
        "launch_token",
    ] {
        assert!(
            handles
                .iter()
                .all(|row| row["result"][field] == handles[0]["result"][field]),
            "native owner changed"
        );
    }
    assert!(!String::from_utf8_lossy(&audit.requests).contains("PRIVATE_PENDING_CONTENT"));
}

/// Builds only private generated HOME and actual ordinary pane process flow.
/// It preserves service/root/store between all connections, collects successful
/// producer completion before teardown, and never alters native owner indexes.
async fn qualify() {
    use std::os::unix::fs::PermissionsExt;
    let node = std::path::PathBuf::from(
        std::env::var_os("MEZ_TEST_NODE_BINARY").expect("explicit installed Node required"),
    );
    let helper = std::path::PathBuf::from(
        std::env::var_os("MEZ_TEST_HELPER_BINARY").expect("fresh own helper required"),
    );
    assert!(node.is_absolute() && node.is_file() && helper.is_absolute() && helper.is_file());
    let directory = Directory(std::fs::canonicalize("/tmp").unwrap().join(format!(
        "mez-pi-pending-{}-{:x}",
        std::process::id(),
        rand::random::<u64>()
    )));
    std::fs::create_dir(&directory.0).unwrap();
    std::fs::set_permissions(&directory.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = directory.0.join("home");
    std::fs::create_dir(&home).unwrap();
    bootstrap(&helper, &home);
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/pi-native-pending-fixture.mjs")
        .canonicalize()
        .unwrap();
    let bin = directory.0.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let pi = bin.join("pi");
    std::fs::write(&pi, format!("#!/bin/sh\n{} {}\nresult=$?\nprintf '%s\\n' \"$result\" > \"$HOME/pending-exit.tmp\"\nmv \"$HOME/pending-exit.tmp\" \"$HOME/pending-exit\"\nexit \"$result\"\n", shlex::try_quote(node.to_str().unwrap()).unwrap(), shlex::try_quote(script.to_str().unwrap()).unwrap())).unwrap();
    std::fs::set_permissions(&pi, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = directory.0.join("control.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    crate::runtime::enable_unix_writer_credentials(listener.as_raw_fd()).unwrap();
    let usage = directory.0.join("usage.sqlite");
    let mut service = RuntimeServiceFixture::new().control_socket(&path).build();
    let primary = service
        .attach_primary(
            "pending-user",
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
    processes.0.lock().unwrap().push(root);
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
    let mut producer = None;
    let mut committed = None;
    for applied in [true, false] {
        let (socket, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let (state, served) = connection(
            socket,
            handle.clone(),
            audit.clone(),
            Some(applied),
            &node,
            &processes,
        )
        .await;
        assert_eq!(
            served.unwrap_err().io_kind(),
            Some(std::io::ErrorKind::BrokenPipe)
        );
        let actual = state.unix_origin().unwrap();
        assert!(actual.is_live());
        if let Some(original) = producer {
            assert!(actual.identity == original, "producer incarnation changed");
        } else {
            producer = Some(actual.identity);
        }
        let current = ledger(&usage);
        assert_eq!(
            (
                current.requests.len(),
                current.events.len(),
                current.sources.len(),
                current.revisions.len(),
                current.observations
            ),
            (1, 1, 1, 1, 1)
        );
        assert!(
            current.requests[0].0 == current.sources[0].1,
            "request lost its original durable source owner"
        );
        assert!(
            current.requests[0].4.is_some(),
            "durable request has no incurred observation link"
        );
        assert_eq!(
            (
                current.events[0].2,
                current.events[0].3,
                current.events[0].4,
                current.events[0].5,
                current.events[0].6
            ),
            (10, 7, 4, Some(2), Some(3))
        );
        if let Some(original) = committed.as_ref() {
            assert!(&current == original, "immediate replay changed ledger");
        } else {
            committed = Some(current);
        }
    }
    let committed = committed.unwrap();
    {
        let audit = audit.lock().unwrap();
        let requests = frames(&audit.requests);
        let ends = requests
            .iter()
            .filter(|row| row["method"] == "agent/external/usage-end")
            .collect::<Vec<_>>();
        assert_eq!(ends.len(), 2);
        assert!(
            ends[0]["params"]["request_id"].as_str() == Some(committed.requests[0].1.as_str()),
            "ledger request differs from original marker"
        );
        assert!(
            ends[0]["params"]["observed_at"].as_i64() == Some(committed.events[0].1),
            "ledger reporting UTC differs from original frozen report"
        );
    }
    let (socket, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let (served, ()) = tokio::join!(
        connection(
            socket,
            handle.clone(),
            audit.clone(),
            None,
            &node,
            &processes
        ),
        async {
            wait_recovered(&home).await;
            assert!(
                ledger(&usage) == committed,
                "timer replay changed durable obligation"
            );
            handle
                .execute_terminal_command(primary.clone(), "agent-shell".into())
                .await
                .unwrap();
            let status = handle
                .execute_agent_shell_command(primary.clone(), "/status".into())
                .await
                .unwrap();
            // Sticky epoch gap deliberately takes precedence over the generic
            // reported coverage label. Real ACKs and the conserved ledger prove
            // delivery; displaying them must not erase the truthful gap.
            assert!(
                status.contains("actual-model")
                    && status.contains("unavailable-pi-usage-durable-acknowledgment"),
                "status lost expense or sticky gap"
            );
            assert!(!status.contains("request-bound-external-reported"), "generic coverage masked the sticky gap");
            assert!(!status.contains("PRIVATE_PENDING_CONTENT"));
            assert!(
                handle
                    .pending_agent_provider_tasks()
                    .await
                    .unwrap()
                    .is_empty()
            );
            std::fs::write(home.join("pending-next.tmp"), "recovered").unwrap();
            std::fs::rename(home.join("pending-next.tmp"), home.join("pending-next")).unwrap();
        }
    );
    let (state, served) = served;
    assert!(served.unwrap() > 0);
    assert!(
        state.unix_origin().unwrap().identity == producer.unwrap(),
        "timer used another native producer"
    );
    child_completion(&home).await.unwrap(); // BEFORE actor shutdown or process cleanup.
    assert!(
        ledger(&usage) == committed,
        "teardown changed incurred expense"
    );
    assert_recovery(&audit.lock().unwrap());
    handle.shutdown().await.unwrap();
    let mut returned = actor_task.await.unwrap().service;
    assert!(
        mez_mux::process::process_parent_identity_for_pid(root.process_id) == Some(root),
        "original pane root was replaced"
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
    assert_eq!(
        returned.external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
    returned.terminate_all_pane_processes().unwrap();
}

/// A complete marker cannot excuse nonzero/missing exit and a neutral exit
/// cannot excuse absent assertions; fixed completion data redacts all wire IDs.
#[test]
fn pi_pending_completion_requires_real_exit_and_final_assertions() {
    assert_eq!(completion(Some(0), true), Ok(()));
    assert_eq!(completion(Some(0), false), Err(ChildFailure::Phase));
    for status in [None, Some(1), Some(78), Some(137)] {
        for complete in [false, true] {
            assert_eq!(
                completion(status, complete),
                Err(ChildFailure::Status(status))
            );
        }
    }
}

/// Real pre-byte applied:true then inert applied:false ACK loss exhausts the
/// unchanged core's two immediate attempts. The same healthy installed ordinary
/// owner then recovers using its real default finite timer, without a second
/// vendor callback, synthetic parent ACK or another marker/source/namespace.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit already-installed Node and freshly built own mez required; native finite pending recovery"]
async fn external_pi_native_pending_timer_recovers_without_vendor_callback() {
    tokio::time::timeout(Duration::from_secs(45), qualify())
        .await
        .unwrap();
}
