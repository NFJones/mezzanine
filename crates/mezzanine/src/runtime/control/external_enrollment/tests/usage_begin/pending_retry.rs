//! Callback-independent contention recovery through the ordinary native actor.
//!
//! An owned normally launched child supplies real Unix origin/current-writer
//! evidence. Parent-authored frames exercise the component actor, not an installed
//! vendor SDK or producer transport. An actual SQLite writer refuses initial
//! ledger admission; the test-only checkpoint observes failure without inventing
//! its outcome. Unwinding checks release the writer/probe and terminate the
//! original actor's roots before surfacing a failure. Exactly one end must commit.

use super::*;
use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
use futures_util::FutureExt;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::Ordering;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Owns a generated native socket/root and immediately captured child status.
/// Construction panic cleanup retains the real service until it can transfer to
/// Fixture. This does not modify shared producer helpers or the Pi timer lane.
async fn pending_fixture() -> Option<Fixture> {
    let directory = std::path::Path::new("/tmp").join(format!(
        "mez-pending-retry-{}-{:x}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("control.sock");
    let mut service = RuntimeServiceFixture::new().control_socket(&path).build();
    let constructed = AssertUnwindSafe(async {
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        crate::runtime::enable_unix_writer_credentials(listener.as_raw_fd()).unwrap();
        service.start_initial_pane_process(None).unwrap();
        let executable = std::env::current_exe().unwrap();
        let temporary = directory.join("child-exit.tmp");
        let completed = directory.join("child-exit");
        // RuntimeServiceFixture uses /bin/sh, not private user shell hooks. The
        // SAME command captures this exact child's status immediately. Atomic
        // rename prevents an empty/partial file from masquerading as completion.
        let command = format!(
            "MEZ_TEST_PENDING_RETRY=1 {} --exact runtime::control::external_enrollment::tests::usage_begin::pending_retry::pending_retry_native_child --ignored --quiet; status=$?; printf '%s\\n' \"$status\" > {} && mv {} {}\n",
            shlex::try_quote(executable.to_str().unwrap()).unwrap(),
            shlex::try_quote(temporary.to_str().unwrap()).unwrap(),
            shlex::try_quote(temporary.to_str().unwrap()).unwrap(),
            shlex::try_quote(completed.to_str().unwrap()).unwrap(),
        );
        service.write_runtime_pane_input("%1", command.as_bytes()).unwrap();
        let (mut socket, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept()).await.unwrap().unwrap();
        let uid = crate::runtime::current_effective_uid();
        let origin = match crate::runtime::capture_unix_origin(socket.as_raw_fd(), uid) {
            Ok(origin) => origin,
            Err(error) if error.raw_os_error() == Some(libc::ENOPROTOOPT) => return None,
            Err(error) => panic!("native pending source capture failed: {error}"),
        };
        assert_ne!(origin.identity.process_id, std::process::id());
        let mut connection = ControlConnectionState::new(true, false);
        connection.bind_authenticated_peer(AuthenticatedPeer::unix_user(uid)).unwrap();
        connection.bind_unix_origin(Arc::new(origin)).unwrap();
        let mut qualified = crate::runtime::UnixOriginStream::new(&mut socket, connection.unix_origin().cloned());
        let mut ready = [0];
        tokio::time::timeout(Duration::from_secs(10), qualified.read_exact(&mut ready)).await.unwrap().unwrap();
        assert_eq!(ready, [1]);
        assert!(connection.unix_origin().unwrap().writer_confirmed());
        drop(qualified);
        Some((listener, socket, connection))
    }).catch_unwind().await;
    match constructed {
        Ok(Some((listener, socket, connection))) => Some(Fixture {
            service,
            directory,
            listener,
            socket,
            connection,
        }),
        other => {
            let terminated = service.terminate_all_pane_processes();
            let removed = std::fs::remove_dir_all(&directory);
            if let Err(panic) = other {
                std::panic::resume_unwind(panic);
            }
            terminated.unwrap();
            removed.unwrap();
            None
        }
    }
}

/// A normally invoked native producer does no vendor work or accounting RPC.
/// Its genuine Unix write qualifies current-writer proof; only fixture release
/// ends this child. The shell wrapper records actual libtest exit, never EOF.
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

/// Waits for atomic publication of a complete child status record. Nonzero or
/// malformed contents remain evidence to fail after original-owner cleanup.
async fn child_status(
    directory: &std::path::Path,
) -> std::result::Result<String, tokio::time::error::Elapsed> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(status) = std::fs::read_to_string(directory.join("child-exit")) {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
}

/// Sends exactly one native-authorized end. A separate initialized connection
/// holds a real Immediate writer past250ms. Only after observed first refusal
/// does the fixture release its lock and checkpoint: no callback/client resend,
/// fresh marker/source or manual ACK. All intermediate assertion panics are
/// caught for release/shutdown/child/root cleanup, then resumed unchanged.
/// The baseline final durable assertion is a behavior RED, not compile failure.
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
    // The database and probe stay outside the caught body so every panic path
    // can release both, even while the original task retains its admission.
    let database = rusqlite::Connection::open(&path).unwrap();
    let service = std::mem::replace(&mut fixture.service, RuntimeServiceFixture::new().build());
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let task = tokio::spawn(actor.run());
    let mut end_task = None;
    let checked = AssertUnwindSafe(async {
        let begin = crate::control::encode_control_body(&serde_json::json!({"jsonrpc":"2.0","id":"begin","method":"agent/external/usage-begin","params":params}).to_string());
        let response = handle.handle_control_input_for_connection(begin, 8192, fixture.connection.clone()).await.unwrap();
        let (body, _) = crate::control::decode_control_frame(&response.output, 8192).unwrap();
        let ready: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(ready["result"]["accepted"], true);
        let observed_at = crate::runtime::current_unix_seconds();
        let mut end = params.clone();
        end["event_id"] = serde_json::json!("b".repeat(64));
        end["observed_at"] = serde_json::json!(observed_at);
        end["provider"] = serde_json::json!("fixture");
        end["model"] = serde_json::json!("original-model");
        end["counters"] = serde_json::json!({"input_tokens":10,"output_tokens":3,"cached_input_tokens":0});
        database.execute_batch("BEGIN IMMEDIATE").unwrap();
        let started = Instant::now();
        let sender = handle.clone();
        let connection = fixture.connection.clone();
        let request = crate::control::encode_control_body(&serde_json::json!({"jsonrpc":"2.0","id":"only-end","method":"agent/external/usage-end","params":end}).to_string());
        end_task = Some(tokio::spawn(async move { sender.handle_control_input_for_connection(request, 8192, connection).await }));
        tokio::time::timeout(Duration::from_secs(5), probe.failed.notified()).await.expect("actual first SQLite writer refusal checkpoint was not reached");
        assert!(started.elapsed() >= Duration::from_millis(200));
        assert_eq!(probe.attempts.load(Ordering::SeqCst), 1);
        assert_eq!(probe.failure.get(), Some(&crate::error::MezErrorKind::Io));
        assert_eq!(tokio::time::timeout(Duration::from_secs(1), handle.lifecycle_state()).await.unwrap().unwrap(), crate::runtime::RuntimeLifecycleState::Running);
        for table in ["token_usage_events", "external_usage_observations", "external_usage_receipts", "external_usage_streams"] {
            assert_eq!(database.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row.get::<_, i64>(0)).unwrap(), 0);
        }
        assert!(database.query_row("SELECT observation_id FROM external_usage_requests", [], |row| row.get::<_, Option<String>>(0)).unwrap().is_none());
        database.execute_batch("ROLLBACK").unwrap();
        probe.release.notify_one();
        let response = tokio::time::timeout(Duration::from_secs(5), end_task.take().unwrap()).await.unwrap().unwrap().unwrap();
        let (body, _) = crate::control::decode_control_frame(&response.output, 8192).unwrap();
        let result: serde_json::Value = serde_json::from_str(&body).unwrap();
        (result, ready, observed_at)
    }).catch_unwind().await;
    let rollback = if database.is_autocommit() {
        Ok(())
    } else {
        database.execute_batch("ROLLBACK")
    };
    probe.release.notify_one();
    if let Some(waiter) = end_task {
        waiter.abort();
    }
    let released_child =
        tokio::time::timeout(Duration::from_secs(1), fixture.socket.write_all(&[2])).await;
    let completed = child_status(&fixture.directory).await;
    let shutdown = handle.shutdown().await;
    let exited = task.await;
    // Restore the original service into Fixture BEFORE asserting cleanup/results:
    // Fixture::drop now terminates the correct native roots on every later panic.
    let mut exit = exited.unwrap();
    let terminated = exit.service.terminate_all_pane_processes();
    fixture.service = exit.service;
    rollback.unwrap();
    terminated.unwrap();
    shutdown.unwrap();
    released_child.unwrap().unwrap();
    assert_eq!(
        completed.unwrap(),
        "0\n",
        "actual native child did not exit successfully"
    );
    let (result, ready, observed_at) = match checked {
        Ok(value) => value,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    assert_eq!(
        fixture
            .service
            .external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
    assert_eq!(fixture.service.control.external_agents().bindings.len(), 1);
    assert_eq!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .next()
            .unwrap()
            .accounting_owner,
        owner
    );
    // All owners are cleaned up before the expected baseline behavior failure.
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
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        i64::try_from(observed_at).unwrap()
    );
    assert_eq!(
        database
            .query_row(
                "SELECT admitted_at FROM external_usage_requests",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        i64::try_from(ready["result"]["admitted_at"].as_u64().unwrap()).unwrap()
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
