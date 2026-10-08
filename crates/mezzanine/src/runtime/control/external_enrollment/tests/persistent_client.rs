//! Installed shared-client acceptance through an ordinary Node pane descendant.

use super::*;

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
/// Reload keeps one run while new/resume/fork create exact fresh registrations.
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
        serve_async_runtime_control_connection_loop,
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
                .unwrap()
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
            tasks.spawn(async move {
                let mut connection = ControlConnectionState::new(true, false);
                serve_async_runtime_control_connection_loop(
                    &mut stream,
                    &handle,
                    &mut connection,
                    AsyncRuntimeControlConnectionConfig::new(
                        8192,
                        crate::runtime::current_effective_uid(),
                    )
                    .unwrap(),
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
    let ((), mut exit) = tokio::time::timeout(Duration::from_secs(25), async {
        tokio::join!(server, actor.run())
    })
    .await
    .unwrap();
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
        if typed_pi || opencode || helper_callback {
            1
        } else {
            2
        }
    );
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
        if opencode || helper_callback {
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
