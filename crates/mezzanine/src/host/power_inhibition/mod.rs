//! Host power-inhibition leases for active agent work.
//!
//! This module owns the platform-neutral transition contract for host power
//! assertions. Runtime turn accounting chooses the desired mode elsewhere;
//! this boundary acquires and releases only resources created by Mez. A mode
//! transition is idempotent, display acquisition never discards a successful
//! system lease, and drop releases every retained lease in display-first order.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(target_os = "macos"))]
mod unsupported;

#[cfg(target_os = "macos")]
pub(crate) use macos::MacOsPowerInhibitionBackend;
#[cfg(not(target_os = "macos"))]
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

/// One platform-owned resource identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PowerInhibitionResource {
    /// Prevent automatic idle system sleep.
    System,
    /// Prevent automatic display sleep.
    Display,
}

/// Native host interface used by the transition controller.
pub(crate) trait PowerInhibitionBackend {
    /// Acquires one resource and returns an opaque host lease identifier.
    fn acquire(&mut self, resource: PowerInhibitionResource) -> std::result::Result<u32, String>;
    /// Releases exactly one previously acquired host lease identifier.
    fn release(&mut self, lease_id: u32) -> std::result::Result<(), String>;
}

/// Owns the assertions Mez successfully created through one platform backend.
#[derive(Debug)]
pub(crate) struct PowerInhibitionController<B: PowerInhibitionBackend> {
    backend: B,
    system_lease: Option<u32>,
    display_lease: Option<u32>,
    state: PowerInhibitionState,
    last_error: Option<String>,
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
        }
    }

    /// Returns the resources Mez currently owns.
    pub(crate) fn state(&self) -> PowerInhibitionState {
        self.state
    }

    /// Returns the last nonfatal backend error observed during reconciliation.
    pub(crate) fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Reconciles owned leases with the requested mode without duplicating
    /// successful acquisitions or releasing resources Mez did not create.
    pub(crate) fn reconcile(&mut self, mode: PowerInhibitionMode) {
        self.last_error = None;
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
        if self.system_lease.is_some() {
            return;
        }
        match self.backend.acquire(PowerInhibitionResource::System) {
            Ok(lease) => self.system_lease = Some(lease),
            Err(error) => {
                self.last_error = Some(error);
                self.state = PowerInhibitionState::Unavailable;
            }
        }
    }

    fn acquire_display(&mut self) {
        if self.display_lease.is_some() {
            return;
        }
        match self.backend.acquire(PowerInhibitionResource::Display) {
            Ok(lease) => self.display_lease = Some(lease),
            Err(error) => self.last_error = Some(error),
        }
    }

    fn release_display(&mut self) {
        let Some(lease) = self.display_lease else {
            return;
        };
        match self.backend.release(lease) {
            Ok(()) => self.display_lease = None,
            Err(error) => self.last_error = Some(error),
        }
    }

    fn release_all(&mut self) {
        self.release_display();
        if self.display_lease.is_none()
            && let Some(lease) = self.system_lease
        {
            match self.backend.release(lease) {
                Ok(()) => self.system_lease = None,
                Err(error) => self.last_error = Some(error),
            }
        }
        self.refresh_state();
    }

    fn refresh_state(&mut self) {
        self.state = match (self.system_lease.is_some(), self.display_lease.is_some()) {
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
    }
}

/// Creates the production controller for the current host platform.
#[cfg(target_os = "macos")]
pub(crate) fn production_power_inhibition_controller()
-> PowerInhibitionController<MacOsPowerInhibitionBackend> {
    PowerInhibitionController::new(MacOsPowerInhibitionBackend::new())
}

/// Creates the unavailable production controller on platforms without a
/// backend in this milestone.
#[cfg(not(target_os = "macos"))]
pub(crate) fn production_power_inhibition_controller()
-> PowerInhibitionController<UnsupportedPowerInhibitionBackend> {
    PowerInhibitionController::new(UnsupportedPowerInhibitionBackend)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default)]
    struct FakeBackend {
        calls: Vec<String>,
        fail_display: bool,
        fail_release: bool,
        next_id: u32,
    }

    impl PowerInhibitionBackend for FakeBackend {
        fn acquire(
            &mut self,
            resource: PowerInhibitionResource,
        ) -> std::result::Result<u32, String> {
            self.calls.push(format!("acquire:{resource:?}"));
            if resource == PowerInhibitionResource::Display && self.fail_display {
                return Err("display unavailable".to_string());
            }
            self.next_id += 1;
            Ok(self.next_id)
        }

        fn release(&mut self, lease_id: u32) -> std::result::Result<(), String> {
            self.calls.push(format!("release:{lease_id}"));
            if self.fail_release {
                Err("release unavailable".to_string())
            } else {
                Ok(())
            }
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
        assert_eq!(controller.backend.calls, ["acquire:System", "release:1"]);
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
            controller.backend.calls,
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
            controller.backend.calls,
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
        controller.backend.fail_release = false;
        controller.reconcile(PowerInhibitionMode::Disabled);
        assert_eq!(controller.state(), PowerInhibitionState::Inactive);
        assert_eq!(
            controller.backend.calls,
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
            controller.backend.calls,
            ["acquire:System", "acquire:Display", "release:2"]
        );
        controller.backend.fail_release = false;
        controller.reconcile(PowerInhibitionMode::Disabled);
        assert_eq!(controller.state(), PowerInhibitionState::Inactive);
        assert_eq!(
            controller.backend.calls,
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
