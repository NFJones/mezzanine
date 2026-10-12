//! Installed original-epoch proof rejection with a still-living native creator.
//! Only owned bootstrap roots, public module callbacks and real helper ingress
//! are used. Deterministic proof/lease aging is not real wall-clock idle evidence;
//! it distinguishes observer loss from process death without weakening guards.

use super::*;
use crate::host::async_runtime::{
    AsyncRuntimeActorConfig, AsyncRuntimeControlConnectionConfig, AsyncRuntimeSessionActor,
    serve_async_runtime_control_connection_loop,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The actual bare-installed module must close exactly its retained local owner
/// on native terminal epoch rejection, cancel the timer, withhold queued helpers
/// and same-owner re-admission while preserving vendor callbacks. The creator
/// remains alive after telemetry expires; only its later real exit releases the
/// native namespace. No supplied PID/token, primary role or provider work exists.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit MEZ_TEST_NODE_BINARY and freshly built sibling mez required"]
async fn external_curated_installed_negative_proof_closes_local_owner_not_creator() {
    let node = std::path::PathBuf::from(
        std::env::var_os("MEZ_TEST_NODE_BINARY").expect("explicit offline Node path"),
    );
    assert!(node.is_absolute() && node.is_file());
    let Some(mut fixture) = fixture("hold").await else {
        panic!("installed negative proof requires native peer evidence");
    };
    let executable = std::env::current_exe().unwrap();
    let helper = executable.parent().unwrap().parent().unwrap().join("mez");
    assert!(helper.is_file());
    let home = fixture.directory.join("home");
    std::fs::create_dir(&home).unwrap();
    let output = tokio::time::timeout(
        Duration::from_secs(20),
        tokio::process::Command::new(&helper)
            .args(["--json", "bootstrap", "claude"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &home)
            .current_dir(&fixture.directory)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["result"], "installed");
    assert_eq!(report["runtime_verification"], "not-performed");
    let module_path = home.join(".claude/skills/mezzanine-telemetry/hooks/register.mjs");
    let installed_bytes = std::fs::read(&module_path).unwrap();
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/curated-idle-negative-ordinary-fixture.mjs")
        .canonicalize()
        .unwrap();
    let launch =
        serde_json::json!({"node":node,"script":script,"directory":fixture.directory}).to_string();
    assert!(launch.len() <= 4096);
    fixture.socket.write_all(&[7]).await.unwrap();
    fixture
        .socket
        .write_all(&u32::try_from(launch.len()).unwrap().to_be_bytes())
        .await
        .unwrap();
    fixture.socket.write_all(launch.as_bytes()).await.unwrap();
    serve_original_helpers(&mut fixture, 3).await;
    wait_report(&fixture.directory, "positive-proof").await;
    assert_eq!(
        std::fs::read(fixture.directory.join("callback-returned")).unwrap(),
        b"returned"
    );
    let public: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture.directory.join("positive-proof")).unwrap())
            .unwrap();
    let root = fixture.service.pane_process_identity("%1").unwrap();
    let registry = fixture.service.control.external_agents();
    assert_eq!(registry.bindings.len(), 1);
    let binding = registry.bindings.values().next().unwrap();
    assert!(!binding.retired);
    assert_eq!(public["generation"], binding.generation);
    assert_eq!(
        public["run_id"],
        binding.enrollment.as_ref().unwrap().run_generation
    );
    assert!(binding.expires > current_unix_seconds());
    let owner = binding.enrollment.as_ref().unwrap();
    assert_eq!(owner.epoch, 1);
    assert_eq!(owner.curated_observer.as_ref().unwrap().sequence, 1);
    assert!(owner.has_live_observer());
    let producer = owner.producer.clone();
    assert!(producer.is_live());
    // Deterministic callback loss and lease expiry; the real producer survives.
    let binding = fixture
        .service
        .control
        .external_agents_mut()
        .bindings
        .values_mut()
        .next()
        .unwrap();
    binding
        .enrollment
        .as_mut()
        .unwrap()
        .curated_observer
        .as_mut()
        .unwrap()
        .observed_at = Some(Instant::now() - Duration::from_secs(31));
    binding.expires = current_unix_seconds() - 1;
    fixture.service.renew_connected_external_observers();
    fixture.service.reconcile_external_agent_registrations();
    let registry = fixture.service.control.external_agents();
    assert!(registry.bindings.values().next().unwrap().retired);
    assert!(producer.is_live());
    assert!(!registry.enrollments.curated_namespaces.is_empty());
    std::fs::write(fixture.directory.join("retired-while-alive"), b"retired").unwrap();
    serve_original_helpers(&mut fixture, 1).await;
    wait_report(&fixture.directory, "local-closed").await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), fixture.listener.accept())
            .await
            .is_err(),
        "closed local owner launched another helper or re-admitted"
    );
    assert!(producer.is_live());
    assert_eq!(
        fixture
            .service
            .pane_process_identity("%1")
            .unwrap()
            .start_token,
        root.start_token
    );
    let registry = fixture.service.control.external_agents();
    assert_eq!(registry.bindings.len(), 1);
    assert!(registry.bindings.values().next().unwrap().retired);
    assert!(!registry.enrollments.curated_namespaces.is_empty());
    assert!(registry.enrollments.pending.is_empty());
    assert_eq!(
        std::fs::read(&module_path).unwrap(),
        installed_bytes,
        "acceptance must not alter installed source to pass"
    );
    std::fs::write(fixture.directory.join("exit-producer"), b"exit").unwrap();
    let mut completed = [0];
    tokio::time::timeout(
        Duration::from_secs(10),
        fixture.socket.read_exact(&mut completed),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(completed, [1]);
    assert!(!producer.is_live());
    fixture.service.reconcile_external_agent_registrations();
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .enrollments
            .curated_namespaces
            .is_empty()
    );
    assert_eq!(
        fixture
            .service
            .pane_process_identity("%1")
            .unwrap()
            .start_token,
        root.start_token,
        "surviving shell is not the retired Node producer"
    );
}

/// Serves a finite number of actual native helper connections through the real
/// actor, then returns its service for explicit deterministic lease/loss checks.
/// Each caller stays uninitialized and gains no primary/control capability.
async fn serve_original_helpers(fixture: &mut Fixture, requests: usize) {
    let service = std::mem::replace(&mut fixture.service, RuntimeServiceFixture::new().build());
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let task = tokio::spawn(actor.run());
    for _ in 0..requests {
        let (mut socket, _) =
            tokio::time::timeout(Duration::from_secs(10), fixture.listener.accept())
                .await
                .unwrap()
                .unwrap();
        let mut connection = ControlConnectionState::new(true, false);
        serve_async_runtime_control_connection_loop(
            &mut socket,
            &handle,
            &mut connection,
            AsyncRuntimeControlConnectionConfig::new(8192, crate::runtime::current_effective_uid())
                .unwrap(),
            |_, _| false,
        )
        .await
        .unwrap();
        assert!(!connection.initialized());
        assert!(connection.caller_client_id().is_none());
    }
    handle.shutdown().await.unwrap();
    fixture.service = task.await.unwrap().service;
}

/// Waits only for an owned local producer report under a finite fixture deadline.
async fn wait_report(directory: &std::path::Path, name: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !directory.join(name).is_file() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("ordinary installed fixture did not report its phase");
}
