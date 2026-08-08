//! Native process metadata used to describe live pane foreground jobs.
//!
//! Pane shells may move interactive jobs into separate foreground process
//! groups. The multiplexer queries the group leader directly so agent-shell
//! bootstrap certification and pane titles reflect the actual live process.
//! Linux reads procfs, while macOS uses libproc. Other targets fail softly by
//! returning no metadata; callers retain their recorded spawn directory and
//! avoid treating best-effort host inspection as authoritative.

use std::path::PathBuf;

/// Returns the host-reported short process name for `pid` when available.
#[cfg(target_os = "linux")]
pub(super) fn process_name_for_pid(pid: u32) -> Option<String> {
    let name = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    let name = name
        .trim_end_matches('\n')
        .trim_end_matches('\r')
        .to_string();
    (!name.is_empty()).then_some(name)
}

/// Returns the Darwin libproc short process name for `pid` when available.
#[cfg(target_os = "macos")]
pub(super) fn process_name_for_pid(pid: u32) -> Option<String> {
    let pid = libc::c_int::try_from(pid).ok()?;
    let mut buffer = [0_u8; 256];
    // SAFETY: libproc receives a valid writable byte buffer for the duration of
    // the call. The returned length is checked before indexing the buffer.
    let length = unsafe {
        libc::proc_name(
            pid,
            buffer.as_mut_ptr().cast(),
            u32::try_from(buffer.len()).ok()?,
        )
    };
    let length = usize::try_from(length).ok()?;
    let name = std::str::from_utf8(buffer.get(..length)?).ok()?.to_string();
    (!name.is_empty()).then_some(name)
}

/// Returns no process name on hosts without a reviewed native implementation.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn process_name_for_pid(_pid: u32) -> Option<String> {
    None
}

/// Returns the procfs current working directory for `pid` when available.
#[cfg(target_os = "linux")]
pub(super) fn current_working_directory_for_pid(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// Returns the Darwin libproc current working directory for `pid`.
#[cfg(target_os = "macos")]
pub(super) fn current_working_directory_for_pid(pid: u32) -> Option<PathBuf> {
    use std::ffi::CStr;
    use std::mem::{MaybeUninit, size_of};
    use std::os::unix::ffi::OsStrExt;

    let pid = libc::c_int::try_from(pid).ok()?;
    let mut info = MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
    let info_size = libc::c_int::try_from(size_of::<libc::proc_vnodepathinfo>()).ok()?;
    // SAFETY: libproc receives a correctly sized writable structure. The
    // structure is initialized only when the call reports the complete size.
    let length = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            info.as_mut_ptr().cast(),
            info_size,
        )
    };
    if length != info_size {
        return None;
    }
    // SAFETY: the exact structure size was initialized successfully above.
    let info = unsafe { info.assume_init() };
    // SAFETY: Darwin's vnode path is a fixed NUL-terminated C character array
    // on successful PROC_PIDVNODEPATHINFO queries.
    let path = unsafe { CStr::from_ptr(info.pvi_cdir.vip_path.as_ptr().cast()) };
    if path.to_bytes().is_empty() {
        return None;
    }
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(path.to_bytes())))
}

/// Returns no live working directory without a reviewed native implementation.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn current_working_directory_for_pid(_pid: u32) -> Option<PathBuf> {
    None
}
