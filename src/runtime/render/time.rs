//! Runtime render time and uptime formatting helpers.
//!
//! This module owns wall-clock and system-uptime text used by render frame
//! context assembly. Keeping these helpers separate avoids mixing platform
//! status reads into the render facade.

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

/// Reads system uptime from `/proc/uptime`.
fn runtime_system_uptime_seconds() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/uptime").ok()?;
    let seconds = text.split_whitespace().next()?;
    seconds.split('.').next()?.parse::<u64>().ok()
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
