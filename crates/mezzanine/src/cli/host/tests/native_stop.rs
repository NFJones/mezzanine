//! Public Stop completion versus real retained native ownership.
//!
//! These fixtures own every spawned Child and keep PTY masters alive so socket
//! removal or accidental hangup cannot substitute for termination and reap.
//! Product outcomes are captured before independent fixture cleanup, and tests
//! use ordinary Tokio/libtest stacks rather than the older host fixture's stack.

use super::*;

/// Admission-only/missing/false replies, EOF and a stalled original connection
/// must all remain unproven even when the private socket is actually removed.
/// This protocol-only fixture owns no child and cannot manufacture a native
/// receipt. The real native counterpart below supplies the kernel oracle.
#[tokio::test(flavor = "current_thread")]
async fn public_host_stop_rejects_incomplete_or_lost_receipt() {
    for mode in ["admission", "false", "missing", "eof", "stall"] {
        let root = test_root("stop-receipt");
        let env = test_env(&root);
        let runtime_root = default_socket_directory(&env.runtime).unwrap().path;
        fs::create_dir_all(&runtime_root).unwrap();
        fs::set_permissions(&runtime_root, fs::Permissions::from_mode(0o700)).unwrap();
        let socket = host_socket_path(&runtime_root).unwrap();
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let server = async {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut buffer = [0; 4096];
                let read = stream.read(&mut buffer).await.unwrap();
                if read == 0 {
                    break;
                }
                bytes.extend_from_slice(&buffer[..read]);
                if crate::control::decode_control_frame(&bytes, HOST_RESPONSE_LIMIT).is_ok() {
                    break;
                }
            }
            let result = match mode {
                "admission" => Some(serde_json::json!({"shutting_down":true})),
                "false" => Some(serde_json::json!({"shutdown_completed":false})),
                "missing" => Some(serde_json::json!({})),
                "stall" => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    None
                }
                _ => None,
            };
            if let Some(result) = result {
                let body = serde_json::json!({"jsonrpc":"2.0","id":"host-cli","result":result});
                stream
                    .write_all(&crate::control::encode_control_body(&body.to_string()))
                    .await
                    .unwrap();
            }
            drop(stream);
            drop(listener);
            fs::remove_file(&socket).unwrap();
        };
        let stop = stop_host_with_completion(&env, Duration::from_millis(50));
        let (result, ()) = tokio::join!(stop, server);
        assert!(
            result.is_err(),
            "{mode} was mistaken for completion: {result:?}"
        );
        assert!(!socket.exists());
        fs::remove_dir_all(root).unwrap();
    }
}

/// A sticky actual-owner failure is independent of native cardinality. The
/// authenticated public completion response must reject a failed host at zero,
/// and unlinking its management socket must not turn that verdict into success.
#[tokio::test(flavor = "current_thread")]
async fn public_host_stop_rejects_retained_failure_at_native_zero() {
    let root = test_root("stop-failed-zero");
    let env = test_env(&root);
    let server = native_stop_host(&env);
    let socket = server.socket_path().to_path_buf();
    let owner = server.router().native_cleanup_owner();
    owner.retain_native_failure_for_test();
    assert_eq!(owner.native_pending_count().unwrap(), 0);
    let serving = tokio::spawn(async move {
        let result = server.serve(std::future::pending()).await;
        drop(server);
        result
    });
    let mut output = Vec::new();
    let mut command_native = super::super::super::native_exit::CliNativeOwnership::default();
    let stopped = run_host(
        HostCliArgs {
            command: HostCliCommand::Stop { timeout: 5 },
        },
        env,
        CliOutputFormat::Json,
        &mut output,
        &mut command_native,
    )
    .await;
    let server_result = serving.await.unwrap();
    assert!(stopped.is_err(), "zero masked failure: {stopped:?}");
    assert!(server_result.is_err());
    assert!(!socket.exists());
    assert_eq!(owner.native_pending_count().unwrap(), 0);
    fs::remove_dir_all(root).unwrap();
}

/// Creates one private local host with the ordinary finite shutdown budget.
fn native_stop_host(env: &CliEnv) -> HostServer {
    HostServer::bind(HostServerConfig {
        runtime_root: default_socket_directory(&env.runtime).unwrap().path,
        owner_uid: env.runtime.uid,
        config_root: env.config_paths().unwrap().root().to_path_buf(),
        config_layers: vec![ConfigLayer {
            name: "native-stop".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: DEFAULT_CONFIG_TOML.to_string(),
        }],
        shell: ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        max_sessions: 8,
        max_live_sessions: 4,
        shutdown_timeout: Duration::from_secs(2),
        checkpoint_interval: Duration::from_secs(300),
        default_session_policy: crate::host::router::HostDefaultSessionPolicy::MostRecentAttachable,
        default_lease_lifetime_seconds: 0,
        failed_lease_retention_seconds: 604_800,
        released_lease_retention_seconds: 604_800,
        iroh_invitation_issuer: None,
        max_remote_leases: 8,
        audit_log: None,
    })
    .unwrap()
}

/// Successful public Stop must already have reaped the exact retained root;
/// an unrelated fixture-owned native root must survive unchanged. In the
/// negative case the same host has a real Child still leased to this fixture,
/// so bounded shutdown cannot prove retirement. Even after the actual server
/// returns error and unlinks its socket, Stop must remain an error and the
/// original child must still be alive. Only then is the exact fixture returned
/// and explicitly retired; that later cleanup cannot rewrite the product result.
#[tokio::test(flavor = "current_thread")]
async fn public_host_stop_requires_native_completion_not_socket_removal() {
    for pending in [false, true] {
        let root = test_root("native-stop");
        let env = test_env(&root);
        let server = native_stop_host(&env);
        let socket = server.socket_path().to_path_buf();
        let router = server.router();
        let serving = tokio::spawn(async move {
            let result = server.serve(std::future::pending()).await;
            drop(server);
            result
        });
        let created = request_host(
            &env,
            "host/session/create",
            serde_json::Value::Object(local_host_launch_context(&env).unwrap()),
        )
        .await
        .unwrap();
        let runtime = router
            .runtime_for_tests(created["session_id"].as_str().unwrap())
            .unwrap();
        let native = runtime.native_retirement_owner();
        let process = mez_mux::process::spawn_retained_argv_pty_process(
            Path::new("/bin/cat"),
            &[],
            &[],
            mez_mux::layout::Size::new(80, 24).unwrap(),
            None,
            native.reserve().unwrap(),
        )
        .unwrap();
        let pid = process.primary_pid();
        let exact = rustix::process::Pid::from_raw(i32::try_from(pid).unwrap()).unwrap();
        let start = mez_mux::process::process_start_token_for_pid(pid);
        let master = process.duplicate_master_fd().unwrap();
        let held = if pending {
            Some(process)
        } else {
            drop(process);
            None
        };

        let mut other = mez_mux::process::spawn_argv_pty_process(
            Path::new("/bin/cat"),
            &[],
            &[],
            mez_mux::layout::Size::new(80, 24).unwrap(),
            None,
        )
        .unwrap();
        let other_pid = other.primary_pid();
        let other_exact =
            rustix::process::Pid::from_raw(i32::try_from(other_pid).unwrap()).unwrap();
        let other_start = mez_mux::process::process_start_token_for_pid(other_pid);
        let mut output = Vec::new();
        let mut command_native = super::super::super::native_exit::CliNativeOwnership::default();
        let stopped = run_host(
            HostCliArgs {
                command: HostCliCommand::Stop { timeout: 5 },
            },
            env,
            CliOutputFormat::Json,
            &mut output,
            &mut command_native,
        )
        .await;
        // No await may supply later completion before the public Stop oracle.
        let original_after_stop = rustix::process::test_kill_process(exact);
        let original_start_after_stop = mez_mux::process::process_start_token_for_pid(pid);
        let pending_after_stop = native.outstanding();
        let other_after_stop = rustix::process::test_kill_process(other_exact);
        let other_start_after_stop = mez_mux::process::process_start_token_for_pid(other_pid);
        let server_result = serving.await.unwrap();
        let socket_removed = !socket.exists();

        // Independent exact-owner cleanup runs only after product observations.
        // A retained failure receipt may keep this result Err after actual reap.
        if pending {
            drop(held);
            native.seal();
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            let attempt = native
                .prepare_explicit_cleanup_retry(deadline)
                .unwrap()
                .admit()
                .unwrap();
            let _ = crate::host::async_runtime::drain_native_pane_retirement_for_attempt(
                &native,
                &attempt,
                tokio::time::Instant::from_std(deadline),
            )
            .await;
        }
        other.terminate(Duration::from_millis(10)).unwrap();
        assert_eq!(
            native.outstanding(),
            0,
            "fixture native cleanup remains retained"
        );
        assert_eq!(
            rustix::process::test_kill_process(exact),
            Err(rustix::io::Errno::SRCH)
        );
        drop(master);

        assert_eq!(
            stopped.is_err(),
            pending,
            "public Stop verdict: {stopped:?}"
        );
        assert_eq!(
            server_result.is_err(),
            pending,
            "server verdict: {server_result:?}"
        );
        assert!(socket_removed, "actual host socket was not removed");
        assert_eq!(other_after_stop, Ok(()));
        assert_eq!(other_start_after_stop, other_start);
        if pending {
            assert_eq!(original_after_stop, Ok(()));
            assert_eq!(original_start_after_stop, start);
            assert!(pending_after_stop > 0);
        } else {
            assert_eq!(original_after_stop, Err(rustix::io::Errno::SRCH));
            assert_eq!(pending_after_stop, 0);
        }
        fs::remove_dir_all(root).unwrap();
    }
}
