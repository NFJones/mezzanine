//! Original physical allocation published before fresh custody can accept mail.
//!
//! This fixed, private, checksummed registration is independent of optional
//! message snapshots. Only a fenced, empty original can publish it. Recovery
//! requires the existing registration and the same directory/database allocation;
//! it never initializes a replacement or upgrades labels into native authority.
//! Native qualification still reads the actual private context and guarded
//! journal in the original source/ACK snapshot. At most two 362-byte files exist
//! during publication; no message bodies or reconnect credentials are stored.

use std::io::{Read, Write};

use mez_agent::messaging::{
    MessageService, MessageServiceSnapshot, MessageStorageBinding, MessageStorageSnapshot,
};
use rustix::fs::{AtFlags, Mode, OFlags, linkat, openat, unlinkat};

use super::{ownership::OwnedCustodyStore, *};

const NAME: &str = "registration.v1";
const STAGE: &str = "registration.v1.pending";
const MAGIC: &[u8; 8] = b"MMPROOT1";
const HEADER: usize = 74;
const MAX_BYTES: usize = HEADER + 256 + 32;

impl OwnedCustodyStore<'_> {
    /// Publishes a fresh original before service activation. Existing nonempty
    /// history, foreign/malformed registration or an unowned stage refuses.
    /// An identical previously published result can reconcile a lost reply.
    pub(crate) fn publish_registration(&mut self) -> Result<()> {
        self.revalidate()?;
        let store = self.store_mut();
        if !store.acceptance_plan(0)?.is_empty_original() {
            return Err(invalid("publication requires an empty original"));
        }
        require_no_stage(&store.directory)?;
        let bytes = encode(&store.original_registration_binding()?)?;
        match read_file(&store.directory, NAME) {
            Ok(existing) => {
                if existing != bytes {
                    return Err(invalid("existing publication differs"));
                }
                // Reconcile an exact prior publication, including a lost sync
                // result. Never regenerate the original from current labels.
                private_file(&store.directory, NAME, OFlags::RDWR)?.sync_all()?;
                store.directory.sync_all()?;
            }
            Err(error) if error.io_kind() == Some(std::io::ErrorKind::NotFound) => {
                let mut stage = std::fs::File::from(
                    openat(
                        &store.directory,
                        STAGE,
                        OFlags::WRONLY
                            | OFlags::CREATE
                            | OFlags::EXCL
                            | OFlags::NOFOLLOW
                            | OFlags::CLOEXEC,
                        Mode::RUSR | Mode::WUSR,
                    )
                    .map_err(std::io::Error::from)?,
                );
                let result = (|| -> Result<()> {
                    stage.write_all(&bytes)?;
                    stage.sync_all()?;
                    linkat(
                        &store.directory,
                        STAGE,
                        &store.directory,
                        NAME,
                        AtFlags::empty(),
                    )
                    .map_err(std::io::Error::from)?;
                    Ok(())
                })();
                // Retire only this actual stage inode. A colliding preexisting
                // stage was never opened and cannot be deleted by this attempt.
                let cleanup = remove_stage(&store.directory, &stage);
                result?;
                cleanup?;
                store.directory.sync_all()?;
                if read_file(&store.directory, NAME)? != bytes {
                    return Err(invalid("publication changed"));
                }
            }
            Err(error) => return Err(error),
        }
        self.revalidate()
    }

    /// Synthesizes a bodyless cold seed from the actual original registration.
    /// It contains no source/ACK claims: the existing complete history scanner
    /// must reconstruct every original and allocator gap in its SQL snapshot.
    pub(crate) fn registered_recovery_checkpoint(
        &mut self,
        retention_messages: usize,
        retention_bytes: usize,
    ) -> Result<MessageServiceSnapshot> {
        self.revalidate()?;
        let store = self.store_mut();
        require_no_stage(&store.directory)?;
        let binding = decode(&read_file(&store.directory, NAME)?)?;
        if binding != store.original_registration_binding()? {
            return Err(invalid("original allocation changed"));
        }
        let mut checkpoint = MessageService::with_limits(retention_messages, retention_bytes)
            .snapshot_state()
            .map_err(|_| invalid("empty recovery seed"))?;
        checkpoint.schema_version = 10;
        checkpoint.storage_snapshot = Some(MessageStorageSnapshot {
            format_version: 1,
            binding: Some(binding),
            sources: Vec::new(),
            native: Vec::new(),
        });
        checkpoint
            .validate_storage_snapshot()
            .map_err(|_| invalid("recovery seed framing"))?;
        self.revalidate()?;
        Ok(checkpoint)
    }
}

impl MmpCustodyStore {
    /// Describes the allocation's immutable origin, not its current generation.
    /// The original zero floors never hide current rows: complete recovery must
    /// scan the persistent watermark/history rather than use these as maxima.
    fn original_registration_binding(&self) -> Result<MessageStorageBinding> {
        let state = self.checked_state(&self.connection)?;
        if !state.watermark_origin_trusted {
            return Err(invalid("allocator origin unknown"));
        }
        let directory = self.directory.metadata()?;
        Ok(MessageStorageBinding {
            owner: self.owner.clone(),
            store_id: self
                .store_id
                .as_slice()
                .try_into()
                .map_err(|_| invalid("allocation framing"))?,
            generation: 0,
            sequence_watermark: 0,
            directory_device: directory.dev(),
            directory_inode: directory.ino(),
            database_device: self.database_identity.device,
            database_inode: self.database_identity.inode,
        })
    }
}

/// Encodes one bounded origin; its checksum covers framing and all identities.
fn encode(binding: &MessageStorageBinding) -> Result<Vec<u8>> {
    validate_identifier(&binding.owner)?;
    let mut bytes = Vec::with_capacity(MAX_BYTES);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&binding.store_id);
    for n in [
        binding.generation,
        binding.sequence_watermark,
        binding.directory_device,
        binding.directory_inode,
        binding.database_device,
        binding.database_inode,
    ] {
        bytes.extend_from_slice(&n.to_le_bytes());
    }
    bytes.extend_from_slice(&(binding.owner.len() as u16).to_le_bytes());
    bytes.extend_from_slice(binding.owner.as_bytes());
    let digest = Sha256::digest(&bytes);
    bytes.extend_from_slice(&digest);
    Ok(bytes)
}

/// Rejects unknown, truncated, excessive or cross-checksummed original framing.
fn decode(bytes: &[u8]) -> Result<MessageStorageBinding> {
    if bytes.len() < HEADER + 33 || bytes.len() > MAX_BYTES || &bytes[..8] != MAGIC {
        return Err(invalid("registration framing"));
    }
    let owner_len = u16::from_le_bytes(
        bytes[72..74]
            .try_into()
            .map_err(|_| invalid("owner width"))?,
    ) as usize;
    if owner_len > 256
        || bytes.len() != HEADER + owner_len + 32
        || Sha256::digest(&bytes[..HEADER + owner_len]).as_slice() != &bytes[HEADER + owner_len..]
    {
        return Err(invalid("registration checksum/length"));
    }
    let number = |offset| -> Result<u64> {
        Ok(u64::from_le_bytes(
            bytes[offset..offset + 8]
                .try_into()
                .map_err(|_| invalid("number width"))?,
        ))
    };
    let binding = MessageStorageBinding {
        owner: std::str::from_utf8(&bytes[HEADER..HEADER + owner_len])
            .map_err(|_| invalid("owner encoding"))?
            .into(),
        store_id: bytes[8..24]
            .try_into()
            .map_err(|_| invalid("allocation width"))?,
        generation: number(24)?,
        sequence_watermark: number(32)?,
        directory_device: number(40)?,
        directory_inode: number(48)?,
        database_device: number(56)?,
        database_inode: number(64)?,
    };
    validate_identifier(&binding.owner)?;
    if binding.generation != 0
        || binding.sequence_watermark != 0
        || binding.directory_inode == 0
        || binding.database_inode == 0
        || binding.store_id.iter().all(|b| *b == 0)
    {
        return Err(invalid("registration origin"));
    }
    Ok(binding)
}

/// Opens only a private owned single-link regular file beneath the held root.
fn private_file(directory: &std::fs::File, name: &str, access: OFlags) -> Result<std::fs::File> {
    let file = std::fs::File::from(
        openat(
            directory,
            name,
            access | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(std::io::Error::from)?,
    );
    let m = file.metadata()?;
    if !m.is_file()
        || m.nlink() != 1
        || m.uid() != rustix::process::geteuid().as_raw()
        || m.mode() & 0o077 != 0
        || m.len() > MAX_BYTES as u64
    {
        return Err(invalid(
            "registration file must be bounded private and single-link",
        ));
    }
    Ok(file)
}

/// Reads a constant-size original and detects observed inode/path changes.
fn read_file(directory: &std::fs::File, name: &str) -> Result<Vec<u8>> {
    let mut file = private_file(directory, name, OFlags::RDONLY)?;
    let before = file.metadata()?;
    let mut bytes = Vec::with_capacity(MAX_BYTES);
    (&mut file)
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    let current = private_file(directory, name, OFlags::RDONLY)?.metadata()?;
    let revision = |m: &fs::Metadata| {
        (
            m.dev(),
            m.ino(),
            m.len(),
            m.mtime(),
            m.mtime_nsec(),
            m.ctime(),
            m.ctime_nsec(),
        )
    };
    if bytes.len() > MAX_BYTES
        || revision(&before) != revision(&after)
        || revision(&after) != revision(&current)
    {
        return Err(invalid("registration changed during read"));
    }
    Ok(bytes)
}

/// An orphan or unowned stage cannot be ignored as though publication finished.
/// No file contents are opened or deleted while classifying this fixed slot.
fn require_no_stage(directory: &std::fs::File) -> Result<()> {
    match rustix::fs::statat(directory, STAGE, AtFlags::SYMLINK_NOFOLLOW) {
        Err(error) if error == rustix::io::Errno::NOENT => Ok(()),
        Ok(_) => Err(invalid("unowned publication stage remains")),
        Err(error) => Err(std::io::Error::from(error).into()),
    }
}

/// Removes only the actual stage this publisher created under its live fence.
fn remove_stage(directory: &std::fs::File, stage: &std::fs::File) -> Result<()> {
    let current = std::fs::File::from(
        openat(
            directory,
            STAGE,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(std::io::Error::from)?,
    );
    if FileIdentity::from_metadata(&current.metadata()?)
        != FileIdentity::from_metadata(&stage.metadata()?)
    {
        return Err(invalid("publication stage changed"));
    }
    unlinkat(directory, STAGE, AtFlags::empty()).map_err(std::io::Error::from)?;
    Ok(())
}

/// Preserves a content-free diagnostic at original registration boundaries.
fn invalid(detail: &str) -> MezError {
    MezError::invalid_state(format!("MMP original registration {detail}"))
}

#[cfg(test)]
mod tests;
