//! Installed Codex callback commands through real ordinary native creators.
//!
//! The isolated fixture installs the actual fixed binary and executes exactly
//! the generated shell command. No supplied PID, capability, inherited observer
//! fd or caller role replaces actual native writer/parent/root authorization.

use super::*;

/// Executes a real installed callback from the isolated native creator. Kernel
/// writer evidence is consumed on its own socket; absence cases must produce no
/// IPC at all. The fixture verifies neutral stdout/stderr/exit before its ACK.
async fn installed_callback(
    fixture: &mut Fixture,
    handle: &crate::host::async_runtime::AsyncRuntimeSessionHandle,
    command: &str,
    event: serde_json::Value,
    callback_count: usize,
) {
    use crate::host::async_runtime::{
        AsyncRuntimeControlConnectionConfig, serve_async_runtime_control_connection_loop,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let launch = serde_json::json!({"command":command,"event":event}).to_string();
    fixture.socket.write_all(&[9]).await.unwrap();
    fixture
        .socket
        .write_all(&u32::try_from(launch.len()).unwrap().to_be_bytes())
        .await
        .unwrap();
    fixture.socket.write_all(launch.as_bytes()).await.unwrap();
    for _ in 0..callback_count {
        let (mut socket, _) =
            tokio::time::timeout(Duration::from_secs(2), fixture.listener.accept())
                .await
                .unwrap()
                .unwrap();
        let mut connection = ControlConnectionState::new(true, false);
        serve_async_runtime_control_connection_loop(
            &mut socket,
            handle,
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
    }
    let mut neutral = [0];
    // The creator sends this ACK only after wait_with_output has observed the
    // actual helper exit. Keep polling for forbidden extra IPC until that point,
    // including delayed connects; queued accepts win over a simultaneous ACK.
    tokio::select! {
        biased;
        _ = fixture.listener.accept() => panic!("installed callback produced unexpected IPC"),
        completed = tokio::time::timeout(
            Duration::from_secs(10), fixture.socket.read_exact(&mut neutral),
        ) => { completed.unwrap().unwrap(); }
    }
    assert_eq!(neutral, [1]);
}

/// A normally invoked creator executes production-installed SessionStart hooks
/// with no fd3 or launcher secret. Real Unix ingress must register only its
/// qualified native creator, preserve identity on retry, retain unavailable
/// freshness/usage and never initialize a client or allocate provider work.
/// Real-shaped selected-model, OTEL completion and context-total fields remain
/// inert even alongside the admitted hook: a durable source namespace does not
/// grant request readiness, import unqualified telemetry or produce expense.
/// The test requires a freshly built binary and does not run actual Codex.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires freshly built sibling mez binary; real installed Codex helper qualification"]
async fn external_codex_installed_session_start_admits_native_creator_without_launcher() {
    use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
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
    for event in ["PreToolUse", "PermissionRequest", "PostToolUse"] {
        assert_eq!(document["hooks"][event][0]["hooks"][0]["command"], command);
    }
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
        ("PreToolUse", "a"),
        ("PermissionRequest", "a"),
        ("PostToolUse", "a"),
        ("Stop", "a"),
        ("UserPromptSubmit", "b"),
        ("Interrupt", "b"),
        ("Stop", "a"),
        ("SessionEnd", ""),
    ] {
        installed_callback(&mut fixture, &handle, command, serde_json::json!({"hook_event_name":event,"session_id":"session-a","source":"startup","turn_id":turn,"stop_hook_active":false,"prompt":"PRIVATE","transcript_path":"PRIVATE","usage":{"input_tokens":999},
            "model":"selected-not-response","permission_mode":"default","last_assistant_message":"PRIVATE","tool_name":"PRIVATE","tool_input":{"command":"PRIVATE"},"tool_response":"PRIVATE",
            "event.name":"codex.sse_event","event.kind":"response.completed","conversation.id":"unrelated-session",
            "input_token_count":"121","output_token_count":"13","cached_token_count":32,"cache_write_token_count":4,"reasoning_token_count":2,
            "user.email":"PRIVATE","total_token_usage":{"input_tokens":5000,"total_tokens":6000},"last_token_usage":{"input_tokens":1000,"total_tokens":1000}}), 1).await;
    }
    handle.shutdown().await.unwrap();
    fixture.service = task.await.unwrap().service;
    let registry = fixture.service.control.external_agents();
    assert_eq!(registry.bindings.len(), 1);
    let binding = registry.bindings.values().next().unwrap();
    assert_eq!(binding.harness, "codex");
    let source = binding.enrollment.as_ref().unwrap();
    assert!(source.usage_owner_is_durable);
    assert!(!source.request_reports_seen);
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
    assert_eq!(presentation.sequence, 6);
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
    for table in [
        "external_usage_requests",
        "external_usage_observations",
        "external_usage_receipts",
        "external_usage_streams",
    ] {
        assert_eq!(
            database
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0,
            "unqualified callback counters cannot allocate {table}"
        );
    }
}

/// Production-installed synchronous model hooks execute the fresh fixed binary
/// under an ordinary native creator and settle three distinct equal-count model
/// requests, including failed and cancelled outcomes. Private/selected-model
/// metadata and occupancy totals remain inert; each helper exits neutrally and
/// the exact SessionEnd retires presentation without inventing producer death.
/// This source-shaped installed path executes no vendor or provider request.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires freshly built sibling mez binary; real installed Codex helper qualification"]
async fn external_codex_installed_model_hooks_record_incurred_requests_without_launcher() {
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
    for event in ["PreToolUse", "PermissionRequest", "PostToolUse"] {
        assert_eq!(document["hooks"][event][0]["hooks"][0]["command"], command);
    }
    let usage = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&usage));
    let service = std::mem::replace(&mut fixture.service, RuntimeServiceFixture::new().build());
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let task = tokio::spawn(actor.run());
    let mut post_index = 0;
    for (event, turn) in [
        ("SessionStart", "a"),
        ("UserPromptSubmit", "a"),
        ("PreModelRequest", "a"),
        ("PostModelRequest", "a"),
        ("PreModelRequest", "a"),
        ("PostModelRequest", "a"),
        ("PreModelRequest", "a"),
        ("PostModelRequest", "a"),
        ("Stop", "a"),
        ("SessionEnd", ""),
    ] {
        let mut payload = serde_json::json!({"hook_event_name":event,"session_id":"session-a","source":"startup","turn_id":turn,"stop_hook_active":false,
            "model":"PRIVATE-selected-not-response","cwd":"PRIVATE","transcript_path":"PRIVATE","error":"PRIVATE",
            "total_token_usage":{"input_tokens":5000},"last_token_usage":{"input_tokens":1000}});
        if event == "PostModelRequest" {
            payload["status"] = ["completed", "failed", "cancelled"][post_index].into();
            payload["token_usage"] = serde_json::json!({"input_tokens":10,"output_tokens":3,"cached_input_tokens":2,"reasoning_output_tokens":1,"total_tokens":13});
            post_index += 1;
        }
        let event_command = document["hooks"][event][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        if matches!(event, "PreModelRequest" | "PostModelRequest") {
            assert_eq!(document["hooks"][event][0]["hooks"][0]["async"], false);
            assert_eq!(document["hooks"][event][0]["hooks"][0]["timeout"], 3);
        }
        let launch = serde_json::json!({"command":event_command,"event":payload}).to_string();
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
                .unwrap_or_else(|error| panic!("installed {event} produced no callback: {error}"))
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
    assert!(source.request_reports_seen);
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
    assert_eq!(presentation.sequence, 2);
    assert_eq!(presentation.state, "ready");
    assert!(binding.retired);
    assert!(fixture.service.external_agent_rows().is_empty());
    assert!(fixture.service.live_pane_harness_status("%1").is_none());
    assert!(fixture.connection.unix_origin().unwrap().is_live());
    let database = rusqlite::Connection::open(usage).unwrap();
    let totals: (i64,i64,i64,i64) = database.query_row("SELECT count(*),sum(input_tokens),sum(output_tokens),sum(reasoning_tokens) FROM token_usage_events", [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap();
    assert_eq!(totals, (3, 30, 9, 3));
    let labels: (String, String) = database
        .query_row(
            "SELECT provider,model FROM token_usage_events LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(labels, ("unavailable".into(), "unavailable".into()));
    for (table, count) in [
        ("external_usage_sources", 1),
        ("external_usage_requests", 3),
        ("external_usage_observations", 3),
        ("external_usage_receipts", 3),
        ("external_usage_streams", 1),
    ] {
        assert_eq!(
            database
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            count,
            "{table}"
        );
    }
}

/// A shared creator can retain genuine pane ancestry and inherited routing while
/// serving another client. Its installed fixed helper must remain neutral and
/// refuse before IPC, so even a server from before the runtime guard cannot
/// assign that callback to the launching pane. No vendor or provider is run.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires freshly built sibling mez binary; installed shared-mode qualification"]
async fn external_codex_installed_shared_server_callback_is_neutral_without_ipc() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let Some(mut fixture) = fixture("codex-shared").await else {
        panic!("shared Codex qualification requires native peer evidence")
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
        serde_json::from_slice(&std::fs::read(home.join(".codex/hooks.json")).unwrap()).unwrap();
    let command = document["hooks"]["SessionStart"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    let launch = serde_json::json!({"command":command,"event":{"hook_event_name":"SessionStart","session_id":"session-a","source":"startup"}}).to_string();
    fixture.socket.write_all(&[9]).await.unwrap();
    fixture
        .socket
        .write_all(&u32::try_from(launch.len()).unwrap().to_be_bytes())
        .await
        .unwrap();
    fixture.socket.write_all(launch.as_bytes()).await.unwrap();
    let mut neutral = [0];
    tokio::time::timeout(
        Duration::from_secs(10),
        fixture.socket.read_exact(&mut neutral),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(neutral, [1]);
    assert!(
        tokio::time::timeout(Duration::from_millis(200), fixture.listener.accept())
            .await
            .is_err()
    );
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .is_empty()
    );
    assert!(fixture.service.external_agent_rows().is_empty());
}

/// Installed callbacks qualify all initial main-session boundaries under the
/// same living ordinary creator, retaining older threads until all are admitted.
/// Each session owns one durable namespace and
/// records one model request. Same-session resume declarations are inert, compact
/// and child lifecycle contexts produce no IPC. Unstarted child model facts cannot
/// admit expense; later parent facts still work. End
/// and late model callbacks cannot revive an original retired session or bill
/// it twice. The next session's clear/fork/resume source cannot import history.
/// This uses isolated bootstrap and the actual fixed helper, not actual Codex.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires freshly built sibling mez binary; installed session-boundary qualification"]
async fn external_codex_installed_session_boundaries_preserve_parent_and_accounting() {
    use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
    let Some(mut fixture) = fixture("hold").await else {
        panic!("installed Codex session qualification requires native peer evidence");
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
    let installed = std::process::Command::new(helper)
        .args(["--json", "bootstrap", "codex"])
        .env_clear()
        .env("HOME", &home)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(installed.status.success(), "{installed:?}");
    assert!(installed.stderr.is_empty());
    let document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.join(".codex/hooks.json")).unwrap()).unwrap();
    let usage = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&usage));
    let service = std::mem::replace(&mut fixture.service, RuntimeServiceFixture::new().build());
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let task = tokio::spawn(actor.run());
    for (index, boundary) in ["startup", "resume", "clear", "fork"]
        .into_iter()
        .enumerate()
    {
        let session = format!("session-{index}");
        for (event, source, child, callback_count) in [
            ("SessionStart", boundary, false, 1),
            ("SessionStart", boundary, false, 1),
            ("SessionStart", "resume", false, 1),
            ("SessionStart", "compact", false, 0),
            ("SessionStart", "clear", true, 0),
            ("SessionEnd", boundary, true, 0),
            ("UserPromptSubmit", boundary, false, 1),
            ("PreModelRequest", boundary, true, 1),
            ("PostModelRequest", boundary, true, 2),
            ("PreModelRequest", boundary, false, 1),
            ("PostModelRequest", boundary, false, 1),
            ("Stop", boundary, false, 1),
        ] {
            let mut payload = serde_json::json!({"hook_event_name":event,"session_id":session,"source":source,
                "turn_id":"turn-a","stop_hook_active":false,"status":"completed",
                "token_usage":{"input_tokens":10,"output_tokens":3},"cwd":"PRIVATE","transcript_path":"PRIVATE"});
            if child {
                payload["agent_id"] = "child-a".into();
                payload["agent_type"] = "worker".into();
            }
            let command = document["hooks"][event][0]["hooks"][0]["command"]
                .as_str()
                .unwrap();
            installed_callback(&mut fixture, &handle, command, payload, callback_count).await;
        }
    }
    // New threads can coexist in one local creator. Retire in a different order
    // from admission; every exact session remains separate and no late callback
    // can adopt another living session or recharge an ended one.
    for (index, boundary) in ["startup", "resume", "clear", "fork"]
        .into_iter()
        .enumerate()
        .rev()
    {
        for (event, callback_count) in [
            ("SessionEnd", 1),
            ("SessionStart", 1),
            ("PostModelRequest", 2),
        ] {
            let payload = serde_json::json!({"hook_event_name":event,"session_id":format!("session-{index}"),
                "source":boundary,"turn_id":"turn-a","stop_hook_active":false,"status":"completed",
                "token_usage":{"input_tokens":10,"output_tokens":3}});
            let command = document["hooks"][event][0]["hooks"][0]["command"]
                .as_str()
                .unwrap();
            installed_callback(&mut fixture, &handle, command, payload, callback_count).await;
        }
    }
    handle.shutdown().await.unwrap();
    fixture.service = task.await.unwrap().service;
    let registry = fixture.service.control.external_agents();
    assert_eq!(registry.bindings.len(), 4);
    let mut owners = std::collections::BTreeSet::new();
    for binding in registry.bindings.values() {
        assert!(binding.retired);
        assert_eq!(binding.enrollment.as_ref().unwrap().epoch, 1);
        assert!(owners.insert(binding.accounting_owner.clone()));
        let presentation = binding
            .registration
            .as_ref()
            .unwrap()
            .presentation
            .as_ref()
            .unwrap();
        assert_eq!(presentation.sequence, 2);
        assert_eq!(presentation.state, "ready");
    }
    assert!(registry.enrollments.pending.is_empty());
    assert!(fixture.service.external_agent_rows().is_empty());
    assert!(fixture.service.live_pane_harness_status("%1").is_none());
    assert!(fixture.connection.unix_origin().unwrap().is_live());
    let database = rusqlite::Connection::open(usage).unwrap();
    for table in [
        "external_usage_sources",
        "external_usage_requests",
        "token_usage_events",
    ] {
        assert_eq!(
            database
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            4,
            "{table}"
        );
    }
    assert_eq!(
        database
            .query_row(
                "SELECT sum(input_tokens) FROM token_usage_events",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        40
    );
}

/// The generated child boundary hooks bind actual native children only to their
/// original qualified parent session. Equal parent/sibling turn labels maintain
/// distinct readiness; Stop may continue and cannot retire the parent. Installed
/// duplicate Post callbacks without a new Pre cannot recharge settled expense.
/// This runs the actual fresh helper and isolated installer without vendor calls.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires freshly built sibling mez binary; installed child accounting qualification"]
async fn external_codex_installed_child_hooks_account_without_parent_overwrite() {
    use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
    let Some(mut fixture) = fixture("hold").await else {
        panic!("installed child qualification requires native evidence")
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
    let installed = std::process::Command::new(helper)
        .args(["--json", "bootstrap", "codex"])
        .env_clear()
        .env("HOME", &home)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(installed.status.success(), "{installed:?}");
    assert!(installed.stderr.is_empty());
    let document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.join(".codex/hooks.json")).unwrap()).unwrap();
    let usage = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&usage));
    let service = std::mem::replace(&mut fixture.service, RuntimeServiceFixture::new().build());
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let task = tokio::spawn(actor.run());
    for (event, id, turn, callbacks) in [
        ("SessionStart", None, "shared", 1),
        ("UserPromptSubmit", None, "shared", 1),
        ("SubagentStart", Some("a"), "shared", 1),
        ("SubagentStart", Some("b"), "shared", 1),
        ("PreModelRequest", None, "shared", 1),
        ("PreModelRequest", Some("a"), "shared", 1),
        ("PreModelRequest", Some("b"), "shared", 1),
        ("SubagentStop", Some("a"), "shared", 1),
        ("PostModelRequest", Some("a"), "shared", 1),
        ("PostModelRequest", None, "shared", 1),
        ("PostModelRequest", Some("b"), "shared", 1),
        ("Stop", None, "shared", 1),
        ("PreModelRequest", Some("a"), "shared", 1),
        ("PostModelRequest", Some("a"), "shared", 1),
        ("SubagentStop", Some("a"), "shared", 1),
        ("PostModelRequest", Some("a"), "shared", 2),
        ("SessionEnd", Some("a"), "shared", 0),
    ] {
        let mut payload = serde_json::json!({"hook_event_name":event,"session_id":"session-a","source":"startup",
            "turn_id":turn,"stop_hook_active":false,"status":"completed","token_usage":{"input_tokens":10,"output_tokens":3},
            "cwd":"PRIVATE","agent_transcript_path":"PRIVATE","last_assistant_message":"PRIVATE"});
        if let Some(id) = id {
            payload["agent_id"] = id.into();
            payload["agent_type"] = "PRIVATE".into();
        }
        let command = document["hooks"][event][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        installed_callback(&mut fixture, &handle, command, payload, callbacks).await;
    }
    handle.shutdown().await.unwrap();
    fixture.service = task.await.unwrap().service;
    let registry = fixture.service.control.external_agents();
    assert_eq!(registry.bindings.len(), 1);
    let binding = registry.bindings.values().next().unwrap();
    assert!(!binding.retired);
    assert_eq!(binding.enrollment.as_ref().unwrap().epoch, 1);
    let presentation = binding
        .registration
        .as_ref()
        .unwrap()
        .presentation
        .as_ref()
        .unwrap();
    assert_eq!(presentation.sequence, 2);
    assert_eq!(presentation.state, "ready");
    assert_eq!(fixture.service.external_agent_rows().len(), 1);
    let database = rusqlite::Connection::open(usage).unwrap();
    for table in ["external_usage_requests", "token_usage_events"] {
        assert_eq!(
            database
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            4
        );
    }
    assert_eq!(
        database
            .query_row(
                "SELECT sum(input_tokens) FROM token_usage_events",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        40
    );
    assert_eq!(
        database
            .query_row(
                "SELECT count(DISTINCT owner) FROM external_usage_requests",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert!(fixture.connection.unix_origin().unwrap().is_live());
}
