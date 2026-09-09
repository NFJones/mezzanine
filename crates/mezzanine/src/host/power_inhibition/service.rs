//! Nonblocking latest-value service for host power inhibition.
//!
//! Runtime actors publish only desired modes through a cloneable handle. One
//! blocking worker owns the platform controller and performs every native
//! acquire or release outside actor execution. Desired values are monotonic,
//! duplicate modes are ignored, and intermediate changes may coalesce before
//! the worker observes them. Confirmed snapshots remain distinct from desired
//! state so callers never mistake a queued or failed request for held host
//! resources. Failed reconciliation retries with bounded exponential backoff;
//! a newer generation interrupts that wait, although an in-progress synchronous
//! backend call remains subject to adapter-owned deadlines and is not cancelled.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::{
    PowerInhibitionBackend, PowerInhibitionBackendKind, PowerInhibitionController,
    PowerInhibitionErrorClass, PowerInhibitionMode, PowerInhibitionResourceState,
    PowerInhibitionState,
};

const PRODUCTION_INITIAL_RETRY_DELAY: Duration = Duration::from_secs(1);
const PRODUCTION_MAX_RETRY_DELAY: Duration = Duration::from_secs(60);
const PRODUCTION_HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy)]
struct PowerInhibitionRetryPolicy {
    initial_delay: Duration,
    max_delay: Duration,
    health_check_interval: Duration,
}

impl Default for PowerInhibitionRetryPolicy {
    fn default() -> Self {
        Self {
            initial_delay: PRODUCTION_INITIAL_RETRY_DELAY,
            max_delay: PRODUCTION_MAX_RETRY_DELAY,
            health_check_interval: PRODUCTION_HEALTH_CHECK_INTERVAL,
        }
    }
}

impl PowerInhibitionRetryPolicy {
    fn next_delay(self, current: Duration) -> Duration {
        current.saturating_mul(2).min(self.max_delay)
    }
}

/// Latest desired and confirmed state for one session power worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PowerInhibitionSnapshot {
    /// Monotonic generation of the latest actor-published mode.
    pub(crate) desired_generation: u64,
    /// Latest mode requested by the runtime actor.
    pub(crate) desired_mode: PowerInhibitionMode,
    /// Latest desired generation fully reconciled by the worker.
    pub(crate) confirmed_generation: u64,
    /// Mode targeted by the latest completed worker reconciliation.
    pub(crate) confirmed_mode: PowerInhibitionMode,
    /// Native adapter kind owned by the worker.
    pub(crate) backend_kind: PowerInhibitionBackendKind,
    /// Aggregate state confirmed after the latest reconciliation.
    pub(crate) state: PowerInhibitionState,
    /// Confirmed ownership of the system-sleep resource.
    pub(crate) system_resource: PowerInhibitionResourceState,
    /// Confirmed ownership of the display-sleep resource.
    pub(crate) display_resource: PowerInhibitionResourceState,
    /// Bounded classification of the latest nonfatal backend failure.
    pub(crate) last_error: Option<PowerInhibitionErrorClass>,
}

impl PowerInhibitionSnapshot {
    fn initial(backend_kind: PowerInhibitionBackendKind) -> Self {
        Self {
            desired_generation: 0,
            desired_mode: PowerInhibitionMode::Disabled,
            confirmed_generation: 0,
            confirmed_mode: PowerInhibitionMode::Disabled,
            backend_kind,
            state: PowerInhibitionState::Inactive,
            system_resource: PowerInhibitionResourceState::NotHeld,
            display_resource: PowerInhibitionResourceState::NotHeld,
            last_error: None,
        }
    }
}

#[derive(Debug)]
struct PowerInhibitionSharedState {
    snapshot: PowerInhibitionSnapshot,
    publishers: usize,
    closed: bool,
}

#[derive(Debug)]
struct PowerInhibitionShared {
    state: Mutex<PowerInhibitionSharedState>,
    changed: Condvar,
}

fn lock_shared(shared: &PowerInhibitionShared) -> MutexGuard<'_, PowerInhibitionSharedState> {
    shared
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Cloneable nonblocking publisher and latest-snapshot reader for one session.
///
/// Publishing holds only the small in-memory state lock; native backend work
/// is exclusively worker-owned. Dropping the final handle publishes one
/// terminal `Disabled` value so retained resources are cleaned up before the
/// worker exits.
#[derive(Debug)]
pub(crate) struct PowerInhibitionHandle {
    shared: Arc<PowerInhibitionShared>,
}

impl Clone for PowerInhibitionHandle {
    fn clone(&self) -> Self {
        let mut state = lock_shared(&self.shared);
        state.publishers = state.publishers.saturating_add(1);
        drop(state);
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl PowerInhibitionHandle {
    /// Publishes a changed desired mode and returns its monotonic generation.
    ///
    /// Repeated publication of the current mode returns the existing
    /// generation without waking the worker or restarting backend work.
    pub(crate) fn publish(&self, mode: PowerInhibitionMode) -> u64 {
        let mut state = lock_shared(&self.shared);
        if state.closed {
            return state.snapshot.desired_generation;
        }
        if state.snapshot.desired_mode == mode {
            return state.snapshot.desired_generation;
        }
        state.snapshot.desired_generation = state.snapshot.desired_generation.saturating_add(1);
        state.snapshot.desired_mode = mode;
        let generation = state.snapshot.desired_generation;
        self.shared.changed.notify_one();
        generation
    }

    /// Returns the latest desired and confirmed service state without host I/O.
    pub(crate) fn snapshot(&self) -> PowerInhibitionSnapshot {
        lock_shared(&self.shared).snapshot
    }

    /// Closes this session service after publishing terminal `Disabled`.
    ///
    /// Later publications are ignored. The worker exits only after it has
    /// reconciled the terminal generation and published its confirmed state.
    pub(crate) fn close(&self) {
        let mut state = lock_shared(&self.shared);
        if state.closed {
            return;
        }
        state.closed = true;
        if state.snapshot.desired_mode != PowerInhibitionMode::Disabled {
            state.snapshot.desired_generation = state.snapshot.desired_generation.saturating_add(1);
            state.snapshot.desired_mode = PowerInhibitionMode::Disabled;
        }
        self.shared.changed.notify_one();
    }
}

impl Drop for PowerInhibitionHandle {
    fn drop(&mut self) {
        let mut state = lock_shared(&self.shared);
        state.publishers = state.publishers.saturating_sub(1);
        if state.publishers == 0 {
            state.closed = true;
            if state.snapshot.desired_mode != PowerInhibitionMode::Disabled {
                state.snapshot.desired_generation =
                    state.snapshot.desired_generation.saturating_add(1);
                state.snapshot.desired_mode = PowerInhibitionMode::Disabled;
            }
            self.shared.changed.notify_one();
        }
    }
}

/// Final outcome of a power worker after terminal cleanup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PowerInhibitionWorkerReport {
    /// Number of reconciliation attempts, including periodic health checks.
    pub(crate) work_units: u64,
    /// Final snapshot after the last publisher requested terminal cleanup.
    pub(crate) final_snapshot: PowerInhibitionSnapshot,
}

/// Blocking one-session worker that exclusively owns native power resources.
#[derive(Debug)]
pub(crate) struct PowerInhibitionWorker<B: PowerInhibitionBackend> {
    shared: Arc<PowerInhibitionShared>,
    controller: PowerInhibitionController<B>,
    retry_policy: PowerInhibitionRetryPolicy,
}

impl<B: PowerInhibitionBackend> PowerInhibitionWorker<B> {
    /// Runs until every handle is dropped and terminal `Disabled` cleanup is
    /// confirmed. Host backend latency cannot block any publishing caller.
    pub(crate) fn run(mut self) -> PowerInhibitionWorkerReport {
        let mut settled_generation = 0;
        let mut work_units = 0_u64;
        let mut retry_delay = self.retry_policy.initial_delay;
        loop {
            let desired = {
                let mut state = lock_shared(&self.shared);
                while state.snapshot.desired_generation <= settled_generation && !state.closed {
                    let (next_state, wait) = self
                        .shared
                        .changed
                        .wait_timeout(state, self.retry_policy.health_check_interval)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state = next_state;
                    if wait.timed_out() {
                        break;
                    }
                }
                if state.closed
                    && state.snapshot.desired_generation <= settled_generation
                    && self.controller.satisfies(PowerInhibitionMode::Disabled)
                {
                    break;
                }
                (
                    state.snapshot.desired_generation,
                    state.snapshot.desired_mode,
                )
            };

            self.controller.reconcile(desired.1);
            work_units = work_units.saturating_add(1);
            let satisfied = self.controller.satisfies(desired.1);

            let mut state = lock_shared(&self.shared);
            if satisfied && desired.0 >= state.snapshot.confirmed_generation {
                state.snapshot.confirmed_generation = desired.0;
                state.snapshot.confirmed_mode = desired.1;
            }
            state.snapshot.state = self.controller.state();
            state.snapshot.system_resource = self.controller.system_resource_state();
            state.snapshot.display_resource = self.controller.display_resource_state();
            state.snapshot.last_error = self.controller.last_error_class();
            self.shared.changed.notify_all();
            let terminal_cleanup_confirmed = state.closed
                && state.snapshot.desired_mode == PowerInhibitionMode::Disabled
                && state.snapshot.desired_generation == desired.0
                && satisfied;
            if terminal_cleanup_confirmed {
                break;
            }

            if state.snapshot.desired_generation != desired.0 {
                retry_delay = self.retry_policy.initial_delay;
                drop(state);
                continue;
            }

            if satisfied {
                settled_generation = desired.0;
                retry_delay = self.retry_policy.initial_delay;
                drop(state);
                continue;
            }

            let retry_deadline = Instant::now() + retry_delay;
            while state.snapshot.desired_generation == desired.0 {
                let now = Instant::now();
                if now >= retry_deadline {
                    break;
                }
                let (next_state, _) = self
                    .shared
                    .changed
                    .wait_timeout(state, retry_deadline.saturating_duration_since(now))
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state = next_state;
            }
            if state.snapshot.desired_generation == desired.0 {
                retry_delay = self.retry_policy.next_delay(retry_delay);
            } else {
                retry_delay = self.retry_policy.initial_delay;
            }
        }

        PowerInhibitionWorkerReport {
            work_units,
            final_snapshot: lock_shared(&self.shared).snapshot,
        }
    }
}

/// Creates one isolated latest-value service around a platform controller.
pub(crate) fn power_inhibition_service<B: PowerInhibitionBackend>(
    controller: PowerInhibitionController<B>,
) -> (PowerInhibitionHandle, PowerInhibitionWorker<B>) {
    power_inhibition_service_with_retry_policy(controller, PowerInhibitionRetryPolicy::default())
}

fn power_inhibition_service_with_retry_policy<B: PowerInhibitionBackend>(
    controller: PowerInhibitionController<B>,
    retry_policy: PowerInhibitionRetryPolicy,
) -> (PowerInhibitionHandle, PowerInhibitionWorker<B>) {
    let shared = Arc::new(PowerInhibitionShared {
        state: Mutex::new(PowerInhibitionSharedState {
            snapshot: PowerInhibitionSnapshot::initial(controller.backend_kind()),
            publishers: 1,
            closed: false,
        }),
        changed: Condvar::new(),
    });
    (
        PowerInhibitionHandle {
            shared: Arc::clone(&shared),
        },
        PowerInhibitionWorker {
            shared,
            controller,
            retry_policy,
        },
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use super::*;
    use crate::host::power_inhibition::{PowerInhibitionLease, PowerInhibitionResource};

    #[derive(Debug, Default)]
    struct FakeState {
        calls: Mutex<Vec<String>>,
        block_acquire: AtomicBool,
        block_successful_system_acquire: AtomicBool,
        entered_acquire: AtomicBool,
        system_acquire_failures: AtomicUsize,
        display_acquire_failures: AtomicUsize,
        display_health_failures: AtomicUsize,
        release_failures: AtomicUsize,
        gate: Condvar,
        gate_lock: Mutex<()>,
    }

    impl FakeState {
        fn fail_next(counter: &AtomicUsize) -> bool {
            counter
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[derive(Debug)]
    struct FakeLease {
        state: Arc<FakeState>,
        resource: PowerInhibitionResource,
        released: bool,
    }

    impl PowerInhibitionLease for FakeLease {
        fn release(&mut self) -> std::result::Result<(), String> {
            if !self.released {
                self.state
                    .calls
                    .lock()
                    .unwrap()
                    .push(format!("release:{:?}", self.resource));
                if FakeState::fail_next(&self.state.release_failures) {
                    return Err("release unavailable".to_string());
                }
                self.released = true;
            }
            Ok(())
        }

        fn is_held(&self) -> bool {
            !self.released
        }

        fn health_check(&mut self) -> std::result::Result<(), String> {
            if self.resource == PowerInhibitionResource::Display
                && FakeState::fail_next(&self.state.display_health_failures)
            {
                self.released = true;
                return Err("display owner changed".to_string());
            }
            Ok(())
        }
    }

    #[derive(Debug)]
    struct FakeBackend {
        state: Arc<FakeState>,
    }

    impl PowerInhibitionBackend for FakeBackend {
        fn kind(&self) -> PowerInhibitionBackendKind {
            PowerInhibitionBackendKind::Test
        }

        fn acquire(
            &mut self,
            resource: PowerInhibitionResource,
        ) -> std::result::Result<Box<dyn PowerInhibitionLease>, String> {
            self.state
                .calls
                .lock()
                .unwrap()
                .push(format!("acquire:{resource:?}"));
            self.state.entered_acquire.store(true, Ordering::Release);
            if self.state.block_acquire.load(Ordering::Acquire) {
                let guard = self.state.gate_lock.lock().unwrap();
                let _guard = self
                    .state
                    .gate
                    .wait_while(guard, |_| self.state.block_acquire.load(Ordering::Acquire))
                    .unwrap();
            }
            let failures = match resource {
                PowerInhibitionResource::System => &self.state.system_acquire_failures,
                PowerInhibitionResource::Display => &self.state.display_acquire_failures,
            };
            if FakeState::fail_next(failures) {
                return Err(format!("{resource:?} acquisition unavailable"));
            }
            if resource == PowerInhibitionResource::System
                && self
                    .state
                    .block_successful_system_acquire
                    .load(Ordering::Acquire)
            {
                let guard = self.state.gate_lock.lock().unwrap();
                let _guard = self
                    .state
                    .gate
                    .wait_while(guard, |_| {
                        self.state
                            .block_successful_system_acquire
                            .load(Ordering::Acquire)
                    })
                    .unwrap();
            }
            Ok(Box::new(FakeLease {
                state: Arc::clone(&self.state),
                resource,
                released: false,
            }))
        }
    }

    fn fake_service(
        block_acquire: bool,
    ) -> (
        Arc<FakeState>,
        PowerInhibitionHandle,
        PowerInhibitionWorker<FakeBackend>,
    ) {
        let state = Arc::new(FakeState::default());
        state.block_acquire.store(block_acquire, Ordering::Release);
        let controller = PowerInhibitionController::new(FakeBackend {
            state: Arc::clone(&state),
        });
        let (handle, worker) = power_inhibition_service_with_retry_policy(
            controller,
            PowerInhibitionRetryPolicy {
                initial_delay: Duration::from_millis(5),
                max_delay: Duration::from_millis(20),
                health_check_interval: Duration::from_secs(5),
            },
        );
        (state, handle, worker)
    }

    fn fake_service_with_retry_delay(
        retry_delay: Duration,
    ) -> (
        Arc<FakeState>,
        PowerInhibitionHandle,
        PowerInhibitionWorker<FakeBackend>,
    ) {
        let state = Arc::new(FakeState::default());
        let controller = PowerInhibitionController::new(FakeBackend {
            state: Arc::clone(&state),
        });
        let (handle, worker) = power_inhibition_service_with_retry_policy(
            controller,
            PowerInhibitionRetryPolicy {
                initial_delay: retry_delay,
                max_delay: retry_delay,
                health_check_interval: Duration::from_secs(5),
            },
        );
        (state, handle, worker)
    }

    fn fake_service_with_health_check_interval(
        health_check_interval: Duration,
    ) -> (
        Arc<FakeState>,
        PowerInhibitionHandle,
        PowerInhibitionWorker<FakeBackend>,
    ) {
        let state = Arc::new(FakeState::default());
        let controller = PowerInhibitionController::new(FakeBackend {
            state: Arc::clone(&state),
        });
        let (handle, worker) = power_inhibition_service_with_retry_policy(
            controller,
            PowerInhibitionRetryPolicy {
                initial_delay: Duration::from_millis(5),
                max_delay: Duration::from_millis(20),
                health_check_interval,
            },
        );
        (state, handle, worker)
    }

    /// Verifies cloned handles observe one latest value and that dropping one
    /// publisher cannot close the worker while another publisher remains.
    #[test]
    fn cloned_handles_share_latest_value_and_worker_lifetime() {
        let (_state, handle, worker) = fake_service(false);
        let cloned = handle.clone();
        let worker = std::thread::spawn(move || worker.run());

        assert_eq!(handle.publish(PowerInhibitionMode::System), 1);
        wait_until(|| cloned.snapshot().confirmed_generation == 1);
        drop(handle);
        assert_eq!(cloned.publish(PowerInhibitionMode::SystemAndDisplay), 2);
        wait_until(|| cloned.snapshot().confirmed_generation == 2);
        assert_eq!(
            cloned.snapshot().state,
            PowerInhibitionState::SystemAndDisplay
        );

        drop(cloned);
        let report = worker.join().unwrap();
        assert_eq!(
            report.final_snapshot.confirmed_mode,
            PowerInhibitionMode::Disabled
        );
        assert_eq!(report.final_snapshot.state, PowerInhibitionState::Inactive);
    }

    fn wait_until(mut predicate: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !predicate() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for power worker"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Verifies duplicate modes retain one generation and queued changed modes
    /// coalesce to the latest value before the worker starts.
    #[test]
    fn service_deduplicates_and_coalesces_changed_values() {
        let (state, handle, worker) = fake_service(false);
        assert_eq!(handle.publish(PowerInhibitionMode::System), 1);
        assert_eq!(handle.publish(PowerInhibitionMode::System), 1);
        assert_eq!(handle.publish(PowerInhibitionMode::SystemAndDisplay), 2);
        assert_eq!(handle.publish(PowerInhibitionMode::System), 3);

        let worker = std::thread::spawn(move || worker.run());
        wait_until(|| handle.snapshot().confirmed_generation == 3);
        drop(handle);
        let report = worker.join().unwrap();

        assert_eq!(report.work_units, 2);
        assert_eq!(
            *state.calls.lock().unwrap(),
            ["acquire:System", "release:System"]
        );
    }

    /// Verifies a blocked native acquire leaves desired state newer than the
    /// typed confirmed resource snapshot until worker reconciliation finishes.
    #[test]
    fn snapshot_separates_desired_from_confirmed_state() {
        let (state, handle, worker) = fake_service(true);
        let worker = std::thread::spawn(move || worker.run());
        handle.publish(PowerInhibitionMode::System);
        wait_until(|| state.entered_acquire.load(Ordering::Acquire));

        let pending = handle.snapshot();
        assert_eq!(pending.desired_generation, 1);
        assert_eq!(pending.desired_mode, PowerInhibitionMode::System);
        assert_eq!(pending.confirmed_generation, 0);
        assert_eq!(pending.confirmed_mode, PowerInhibitionMode::Disabled);
        assert_eq!(pending.backend_kind, PowerInhibitionBackendKind::Test);
        assert_eq!(
            pending.system_resource,
            PowerInhibitionResourceState::NotHeld
        );

        state.block_acquire.store(false, Ordering::Release);
        state.gate.notify_all();
        wait_until(|| handle.snapshot().confirmed_generation == 1);
        let confirmed = handle.snapshot();
        assert_eq!(confirmed.state, PowerInhibitionState::System);
        assert_eq!(
            confirmed.system_resource,
            PowerInhibitionResourceState::Held
        );
        drop(handle);
        let _ = worker.join().unwrap();
    }

    /// Verifies dropping the final publisher queues and confirms terminal
    /// `Disabled` cleanup in display-first, system-second order.
    #[test]
    fn final_handle_drop_confirms_terminal_disabled_cleanup() {
        let (state, handle, worker) = fake_service(false);
        let worker = std::thread::spawn(move || worker.run());
        handle.publish(PowerInhibitionMode::SystemAndDisplay);
        wait_until(|| handle.snapshot().confirmed_generation == 1);
        drop(handle);

        let report = worker.join().unwrap();
        assert_eq!(
            report.final_snapshot.desired_mode,
            PowerInhibitionMode::Disabled
        );
        assert_eq!(
            report.final_snapshot.confirmed_mode,
            PowerInhibitionMode::Disabled
        );
        assert_eq!(report.final_snapshot.state, PowerInhibitionState::Inactive);
        assert_eq!(
            *state.calls.lock().unwrap(),
            [
                "acquire:System",
                "acquire:Display",
                "release:Display",
                "release:System",
            ]
        );
    }

    /// Verifies publishing remains bounded while the worker is blocked inside
    /// a native backend call, preserving actor responsiveness.
    #[test]
    fn publishing_does_not_wait_for_blocked_backend_work() {
        let (state, handle, worker) = fake_service(true);
        let worker = std::thread::spawn(move || worker.run());
        handle.publish(PowerInhibitionMode::System);
        wait_until(|| state.entered_acquire.load(Ordering::Acquire));

        let started = Instant::now();
        assert_eq!(handle.publish(PowerInhibitionMode::SystemAndDisplay), 2);
        assert!(started.elapsed() < Duration::from_millis(100));

        state.block_acquire.store(false, Ordering::Release);
        state.gate.notify_all();
        wait_until(|| handle.snapshot().confirmed_generation == 2);
        drop(handle);
        let _ = worker.join().unwrap();
    }

    /// Verifies a failed system acquisition remains unconfirmed and is retried
    /// until the unchanged desired generation is actually satisfied.
    #[test]
    fn failed_acquisition_retries_until_desired_generation_is_satisfied() {
        let (state, handle, worker) = fake_service(false);
        state.system_acquire_failures.store(1, Ordering::Release);
        state
            .block_successful_system_acquire
            .store(true, Ordering::Release);
        let worker = std::thread::spawn(move || worker.run());

        assert_eq!(handle.publish(PowerInhibitionMode::System), 1);
        wait_until(|| {
            let snapshot = handle.snapshot();
            snapshot.confirmed_generation == 0
                && snapshot.last_error == Some(PowerInhibitionErrorClass::SystemAcquire)
        });
        let failed = handle.snapshot();
        assert_eq!(failed.desired_generation, 1);
        assert_eq!(failed.confirmed_generation, 0);
        assert_eq!(
            failed.system_resource,
            PowerInhibitionResourceState::NotHeld
        );
        assert_eq!(
            failed.last_error,
            Some(PowerInhibitionErrorClass::SystemAcquire)
        );

        state
            .block_successful_system_acquire
            .store(false, Ordering::Release);
        state.gate.notify_all();
        wait_until(|| handle.snapshot().confirmed_generation == 1);
        assert_eq!(state.calls(), ["acquire:System", "acquire:System"]);
        drop(handle);
        let _ = worker.join().unwrap();
    }

    /// Verifies a failed release retains both ownership and the older confirmed
    /// generation until retry succeeds for the unchanged Disabled request.
    #[test]
    fn failed_release_retries_without_confirming_disabled_early() {
        let (state, handle, worker) = fake_service(false);
        let worker = std::thread::spawn(move || worker.run());
        handle.publish(PowerInhibitionMode::System);
        wait_until(|| handle.snapshot().confirmed_generation == 1);
        state.release_failures.store(1, Ordering::Release);

        assert_eq!(handle.publish(PowerInhibitionMode::Disabled), 2);
        wait_until(|| {
            state
                .calls()
                .iter()
                .filter(|call| *call == "release:System")
                .count()
                == 1
        });
        let failed = handle.snapshot();
        assert_eq!(failed.desired_generation, 2);
        assert_eq!(failed.confirmed_generation, 1);
        assert_eq!(failed.confirmed_mode, PowerInhibitionMode::System);
        assert_eq!(failed.system_resource, PowerInhibitionResourceState::Held);
        assert_eq!(
            failed.last_error,
            Some(PowerInhibitionErrorClass::SystemRelease)
        );

        wait_until(|| handle.snapshot().confirmed_generation == 2);
        assert_eq!(
            state.calls(),
            ["acquire:System", "release:System", "release:System"]
        );
        drop(handle);
        let _ = worker.join().unwrap();
    }

    /// Verifies a changed generation supersedes a long retry wait and confirms
    /// its already-satisfied weaker mode without waiting for the old deadline.
    #[test]
    fn changed_generation_preempts_retry_wait() {
        let retry_delay = Duration::from_secs(5);
        let (state, handle, worker) = fake_service_with_retry_delay(retry_delay);
        state.display_acquire_failures.store(1, Ordering::Release);
        let worker = std::thread::spawn(move || worker.run());
        handle.publish(PowerInhibitionMode::SystemAndDisplay);
        wait_until(|| state.calls().len() == 2);

        let started = Instant::now();
        assert_eq!(handle.publish(PowerInhibitionMode::System), 2);
        wait_until(|| handle.snapshot().confirmed_generation == 2);
        assert!(started.elapsed() < Duration::from_millis(250));
        assert_eq!(
            handle.snapshot().confirmed_mode,
            PowerInhibitionMode::System
        );

        drop(handle);
        let _ = worker.join().unwrap();
    }

    /// Verifies periodic worker reconciliation notices a display lease whose
    /// service owner vanished and reacquires it without a new desired generation.
    #[test]
    fn health_check_reacquires_lost_display_lease() {
        let (state, handle, worker) =
            fake_service_with_health_check_interval(Duration::from_millis(5));
        let worker = std::thread::spawn(move || worker.run());
        handle.publish(PowerInhibitionMode::SystemAndDisplay);
        wait_until(|| handle.snapshot().confirmed_generation == 1);

        state.display_health_failures.store(1, Ordering::Release);
        wait_until(|| {
            state
                .calls()
                .iter()
                .filter(|call| *call == "acquire:Display")
                .count()
                == 2
        });

        let snapshot = handle.snapshot();
        assert_eq!(snapshot.desired_generation, 1);
        assert_eq!(snapshot.confirmed_generation, 1);
        assert_eq!(snapshot.state, PowerInhibitionState::SystemAndDisplay);
        assert_eq!(
            snapshot.display_resource,
            PowerInhibitionResourceState::Held
        );
        drop(handle);
        let _ = worker.join().unwrap();
    }

    /// Verifies terminal Disabled preempts a long acquisition retry and an
    /// older failed completion cannot claim that the newer desired mode is held.
    #[test]
    fn terminal_disabled_preempts_retry_wait_and_supersedes_stale_completion() {
        let retry_delay = Duration::from_secs(5);
        let (state, handle, worker) = fake_service_with_retry_delay(retry_delay);
        state.system_acquire_failures.store(1, Ordering::Release);
        let worker = std::thread::spawn(move || worker.run());
        handle.publish(PowerInhibitionMode::System);
        wait_until(|| state.calls().len() == 1);

        let started = Instant::now();
        handle.close();
        let report = worker.join().unwrap();
        assert!(started.elapsed() < Duration::from_millis(250));
        assert_eq!(report.final_snapshot.desired_generation, 2);
        assert_eq!(
            report.final_snapshot.desired_mode,
            PowerInhibitionMode::Disabled
        );
        assert_eq!(report.final_snapshot.confirmed_generation, 2);
        assert_eq!(
            report.final_snapshot.confirmed_mode,
            PowerInhibitionMode::Disabled
        );
        assert_eq!(report.final_snapshot.state, PowerInhibitionState::Inactive);
        assert_eq!(
            report.final_snapshot.system_resource,
            PowerInhibitionResourceState::NotHeld
        );
        drop(handle);
    }

    /// Verifies final-handle cleanup cannot report terminal Disabled while a
    /// retained lease remains after a failed release, and retries to completion.
    #[test]
    fn terminal_cleanup_retries_retained_lease_before_worker_exit() {
        let (state, handle, worker) = fake_service(false);
        let worker = std::thread::spawn(move || worker.run());
        handle.publish(PowerInhibitionMode::System);
        wait_until(|| handle.snapshot().confirmed_generation == 1);
        state.release_failures.store(1, Ordering::Release);
        drop(handle);

        let report = worker.join().unwrap();
        assert_eq!(
            report.final_snapshot.desired_mode,
            PowerInhibitionMode::Disabled
        );
        assert_eq!(
            report.final_snapshot.confirmed_mode,
            PowerInhibitionMode::Disabled
        );
        assert_eq!(report.final_snapshot.state, PowerInhibitionState::Inactive);
        assert_eq!(
            report.final_snapshot.system_resource,
            PowerInhibitionResourceState::NotHeld
        );
        assert_eq!(
            state.calls(),
            ["acquire:System", "release:System", "release:System"]
        );
    }
}
