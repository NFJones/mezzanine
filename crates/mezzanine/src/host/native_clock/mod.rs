//! Retained, suspend-inclusive native clock with explicit boot qualification.
//!
//! Capture runs on a blocking owner: fixed kernel UUID, bounded zero-offset
//! Linux namespace qualification, or Darwin native timebase. Samples retain that
//! original boot/namespace and refuse rollback, failure or namespace replacement.
//! There is no UTC/Instant fallback and no constructor from saved identifiers.
//! A sample and boot UUID describe elapsed time, never an owner or permission.
//! Cross-boot durable elapsed accounting belongs to the custody store, not here.
//!
//! Linux sampling performs six bounded namespace metadata observations around
//! CLOCK_BOOTTIME. It does not open/read clock, offset or UUID files after capture.
//! Darwin sampling reads mach_continuous_time using the retained checked ratio.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

mod boot_uuid;
pub(crate) use boot_uuid::native_boot_uuid;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux::Clock;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos::Clock;

/// One actual kernel boot and clock qualification; intentionally not serializable.
#[derive(Debug)]
pub(crate) struct NativeBootClock {
    boot_uuid: String,
    clock: Clock,
    observation: Observation,
}
impl NativeBootClock {
    /// Captures actual native interfaces or refuses without creating an epoch.
    pub(crate) fn capture() -> Option<Self> {
        let clock = Self {
            boot_uuid: native_boot_uuid()?,
            clock: Clock::capture()?,
            observation: Observation::default(),
        };
        clock.millis()?;
        Some(clock)
    }

    /// Returns the original kernel UUID, never a user or persisted selector.
    pub(crate) fn boot_uuid(&self) -> &str {
        &self.boot_uuid
    }

    /// Samples without waiting for another reader. Concurrent calls may return
    /// unavailable; actual native failure or rollback permanently poisons this
    /// captured clock so later recovery cannot silently create a fresh lifetime.
    pub(crate) fn millis(&self) -> Option<u64> {
        self.observation.sample(|| self.clock.millis())
    }

    /// Injects native read failure into this fixture's actual retained clock.
    #[cfg(test)]
    pub(crate) fn invalidate_for_tests(&self) {
        assert!(self.observation.sample(|| None).is_none());
    }
}

/// Unsupported platforms cannot manufacture a boot clock.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[derive(Debug)]
struct Clock;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl Clock {
    fn capture() -> Option<Self> {
        None
    }
    fn millis(&self) -> Option<u64> {
        None
    }
}

/// Nonwaiting serialization keeps concurrent sample completion order from being
/// mistaken for kernel rollback. MAX is an absorbing failure, never a timestamp.
#[derive(Debug, Default)]
struct Observation {
    sampling: AtomicBool,
    high_water: AtomicU64,
}
impl Observation {
    /// Runs one bounded native read; invalid/decreasing observations are fatal to
    /// this clock. Contention alone does not change its previous qualification.
    fn sample(&self, read: impl FnOnce() -> Option<u64>) -> Option<u64> {
        if self
            .sampling
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return None;
        }
        let mut guard = Sampling {
            observation: self,
            finished: false,
        };
        let previous = self.high_water.load(Ordering::Relaxed);
        let sampled = (previous != u64::MAX)
            .then(read)
            .flatten()
            .filter(|now| *now > 0 && *now <= (i64::MAX as u64) - 60_000 && *now >= previous);
        self.high_water
            .store(sampled.unwrap_or(u64::MAX), Ordering::Relaxed);
        guard.finished = true;
        sampled
    }
}

/// Releases the nonwaiting sampling guard and poisons on unexpected unwind.
struct Sampling<'a> {
    observation: &'a Observation,
    finished: bool,
}
impl Drop for Sampling<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.observation
                .high_water
                .store(u64::MAX, Ordering::Relaxed);
        }
        self.observation.sampling.store(false, Ordering::Release);
    }
}

/// Qualification boundary for sampled worker/current leader/offset-owner
/// namespaces. Tuple equality includes both device and inode; no name, caller
/// selector or parsed zero offset can replace the native object association.
#[cfg(any(target_os = "linux", test))]
fn namespace_epoch_matches(captured: (u64, u64), observed: [(u64, u64); 3]) -> bool {
    observed.iter().all(|identity| *identity == captured)
}

/// Admits both known offset records exactly once, with zero signed seconds and
/// unsigned nanoseconds. Reject unknown/duplicate/missing/oversize/malformed
/// records rather than pretending a noninitial clock shares a persistent epoch.
#[cfg(any(target_os = "linux", test))]
fn zero_offsets(bytes: &[u8]) -> bool {
    if bytes.len() > 256 {
        return false;
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let (mut monotonic, mut boottime) = (false, false);
    for line in text.lines() {
        let mut fields = line.split_ascii_whitespace();
        let slot = match fields.next() {
            Some("monotonic") => &mut monotonic,
            Some("boottime") => &mut boottime,
            _ => return false,
        };
        if *slot
            || fields.next().and_then(|value| value.parse::<i64>().ok()) != Some(0)
            || fields.next().and_then(|value| value.parse::<u32>().ok()) != Some(0)
            || fields.next().is_some()
        {
            return false;
        }
        *slot = true;
    }
    monotonic && boottime
}

/// Checks native seconds/nanoseconds before converting to positive SQLite-range
/// milliseconds; no negative, malformed, wrapped or saturated timestamp survives.
#[cfg(any(target_os = "linux", test))]
fn timespec_millis(seconds: i64, nanos: i64) -> Option<u64> {
    if seconds < 0 || !(0..1_000_000_000).contains(&nanos) {
        return None;
    }
    let nanos = u128::try_from(seconds)
        .ok()?
        .checked_mul(1_000_000_000)?
        .checked_add(u128::try_from(nanos).ok()?)?;
    positive_millis(nanos / 1_000_000)
}

/// Public Mach timebase is a rational nanosecond conversion, not a frequency
/// guessed from the host architecture. Zero factors and unrepresentable results
/// stay unavailable; wide multiplication preserves exact submillisecond bounds.
#[cfg(any(target_os = "macos", test))]
fn tick_millis(ticks: u64, numer: u32, denom: u32) -> Option<u64> {
    if numer == 0 || denom == 0 {
        return None;
    }
    positive_millis(
        u128::from(ticks).checked_mul(u128::from(numer))? / u128::from(denom) / 1_000_000,
    )
}

/// Keeps the value positive and leaves exact integer room for the finite window.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn positive_millis(value: u128) -> Option<u64> {
    let value = u64::try_from(value).ok()?;
    (value > 0 && value <= (i64::MAX as u64).checked_sub(60_000)?).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Equal millisecond samples are valid. Rollback is absorbing: even a later
    /// forward value cannot create a new epoch under the same retained owner.
    #[test]
    fn native_clock_rollback_cannot_be_requalified_by_later_sample() {
        let clock = Observation::default();
        for now in [100, 100, 101] {
            assert_eq!(clock.sample(|| Some(now)), Some(now));
        }
        assert_eq!(clock.sample(|| Some(100)), None);
        assert_eq!(clock.sample(|| panic!("poisoned clock read again")), None);
    }

    /// Missing, zero and overflow samples permanently invalidate qualification;
    /// none can be repaired by a wall clock or a subsequent native observation.
    #[test]
    fn native_clock_read_failure_and_range_errors_are_absorbing() {
        for value in [None, Some(0), Some(i64::MAX as u64), Some(u64::MAX)] {
            let clock = Observation::default();
            assert_eq!(clock.sample(|| Some(10)), Some(10));
            assert_eq!(clock.sample(|| value), None);
            assert_eq!(clock.sample(|| Some(20)), None);
        }
    }

    /// A simultaneous observer cannot block the actor or manufacture rollback
    /// through completion order. Busy refusal leaves the original sample valid.
    #[test]
    fn native_clock_contention_does_not_wait_or_poison_original() {
        let clock = Observation::default();
        assert_eq!(
            clock.sample(|| {
                assert_eq!(clock.sample(|| panic!("contended reader ran")), None);
                Some(100)
            }),
            Some(100)
        );
        assert_eq!(clock.sample(|| Some(101)), Some(101));
    }

    /// An unexpected native-reader unwind releases the guard but poisons its
    /// epoch. Subsequent callers refuse, rather than waiting or resetting time.
    #[test]
    fn native_clock_reader_unwind_releases_guard_and_refuses_recovery() {
        let clock = Observation::default();
        assert!(
            std::panic::catch_unwind(|| clock.sample(|| panic!("fixture native read"))).is_err()
        );
        assert!(!clock.sampling.load(Ordering::Acquire));
        assert_eq!(clock.sample(|| Some(100)), None);
    }

    /// Actual native qualification can cross a blocking-worker/thread boundary
    /// without resetting boot identity or elapsed time. Environments lacking
    /// supported zero-offset native interfaces remain explicitly unavailable.
    #[test]
    fn native_clock_retains_actual_boot_across_worker_transfer() {
        let Some(clock) = NativeBootClock::capture() else {
            return;
        };
        let boot = clock.boot_uuid().to_owned();
        let before = clock.millis().unwrap();
        let clock = std::thread::spawn(move || {
            assert!(clock.millis().unwrap() >= before);
            clock
        })
        .join()
        .unwrap();
        assert_eq!(clock.boot_uuid(), boot);
        clock.invalidate_for_tests();
        assert_eq!(clock.millis(), None);
    }

    /// Matching worker/leader CURRENT clocks plus zero offsets from a distinct
    /// CHILDREN namespace must not qualify. Exercise the actual before/after
    /// predicate without unsharing, mutating clocks or opening another user's
    /// process. This is a structural negative, not divergent-namespace execution.
    #[test]
    fn external_source_activation_clock_rejects_split_children_namespace() {
        let current = (1, 10);
        let other = (1, 11);
        assert!(zero_offsets(b"monotonic 0 0\nboottime 0 0\n"));
        assert!(
            !namespace_epoch_matches(current, [current, current, other]),
            "zero offsets belong to the wrong namespace despite matching current clocks"
        );
        assert!(namespace_epoch_matches(current, [current; 3]));
        for changed in [
            [other, current, current],
            [current, other, current],
            [current, current, other],
            [(2, 10), current, current],
        ] {
            assert!(!namespace_epoch_matches(current, changed));
        }
    }

    /// Offsets are parsed without changing the host namespace or clock. Only
    /// both fixed zero records qualify; malformed or nonzero data cannot extend
    /// a receipt by masquerading as the initial boot-clock epoch.
    #[test]
    fn external_source_activation_clock_namespace_offsets_fail_closed() {
        assert!(zero_offsets(b"monotonic 0 0\nboottime 0 0\n"));
        assert!(zero_offsets(b"boottime  0  0\nmonotonic 0 0\n"));
        for bytes in [
            b"".as_slice(),
            b"boottime 0 0\n",
            b"monotonic 0 0\nboottime 1 0\n",
            b"monotonic -1 0\nboottime 0 0\n",
            b"monotonic 0 0\nboottime 0 1\n",
            b"monotonic 0 0\nboottime 0 -1\n",
            b"monotonic 0 0\nboottime 0 0\nboottime 0 0\n",
            b"monotonic 0 0\nunknown 0 0\n",
            b"monotonic 0 0\nboottime 0 0 extra\n",
            b"monotonic 0 0\nboottime 0 0\0\n",
        ] {
            assert!(!zero_offsets(bytes));
        }
        assert!(!zero_offsets(&[b' '; 257]));
    }

    /// Native conversion preserves millisecond truncation with checked ranges;
    /// zero/negative/invalid values and extreme positive overflow remain absent.
    /// Darwin math is exercised portably, not claimed as a macOS execution run.
    #[test]
    fn external_source_activation_clock_conversion_is_checked() {
        assert_eq!(timespec_millis(1, 999_999_999), Some(1_999));
        assert_eq!(timespec_millis(0, 999_999), None);
        for (seconds, nanos) in [(-1, 0), (1, -1), (1, 1_000_000_000), (i64::MAX, 0)] {
            assert_eq!(timespec_millis(seconds, nanos), None);
        }
        assert_eq!(tick_millis(3_000_000, 2, 3), Some(2));
        assert_eq!(tick_millis(u64::MAX, u32::MAX, 1), None);
        assert_eq!(tick_millis(1_000_000, 0, 1), None);
        assert_eq!(tick_millis(1_000_000, 1, 0), None);
        assert_eq!(positive_millis(0), None);
        assert_eq!(positive_millis(i64::MAX as u128), None);
    }
}
