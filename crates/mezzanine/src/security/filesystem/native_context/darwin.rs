//! Darwin native context from public kernel process and vnode APIs.
//!
//! An SDK-compiled shim reads kinfo_proc credentials, avoiding a guessed Rust
//! layout and the shell reader's daemon-group approximation. libproc provides
//! the cwd vnode's device/inode/path; a held no-follow open must match it before
//! and after capture. The enclosing root also rechecks process start token,
//! credentials and complete physical ancestry before each preparation/commit.

use crate::error::{MezError, Result};
use mez_mux::process::ProcessCredentials;
use rustix::fs::{FileType, Mode, OFlags, fstat, openat};
use std::fs::File;
use std::mem::{MaybeUninit, size_of};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

/// Stable private shim ABI; kernel/SDK layouts never cross this Rust boundary.
#[repr(C)]
struct CredentialRecord {
    /// Effective target UID from the kernel credential.
    user_id: u32,
    /// Effective target GID, the first kernel credential group.
    primary_group_id: u32,
    /// Initialized entries, validated independently after the FFI return.
    group_count: u32,
    /// Finite transfer bound; SDK compile and runtime checks prohibit truncation.
    groups: [u32; 64],
}
unsafe extern "C" {
    /// Initializes one complete private record or returns -1 with errno.
    fn mez_darwin_native_credentials(
        pid: libc::pid_t,
        output: *mut CredentialRecord,
    ) -> libc::c_int;
}

/// Reads exact kernel credential groups for the requested process. No lookup
/// falls back to the calling daemon's identities; invalid/absent/truncated
/// evidence is an error, including a failed or partial sysctl operation.
pub(super) fn credentials(pid: u32) -> Result<ProcessCredentials> {
    let pid = libc::pid_t::try_from(pid)
        .ok()
        .filter(|pid| *pid > 0)
        .ok_or_else(|| MezError::invalid_args("native credential PID is invalid"))?;
    let mut record = MaybeUninit::<CredentialRecord>::zeroed();
    // SAFETY: the shim receives a correctly sized/aligned writable repr(C)
    // record. Zero initialization also makes every field safe to inspect; only
    // complete success is consumed and the count is checked before slicing.
    if unsafe { mez_darwin_native_credentials(pid, record.as_mut_ptr()) } != 0 {
        return Err(MezError::from(std::io::Error::last_os_error())
            .with_context("native Darwin target credentials unavailable"));
    }
    // SAFETY: successful shim return initialized the complete fixed record.
    let record = unsafe { record.assume_init() };
    let count = usize::try_from(record.group_count)
        .ok()
        .filter(|count| *count > 0 && *count <= record.groups.len())
        .ok_or_else(|| MezError::invalid_state("native Darwin group evidence is truncated"))?;
    if record.groups[0] != record.primary_group_id {
        return Err(MezError::invalid_state(
            "native Darwin primary group evidence changed",
        ));
    }
    Ok(ProcessCredentials {
        user_id: record.user_id,
        primary_group_id: record.primary_group_id,
        supplementary_group_ids: record.groups[..count]
            .iter()
            .copied()
            .filter(|group| *group != record.primary_group_id)
            .collect(),
    })
}

/// Exact kernel cwd identity, including path bytes without lossy UTF-8 conversion.
#[derive(PartialEq, Eq)]
struct KernelDirectory {
    /// Current kernel-reported physical cwd path, required to be absolute.
    path: PathBuf,
    /// Darwin device bit pattern; fstat's signed representation is matched exactly.
    device: u32,
    /// Kernel vnode inode, compared while the opened descriptor remains held.
    inode: u64,
}

/// Reads a complete libproc vnode record, rejecting incomplete/non-directory
/// results and unterminated paths rather than scanning outside its fixed buffer.
fn kernel_directory(pid: u32) -> Result<KernelDirectory> {
    let pid = libc::pid_t::try_from(pid)
        .ok()
        .filter(|pid| *pid > 0)
        .ok_or_else(|| MezError::invalid_args("native cwd PID is invalid"))?;
    let capacity = libc::c_int::try_from(size_of::<libc::proc_vnodepathinfo>())
        .map_err(|_| MezError::invalid_state("native Darwin vnode record is too large"))?;
    let mut record = MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
    // SAFETY: libproc gets a correctly sized writable full record. Only its
    // exact complete-size success initializes data consumed below.
    let length = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            record.as_mut_ptr().cast(),
            capacity,
        )
    };
    if length != capacity {
        return Err(MezError::invalid_state(
            "native Darwin kernel cwd record unavailable",
        ));
    }
    // SAFETY: the exact record size was initialized successfully above.
    let record = unsafe { record.assume_init() };
    let stat = record.pvi_cdir.vip_vi.vi_stat;
    if FileType::from_raw_mode(stat.vst_mode) != FileType::Directory {
        return Err(MezError::invalid_state(
            "native Darwin cwd vnode is not a directory",
        ));
    }
    let bytes = record
        .pvi_cdir
        .vip_path
        .iter()
        .flatten()
        .map(|value| value.to_ne_bytes()[0])
        .collect::<Vec<_>>();
    let end = bytes
        .iter()
        .position(|value| *value == 0)
        .ok_or_else(|| MezError::invalid_state("native Darwin cwd path is unterminated"))?;
    let path = PathBuf::from(std::ffi::OsStr::from_bytes(&bytes[..end]));
    if !path.is_absolute() {
        return Err(MezError::invalid_state(
            "native Darwin kernel cwd path is unavailable",
        ));
    }
    Ok(KernelDirectory {
        path,
        device: stat.vst_dev,
        inode: stat.vst_ino,
    })
}

/// Holds the exact vnode observed in the target's kernel cwd, then reobserves
/// the original target to reject path/object replacement during the open.
pub(super) fn open_process_cwd(pid: u32) -> Result<File> {
    let original = kernel_directory(pid)?;
    let file = File::from(
        openat(
            rustix::fs::CWD,
            &original.path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(std::io::Error::from)?,
    );
    let stat = fstat(&file).map_err(std::io::Error::from)?;
    // Darwin's signed dev_t and libproc's u32 encode the same 32 bits. A byte
    // reinterpretation avoids narrowing or rejecting legitimate high-bit IDs.
    if stat.st_dev != i32::from_ne_bytes(original.device.to_ne_bytes())
        || stat.st_ino != original.inode
        || FileType::from_raw_mode(stat.st_mode) != FileType::Directory
        || kernel_directory(pid)? != original
    {
        return Err(MezError::conflict(
            "native Darwin cwd vnode changed during capture",
        ));
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    /// The private C/Rust layout remains a fixed uint32 record, independent of
    /// SDK kinfo_proc pointer/layout changes on the supported 64-bit targets.
    #[test]
    fn private_credential_record_layout_is_fixed() {
        assert_eq!(size_of::<CredentialRecord>(), 67 * size_of::<u32>());
    }

    /// Actual kernel credentials and vnode evidence must agree for an owned
    /// ordinary child. Missing/dead PID evidence must never borrow our groups.
    #[test]
    fn actual_foreign_child_context_and_dead_pid_are_distinct() {
        let mut child = std::process::Command::new("/bin/cat")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let pid = child.id();
        let original = credentials(pid);
        let caller = credentials(std::process::id());
        let opened = open_process_cwd(pid);
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(original.unwrap(), caller.unwrap());
        let file = opened.unwrap();
        let kernel = fstat(&file).unwrap();
        assert_eq!(
            crate::security::filesystem::stat_device(&kernel),
            file.metadata().unwrap().dev()
        );
        assert_eq!(kernel.st_ino, file.metadata().unwrap().ino());
        assert!(credentials(pid).is_err());
        assert!(open_process_cwd(pid).is_err());
        assert!(credentials(0).is_err());
        assert!(credentials(u32::MAX).is_err());
    }
}
