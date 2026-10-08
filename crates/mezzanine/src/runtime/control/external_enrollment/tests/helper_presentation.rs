//! Native child helpers use an existing independently qualified run, not a
//! supplied PID or a new accounting/registration owner.

use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Invoked as a genuine direct child of the ordinary producer. The actual CLI
/// parser/dispatcher discovers its route from ordinary MEZ environment and
/// consumes the private envelope only on stdin. Even daemon rejection must
/// preserve neutral output, successful exit and empty diagnostics. This is a
/// self-executing Rust fixture, not an installed vendor/helper-binary assertion.
#[test]
#[ignore = "self-executing fixed CLI helper fixture"]
fn external_helper_presentation_cli_child_fixture() {
    if std::env::var_os("MEZ_TEST_ENROLL_MODE").is_none() {
        return;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = runtime
        .block_on(crate::cli::run_with(
            vec!["mez".into(), "harness-event".into()],
            crate::cli::CliEnv::from_process(),
            false,
            &mut stdout,
            &mut stderr,
        ))
        .unwrap();
    assert_eq!(code, 0);
    assert_eq!(stdout, b"{}\n");
    assert!(stderr.is_empty());
}

/// A normally invoked, independently enrolled producer spawns the actual fixed
/// CLI dispatcher as a direct child with private stdin, not argv/env credentials.
/// Each helper opens its own Unix connection and sends a real framed request;
/// native sender/parent/ancestry and actor settlement decide attribution. Allowed
/// updates and identical replay preserve the original run, while the ordinary
/// producer-only method and wrong handles remain neutral but cannot update it.
#[tokio::test(flavor = "current_thread")]
async fn external_helper_presentation_cli_unix_actor_roundtrip_is_neutral() {
    use crate::host::async_runtime::{
        AsyncRuntimeActorConfig, AsyncRuntimeControlConnectionConfig, AsyncRuntimeSessionActor,
        serve_async_runtime_control_connection_loop,
    };
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let response = enroll_producer(&mut fixture).await;
    let binding = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap();
    let before = (
        binding.generation,
        binding.accounting_owner.clone(),
        binding.expires,
        binding.enrollment.as_ref().unwrap().observers.len(),
    );
    let clients = fixture.service.session().clients().len();
    let service = std::mem::replace(&mut fixture.service, RuntimeServiceFixture::new().build());
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let actor_task = tokio::spawn(actor.run());
    for (operation, wrong_handle, sequence, state) in [
        ("helper-presentation", false, 1, "running"),
        ("helper-presentation", false, 1, "running"),
        ("presentation", false, 2, "failed"),
        ("helper-presentation", true, 2, "failed"),
        ("helper-observe", false, 2, "complete"),
        ("helper-observe", false, 2, "complete"),
        ("helper-observe", true, 3, "failed"),
    ] {
        let token = if wrong_handle {
            serde_json::json!("x".repeat(43))
        } else {
            response["result"]["launch_token"].clone()
        };
        let event = if operation == "helper-observe" {
            let generation = if wrong_handle {
                serde_json::json!(0)
            } else {
                response["result"]["generation"].clone()
            };
            let envelope = serde_json::json!({"operation":operation,"harness":"pi","generation":generation,
                "external_session_id":"session-a","data":{"sequence":sequence,"state":state,"title":"CLI fixture"}});
            assert!(envelope.get("launch_token").is_none());
            zeroize::Zeroizing::new(envelope.to_string())
        } else {
            zeroize::Zeroizing::new(serde_json::json!({"operation":operation,
                "launch_token":token,"generation":response["result"]["generation"],"external_session_id":"session-a",
                "data":{"sequence":sequence,"state":state,"title":"CLI fixture"}}).to_string())
        };
        let length = u32::try_from(event.len()).unwrap().to_be_bytes();
        fixture.socket.write_all(&[5]).await.unwrap();
        fixture.socket.write_all(&length).await.unwrap();
        fixture.socket.write_all(event.as_bytes()).await.unwrap();
        let (mut socket, _) =
            tokio::time::timeout(Duration::from_secs(10), fixture.listener.accept())
                .await
                .unwrap()
                .unwrap();
        let uid = crate::runtime::current_effective_uid();
        let origin =
            Arc::new(crate::runtime::capture_unix_origin(socket.as_raw_fd(), uid).unwrap());
        let mut connection = ControlConnectionState::new(true, false);
        connection
            .bind_authenticated_peer(AuthenticatedPeer::unix_user(uid))
            .unwrap();
        connection.bind_unix_origin(origin.clone()).unwrap();
        tokio::time::timeout(
            Duration::from_secs(10),
            serve_async_runtime_control_connection_loop(
                &mut socket,
                &handle,
                &mut connection,
                AsyncRuntimeControlConnectionConfig::new(8192, uid).unwrap(),
                |served, _| served >= 1,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(origin.writer_confirmed());
        assert!(!connection.initialized());
        assert!(connection.caller_client_id().is_none());
        let mut neutral_success = [0];
        tokio::time::timeout(
            Duration::from_secs(10),
            fixture.socket.read_exact(&mut neutral_success),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            neutral_success,
            [1],
            "fixed helper changed output or exit behavior"
        );
        assert!(
            !origin.is_live(),
            "producer must reap the completed helper before acknowledgment"
        );
    }
    handle.shutdown().await.unwrap();
    let mut exit = actor_task.await.unwrap();
    assert_eq!(exit.service.session().clients().len(), clients);
    assert_eq!(exit.service.control.external_agents().bindings.len(), 1);
    let binding = exit
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap();
    assert!(!binding.retired);
    assert_eq!(
        (
            binding.generation,
            binding.accounting_owner.clone(),
            binding.expires,
            binding.enrollment.as_ref().unwrap().observers.len()
        ),
        before
    );
    let presentation = binding
        .registration
        .as_ref()
        .unwrap()
        .presentation
        .as_ref()
        .unwrap();
    assert_eq!(presentation.sequence, 2);
    assert_eq!(presentation.state, "complete");
    assert!(
        exit.service
            .control
            .external_agents()
            .enrollments
            .pending
            .is_empty()
    );
    assert_eq!(exit.service.reconcile_external_agent_registrations(), 0);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Runs genuine off-actor native observation and returns the parsed settlement
/// without printing private handles or presentation metadata on failure.
pub(super) async fn settle(
    fixture: &mut Fixture,
    work: ExternalEnrollmentWork,
    connection: &ControlConnectionState,
) -> serde_json::Value {
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, connection),
    )
    .unwrap()
}

/// Admits the actual ordinary producer using native evidence before any helper
/// exists, retaining only the test-private response for callback construction.
pub(super) async fn enroll_producer(fixture: &mut Fixture) -> serde_json::Value {
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    let response: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, &fixture.connection),
    )
    .unwrap();
    assert!(response.get("error").is_none());
    response
}

/// A private run handle accompanies inert presentation metadata; it does not
/// contain a PID, pane claim or general RPC selector.
fn callback(response: &serde_json::Value, sequence: u64) -> JsonRpcRequest {
    crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"helper",
        "method":"agent/external/helper-presentation","params":{"launch_token":response["result"]["launch_token"],
        "generation":response["result"]["generation"],"external_session_id":"session-a","sequence":sequence,
        "state":"running","title":"PRIVATE fixture title"}}).to_string()).unwrap()
}

/// Starts a genuine direct child through the normally invoked producer; only
/// nonsecret ordinary MEZ discovery is inherited. Kernel sender evidence is
/// consumed from the helper's own transport, with no injected process identity.
pub(super) async fn child(
    fixture: &mut Fixture,
) -> (tokio::net::UnixStream, ControlConnectionState) {
    fixture.socket.write_all(&[4]).await.unwrap();
    let (mut socket, _) = tokio::time::timeout(Duration::from_secs(10), fixture.listener.accept())
        .await
        .unwrap()
        .unwrap();
    let uid = crate::runtime::current_effective_uid();
    let origin = Arc::new(crate::runtime::capture_unix_origin(socket.as_raw_fd(), uid).unwrap());
    let mut connection = ControlConnectionState::new(true, false);
    connection
        .bind_authenticated_peer(AuthenticatedPeer::unix_user(uid))
        .unwrap();
    connection.bind_unix_origin(origin.clone()).unwrap();
    let mut qualified = crate::runtime::UnixOriginStream::new(&mut socket, Some(origin));
    let mut ready = [0];
    qualified.read_exact(&mut ready).await.unwrap();
    assert_eq!(ready, [1]);
    assert!(connection.unix_origin().unwrap().writer_confirmed());
    drop(qualified);
    (socket, connection)
}

/// A fixture process must actually exit before late-work assertions; polling
/// the retained kernel lifetime avoids treating socket closure as process death.
async fn release(socket: &mut tokio::net::UnixStream, connection: &ControlConnectionState) {
    socket.write_all(&[2]).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while connection.unix_origin().unwrap().is_live() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

/// A real child plus the current run handle can update only existing generic
/// presentation. Helper EOF cannot retire the producer, extend its lease, add
/// an observer, allocate accounting identity or initialize a client. Identical
/// sequence replay is inert and work Debug cannot expose private callback data.
#[tokio::test(flavor = "current_thread")]
async fn external_helper_presentation_native_child_preserves_existing_run_only() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let response = enroll_producer(&mut fixture).await;
    let (mut socket, connection) = child(&mut fixture).await;
    let binding = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap();
    let before = (
        binding.generation,
        binding.accounting_owner.clone(),
        binding.expires,
        binding.enrollment.as_ref().unwrap().observers.len(),
    );
    for expected_changed in [true, false] {
        let work = fixture
            .service
            .prepare_external_helper_presentation(&callback(&response, 1), &connection)
            .unwrap();
        assert!(
            !format!("{work:?}").contains(response["result"]["launch_token"].as_str().unwrap())
        );
        assert!(!format!("{work:?}").contains("PRIVATE fixture title"));
        let native = work.clone();
        let observed = tokio::task::spawn_blocking(move || native.observe())
            .await
            .unwrap();
        let result: serde_json::Value = serde_json::from_str(
            &fixture
                .service
                .complete_external_enrollment(work, observed, &connection),
        )
        .unwrap();
        assert_eq!(result["result"]["changed"], expected_changed);
    }
    socket.write_all(&[2]).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while connection.unix_origin().unwrap().is_live() {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(fixture.service.reconcile_external_agent_registrations(), 0);
    assert_eq!(fixture.service.control.external_agents().bindings.len(), 1);
    let binding = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap();
    assert_eq!(
        (
            binding.generation,
            binding.accounting_owner.clone(),
            binding.expires,
            binding.enrollment.as_ref().unwrap().observers.len()
        ),
        before
    );
    assert!(!binding.retired);
    assert!(!connection.initialized());
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .enrollments
            .pending
            .is_empty()
    );
}

/// The producer itself is not a helper, and a separate pane's genuine child
/// cannot borrow a private handle to attribute observations to the first run.
/// Wrong generation/session and unsupported selectors reject before reservation.
#[tokio::test(flavor = "current_thread")]
async fn external_helper_presentation_rejects_unrelated_parent_and_stale_selectors() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let response = enroll_producer(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_helper_presentation(&callback(&response, 1), &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(observed.is_err());
    assert!(
        fixture
            .service
            .complete_external_enrollment(work, observed, &fixture.connection)
            .contains("error")
    );
    let Some(mut other) = super::fixture("hold").await else {
        return;
    };
    let (_socket, connection) = child(&mut other).await;
    let work = fixture
        .service
        .prepare_external_helper_presentation(&callback(&response, 1), &connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(observed.is_err());
    assert!(
        fixture
            .service
            .complete_external_enrollment(work, observed, &connection)
            .contains("error")
    );
    for (field, value) in [
        ("generation", serde_json::json!(0)),
        ("external_session_id", serde_json::json!("other")),
        ("pane_id", serde_json::json!("%1")),
        ("pid", serde_json::json!(1)),
    ] {
        let mut request = callback(&response, 1);
        let mut params: serde_json::Value =
            serde_json::from_str(request.params.as_deref().unwrap()).unwrap();
        params[field] = value;
        request.params = Some(params.to_string());
        assert!(
            fixture
                .service
                .prepare_external_helper_presentation(&request, &connection)
                .is_err()
        );
    }
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .enrollments
            .pending
            .is_empty()
    );
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .next()
            .unwrap()
            .registration
            .as_ref()
            .unwrap()
            .presentation
            .is_none()
    );
}

/// Successful native evidence is insufficient if the actor's run, root,
/// producer, child, ingress, writer or admission deadline has changed. Every
/// rejected completion releases its reservation and publishes no presentation;
/// replacement remains live and cannot be overwritten by old helper work.
#[tokio::test(flavor = "current_thread")]
async fn external_helper_presentation_rechecks_authority_at_settlement() {
    for change in [
        "deadline",
        "root",
        "producer",
        "child",
        "connection",
        "writer",
        "expired",
        "replacement",
    ] {
        let Some(mut fixture) = fixture("hold").await else {
            return;
        };
        let response = enroll_producer(&mut fixture).await;
        let (mut socket, connection) = child(&mut fixture).await;
        let mut work = fixture
            .service
            .prepare_external_helper_presentation(&callback(&response, 1), &connection)
            .unwrap();
        let native = work.clone();
        let observed = tokio::task::spawn_blocking(move || native.observe())
            .await
            .unwrap();
        assert!(observed.is_ok());
        match change {
            "deadline" => work.deadline = Instant::now(),
            "root" => {
                fixture.service.terminate_all_pane_processes().unwrap();
                fixture.service.start_initial_pane_process(None).unwrap();
            }
            "producer" => release(&mut fixture.socket, &fixture.connection).await,
            "child" => release(&mut socket, &connection).await,
            "writer" => connection.unix_origin().unwrap().record_writer(false),
            "expired" => {
                fixture
                    .service
                    .control
                    .external_agents_mut()
                    .bindings
                    .values_mut()
                    .next()
                    .unwrap()
                    .expires = current_unix_seconds() - 1
            }
            "replacement" => {
                let mut request = request();
                let mut params: serde_json::Value =
                    serde_json::from_str(request.params.as_deref().unwrap()).unwrap();
                params["observer_instance"] = serde_json::json!("replacement");
                params["predecessor_generation"] = response["result"]["generation"].clone();
                request.params = Some(params.to_string());
                let replacement = fixture
                    .service
                    .prepare_external_enrollment(&request, &fixture.connection)
                    .unwrap();
                assert!(
                    settle(&mut fixture, replacement, &connection)
                        .await
                        .get("error")
                        .is_some()
                );
                // The helper cannot settle producer work. Retry through the
                // original qualified producer connection to publish replacement.
                let replacement = fixture
                    .service
                    .prepare_external_enrollment(&request, &fixture.connection)
                    .unwrap();
                let producer_connection = fixture.connection.clone();
                assert!(
                    settle(&mut fixture, replacement, &producer_connection)
                        .await
                        .get("error")
                        .is_none()
                );
            }
            "connection" => {}
            _ => unreachable!(),
        }
        let completion_connection = if change == "connection" {
            &fixture.connection
        } else {
            &connection
        };
        let result: serde_json::Value = serde_json::from_str(
            &fixture
                .service
                .complete_external_enrollment(work, observed, completion_connection),
        )
        .unwrap();
        assert!(
            result.get("error").is_some(),
            "changed {change} authority was accepted"
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
        assert!(
            fixture
                .service
                .control
                .external_agents()
                .bindings
                .values()
                .all(|binding| binding
                    .registration
                    .as_ref()
                    .is_none_or(|registration| registration.presentation.is_none()))
        );
        if change == "replacement" {
            assert!(
                fixture
                    .service
                    .control
                    .external_agents()
                    .bindings
                    .values()
                    .all(|binding| !binding.retired)
            );
        }
    }
}

/// Helpers share the same finite native admission pool as producers. Capacity
/// failure cannot allocate another run, and rejected native observations release
/// every held slot. Generic sequence replay/conflict semantics remain unchanged.
#[tokio::test(flavor = "current_thread")]
async fn external_helper_presentation_capacity_and_sequences_are_bounded() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let response = enroll_producer(&mut fixture).await;
    let (_socket, connection) = child(&mut fixture).await;
    let mut held = Vec::new();
    for _ in 0..MAX_PENDING {
        held.push(
            fixture
                .service
                .prepare_external_helper_presentation(&callback(&response, 2), &connection)
                .unwrap(),
        );
    }
    assert_eq!(
        fixture
            .service
            .prepare_external_enrollment(&request(), &fixture.connection)
            .unwrap_err()
            .kind(),
        crate::error::MezErrorKind::RateLimited
    );
    assert_eq!(
        fixture
            .service
            .prepare_external_helper_presentation(&callback(&response, 2), &connection)
            .unwrap_err()
            .kind(),
        crate::error::MezErrorKind::RateLimited
    );
    for work in held {
        fixture.service.complete_external_enrollment(
            work,
            Err(MezError::conflict("discarded fixture work")),
            &connection,
        );
    }
    for (sequence, changed) in [(2, Some(true)), (2, Some(false)), (1, None), (0, None)] {
        let work = fixture
            .service
            .prepare_external_helper_presentation(&callback(&response, sequence), &connection)
            .unwrap();
        let result = settle(&mut fixture, work, &connection).await;
        match changed {
            Some(changed) => assert_eq!(result["result"]["changed"], changed),
            None => assert!(result.get("error").is_some()),
        }
    }
    let mut conflicting = callback(&response, 2);
    let mut params: serde_json::Value =
        serde_json::from_str(conflicting.params.as_deref().unwrap()).unwrap();
    params["state"] = serde_json::json!("failed");
    conflicting.params = Some(params.to_string());
    let work = fixture
        .service
        .prepare_external_helper_presentation(&conflicting, &connection)
        .unwrap();
    assert!(
        settle(&mut fixture, work, &connection)
            .await
            .get("error")
            .is_some()
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
    assert_eq!(fixture.service.control.external_agents().bindings.len(), 1);
    let binding = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap();
    assert_eq!(
        binding
            .registration
            .as_ref()
            .unwrap()
            .presentation
            .as_ref()
            .unwrap()
            .sequence,
        2
    );
    assert_eq!(
        binding
            .registration
            .as_ref()
            .unwrap()
            .presentation
            .as_ref()
            .unwrap()
            .state,
        "running"
    );
}

/// Production actor dispatch must recognize the helper method, perform native
/// work off actor, and settle once even after the caller abandons its reply.
/// A gated worker leaves lifecycle requests responsive and allocates no client,
/// producer, accounting identity or observer endpoint during callback settlement.
#[tokio::test(flavor = "current_thread")]
async fn external_helper_presentation_actor_settles_lost_reply_off_actor() {
    use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let response = enroll_producer(&mut fixture).await;
    let (_socket, connection) = child(&mut fixture).await;
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let settled = Arc::new(tokio::sync::Notify::new());
    fixture
        .service
        .control
        .external_agents_mut()
        .enrollments
        .worker_gate = Some((started.clone(), release.clone()));
    fixture
        .service
        .control
        .external_agents_mut()
        .enrollments
        .settled = Some(settled.clone());
    let clients = fixture.service.session().clients().len();
    let service = std::mem::replace(&mut fixture.service, RuntimeServiceFixture::new().build());
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let actor_task = tokio::spawn(actor.run());
    let caller = handle.clone();
    let request = callback(&response, 1);
    let body = serde_json::json!({"jsonrpc":"2.0","id":"helper","method":request.method,
        "params":serde_json::from_str::<serde_json::Value>(request.params.as_deref().unwrap()).unwrap()}).to_string();
    let pending = tokio::spawn(async move {
        caller
            .handle_control_input_for_connection(
                crate::control::encode_control_body(&body),
                8192,
                connection,
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), handle.lifecycle_state())
        .await
        .unwrap()
        .unwrap();
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), settled.notified())
        .await
        .unwrap();
    handle.shutdown().await.unwrap();
    let mut exit = actor_task.await.unwrap();
    assert_eq!(exit.service.session().clients().len(), clients);
    assert_eq!(exit.service.control.external_agents().bindings.len(), 1);
    let binding = exit
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap();
    assert_eq!(
        binding
            .registration
            .as_ref()
            .unwrap()
            .presentation
            .as_ref()
            .unwrap()
            .sequence,
        1
    );
    assert_eq!(binding.enrollment.as_ref().unwrap().observers.len(), 1);
    assert!(
        exit.service
            .control
            .external_agents()
            .enrollments
            .pending
            .is_empty()
    );
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Before reservation, the helper ingress rejects unqualified/initialized
/// connections, legacy handles and Pi-owned sequences. Exact metadata bounds
/// and strict unique fields apply even to programmatic callers, with no token
/// or title echoed in diagnostics and no enrollment side effects on rejection.
#[tokio::test(flavor = "current_thread")]
async fn external_helper_presentation_admission_contract_is_strict() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let response = enroll_producer(&mut fixture).await;
    let (socket, connection) = child(&mut fixture).await;
    let request = callback(&response, 1);
    let raw = request.params.clone().unwrap();
    let mut bounded = request.clone();
    bounded.params = Some(format!("{raw}{}", " ".repeat(4096 - raw.len())));
    let work = fixture
        .service
        .prepare_external_helper_presentation(&bounded, &connection)
        .unwrap();
    fixture.service.complete_external_enrollment(
        work,
        Err(MezError::conflict("discarded fixture work")),
        &connection,
    );
    bounded.params.as_mut().unwrap().push(' ');
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&bounded, &connection)
            .is_err()
    );
    for params in [
        format!("{} ,\"state\":\"failed\"}}", raw.trim_end_matches('}')),
        format!(
            "{} ,\"\\u0073tate\":\"failed\"}}",
            raw.trim_end_matches('}')
        ),
    ] {
        let mut duplicate = request.clone();
        duplicate.params = Some(params);
        assert!(
            fixture
                .service
                .prepare_external_helper_presentation(&duplicate, &connection)
                .is_err()
        );
    }
    let mut wrong_method = request.clone();
    wrong_method.method = "agent/external/enroll".into();
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&wrong_method, &connection)
            .is_err()
    );
    let unqualified = Arc::new(
        crate::runtime::capture_unix_origin(
            socket.as_raw_fd(),
            crate::runtime::current_effective_uid(),
        )
        .unwrap(),
    );
    let mut unseen = ControlConnectionState::new(true, false);
    unseen
        .bind_authenticated_peer(AuthenticatedPeer::unix_user(unqualified.uid()))
        .unwrap();
    unseen.bind_unix_origin(unqualified).unwrap();
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&request, &unseen)
            .is_err()
    );
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(
                &request,
                &ControlConnectionState::new(true, false)
            )
            .is_err()
    );
    let primary = fixture
        .service
        .attach_primary(
            "helper contract",
            true,
            mez_mux::layout::Size::new(80, 24).unwrap(),
            120,
        )
        .unwrap();
    let initialized = ControlConnectionState::trusted_existing_client(primary.clone());
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&request, &initialized)
            .is_err()
    );
    let legacy: serde_json::Value = serde_json::from_str(&fixture.service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"legacy","method":"agent/external/launch","params":{"pane_id":"%1","harness":"pi","version":"fixture"}}"#, &primary)).unwrap();
    assert!(legacy.get("error").is_none());
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&callback(&legacy, 1), &connection)
            .unwrap_err()
            .message()
            .contains("independently enrolled")
    );
    let pi = crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"pi", "method":"agent/external/pi-observation","params":{
        "launch_token":response["result"]["launch_token"],"generation":response["result"]["generation"],"external_session_id":"session-a",
        "sequence":1,"event":{"type":"session_start","reason":"startup"}}}).to_string()).unwrap();
    fixture
        .service
        .dispatch_external_agent_request(&pi, &fixture.connection)
        .unwrap();
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&request, &connection)
            .unwrap_err()
            .message()
            .contains("Pi lifecycle")
    );
    connection.unix_origin().unwrap().record_writer(false);
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&request, &connection)
            .is_err()
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
    assert!(!connection.initialized());
}

/// Helper frames use the same exact bounded asynchronous admission as enrollment.
/// Batched/oversized frames cannot reserve work; one exact-limit frame updates
/// the existing registration only. No synchronous control fallback may execute
/// native helper observation or bypass its worker/settlement ownership.
#[tokio::test(flavor = "current_thread")]
async fn external_helper_presentation_actor_frame_boundary_is_exact() {
    use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let response = enroll_producer(&mut fixture).await;
    let (_socket, connection) = child(&mut fixture).await;
    let request = callback(&response, 1);
    let base = serde_json::json!({"jsonrpc":"2.0","id":"helper","method":request.method,
        "params":serde_json::from_str::<serde_json::Value>(request.params.as_deref().unwrap()).unwrap()}).to_string();
    let mut synchronous_connection = connection.clone();
    let synchronous: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .dispatch_runtime_control_body_for_connection(&base, &mut synchronous_connection),
    )
    .unwrap();
    assert!(synchronous.get("error").is_some());
    let service = std::mem::replace(&mut fixture.service, RuntimeServiceFixture::new().build());
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let actor_task = tokio::spawn(actor.run());
    let exact = format!("{base}{}", " ".repeat(8192 - base.len()));
    let oversized = crate::control::encode_control_body(&format!("{exact} "));
    let mut batched = crate::control::encode_control_body(&base);
    batched.extend_from_slice(&crate::control::encode_control_body(&base));
    for frame in [oversized, batched] {
        let result = handle
            .handle_control_input_for_connection(frame, 16384, connection.clone())
            .await
            .unwrap();
        let (body, _) = crate::control::decode_control_frame(&result.output, 16384).unwrap();
        assert!(
            serde_json::from_str::<serde_json::Value>(&body)
                .unwrap()
                .get("error")
                .is_some()
        );
    }
    let result = handle
        .handle_control_input_for_connection(
            crate::control::encode_control_body(&exact),
            16384,
            connection.clone(),
        )
        .await
        .unwrap();
    let (body, _) = crate::control::decode_control_frame(&result.output, 16384).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["result"]["changed"],
        true
    );
    assert!(!result.connection.initialized());
    handle.shutdown().await.unwrap();
    let mut exit = actor_task.await.unwrap();
    assert_eq!(exit.service.control.external_agents().bindings.len(), 1);
    assert!(
        exit.service
            .control
            .external_agents()
            .enrollments
            .pending
            .is_empty()
    );
    exit.service.terminate_all_pane_processes().unwrap();
}
