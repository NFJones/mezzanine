//! Descriptor-held native file capabilities, not a sandbox for the daemon.
//!
//! Capture exact authorized objects and preimages using no-follow, nonblocking
//! opens. Revalidate live root identity/credentials/cwd, current actor scopes,
//! physical destination and held ancestry before publication. The actor must
//! serialize conflicting Mezzanine writes and supply a fresh commit lease.
//! Revalidation plus rename/unlink is NOT atomic CAS against external writers:
//! an external rename after the final check remains possible. Linux/macOS use
//! the same portable openat primitives; there is no helper or shell fallback.
//! Replacement preserves ordinary mode bits, not ownership, ACLs or xattrs;
//! privilege bits are suppressed. Hard-linked updates replace only this entry.

use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use sha2::{Digest, Sha256};

use mez_agent::permissions::{PathResolutionStatus, PathScopes};
use mez_mux::process::{
    ProcessCredentials, current_working_directory_for_pid, process_credentials_for_pid,
    process_start_token_for_pid,
};
use rustix::fs::{
    AtFlags, FileType, Mode, OFlags, RenameFlags, fstat, openat, renameat, renameat_with, statat,
    unlinkat,
};

use super::resolution::{PhysicalPath, resolve_physical, same_object};
use crate::error::{MezError, Result};

/// Finite per-file snapshot/staging budget; dispatcher also bounds aggregate work.
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// Local process lifetime and filesystem context captured without shell selection.
pub(crate) struct NativeFilesystemRoot {
    /// Kernel process identifier, always paired with start token.
    pid: u32,
    /// Exact process lifetime captured before reading credentials/cwd.
    start_token: u64,
    /// Daemon-compatible effective credentials; never change daemon credentials.
    credentials: ProcessCredentials,
    /// Canonical cwd spelling and held physical identity.
    cwd: PhysicalPath,
}

impl NativeFilesystemRoot {
    /// Captures a live local root with identical daemon credentials. Fails closed
    /// if any metadata is unavailable, replaced or incompatible; no PATH needed.
    pub(crate) fn capture(pid: u32) -> Result<Self> {
        let start_token = process_start_token_for_pid(pid).ok_or_else(|| {
            MezError::invalid_state("native filesystem root lifetime unavailable")
        })?;
        let credentials = normalized_credentials(pid)?;
        if credentials != normalized_credentials(std::process::id())? {
            return Err(MezError::forbidden(
                "native filesystem root credentials differ from daemon",
            ));
        }
        let cwd_path = current_working_directory_for_pid(pid)
            .ok_or_else(|| MezError::invalid_state("native filesystem root cwd unavailable"))?;
        let cwd = resolve_physical(&cwd_path, ".")?;
        if !cwd.missing.is_empty()
            || FileType::from_raw_mode(cwd.stat.st_mode) != FileType::Directory
        {
            return Err(MezError::invalid_state(
                "native filesystem root cwd is not a directory",
            ));
        }
        let root = Self {
            pid,
            start_token,
            credentials,
            cwd,
        };
        root.revalidate()?;
        Ok(root)
    }

    /// Re-observes lifetime, credentials and cwd; moving/replacing cwd revokes
    /// this root rather than extending a descriptor grant to the new location.
    pub(crate) fn revalidate(&self) -> Result<()> {
        if process_start_token_for_pid(self.pid) != Some(self.start_token)
            || normalized_credentials(self.pid)? != self.credentials
            || normalized_credentials(std::process::id())? != self.credentials
        {
            return Err(MezError::conflict(
                "native filesystem process identity changed",
            ));
        }
        let cwd = current_working_directory_for_pid(self.pid)
            .ok_or_else(|| MezError::conflict("native filesystem cwd disappeared"))?;
        let current = resolve_physical(&cwd, ".")?;
        if current.path != self.cwd.path || !same_object(&current.stat, &self.cwd.stat) {
            return Err(MezError::conflict("native filesystem cwd identity changed"));
        }
        revalidate_ancestry(&self.cwd)
    }

    /// Returns captured local cwd; never changes the daemon's process directory.
    pub(crate) fn cwd(&self) -> &Path {
        &self.cwd.path
    }
}

/// Normalizes supplementary groups for exact effective-credential comparison.
fn normalized_credentials(pid: u32) -> Result<ProcessCredentials> {
    // Darwin's existing shell-sandbox reader substitutes daemon supplementary
    // groups for another PID. That approximation cannot authorize daemon-side
    // filesystem access. Only our own process has independently known groups.
    require_exact_group_evidence(!cfg!(target_os = "macos") || pid == std::process::id())?;
    let mut credentials = process_credentials_for_pid(pid)
        .ok_or_else(|| MezError::invalid_state("native filesystem credentials unavailable"))?;
    credentials.supplementary_group_ids.sort_unstable();
    credentials.supplementary_group_ids.dedup();
    Ok(credentials)
}

/// Fails closed when the platform cannot independently observe target groups.
fn require_exact_group_evidence(available: bool) -> Result<()> {
    if !available {
        return Err(MezError::forbidden(
            "native filesystem target supplementary groups cannot be verified on this platform",
        ));
    }
    Ok(())
}

/// Authorized regular-file or missing-entry capability, with immutable preimage.
pub(crate) struct NativeFileCapability {
    /// Original spelling so link retargeting can revoke the captured operation.
    requested: String,
    /// Physical resolution and held parent directories.
    physical: PhysicalPath,
    /// Exact raw preimage; None means missing, Some(empty) means empty file.
    preimage: Option<Vec<u8>>,
    /// Captured scopes must never be widened by a later actor lease.
    scopes: PathScopes,
    /// True when capture authorized mutation as well as snapshot reading.
    writable: bool,
}

impl NativeFileCapability {
    /// Captures only a scoped regular file or missing create target. Existing
    /// files require read authority even for mutation; special nodes fail before
    /// bounded reading so a FIFO/device cannot block the daemon worker.
    pub(crate) fn capture(
        root: &NativeFilesystemRoot,
        scopes: &PathScopes,
        requested: &str,
        writable: bool,
    ) -> Result<Self> {
        root.revalidate()?;
        let physical = resolve_physical(root.cwd(), requested)?;
        authorize(scopes, root.cwd(), &physical.path, writable)?;
        if physical.path.file_name().is_none() {
            return Err(MezError::forbidden(
                "native file capability cannot target a filesystem root",
            ));
        }
        let preimage = if physical.missing.is_empty() {
            if FileType::from_raw_mode(physical.stat.st_mode) != FileType::RegularFile {
                return Err(MezError::forbidden(
                    "native file capability requires a regular file",
                ));
            }
            Some(read_exact_preimage(&physical)?)
        } else {
            None
        };
        revalidate_ancestry(&physical)?;
        Ok(Self {
            requested: requested.to_string(),
            physical,
            preimage,
            scopes: scopes.clone(),
            writable,
        })
    }

    /// Returns captured exact bytes, separately from normalized matching text.
    pub(crate) fn preimage(&self) -> Option<&[u8]> {
        self.preimage.as_deref()
    }

    /// Returns the positively resolved target spelling used in effect evidence.
    pub(crate) fn path(&self) -> &Path {
        &self.physical.path
    }

    /// Verifies a fresh actor lease preserves or narrows captured authority and
    /// the original spelling still denotes this exact physical object/preimage.
    pub(crate) fn revalidate(
        &self,
        root: &NativeFilesystemRoot,
        current_scopes: &PathScopes,
    ) -> Result<()> {
        root.revalidate()?;
        authorize(&self.scopes, root.cwd(), self.path(), self.writable)?;
        authorize(current_scopes, root.cwd(), self.path(), self.writable)?;
        revalidate_ancestry(&self.physical)?;
        let current = resolve_physical(root.cwd(), &self.requested)?;
        if current.path != self.physical.path || current.missing != self.physical.missing {
            return Err(MezError::conflict(
                "native filesystem target resolution changed",
            ));
        }
        match &self.preimage {
            Some(bytes)
                if same_object(&current.stat, &self.physical.stat)
                    && current.stat.st_mode == self.physical.stat.st_mode
                    && current.stat.st_uid == self.physical.stat.st_uid
                    && current.stat.st_gid == self.physical.stat.st_gid
                    && read_exact_preimage(&current)? == *bytes =>
            {
                Ok(())
            }
            None if !current.missing.is_empty()
                && same_object(&current.stat, &self.physical.stat) =>
            {
                Ok(())
            }
            _ => Err(MezError::conflict(
                "native filesystem preimage or object changed",
            )),
        }
    }

    /// Creates exactly one missing directory under fresh explicit authority.
    /// The caller must record this effect and recapture file capabilities after
    /// creation; no recursive ancestors or cleanup of unowned entries is implied.
    pub(crate) fn create_parent_directory(
        root: &NativeFilesystemRoot,
        current_scopes: &PathScopes,
        requested: &str,
    ) -> Result<std::path::PathBuf> {
        root.revalidate()?;
        let physical = resolve_physical(root.cwd(), requested)?;
        authorize(current_scopes, root.cwd(), &physical.path, true)?;
        if physical.missing.len() != 1 {
            return Err(MezError::conflict(
                "native parent creation requires exactly one missing directory",
            ));
        }
        revalidate_ancestry(&physical)?;
        let parent = &physical
            .directories
            .last()
            .ok_or_else(|| MezError::invalid_state("native parent creation lost parent"))?
            .file;
        rustix::fs::mkdirat(
            parent,
            &physical.missing[0],
            Mode::RUSR | Mode::WUSR | Mode::XUSR,
        )
        .map_err(std::io::Error::from)?;
        Ok(physical.path)
    }

    /// Stages bounded bytes in an exclusive same-directory regular file. Missing
    /// parent creation is a separate actor-authorized operation; never create
    /// broader ancestors implicitly from a file write grant.
    pub(crate) fn stage(
        &self,
        root: &NativeFilesystemRoot,
        current_scopes: &PathScopes,
        bytes: &[u8],
    ) -> Result<NativeStagedFile> {
        self.revalidate(root, current_scopes)?;
        if !self.writable || bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(MezError::forbidden(
                "native staging lacks authority or exceeds byte budget",
            ));
        }
        if self.physical.missing.len() > 1 {
            return Err(MezError::invalid_state(
                "native staging requires existing authorized parent directories",
            ));
        }
        let parent = self
            .physical
            .directories
            .last()
            .ok_or_else(|| MezError::invalid_state("native capability lost parent"))?
            .file
            .try_clone()?;
        let name = OsString::from(format!(".mez-native-stage-{}", rand::random::<u128>()));
        let descriptor = openat(
            &parent,
            &name,
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(std::io::Error::from)?;
        let mut stage = NativeStagedFile {
            parent,
            name,
            file: File::from(descriptor),
            published: false,
            target: self.path().to_path_buf(),
            digest: Sha256::digest(bytes).into(),
            byte_length: bytes.len() as u64,
            safe_mode: if self.preimage.is_some() {
                Mode::from_bits_truncate(self.physical.stat.st_mode & 0o777)
            } else {
                Mode::RUSR | Mode::WUSR
            },
        };
        stage.file.write_all(bytes)?;
        rustix::fs::fchmod(&stage.file, stage.safe_mode).map_err(std::io::Error::from)?;
        stage.file.sync_all()?;
        Ok(stage)
    }

    /// Publishes staged bytes only after fresh actor authority and exact object
    /// revalidation. Adds never replace competing creates; updates are atomic
    /// per-file entry replacements, not whole-patch transactions or external CAS.
    pub(crate) fn publish(
        &self,
        root: &NativeFilesystemRoot,
        current_scopes: &PathScopes,
        stage: &mut NativeStagedFile,
    ) -> Result<()> {
        if !self.writable {
            return Err(MezError::forbidden(
                "native publication lacks write authority",
            ));
        }
        if stage.published || stage.target != self.path() {
            return Err(MezError::conflict(
                "native stage belongs to another target or is already published",
            ));
        }
        self.revalidate(root, current_scopes)?;
        let parent = &self
            .physical
            .directories
            .last()
            .ok_or_else(|| MezError::invalid_state("native capability lost parent"))?
            .file;
        if !same_object(
            &fstat(parent).map_err(std::io::Error::from)?,
            &fstat(&stage.parent).map_err(std::io::Error::from)?,
        ) {
            return Err(MezError::conflict(
                "native staging parent differs from target",
            ));
        }
        stage.verify_owned_entry()?;
        stage.verify_bytes()?;
        let target = self
            .path()
            .file_name()
            .ok_or_else(|| MezError::invalid_state("native target lacks entry name"))?;
        if self.preimage.is_none() {
            renameat_with(
                &stage.parent,
                &stage.name,
                parent,
                target,
                RenameFlags::NOREPLACE,
            )
            .map_err(std::io::Error::from)?;
        } else {
            renameat(&stage.parent, &stage.name, parent, target).map_err(std::io::Error::from)?;
        }
        stage.published = true;
        Ok(())
    }

    /// Removes a revalidated regular entry. The caller must record a committed
    /// destination before deleting a move source; errors retain earlier effects.
    pub(crate) fn delete(
        &self,
        root: &NativeFilesystemRoot,
        current_scopes: &PathScopes,
    ) -> Result<()> {
        if !self.writable || self.preimage.is_none() {
            return Err(MezError::forbidden(
                "native deletion requires an existing writable preimage",
            ));
        }
        self.revalidate(root, current_scopes)?;
        let parent = &self
            .physical
            .directories
            .last()
            .ok_or_else(|| MezError::invalid_state("native capability lost parent"))?
            .file;
        let name = self
            .path()
            .file_name()
            .ok_or_else(|| MezError::invalid_state("native target lacks entry name"))?;
        unlinkat(parent, name, AtFlags::empty()).map_err(std::io::Error::from)?;
        Ok(())
    }
}

/// Exclusive staged entry retained by descriptor through publication/cleanup.
pub(crate) struct NativeStagedFile {
    /// Captured same-directory publication parent.
    parent: File,
    /// Unpredictable transaction-owned entry name, not model-authored.
    name: OsString,
    /// Held regular object to verify before publication or cleanup.
    file: File,
    /// Positive rename evidence prevents cleanup of a published destination.
    published: bool,
    /// Exact destination bound at staging, not merely the same parent directory.
    target: std::path::PathBuf,
    /// Digest and length of the bytes supplied by the semantic plan.
    digest: [u8; 32],
    /// Exact staged byte count, bounded by the per-file budget.
    byte_length: u64,
    /// Intended ordinary permissions, with every privilege bit suppressed.
    safe_mode: Mode,
}

impl NativeStagedFile {
    /// Rejects a replaced staging entry rather than publishing/deleting it.
    fn verify_owned_entry(&self) -> Result<()> {
        let named = statat(&self.parent, &self.name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(std::io::Error::from)?;
        let held = fstat(&self.file).map_err(std::io::Error::from)?;
        if !same_object(&named, &held)
            || FileType::from_raw_mode(held.st_mode) != FileType::RegularFile
        {
            return Err(MezError::conflict(
                "native transaction-owned stage was replaced",
            ));
        }
        Ok(())
    }

    /// Checks bytes through the held descriptor before publication, so an
    /// in-place staging modification cannot publish an unplanned payload.
    #[allow(
        clippy::useless_conversion,
        reason = "Darwin Mode bits are u16 while MetadataExt mode is always u32"
    )]
    fn verify_bytes(&self) -> Result<()> {
        use std::os::unix::fs::FileExt;
        use std::os::unix::fs::MetadataExt;
        let metadata = self.file.metadata()?;
        // Check before rename, independently of inode and bytes. Cleanup still
        // identifies ownership by descriptor so a chmod cannot strand our stage.
        if metadata.mode() & 0o7777 != u32::from(self.safe_mode.bits())
            || metadata.uid() != normalized_credentials(std::process::id())?.user_id
            || metadata.nlink() != 1
        {
            return Err(MezError::conflict("native staged safe metadata changed"));
        }
        if metadata.len() != self.byte_length {
            return Err(MezError::conflict("native staged byte length changed"));
        }
        let mut bytes = vec![0; self.byte_length as usize];
        self.file.read_exact_at(&mut bytes, 0)?;
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        if digest != self.digest {
            return Err(MezError::conflict("native staged bytes changed"));
        }
        Ok(())
    }
}

impl Drop for NativeStagedFile {
    /// Best-effort cleanup only of an unpublished positively identified entry.
    /// External same-directory renames after the check remain a documented race.
    fn drop(&mut self) {
        if !self.published && self.verify_owned_entry().is_ok() {
            let _ = unlinkat(&self.parent, &self.name, AtFlags::empty());
        }
    }
}

/// Checks the existing resolved authority contract, including explicit read
/// access for snapshots and optional write access for publication.
fn authorize(scopes: &PathScopes, cwd: &Path, target: &Path, writable: bool) -> Result<()> {
    if scopes.resolution_status == PathResolutionStatus::Unresolved
        || Path::new(&scopes.current_directory) != cwd
    {
        return Err(MezError::forbidden(
            "native filesystem authority is unresolved or belongs to another cwd",
        ));
    }
    let contains = |paths: &[String]| {
        paths
            .iter()
            .any(|scope| target.starts_with(Path::new(scope)))
    };
    if !contains(&scopes.read_scopes) || (writable && !contains(&scopes.write_scopes)) {
        return Err(MezError::forbidden(
            "native filesystem target exceeds current read/write scopes",
        ));
    }
    Ok(())
}

/// Rechecks each held directory against its captured physical location. Grants
/// do not follow directory relocation into a newly unauthorized location.
fn revalidate_ancestry(physical: &PhysicalPath) -> Result<()> {
    for witness in &physical.directories {
        let current = resolve_physical(
            Path::new("/"),
            witness
                .path
                .to_str()
                .ok_or_else(|| MezError::invalid_args("native ancestor is not UTF-8"))?,
        )?;
        if !current.missing.is_empty()
            || !same_object(
                &current.stat,
                &fstat(&witness.file).map_err(std::io::Error::from)?,
            )
        {
            return Err(MezError::conflict(
                "native filesystem ancestor was relocated or replaced",
            ));
        }
    }
    Ok(())
}

/// Opens and reads only a positively identified regular file, bounded by bytes.
/// Nonblocking protects against a special-node swap between metadata and open.
fn read_exact_preimage(physical: &PhysicalPath) -> Result<Vec<u8>> {
    let parent = &physical
        .directories
        .last()
        .ok_or_else(|| MezError::invalid_state("native snapshot lost parent"))?
        .file;
    let name = physical
        .path
        .file_name()
        .ok_or_else(|| MezError::forbidden("native snapshot cannot read filesystem root"))?;
    let descriptor = openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(std::io::Error::from)?;
    let mut file = File::from(descriptor);
    let stat = fstat(&file).map_err(std::io::Error::from)?;
    if !same_object(&stat, &physical.stat)
        || FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
    {
        return Err(MezError::conflict(
            "native snapshot target object changed or is nonregular",
        ));
    }
    if stat.st_size < 0 || stat.st_size as u64 > MAX_FILE_BYTES {
        return Err(MezError::invalid_args(
            "native snapshot exceeds byte budget",
        ));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(MezError::invalid_args(
            "native snapshot exceeded byte budget during read",
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod evidence_tests {
    use super::*;

    /// Mutating only captured evidence models stale PID lifetime, credentials
    /// and cwd observations without changing daemon credentials or global cwd.
    #[test]
    fn root_rejects_stale_lifetime_credentials_and_cwd() {
        let mut root = NativeFilesystemRoot::capture(std::process::id()).unwrap();
        root.start_token ^= 1;
        assert!(root.revalidate().is_err());
        let mut root = NativeFilesystemRoot::capture(std::process::id()).unwrap();
        root.credentials.user_id ^= 1;
        assert!(root.revalidate().is_err());
        let mut root = NativeFilesystemRoot::capture(std::process::id()).unwrap();
        root.credentials.supplementary_group_ids.push(u32::MAX);
        assert!(root.revalidate().is_err());
        let mut root = NativeFilesystemRoot::capture(std::process::id()).unwrap();
        root.cwd.path.push("not-current-cwd");
        assert!(root.revalidate().is_err());
    }

    /// Missing independent target-group evidence cannot be replaced with the
    /// daemon's groups even when the target's UID and primary GID match.
    #[test]
    fn unavailable_target_group_evidence_fails_closed() {
        assert!(require_exact_group_evidence(false).is_err());
        assert!(require_exact_group_evidence(true).is_ok());
    }

    /// Darwin's shell-oriented reader substitutes daemon groups for another
    /// PID, so the filesystem boundary rejects that reader before admission.
    #[cfg(target_os = "macos")]
    #[test]
    fn darwin_other_pid_credentials_are_not_independently_verified() {
        assert!(normalized_credentials(std::process::id().saturating_add(1)).is_err());
    }
}
