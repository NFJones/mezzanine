//! Offline installed-native acceptance for the source-feasible supported subset.
//!
//! The production CLI installs into isolated default roots. Four independent
//! ordinary children on two real panes discover their installed configuration
//! and emit source-shaped callbacks themselves. The parent serves production
//! Unix framing, current-writer credentials and actor settlement; it never
//! manufactures enrollment, request readiness, usage completion or capabilities.
//! Claude's loader is an explicitly SDK-shaped shim, not a real vendor loader.
//! Actor tasks are restarted only to inspect the same retained service at phase
//! barriers, not to claim daemon replacement, restart continuity or full billing.

use super::*;
use crate::host::async_runtime::{
    AsyncRuntimeActorConfig, AsyncRuntimeControlConnectionConfig, AsyncRuntimeSessionActor,
    serve_authenticated_async_runtime_control_connection_loop_with_snapshots,
};
use crate::host::terminal::{AttachedTerminalClientStepPlan, TerminalClientLoopAction};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

/// Read-only wire audit outside the real writer-qualified stream. Captured
/// bytes are bounded test evidence, never modified or synthesized on ingress.
struct WireAudit<S> {
    stream: S,
    received: Vec<u8>,
}

impl<S: AsyncRead + Unpin> AsyncRead for WireAudit<S> {
    /// Forwards native reads unchanged and retains only newly filled bytes.
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.stream).poll_read(cx, buf);
        if matches!(result, Poll::Ready(Ok(()))) {
            self.received.extend_from_slice(&buf.filled()[before..]);
            assert!(
                self.received.len() <= 65536,
                "bounded installed-helper exchange"
            );
        }
        result
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for WireAudit<S> {
    /// Forwards reply bytes without altering ACKs or native writer semantics.
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, bytes)
    }
    /// Flushes the unchanged production reply.
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }
    /// Shuts down only this fixture helper connection.
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

/// Rejects content-bearing fields and markers in actual framed helper requests.
/// This inspects child-produced evidence; it never fills a parent usage report.
fn audit_wire(bytes: &[u8]) {
    assert!(!bytes.is_empty());
    for forbidden in [
        "PRIVATE_MATRIX_CONTENT",
        "\"prompt\"",
        "\"answer\"",
        "\"text\"",
        "\"transcript_path\"",
        "\"transcriptPath\"",
        "\"user_email\"",
        "\"user.email\"",
        "\"error\"",
        "\"toolUses\"",
        "\"launch_token\"",
        "\"process_id\"",
        "\"pid\"",
    ] {
        assert!(
            !bytes
                .windows(forbidden.len())
                .any(|window| window == forbidden.as_bytes()),
            "forbidden field reached installed helper wire: {forbidden}"
        );
    }
}

/// Owns only fixture roots and panes, removing them even on assertion failure.
struct Matrix {
    service: RuntimeSessionService,
    directory: PathBuf,
    listener: tokio::net::UnixListener,
}

impl Drop for Matrix {
    /// Terminates fixture pane processes without touching any user process.
    fn drop(&mut self) {
        let _ = self.service.terminate_all_pane_processes();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// Retains the actual fake vendor's kernel identity at its child-owned barrier.
struct Vendor {
    harness: &'static str,
    pane: &'static str,
    socket: tokio::net::UnixStream,
    origin: Arc<crate::runtime::UnixOriginProcess>,
}

/// Captures names, bytes and non-access-time metadata for read-only comparisons.
type TreeRow = (PathBuf, Vec<u8>, u64, u32, i64, i64);

/// Observes exact fixture tree names and bytes, with no following of symlinks.
fn tree(root: &Path) -> Vec<TreeRow> {
    /// Walks fixture-owned nodes deterministically without following symlinks.
    fn walk(root: &Path, path: &Path, rows: &mut Vec<TreeRow>) {
        let metadata = std::fs::symlink_metadata(path).unwrap();
        assert!(!metadata.file_type().is_symlink());
        rows.push((
            path.strip_prefix(root).unwrap().into(),
            if metadata.is_file() {
                std::fs::read(path).unwrap()
            } else {
                Vec::new()
            },
            metadata.ino(),
            metadata.mode(),
            metadata.mtime(),
            metadata.mtime_nsec(),
        ));
        if metadata.is_dir() {
            let mut children = std::fs::read_dir(path)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>();
            children.sort();
            for child in children {
                walk(root, &child, rows);
            }
        }
    }
    let mut rows = Vec::new();
    walk(root, root, &mut rows);
    rows
}

/// Runs the actual bare CLI under cleared environment, with no root/apply flags.
async fn bootstrap(helper: &Path, home: &Path, harness: &str, flags: &[&str]) -> serde_json::Value {
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new(helper)
            .args(["--json", "bootstrap", harness])
            .args(flags)
            .env_clear()
            .env("HOME", home)
            .env("PATH", "/usr/bin:/bin")
            .current_dir(home)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        output.status.success(),
        "{harness} bootstrap failed: {output:?}"
    );
    assert!(output.stderr.is_empty());
    serde_json::from_slice(&output.stdout).unwrap()
}

/// Qualifies absent-root preview, install, repeat and recognized missing-file
/// repair while retaining authored settings/exporters/sibling callbacks. Shared
/// Codex/Cursor whole-file repair is tested BEFORE adding shared authored members:
/// losing the whole file cannot honestly preserve callbacks that were deleted.
async fn install(matrix: &Matrix, harness: &str, helper: &Path) -> PathBuf {
    let home = matrix.directory.join(format!("home-{harness}"));
    std::fs::create_dir(&home).unwrap();
    let before = tree(&home);
    assert_eq!(
        bootstrap(helper, &home, harness, &["--dry-run"]).await["result"],
        "preview"
    );
    assert_eq!(
        tree(&home),
        before,
        "absent preview must not create private state"
    );
    let root = home.join(format!(".{harness}"));
    std::fs::create_dir(&root).unwrap();
    let settings = br#"{"statusLine":{"command":"author-status"},"otel":{"exporter":"author-exporter","content":false},"authored":true}"#;
    std::fs::write(root.join("settings.json"), settings).unwrap();
    std::fs::create_dir(root.join("author-hooks")).unwrap();
    std::fs::write(root.join("author-hooks/callback.json"), b"AUTHOR CALLBACK").unwrap();
    let installed = bootstrap(helper, &home, harness, &[]).await;
    assert_eq!(installed["result"], "installed");
    assert_eq!(installed["scope_root"], root.to_str().unwrap());
    assert_eq!(installed["runtime_verification"], "not-performed");
    match harness {
        "copilot" => assert_eq!(installed["usage"], "unavailable-safe-activation"),
        "cursor" => assert_eq!(installed["usage"], "unavailable-counter-schema"),
        _ => {}
    }
    assert_eq!(
        bootstrap(helper, &home, harness, &[]).await["result"],
        "unchanged"
    );
    let owned = root.join(match harness {
        "claude" => "skills/mezzanine-telemetry/hooks/register.mjs",
        "copilot" => "hooks/mezzanine.json",
        _ => "hooks.json",
    });
    let original = std::fs::read(&owned).unwrap();
    std::fs::remove_file(&owned).unwrap();
    assert_eq!(
        bootstrap(helper, &home, harness, &[]).await["result"],
        "repaired"
    );
    assert_eq!(std::fs::read(&owned).unwrap(), original);
    if harness == "codex" || harness == "cursor" {
        let mut document: serde_json::Value = serde_json::from_slice(&original).unwrap();
        let (event, member) = if harness == "codex" {
            (
                "SessionStart",
                serde_json::json!({"hooks":[{"type":"command","command":"printf '{}\\n'"}]}),
            )
        } else {
            (
                "sessionStart",
                serde_json::json!({"command":"printf '{}\\n'"}),
            )
        };
        document["hooks"][event]
            .as_array_mut()
            .unwrap()
            .insert(0, member);
        document["authored"] = true.into();
        std::fs::write(&owned, serde_json::to_vec(&document).unwrap()).unwrap();
    }
    let settled = tree(&home);
    assert_eq!(
        bootstrap(helper, &home, harness, &["--dry-run"]).await["result"],
        "preview"
    );
    assert_eq!(
        bootstrap(helper, &home, harness, &["--check"]).await["result"],
        "checked"
    );
    assert_eq!(
        bootstrap(helper, &home, harness, &[]).await["result"],
        "unchanged"
    );
    assert_eq!(
        tree(&home),
        settled,
        "authored callbacks and installed preview must remain inert"
    );
    assert_eq!(std::fs::read(root.join("settings.json")).unwrap(), settings);
    assert_eq!(
        std::fs::read(root.join("author-hooks/callback.json")).unwrap(),
        b"AUTHOR CALLBACK"
    );
    home
}

/// Launches the child normally under the actual pane root with only standard
/// nonsecret discovery and isolated HOME/PATH. No module/helper entry is passed.
async fn launch(
    matrix: &mut Matrix,
    harness: &'static str,
    pane: &'static str,
    home: &Path,
    node: &Path,
) -> Vendor {
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/installed-harness-matrix-fixture.mjs")
        .canonicalize()
        .unwrap();
    let quote = |path: &Path| {
        shlex::try_quote(path.to_str().unwrap())
            .unwrap()
            .into_owned()
    };
    let command = format!(
        "env -i HOME={} PATH=/usr/bin:/bin MEZ=\"$MEZ\" MEZ_PANE=\"$MEZ_PANE\" {} {} {harness} 2>{} &\n",
        quote(home),
        quote(node),
        quote(&script),
        quote(&home.join("fixture.stderr"))
    );
    matrix
        .service
        .write_runtime_pane_input(pane, command.as_bytes())
        .unwrap();
    let (mut socket, _) = tokio::time::timeout(Duration::from_secs(20), matrix.listener.accept())
        .await
        .unwrap()
        .unwrap();
    let origin = Arc::new(
        crate::runtime::capture_unix_origin(
            socket.as_raw_fd(),
            crate::runtime::current_effective_uid(),
        )
        .unwrap(),
    );
    let mut qualified = crate::runtime::UnixOriginStream::new(&mut socket, Some(origin.clone()));
    let mut ready = [255];
    tokio::time::timeout(Duration::from_secs(20), qualified.read_exact(&mut ready))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ready, [0]);
    assert!(origin.writer_confirmed());
    assert_ne!(origin.identity.process_id, std::process::id());
    drop(qualified);
    Vendor {
        harness,
        pane,
        socket,
        origin,
    }
}

/// Runs helpers until a child declares its next phase. Native ingress grants no
/// role and every accepted helper must prove its own current writer identity.
/// The retained service is extracted only after the child's callback completed.
async fn advance(matrix: &mut Matrix, vendor: &mut Vendor, phase: u8) {
    let service = std::mem::replace(&mut matrix.service, RuntimeServiceFixture::new().build());
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let task = tokio::spawn(actor.run());
    vendor.socket.write_all(&[1]).await.unwrap();
    let mut completed = [255];
    loop {
        tokio::select! {
            result = vendor.socket.read_exact(&mut completed) => {
                if let Err(error) = result {
                    handle.shutdown().await.unwrap();
                    matrix.service = task.await.unwrap().service;
                    let diagnostic = std::fs::read_to_string(matrix.directory.join(format!("home-{}/fixture.stderr", vendor.harness))).unwrap();
                    panic!("{} phase {phase}: {error}; {}", vendor.harness, &diagnostic[..diagnostic.len().min(8192)]);
                }
                assert_eq!(completed, [phase]); break;
            }
            result = matrix.listener.accept() => {
                let (mut socket, _) = result.unwrap();
                let uid = crate::runtime::current_effective_uid();
                let origin = Arc::new(crate::runtime::capture_unix_origin(socket.as_raw_fd(), uid).unwrap());
                let mut connection = ControlConnectionState::new(true, false);
                connection.bind_authenticated_peer(AuthenticatedPeer::unix_user(uid)).unwrap();
                connection.bind_unix_origin(origin.clone()).unwrap();
                let qualified = crate::runtime::UnixOriginStream::new(&mut socket, Some(origin.clone()));
                let mut audit = WireAudit { stream: qualified, received: Vec::new() };
                tokio::time::timeout(Duration::from_secs(15),
                    serve_authenticated_async_runtime_control_connection_loop_with_snapshots(&mut audit,
                        AuthenticatedPeer::unix_user(uid), &handle, &mut connection,
                        AsyncRuntimeControlConnectionConfig::new(8192, uid).unwrap(), None, |_, _| false))
                    .await.unwrap().unwrap();
                audit_wire(&audit.received);
                assert!(!connection.initialized());
                assert!(connection.caller_client_id().is_none());
                assert!(connection.unix_origin().unwrap().writer_confirmed());
            }
        }
    }
    handle.shutdown().await.unwrap();
    matrix.service = task.await.unwrap().service;
}

/// Drives the real attached browser, denying lifecycle keys for an installed
/// external row while Enter focuses its pane. Status is exercised both through
/// terminal agent-status without a native session and native Harness /status.
fn inspect_browser(service: &mut RuntimeSessionService, primary: &mez_core::ids::ClientId) {
    service.session.select_pane_global(primary, "%2").unwrap();
    let status: serde_json::Value = serde_json::from_str(
        &service
            .execute_terminal_command(primary, "agent-status")
            .unwrap(),
    )
    .unwrap();
    let body = status["outcomes"][0]["body"].as_str().unwrap();
    assert!(body.contains("### External Telemetry"), "{body}");
    assert!(service.agent_shell_store().get("%2").is_none());
    service.session.select_pane_global(primary, "%1").unwrap();
    let status = service
        .execute_agent_shell_command(primary, "/status")
        .unwrap();
    assert!(
        status.contains("unavailable-claude-usage-finalized-counters"),
        "{status}"
    );
    let target = service
        .external_agent_rows()
        .into_iter()
        .find(|row| row["harness"] == "cursor")
        .unwrap();
    service
        .execute_attached_display_command(primary, "list-agents")
        .unwrap();
    let browser = service.agent_management_browser(primary).unwrap().0;
    assert_eq!(
        browser.records().len(),
        5,
        "four external rows and one idle native harness"
    );
    let input = |service: &mut RuntimeSessionService, bytes: &[u8]| {
        service
            .apply_attached_terminal_step_plan(
                primary,
                &AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::ForwardToPane(bytes.to_vec())],
                    output_lines: Vec::new(),
                    output_line_style_spans: Vec::new(),
                    input_hangup: false,
                    output_hangup: false,
                    error_roles: Vec::new(),
                },
            )
            .unwrap();
    };
    input(
        service,
        format!("/{}\r", target["agent_id"].as_str().unwrap()).as_bytes(),
    );
    for key in [b"i".as_slice(), b"p".as_slice(), b"d".as_slice()] {
        input(service, key);
        assert_eq!(service.active_pane_id().unwrap().as_str(), "%1");
        assert_eq!(service.external_agent_rows().len(), 4);
        assert_eq!(service.agent_human_pause_status("%2"), None);
        assert!(service.find_pane_descriptor("%2").is_some());
    }
    input(service, b"\r");
    assert!(service.primary_display_overlay().is_none());
    assert_eq!(service.active_pane_id().unwrap().as_str(), "%2");
    service.session.select_pane_global(primary, "%1").unwrap();
}

/// One actual production install-to-native matrix holds four source-independent
/// creators on two panes with an idle native harness and pinned titles. Retry
/// cannot duplicate registration; all external rows are read-only and associated
/// with their actual pane/native parent. Claude's synthetic current request is
/// durably charged once and missing counters stay unknown. Unqualified classic
/// counters never create expense. Exact retirement removes only its own row,
/// retaining the other live providers and historical incurred Claude expense.
/// This qualifies existing paths, not a newly fixed behavior or live vendor.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit offline Node and freshly built sibling mez; installed-native supported subset"]
async fn external_installed_matrix_supported_subset_two_panes_four_creators() {
    for _ in 0..2 {
        qualify_matrix().await;
    }
}

/// Repeats the complete installed-source run with independently isolated HOME,
/// pane roots and creators, preventing a prior fixture's authority or ledger
/// from making a later install-to-native invocation pass accidentally.
async fn qualify_matrix() {
    let node = PathBuf::from(
        std::env::var_os("MEZ_TEST_NODE_BINARY").expect("explicit offline Node path"),
    );
    assert!(node.is_absolute() && node.is_file());
    let helper = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("mez");
    assert!(helper.is_file());
    let directory = Path::new("/tmp").join(format!(
        "mez-installed-matrix-{}-{:x}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("control.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    crate::runtime::enable_unix_writer_credentials(listener.as_raw_fd()).unwrap();
    let service = RuntimeServiceFixture::new().control_socket(&path).build();
    let mut matrix = Matrix {
        service,
        directory,
        listener,
    };
    let usage = matrix.directory.join("usage.sqlite");
    matrix
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&usage));
    let primary = matrix
        .service
        .attach_primary(
            "matrix-inspector",
            true,
            mez_mux::layout::Size::new(120, 40).unwrap(),
            120,
        )
        .unwrap();
    matrix.service.start_initial_pane_process(None).unwrap();
    matrix
        .service
        .split_pane_with_process(&primary, mez_mux::layout::SplitDirection::Vertical, None)
        .unwrap();
    matrix
        .service
        .session
        .select_pane_global(&primary, "%1")
        .unwrap();
    matrix
        .service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    for pane in ["%1", "%2"] {
        matrix
            .service
            .session
            .set_pane_title_explicit(pane, "Matrix pinned")
            .unwrap();
    }
    let mut vendors = Vec::new();
    for (harness, pane) in [
        ("claude", "%1"),
        ("codex", "%1"),
        ("copilot", "%2"),
        ("cursor", "%2"),
    ] {
        let home = install(&matrix, harness, &helper).await;
        vendors.push(launch(&mut matrix, harness, pane, &home, &node).await);
    }
    let clients = matrix.service.session().clients().len();
    for vendor in &mut vendors {
        advance(&mut matrix, vendor, 1).await;
    }
    let registry = matrix.service.control.external_agents();
    assert_eq!(
        registry.bindings.len(),
        4,
        "four real native admissions required"
    );
    assert_eq!(
        matrix.service.external_agent_rows().len(),
        4,
        "all held creators must remain live"
    );
    let owners = registry
        .bindings
        .values()
        .map(|binding| binding.accounting_owner.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(owners.len(), 4);
    for vendor in &vendors {
        let binding = registry
            .bindings
            .values()
            .find(|binding| binding.harness == vendor.harness)
            .unwrap();
        assert_eq!(binding.pane_id, vendor.pane);
        assert!(
            binding
                .enrollment
                .as_ref()
                .unwrap()
                .producer
                .matches_parent(vendor.origin.uid(), vendor.origin.identity)
        );
        let row = matrix
            .service
            .external_agent_rows()
            .into_iter()
            .find(|row| row["harness"] == vendor.harness)
            .unwrap();
        assert_eq!(row["controls"], serde_json::json!([]));
        assert_eq!(row["status"], "running");
        let status = matrix
            .service
            .runtime_agent_status_display_with_options(vendor.pane, false)
            .unwrap();
        assert!(status.contains("### External Telemetry"), "{status}");
    }
    assert!(matrix.service.agent_shell_store().get("%1").is_some());
    assert!(matrix.service.agent_shell_store().get("%2").is_none());
    for vendor in &mut vendors {
        advance(&mut matrix, vendor, 2).await;
    }
    for row in matrix.service.external_agent_rows() {
        assert_eq!(row["controls"], serde_json::json!([]));
        if row["harness"] == "claude" {
            // The child completed a real missing-counter callback after its
            // durably acknowledged expense; browser/status must expose both.
            assert_eq!(
                row["usage_coverage"],
                "unavailable-claude-usage-finalized-counters"
            );
        } else {
            assert_ne!(row["usage_coverage"], "request-bound-external-reported");
        }
    }
    let status = matrix
        .service
        .runtime_agent_status_display_with_options("%1", false)
        .unwrap();
    assert!(status.contains("response-model"), "{status}");
    assert!(
        status.contains("unavailable-claude-usage-finalized-counters"),
        "{status}"
    );
    for pane in ["%1", "%2"] {
        assert_eq!(
            matrix.service.session.pane_title_state(pane).unwrap(),
            (
                "Matrix pinned".into(),
                mez_mux::layout::PaneTitleSource::Explicit
            )
        );
    }
    inspect_browser(&mut matrix.service, &primary);
    assert!(
        matrix
            .service
            .terminal_frame_context()
            .panes
            .values()
            .all(|pane| pane.pane_title_override.is_none())
    );
    let database = rusqlite::Connection::open(&usage).unwrap();
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM token_usage_events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        database
            .query_row(
                "SELECT sum(input_tokens) FROM token_usage_events",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        16
    );
    for (index, vendor) in vendors.iter_mut().enumerate() {
        advance(&mut matrix, vendor, 3).await;
        let claude = vendor.harness == "claude";
        assert_eq!(
            matrix.service.external_agent_rows().len(),
            if claude { 4 } else { 3 - index }
        );
        let binding = matrix
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .find(|binding| binding.harness == vendor.harness)
            .unwrap();
        assert_eq!(binding.retired, !claude);
        vendor.socket.write_all(&[1]).await.unwrap();
        vendor.socket.shutdown().await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while vendor.origin.is_live() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        if claude {
            assert_eq!(matrix.service.reconcile_external_agent_registrations(), 1);
            assert_eq!(matrix.service.external_agent_rows().len(), 3);
        }
    }
    assert_eq!(matrix.service.session().clients().len(), clients);
    assert!(matrix.service.agent_shell_store().get("%1").is_some());
    assert!(
        matrix
            .service
            .control
            .external_agents()
            .enrollments
            .pending
            .is_empty()
    );
    assert_eq!(
        matrix.service.external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
    assert!(
        matrix
            .service
            .runtime_agent_status_display_with_options("%1", false)
            .unwrap()
            .contains("response-model")
    );
    let bytes = tree(&matrix.directory);
    assert!(bytes.iter().all(|(_, bytes, ..)| {
        !bytes
            .windows(b"PRIVATE_MATRIX_CONTENT".len())
            .any(|window| window == b"PRIVATE_MATRIX_CONTENT")
    }));
}
