//! Pure SDK request adapter through genuine ordinary-process native ingress.
//! Only temporary compiled siblings and a fixed helper are used; no vendor,
//! paid request, user settings or client-supplied native identity exists.

use super::*;
use crate::host::async_runtime::{
    AsyncRuntimeActorConfig, AsyncRuntimeControlConnectionConfig, AsyncRuntimeSessionActor,
    serve_async_runtime_control_connection_loop,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// An ordinary Node creator uses the literal SDK runner and pure modules to
/// obtain genuine original request readiness, deliver synthetic expense once,
/// retain local ACK replay after stop and refuse new work. Every helper request
/// crosses real writer/parent/root proof; no initialized roles are introduced.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit MEZ_TEST_NODE_BINARY and freshly built sibling mez required"]
async fn external_curated_request_sdk_client_has_native_durable_ack() {
    qualify_request_client("curated-request-accounting-ordinary-fixture.mjs", false, 4).await;
}

/// The full literal Claude generator consumes public-contract fake outcomes on
/// an ordinary Node creator. Real native begin/end admission charges once on
/// callback replay, preserves unknown usage, and never grants client roles.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit MEZ_TEST_NODE_BINARY and freshly built sibling mez required"]
async fn external_claude_request_generator_has_native_durable_ack() {
    qualify_request_client("claude-request-ordinary-fixture.mjs", true, 14).await;
}

/// Bare production bootstrap must install the actual module consumed by an
/// ordinary SDK-shaped creator. No manual source rendering, root/version/apply
/// flag, launcher capability or paid request substitutes for this assembled seam.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit MEZ_TEST_NODE_BINARY and freshly built sibling mez required"]
async fn external_claude_bare_bootstrap_installed_module_has_native_durable_usage() {
    qualify_request_client_with_install("claude-request-ordinary-fixture.mjs", true, 14, true)
        .await;
}

/// Shared owned fixture for the pure runner and full literal generator; only
/// compiled source and fixed test scripts enter the ordinary creator launch.
async fn qualify_request_client(script_name: &str, claude: bool, requests: usize) {
    qualify_request_client_with_install(script_name, claude, requests, false).await;
}

/// Selects manual component sources or actual installed bytes, never replaces
/// installed source to make the assembled workflow pass. Both use native ingress.
async fn qualify_request_client_with_install(
    script_name: &str,
    claude: bool,
    requests: usize,
    installed: bool,
) {
    let node = std::path::PathBuf::from(
        std::env::var_os("MEZ_TEST_NODE_BINARY").expect("explicit offline Node path"),
    );
    assert!(node.is_absolute() && node.is_file());
    let Some(mut fixture) = fixture("hold").await else {
        panic!("SDK request qualification requires native peer evidence");
    };
    let executable = std::env::current_exe().unwrap();
    let helper = executable.parent().unwrap().parent().unwrap().join("mez");
    assert!(helper.is_file());
    let mut directory = fixture.directory.join("sdk");
    if installed {
        assert!(claude);
        let home = fixture.directory.join("home");
        std::fs::create_dir(&home).unwrap();
        let preview = tokio::process::Command::new(&helper)
            .args(["--json", "bootstrap", "claude", "--dry-run"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &home)
            .kill_on_drop(true)
            .current_dir(&fixture.directory)
            .output();
        let preview = tokio::time::timeout(std::time::Duration::from_secs(20), preview)
            .await
            .unwrap()
            .unwrap();
        assert!(preview.status.success());
        assert_eq!(
            std::fs::read_dir(&home).unwrap().count(),
            0,
            "preview must not create vendor or private installer state"
        );
        for expected in ["installed", "unchanged"] {
            let output = tokio::process::Command::new(&helper)
                .args(["--json", "bootstrap", "claude"])
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", &home)
                .kill_on_drop(true)
                .current_dir(&fixture.directory)
                .output();
            let output = tokio::time::timeout(std::time::Duration::from_secs(20), output)
                .await
                .unwrap()
                .unwrap();
            assert!(output.status.success(), "owned bare bootstrap failed");
            let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(report["result"], expected);
            assert_eq!(report["runtime_verification"], "not-performed");
        }
        directory = home.join(".claude/skills/mezzanine-telemetry/hooks");
        assert!(directory.join("register.mjs").is_file());
    } else {
        std::fs::create_dir(&directory).unwrap();
        for (name, source) in
            crate::integrations::bootstrap::curated_client::request_accounting_sources()
        {
            std::fs::write(directory.join(name), source).unwrap();
        }
        std::fs::write(
            directory.join("curated_lifetime.mjs"),
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/integrations/bootstrap/curated_lifetime.mjs"
            )),
        )
        .unwrap();
        std::fs::write(
            directory.join("runner.mjs"),
            crate::integrations::bootstrap::curated_client::request_accounting_body(&helper)
                .unwrap(),
        )
        .unwrap();
    }
    let path = fixture.directory.join("usage.sqlite");
    if claude && !installed {
        std::fs::write(
            directory.join("register.mjs"),
            crate::integrations::bootstrap::curated_client::module_source(&helper, 1000).unwrap(),
        )
        .unwrap();
        std::fs::write(
            directory.join("claude_observer.mjs"),
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/integrations/bootstrap/claude_observer.mjs"
            )),
        )
        .unwrap();
        std::fs::write(
            directory.join("claude_request.mjs"),
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/integrations/bootstrap/claude_request.mjs"
            )),
        )
        .unwrap();
    }
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
    if claude {
        fixture
            .service
            .session
            .set_pane_title_explicit("%1", "Pinned")
            .unwrap();
    }
    let inspection_client = if installed {
        Some(
            fixture
                .service
                .attach_primary(
                    "assembled-status",
                    true,
                    mez_mux::layout::Size::new(100, 40).unwrap(),
                    120,
                )
                .unwrap(),
        )
    } else {
        None
    };
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts")
        .join(script_name)
        .canonicalize()
        .unwrap();
    let launch = serde_json::json!({"node":node,"script":script,"directory":directory}).to_string();
    assert!(launch.len() <= 4096);
    let service = std::mem::replace(&mut fixture.service, RuntimeServiceFixture::new().build());
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let task = tokio::spawn(actor.run());
    fixture.socket.write_all(&[7]).await.unwrap();
    fixture
        .socket
        .write_all(&u32::try_from(launch.len()).unwrap().to_be_bytes())
        .await
        .unwrap();
    fixture.socket.write_all(launch.as_bytes()).await.unwrap();
    for ordinal in 0..requests {
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
        if installed && ordinal == 4 {
            let status = handle
                .execute_agent_shell_command(
                    inspection_client.as_ref().unwrap().clone(),
                    "/status".into(),
                )
                .await
                .unwrap();
            assert!(status.contains("### External Telemetry"), "{status}");
            assert!(status.contains("awaiting-first-proof"), "{status}");
            assert!(
                status.contains("request-bound-external-reported"),
                "{status}"
            );
            assert!(status.contains("response-model"), "{status}");
        }
    }
    let mut completed = [0];
    tokio::time::timeout(
        Duration::from_secs(10),
        fixture.socket.read_exact(&mut completed),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(completed, [1]);
    handle.shutdown().await.unwrap();
    fixture.service = task.await.unwrap().service;
    assert_eq!(
        fixture
            .service
            .external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .enrollments
            .pending
            .is_empty()
    );
    if claude {
        let bindings = fixture.service.control.external_agents();
        let presentation = bindings
            .bindings
            .values()
            .next()
            .unwrap()
            .registration
            .as_ref()
            .unwrap()
            .presentation
            .as_ref()
            .unwrap();
        assert_eq!(presentation.sequence, 6);
        assert_eq!(presentation.state, "complete");
        assert_eq!(presentation.title.as_deref(), Some("Claude"));
        assert!(
            fixture.service.terminal_frame_context().panes["%1"]
                .pane_title_override
                .is_none()
        );
    }
    if installed {
        assert!(fixture.service.agent_shell_store().get("%1").is_none());
        assert!(
            fixture.service.external_agent_rows().is_empty(),
            "completed ordinary creator must not remain live under the pane shell"
        );
        assert_eq!(fixture.service.reconcile_external_agent_registrations(), 1);
        let status = fixture
            .service
            .runtime_agent_status_display_with_options("%1", false)
            .unwrap();
        assert!(
            status.contains("No current external telemetry is observed"),
            "{status}"
        );
        assert!(
            status.contains("response-model"),
            "incurred usage must survive telemetry retirement: {status}"
        );
    }
    let database = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        database
            .query_row(
                "SELECT sum(input_tokens) FROM token_usage_events",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        if claude { 16 } else { 10 }
    );
    assert_eq!(
        database
            .query_row(
                "SELECT count(*) FROM external_usage_observations",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
}
