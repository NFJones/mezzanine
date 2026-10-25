//! Suspend-inclusive same-boot clock sampling for inert source preparation.
//!
//! Linux CLOCK_BOOTTIME and Darwin mach_continuous_time advance during suspend;
//! neither is UTC or a serialized Rust Instant. Linux time namespaces can offset
//! boottime, so only a verified zero-offset current-thread clock is supported.
//! Missing/ambiguous interfaces fail closed for this optional metadata feature,
//! without changing ordinary enrollment authority or inventing a fallback.
//!
//! Sources: clock_gettime(3), time_namespaces(7), and Apple's public XNU
//! osfmk/mach/mach_time.h (mach_continuous_time, available since macOS 10.12).
//! Values are process-independent only within the separately qualified native
//! boot; a timestamp alone never proves that boot or a producer's identity.

/// Samples only the supported kernel elapsed clock. No process, environment,
/// wall-clock fallback, namespace mutation or persisted native identifier exists.
#[cfg(target_os = "linux")]
pub(super) fn native_boot_millis() -> Option<u64> {
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;
    // The offsets file describes the leader's time-for-children namespace,
    // which can differ from its current clock after unshare(CLONE_NEWTIME).
    // Hold every namespace object across reading/sampling to fence identity.
    let thread = std::fs::File::open("/proc/thread-self/ns/time").ok()?;
    let leader = std::fs::File::open("/proc/self/ns/time").ok()?;
    let children = std::fs::File::open("/proc/self/ns/time_for_children").ok()?;
    let identity = |metadata: std::fs::Metadata| (metadata.dev(), metadata.ino());
    let captured = identity(thread.metadata().ok()?);
    if !namespace_epoch_matches(
        captured,
        [
            captured,
            identity(leader.metadata().ok()?),
            identity(children.metadata().ok()?),
        ],
    ) {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open("/proc/self/timens_offsets")
        .ok()?
        .take(257)
        .read_to_end(&mut bytes)
        .ok()?;
    if !zero_offsets(&bytes) {
        return None;
    }
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: fixed read-only clock ID and initialized writable timespec.
    if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut time) } != 0 {
        return None;
    }
    if !namespace_epoch_matches(
        captured,
        [
            identity(std::fs::metadata("/proc/thread-self/ns/time").ok()?),
            identity(std::fs::metadata("/proc/self/ns/time").ok()?),
            identity(std::fs::metadata("/proc/self/ns/time_for_children").ok()?),
        ],
    ) {
        return None;
    }
    timespec_millis(time.tv_sec, time.tv_nsec)
}

/// Qualification boundary for sampled worker/current leader/offset-owner
/// namespaces. Tuple equality includes both device and inode; no name, caller
/// selector or parsed zero offset can replace the native object association.
#[cfg(any(target_os = "linux", test))]
fn namespace_epoch_matches(captured: (u64, u64), observed: [(u64, u64); 3]) -> bool {
    observed.iter().all(|identity| *identity == captured)
}

/// Uses the public continuous-clock ABI without deprecated libc Mach wrappers.
/// Conversion checks both timebase factors and wide arithmetic before narrowing.
#[cfg(target_os = "macos")]
pub(super) fn native_boot_millis() -> Option<u64> {
    /// Public mach_timebase_info_data_t layout from mach/mach_time.h.
    #[repr(C)]
    struct Timebase {
        /// Numerator converting continuous ticks into nanoseconds.
        numer: u32,
        /// Nonzero denominator for the same exact conversion.
        denom: u32,
    }
    unsafe extern "C" {
        /// Public suspend-inclusive native counter, available since macOS 10.12.
        fn mach_continuous_time() -> u64;
        /// Fills the exact public timebase structure; zero return means success.
        #[link_name = "mach_timebase_info"]
        fn native_timebase(info: *mut Timebase) -> libc::c_int;
    }
    let mut info = Timebase { numer: 0, denom: 0 };
    // SAFETY: public macOS ABI, initialized correctly sized C structure; these
    // are read-only native time queries, not capabilities or process operations.
    if unsafe { native_timebase(&mut info) } != 0 {
        return None;
    }
    // SAFETY: public macOS >=10.12 zero-argument continuous-time API.
    tick_millis(unsafe { mach_continuous_time() }, info.numer, info.denom)
}

/// Unsupported operating systems cannot manufacture a comparable native clock.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn native_boot_millis() -> Option<u64> {
    None
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
