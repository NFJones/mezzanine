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
