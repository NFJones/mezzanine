//! Runtime render time and uptime formatting helpers.
//!
//! This module owns wall-clock and system-uptime text used by render frame
//! context assembly. Keeping these helpers separate avoids mixing platform
//! status reads into the render facade.

#[cfg(any(test, target_os = "macos"))]
use std::time::Duration;
#[cfg(target_os = "macos")]
use std::time::{SystemTime, UNIX_EPOCH};

/// Returns the current local time formatted for status bars.
pub(super) fn runtime_local_datetime_seconds_string() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Returns a human-readable system uptime string for status bars.
pub(super) fn runtime_human_system_uptime() -> String {
    runtime_system_uptime_seconds()
        .map(runtime_format_human_duration)
        .unwrap_or_else(|| "uptime unknown".to_string())
}

/// Reads system uptime from the platform's host uptime source.
#[cfg(target_os = "linux")]
fn runtime_system_uptime_seconds() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/uptime").ok()?;
    runtime_linux_uptime_seconds(&text)
}

/// Reads system boot time from the macOS kernel and computes elapsed seconds.
#[cfg(target_os = "macos")]
fn runtime_system_uptime_seconds() -> Option<u64> {
    let mut name = [libc::CTL_KERN, libc::KERN_BOOTTIME];
    let mut boot_time = std::mem::MaybeUninit::<libc::timeval>::zeroed();
    let mut boot_time_size = std::mem::size_of::<libc::timeval>();
    // SAFETY: `name` is the fixed two-element KERN_BOOTTIME MIB, `boot_time`
    // points to writable storage of exactly the reported size, and this query
    // passes no replacement value to the kernel.
    let result = unsafe {
        libc::sysctl(
            name.as_mut_ptr(),
            name.len() as libc::c_uint,
            boot_time.as_mut_ptr().cast(),
            &mut boot_time_size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 || boot_time_size != std::mem::size_of::<libc::timeval>() {
        return None;
    }
    // SAFETY: a successful fixed-size sysctl call initialized the complete
    // `timeval` value checked above.
    let boot_time = unsafe { boot_time.assume_init() };
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    runtime_uptime_seconds_from_epoch_parts(
        i128::from(boot_time.tv_sec),
        i128::from(boot_time.tv_usec),
        now,
    )
}

/// Unsupported hosts retain the existing visible fallback text.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn runtime_system_uptime_seconds() -> Option<u64> {
    None
}

/// Parses the first `/proc/uptime` token without rounding fractional seconds.
#[cfg(any(test, target_os = "linux"))]
fn runtime_linux_uptime_seconds(text: &str) -> Option<u64> {
    let token = text.split_whitespace().next()?;
    let (whole, fraction) = token
        .split_once('.')
        .map_or((token, None), |(whole, fraction)| (whole, Some(fraction)));
    if whole.is_empty() || !whole.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if fraction.is_some_and(|fraction| {
        fraction.is_empty() || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    }) {
        return None;
    }
    whole.parse::<u64>().ok()
}

/// Computes elapsed whole seconds from a boot timestamp and current epoch time.
#[cfg(any(test, target_os = "macos"))]
fn runtime_uptime_seconds_from_epoch_parts(
    boot_seconds: i128,
    boot_microseconds: i128,
    now: Duration,
) -> Option<u64> {
    if boot_seconds < 0 || !(0..1_000_000).contains(&boot_microseconds) {
        return None;
    }
    let boot_total_microseconds = boot_seconds
        .checked_mul(1_000_000)?
        .checked_add(boot_microseconds)?;
    let now_total_microseconds = i128::from(now.as_secs())
        .checked_mul(1_000_000)?
        .checked_add(i128::from(now.subsec_micros()))?;
    let elapsed_microseconds = now_total_microseconds.checked_sub(boot_total_microseconds)?;
    if elapsed_microseconds < 0 {
        return None;
    }
    u64::try_from(elapsed_microseconds / 1_000_000).ok()
}

/// Formats a duration in seconds for compact status-bar display.
///
/// # Parameters
/// - `seconds`: Duration to render.
fn runtime_format_human_duration(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    if days > 0 {
        format!("{days}d {hours:02}h {minutes:02}m")
    } else if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies Linux uptime parsing truncates valid fractional seconds while
    /// rejecting empty, signed, non-numeric, and malformed fractional tokens.
    #[test]
    fn linux_uptime_parser_accepts_only_valid_first_tokens() {
        assert_eq!(runtime_linux_uptime_seconds("123.45 99.0\n"), Some(123));
        assert_eq!(runtime_linux_uptime_seconds("0 0\n"), Some(0));
        for invalid in ["", ".5 0", "1. 0", "-1.0 0", "1.x 0", "nan 0"] {
            assert_eq!(runtime_linux_uptime_seconds(invalid), None, "{invalid:?}");
        }
    }

    /// Verifies boot-time arithmetic preserves microsecond ordering, floors to
    /// whole elapsed seconds, and rejects negative, invalid, or future values.
    #[test]
    fn uptime_arithmetic_rejects_invalid_and_future_boot_times() {
        let now = Duration::new(10_000, 750_000_000);
        assert_eq!(
            runtime_uptime_seconds_from_epoch_parts(9_000, 250_000, now),
            Some(1_000)
        );
        assert_eq!(
            runtime_uptime_seconds_from_epoch_parts(10_000, 700_000, now),
            Some(0)
        );
        assert_eq!(runtime_uptime_seconds_from_epoch_parts(-1, 0, now), None);
        assert_eq!(
            runtime_uptime_seconds_from_epoch_parts(9_000, 1_000_000, now),
            None
        );
        assert_eq!(
            runtime_uptime_seconds_from_epoch_parts(10_001, 0, now),
            None
        );
    }

    /// Verifies compact uptime rendering retains the established seconds,
    /// minutes, hours, and day boundary forms used by status templates.
    #[test]
    fn human_duration_format_preserves_status_boundaries() {
        assert_eq!(runtime_format_human_duration(59), "59s");
        assert_eq!(runtime_format_human_duration(60), "1m");
        assert_eq!(runtime_format_human_duration(3_599), "59m");
        assert_eq!(runtime_format_human_duration(3_600), "1h 00m");
        assert_eq!(runtime_format_human_duration(90_061), "1d 01h 01m");
    }

    /// Verifies the real macOS KERN_BOOTTIME source returns a plausible
    /// positive host uptime without relying on an exact wall-clock value.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_uptime_source_reports_plausible_elapsed_seconds() {
        let uptime = runtime_system_uptime_seconds().expect("macOS boot time should be readable");
        assert!(uptime > 0);
        assert!(uptime < 200 * 366 * 86_400);
    }

    /// Verifies a Linux host's production `/proc` source parses successfully
    /// when the platform exposes the expected kernel file.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_uptime_source_reads_proc_uptime() {
        assert!(runtime_system_uptime_seconds().is_some());
    }
}
