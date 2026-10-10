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
    qualify_request_client("claude-request-ordinary-fixture.mjs", true, 8).await;
}

/// Shared owned fixture for the pure runner and full literal generator; only
/// compiled source and fixed test scripts enter the ordinary creator launch.
async fn qualify_request_client(script_name: &str, claude: bool, requests: usize) {
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
    let directory = fixture.directory.join("sdk");
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
        crate::integrations::bootstrap::curated_client::request_accounting_body(&helper).unwrap(),
    )
    .unwrap();
    let path = fixture.directory.join("usage.sqlite");
    if claude {
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
