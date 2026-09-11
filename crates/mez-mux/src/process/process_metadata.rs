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

/// Locates the environment region inside a raw `KERN_PROCARGS2` buffer.
///
/// The buffer layout is a 32-bit argc field, the executable path, NUL padding,
/// exactly argc NUL-terminated argv strings, and the NUL-separated environment.
/// Counting argv is required because there is no distinct empty-string marker
/// between the final argument and the first environment entry.
#[cfg(any(target_os = "macos", test))]
pub(super) fn parse_macos_environment_bytes(buffer: &[u8]) -> Option<&[u8]> {
    let argc = i32::from_ne_bytes(buffer.get(..4)?.try_into().ok()?);
    let argc = usize::try_from(argc).ok()?;
    let mut position = 4;

    // Skip the executable path stored separately from argv.
    let executable_end = buffer.get(position..)?.iter().position(|byte| *byte == 0)?;
    position += executable_end + 1;

    // Darwin pads between the executable path and argv[0] with NUL bytes.
    while buffer.get(position) == Some(&0) {
        position += 1;
    }

    for _ in 0..argc {
        let argument_end = buffer.get(position..)?.iter().position(|byte| *byte == 0)?;
        position += argument_end + 1;
    }

    buffer.get(position..)
}

/// Returns the procfs executable path for `pid` when available.
#[cfg(target_os = "linux")]
pub fn process_executable_path_for_pid(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

/// Returns the Darwin libproc executable path for `pid`.
#[cfg(target_os = "macos")]
pub fn process_executable_path_for_pid(pid: u32) -> Option<PathBuf> {
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
pub fn process_executable_path_for_pid(_pid: u32) -> Option<PathBuf> {
    None
}

/// One host-observed process instance bounded by its executable path.
///
/// The start token is the kernel-reported creation time for one pid. Pairing
/// it with the executable path lets callers detect a pid replacement instead
/// of attributing one process's executable to another process's lifetime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInstanceIdentity {
    /// Process id that supplied both observations.
    pub process_id: u32,
    /// Kernel-reported opaque start token for that pid.
    pub start_token: u64,
    /// Absolute executable path resolved by the host kernel.
    pub executable_path: PathBuf,
}

/// Typed reason one live process instance identity could not be observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessInstanceIdentityUnavailable {
    /// The pid's creation token changed between the executable path read and
    /// the re-observation, so a replacement process now owns the pid.
    StartTokenChanged,
    /// The host exposed no creation token or executable path for the pid.
    Unreadable,
}

/// Parses the creation-time token from one Linux `/proc/<pid>/stat` record.
///
/// Field 22 (`starttime`) is the opaque token. Field 2 (the command name) is
/// enclosed in parentheses and can itself contain spaces and parentheses, so
/// only the final closing parenthesis can delimit the command field.
#[cfg(any(target_os = "linux", test))]
pub(super) fn parse_linux_stat_start_token(stat: &str) -> Option<u64> {
    let command_end = stat.rfind(')')?;
    // The field after the command is field 3 (state); field 22 is therefore the
    // twentieth whitespace-separated field after the command.
    stat.get(command_end + 1..)?
        .split_whitespace()
        .nth(19)
        .and_then(|field| field.parse().ok())
}

/// Returns the Linux procfs creation-time token for `pid` when available.
#[cfg(target_os = "linux")]
pub fn process_start_token_for_pid(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_linux_stat_start_token(&stat)
}

/// Returns the Darwin libproc creation-time token for `pid` when available.
#[cfg(target_os = "macos")]
pub fn process_start_token_for_pid(pid: u32) -> Option<u64> {
    use std::mem::{MaybeUninit, size_of};

    let pid = libc::c_int::try_from(pid).ok()?;
    let mut info = MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let info_size = libc::c_int::try_from(size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: libproc receives a correctly sized writable BSD-info structure.
    // The structure is initialized only when the call reports the full size.
    let length = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
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
    u64::from(info.pbi_start_tvsec)
        .checked_mul(1_000_000)?
        .checked_add(u64::from(info.pbi_start_tvusec))
}

/// Returns no creation-time token on hosts without a reviewed native reader.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn process_start_token_for_pid(_pid: u32) -> Option<u64> {
    None
}

/// Returns the host-verified identity for one live process instance.
///
/// The executable path is always paired with a creation-time token re-read
/// after the path. A pid replacement between the two observations yields
/// [`ProcessInstanceIdentityUnavailable::StartTokenChanged`] instead of a
/// mixed identity, and a failed host read yields
/// [`ProcessInstanceIdentityUnavailable::Unreadable`].
#[cfg(target_os = "linux")]
pub fn process_executable_identity_for_pid(
    pid: u32,
) -> Result<ProcessInstanceIdentity, ProcessInstanceIdentityUnavailable> {
    let start_token =
        process_start_token_for_pid(pid).ok_or(ProcessInstanceIdentityUnavailable::Unreadable)?;
    let executable_path = std::fs::read_link(format!("/proc/{pid}/exe"))
        .map_err(|_| ProcessInstanceIdentityUnavailable::Unreadable)?;
    identity_from_reobservation(
        pid,
        start_token,
        executable_path,
        process_start_token_for_pid(pid),
    )
}

/// Returns the host-verified identity for one live Darwin process instance.
#[cfg(target_os = "macos")]
pub fn process_executable_identity_for_pid(
    pid: u32,
) -> Result<ProcessInstanceIdentity, ProcessInstanceIdentityUnavailable> {
    let start_token =
        process_start_token_for_pid(pid).ok_or(ProcessInstanceIdentityUnavailable::Unreadable)?;
    let executable_path = process_executable_path_for_pid(pid)
        .ok_or(ProcessInstanceIdentityUnavailable::Unreadable)?;
    identity_from_reobservation(
        pid,
        start_token,
        executable_path,
        process_start_token_for_pid(pid),
    )
}

/// Returns no process instance identity without a reviewed native reader.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn process_executable_identity_for_pid(
    _pid: u32,
) -> Result<ProcessInstanceIdentity, ProcessInstanceIdentityUnavailable> {
    Err(ProcessInstanceIdentityUnavailable::Unreadable)
}

/// Builds one identity only when the re-observed token anchors the path read.
///
/// A mismatch, or a missing re-observation because the pid vanished mid-read,
/// is a typed pid replacement rather than an unreadable identity.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn identity_from_reobservation(
    process_id: u32,
    start_token: u64,
    executable_path: PathBuf,
    reobserved_start_token: Option<u64>,
) -> Result<ProcessInstanceIdentity, ProcessInstanceIdentityUnavailable> {
    if reobserved_start_token == Some(start_token) {
        Ok(ProcessInstanceIdentity {
            process_id,
            start_token,
            executable_path,
        })
    } else {
        Err(ProcessInstanceIdentityUnavailable::StartTokenChanged)
    }
}

/// Maximum raw bytes accepted from a Linux procfs environment source.
///
/// Real process environments are far smaller; the cap bounds host reads so a
/// misbehaving or exotic process cannot drive unbounded allocation.
#[cfg(target_os = "linux")]
const PROCESS_ENVIRONMENT_READ_CAP: u64 = 1024 * 1024;

/// Returns the procfs exec-time environment for `pid` when available.
#[cfg(target_os = "linux")]
pub fn process_environment_for_pid(pid: u32) -> Option<Vec<RawEnvironmentEntry>> {
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
pub fn process_environment_for_pid(pid: u32) -> Option<Vec<RawEnvironmentEntry>> {
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
pub fn process_environment_for_pid(_pid: u32) -> Option<Vec<RawEnvironmentEntry>> {
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
pub fn current_working_directory_for_pid(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// Returns the Darwin libproc current working directory for `pid`.
#[cfg(target_os = "macos")]
pub fn current_working_directory_for_pid(pid: u32) -> Option<PathBuf> {
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
pub fn current_working_directory_for_pid(_pid: u32) -> Option<PathBuf> {
    None
}

/// Host-reported process credentials for one pid.
///
/// The values are read from the host kernel, not from shell commands, so
/// native shell mode can resolve sandbox identity without touching the pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessCredentials {
    /// Effective user ID.
    pub user_id: u32,
    /// Effective primary group ID.
    pub primary_group_id: u32,
    /// Supplementary group IDs reported by the host, excluding the primary.
    pub supplementary_group_ids: Vec<u32>,
}

/// Returns the procfs process credentials for `pid` when available.
#[cfg(target_os = "linux")]
pub fn process_credentials_for_pid(pid: u32) -> Option<ProcessCredentials> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let mut user_id = None;
    let mut primary_group_id = None;
    let mut supplementary_group_ids = Vec::new();
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("Uid:") {
            user_id = Some(rest.split_whitespace().nth(1)?.parse().ok()?);
        } else if let Some(rest) = line.strip_prefix("Gid:") {
            primary_group_id = Some(rest.split_whitespace().nth(1)?.parse().ok()?);
        } else if let Some(rest) = line.strip_prefix("Groups:") {
            supplementary_group_ids = rest
                .split_whitespace()
                .map(|field| field.parse())
                .collect::<Result<Vec<u32>, _>>()
                .ok()?;
        }
    }
    let user_id = user_id?;
    let primary_group_id = primary_group_id?;
    supplementary_group_ids.retain(|group_id| *group_id != primary_group_id);
    Some(ProcessCredentials {
        user_id,
        primary_group_id,
        supplementary_group_ids,
    })
}

/// Returns the Darwin libproc process credentials for `pid` when available.
#[cfg(target_os = "macos")]
pub fn process_credentials_for_pid(pid: u32) -> Option<ProcessCredentials> {
    use std::mem::{MaybeUninit, size_of};

    let pid = libc::c_int::try_from(pid).ok()?;
    let mut info = MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let info_size = libc::c_int::try_from(size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: libproc receives a correctly sized writable full BSD-info
    // structure. The structure is initialized only when the call reports the
    // complete size.
    let length = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
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
    let user_id = info.pbi_uid;
    let primary_group_id = info.pbi_gid;
    // Darwin cannot enumerate another process's supplementary groups, so
    // reuse the mez process groups only when the target shares its identity.
    // SAFETY: geteuid reports the mez process effective user ID.
    if user_id != unsafe { libc::geteuid() } {
        return None;
    }
    // SAFETY: a null buffer with capacity zero queries the required count.
    let capacity = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    if capacity < 0 {
        return None;
    }
    let mut buffer = vec![0 as libc::gid_t; usize::try_from(capacity).ok()?];
    // SAFETY: the buffer is writable for `capacity` gid entries.
    let count = unsafe { libc::getgroups(capacity, buffer.as_mut_ptr()) };
    if count < 0 {
        return None;
    }
    buffer.truncate(usize::try_from(count).ok()?);
    buffer.retain(|group_id| *group_id != primary_group_id);
    Some(ProcessCredentials {
        user_id,
        primary_group_id,
        supplementary_group_ids: buffer,
    })
}

/// Returns no process credentials on hosts without a reviewed native reader.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn process_credentials_for_pid(_pid: u32) -> Option<ProcessCredentials> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies the stat parser isolates field 22 even when the command name
    /// contains spaces and parentheses, which shift naive whitespace splits.
    #[test]
    fn parses_creation_token_from_stat_with_hostile_command_names() {
        let plain = "7314 (bash) S 1 7314 7314 0 -1 4194560 1 2 3 4 5 6 7 8 9 10 11 12 99999999 14";
        assert_eq!(parse_linux_stat_start_token(plain), Some(99_999_999));

        let hostile = "7314 (weird (name) with spaces) R 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 424242 20";
        assert_eq!(parse_linux_stat_start_token(hostile), Some(424_242));

        assert_eq!(parse_linux_stat_start_token("not a stat record"), None);
        assert_eq!(parse_linux_stat_start_token("7314 (bash) S 1 2"), None);
        assert_eq!(parse_linux_stat_start_token("7314 (bash"), None);
    }

    /// Verifies the live reader returns one stable, absolute identity for a
    /// process that cannot be replaced while the test holds it.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn live_process_identity_is_stable_for_the_current_process() {
        let pid = std::process::id();
        let first = process_start_token_for_pid(pid).expect("live start token");
        let second = process_start_token_for_pid(pid).expect("live start token");
        assert_eq!(first, second);

        let identity = process_executable_identity_for_pid(pid).expect("live identity");
        assert_eq!(identity.process_id, pid);
        assert_eq!(identity.start_token, first);
        assert!(identity.executable_path.is_absolute());
    }

    /// Verifies unreviewed targets fail closed instead of guessing metadata.
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn unsupported_targets_return_no_process_instance_identity() {
        let pid = std::process::id();
        assert_eq!(process_start_token_for_pid(pid), None);
        assert_eq!(
            process_executable_identity_for_pid(pid),
            Err(ProcessInstanceIdentityUnavailable::Unreadable)
        );
    }

    /// Verifies a creation-token change between observations is reported as a
    /// typed pid replacement instead of collapsing into an unreadable
    /// identity, so the pane resolver can settle the replacement reason.
    #[test]
    fn start_token_replacement_is_reported_not_collapsed_to_unreadable() {
        let executable_path = PathBuf::from("/usr/bin/fish");
        let stable = identity_from_reobservation(4242, 7, executable_path.clone(), Some(7))
            .expect("an unchanged token must build the identity");
        assert_eq!(stable.process_id, 4242);
        assert_eq!(stable.start_token, 7);
        assert_eq!(
            identity_from_reobservation(4242, 7, executable_path.clone(), Some(99)),
            Err(ProcessInstanceIdentityUnavailable::StartTokenChanged)
        );
        assert_eq!(
            identity_from_reobservation(4242, 7, executable_path, None),
            Err(ProcessInstanceIdentityUnavailable::StartTokenChanged)
        );
    }
}
