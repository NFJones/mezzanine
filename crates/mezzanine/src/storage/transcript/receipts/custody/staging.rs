//! Unpublished canonical custody images, qualified only by the original root.
//!
//! A complete private stage retains its exact row owner through process loss.
//! It can be linked into the final namespace only after source and physical
//! batch qualification under the conversation lease. An incomplete JSON prefix
//! owns no canonical row: only lookup-only original-root recovery with neither
//! preparation nor canonical journal may abort it, leaving its source untouched.
//! Ordinary producers fence both native and handoff stages without decoding them.

use super::*;

/// An actual bounded private staging object, never a decoded input capability.
pub(in crate::storage::transcript) struct CustodyStage {
    file: File,
    root: File,
    directory: File,
    path: PathBuf,
    bytes: Vec<u8>,
    data: Option<AppendReceipt>,
}

impl CustodyStage {
    /// A complete immutable plan remains data until original-root qualification.
    pub(in crate::storage::transcript) fn plan(&self) -> Option<&CustodyAppendPlan> {
        self.data.as_ref().and_then(AppendReceipt::custody_plan)
    }

    /// Reconstructs only fully validated complete canonical rows.
    pub(in crate::storage::transcript) fn rows(&self) -> Result<Vec<TranscriptEntry>> {
        self.data.as_ref().ok_or_else(invalid)?.entries()
    }

    /// Rechecks currently named original objects before either finalization or
    /// abort; an unrelated alias, mutation or replacement cannot be consumed.
    fn revalidate(&self) -> Result<()> {
        let parent = self.path.parent().ok_or_else(invalid)?;
        if !same(
            &directory(parent.parent().ok_or_else(invalid)?)?,
            &self.root,
        )? || !same(&directory(parent)?, &self.directory)?
            || !same(&private_file(&self.path)?, &self.file)?
            || self.file.metadata()?.nlink() != 1
            || self.file.metadata()?.len() != self.bytes.len() as u64
            || file_digest(&self.file)? != <[u8; 32]>::from(Sha256::digest(&self.bytes))
        {
            return Err(invalid());
        }
        Ok(())
    }

    /// Aborts only an incomplete unpublished candidate. The caller has proved
    /// accepted-source identity and journal absence in the original MMP root;
    /// this operation grants no commit, ACK, task or provider continuation.
    pub(in crate::storage::transcript) fn abort_incomplete(self) -> Result<()> {
        if self.data.is_some() {
            return Err(invalid());
        }
        self.revalidate()?;
        let final_path = self.path.with_extension("");
        match fs::symlink_metadata(final_path) {
            Ok(_) => return Err(invalid()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        self.file.sync_all()?;
        fs::remove_file(&self.path)?;
        self.directory.sync_all()?;
        self.root.sync_all()?;
        Ok(())
    }

    /// Promotes the same actual complete file after physical/source checks.
    /// Atomic linking never clobbers another owner. A lost link/unlink reply is
    /// reconciled by the ordinary exact final-file qualification on retry.
    pub(in crate::storage::transcript) fn publish(
        self,
        store: &AgentTranscriptStore,
        lease: &File,
    ) -> Result<Arc<CustodyAppendReceipt>> {
        let data = self.data.as_ref().ok_or_else(invalid)?;
        self.revalidate()?;
        self.file.sync_all()?;
        let final_path = self.path.with_extension("");
        match fs::hard_link(&self.path, &final_path) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if !same(&private_file(&final_path)?, &self.file)? {
                    return Err(invalid());
                }
            }
            Err(error) => return Err(error.into()),
        }
        self.directory.sync_all()?;
        self.root.sync_all()?;
        let owner = store
            .custody_append_receipt_locked(
                &data.conversation_id,
                data.custody_plan().ok_or_else(invalid)?.source().kind(),
                &data
                    .custody_plan()
                    .ok_or_else(invalid)?
                    .source()
                    .occurrence(),
                lease,
            )?
            .ok_or_else(invalid)?;
        // Qualification after linking must still return the original objects,
        // even if a parent was replaced between the named-path operations.
        if !same(&owner.file, &self.file)?
            || !same(&owner.root, &self.root)?
            || !same(&owner.directory, &self.directory)?
            || owner.digest != <[u8; 32]>::from(Sha256::digest(&self.bytes))
        {
            return Err(invalid());
        }
        Ok(owner)
    }
}

impl AgentTranscriptStore {
    /// Reads only this original occurrence's staged file under its mutation
    /// lease. Invalid complete frames and malformed prefixes refuse, rather
    /// than being treated as an interrupted write or qualified absence.
    pub(in crate::storage::transcript) fn custody_stage_locked(
        &self,
        conversation: &str,
        kind: CustodyReceiptKind,
        occurrence: &[u8; 32],
        _lease: &File,
    ) -> Result<Option<CustodyStage>> {
        let path = self
            .append_receipt_directory()
            .join(filename(conversation, kind, occurrence))
            .with_extension("json.tmp");
        let file = match private_file(&path) {
            Ok(file) => file,
            Err(error) if error.io_kind() == Some(std::io::ErrorKind::NotFound) => return Ok(None),
            Err(error) => return Err(error),
        };
        let length = file.metadata()?.len();
        if length > MAX_RECEIPT_BYTES as u64 || file.metadata()?.nlink() != 1 {
            return Err(invalid());
        }
        let mut bytes = Vec::new();
        (&file)
            .take(MAX_RECEIPT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 != length {
            return Err(invalid());
        }
        let data = match decode(&bytes) {
            Ok(data) => {
                if data.conversation_id != conversation
                    || data.custody_plan().is_none_or(|p| {
                        p.source().kind() != kind || p.source().occurrence() != *occurrence
                    })
                {
                    return Err(invalid());
                }
                Some(data)
            }
            Err(_) if incomplete_json(&bytes) => None,
            Err(error) => return Err(error),
        };
        let stage = CustodyStage {
            file,
            root: directory(&self.root)?,
            directory: directory(path.parent().ok_or_else(invalid)?)?,
            path,
            bytes,
            data,
        };
        stage.revalidate()?;
        Ok(Some(stage))
    }

    /// Routes only native staging names from the caller's captured inventory
    /// under all relevant conversation locks. A selected special/aliased/
    /// oversized object is an error, never a free append slot. Ordinary receipt
    /// checks must reuse these paths so a second enumeration cannot hide loss.
    pub(in crate::storage::transcript) fn has_custody_staging_paths(
        &self,
        conversation: &str,
        except: Option<(CustodyReceiptKind, &[u8; 32])>,
        paths: &[PathBuf],
    ) -> Result<bool> {
        let own = except
            .map(|(kind, occurrence)| format!("{}.tmp", filename(conversation, kind, occurrence)));
        for path in paths {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                return Err(invalid());
            };
            if !name.ends_with(".json.tmp") || !is_custody_filename(&path.with_extension("")) {
                continue;
            }
            let final_path = path.with_extension("");
            if receipt_conversation_id(&final_path)? != Some(conversation) {
                continue;
            }
            let file = private_file(path)?;
            if file.metadata()?.len() > MAX_RECEIPT_BYTES as u64 {
                return Err(invalid());
            }
            if own.as_deref() != Some(name) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// Bounded JSON parsing distinguishes a normal unfinished prefix from invalid
/// complete data. A partial UTF-8 code point may occur only at the physical end.
fn incomplete_json(bytes: &[u8]) -> bool {
    use serde::de::DeserializeSeed;
    let bytes = match std::str::from_utf8(bytes) {
        Ok(_) => bytes,
        Err(error) if error.error_len().is_none() => &bytes[..error.valid_up_to()],
        Err(_) => return false,
    };
    let mut nodes = 8192;
    let mut text = MAX_CUSTODY_APPEND_BYTES + 256 * 1024;
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    matches!(Count { nodes: &mut nodes, text: &mut text }.deserialize(&mut decoder),
        Err(error) if error.is_eof())
}
