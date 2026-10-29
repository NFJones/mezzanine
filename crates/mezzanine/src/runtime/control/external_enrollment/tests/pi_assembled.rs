//! Bare Pi bootstrap to ordinary native producer acceptance, entirely offline.
//!
//! The real product CLI owns root selection/publication/helper pinning. A normal
//! PATH command discovers installed package entries and emits source-shaped
//! callbacks; genuine kernel origin/writer evidence owns admission and ACKs.
//! Usage receipts and terminal source retirement have distinct typed ACKs;
//! neither can stand in for the other's durable completion evidence.
//! This qualifies the assembled local chain, not a real Pi SDK/provider, daemon
//! restart, source-recovery protocol, or the complete multi-vendor support goal.

use super::*;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// Records bounded private fixture wire bytes for schema/privacy assertions.
/// Captured capability replies stay in test memory and never enter diagnostics.
#[derive(Default)]
pub(super) struct WireAudit {
    pub(super) requests: Vec<u8>,
    pub(super) replies: Vec<u8>,
}

/// Wraps an already native-qualified stream without fabricating origin or ACKs.
pub(super) struct AuditedIo<S> {
    pub(super) inner: S,
    pub(super) audit: Arc<std::sync::Mutex<WireAudit>>,
}

impl<S: AsyncRead + Unpin> AsyncRead for AuditedIo<S> {
    /// Captures only bytes actually returned by the genuine native read owner.
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

impl<S: AsyncWrite + Unpin> AsyncWrite for AuditedIo<S> {
    /// Captures only successfully written reply bytes; no report is completed here.
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.inner).poll_write(cx, bytes);
        if let Poll::Ready(Ok(written)) = result {
            let mut audit = this.audit.lock().unwrap();
            assert!(audit.replies.len() + written <= 65536);
            audit.replies.extend_from_slice(&bytes[..written]);
        }
        result
    }

    /// Delegates actual flush ordering to the transport, preserving durable ACK semantics.
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    /// Delegates shutdown without adding another lifecycle owner.
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

/// Decodes completed actual control frames without printing private wire values.
pub(super) fn wire_frames(mut bytes: &[u8]) -> Vec<serde_json::Value> {
    let mut frames = Vec::new();
    while !bytes.is_empty() {
        let (body, consumed) = crate::control_client::decode_control_frame(bytes, 8192).unwrap();
        frames.push(serde_json::from_str(&body).unwrap());
        bytes = &bytes[consumed..];
    }
    frames
}

/// Bounded marker observation coordinates test-owned phases, never enrollment,
/// usage completion or runtime capabilities. The producer publishes atomically.
async fn wait_phase(home: &std::path::Path, phase: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if std::fs::read_to_string(home.join("assembled-phase"))
                .ok()
                .as_deref()
                == Some(phase)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("ordinary producer did not reach fixture phase {phase}"));
}

/// Uses only the actual product command and ordinary HOME root selection.
/// Installation reporting remains distinct from live lifecycle/accounting proof.
pub(super) fn bare_bootstrap(
    helper: &std::path::Path,
    home: &std::path::Path,
    dry_run: bool,
) -> serde_json::Value {
    let mut command = std::process::Command::new(helper);
    command.args(["--json", "bootstrap", "pi"]);
    if dry_run {
        command.arg("--dry-run");
    }
    let output = command
        .env_clear()
        .env("HOME", home)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "bare bootstrap failed");
    assert!(output.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["root_source"], "vendor-default");
    assert_eq!(
        report["scope_root"],
        home.join(".pi/agent").to_str().unwrap()
    );
    assert_eq!(report["lifecycle"], "unverified");
    assert_eq!(report["usage"], "unverified");
    report
}

/// Searches only the unique fixture HOME for an exact preserved private edit.
/// The entry budget and exact-size filter bound traversal/reads; directory links
/// are never followed and this does not inspect any real user configuration.
fn contains_preserved_edit(directory: &std::path::Path, bytes: &[u8], budget: &mut usize) -> bool {
    for entry in std::fs::read_dir(directory).unwrap() {
        assert!(
            *budget > 0,
            "private fixture archive traversal exceeded its bound"
        );
        *budget -= 1;
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        if kind.is_dir() && contains_preserved_edit(&entry.path(), bytes, budget) {
            return true;
        }
        if kind.is_file()
            && entry.metadata().unwrap().len() == bytes.len() as u64
            && std::fs::read(entry.path()).unwrap() == bytes
        {
            return true;
        }
    }
    false
}

/// Production bare CLI preview/install/repeat discovers its default HOME root,
/// then ordinary `pi` loads the installed factory without entry/helper arguments.
/// Genuine Node writer/parent/pane proof publishes typed finalized expense and
/// missing-counter gaps; status/browser remain observational, native state and
/// title pins coexist, and exact quit retires with no primary/provider authority.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit installed MEZ_TEST_NODE_BINARY and freshly built sibling mez required; offline assembled acceptance"]
async fn external_pi_bare_bootstrap_ordinary_launch_accounting_and_retirement() {
    use crate::host::async_runtime::{
        AsyncRuntimeActorConfig, AsyncRuntimeControlConnectionConfig, AsyncRuntimeSessionActor,
        serve_authenticated_async_runtime_control_connection_loop_with_snapshots,
    };
    use std::os::unix::fs::PermissionsExt;
    /// Removes only this isolated bootstrap/socket fixture on all exits.
    struct Directory(std::path::PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let node = std::path::PathBuf::from(
        std::env::var_os("MEZ_TEST_NODE_BINARY").expect("explicit already-installed Node path"),
    );
    assert!(node.is_absolute() && node.is_file());
    let helper = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("mez");
    assert!(
        helper.is_file(),
        "build the real mez binary before qualification"
    );
    let directory = std::fs::canonicalize("/tmp").unwrap().join(format!(
        "mez-pi-assembled-{}-{:x}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir(&directory).unwrap();
    let _directory = Directory(directory.clone());
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = directory.join("home");
    std::fs::create_dir(&home).unwrap();
    let root = home.join(".pi/agent");
    assert_eq!(bare_bootstrap(&helper, &home, true)["result"], "preview");
    assert!(!root.exists());
    assert!(!home.join(".config").exists());
    assert_eq!(bare_bootstrap(&helper, &home, false)["result"], "installed");
    std::fs::write(root.join("settings.json"), b"{\"authored\":true}").unwrap();
    std::fs::write(
        root.join("extensions/authored.mjs"),
        b"// unrelated authored extension\n",
    )
    .unwrap();
    assert_eq!(bare_bootstrap(&helper, &home, false)["result"], "unchanged");
    let entry = root.join("extensions/mezzanine/index.mjs");
    let installed_entry = std::fs::read(&entry).unwrap();
    let private_edit = b"PRIVATE_OWNED_INDEX_EDIT_PRESERVE_EXACTLY\n";
    std::fs::write(&entry, private_edit).unwrap();
    assert_eq!(bare_bootstrap(&helper, &home, true)["result"], "preview");
    assert_eq!(std::fs::read(&entry).unwrap(), private_edit);
    let repaired = bare_bootstrap(&helper, &home, false);
    assert!(!repaired["preserved_paths"].as_array().unwrap().is_empty());
    assert_eq!(std::fs::read(&entry).unwrap(), installed_entry);
    assert!(contains_preserved_edit(&home, private_edit, &mut 512));
    assert_eq!(bare_bootstrap(&helper, &home, false)["result"], "unchanged");
    assert_eq!(
        std::fs::read(root.join("settings.json")).unwrap(),
        b"{\"authored\":true}"
    );
    assert_eq!(
        std::fs::read(root.join("extensions/authored.mjs")).unwrap(),
        b"// unrelated authored extension\n"
    );
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/pi-bootstrap-assembled-fixture.mjs")
        .canonicalize()
        .unwrap();
    let outside = std::process::Command::new(&node)
        .arg(&script)
        .env_clear()
        .env("HOME", &home)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        outside.status.success(),
        "outside-Mez callbacks changed ordinary exit"
    );
    assert_eq!(outside.stdout, b"PI_OFFLINE_COMPLETE\n");
    assert!(outside.stderr.is_empty());
    std::fs::remove_file(home.join("assembled-completed")).unwrap();
    let bin = directory.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let pi = bin.join("pi");
    std::fs::write(
        &pi,
        format!(
            "#!/bin/sh\nexec {} {}\n",
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
    let mut service = RuntimeServiceFixture::new().control_socket(&path).build();
    let primary = service
        .attach_primary(
            "assembled-user",
            true,
            mez_mux::layout::Size::new(100, 40).unwrap(),
            120,
        )
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .session
        .set_pane_title_explicit("%1", "Authored title")
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
    // An ordinary primary UI toggle retains the native conversation while
    // presenting the process surface. Telemetry never grants input rights.
    assert_eq!(
        service.toggle_active_agent_shell().unwrap().2,
        crate::runtime::AgentShellVisibility::Hidden
    );
    let clients = service.session().clients().len();
    let usage = directory.join("usage.sqlite");
    service.set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&usage));
    let launch = format!(
        "HOME={} PATH={}:$PATH pi\n",
        shlex::try_quote(home.to_str().unwrap()).unwrap(),
        shlex::try_quote(bin.to_str().unwrap()).unwrap()
    );
    service
        .write_runtime_pane_input("%1", launch.as_bytes())
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let audit = Arc::new(std::sync::Mutex::new(WireAudit::default()));
    let server = async {
        let (mut socket, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let uid = crate::runtime::current_effective_uid();
        let origin = crate::runtime::capture_unix_origin(socket.as_raw_fd(), uid).unwrap();
        assert_ne!(origin.identity.process_id, std::process::id());
        assert_eq!(
            mez_mux::process::process_executable_identity_for_pid(origin.identity.process_id)
                .unwrap()
                .executable_path,
            node.canonicalize().unwrap()
        );
        let mut connection = ControlConnectionState::new(true, false);
        connection
            .bind_authenticated_peer(AuthenticatedPeer::unix_user(origin.uid()))
            .unwrap();
        connection.bind_unix_origin(Arc::new(origin)).unwrap();
        let qualified =
            crate::runtime::UnixOriginStream::new(&mut socket, connection.unix_origin().cloned());
        let mut audited = AuditedIo {
            inner: qualified,
            audit: audit.clone(),
        };
        let transport = serve_authenticated_async_runtime_control_connection_loop_with_snapshots(
            &mut audited,
            AuthenticatedPeer::unix_user(uid),
            &handle,
            &mut connection,
            AsyncRuntimeControlConnectionConfig::new(8192, uid).unwrap(),
            None,
            |_, _| false,
        );
        let inspect = async {
            for phase in ["registered", "waiting", "accounted", "gap"] {
                wait_phase(&home, phase).await;
                handle
                    .execute_terminal_command(primary.clone(), "agent-shell".into())
                    .await
                    .unwrap();
                let status = handle
                    .execute_agent_shell_command(primary.clone(), "/status".into())
                    .await
                    .unwrap();
                assert!(status.contains("### External Telemetry"));
                assert!(!status.contains("PRIVATE_CALLBACK_CONTENT"));
                if phase == "waiting" {
                    assert!(status.contains("input-wait"));
                }
                if phase == "accounted" {
                    assert!(status.contains("actual-model"));
                    assert!(status.contains("unavailable-pi-usage-response-correlation"));
                }
                if phase == "gap" {
                    assert!(status.contains("finalized-counters"));
                }
                let list = handle
                    .execute_terminal_command(primary.clone(), "list-agents".into())
                    .await
                    .unwrap();
                assert!(list.contains("pi") || list.contains("Pi"));
                assert!(!list.contains("PRIVATE_CALLBACK_CONTENT"));
                let config = handle
                    .terminal_client_loop_config(Default::default())
                    .await
                    .unwrap();
                let view = handle
                    .render_client_view(
                        crate::runtime::ClientViewRole::Primary,
                        mez_mux::layout::Size::new(100, 40).unwrap(),
                        config,
                    )
                    .await
                    .unwrap()
                    .unwrap();
                assert!(
                    view.lines
                        .iter()
                        .any(|line| line.contains("Authored title")),
                    "user-pinned title was lost while external telemetry was live"
                );
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
                // Only a synthetic-provider sequencing marker crosses this
                // test gate; no session, ticket, capability or usage data.
                std::fs::write(home.join("assembled-next.tmp"), phase).unwrap();
                std::fs::rename(home.join("assembled-next.tmp"), home.join("assembled-next"))
                    .unwrap();
            }
        };
        let (served, ()) = tokio::join!(transport, inspect);
        assert!(served.unwrap() > 0);
        assert!(!connection.initialized());
        assert!(connection.caller_client_id().is_none());
        assert!(connection.unix_origin().unwrap().writer_confirmed());
        handle.shutdown().await.unwrap();
    };
    let ((), mut exit) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(server, actor.run())
    })
    .await
    .unwrap();
    assert_eq!(exit.service.session().clients().len(), clients);
    assert_eq!(
        exit.service.session().pane_title_state("%1").unwrap().0,
        "Authored title"
    );
    assert_eq!(
        exit.service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .session_id,
        native_conversation
    );
    assert!(exit.service.agent_turn_ledger().turns().is_empty());
    assert_eq!(exit.service.control.external_agents().bindings.len(), 1);
    let binding = exit
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap();
    assert!(binding.retired);
    assert_eq!(binding.harness, "pi");
    assert!(binding.enrollment.as_ref().unwrap().request_reports_seen);
    assert!(exit.service.external_agent_rows().is_empty());
    assert_eq!(
        exit.service.external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
    let database = rusqlite::Connection::open(&usage).unwrap();
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM external_usage_requests", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        4
    );
    // The missing-counter turn has an original readiness marker, not a zero
    // charge. Only finalized reports link observations to three expense rows.
    assert_eq!(
        database
            .query_row(
                "SELECT count(*) FROM external_usage_requests WHERE observation_id IS NOT NULL",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        3
    );
    assert_eq!(
        database
            .query_row(
                "SELECT count(*) FROM external_usage_observations",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        3
    );
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM token_usage_events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        3
    );
    let counters = database.query_row("SELECT sum(input_tokens), sum(output_tokens), sum(reasoning_tokens), sum(cached_input_tokens), sum(cache_write_input_tokens) FROM token_usage_events", [], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get::<_, i64>(3)?, row.get::<_, i64>(4)?))).unwrap();
    assert_eq!(counters, (30, 21, 12, 6, 9));
    let audit = audit.lock().unwrap();
    let requests = wire_frames(&audit.requests);
    let replies = wire_frames(&audit.replies);
    assert!(requests.iter().all(|request| matches!(
        request["method"].as_str(),
        Some(
            "agent/external/enroll"
                | "agent/external/pi-observation"
                | "agent/external/usage-begin"
                | "agent/external/usage-end"
                | "agent/external/source-end"
        )
    )));
    let enrolled = replies
        .iter()
        .find(|reply| reply["result"]["registered"] == true)
        .unwrap();
    assert_eq!(enrolled["result"]["controls"], serde_json::json!([]));
    assert_eq!(enrolled["result"]["usage"], "unavailable-source-continuity");
    for request in requests
        .iter()
        .filter(|request| request["method"] == "agent/external/usage-end")
    {
        assert_eq!(request["params"]["model"], "actual-model");
        assert_eq!(request["params"]["provider"], "pi-offline-provider");
        assert_eq!(request["params"]["counters"]["input_tokens"], 10);
        assert!(request["params"]["observed_at"].as_u64().unwrap() > 1);
        let reply = replies
            .iter()
            .find(|reply| reply["id"] == request["id"])
            .unwrap();
        assert_eq!(reply["result"]["durable"], true);
        assert!(reply["result"]["applied"].is_boolean());
        assert!(reply["result"]["revision"].as_u64().unwrap() > 0);
    }
    assert!(!String::from_utf8_lossy(&audit.requests).contains("PRIVATE_CALLBACK_CONTENT"));
    assert!(!String::from_utf8_lossy(&audit.requests).contains("PRIVATE_UPSTREAM_RESPONSE"));
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "agent/external/enroll")
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "agent/external/usage-end")
            .count(),
        3
    );
    assert_eq!(
        replies
            .iter()
            .filter(|reply| reply["result"]["durable"] == true)
            .count(),
        4
    );
    let endings = requests
        .iter()
        .filter(|request| request["method"] == "agent/external/source-end")
        .collect::<Vec<_>>();
    assert_eq!(endings.len(), 1);
    let ending = endings[0];
    assert!(
        ending["params"]
            == serde_json::json!({
                "launch_token": enrolled["result"]["launch_token"],
                "generation": enrolled["result"]["generation"],
                "external_session_id": enrolled["result"]["external_session_id"]
            }),
        "retirement must retain the original exact source selectors"
    );
    assert!(
        requests.last().unwrap()["id"] == ending["id"],
        "source retirement must not authorize another telemetry exchange"
    );
    let ended = replies
        .iter()
        .find(|reply| reply["id"] == ending["id"])
        .unwrap();
    assert!(
        ended["result"]
            == serde_json::json!({
                "protocol": "external-source-end/1", "retired": true, "durable": true,
                "generation": enrolled["result"]["generation"]
            }),
        "source retirement requires its own typed durable receipt"
    );
    drop(audit);
    exit.service.terminate_all_pane_processes().unwrap();
}
