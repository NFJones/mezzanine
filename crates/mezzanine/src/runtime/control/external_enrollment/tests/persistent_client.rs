//! Installed shared-client acceptance through an ordinary Node pane descendant.

use super::*;

/// The installed qualification wrapper must not inline an entire native actor
/// workflow in its caller's default-stack future. Measure the actual opaque
/// future type without constructing/polling it or spawning any native resource;
/// this frame budget is not a guarantee of whole-call-stack usage or OS support.
#[test]
fn external_enrollment_installed_fixture_future_is_bounded() {
    /// Reads only a function's concrete returned future layout, never calls it.
    fn bytes<F>(
        _: fn(&'static str, &'static str, usize, usize, bool, Option<&'static str>) -> F,
    ) -> usize {
        std::mem::size_of::<F>()
    }
    let size = bytes(qualify_installed_node_entry);
    eprintln!("installed qualification wrapper future bytes={size}");
    /// Measures the actor run state without constructing or polling an actor.
    fn actor_bytes<F>(_: fn(crate::host::async_runtime::AsyncRuntimeSessionActor) -> F) -> usize {
        std::mem::size_of::<F>()
    }
    let actor = actor_bytes(crate::host::async_runtime::AsyncRuntimeSessionActor::run);
    eprintln!("installed qualification actor-run future bytes={actor}");
    eprintln!(
        "installed qualification layouts work={} binding={} enrollment={} error={}",
        std::mem::size_of::<ExternalEnrollmentWork>(),
        std::mem::size_of::<LaunchBinding>(),
        std::mem::size_of::<EnrollmentBinding>(),
        std::mem::size_of::<MezError>()
    );
    assert!(
        size <= 112 * 1024,
        "installed qualification wrapper future exceeds 112KiB: {size}"
    );
}

/// Observes an already kernel-qualified stream and classifies the fixed retired
/// error. One explicit fault fixture can withhold the first complete committed
/// usage reply and close the connection before the client reads any ACK bytes.
/// Private framed bytes remain transient and are never printed or retained in
/// shared diagnostics; the loss witness is only an atomic Boolean.
struct RetiredReplySpy<S> {
    stream: S,
    pending: Vec<u8>,
    rejected: std::sync::Arc<std::sync::atomic::AtomicBool>,
    lose_commit: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    lose_end: bool,
    close_after_loss: bool,
}

impl<S: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for RetiredReplySpy<S> {
    /// Delegates native sender reads except for the armed postcommit EOF; no
    /// incoming request bytes or native sender evidence are manufactured.
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if self.close_after_loss {
            return std::task::Poll::Ready(Ok(()));
        }
        std::pin::Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl<S: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for RetiredReplySpy<S> {
    /// Except for the explicitly armed fault, observes only actual writes. The
    /// fault requires a whole encoded frame in this call; changed framing fails
    /// its mandatory loss assertion rather than silently claiming coverage.
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        if let Some(lost) = self.lose_commit.as_ref()
            && !lost.load(std::sync::atomic::Ordering::SeqCst)
            && let Ok((body, consumed)) = crate::control::decode_control_frame(buf, 8192)
            && consumed == buf.len()
            && let Ok(reply) = serde_json::from_str::<serde_json::Value>(&body)
            && reply["result"]["durable"] == true
            && ((self.lose_end
                && reply["result"]["protocol"] == "external-source-end/1"
                && reply["result"]["retired"] == true)
                || (!self.lose_end
                    && reply["result"]["accepted"] == true
                    && reply["result"]["applied"] == true
                    && reply["result"]["coverage"]["source"] == "external-reported"))
        {
            lost.store(true, std::sync::atomic::Ordering::SeqCst);
            self.close_after_loss = true;
            // The actor already committed. Consume the fixture writer's frame
            // without delivering it, then EOF the server read to close the real
            // socket. This models reply loss, never an unexecuted expense/end.
            return std::task::Poll::Ready(Ok(buf.len()));
        }
        let result = std::pin::Pin::new(&mut self.stream).poll_write(cx, buf);
        if let std::task::Poll::Ready(Ok(written)) = &result {
            if self.pending.len().saturating_add(*written) <= 8320 {
                self.pending.extend_from_slice(&buf[..*written]);
                if let Ok((body, consumed)) =
                    crate::control::decode_control_frame(&self.pending, 8192)
                {
                    if let Ok(reply) = serde_json::from_str::<serde_json::Value>(&body)
                        && reply["error"]["message"] == "external native source has retired"
                        && reply["error"]["code"] == -32006
                    {
                        self.rejected
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                    self.pending.drain(..consumed);
                }
            } else {
                self.pending.clear();
            }
        }
        result
    }

    /// Preserves native flush behavior.
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_flush(cx)
    }
    /// Preserves native shutdown behavior.
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

/// Installed revision-8 client and accounting sibling run as an ordinary Node
/// descendant, obtain genuine native request readiness, publish normalized
/// expense with typed durable ACK and retire without primary/observer rights.
/// A same-run epoch-two successor reopens the original readiness and tests
/// ledger replay/conflict with fresh local tickets, not predecessor ACK caches.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit installed Node path and built mez helper required; offline only"]
async fn external_enrollment_installed_request_accounting_has_durable_native_ack() {
    qualify_installed_node_entry(
        "request-accounting-ordinary-fixture.mjs",
        "persistent_client.mjs",
        2,
        1,
        false,
        None,
    )
    .await;
}

/// Drops the first actual durable usage ACK after actor/storage commit, before
/// any reply bytes reach the ordinary producer. Its same native process must
/// reconnect with the original source/ticket, receive applied=false and retire;
/// exact database counts below forbid recharging or silently skipping the fault.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit installed Node path and built mez helper required; offline only"]
async fn external_enrollment_installed_request_accounting_recovers_lost_commit_ack() {
    qualify_installed_node_entry(
        "request-accounting-lost-ack-fixture.mjs",
        "persistent_client.mjs",
        2,
        1,
        false,
        None,
    )
    .await;
}

/// The end worker commits/inspects the real original tombstone before the first
/// reply is lost. A fresh native connection from the SAME producer must recover
/// its exact end receipt without enrollment or telemetry revival. One source at
/// Retired3 below forbids treating a new activation/mutation as recovery.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit installed Node path and built mez helper required; offline only"]
async fn external_enrollment_installed_source_end_recovers_lost_commit_ack() {
    qualify_installed_node_entry(
        "source-end-lost-ack-ordinary-fixture.mjs",
        "persistent_client.mjs",
        2,
        1,
        false,
        None,
    )
    .await;
}

/// Explicit offline qualification requires an already-installed Node and built
/// mez helper. The ordinary producer loads installed compiled siblings, validates
/// daemon peer UID through the read-only helper, and itself sends enrollment,
/// presentation, reload and end over real kernel-qualified Unix actor ingress.
/// Handoff retains a positive original run, advances the observer epoch, rejects
/// publication from the still-connected old owner, and keeps the successor usable.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit MEZ_TEST_NODE_BINARY and locally built mez helper required"]
async fn external_enrollment_installed_persistent_node_client_owns_native_sender() {
    qualify_installed_node_entry(
        "persistent-client-ordinary-fixture.mjs",
        "persistent_client.mjs",
        2,
        1,
        false,
        None,
    )
    .await;
}

/// The actual installed Pi default factory activates ordinary callbacks with no
/// vendor observer descriptor, wrapper markers or injected session binding.
/// Reload keeps one run while new/resume/fork metadata select never-retired
/// targets; historical same-ID source continuity is not qualified here.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit MEZ_TEST_NODE_BINARY and locally built mez helper required"]
async fn external_enrollment_installed_ordinary_pi_entry_uses_daemon_reducer() {
    qualify_installed_node_entry(
        "pi-persistent-ordinary-fixture.mjs",
        "index.mjs",
        5,
        4,
        true,
        None,
    )
    .await;
}

/// An actual retired source cannot reactivate the same ID with a fresh observer
/// name. Its child remains failed/unavailable and sends no later request/fork or
/// expense fallback; native origin/writer proof and reply classification are real.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit MEZ_TEST_NODE_BINARY and freshly built mez helper required; offline only"]
async fn external_enrollment_installed_pi_retired_same_id_remains_unavailable() {
    qualify_installed_node_entry(
        "pi-retired-source-ordinary-fixture.mjs",
        "index.mjs",
        3,
        2,
        false,
        None,
    )
    .await;
}

/// A deliberate final child assertion failure after real retired-source denial
/// must fail parent qualification, not be mistaken for success on socket EOF.
/// The expected panic names only the process result, never private frames.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit MEZ_TEST_NODE_BINARY and freshly built mez helper required; offline only"]
#[should_panic(expected = "ordinary fixture child exited unsuccessfully: Some(1)")]
async fn external_enrollment_installed_pi_retired_child_assertion_failure_is_rejected() {
    qualify_installed_node_entry_result(
        "pi-retired-source-ordinary-fixture.mjs",
        "index.mjs",
        3,
        2,
        false,
        None,
        true,
    )
    .await;
}

/// The installed ordinary Pi producer sends finalized error/abort/success
/// expense through real native readiness and typed durable ACK, not parent
/// completion. More than 4096 messages retain one source/model/category stream;
/// early counters, tool totals and replay never inflate durable expense.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit MEZ_TEST_NODE_BINARY and freshly built mez helper required; offline only"]
async fn external_enrollment_installed_ordinary_pi_usage_has_bounded_durable_stream() {
    qualify_installed_node_entry(
        "pi-usage-ordinary-fixture.mjs",
        "index.mjs",
        1,
        1,
        false,
        None,
    )
    .await;
}

/// Real installed callbacks publish missing-finalized-counter diagnostics before
/// new-session handoff and quit retirement. Original readiness records remain,
/// but no observation/stream/delta or native control authority is fabricated.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit MEZ_TEST_NODE_BINARY and freshly built mez helper required; offline only"]
async fn external_enrollment_installed_ordinary_pi_loss_precedes_retirement_without_expense() {
    qualify_installed_node_entry(
        "pi-loss-ordinary-fixture.mjs",
        "index.mjs",
        2,
        2,
        false,
        None,
    )
    .await;
}

/// Runs an explicit offline installed-artifact producer through real native
/// The installed TUI config resolves the real default {id,tui} module. A local
/// client-selected cache/route drives native Bun-origin enrollment, exact session
/// filtering and retirement; no server-owned pane hints or provider work occur.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit MEZ_TEST_NODE_BINARY (Bun host) and built mez helper required"]
async fn external_enrollment_installed_opencode_tui_uses_client_local_association() {
    qualify_installed_node_entry(
        "opencode-tui-ordinary-fixture.mjs",
        "opencode_tui.mjs",
        2,
        2,
        false,
        None,
    )
    .await;
}

/// Runs an explicit offline installed-artifact producer through real native
/// sender, peer helper, protocol actor and lifecycle settlement ownership.
/// Shared-client label qualification is not a vendor installation/loader claim.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit MEZ_TEST_NODE_BINARY and locally built mez helper required"]
async fn external_enrollment_installed_shared_client_supports_six_canonical_labels() {
    for harness in ["claude", "codex", "copilot", "opencode", "cursor", "pi"] {
        qualify_installed_node_entry(
            "persistent-client-ordinary-fixture.mjs",
            "persistent_client.mjs",
            2,
            1,
            false,
            Some(harness),
        )
        .await;
    }
}

/// An installed producer captures a public packet and spawns the built fixed
/// helper itself. Only helper-observe can create presentation; the actor checks
/// it while the producer survives helper EOF and a disconnected observer.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit MEZ_TEST_NODE_BINARY and freshly built mez helper required"]
async fn external_enrollment_installed_public_snapshots_reach_native_child_helper() {
    for harness in ["claude", "codex", "copilot", "opencode", "cursor", "pi"] {
        qualify_installed_node_entry(
            "persistent-helper-ordinary-fixture.mjs",
            "persistent_client.mjs",
            2,
            1,
            false,
            Some(harness),
        )
        .await;
    }
}

/// Qualifies actual installed shared bytes, native process ownership and exact
/// observer replacement, without impersonating six vendor loader integrations.
async fn qualify_installed_node_entry(
    script_name: &str,
    entry_name: &str,
    connections: usize,
    bindings: usize,
    typed_pi: bool,
    harness: Option<&str>,
) {
    qualify_installed_node_entry_result(
        script_name,
        entry_name,
        connections,
        bindings,
        typed_pi,
        harness,
        false,
    )
    .await;
}

/// Reads only a bounded numeric shell-published result for up to five seconds.
/// Missing, malformed or oversized completion is unavailable, never success.
async fn installed_fixture_child_status(path: &std::path::Path) -> Option<u8> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(metadata) = tokio::fs::metadata(path).await {
                if metadata.len() > 4 {
                    return None;
                }
                return tokio::fs::read_to_string(path)
                    .await
                    .ok()
                    .and_then(|result| result.trim().parse::<u8>().ok());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .ok()
    .flatten()
}

/// Runs native qualification with an optional deliberately failing child tail.
/// Child exit evidence is a separate test result, never admission authority.
async fn qualify_installed_node_entry_result(
    script_name: &str,
    entry_name: &str,
    connections: usize,
    bindings: usize,
    typed_pi: bool,
    harness: Option<&str>,
    fail_child: bool,
) {
    /// Removes only the unique offline install/socket fixture on every exit.
    struct Directory(std::path::PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    use crate::host::async_runtime::{
        AsyncRuntimeActorConfig, AsyncRuntimeControlConnectionConfig, AsyncRuntimeSessionActor,
        serve_authenticated_async_runtime_control_connection_loop_with_snapshots,
    };
    use crate::integrations::bootstrap::installer::{Operation, plan, plan_private};
    let node = std::path::PathBuf::from(
        std::env::var_os("MEZ_TEST_NODE_BINARY").expect("explicit installed Node path"),
    );
    assert!(node.is_absolute() && node.is_file());
    let executable = std::env::current_exe().unwrap();
    let helper = executable.parent().unwrap().parent().unwrap().join("mez");
    assert!(
        helper.is_file(),
        "build mez helper before explicit qualification"
    );
    let directory = std::path::Path::new("/tmp").join(format!(
        "mez-node-enroll-{}-{:x}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir(&directory).unwrap();
    let _directory = Directory(directory.clone());
    std::fs::set_permissions(
        &directory,
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    let path = directory.join("control.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    crate::runtime::enable_unix_writer_credentials(listener.as_raw_fd()).unwrap();
    let opencode = entry_name == "opencode_tui.mjs";
    let helper_callback = script_name == "persistent-helper-ordinary-fixture.mjs";
    let root = directory.join(if opencode {
        "opencode-config"
    } else {
        "pi-config"
    });
    std::fs::create_dir(&root).unwrap();
    let mut manifest = if opencode {
        crate::integrations::bootstrap::opencode_artifact::manifest()
    } else {
        crate::integrations::bootstrap::pi_artifact::candidate_manifest()
    };
    // This fixture selects the already-built product as the installing executable
    // instead of libtest itself; no runtime admission evidence is supplied here.
    let entry = manifest
        .entries
        .iter_mut()
        .find(|entry| entry.path.ends_with("/peer_helper.mjs"))
        .unwrap();
    entry.artifact = crate::integrations::bootstrap::reconciliation::Artifact::File {
        bytes: format!(
            "export const peerHelper = {};\n",
            serde_json::json!(helper.to_str().unwrap())
        )
        .into_bytes(),
    };
    // Current OpenCode owns multiple config arrays and requires production
    // private publication. The unique fixture directory is its isolated HOME.
    let install = || {
        if opencode {
            plan_private(&root, &directory, &manifest, Operation::Install)
        } else {
            plan(&root, &manifest, Operation::Install)
        }
    };
    install().unwrap().apply().unwrap();
    assert!(install().unwrap().changed_paths().is_empty());
    eprintln!("installed qualification stage=installed");
    let mut service = RuntimeServiceFixture::new().control_socket(&path).build();
    eprintln!("installed qualification stage=service-built");
    let pi_usage = script_name == "pi-usage-ordinary-fixture.mjs";
    let request_handoff = script_name == "request-accounting-ordinary-fixture.mjs";
    let request_ack_loss = script_name == "request-accounting-lost-ack-fixture.mjs";
    let request_accounting = request_handoff || request_ack_loss || pi_usage;
    let end_ack_loss = script_name == "source-end-lost-ack-ordinary-fixture.mjs";
    let source_end = script_name == "persistent-client-ordinary-fixture.mjs" || end_ack_loss;
    let pi_source_end = script_name == "pi-persistent-ordinary-fixture.mjs";
    let pi_loss = script_name == "pi-loss-ordinary-fixture.mjs";
    let pi_retired = script_name == "pi-retired-source-ordinary-fixture.mjs";
    let retired_rejected = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let committed_reply_lost = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    if request_accounting || pi_loss || source_end || pi_source_end {
        service.set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(
            directory.join("usage.sqlite"),
        ));
    }
    service.start_initial_pane_process(None).unwrap();
    eprintln!("installed qualification stage=pane-started");
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts")
        .join(script_name)
        .canonicalize()
        .unwrap();
    // The pane shell publishes only Node's actual numeric exit status, atomically
    // after process completion. This test-only result is not native RPC evidence.
    let child_result = directory.join("child-result");
    let child_result_pending = directory.join("child-result-pending");
    let eof_stage = directory.join("native-eof-stage");
    let command = format!(
        "({}{} {} {} {} {} {} {}); result=$?; printf '%s\\n' \"$result\" > {}; mv {} {}\n",
        if opencode { "BUN_BE_BUN=1 " } else { "" },
        shlex::try_quote(node.to_str().unwrap()).unwrap(),
        shlex::try_quote(script.to_str().unwrap()).unwrap(),
        shlex::try_quote(
            (if opencode {
                root.join("tui.json")
            } else {
                root.join("extensions/mezzanine").join(entry_name)
            })
            .to_str()
            .unwrap()
        )
        .unwrap(),
        shlex::try_quote(
            root.join(if opencode {
                "plugins/mezzanine/peer_helper.mjs"
            } else {
                "extensions/mezzanine/peer_helper.mjs"
            })
            .to_str()
            .unwrap()
        )
        .unwrap(),
        shlex::try_quote(harness.unwrap_or("pi")).unwrap(),
        if fail_child {
            "assertion-failure"
        } else {
            "success"
        },
        shlex::try_quote(eof_stage.to_str().unwrap()).unwrap(),
        shlex::try_quote(child_result_pending.to_str().unwrap()).unwrap(),
        shlex::try_quote(child_result_pending.to_str().unwrap()).unwrap(),
        shlex::try_quote(child_result.to_str().unwrap()).unwrap(),
    );
    service
        .write_runtime_pane_input("%1", command.as_bytes())
        .unwrap();
    let clients = service.session().clients().len();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    eprintln!("installed qualification stage=actor-constructed");
    // A single owned task gives actor polling its own heap-backed task boundary,
    // rather than nesting it under install/server/timeout/join poll frames. This
    // keeps the production current-thread stack and concurrency unchanged. The
    // JoinSet aborts on unwind; normal completion still requires shutdown+join.
    let mut actor_task = tokio::task::JoinSet::new();
    actor_task.spawn(actor.run());
    let server = async {
        let mut tasks = tokio::task::JoinSet::new();
        let mut producer = None;
        for ordinal in 0..connections {
            let (mut stream, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
                .await
                .unwrap_or_else(|_| {
                    panic!(
                        "ordinary native fixture stopped before connection {ordinal}/{connections}"
                    )
                })
                .unwrap();
            let origin = crate::runtime::capture_unix_origin(
                stream.as_raw_fd(),
                crate::runtime::current_effective_uid(),
            )
            .unwrap();
            assert_ne!(origin.identity.process_id, std::process::id());
            let identity =
                mez_mux::process::process_executable_identity_for_pid(origin.identity.process_id)
                    .unwrap();
            assert_eq!(
                identity.executable_path,
                if helper_callback && ordinal == 1 {
                    helper.canonicalize().unwrap()
                } else {
                    node.canonicalize().unwrap()
                },
                "peer helper supplied sender identity instead of Node"
            );
            if let Some(pid) = producer {
                if helper_callback && ordinal == 1 {
                    assert_ne!(origin.identity.process_id, pid);
                    assert_eq!(origin.identity.parent_process_id, pid);
                } else {
                    assert_eq!(origin.identity.process_id, pid);
                }
            } else {
                producer = Some(origin.identity.process_id);
            }
            let handle = handle.clone();
            let retired_rejected = retired_rejected.clone();
            let lose_commit =
                (request_ack_loss || end_ack_loss).then(|| committed_reply_lost.clone());
            tasks.spawn(async move {
                eprintln!("installed qualification stage=connection-started");
                let mut connection = ControlConnectionState::new(true, false);
                connection
                    .bind_authenticated_peer(AuthenticatedPeer::unix_user(origin.uid()))
                    .unwrap();
                connection
                    .bind_unix_origin(std::sync::Arc::new(origin))
                    .unwrap();
                let qualified = crate::runtime::UnixOriginStream::new(
                    &mut stream,
                    connection.unix_origin().cloned(),
                );
                let mut observed = RetiredReplySpy {
                    stream: qualified,
                    pending: Vec::new(),
                    rejected: retired_rejected,
                    lose_commit,
                    lose_end: end_ack_loss,
                    close_after_loss: false,
                };
                eprintln!("installed qualification stage=connection-serving");
                serve_authenticated_async_runtime_control_connection_loop_with_snapshots(
                    &mut observed,
                    AuthenticatedPeer::unix_user(crate::runtime::current_effective_uid()),
                    &handle,
                    &mut connection,
                    AsyncRuntimeControlConnectionConfig::new(
                        8192,
                        crate::runtime::current_effective_uid(),
                    )
                    .unwrap(),
                    None,
                    |_, _| false,
                )
                .await
                .unwrap();
                eprintln!("installed qualification stage=connection-served");
                assert!(!connection.initialized());
                assert!(connection.caller_client_id().is_none());
                assert!(connection.unix_origin().unwrap().writer_confirmed());
            });
        }
        while let Some(result) = tasks.join_next().await {
            result.unwrap();
        }
        // A test-only barrier proves the injected assertion occurs after the
        // parent observed every native EOF, not merely after client detach.
        if fail_child {
            tokio::fs::write(&eof_stage, b"1").await.unwrap();
        }
        // The public-helper fixture keeps its producer alive until the stopped
        // actor's live-owner assertions below. Its exit is required afterward.
        let child_status = if helper_callback {
            None
        } else {
            installed_fixture_child_status(&child_result).await
        };
        let reached_final_assertion =
            !fail_child || tokio::fs::read_to_string(&eof_stage).await.ok().as_deref() == Some("2");
        handle.shutdown().await.unwrap();
        (child_status, reached_final_assertion)
    };
    let ((child_status, reached_final_assertion), mut exit) = tokio::time::timeout(
        Duration::from_secs(if pi_usage { 300 } else { 25 }),
        async {
            tokio::join!(server, async {
                actor_task.join_next().await.unwrap().unwrap()
            })
        },
    )
    .await
    .unwrap();
    if !reached_final_assertion {
        exit.service.terminate_all_pane_processes().unwrap();
        panic!("ordinary fixture did not reach the intentional post-EOF assertion");
    }
    if !helper_callback && child_status != Some(0) {
        exit.service.terminate_all_pane_processes().unwrap();
        panic!("ordinary fixture child exited unsuccessfully: {child_status:?}");
    }
    assert_eq!(
        retired_rejected.load(std::sync::atomic::Ordering::SeqCst),
        pi_retired,
        "actual native retired-source conflict classification"
    );
    assert_eq!(
        committed_reply_lost.load(std::sync::atomic::Ordering::SeqCst),
        request_ack_loss || end_ack_loss,
        "fixture must actually withhold one committed native usage/end reply"
    );
    if pi_retired {
        assert!(
            !directory.join("usage.sqlite").exists(),
            "no-store lifecycle fixture"
        );
        assert!(
            exit.service
                .control
                .external_agents()
                .bindings
                .values()
                .all(|binding| {
                    binding.pi_lifecycle.is_some()
                        && !binding.enrollment.as_ref().unwrap().request_reports_seen
                })
        );
        assert_eq!(
            exit.service.external_usage_reservation_counts_for_tests(),
            (0, 0)
        );
        assert_eq!(
            exit.service
                .external_token_usage(None)
                .values()
                .map(|usage| usage.input_tokens)
                .sum::<u64>(),
            0
        );
    }
    assert_eq!(exit.service.session().clients().len(), clients);
    assert_eq!(
        exit.service.control.external_agents().bindings.len(),
        bindings
    );
    assert!(
        exit.service
            .control
            .external_agents()
            .bindings
            .values()
            .all(|binding| binding.retired != helper_callback)
    );
    let binding = exit
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .max_by_key(|binding| binding.generation)
        .unwrap();
    assert_eq!(binding.retired, !helper_callback);
    if source_end || pi_source_end {
        // The installed client required a real exact receipt before exiting.
        // No persistence-service drain is used to substitute for its end worker.
        let database = rusqlite::Connection::open(directory.join("usage.sqlite")).unwrap();
        let receipt: (i64, i64, i64) = database
            .query_row(
                "SELECT count(*), min(state), min(revision) FROM external_usage_source_activations",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(receipt, (if pi_source_end { 4 } else { 1 }, 2, 3));
    }
    if helper_callback {
        assert!(binding.enrollment.as_ref().unwrap().producer.is_live());
        assert_eq!(binding.enrollment.as_ref().unwrap().observers.len(), 1);
    }
    if let Some(harness) = harness {
        assert_eq!(binding.harness, harness);
    }
    assert_eq!(
        binding.enrollment.as_ref().unwrap().epoch,
        if request_handoff {
            2
        } else if typed_pi
            || opencode
            || helper_callback
            || request_accounting
            || pi_loss
            || pi_retired
            || end_ack_loss
        {
            1
        } else {
            2
        }
    );
    if pi_loss {
        assert!(
            exit.service
                .control
                .external_agents()
                .bindings
                .values()
                .all(|binding| {
                    binding
                        .pi_lifecycle
                        .as_ref()
                        .and_then(|projection| projection.usage_gap_coverage())
                        == Some("unavailable-pi-usage-finalized-counters")
                        && !binding.enrollment.as_ref().unwrap().request_reports_seen
                })
        );
        assert_eq!(
            exit.service
                .external_token_usage(None)
                .values()
                .map(|usage| usage.input_tokens)
                .sum::<u64>(),
            0
        );
        let database = rusqlite::Connection::open(directory.join("usage.sqlite")).unwrap();
        assert_eq!(
            database
                .query_row("SELECT count(*) FROM external_usage_requests", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            2
        );
        for table in [
            "external_usage_observations",
            "external_usage_streams",
            "token_usage_events",
        ] {
            assert_eq!(
                database
                    .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
    }
    if request_accounting {
        let expected_requests = if pi_usage { 4105_u64 } else { 1 };
        let expected_input = expected_requests * 10;
        assert!(binding.enrollment.as_ref().unwrap().request_reports_seen);
        assert_eq!(
            exit.service
                .external_token_usage(None)
                .values()
                .map(|usage| usage.input_tokens)
                .sum::<u64>(),
            expected_input
        );
        assert_eq!(
            exit.service.external_usage_reservation_counts_for_tests(),
            (0, 0)
        );
        let database = rusqlite::Connection::open(directory.join("usage.sqlite")).unwrap();
        assert_eq!(
            database
                .query_row(
                    "SELECT sum(input_tokens) FROM token_usage_events",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            expected_input as i64
        );
        assert_eq!(
            database
                .query_row("SELECT count(*) FROM external_usage_requests", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            expected_requests as i64
        );
        assert_eq!(
            database
                .query_row(
                    "SELECT count(*) FROM external_usage_observations",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            expected_requests as i64
        );
        for table in ["external_usage_streams", "external_usage_sources"] {
            assert_eq!(
                database
                    .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                1
            );
        }
        assert_eq!(
            database
                .query_row("SELECT count(*) FROM token_usage_events", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            expected_requests as i64
        );
        if pi_usage {
            assert!(exit.service.agent_shell_store().get("%1").is_none());
            let status = exit.service.runtime_agent_status_display("%1").unwrap();
            assert!(status.contains("pi-offline-provider"));
            assert!(status.contains("actual-model"));
            assert!(!status.contains("selected-model"));
            assert!(!status.contains("PRIVATE_CALLBACK_CONTENT"));
            let reopened =
                crate::storage::token_usage::TokenUsageStore::new(directory.join("usage.sqlite"));
            let snapshot = reopened
                .history_snapshot(
                    crate::runtime::current_unix_seconds(),
                    &[1],
                    &Default::default(),
                )
                .unwrap();
            assert_eq!(
                snapshot.windows[&1]
                    .values()
                    .map(|usage| usage.usage.input_tokens)
                    .sum::<u64>(),
                expected_input
            );
        }
    }
    if typed_pi {
        assert!(
            exit.service
                .control
                .external_agents()
                .bindings
                .values()
                .all(|binding| binding.pi_lifecycle.is_some())
        );
        assert!(
            exit.service
                .control
                .external_agents()
                .bindings
                .values()
                .any(|binding| binding.enrollment.as_ref().unwrap().epoch == 2)
        );
    }
    assert_eq!(
        binding
            .registration
            .as_ref()
            .unwrap()
            .presentation
            .as_ref()
            .unwrap()
            .state,
        // Missing final counters do not manufacture an agent_settled fact.
        if opencode || helper_callback || pi_loss || pi_retired {
            "running"
        } else {
            "complete"
        }
    );
    assert!(
        exit.service
            .control
            .external_agents()
            .enrollments
            .pending
            .is_empty()
    );
    if helper_callback {
        // Release the deliberately live producer only after every actor/native
        // assertion, then require its actual final process success as well.
        tokio::fs::write(&eof_stage, b"1").await.unwrap();
        let child_status = installed_fixture_child_status(&child_result).await;
        if child_status != Some(0) {
            exit.service.terminate_all_pane_processes().unwrap();
            panic!("ordinary fixture child exited unsuccessfully: {child_status:?}");
        }
    }
    exit.service.terminate_all_pane_processes().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}
