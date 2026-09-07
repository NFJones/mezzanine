//! `org.freedesktop.ScreenSaver` protocol data and cookie lease.

use std::fmt;
use std::sync::Arc;

use super::super::PowerInhibitionLease;
use super::LinuxPowerInhibitionFailure;

pub(super) const DESTINATION: &str = "org.freedesktop.ScreenSaver";
pub(super) const PATH: &str = "/org/freedesktop/ScreenSaver";
pub(super) const INTERFACE: &str = "org.freedesktop.ScreenSaver";
pub(super) const INHIBIT_METHOD: &str = "Inhibit";
pub(super) const UNINHIBIT_METHOD: &str = "UnInhibit";

/// Exact strings passed to the desktop ScreenSaver inhibitor method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ScreenSaverInhibitRequest {
    pub(super) application: &'static str,
    pub(super) reason: &'static str,
}

impl ScreenSaverInhibitRequest {
    pub(super) fn for_active_turn(reason: &'static str) -> Self {
        Self {
            application: "io.mezzanine.Mez",
            reason,
        }
    }

    pub(super) fn body(self) -> (&'static str, &'static str) {
        (self.application, self.reason)
    }
}

/// Owner-bound release operation retained beside one ScreenSaver cookie.
pub(super) trait ScreenSaverUninhibitor: fmt::Debug + Send + Sync + 'static {
    /// Releases the cookie only through the connection and unique service
    /// owner that returned it.
    fn uninhibit(&self, cookie: u32) -> Result<(), LinuxPowerInhibitionFailure>;

    /// Returns whether the unique service owner that issued the cookie remains.
    fn owner_is_active(&self) -> Result<bool, LinuxPowerInhibitionFailure>;
}

/// Owned ScreenSaver cookie and the connection identity that created it.
#[derive(Debug)]
pub(super) struct ScreenSaverLease {
    uninhibitor: Arc<dyn ScreenSaverUninhibitor>,
    cookie: Option<u32>,
}

impl ScreenSaverLease {
    pub(super) fn new(uninhibitor: Arc<dyn ScreenSaverUninhibitor>, cookie: u32) -> Self {
        Self {
            uninhibitor,
            cookie: Some(cookie),
        }
    }
}

impl PowerInhibitionLease for ScreenSaverLease {
    fn release(&mut self) -> Result<(), String> {
        let Some(cookie) = self.cookie else {
            return Ok(());
        };
        match self.uninhibitor.uninhibit(cookie) {
            Ok(()) => {
                self.cookie = None;
                Ok(())
            }
            Err(failure) => {
                // `UnInhibit` completion is ambiguous after every transport or
                // protocol failure. Discarding this cookie drops its dedicated
                // connection, which is the ScreenSaver protocol cleanup fallback.
                self.cookie = None;
                Err(failure.to_string())
            }
        }
    }

    fn is_held(&self) -> bool {
        self.cookie.is_some()
    }

    fn health_check(&mut self) -> Result<(), String> {
        let Some(_) = self.cookie else {
            return Err(LinuxPowerInhibitionFailure::Disconnected.to_string());
        };
        match self.uninhibitor.owner_is_active() {
            Ok(true) => Ok(()),
            Ok(false) => {
                self.cookie = None;
                Err(LinuxPowerInhibitionFailure::OwnerChanged.to_string())
            }
            Err(failure) => {
                self.cookie = None;
                Err(failure.to_string())
            }
        }
    }
}

impl Drop for ScreenSaverLease {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Debug, Default)]
    struct FakeUninhibitor {
        cookies: Mutex<Vec<u32>>,
    }

    impl ScreenSaverUninhibitor for FakeUninhibitor {
        fn uninhibit(&self, cookie: u32) -> Result<(), LinuxPowerInhibitionFailure> {
            self.cookies.lock().unwrap().push(cookie);
            Ok(())
        }

        fn owner_is_active(&self) -> Result<bool, LinuxPowerInhibitionFailure> {
            Ok(true)
        }
    }

    /// Verifies graceful release sends exactly the cookie returned by the
    /// matching acquisition and does not send it more than once.
    #[test]
    fn screensaver_cookie_is_released_exactly_once() {
        let uninhibitor = Arc::new(FakeUninhibitor::default());
        let mut lease = ScreenSaverLease::new(uninhibitor.clone(), 0x51a7);

        lease.release().unwrap();
        lease.release().unwrap();
        drop(lease);

        assert_eq!(*uninhibitor.cookies.lock().unwrap(), [0x51a7]);
    }

    /// Verifies dropping an unreleased cookie makes one best-effort cleanup
    /// attempt before the owner-bound connection is discarded.
    #[test]
    fn screensaver_cookie_drop_attempts_cleanup() {
        let uninhibitor = Arc::new(FakeUninhibitor::default());
        drop(ScreenSaverLease::new(uninhibitor.clone(), 73));
        assert_eq!(*uninhibitor.cookies.lock().unwrap(), [73]);
    }

    #[derive(Debug)]
    struct FailingUninhibitor {
        cookies: Mutex<Vec<u32>>,
        failure: LinuxPowerInhibitionFailure,
    }

    impl ScreenSaverUninhibitor for FailingUninhibitor {
        fn uninhibit(&self, cookie: u32) -> Result<(), LinuxPowerInhibitionFailure> {
            self.cookies.lock().unwrap().push(cookie);
            Err(self.failure)
        }

        fn owner_is_active(&self) -> Result<bool, LinuxPowerInhibitionFailure> {
            Ok(true)
        }
    }

    /// Verifies an ambiguous graceful-release failure discards the cookie and
    /// falls back to connection teardown instead of retrying a possibly applied call.
    #[test]
    fn failed_uninhibit_is_not_retried_with_an_ambiguous_cookie() {
        let uninhibitor = Arc::new(FailingUninhibitor {
            cookies: Mutex::new(Vec::new()),
            failure: LinuxPowerInhibitionFailure::Timeout,
        });
        let mut lease = ScreenSaverLease::new(uninhibitor.clone(), 91);

        assert_eq!(lease.release().unwrap_err(), "D-Bus operation timed out");
        assert!(!lease.is_held());
        lease.release().unwrap();
        drop(lease);

        assert_eq!(*uninhibitor.cookies.lock().unwrap(), [91]);
    }
}
