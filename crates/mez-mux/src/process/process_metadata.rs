//! Native process metadata used to describe live pane foreground jobs.
//!
//! Pane shells may move interactive jobs into separate foreground process
//! groups. The multiplexer queries the group leader directly so agent-shell
//! bootstrap certification and pane titles reflect the actual live process.
//! The module also exposes the primary process executable path and exec-time
//! environment so native-mode execution can infer shell context without ever
//! running commands through the pane. Linux reads procfs, while macOS uses
//! libproc and `KERN_PROCARGS2`. Other targets fail softly by returning no
//! metadata; callers retain their recorded spawn directory and avoid
//! treating best-effort host inspection as authoritative.

use std::path::PathBuf;

/// One raw environment entry with unvalidated key and value bytes.
///
/// Host environment readers must not require UTF-8: valid POSIX environments
/// can contain arbitrary non-NUL bytes in values. Consumers decode or match
/// bytes only where their own contract needs text, and treat environment
/// contents as protected runtime state that must never be logged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawEnvironmentEntry {
    /// Raw entry key bytes without the `=` separator.
    pub key: Vec<u8>,
    /// Raw entry value bytes; may be empty and may contain non-UTF-8 bytes.
    pub value: Vec<u8>,
}

/// Parses raw NUL-separated KEY=VALUE environment bytes into entries.
///
/// Segments without `=` (or with an empty key) are skipped so truncated or
/// host-padded regions degrade to the well-formed subset instead of
/// producing invalid entries. Values may contain arbitrary non-NUL bytes.
pub(super) fn parse_environment_bytes(bytes: &[u8]) -> Vec<RawEnvironmentEntry> {
    bytes
        .split(|byte| *byte == 0)
        .filter_map(|segment| {
            let equals = segment.iter().position(|byte| *byte == b'=')?;
            let (key, value) = segment.split_at(equals);
            if key.is_empty() {
                return None;
            }
            Some(RawEnvironmentEntry {
                key: key.to_vec(),
                value: value[1..].to_vec(),
            })
        })
        .collect()
}

/// Reports whether a raw environment region contains at least one well-formed
/// KEY=VALUE assignment.
///
/// This validates candidate macOS argv/environment splits without trusting
/// the platform-specific argc encoding.
#[cfg(any(target_os = "macos", test))]
fn environment_region_looks_valid(bytes: &[u8]) -> bool {
    // The region must begin with a well-formed KEY=VALUE assignment; any
    // argv padding or leftover argv strings at the front indicate a wrong
    // candidate offset and must not validate.
    bytes.split(|byte| *byte == 0).next().is_some_and(|first| {
        !first.is_empty() && first.first() != Some(&b'=') && first.contains(&b'=')
    })
}

/// Locates the environment region inside a raw `KERN_PROCARGS2` buffer.
///
/// The buffer layout is a leading argc field followed by NUL-terminated argv
/// strings, an empty-string end-of-argv marker, and the NUL-separated
/// environment. The kernel encodes argc differently on Apple Silicon than on
/// Intel, so the field cannot be parsed portably; instead the argv region is
/// skipped by scanning for the end-of-argv empty string from either candidate
/// offset and validating the trailing region against the KEY=VALUE contract.
#[cfg(any(target_os = "macos", test))]
pub(super) fn parse_macos_environment_bytes(buffer: &[u8]) -> Option<&[u8]> {
    if buffer.len() <= 8 {
        return None;
    }
    for argv_start in [4_usize, 8_usize] {
        let mut position = argv_start;
        loop {
            let remainder = buffer.get(position..)?;
            let relative_end = remainder.iter().position(|byte| *byte == 0)?;
            let string_end = position + relative_end;
            if string_end == position {
                // Empty string: the end-of-argv marker.
                break;
            }
            position = string_end + 1;
        }
        let environment_region = buffer.get(position + 1..)?;
        if environment_region_looks_valid(environment_region) {
            return Some(environment_region);
        }
    }
    None
}

/// Returns the procfs executable path for `pid` when available.
#[cfg(target_os = "linux")]
pub(super) fn process_executable_path_for_pid(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

/// Returns the Darwin libproc executable path for `pid`.
#[cfg(target_os = "macos")]
pub(super) fn process_executable_path_for_pid(pid: u32) -> Option<PathBuf> {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let pid = libc::c_int::try_from(pid).ok()?;
    let mut buffer = [0_u8; 4096];
    // SAFETY: libproc writes at most `buffersize` bytes into the buffer and
    // reports the byte length on success; a non-positive result is failure.
    let length = unsafe {
        libc::proc_pidpath(
            pid,
            buffer.as_mut_ptr().cast(),
            u32::try_from(buffer.len()).ok()?,
        )
    };
    if length <= 0 {
        return None;
    }
    let length = usize::try_from(length).ok()?;
    let path = buffer.get(..length)?;
    (!path.is_empty()).then(|| PathBuf::from(OsStr::from_bytes(path)))
}

/// Returns no executable path on hosts without a reviewed native reader.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn process_executable_path_for_pid(_pid: u32) -> Option<PathBuf> {
    None
}

/// Maximum raw bytes accepted from a process environment source.
///
/// Real process environments are far smaller; the cap bounds host reads so a
/// misbehaving or exotic process cannot drive unbounded allocation.
const PROCESS_ENVIRONMENT_READ_CAP: u64 = 1024 * 1024;

/// Returns the procfs exec-time environment for `pid` when available.
#[cfg(target_os = "linux")]
pub(super) fn process_environment_for_pid(pid: u32) -> Option<Vec<RawEnvironmentEntry>> {
    use std::io::Read;

    let file = std::fs::File::open(format!("/proc/{pid}/environ")).ok()?;
    let mut bytes = Vec::new();
    file.take(PROCESS_ENVIRONMENT_READ_CAP + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    // Reading past the cap means the environment was truncated and therefore
    // cannot be trusted as the complete contract for a spawned child.
    if bytes.len() as u64 > PROCESS_ENVIRONMENT_READ_CAP {
        return None;
    }
    Some(parse_environment_bytes(&bytes))
}

/// Maximum bytes accepted from a Darwin `KERN_PROCARGS2` environment query.
#[cfg(target_os = "macos")]
const MACOS_PROCARGS2_READ_CAP: usize = 8 * 1024 * 1024;

/// Returns the Darwin `KERN_PROCARGS2` exec-time environment for `pid`.
#[cfg(target_os = "macos")]
pub(super) fn process_environment_for_pid(pid: u32) -> Option<Vec<RawEnvironmentEntry>> {
    let pid = libc::c_int::try_from(pid).ok()?;
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size: usize = 0;
    // SAFETY: the mib slice is valid for the call duration and `size` is a
    // writable usize; a NULL oldp query reports the required buffer size.
    let status = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 || size == 0 || size > MACOS_PROCARGS2_READ_CAP {
        return None;
    }
    let mut buffer = vec![0_u8; size];
    // SAFETY: `buffer` is writable for `size` bytes and the mib slice is
    // valid for the call duration.
    let status = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            buffer.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 {
        return None;
    }
    buffer.truncate(size);
    Some(parse_environment_bytes(parse_macos_environment_bytes(
        &buffer,
    )?))
}

/// Returns no environment on hosts without a reviewed native reader.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn process_environment_for_pid(_pid: u32) -> Option<Vec<RawEnvironmentEntry>> {
    None
}

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
