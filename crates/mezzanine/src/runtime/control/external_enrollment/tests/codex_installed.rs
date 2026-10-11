//! Installed Codex callback commands through real ordinary native creators.
//!
//! The isolated fixture installs the actual fixed binary and executes exactly
//! the generated shell command. No supplied PID, capability, inherited observer
//! fd or caller role replaces actual native writer/parent/root authorization.

use super::*;

/// A normally invoked creator executes production-installed SessionStart hooks
/// with no fd3 or launcher secret. Real Unix ingress must register only its
/// qualified native creator, preserve identity on retry, retain unavailable
/// freshness/usage and never initialize a client or allocate provider work.
/// The test requires a freshly built binary and does not run actual Codex.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires freshly built sibling mez binary; real installed Codex helper qualification"]
async fn external_codex_installed_session_start_admits_native_creator_without_launcher() {
    use crate::host::async_runtime::{
        AsyncRuntimeActorConfig, AsyncRuntimeControlConnectionConfig, AsyncRuntimeSessionActor,
        serve_async_runtime_control_connection_loop,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let Some(mut fixture) = fixture("hold").await else {
        panic!("installed Codex helper qualification requires native peer evidence");
    };
    let helper = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("mez");
    let home = fixture.directory.join("home");
    let vendor = home.join(".codex");
    std::fs::create_dir(&home).unwrap();
    let installed = std::process::Command::new(&helper)
        .args(["--json", "bootstrap", "codex"])
        .env_clear()
        .env("HOME", &home)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(installed.status.success(), "{installed:?}");
    assert!(installed.stderr.is_empty());
    let document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(vendor.join("hooks.json")).unwrap()).unwrap();
    let command = document["hooks"]["SessionStart"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(!command.contains("MEZ_CODEX_HELPER"));
    let usage = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&usage));
    let service = std::mem::replace(&mut fixture.service, RuntimeServiceFixture::new().build());
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let task = tokio::spawn(actor.run());
    for (event, turn) in [
        ("SessionStart", "a"),
        ("SessionStart", "a"),
        ("UserPromptSubmit", "a"),
        ("Stop", "a"),
        ("UserPromptSubmit", "b"),
        ("Interrupt", "b"),
        ("Stop", "a"),
        ("SessionEnd", ""),
    ] {
        let launch = serde_json::json!({"command":command,"event":{"hook_event_name":event,"session_id":"session-a","source":"startup","turn_id":turn,"stop_hook_active":false,"prompt":"PRIVATE","transcript_path":"PRIVATE","usage":{"input_tokens":999}}}).to_string();
        fixture.socket.write_all(&[9]).await.unwrap();
        fixture
            .socket
            .write_all(&u32::try_from(launch.len()).unwrap().to_be_bytes())
            .await
            .unwrap();
        fixture.socket.write_all(launch.as_bytes()).await.unwrap();
        let (mut socket, _) =
            tokio::time::timeout(Duration::from_secs(2), fixture.listener.accept())
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
            |served, _| served >= 1,
        )
        .await
        .unwrap();
        assert!(!connection.initialized());
        assert!(connection.unix_origin().unwrap().writer_confirmed());
        assert!(connection.caller_client_id().is_none());
        let mut neutral = [0];
        tokio::time::timeout(
            Duration::from_secs(10),
            fixture.socket.read_exact(&mut neutral),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(neutral, [1]);
    }
    handle.shutdown().await.unwrap();
    fixture.service = task.await.unwrap().service;
    let registry = fixture.service.control.external_agents();
    assert_eq!(registry.bindings.len(), 1);
    let binding = registry.bindings.values().next().unwrap();
    assert_eq!(binding.harness, "codex");
    let source = binding.enrollment.as_ref().unwrap();
    assert!(source.usage_owner_is_durable);
    assert!(source.producer.matches_parent(
        fixture.connection.unix_origin().unwrap().uid(),
        fixture.connection.unix_origin().unwrap().identity
    ));
    assert!(source.observers.is_empty());
    assert!(registry.enrollments.pending.is_empty());
    let presentation = binding
        .registration
        .as_ref()
        .unwrap()
        .presentation
        .as_ref()
        .unwrap();
    assert_eq!(presentation.sequence, 4);
    assert_eq!(presentation.state, "interrupted");
    assert!(binding.retired);
    assert!(fixture.service.external_agent_rows().is_empty());
    assert!(fixture.service.live_pane_harness_status("%1").is_none());
    assert!(fixture.connection.unix_origin().unwrap().is_live());
    let database = rusqlite::Connection::open(usage).unwrap();
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM token_usage_events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM external_usage_sources", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}
