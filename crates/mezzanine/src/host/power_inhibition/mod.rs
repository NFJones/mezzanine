//! Host power-inhibition leases for active agent work.
//!
//! This module owns the platform-neutral transition contract for host power
//! assertions. Runtime turn accounting chooses the desired mode elsewhere;
//! this boundary acquires and releases only resources created by Mez. A mode
//! transition is idempotent, display acquisition never discards a successful
//! system lease, and drop releases every retained lease in display-first order.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
mod service;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod unsupported;

#[cfg(target_os = "linux")]
pub(crate) use linux::LinuxDbusPowerInhibitionBackend;
#[cfg(target_os = "macos")]
pub(crate) use macos::MacOsPowerInhibitionBackend;
pub(crate) use service::{
    PowerInhibitionHandle, PowerInhibitionSnapshot, PowerInhibitionWorker, power_inhibition_service,
};
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) use unsupported::UnsupportedPowerInhibitionBackend;

/// The power resources Mez may acquire for currently active agent work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum PowerInhibitionMode {
    /// Do not retain a host power assertion.
    #[default]
    Disabled,
    /// Prevent automatic idle system sleep.
    System,
    /// Prevent automatic idle system and display sleep where supported.
    SystemAndDisplay,
}

/// The resources that are currently known to be held by Mez.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum PowerInhibitionState {
    /// Mez owns no host power resource.
    #[default]
    Inactive,
    /// Mez owns only the idle-system-sleep assertion.
    System,
    /// Mez owns the idle-system-sleep and display assertions.
    SystemAndDisplay,
    /// The backend did not provide the requested resource.
    Unavailable,
    /// Mez retained system inhibition but could not acquire display inhibition.
    SystemOnly,
}

/// Native adapter family used by one session power worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PowerInhibitionBackendKind {
    /// Native Linux logind and ScreenSaver D-Bus inhibitors.
    LinuxDbus,
    /// Native macOS IOKit assertions.
    MacOsIokit,
    /// No native adapter is available for this host platform.
    Unsupported,
    /// Deterministic backend used by focused regression tests.
    #[cfg(test)]
    Test,
}

/// Confirmed ownership state for one independently managed power resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum PowerInhibitionResourceState {
    /// The worker does not own this resource.
    #[default]
    NotHeld,
    /// The worker owns this resource through an opaque native lease.
    Held,
}

/// Bounded classification of a nonfatal native power-operation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PowerInhibitionErrorClass {
    /// The system-sleep resource could not be acquired.
    SystemAcquire,
    /// The display-sleep resource could not be acquired.
    DisplayAcquire,
    /// The display-sleep resource could not be released.
    DisplayRelease,
    /// The system-sleep resource could not be released.
    SystemRelease,
}

/// One platform-owned resource identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PowerInhibitionResource {
    /// Prevent automatic idle system sleep.
    System,
    /// Prevent automatic display sleep.
    Display,
}

/// One opaque platform-owned power-inhibition lease.
///
/// Implementations retain every native value needed for cleanup. A failed
/// explicit release normally preserves ownership so the controller can retry;
/// an adapter that falls back to connection teardown must instead report that
/// it is no longer held. Drop performs one final best-effort release without
/// exposing platform handles.
pub(crate) trait PowerInhibitionLease: std::fmt::Debug + Send {
    /// Releases this lease. Failure must leave it retryable unless cleanup
    /// fallback made `is_held` false.
    fn release(&mut self) -> std::result::Result<(), String>;

    /// Returns whether the native resource is still believed to be owned.
    fn is_held(&self) -> bool {
        true
    }

    /// Performs one adapter-bounded liveness check for retained ownership.
    fn health_check(&mut self) -> std::result::Result<(), String> {
        Ok(())
    }
}

/// Native host interface used by the transition controller.
///
/// Calls are synchronous and may block for adapter-owned deadlines. The
/// service isolates them on its blocking worker, but cannot safely cancel an
/// in-progress native call through this trait; adapters remain responsible for
/// bounding their own operation latency.
pub(crate) trait PowerInhibitionBackend: std::fmt::Debug + Send {
    /// Returns the bounded native adapter identity used for status snapshots.
    fn kind(&self) -> PowerInhibitionBackendKind;

    /// Acquires one resource and returns its opaque owned lease.
    fn acquire(
        &mut self,
        resource: PowerInhibitionResource,
    ) -> std::result::Result<Box<dyn PowerInhibitionLease>, String>;
}

impl<B: PowerInhibitionBackend + ?Sized> PowerInhibitionBackend for Box<B> {
    fn kind(&self) -> PowerInhibitionBackendKind {
        (**self).kind()
    }

    fn acquire(
        &mut self,
        resource: PowerInhibitionResource,
    ) -> std::result::Result<Box<dyn PowerInhibitionLease>, String> {
        (**self).acquire(resource)
    }
}

/// Owns the assertions Mez successfully created through one platform backend.
#[derive(Debug)]
pub(crate) struct PowerInhibitionController<B: PowerInhibitionBackend> {
    backend: B,
    system_lease: Option<Box<dyn PowerInhibitionLease>>,
    display_lease: Option<Box<dyn PowerInhibitionLease>>,
    state: PowerInhibitionState,
    last_error: Option<String>,
    last_error_class: Option<PowerInhibitionErrorClass>,
}

impl<B: PowerInhibitionBackend> PowerInhibitionController<B> {
    /// Creates a controller with no active host power leases.
    pub(crate) fn new(backend: B) -> Self {
        Self {
            backend,
            system_lease: None,
            display_lease: None,
            state: PowerInhibitionState::Inactive,
            last_error: None,
            last_error_class: None,
        }
    }

    /// Returns the bounded identity of the controller's native adapter.
    pub(crate) fn backend_kind(&self) -> PowerInhibitionBackendKind {
        self.backend.kind()
    }

    /// Returns the resources Mez currently owns.
    pub(crate) fn state(&self) -> PowerInhibitionState {
        self.state
    }

    /// Returns the last nonfatal backend error observed during reconciliation.
    pub(crate) fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Returns a bounded class for the latest nonfatal backend failure.
    pub(crate) fn last_error_class(&self) -> Option<PowerInhibitionErrorClass> {
        self.last_error_class
    }

    /// Returns confirmed ownership of the system-sleep resource.
    pub(crate) fn system_resource_state(&self) -> PowerInhibitionResourceState {
        if self
            .system_lease
            .as_ref()
            .is_some_and(|lease| lease.is_held())
        {
            PowerInhibitionResourceState::Held
        } else {
            PowerInhibitionResourceState::NotHeld
        }
    }

    /// Returns confirmed ownership of the display-sleep resource.
    pub(crate) fn display_resource_state(&self) -> PowerInhibitionResourceState {
        if self
            .display_lease
            .as_ref()
            .is_some_and(|lease| lease.is_held())
        {
            PowerInhibitionResourceState::Held
        } else {
            PowerInhibitionResourceState::NotHeld
        }
    }

    /// Returns whether owned leases exactly satisfy one desired mode.
    pub(crate) fn satisfies(&self, mode: PowerInhibitionMode) -> bool {
        let system_held = self
            .system_lease
            .as_ref()
            .is_some_and(|lease| lease.is_held());
        let display_held = self
            .display_lease
            .as_ref()
            .is_some_and(|lease| lease.is_held());
        match mode {
            PowerInhibitionMode::Disabled => !system_held && !display_held,
            PowerInhibitionMode::System => system_held && !display_held,
            PowerInhibitionMode::SystemAndDisplay => system_held && display_held,
        }
    }

    /// Reconciles owned leases with the requested mode without duplicating
    /// successful acquisitions or releasing resources Mez did not create.
    pub(crate) fn reconcile(&mut self, mode: PowerInhibitionMode) {
        self.last_error = None;
        self.last_error_class = None;
        match mode {
            PowerInhibitionMode::Disabled => self.release_all(),
            PowerInhibitionMode::System => {
                self.release_display();
                self.acquire_system();
                self.refresh_state();
            }
            PowerInhibitionMode::SystemAndDisplay => {
                self.acquire_system();
                if self.system_lease.is_some() {
                    self.acquire_display();
                }
                self.refresh_state();
            }
        }
    }

    fn acquire_system(&mut self) {
        if let Some(lease) = self.system_lease.as_mut() {
            if let Err(error) = lease.health_check() {
                self.last_error = Some(error);
                self.last_error_class = Some(PowerInhibitionErrorClass::SystemAcquire);
            }
            if lease.is_held() {
                return;
            }
            self.system_lease = None;
        }
        match self.backend.acquire(PowerInhibitionResource::System) {
            Ok(lease) => {
                self.system_lease = Some(lease);
                self.last_error = None;
                self.last_error_class = None;
            }
            Err(error) => {
                self.last_error = Some(error);
                self.last_error_class = Some(PowerInhibitionErrorClass::SystemAcquire);
                self.state = PowerInhibitionState::Unavailable;
            }
        }
    }

    fn acquire_display(&mut self) {
        if let Some(lease) = self.display_lease.as_mut() {
            if let Err(error) = lease.health_check() {
                self.last_error = Some(error);
                self.last_error_class = Some(PowerInhibitionErrorClass::DisplayAcquire);
            }
            if lease.is_held() {
                return;
            }
            self.display_lease = None;
        }
        match self.backend.acquire(PowerInhibitionResource::Display) {
            Ok(lease) => {
                self.display_lease = Some(lease);
                self.last_error = None;
                self.last_error_class = None;
            }
            Err(error) => {
                self.last_error = Some(error);
                self.last_error_class = Some(PowerInhibitionErrorClass::DisplayAcquire);
            }
        }
    }

    fn release_display(&mut self) {
        let Some(lease) = self.display_lease.as_mut() else {
            return;
        };
        match lease.release() {
            Ok(()) => self.display_lease = None,
            Err(error) => {
                self.last_error = Some(error);
                self.last_error_class = Some(PowerInhibitionErrorClass::DisplayRelease);
                if !lease.is_held() {
                    self.display_lease = None;
                }
            }
        }
    }

    fn release_all(&mut self) {
        self.release_display();
        if self.display_lease.is_none()
            && let Some(lease) = self.system_lease.as_mut()
        {
            match lease.release() {
                Ok(()) => self.system_lease = None,
                Err(error) => {
                    self.last_error = Some(error);
                    self.last_error_class = Some(PowerInhibitionErrorClass::SystemRelease);
                    if !lease.is_held() {
                        self.system_lease = None;
                    }
                }
            }
        }
        self.refresh_state();
    }

    fn refresh_state(&mut self) {
        let system_held = self
            .system_lease
            .as_ref()
            .is_some_and(|lease| lease.is_held());
        let display_held = self
            .display_lease
            .as_ref()
            .is_some_and(|lease| lease.is_held());
        self.state = match (system_held, display_held) {
            (true, true) => PowerInhibitionState::SystemAndDisplay,
            (true, false) if self.last_error.is_some() => PowerInhibitionState::SystemOnly,
            (true, false) => PowerInhibitionState::System,
            (false, false) if self.last_error.is_some() => PowerInhibitionState::Unavailable,
            (false, false) => PowerInhibitionState::Inactive,
            (false, true) => PowerInhibitionState::Unavailable,
        };
    }
}

impl<B: PowerInhibitionBackend> Drop for PowerInhibitionController<B> {
    /// Releases every host lease still owned by Mez during shutdown.
    fn drop(&mut self) {
        self.release_all();
        // A failed explicit release deliberately leaves ownership retained.
        // Drop each opaque lease in strength order so its final best-effort
        // cleanup cannot release system protection before display protection.
        drop(self.display_lease.take());
        drop(self.system_lease.take());
    }
}

/// Creates the production controller for the current host platform.
#[cfg(target_os = "macos")]
pub(crate) fn production_power_inhibition_controller()
-> PowerInhibitionController<Box<dyn PowerInhibitionBackend>> {
    PowerInhibitionController::new(Box::new(MacOsPowerInhibitionBackend::new()))
}

/// Creates the native Linux D-Bus controller.
#[cfg(target_os = "linux")]
pub(crate) fn production_power_inhibition_controller()
-> PowerInhibitionController<Box<dyn PowerInhibitionBackend>> {
    PowerInhibitionController::new(Box::new(LinuxDbusPowerInhibitionBackend::new()))
}

/// Creates the unavailable production controller on platforms without a
/// backend in this milestone.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn production_power_inhibition_controller()
-> PowerInhibitionController<Box<dyn PowerInhibitionBackend>> {
    PowerInhibitionController::new(Box::new(UnsupportedPowerInhibitionBackend))
}

/// Creates the production latest-value handle and blocking worker for one session.
pub(crate) fn production_power_inhibition_service() -> (
    PowerInhibitionHandle,
    PowerInhibitionWorker<Box<dyn PowerInhibitionBackend>>,
) {
    power_inhibition_service(production_power_inhibition_controller())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Default)]
    struct FakeBackendState {
        calls: Vec<String>,
        fail_release: bool,
    }

    #[derive(Debug, Default)]
    struct FakeBackend {
        state: Arc<Mutex<FakeBackendState>>,
        fail_display: bool,
        fail_release: bool,
        next_id: u32,
    }

    impl FakeBackend {
        fn calls(&self) -> Vec<String> {
            self.state.lock().unwrap().calls.clone()
        }

        fn set_fail_release(&mut self, fail_release: bool) {
            self.fail_release = fail_release;
            self.state.lock().unwrap().fail_release = fail_release;
        }
    }

    #[derive(Debug)]
    struct FakeLease {
        state: Arc<Mutex<FakeBackendState>>,
        lease_id: u32,
        released: bool,
    }

    impl PowerInhibitionLease for FakeLease {
        fn release(&mut self) -> std::result::Result<(), String> {
            if self.released {
                return Ok(());
            }
            let mut state = self.state.lock().unwrap();
            state.calls.push(format!("release:{}", self.lease_id));
            if state.fail_release {
                Err("release unavailable".to_string())
            } else {
                self.released = true;
                Ok(())
            }
        }
    }

    impl Drop for FakeLease {
        fn drop(&mut self) {
            let _ = self.release();
        }
    }

    impl PowerInhibitionBackend for FakeBackend {
        fn kind(&self) -> PowerInhibitionBackendKind {
            PowerInhibitionBackendKind::Test
        }

        fn acquire(
            &mut self,
            resource: PowerInhibitionResource,
        ) -> std::result::Result<Box<dyn PowerInhibitionLease>, String> {
            let mut state = self.state.lock().unwrap();
            state.calls.push(format!("acquire:{resource:?}"));
            state.fail_release = self.fail_release;
            if resource == PowerInhibitionResource::Display && self.fail_display {
                return Err("display unavailable".to_string());
            }
            self.next_id += 1;
            Ok(Box::new(FakeLease {
                state: Arc::clone(&self.state),
                lease_id: self.next_id,
                released: false,
            }))
        }
    }

    /// Verifies repeated requests for one mode retain exactly one system lease.
    #[test]
    fn controller_deduplicates_system_acquisition() {
        let mut controller = PowerInhibitionController::new(FakeBackend::default());
        controller.reconcile(PowerInhibitionMode::System);
        controller.reconcile(PowerInhibitionMode::System);

        assert_eq!(controller.state(), PowerInhibitionState::System);
        controller.reconcile(PowerInhibitionMode::Disabled);
        assert_eq!(controller.backend.calls(), ["acquire:System", "release:1"]);
    }

    /// Verifies display acquisition failure preserves a successful system lease.
    #[test]
    fn controller_retains_system_lease_after_display_failure() {
        let mut controller = PowerInhibitionController::new(FakeBackend {
            fail_display: true,
            ..FakeBackend::default()
        });
        controller.reconcile(PowerInhibitionMode::SystemAndDisplay);

        assert_eq!(controller.state(), PowerInhibitionState::SystemOnly);
        assert_eq!(controller.last_error(), Some("display unavailable"));
        controller.reconcile(PowerInhibitionMode::Disabled);
        assert_eq!(
            controller.backend.calls(),
            ["acquire:System", "acquire:Display", "release:1"]
        );
    }

    /// Verifies downgrade releases display before system and full shutdown
    /// releases only resources created by this controller.
    #[test]
    fn controller_releases_owned_leases_in_reverse_strength_order() {
        let mut controller = PowerInhibitionController::new(FakeBackend::default());
        controller.reconcile(PowerInhibitionMode::SystemAndDisplay);
        controller.reconcile(PowerInhibitionMode::System);
        controller.reconcile(PowerInhibitionMode::Disabled);

        assert_eq!(controller.state(), PowerInhibitionState::Inactive);
        assert_eq!(
            controller.backend.calls(),
            [
                "acquire:System",
                "acquire:Display",
                "release:2",
                "release:1",
            ]
        );
    }

    /// Verifies a failed release retains the owned lease so a later reconcile
    /// or drop can retry cleanup instead of forgetting an active host resource.
    #[test]
    fn controller_retains_lease_after_release_failure() {
        let mut controller = PowerInhibitionController::new(FakeBackend {
            fail_release: true,
            ..FakeBackend::default()
        });
        controller.reconcile(PowerInhibitionMode::System);
        controller.reconcile(PowerInhibitionMode::Disabled);

        assert_eq!(controller.state(), PowerInhibitionState::SystemOnly);
        assert_eq!(controller.last_error(), Some("release unavailable"));
        controller.backend.set_fail_release(false);
        controller.reconcile(PowerInhibitionMode::Disabled);
        assert_eq!(controller.state(), PowerInhibitionState::Inactive);
        assert_eq!(
            controller.backend.calls(),
            ["acquire:System", "release:1", "release:1"]
        );
    }

    /// Verifies a display-release failure retains the system lease as well,
    /// preserving the stronger-to-weaker cleanup order until a later retry can
    /// release both resources safely.
    #[test]
    fn controller_does_not_release_system_before_failed_display_cleanup() {
        let mut controller = PowerInhibitionController::new(FakeBackend {
            fail_release: true,
            ..FakeBackend::default()
        });
        controller.reconcile(PowerInhibitionMode::SystemAndDisplay);
        controller.reconcile(PowerInhibitionMode::Disabled);

        assert_eq!(controller.state(), PowerInhibitionState::SystemAndDisplay);
        assert_eq!(
            controller.backend.calls(),
            ["acquire:System", "acquire:Display", "release:2"]
        );
        controller.backend.set_fail_release(false);
        controller.reconcile(PowerInhibitionMode::Disabled);
        assert_eq!(controller.state(), PowerInhibitionState::Inactive);
        assert_eq!(
            controller.backend.calls(),
            [
                "acquire:System",
                "acquire:Display",
                "release:2",
                "release:2",
                "release:1",
            ]
        );
    }
}
