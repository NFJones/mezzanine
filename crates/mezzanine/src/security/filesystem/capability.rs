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
//! Original regular-file descriptors remain held with their raw preimages;
//! numeric inode metadata alone is not a lifetime-long object witness.

use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use sha2::{Digest, Sha256};

use mez_agent::permissions::{PathResolutionStatus, PathScopes};
use mez_mux::process::{
    ProcessCredentials, current_working_directory_for_pid, filesystem_credentials_for_pid,
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

/// Separately selected operation directory, retained only for owned navigation.
struct NativeOperationDirectory {
    /// Canonical operation location and complete captured ancestor chain.
    physical: PhysicalPath,
    /// Held directory leaf; numeric metadata alone cannot preserve identity.
    file: File,
}

/// Local process lifetime and filesystem context captured without shell selection.
pub(crate) struct NativeFilesystemRoot {
    /// Kernel process identifier, always paired with start token.
    pid: u32,
    /// Exact process lifetime captured before reading credentials/cwd.
    start_token: u64,
    /// Daemon-compatible effective credentials; never change daemon credentials.
    credentials: ProcessCredentials,
    /// Canonical observed process cwd and its held physical ancestry.
    cwd: PhysicalPath,
    /// Kernel-referenced process cwd object, retained through the whole action.
    cwd_file: File,
    /// Separate owned agent directory; absent when process cwd is selected.
    operation: Option<NativeOperationDirectory>,
}

impl NativeFilesystemRoot {
    /// Captures a live local root with identical daemon credentials. Fails closed
    /// if any metadata is unavailable, replaced or incompatible; no PATH needed.
    pub(crate) fn capture(pid: u32) -> Result<Self> {
        Self::capture_context(pid, None, None)
    }

    /// Captures an actor-bound root lifetime with a separate operation cwd.
    /// The supplied directory is data, never a scope grant; all file access
    /// still requires actor authorization. A stale process token fails closed.
    pub(crate) fn capture_at(
        pid: u32,
        expected_start_token: u64,
        directory: &Path,
    ) -> Result<Self> {
        Self::capture_context(pid, Some(expected_start_token), Some(directory))
    }

    /// Captures process and operation directory identities without shell or
    /// environment inference, then checks their complete observed provenance.
    fn capture_context(
        pid: u32,
        expected_start: Option<u64>,
        directory: Option<&Path>,
    ) -> Result<Self> {
        let start_token = process_start_token_for_pid(pid).ok_or_else(|| {
            MezError::invalid_state("native filesystem root lifetime unavailable")
        })?;
        if expected_start.is_some_and(|expected| expected != start_token) {
            return Err(MezError::conflict(
                "native filesystem root lifetime changed before capture",
            ));
        }
        let credentials = normalized_credentials(pid)?;
        if credentials != normalized_credentials(std::process::id())? {
            return Err(MezError::forbidden(
                "native filesystem root credentials differ from daemon",
            ));
        }
        let process_path = current_working_directory_for_pid(pid)
            .ok_or_else(|| MezError::invalid_state("native filesystem root cwd unavailable"))?;
        let (cwd, process_path_file) = capture_directory(&process_path)?;
        let cwd_file = open_process_cwd(pid)?;
        let kernel = fstat(&cwd_file).map_err(std::io::Error::from)?;
        if !same_object(&cwd.stat, &kernel)
            || !same_object(
                &fstat(&process_path_file).map_err(std::io::Error::from)?,
                &kernel,
            )
        {
            return Err(MezError::conflict(
                "native process cwd path does not identify its kernel directory",
            ));
        }
        let operation = if let Some(directory) = directory.filter(|path| *path != cwd.path) {
            let (physical, file) = capture_directory(directory)?;
            if physical.path == cwd.path && same_object(&physical.stat, &kernel) {
                None
            } else {
                Some(NativeOperationDirectory { physical, file })
            }
        } else {
            None
        };
        let root = Self {
            pid,
            start_token,
            credentials,
            cwd,
            cwd_file,
            operation,
        };
        root.revalidate()?;
        Ok(root)
    }

    /// Re-observes lifetime, credentials, process cwd and operation cwd. Agent
    /// navigation never changes process cwd; replacing either directory or its
    /// ancestry revokes this root instead of refreshing its object witness.
    pub(crate) fn revalidate(&self) -> Result<()> {
        if process_start_token_for_pid(self.pid) != Some(self.start_token)
            || normalized_credentials(self.pid)? != self.credentials
            || normalized_credentials(std::process::id())? != self.credentials
        {
            return Err(MezError::conflict(
                "native filesystem process identity changed",
            ));
        }
        let process_path = current_working_directory_for_pid(self.pid)
            .ok_or_else(|| MezError::conflict("native filesystem cwd disappeared"))?;
        let current = resolve_physical(&process_path, ".")?;
        let process_file = open_process_cwd(self.pid)?;
        let held = fstat(&self.cwd_file).map_err(std::io::Error::from)?;
        if current.path != self.cwd.path
            || !same_object(&current.stat, &held)
            || !same_object(&self.cwd.stat, &held)
            || !same_object(&fstat(&process_file).map_err(std::io::Error::from)?, &held)
        {
            return Err(MezError::conflict(
                "native filesystem process cwd identity changed",
            ));
        }
        revalidate_ancestry(&self.cwd)?;
        if let Some(operation) = &self.operation {
            let current = resolve_physical(&operation.physical.path, ".")?;
            let held = fstat(&operation.file).map_err(std::io::Error::from)?;
            if current.path != operation.physical.path
                || !same_object(&current.stat, &held)
                || !same_object(&operation.physical.stat, &held)
            {
                return Err(MezError::conflict(
                    "native filesystem operation cwd identity changed",
                ));
            }
            revalidate_ancestry(&operation.physical)?;
        }
        if process_start_token_for_pid(self.pid) != Some(self.start_token) {
            return Err(MezError::conflict(
                "native filesystem root lifetime changed during validation",
            ));
        }
        Ok(())
    }

    /// Returns the separately observed process cwd, not the agent's cwd intent.
    pub(crate) fn process_cwd(&self) -> &Path {
        &self.cwd.path
    }

    /// Returns operation cwd without changing daemon or pane process directories.
    pub(crate) fn cwd(&self) -> &Path {
        self.operation
            .as_ref()
            .map_or(self.cwd.path.as_path(), |operation| {
                operation.physical.path.as_path()
            })
    }
}

/// Captures an absolute directory with its leaf and complete ancestry held.
/// Directory-only/no-follow opens cannot block on a substituted FIFO or follow
/// a replaced leaf link; identity comparison rejects swaps during capture.
fn capture_directory(path: &Path) -> Result<(PhysicalPath, File)> {
    if !path.is_absolute() {
        return Err(MezError::invalid_args(
            "native operation cwd must be absolute",
        ));
    }
    let physical = resolve_physical(path, ".")?;
    if !physical.missing.is_empty()
        || FileType::from_raw_mode(physical.stat.st_mode) != FileType::Directory
    {
        return Err(MezError::invalid_state(
            "native operation cwd is not an existing directory",
        ));
    }
    let file = File::from(
        openat(
            rustix::fs::CWD,
            &physical.path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(std::io::Error::from)?,
    );
    if !same_object(&physical.stat, &fstat(&file).map_err(std::io::Error::from)?) {
        return Err(MezError::conflict(
            "native directory changed while capturing its descriptor",
        ));
    }
    revalidate_ancestry(&physical)?;
    Ok((physical, file))
}

/// Opens the actual kernel cwd reference, not a second lookup of its pathname.
/// Linux's proc entry intentionally follows the kernel process reference;
/// Darwin only admits self here because foreign group evidence fails closed.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn open_process_cwd(pid: u32) -> Result<File> {
    #[cfg(target_os = "linux")]
    let path = format!("/proc/{pid}/cwd");
    #[cfg(target_os = "macos")]
    let path = {
        if pid != std::process::id() {
            return Err(MezError::forbidden(
                "native foreign process cwd witness unavailable",
            ));
        }
        ".".to_string()
    };
    openat(
        rustix::fs::CWD,
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|error| {
        MezError::invalid_state(format!("native kernel cwd witness unavailable: {error}"))
    })
}

/// Fails closed on platforms without a supported kernel cwd witness reader.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn open_process_cwd(_pid: u32) -> Result<File> {
    Err(MezError::forbidden(
        "native kernel cwd witness unsupported on this platform",
    ))
}

/// Normalizes supplementary groups for exact effective-credential comparison.
fn normalized_credentials(pid: u32) -> Result<ProcessCredentials> {
    // Darwin's existing shell-sandbox reader substitutes daemon supplementary
    // groups for another PID. That approximation cannot authorize daemon-side
    // filesystem access. Only our own process has independently known groups.
    require_exact_group_evidence(!cfg!(target_os = "macos") || pid == std::process::id())?;
    let mut credentials = filesystem_credentials_for_pid(pid)
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
    /// Held original regular object, preventing identity reuse after unlink.
    original_file: Option<File>,
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
        let (preimage, original_file) = if physical.missing.is_empty() {
            if FileType::from_raw_mode(physical.stat.st_mode) != FileType::RegularFile {
                return Err(MezError::forbidden(
                    "native file capability requires a regular file",
                ));
            }
            let (bytes, file) = read_exact_preimage(&physical)?;
            (Some(bytes), Some(file))
        } else {
            (None, None)
        };
        revalidate_ancestry(&physical)?;
        Ok(Self {
            requested: requested.to_string(),
            physical,
            preimage,
            original_file,
            scopes: scopes.clone(),
            writable,
        })
    }

    /// Returns captured exact bytes, separately from normalized matching text.
    pub(crate) fn preimage(&self) -> Option<&[u8]> {
        self.preimage.as_deref()
    }

    /// Recaptures a positively deleted entry for a later ordered create.
    /// The original ancestry and parent must still be held and the entry must
    /// remain missing; a replacement directory or competing file is rejected.
    /// Calling this method is not itself evidence that deletion occurred.
    /// The checkpoint permits cancellation before fresh bounded resolution.
    pub(crate) fn recapture_missing(
        &self,
        root: &NativeFilesystemRoot,
        current_scopes: &PathScopes,
        checkpoint: impl FnOnce() -> Result<()>,
    ) -> Result<Self> {
        root.revalidate()?;
        if !self.writable || self.preimage.is_none() {
            return Err(MezError::forbidden(
                "native deletion recapture lacks original write authority",
            ));
        }
        authorize(&self.scopes, root.cwd(), self.path(), true)?;
        revalidate_ancestry(&self.physical)?;
        checkpoint()?;
        let next = Self::capture(root, current_scopes, &self.requested, true)?;
        if next.preimage.is_some() || next.path() != self.path() || next.physical.missing.len() != 1
        {
            return Err(MezError::conflict(
                "native deleted entry changed before reuse",
            ));
        }
        self.validate_recaptured_ancestry(&next)?;
        Ok(next)
    }

    /// Recaptures this entry after its exact stage was positively published.
    /// Retains original authority and all ancestor identities while replacing
    /// only the expected leaf object. Equal-byte or relocated objects fail.
    /// Cancellation is checked before fresh resolution; afterwards every new
    /// ancestor descriptor must still identify its original predecessor object.
    pub(crate) fn recapture_published(
        &self,
        root: &NativeFilesystemRoot,
        current_scopes: &PathScopes,
        stage: &NativeStagedFile,
        checkpoint: impl FnOnce() -> Result<()>,
    ) -> Result<Self> {
        root.revalidate()?;
        if !self.writable {
            return Err(MezError::forbidden(
                "native publication recapture lacks original write authority",
            ));
        }
        authorize(&self.scopes, root.cwd(), self.path(), true)?;
        revalidate_ancestry(&self.physical)?;
        if stage.target != self.path() {
            return Err(MezError::conflict(
                "native published stage belongs to another predecessor",
            ));
        }
        checkpoint()?;
        let next = stage.capture_published_target(root, current_scopes, &self.requested)?;
        self.validate_recaptured_ancestry(&next)?;
        Ok(next)
    }

    /// Binds every returned ancestor witness to the original held object.
    /// A pre-capture namespace check alone cannot authorize refreshing these
    /// identities: an ancestor may change during resolution while the same
    /// immediate parent subtree survives. Equal held descriptors retain the
    /// original proof through later stage/commit revalidation.
    fn validate_recaptured_ancestry(&self, next: &Self) -> Result<()> {
        if self.physical.directories.len() != next.physical.directories.len() {
            return Err(MezError::conflict(
                "native recapture ancestor chain changed",
            ));
        }
        for (original, current) in self
            .physical
            .directories
            .iter()
            .zip(&next.physical.directories)
        {
            if original.path != current.path
                || !same_object(
                    &fstat(&original.file).map_err(std::io::Error::from)?,
                    &fstat(&current.file).map_err(std::io::Error::from)?,
                )
            {
                return Err(MezError::conflict(
                    "native recapture ancestor identity changed",
                ));
            }
        }
        Ok(())
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
        if self.preimage.is_some() != self.original_file.is_some() {
            return Err(MezError::invalid_state(
                "native preimage lost original leaf custody",
            ));
        }
        if let Some(original_file) = &self.original_file
            && !same_object(
                &fstat(original_file).map_err(std::io::Error::from)?,
                &self.physical.stat,
            )
        {
            return Err(MezError::conflict("native original leaf witness changed"));
        }
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
                    && read_exact_preimage(&current)?.0 == *bytes =>
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
    /// Captures the exact object this stage positively published for a later
    /// ordered operation. Equal bytes in a substituted inode are insufficient.
    /// Publication evidence stays owned by this stage through the recapture.
    fn capture_published_target(
        &self,
        root: &NativeFilesystemRoot,
        current_scopes: &PathScopes,
        requested: &str,
    ) -> Result<NativeFileCapability> {
        if !self.published {
            return Err(MezError::conflict("native stage has not been published"));
        }
        let capability = NativeFileCapability::capture(root, current_scopes, requested, true)?;
        if capability.path() != self.target
            || !same_object(
                &capability.physical.stat,
                &fstat(&self.file).map_err(std::io::Error::from)?,
            )
        {
            return Err(MezError::conflict(
                "native published object changed before reuse",
            ));
        }
        let parent = capability
            .physical
            .directories
            .last()
            .ok_or_else(|| MezError::invalid_state("native published entry lost parent"))?;
        if !same_object(
            &fstat(&parent.file).map_err(std::io::Error::from)?,
            &fstat(&self.parent).map_err(std::io::Error::from)?,
        ) {
            return Err(MezError::conflict("native published entry parent changed"));
        }
        self.verify_bytes()?;
        Ok(capability)
    }

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
pub(super) fn revalidate_ancestry(physical: &PhysicalPath) -> Result<()> {
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
/// Returns its held descriptor with the bytes so original identity stays pinned.
fn read_exact_preimage(physical: &PhysicalPath) -> Result<(Vec<u8>, File)> {
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
    Ok((bytes, file))
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

    /// Exercises the actual recapture boundary after old ancestry validation.
    /// Moving the same final-parent subtree retains its descriptor identity,
    /// while replacing only an upper ancestor must still revoke recapture.
    fn recapture_rejects_upper_ancestor_transfer(published: bool) {
        let directory =
            std::env::temp_dir().join(format!("mez-recapture-race-{}", rand::random::<u128>()));
        std::fs::create_dir_all(directory.join("upper/parent")).unwrap();
        let directory = std::fs::canonicalize(directory).unwrap();
        let root = NativeFilesystemRoot::capture(std::process::id()).unwrap();
        let grant = directory.to_str().unwrap().to_string();
        let scopes = super::super::host_resolved_path_scopes(
            root.cwd(),
            std::slice::from_ref(&grant),
            std::slice::from_ref(&grant),
            &[],
        )
        .unwrap();
        let path = directory.join("upper/parent/a");
        if !published {
            std::fs::write(&path, b"original").unwrap();
        }
        let capability =
            NativeFileCapability::capture(&root, &scopes, path.to_str().unwrap(), true).unwrap();
        let stage = if published {
            let mut stage = capability.stage(&root, &scopes, b"published").unwrap();
            capability.publish(&root, &scopes, &mut stage).unwrap();
            Some(stage)
        } else {
            capability.delete(&root, &scopes).unwrap();
            None
        };
        let transfer = || {
            std::fs::rename(directory.join("upper"), directory.join("original-upper")).unwrap();
            std::fs::create_dir(directory.join("upper")).unwrap();
            std::fs::rename(
                directory.join("original-upper/parent"),
                directory.join("upper/parent"),
            )
            .unwrap();
            Ok(())
        };
        let result = if let Some(stage) = &stage {
            capability.recapture_published(&root, &scopes, stage, transfer)
        } else {
            capability.recapture_missing(&root, &scopes, transfer)
        };
        let rejected = result.is_err();
        drop(result);
        drop(stage);
        drop(capability);
        std::fs::remove_dir_all(directory).unwrap();
        assert!(
            rejected,
            "recapture adopted a substituted upper ancestor while final parent stayed the same"
        );
    }

    /// A positively published inode and its held immediate parent do not permit
    /// adoption of replacement upper ancestry during fresh capture. The test
    /// injects the rename exactly between old validation and new resolution.
    #[test]
    fn published_recapture_rejects_upper_ancestor_transfer_at_capture_boundary() {
        recapture_rejects_upper_ancestor_transfer(true);
    }

    /// Positive deletion likewise cannot refresh away original upper ancestry
    /// while recapturing the missing entry for a later ordered create. The same
    /// final parent remains held throughout this deterministic boundary swap.
    #[test]
    fn missing_recapture_rejects_upper_ancestor_transfer_at_capture_boundary() {
        recapture_rejects_upper_ancestor_transfer(false);
    }

    /// Injecting numerically aliased cached metadata models the otherwise
    /// nondeterministic inode-reuse boundary without relying on allocator luck.
    /// A held original leaf must reject that substituted object even when its
    /// cached numbers and exact bytes match. This is evidence fault injection,
    /// not a claim that this fixture forces actual kernel inode reuse.
    #[test]
    fn original_leaf_descriptor_rejects_aliased_cached_replacement_metadata() {
        let directory =
            std::env::temp_dir().join(format!("mez-leaf-witness-{}", rand::random::<u128>()));
        std::fs::create_dir(&directory).unwrap();
        let directory = std::fs::canonicalize(directory).unwrap();
        let root = NativeFilesystemRoot::capture(std::process::id()).unwrap();
        let grant = directory.to_str().unwrap().to_string();
        let scopes = super::super::host_resolved_path_scopes(
            root.cwd(),
            std::slice::from_ref(&grant),
            std::slice::from_ref(&grant),
            &[],
        )
        .unwrap();
        let path = directory.join("a");
        std::fs::write(&path, b"same bytes").unwrap();
        let mut capability =
            NativeFileCapability::capture(&root, &scopes, path.to_str().unwrap(), true).unwrap();
        std::fs::rename(&path, directory.join("original")).unwrap();
        std::fs::write(&path, b"same bytes").unwrap();
        let replacement = resolve_physical(root.cwd(), path.to_str().unwrap()).unwrap();
        capability.physical.stat = replacement.stat;
        let rejected = capability.revalidate(&root, &scopes).is_err();
        drop(capability);
        drop(replacement);
        std::fs::remove_dir_all(directory).unwrap();
        assert!(
            rejected,
            "cached metadata replaced the original leaf ownership proof"
        );
    }

    /// An explicitly selected operation directory resolves relative file names
    /// independently of the unchanged process cwd. Both real directory leaves
    /// remain held and validated without modifying global process state.
    #[test]
    fn owned_operation_directory_preserves_independent_process_cwd() {
        let directory =
            std::env::temp_dir().join(format!("mez-owned-fs-root-{}", rand::random::<u128>()));
        std::fs::create_dir(&directory).unwrap();
        let directory = std::fs::canonicalize(directory).unwrap();
        std::fs::write(directory.join("note"), b"owned data").unwrap();
        let process_path = std::env::current_dir().unwrap();
        let token = process_start_token_for_pid(std::process::id()).unwrap();
        let root = NativeFilesystemRoot::capture_at(std::process::id(), token, &directory).unwrap();
        assert_eq!(root.cwd(), directory);
        assert_eq!(root.process_cwd(), process_path);
        let grant = directory.to_str().unwrap().to_string();
        let scopes = super::super::host_resolved_path_scopes(
            root.cwd(),
            std::slice::from_ref(&grant),
            std::slice::from_ref(&grant),
            &[],
        )
        .unwrap();
        let capability = NativeFileCapability::capture(&root, &scopes, "note", true).unwrap();
        assert_eq!(capability.preimage(), Some(b"owned data".as_slice()));
        root.revalidate().unwrap();
        assert_eq!(std::env::current_dir().unwrap(), process_path);
        drop(capability);
        drop(root);
        std::fs::remove_dir_all(directory).unwrap();
    }

    /// Bound root preparation rejects stale PID lifetimes and non-directory or
    /// relative targets. Replacing an already captured operation directory
    /// cannot change its inode witness while the process cwd stays unchanged.
    #[test]
    fn bound_operation_directory_rejects_stale_or_replaced_sources() {
        let directory =
            std::env::temp_dir().join(format!("mez-bound-fs-root-{}", rand::random::<u128>()));
        std::fs::create_dir_all(directory.join("owned")).unwrap();
        let directory = std::fs::canonicalize(directory).unwrap();
        let token = process_start_token_for_pid(std::process::id()).unwrap();
        let owned = directory.join("owned");
        assert!(NativeFilesystemRoot::capture_at(std::process::id(), token ^ 1, &owned).is_err());
        assert!(
            NativeFilesystemRoot::capture_at(std::process::id(), token, Path::new(".")).is_err()
        );
        std::fs::write(directory.join("file"), b"not a directory").unwrap();
        assert!(
            NativeFilesystemRoot::capture_at(std::process::id(), token, &directory.join("file"))
                .is_err()
        );
        let root = NativeFilesystemRoot::capture_at(std::process::id(), token, &owned).unwrap();
        std::fs::rename(&owned, directory.join("previous")).unwrap();
        std::fs::create_dir(&owned).unwrap();
        assert!(root.revalidate().is_err());
        drop(root);
        std::fs::remove_dir_all(directory).unwrap();
    }

    /// Darwin's shell-oriented reader substitutes daemon groups for another
    /// PID, so the filesystem boundary rejects that reader before admission.
    #[cfg(target_os = "macos")]
    #[test]
    fn darwin_other_pid_credentials_are_not_independently_verified() {
        assert!(normalized_credentials(std::process::id().saturating_add(1)).is_err());
    }
}
