//! Callback-independent contention recovery through the ordinary native actor.
//!
//! An owned normally launched child supplies real Unix origin/current-writer
//! evidence. Parent-authored frames exercise the component actor, not an installed
//! vendor SDK or producer transport. An actual SQLite writer refuses the initial
//! ledger admission; a test-only checkpoint observes that failure without
//! manufacturing its outcome. Exactly one end frame must eventually commit.

use super::*;
use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
use std::sync::atomic::Ordering;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Owns a generated native socket/root and an immediately captured child status.
/// This does not modify the shared ordinary fixture or the Pi timer lane.
async fn pending_fixture() -> Option<Fixture> {
    let directory = std::path::Path::new("/tmp").join(format!(
        "mez-pending-retry-{}-{:x}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("control.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    crate::runtime::enable_unix_writer_credentials(listener.as_raw_fd()).unwrap();
    let mut service = RuntimeServiceFixture::new().control_socket(&path).build();
    service.start_initial_pane_process(None).unwrap();
    let executable = std::env::current_exe().unwrap();
    // RuntimeServiceFixture explicitly uses /bin/sh with no user shell hooks.
    // The exit status is captured in the SAME command, immediately after this
    // exact child's libtest exits; EOF never substitutes for successful exit.
    let command = format!(
        "MEZ_TEST_PENDING_RETRY=1 {} --exact runtime::control::external_enrollment::tests::usage_begin::pending_retry::pending_retry_native_child --ignored --quiet; status=$?; printf '%s\\n' \"$status\" > {}\n",
        shlex::try_quote(executable.to_str().unwrap()).unwrap(),
        shlex::try_quote(directory.join("child-exit").to_str().unwrap()).unwrap(),
    );
    service
        .write_runtime_pane_input("%1", command.as_bytes())
        .unwrap();
    let (mut socket, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let uid = crate::runtime::current_effective_uid();
    let origin = match crate::runtime::capture_unix_origin(socket.as_raw_fd(), uid) {
        Ok(origin) => origin,
        Err(error) if error.raw_os_error() == Some(libc::ENOPROTOOPT) => {
            service.terminate_all_pane_processes().unwrap();
            std::fs::remove_dir_all(&directory).unwrap();
            return None;
        }
        Err(error) => panic!("native pending source capture failed: {error}"),
    };
    assert_ne!(origin.identity.process_id, std::process::id());
    let mut connection = ControlConnectionState::new(true, false);
    connection
        .bind_authenticated_peer(AuthenticatedPeer::unix_user(uid))
        .unwrap();
    connection.bind_unix_origin(Arc::new(origin)).unwrap();
    let mut qualified =
        crate::runtime::UnixOriginStream::new(&mut socket, connection.unix_origin().cloned());
    let mut ready = [0];
    tokio::time::timeout(Duration::from_secs(10), qualified.read_exact(&mut ready))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ready, [1]);
    assert!(connection.unix_origin().unwrap().writer_confirmed());
    Some(Fixture {
        service,
        directory,
        listener,
        socket,
        connection,
    })
}

/// A normally invoked native producer does no vendor work or accounting RPC.
/// Its one genuine Unix write qualifies current-writer proof; only the fixture
/// release ends the child. The shell wrapper records actual libtest exit status.
#[test]
#[ignore = "self-executing native pending retry child"]
fn pending_retry_native_child() {
    if std::env::var_os("MEZ_TEST_PENDING_RETRY").is_none() {
        return;
    }
    let discovery = std::env::var("MEZ").unwrap();
    let path = discovery
        .split(crate::runtime::MEZ_ENV_FIELD_SEPARATOR)
        .next()
        .unwrap();
    let mut socket = std::os::unix::net::UnixStream::connect(path).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    socket.write_all(&[1]).unwrap();
    let mut release = [0];
    socket.read_exact(&mut release).unwrap();
    assert_eq!(release, [2]);
}

/// Sends exactly one native-authorized request-end frame. A separate connection
/// holds the real Immediate writer past its250ms busy budget. After the observed
/// first refusal, the fixture releases ONLY that lock and the test checkpoint;
/// no callback/client resend, fresh marker, new source or manual ACK occurs.
/// The baseline one-shot implementation returns an error and zero expense, so
/// the final durable assertion is an actual behavior RED, not a compile failure.
#[tokio::test(flavor = "current_thread")]
async fn external_request_actor_retries_precommit_contention_without_callback() {
    let Some(mut fixture) = pending_fixture().await else {
        return;
    };
    let path = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
    let enrolled = enroll_producer(&mut fixture).await;
    let owner = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap()
        .accounting_owner
        .clone();
    let probe = fixture.service.probe_external_usage_worker_for_tests();
    let params = serde_json::json!({"launch_token":enrolled["result"]["launch_token"],"generation":enrolled["result"]["generation"],"external_session_id":"session-a","request_id":"a".repeat(64)});
    let service = std::mem::replace(&mut fixture.service, RuntimeServiceFixture::new().build());
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let task = tokio::spawn(actor.run());
    let begin = crate::control::encode_control_body(&serde_json::json!({"jsonrpc":"2.0","id":"begin","method":"agent/external/usage-begin","params":params}).to_string());
    let response = handle
        .handle_control_input_for_connection(begin, 8192, fixture.connection.clone())
        .await
        .unwrap();
    let (body, _) = crate::control::decode_control_frame(&response.output, 8192).unwrap();
    let ready: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(ready["result"]["accepted"], true);
    let observed_at = crate::runtime::current_unix_seconds();
    let mut end = params.clone();
    end["event_id"] = serde_json::json!("b".repeat(64));
    end["observed_at"] = serde_json::json!(observed_at);
    end["provider"] = serde_json::json!("fixture");
    end["model"] = serde_json::json!("original-model");
    end["counters"] =
        serde_json::json!({"input_tokens":10,"output_tokens":3,"cached_input_tokens":0});
    let database = rusqlite::Connection::open(&path).unwrap();
    database.execute_batch("BEGIN IMMEDIATE").unwrap();
    let started = Instant::now();
    let sender = handle.clone();
    let connection = fixture.connection.clone();
    let request = crate::control::encode_control_body(&serde_json::json!({"jsonrpc":"2.0","id":"only-end","method":"agent/external/usage-end","params":end}).to_string());
    let end_task = tokio::spawn(async move {
        sender
            .handle_control_input_for_connection(request, 8192, connection)
            .await
    });
    let failed = tokio::time::timeout(Duration::from_secs(5), probe.failed.notified()).await;
    if failed.is_err() {
        database.execute_batch("ROLLBACK").unwrap();
        probe.release.notify_one();
        handle.shutdown().await.unwrap();
        let mut exit = task.await.unwrap();
        exit.service.terminate_all_pane_processes().unwrap();
        panic!("actual first SQLite writer refusal checkpoint was not reached");
    }
    assert!(started.elapsed() >= Duration::from_millis(200));
    assert_eq!(probe.attempts.load(Ordering::SeqCst), 1);
    assert_eq!(probe.failure.get(), Some(&crate::error::MezErrorKind::Io));
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), handle.lifecycle_state())
            .await
            .unwrap()
            .unwrap(),
        crate::runtime::RuntimeLifecycleState::Running
    );
    for table in [
        "token_usage_events",
        "external_usage_observations",
        "external_usage_receipts",
        "external_usage_streams",
    ] {
        assert_eq!(
            database
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    assert!(
        database
            .query_row(
                "SELECT observation_id FROM external_usage_requests",
                [],
                |row| row.get::<_, Option<String>>(0)
            )
            .unwrap()
            .is_none()
    );
    database.execute_batch("ROLLBACK").unwrap();
    probe.release.notify_one();
    let response = tokio::time::timeout(Duration::from_secs(5), end_task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let (body, _) = crate::control::decode_control_frame(&response.output, 8192).unwrap();
    let result: serde_json::Value = serde_json::from_str(&body).unwrap();
    fixture.socket.write_all(&[2]).await.unwrap();
    let completed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(status) = std::fs::read_to_string(fixture.directory.join("child-exit")) {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        completed.trim(),
        "0",
        "actual native child did not exit successfully"
    );
    handle.shutdown().await.unwrap();
    let mut exit = task.await.unwrap();
    assert_eq!(
        exit.service.external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
    assert_eq!(exit.service.control.external_agents().bindings.len(), 1);
    assert_eq!(
        exit.service
            .control
            .external_agents()
            .bindings
            .values()
            .next()
            .unwrap()
            .accounting_owner,
        owner
    );
    exit.service.terminate_all_pane_processes().unwrap();
    // Cleanup precedes the desired behavior assertion, including on baseline RED.
    assert_eq!(
        result["result"]["durable"], true,
        "one original end must recover after precommit writer refusal: {result}"
    );
    assert_eq!(result["result"]["applied"], true);
    assert_eq!(probe.attempts.load(Ordering::SeqCst), 2);
    assert_eq!(
        database
            .query_row(
                "SELECT sum(input_tokens) FROM token_usage_events",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        10
    );
    assert_eq!(
        database
            .query_row("SELECT observed_at FROM token_usage_events", [], |row| row
                .get::<_, u64>(
                0
            ))
            .unwrap(),
        observed_at
    );
    assert_eq!(
        database
            .query_row(
                "SELECT admitted_at FROM external_usage_requests",
                [],
                |row| row.get::<_, u64>(0)
            )
            .unwrap(),
        ready["result"]["admitted_at"].as_u64().unwrap()
    );
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM external_usage_requests", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        1
    );
    for table in [
        "token_usage_events",
        "external_usage_observations",
        "external_usage_receipts",
        "external_usage_streams",
    ] {
        assert_eq!(
            database
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
}
