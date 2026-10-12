//! Installed ordinary Cursor command hooks under genuine pane-native provenance.
//!
//! A test-owned fake vendor executes production-installed helper commands; no
//! real vendor/provider/configuration, fd3 or launcher authority is involved.

use super::*;

/// Bare bootstrap twice in an isolated HOME then ordinary creator execution of
/// exact installed hooks admits only its native source. Prompt/Stop generations
/// repaint observational status, unknown context/usage is never charged, and exact
/// sessionEnd retires without a role grant or vendor continuation decision.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires freshly built sibling mez binary; installed Cursor native helper qualification"]
async fn external_cursor_installed_hooks_admit_and_retire_without_launcher() {
    use crate::host::async_runtime::{
        AsyncRuntimeActorConfig, AsyncRuntimeControlConnectionConfig, AsyncRuntimeSessionActor,
        serve_async_runtime_control_connection_loop,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let Some(mut fixture) = fixture("hold").await else {
        panic!("installed Cursor requires native evidence");
    };
    let helper = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("mez");
    let home = fixture.directory.join("home");
    std::fs::create_dir(&home).unwrap();
    let vendor = home.join(".cursor");
    for result in ["installed", "unchanged"] {
        let output = std::process::Command::new(&helper)
            .args(["--json", "bootstrap", "cursor"])
            .env_clear()
            .env("HOME", &home)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty());
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["result"], result);
        assert_eq!(report["usage"], "unavailable-counter-schema");
        assert_eq!(report["manifest_revision"], 2);
    }
    let document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(vendor.join("hooks.json")).unwrap()).unwrap();
    let usage = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&usage));
    let service = std::mem::replace(&mut fixture.service, RuntimeServiceFixture::new().build());
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let task = tokio::spawn(actor.run());
    for (event, generation, status) in [
        ("sessionStart", "a", "completed"),
        ("sessionStart", "a", "completed"),
        ("beforeSubmitPrompt", "a", "completed"),
        ("preCompact", "a", "completed"),
        ("postToolUse", "a", "completed"),
        ("afterAgentResponse", "a", "completed"),
        ("stop", "a", "completed"),
        ("beforeSubmitPrompt", "b", "completed"),
        ("stop", "b", "aborted"),
        ("beforeSubmitPrompt", "c", "completed"),
        ("stop", "c", "error"),
        ("sessionEnd", "c", "error"),
    ] {
        let command = document["hooks"][event][0]["command"].as_str().unwrap();
        let launch=serde_json::json!({"command":command,"event":{"hook_event_name":event,"session_id":"session-a","conversation_id":"session-a","generation_id":generation,"status":status,"loop_count":0,"reason":"user_close","prompt":"PRIVATE","text":"PRIVATE","transcript_path":"PRIVATE","user_email":"PRIVATE","context_tokens":999,"usage":{"input_tokens":999}}}).to_string();
        fixture.socket.write_all(&[9]).await.unwrap();
        fixture
            .socket
            .write_all(&u32::try_from(launch.len()).unwrap().to_be_bytes())
            .await
            .unwrap();
        fixture.socket.write_all(launch.as_bytes()).await.unwrap();
        let (mut socket, _) =
            tokio::time::timeout(Duration::from_secs(5), fixture.listener.accept())
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
        assert!(connection.caller_client_id().is_none());
        assert!(connection.unix_origin().unwrap().writer_confirmed());
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
    assert_eq!(binding.harness, "cursor");
    assert!(binding.retired);
    let source = binding.enrollment.as_ref().unwrap();
    assert!(source.usage_owner_is_durable);
    assert!(source.producer.matches_parent(
        fixture.connection.unix_origin().unwrap().uid(),
        fixture.connection.unix_origin().unwrap().identity
    ));
    let presentation = binding
        .registration
        .as_ref()
        .unwrap()
        .presentation
        .as_ref()
        .unwrap();
    assert_eq!(presentation.sequence, 6);
    assert_eq!(presentation.state, "failed");
    assert!(registry.enrollments.pending.is_empty());
    assert!(fixture.service.external_agent_rows().is_empty());
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
