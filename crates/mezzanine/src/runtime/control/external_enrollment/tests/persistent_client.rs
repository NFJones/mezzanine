//! Installed shared-client acceptance through an ordinary Node pane descendant.

use super::*;

/// Forwards an already kernel-qualified stream unchanged and classifies only
/// the fixed retired-source error. Private framed bytes remain transient and
/// are never stored in the shared diagnostic or printed.
struct RetiredReplySpy<S> {
    stream: S,
    pending: Vec<u8>,
    rejected: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl<S: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for RetiredReplySpy<S> {
    /// Delegates actual native sender collection without replacing any bytes.
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl<S: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for RetiredReplySpy<S> {
    /// Observes only bytes actually written and preserves partial-write/error
    /// results, so classification never manufactures a native acknowledgment.
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
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
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit installed Node path and built mez helper required; offline only"]
async fn external_enrollment_installed_request_accounting_has_durable_native_ack() {
    qualify_installed_node_entry(
        "request-accounting-ordinary-fixture.mjs",
        "persistent_client.mjs",
        1,
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
    use crate::integrations::bootstrap::installer::{Operation, plan};
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
    plan(&root, &manifest, Operation::Install)
        .unwrap()
        .apply()
        .unwrap();
    assert!(
        plan(&root, &manifest, Operation::Install)
            .unwrap()
            .changed_paths()
            .is_empty()
    );
    let mut service = RuntimeServiceFixture::new().control_socket(&path).build();
    let pi_usage = script_name == "pi-usage-ordinary-fixture.mjs";
    let request_accounting = script_name == "request-accounting-ordinary-fixture.mjs" || pi_usage;
    let pi_loss = script_name == "pi-loss-ordinary-fixture.mjs";
    let pi_retired = script_name == "pi-retired-source-ordinary-fixture.mjs";
    let retired_rejected = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    if request_accounting || pi_loss {
        service.set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(
            directory.join("usage.sqlite"),
        ));
    }
    service.start_initial_pane_process(None).unwrap();
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts")
        .join(script_name)
        .canonicalize()
        .unwrap();
    let command = format!(
        "{}{} {} {} {} {}\n",
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
        shlex::try_quote(harness.unwrap_or("pi")).unwrap()
    );
    service
        .write_runtime_pane_input("%1", command.as_bytes())
        .unwrap();
    let clients = service.session().clients().len();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
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
            tasks.spawn(async move {
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
                };
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
                assert!(!connection.initialized());
                assert!(connection.caller_client_id().is_none());
                assert!(connection.unix_origin().unwrap().writer_confirmed());
            });
        }
        while let Some(result) = tasks.join_next().await {
            result.unwrap();
        }
        handle.shutdown().await.unwrap();
    };
    let ((), mut exit) = tokio::time::timeout(
        Duration::from_secs(if pi_usage { 300 } else { 25 }),
        async { tokio::join!(server, actor.run()) },
    )
    .await
    .unwrap();
    assert_eq!(
        retired_rejected.load(std::sync::atomic::Ordering::SeqCst),
        pi_retired,
        "actual native retired-source conflict classification"
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
    if helper_callback {
        assert!(binding.enrollment.as_ref().unwrap().producer.is_live());
        assert_eq!(binding.enrollment.as_ref().unwrap().observers.len(), 1);
    }
    if let Some(harness) = harness {
        assert_eq!(binding.harness, harness);
    }
    assert_eq!(
        binding.enrollment.as_ref().unwrap().epoch,
        if typed_pi || opencode || helper_callback || request_accounting || pi_loss || pi_retired {
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
        if pi_usage {
            assert_eq!(
                database
                    .query_row("SELECT count(*) FROM external_usage_streams", [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                1
            );
            assert_eq!(
                database
                    .query_row("SELECT count(*) FROM external_usage_sources", [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                1
            );
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
    exit.service.terminate_all_pane_processes().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}
