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
    )
    .await;
}

/// Runs an explicit offline installed-artifact producer through real native
/// sender, peer helper, protocol actor and lifecycle settlement ownership.
async fn qualify_installed_node_entry(
    script_name: &str,
    entry_name: &str,
    connections: usize,
    bindings: usize,
    typed_pi: bool,
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
    let root = directory.join("pi-config");
    std::fs::create_dir(&root).unwrap();
    let mut manifest = crate::integrations::bootstrap::pi_artifact::candidate_manifest();
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
        "{} {} {} {}\n",
        shlex::try_quote(node.to_str().unwrap()).unwrap(),
        shlex::try_quote(script.to_str().unwrap()).unwrap(),
        shlex::try_quote(
            root.join("extensions/mezzanine")
                .join(entry_name)
                .to_str()
                .unwrap()
        )
        .unwrap(),
        shlex::try_quote(
            root.join("extensions/mezzanine/peer_helper.mjs")
                .to_str()
                .unwrap()
        )
        .unwrap()
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
        for _ in 0..connections {
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
                node.canonicalize().unwrap(),
                "peer helper supplied sender identity instead of Node"
            );
            if let Some(pid) = producer {
                assert_eq!(origin.identity.process_id, pid);
            }
            producer = Some(origin.identity.process_id);
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
            .all(|binding| binding.retired)
    );
    let binding = exit
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .max_by_key(|binding| binding.generation)
        .unwrap();
    assert!(binding.retired);
    assert_eq!(
        binding.enrollment.as_ref().unwrap().epoch,
        if typed_pi { 1 } else { 2 }
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
        "complete"
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
