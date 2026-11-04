//! Original private canonical receipt ownership for native input recovery.
//!
//! Native receipts share ordinary transcript routing and mutation locks, but
//! generic recovery must retain them for the original MMP root. The receipt owns
//! canonical row images, never another envelope queue or a task/provider resume
//! token. Actual private file objects, complete bytes and original physical
//! plans are checked again before append or retirement.

use super::*;
use crate::storage::transcript::native_input::{MAX_NATIVE_APPEND_BYTES, MAX_NATIVE_APPEND_ROWS};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::sync::Arc;

/// JSON escaping can double the already encoded canonical TSV; metadata is small.
const MAX_RECEIPT_BYTES: usize = 64 * 1024 * 1024 + 256 * 1024;

/// Actual private canonical receipt; deserialized row metadata cannot construct
/// this owner without the retained original filesystem objects and checked read.
pub(crate) struct NativeInputAppendReceipt {
    data: AppendReceipt,
    file: File,
    root: File,
    directory: File,
    path: PathBuf,
    digest: [u8; 32],
    bytes: u64,
}

impl NativeInputAppendReceipt {
    /// Borrows descriptive original source/position data qualified by this read.
    pub(crate) fn plan(&self) -> &NativeInputAppendPlan {
        // Native construction below rejects every receipt without this field.
        self.data
            .native_input
            .as_ref()
            .expect("validated native canonical receipt")
    }

    /// Reconstructs only this immutable receipt's validated canonical rows.
    pub(crate) fn rows(&self) -> Result<Vec<TranscriptEntry>> {
        self.data.entries()
    }

    /// Checks the retained original file and its currently named objects. A
    /// replacement path or altered bytes cannot substitute for the first owner.
    pub(crate) fn revalidate(&self) -> Result<()> {
        let root_path = self
            .path
            .parent()
            .and_then(Path::parent)
            .ok_or_else(invalid)?;
        let root = directory(root_path)?;
        let parent = directory(self.path.parent().ok_or_else(invalid)?)?;
        let named = private_file(&self.path)?;
        if !same(&root, &self.root)?
            || !same(&parent, &self.directory)?
            || !same(&named, &self.file)?
            || named.metadata()?.len() != self.bytes
            || file_digest(&named)? != self.digest
        {
            return Err(invalid());
        }
        self.file.sync_all()?;
        self.directory.sync_all()?;
        self.root.sync_all()?;
        Ok(())
    }

    /// Retires only this actual file after the original root published c1. It
    /// does not open, repair or otherwise claim rights over a replacement archive.
    pub(crate) fn retire(&self) -> Result<()> {
        if let Err(error) = self.revalidate() {
            if error.io_kind() != Some(std::io::ErrorKind::NotFound)
                || self.file.metadata()?.nlink() != 0
            {
                return Err(error);
            }
            let root_path = self
                .path
                .parent()
                .and_then(Path::parent)
                .ok_or_else(invalid)?;
            if !same(&directory(root_path)?, &self.root)?
                || !same(
                    &directory(self.path.parent().ok_or_else(invalid)?)?,
                    &self.directory,
                )?
            {
                return Err(invalid());
            }
            self.directory.sync_all()?;
            self.root.sync_all()?;
            return Ok(());
        }
        fs::remove_file(&self.path)?;
        self.directory.sync_all()?;
        self.root.sync_all()?;
        Ok(())
    }
}

/// Canonical namespace is separate from ordinary numeric append generations.
pub(super) fn filename(conversation: &str, occurrence: &[u8; 32]) -> String {
    let digest = occurrence
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    format!("{conversation}-native-{digest}.json")
}

impl AgentTranscriptStore {
    /// Reads one exact native canonical owner without loading a source envelope
    /// or enumerating unrelated conversations. Missing is not commit proof.
    pub(crate) fn native_input_append_receipt(
        &self,
        conversation: &str,
        occurrence: &[u8; 32],
    ) -> Result<Option<Arc<NativeInputAppendReceipt>>> {
        validate_conversation_id(conversation)?;
        let root = match directory(&self.root) {
            Ok(root) => root,
            Err(error) if error.io_kind() == Some(std::io::ErrorKind::NotFound) => return Ok(None),
            Err(error) => return Err(error),
        };
        let parent_path = self.append_receipt_directory();
        let parent = match directory(&parent_path) {
            Ok(parent) => parent,
            Err(error) if error.io_kind() == Some(std::io::ErrorKind::NotFound) => return Ok(None),
            Err(error) => return Err(error),
        };
        let path = parent_path.join(filename(conversation, occurrence));
        let file = match private_file(&path) {
            Ok(file) => file,
            Err(error) if error.io_kind() == Some(std::io::ErrorKind::NotFound) => return Ok(None),
            Err(error) => return Err(error),
        };
        let length = file.metadata()?.len();
        let bytes = read_bytes(&file)?;
        let data = decode(&bytes)?;
        if data.version != NATIVE_RECEIPT_VERSION
            || data.conversation_id != conversation
            || data.filename()
                != path
                    .file_name()
                    .and_then(|p| p.to_str())
                    .ok_or_else(invalid)?
            || data
                .native_input
                .as_ref()
                .is_none_or(|p| p.source().occurrence() != *occurrence)
            || !data
                .native_input
                .as_ref()
                .ok_or_else(invalid)?
                .root_matches(&root)?
        {
            return Err(invalid());
        }
        // A crash between hard-link publication and staging retirement leaves
        // two names for the SAME actual object, not a second canonical receipt.
        if file.metadata()?.nlink() == 2 {
            let staged = private_file(&path.with_extension("json.tmp"))?;
            if !same(&file, &staged)? {
                return Err(invalid());
            }
            fs::remove_file(path.with_extension("json.tmp"))?;
            parent.sync_all()?;
        }
        if file.metadata()?.nlink() != 1 {
            return Err(invalid());
        }
        let owner = Arc::new(NativeInputAppendReceipt {
            data,
            file,
            root,
            directory: parent,
            path,
            digest: Sha256::digest(&bytes).into(),
            bytes: length,
        });
        owner.revalidate()?;
        Ok(Some(owner))
    }

    /// Publishes a complete private canonical row image under the already-held
    /// ordinary conversation lease. Matching retry bytes reconcile; an existing
    /// different payload or staging prefix refuses before publication.
    pub(in crate::storage::transcript) fn publish_native_input_append_receipt(
        &self,
        plan: NativeInputAppendPlan,
        rows: &[TranscriptEntry],
        _lease: &File,
    ) -> Result<Arc<NativeInputAppendReceipt>> {
        plan.validate_rows(rows)?;
        let batch = TranscriptAppendBatch::rows(rows.to_vec())?;
        let mut data = AppendReceipt::from_batch(&batch, plan.source().message_sequence)?;
        data.version = NATIVE_RECEIPT_VERSION;
        data.native_input = Some(plan);
        data.digest = digest_payload(&data.rows, None, data.native_input.as_ref())?;
        data.batch()?;
        let bytes = serde_json::to_vec(&data).map_err(|_| invalid())?;
        if bytes.len() > MAX_RECEIPT_BYTES {
            return Err(invalid());
        }
        bounded_json(&bytes)?;
        let _root = directory(&self.root)?;
        let parent_path = self.append_receipt_directory();
        match fs::DirBuilder::new().mode(0o700).create(&parent_path) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.into()),
        }
        let parent = directory(&parent_path)?;
        let native = data.native_input.as_ref().ok_or_else(invalid)?;
        if let Some(existing) =
            self.native_input_append_receipt(&data.conversation_id, &native.source().occurrence())?
        {
            if existing.digest != <[u8; 32]>::from(Sha256::digest(&bytes)) {
                return Err(invalid());
            }
            return Ok(existing);
        }
        let path = parent_path.join(data.filename());
        let temporary = path.with_extension("json.tmp");
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        let mut file = match options.open(&temporary) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&temporary)?,
            Err(error) => return Err(error.into()),
        };
        validate_private_file(&file)?;
        let present = usize::try_from(file.metadata()?.len()).map_err(|_| invalid())?;
        if present > bytes.len() {
            return Err(invalid());
        }
        let mut scratch = [0u8; 8192];
        let mut offset = 0;
        while offset < present {
            let count = scratch.len().min(present - offset);
            file.read_exact(&mut scratch[..count])?;
            if scratch[..count] != bytes[offset..offset + count] {
                return Err(invalid());
            }
            offset += count;
        }
        file.write_all(&bytes[present..])?;
        file.sync_all()?;
        fs::hard_link(&temporary, &path)?;
        fs::remove_file(&temporary)?;
        parent.sync_all()?;
        _root.sync_all()?;
        self.native_input_append_receipt(&data.conversation_id, &native.source().occurrence())?
            .ok_or_else(invalid)
    }
}

/// Recognizes only the explicit native namespace, not words in a conversation ID.
pub(super) fn is_native_filename(path: &Path) -> bool {
    path.file_name()
        .and_then(|p| p.to_str())
        .and_then(|p| p.strip_suffix(".json"))
        .and_then(|p| p.rsplit_once("-native-"))
        .is_some_and(|(_, hash)| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}

/// Generic receipt discovery validates native data with the same expansion
/// limits, while leaving original-root completion and retirement untouched.
pub(super) fn read_data(path: &Path) -> Result<AppendReceipt> {
    decode(&read_bytes(&private_file(path)?)?)
}

fn read_bytes(file: &File) -> Result<Vec<u8>> {
    let length = file.metadata()?.len();
    if length == 0 || length > MAX_RECEIPT_BYTES as u64 {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    file.take(MAX_RECEIPT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != length {
        return Err(invalid());
    }
    Ok(bytes)
}

fn decode(bytes: &[u8]) -> Result<AppendReceipt> {
    bounded_json(bytes)?;
    let data: AppendReceipt = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    if data.version != NATIVE_RECEIPT_VERSION || data.native_input.is_none() {
        return Err(invalid());
    }
    data.batch()?;
    Ok(data)
}

/// Two retained descriptors must identify the same actual kernel object.
fn same(left: &File, right: &File) -> Result<bool> {
    let left = left.metadata()?;
    let right = right.metadata()?;
    Ok(left.dev() == right.dev() && left.ino() == right.ino())
}

/// Private canonical objects use the current owner and no group/other rights.
fn directory(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(invalid());
    }
    Ok(file)
}

/// Opens a bounded private file without following a replacement symlink.
fn private_file(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    validate_private_file(&file)?;
    Ok(file)
}

fn validate_private_file(file: &File) -> Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
        || !(1..=2).contains(&metadata.nlink())
    {
        return Err(invalid());
    }
    Ok(())
}

/// Fixed scratch verifies the entire actual file without another row image.
fn file_digest(file: &File) -> Result<[u8; 32]> {
    let mut cursor = file;
    cursor.seek(SeekFrom::Start(0))?;
    // A concurrently enlarged file still cannot turn revalidation into an
    // unbounded read. Its final digest/length cannot match the retained owner.
    let mut file = cursor.take(MAX_RECEIPT_BYTES as u64 + 1);
    let mut digest = Sha256::new();
    let mut scratch = [0u8; 8192];
    loop {
        let count = file.read(&mut scratch)?;
        if count == 0 {
            break;
        }
        digest.update(&scratch[..count]);
    }
    Ok(digest.finalize().into())
}

/// Bounds container expansion before serde retains native canonical row data.
fn bounded_json(bytes: &[u8]) -> Result<()> {
    use serde::de::DeserializeSeed;
    let mut nodes = 8192usize;
    let mut text = MAX_NATIVE_APPEND_BYTES + 64 * 1024;
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    Count {
        nodes: &mut nodes,
        text: &mut text,
    }
    .deserialize(&mut decoder)
    .map_err(|_| invalid())?;
    decoder.end().map_err(|_| invalid())
}

struct Count<'a> {
    nodes: &'a mut usize,
    text: &'a mut usize,
}
impl<'de> serde::de::DeserializeSeed<'de> for Count<'_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        decoder: D,
    ) -> std::result::Result<(), D::Error> {
        *self.nodes = self
            .nodes
            .checked_sub(1)
            .ok_or_else(|| serde::de::Error::custom("native receipt nodes"))?;
        decoder.deserialize_any(self)
    }
}
impl<'de> serde::de::Visitor<'de> for Count<'_> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("bounded native canonical receipt")
    }
    fn visit_map<A: serde::de::MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<(), A::Error> {
        while map
            .next_key_seed(Count {
                nodes: self.nodes,
                text: self.text,
            })?
            .is_some()
        {
            map.next_value_seed(Count {
                nodes: self.nodes,
                text: self.text,
            })?;
        }
        Ok(())
    }
    fn visit_seq<A: serde::de::SeqAccess<'de>>(
        self,
        mut seq: A,
    ) -> std::result::Result<(), A::Error> {
        let mut count = 0;
        while seq
            .next_element_seed(Count {
                nodes: self.nodes,
                text: self.text,
            })?
            .is_some()
        {
            count += 1;
            if count > MAX_NATIVE_APPEND_ROWS {
                return Err(serde::de::Error::custom("native receipt sequence"));
            }
        }
        Ok(())
    }
    fn visit_str<E: serde::de::Error>(self, value: &str) -> std::result::Result<(), E> {
        *self.text = self
            .text
            .checked_sub(value.len())
            .ok_or_else(|| E::custom("native receipt text"))?;
        Ok(())
    }
    fn visit_string<E: serde::de::Error>(self, value: String) -> std::result::Result<(), E> {
        self.visit_str(&value)
    }
    fn visit_bool<E: serde::de::Error>(self, _: bool) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: serde::de::Error>(self, _: u64) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: serde::de::Error>(self, _: i64) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: serde::de::Error>(self, _: f64) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<(), E> {
        Ok(())
    }
}

fn invalid() -> MezError {
    MezError::invalid_state("original native canonical append receipt unavailable")
}
