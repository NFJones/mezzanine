//! Focused actor boundary tests.

use super::coalesce::coalesce_output_side_effects_for_enqueue;
use super::construction::{
    ActorRequestLane, MAX_ACTOR_REQUEST_BURST_DURATION, MAX_INTERACTIVE_REQUEST_BURST,
    actor_request_burst_duration_exhausted, actor_request_lane_capacities, next_actor_request_lane,
    oldest_queued_request_wait_ms,
};
use super::{
    AsyncRuntimeRequest, AsyncRuntimeRequestEnvelope, DEFAULT_PROVIDER_CLAIM_TIMEOUT_MS,
    DEFAULT_PROVIDER_TIMEOUT_MS, RuntimeSideEffect, coalesce_config_persistence_effects,
};
use crate::runtime::{PersistenceTarget, PersistenceWriteMode};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Notify, Semaphore, oneshot};

mod compaction_source_handoff;
mod provider_source_handoff;
mod remember_source_handoff;
mod side_effect_capacity_overshoot;
mod steering_display_retry;
mod title_source_handoff;

/// Cohesive raw connection state must retain the exact notifier already shared
/// with handles, not allocate a private wake source or wake only one waiter.
/// Both idle listeners are registered before actor notification; one metric
/// increment and two ready futures protect the existing broadcast semantics.
#[tokio::test(flavor = "current_thread")]
async fn raw_message_state_preserves_shared_delivery_broadcast() {
    let service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    let (handle, mut actor) =
        super::AsyncRuntimeSessionActor::new(service, super::AsyncRuntimeActorConfig::default())
            .unwrap();
    assert!(Arc::ptr_eq(
        &handle.message_delivery_notify,
        &actor.message_state.delivery_notify,
    ));
    let first = handle.wait_for_message_delivery();
    let second = handle.wait_for_message_delivery();
    tokio::pin!(first, second);
    assert!(futures_util::poll!(first.as_mut()).is_pending());
    assert!(futures_util::poll!(second.as_mut()).is_pending());
    actor.notify_message_delivery();
    assert!(futures_util::poll!(first.as_mut()).is_ready());
    assert!(futures_util::poll!(second.as_mut()).is_ready());
    assert_eq!(actor.metrics.message_delivery_notifications, 1);
    assert!(actor.message_state.raw_identities.is_empty());
}

/// Saturated actor ingress must not lose a disconnect when event completion
/// cancels control already awaiting cleanup, or an outer timeout cancels it.
/// Retirement is exact-client and duplicate teardown leaves a sibling intact.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn event_completion_during_saturated_disconnect_retains_exact_cleanup() {
    for event_completion in [false, true] {
        let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
        let owner = service
            .attach_primary(
                "departing",
                true,
                mez_mux::layout::Size::new(80, 24).unwrap(),
                120,
            )
            .unwrap();
        let sibling = service
            .attach_primary(
                "sibling",
                true,
                mez_mux::layout::Size::new(80, 24).unwrap(),
                120,
            )
            .unwrap();
        let (handle, mut actor) = super::AsyncRuntimeSessionActor::new(
            service,
            super::AsyncRuntimeActorConfig::default(),
        )
        .unwrap();
        let capacity = handle.sender.normal_admission.available_permits();
        let pressure = handle
            .sender
            .normal_admission
            .clone()
            .acquire_many_owned(capacity as u32)
            .await
            .unwrap();
        let mut connection = crate::control::ControlConnectionState::new(true, true);
        connection.rebind_caller_client(owner.clone());
        connection.own_rebound_client_disconnect_for_test();
        let mut cleanup = Box::pin(
            crate::host::async_runtime::submit_control_connection_disconnect_event(
                &handle,
                &mut connection,
            ),
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(1), &mut cleanup)
                .await
                .is_err()
        );
        if event_completion {
            let task = tokio::spawn(async { Err(crate::MezError::invalid_state("event failed")) });
            let mut events = crate::runtime::IrohEventTask::new(Some(task));
            let (result, completed) = events.supervise(async { cleanup.await.map(|_| 0) }).await;
            assert!(completed);
            assert!(result.is_err());
        } else {
            drop(cleanup);
        }
        assert!(connection.take_disconnect_client_id().is_none());
        drop(pressure);
        let cleanup = actor
            .client_clipboard_route_cleanup_rx
            .try_recv()
            .expect("cancelled disconnect must remain actor-owned");
        actor.apply_transport_cancellation_cleanup(cleanup).await;
        assert!(!actor.service.session().is_attached_primary(&owner));
        assert!(actor.service.session().is_attached_primary(&sibling));
        crate::host::async_runtime::submit_control_connection_disconnect_event(
            &handle,
            &mut connection,
        )
        .await
        .unwrap();
        assert!(actor.client_clipboard_route_cleanup_rx.try_recv().is_err());
    }
}

/// A cancelled initialization response must retain exact attachment teardown
/// after the actor has applied the request. Cleanup is queued synchronously by
/// the discarded response lease, and never detaches an unrelated client.
#[tokio::test(flavor = "current_thread")]
async fn routed_initialization_lost_actor_reply_retains_cleanup() {
    let service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    let (_handle, mut actor) =
        super::AsyncRuntimeSessionActor::new(service, super::AsyncRuntimeActorConfig::default())
            .unwrap();
    let (reply, response) = oneshot::channel();
    drop(response);
    actor.handle_request(AsyncRuntimeRequest::HandleControlInput {
        input: crate::control::encode_control_body(
            r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"lost-reply","requested_version":2,"requested_role":"primary","detach_primary_on_disconnect":true,"client":{"name":"lost-reply","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
        ),
        max_content_length: 4096,
        connection: crate::control::ControlConnectionState::new(true, true),
        retain_connection_cleanup: true,
        reply,
    }).await;
    let client_id = actor
        .service
        .session()
        .clients()
        .iter()
        .find(|client| client.name == "lost-reply")
        .unwrap()
        .id
        .clone();
    assert!(actor.service.session().is_attached_primary(&client_id));
    let cleanup = actor.client_clipboard_route_cleanup_rx.try_recv().unwrap();
    actor.apply_transport_cancellation_cleanup(cleanup).await;
    assert!(!actor.service.session().is_attached_primary(&client_id));
    assert!(actor.client_clipboard_route_cleanup_rx.try_recv().is_err());
}

/// Dropping a real hello reply after actor admission must clean the committed
/// raw identity, not every independent registration with the same pane-less
/// shape. A forged retirement request for that independent owner is inert.
#[tokio::test(flavor = "current_thread")]
async fn raw_message_lost_reply_preserves_independent_identity() {
    let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    let independent =
        service
            .message_service_mut()
            .register_agent(None, None, "independent", Vec::new());
    service
        .message_service_mut()
        .subscribe(&independent.agent_id)
        .unwrap();
    let (_handle, mut actor) =
        super::AsyncRuntimeSessionActor::new(service, super::AsyncRuntimeActorConfig::default())
            .unwrap();
    let (owner, identity) = tokio::sync::watch::channel(None);
    let (reply, response) = oneshot::channel();
    drop(response);
    let _ = actor.dispatch_message_request(AsyncRuntimeRequest::HandleMessageInput {
        input: crate::protocol::message::encode_mmp_body(
            r#"{"protocol":"mmp/1","type":"hello","role":"lost-reply"}"#,
        ),
        max_content_length: 4096,
        connection: mez_agent::messaging::MessageConnection::default(),
        now_ms: 10,
        owner: Some(owner),
        reply,
    });
    assert!(
        identity.borrow().is_some(),
        "registration must have actually committed"
    );
    assert!(actor.message_state.raw_identities.is_empty());
    let (reply, response) = oneshot::channel();
    let _ = actor.dispatch_message_request(AsyncRuntimeRequest::RetireMessageConnection {
        agent_id: independent.agent_id.clone(),
        reply,
    });
    response.await.unwrap().unwrap();
    assert_eq!(
        actor.service.message_service().discover_agents(),
        vec![independent.clone()]
    );
    assert!(
        actor
            .service
            .message_service()
            .subscription(&independent.agent_id)
            .is_some()
    );
}

/// Required audit rejection occurs AFTER hello allocates its identity. The
/// critical audit error must remain visible while exact raw ownership cleans
/// that committed registration and leaves an independent owner untouched.
#[tokio::test(flavor = "current_thread")]
async fn raw_message_failed_audit_cleans_committed_hello() {
    let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    let independent =
        service
            .message_service_mut()
            .register_agent(None, None, "independent", Vec::new());
    service
        .message_service_mut()
        .subscribe(&independent.agent_id)
        .unwrap();
    service.set_audit_log(crate::security::audit::AuditLog::new(
        crate::security::audit::AuditConfig {
            enabled: false,
            path: PathBuf::from("/tmp/unused-mmp-audit"),
            hash_chain: false,
            required: true,
        },
    ));
    let (_handle, mut actor) =
        super::AsyncRuntimeSessionActor::new(service, super::AsyncRuntimeActorConfig::default())
            .unwrap();
    let (owner, identity) = tokio::sync::watch::channel(None);
    let (reply, response) = oneshot::channel();
    let _ = actor.dispatch_message_request(AsyncRuntimeRequest::HandleMessageInput {
        input: crate::protocol::message::encode_mmp_body(
            r#"{"protocol":"mmp/1","type":"hello","role":"audit-rejected"}"#,
        ),
        max_content_length: 4096,
        connection: mez_agent::messaging::MessageConnection::default(),
        now_ms: 10,
        owner: Some(owner),
        reply,
    });
    let error = response.await.unwrap().unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
    assert!(
        identity.borrow().is_some(),
        "audit control must follow committed hello"
    );
    assert!(actor.message_state.raw_identities.is_empty());
    assert_eq!(
        actor.service.message_service().discover_agents(),
        vec![independent.clone()]
    );
    assert!(
        actor
            .service
            .message_service()
            .subscription(&independent.agent_id)
            .is_some()
    );
}

/// A tracked raw identity that acquires pane ownership is an integrity failure,
/// not a peer error. Exact cleanup preserves that foreign state and shutdown
/// still retires every healthy later lease before returning the critical error.
#[tokio::test(flavor = "current_thread")]
async fn raw_message_shutdown_reports_integrity_and_cleans_other_leases() {
    let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    let independent =
        service
            .message_service_mut()
            .register_agent(None, None, "independent", Vec::new());
    service
        .message_service_mut()
        .subscribe(&independent.agent_id)
        .unwrap();
    let (_handle, mut actor) =
        super::AsyncRuntimeSessionActor::new(service, super::AsyncRuntimeActorConfig::default())
            .unwrap();
    let mut owners = Vec::new();
    let mut ids = Vec::new();
    for role in ["corrupted", "healthy"] {
        let (owner, identity) = tokio::sync::watch::channel(None);
        let (reply, response) = oneshot::channel();
        let _ = actor.dispatch_message_request(AsyncRuntimeRequest::HandleMessageInput {
            input: crate::protocol::message::encode_mmp_body(&format!(
                r#"{{"protocol":"mmp/1","type":"hello","role":"{role}"}}"#
            )),
            max_content_length: 4096,
            connection: mez_agent::messaging::MessageConnection::default(),
            now_ms: 10,
            owner: Some(owner),
            reply,
        });
        ids.push(
            response
                .await
                .unwrap()
                .unwrap()
                .connection
                .agent_id
                .unwrap(),
        );
        owners.push(identity);
    }
    let mut foreign = actor
        .service
        .message_service()
        .registered_identity(&ids[0])
        .unwrap()
        .clone();
    foreign.pane_id = Some(mez_core::ids::PaneId::new('%', 1));
    actor
        .service
        .message_service_mut()
        .ensure_agent_identity(foreign.clone(), 11)
        .unwrap();
    let before = actor.service.message_service().snapshot_state();
    let (reply, response) = oneshot::channel();
    let _ = actor.dispatch_message_request(AsyncRuntimeRequest::RetireMessageConnection {
        agent_id: ids[0].clone(),
        reply,
    });
    assert!(
        response
            .await
            .unwrap()
            .unwrap_err()
            .message()
            .contains("ownership changed")
    );
    assert_eq!(actor.service.message_service().snapshot_state(), before);
    let (reply, response) = oneshot::channel();
    assert!(actor.dispatch_message_request(AsyncRuntimeRequest::Shutdown { reply }));
    assert!(
        response
            .await
            .unwrap()
            .unwrap_err()
            .message()
            .contains("ownership changed")
    );
    assert_eq!(
        actor.service.message_service().registered_identity(&ids[0]),
        Some(&foreign)
    );
    assert!(
        actor
            .service
            .message_service()
            .subscription(&ids[0])
            .is_some()
    );
    assert!(
        actor
            .service
            .message_service()
            .registered_identity(&ids[1])
            .is_none()
    );
    assert!(
        actor
            .service
            .message_service()
            .subscription(&independent.agent_id)
            .is_some()
    );
    assert_eq!(actor.message_state.raw_identities.len(), 1);
    drop(owners);
}

/// Verifies that the provider worker watchdog cannot fire before the
/// provider transport timeout. The watchdog cleans up abandoned async
/// claims, so it must leave enough time for a legitimate long-running
/// provider request to settle through the provider layer first.
#[test]
fn provider_claim_timeout_exceeds_provider_transport_timeout() {
    let claim_timeout_ms = std::hint::black_box(DEFAULT_PROVIDER_CLAIM_TIMEOUT_MS);
    let provider_timeout_ms = std::hint::black_box(DEFAULT_PROVIDER_TIMEOUT_MS);

    assert!(
        claim_timeout_ms > provider_timeout_ms,
        "provider claim watchdog {} ms must exceed provider timeout {} ms",
        claim_timeout_ms,
        provider_timeout_ms
    );
}

/// Verifies repeated deferred config replacements for the same destination
/// collapse to the newest complete document.
///
/// A single provider response can contain many `config_change` actions for
/// adjacent theme slots. The actor should persist only the final config text
/// for each file instead of queueing a long series of superseded full-file
/// replacements.
#[test]
fn coalesce_config_persistence_effects_keeps_latest_text_per_target() {
    let config_path = PathBuf::from("/tmp/mez/config.toml");
    let project_path = PathBuf::from("/tmp/project/.mezzanine/config.toml");

    let coalesced = coalesce_config_persistence_effects(vec![
        RuntimeSideEffect::Persist {
            target: PersistenceTarget::Config,
            path: config_path.clone(),
            bytes: b"first".to_vec(),
            mode: PersistenceWriteMode::Replace,
        },
        RuntimeSideEffect::Persist {
            target: PersistenceTarget::ProjectConfig,
            path: project_path.clone(),
            bytes: b"project".to_vec(),
            mode: PersistenceWriteMode::Replace,
        },
        RuntimeSideEffect::Persist {
            target: PersistenceTarget::Config,
            path: config_path.clone(),
            bytes: b"second".to_vec(),
            mode: PersistenceWriteMode::Replace,
        },
    ]);

    assert_eq!(coalesced.len(), 2);
    assert!(matches!(
        &coalesced[0],
        RuntimeSideEffect::Persist { target: PersistenceTarget::Config, path, bytes, .. }
            if path == &config_path && bytes == b"second"
    ));
    assert!(matches!(
        &coalesced[1],
        RuntimeSideEffect::Persist { target: PersistenceTarget::ProjectConfig, path, bytes, .. }
            if path == &project_path && bytes == b"project"
    ));
}

/// Verifies render bursts retain only one level-triggered request for the
/// actor to claim and admit due pane-status providers.
#[test]
fn pane_status_provider_preparation_signals_are_coalesced() {
    let mut queued = VecDeque::from([RuntimeSideEffect::PreparePaneStatusProviders]);
    let mut routes = super::routes::RuntimeSideEffectRouter::default();
    let (retained, coalesced) = coalesce_output_side_effects_for_enqueue(
        &mut queued,
        &mut routes,
        vec![
            RuntimeSideEffect::PreparePaneStatusProviders,
            RuntimeSideEffect::PreparePaneStatusProviders,
        ],
    );

    assert!(retained.is_empty());
    assert_eq!(coalesced, 2);
    assert_eq!(queued.len(), 1);
}

/// Verifies queued interactive requests preempt ordinary actor work, but a
/// sustained input burst deterministically advances normal and maintenance
/// lanes instead of allowing strict-priority starvation.
#[test]
fn actor_request_scheduler_prioritizes_input_with_bounded_downstream_progress() {
    for burst in 0..MAX_INTERACTIVE_REQUEST_BURST {
        assert_eq!(
            next_actor_request_lane(false, true, true, true, burst, 0, false, false),
            Some(ActorRequestLane::Interactive)
        );
    }
    assert_eq!(
        next_actor_request_lane(
            false,
            true,
            true,
            true,
            MAX_INTERACTIVE_REQUEST_BURST,
            0,
            false,
            false
        ),
        Some(ActorRequestLane::Normal)
    );
    assert_eq!(
        next_actor_request_lane(
            false,
            true,
            true,
            true,
            MAX_INTERACTIVE_REQUEST_BURST,
            MAX_INTERACTIVE_REQUEST_BURST,
            false,
            false,
        ),
        Some(ActorRequestLane::Maintenance)
    );
    assert_eq!(
        next_actor_request_lane(true, true, true, true, 0, 0, false, false),
        Some(ActorRequestLane::Urgent)
    );
    assert_eq!(
        next_actor_request_lane(false, true, true, false, 1, 0, true, false),
        Some(ActorRequestLane::Normal)
    );
    assert_eq!(
        next_actor_request_lane(false, false, true, true, 0, 1, false, true),
        Some(ActorRequestLane::Maintenance)
    );
    assert!(MAX_ACTOR_REQUEST_BURST_DURATION.as_millis() > 0);
    assert!(!actor_request_burst_duration_exhausted(
        MAX_ACTOR_REQUEST_BURST_DURATION.saturating_sub(Duration::from_millis(1))
    ));
    assert!(actor_request_burst_duration_exhausted(
        MAX_ACTOR_REQUEST_BURST_DURATION
    ));
}

/// Verifies lane admission retains the configured shared mailbox bound.
#[test]
fn actor_request_scheduler_preserves_configured_mailbox_bound() {
    assert_eq!(actor_request_lane_capacities(0), None);
    assert_eq!(actor_request_lane_capacities(3), None);
    assert_eq!(actor_request_lane_capacities(4), Some((1, 1, 1, 1)));
    assert_eq!(actor_request_lane_capacities(64), Some((1, 16, 39, 8)));
}

/// Verifies normal backlog cannot consume the reserved interactive or urgent
/// admission positions before the fair actor scheduler observes those lanes.
#[test]
fn actor_request_scheduler_reserves_interactive_and_urgent_admission() {
    let (urgent, interactive, normal, maintenance) = actor_request_lane_capacities(64).unwrap();
    assert_eq!(urgent + interactive + normal + maintenance, 64);
    assert!(urgent > 0);
    assert!(interactive > 0);
    assert!(normal > 0);
    assert!(maintenance > 0);
}

/// Verifies a full normal lane cannot consume the independently reserved
/// interactive or urgent admission slots before fair scheduling observes them.
#[tokio::test(flavor = "current_thread")]
async fn actor_request_scheduler_admits_interactive_and_urgent_after_normal_saturation() {
    let (urgent_capacity, interactive_capacity, normal_capacity, maintenance_capacity) =
        actor_request_lane_capacities(4).unwrap();
    let ingress = Arc::new(Mutex::new(Default::default()));
    let sender = crate::host::async_runtime::config::AsyncRuntimeRequestSender {
        ingress: ingress.clone(),
        ingress_notify: Arc::new(Notify::new()),
        closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        urgent_admission: Arc::new(Semaphore::new(urgent_capacity)),
        interactive_admission: Arc::new(Semaphore::new(interactive_capacity)),
        normal_admission: Arc::new(Semaphore::new(normal_capacity)),
        maintenance_admission: Arc::new(Semaphore::new(maintenance_capacity)),
    };

    let (normal_reply, _) = oneshot::channel();
    sender
        .try_send(AsyncRuntimeRequestEnvelope::new(
            AsyncRuntimeRequest::LifecycleState {
                reply: normal_reply,
            },
        ))
        .unwrap();
    let (overflow_reply, _) = oneshot::channel();
    assert!(
        sender
            .try_send(AsyncRuntimeRequestEnvelope::new(
                AsyncRuntimeRequest::LifecycleState {
                    reply: overflow_reply,
                },
            ))
            .is_err()
    );

    let (interactive_reply, _) = oneshot::channel();
    sender
        .try_send(AsyncRuntimeRequestEnvelope::new(
            AsyncRuntimeRequest::ManagedShellLifecycleState {
                pane_id: "%1".to_string(),
                reply: interactive_reply,
            },
        ))
        .unwrap();
    let (urgent_reply, _) = oneshot::channel();
    sender
        .try_send(AsyncRuntimeRequestEnvelope::new(
            AsyncRuntimeRequest::Shutdown {
                reply: urgent_reply,
            },
        ))
        .unwrap();

    let ingress = ingress.lock().unwrap();
    assert_eq!(
        ingress.normal_requests.front().unwrap().envelope.lane,
        ActorRequestLane::Normal
    );
    assert_eq!(
        ingress.interactive_requests.front().unwrap().envelope.lane,
        ActorRequestLane::Interactive
    );
    assert_eq!(
        ingress.urgent_requests.front().unwrap().envelope.lane,
        ActorRequestLane::Urgent
    );
}

/// Verifies closing admission rejects a request even if it previously acquired
/// lane capacity, preventing a post-shutdown request from waiting for a reply
/// after the actor has stopped consuming its ingress.
#[test]
fn actor_request_scheduler_rejects_admission_after_close() {
    let (urgent_capacity, interactive_capacity, normal_capacity, maintenance_capacity) =
        actor_request_lane_capacities(4).unwrap();
    let ingress = Arc::new(Mutex::new(Default::default()));
    let sender = crate::host::async_runtime::config::AsyncRuntimeRequestSender {
        ingress: ingress.clone(),
        ingress_notify: Arc::new(Notify::new()),
        closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        urgent_admission: Arc::new(Semaphore::new(urgent_capacity)),
        interactive_admission: Arc::new(Semaphore::new(interactive_capacity)),
        normal_admission: Arc::new(Semaphore::new(normal_capacity)),
        maintenance_admission: Arc::new(Semaphore::new(maintenance_capacity)),
    };
    let (admitted_reply, _) = oneshot::channel();
    sender
        .try_send(AsyncRuntimeRequestEnvelope::new(
            AsyncRuntimeRequest::LifecycleState {
                reply: admitted_reply,
            },
        ))
        .unwrap();
    sender.close();
    assert!(ingress.lock().unwrap().normal_requests.is_empty());
    let (reply, _) = oneshot::channel();
    assert!(
        sender
            .try_send(AsyncRuntimeRequestEnvelope::new(
                AsyncRuntimeRequest::LifecycleState { reply },
            ))
            .is_err()
    );
}

/// Verifies scheduler diagnostics report real elapsed age for queued work.
#[test]
fn actor_request_scheduler_reports_oldest_queued_age() {
    let (reply, _) = oneshot::channel();
    let request = AsyncRuntimeRequestEnvelope::new(AsyncRuntimeRequest::LifecycleState { reply });
    std::thread::sleep(Duration::from_millis(2));
    assert!(oldest_queued_request_wait_ms(None, None, Some(&request), None) >= 1);
}

/// The real actor loop must route ordinary requests exactly once and retain
/// the common metrics suffix while MMP and shutdown use synchronous ownership.
/// A lifecycle reply and metrics observation precede shutdown; the final count
/// proves no accepted envelope was dropped or replayed by discriminant routing.
#[tokio::test(flavor = "current_thread")]
async fn message_predispatch_preserves_non_message_request_without_effects() {
    let service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    let (handle, actor) =
        super::AsyncRuntimeSessionActor::new(service, super::AsyncRuntimeActorConfig::default())
            .unwrap();
    let client = async {
        let lifecycle = handle.lifecycle_state().await.unwrap();
        assert_eq!(handle.metrics().await.unwrap().commands_processed, 2);
        assert_eq!(handle.shutdown().await.unwrap(), lifecycle);
    };
    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.commands_processed, 3);
    assert_eq!(exit.metrics.commands_processed, 3);
}
