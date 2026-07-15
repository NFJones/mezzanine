//! Multiplexer-owned paste-buffer state and validation.
//!
//! Host clipboard process execution remains in the Mezzanine composition crate.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{MuxError, Result};

/// Default maximum byte size accepted by one paste buffer.
pub const DEFAULT_PASTE_BUFFER_LIMIT_BYTES: usize = 1_048_576;

/// Carries Paste Buffer state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteBuffer {
    /// Stores the name value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub name: String,
    /// Stores the bytes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub bytes: usize,
    /// Stores the created at unix seconds value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub created_at_unix_seconds: u64,
    /// Stores the origin value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub origin: Option<String>,
    /// Stores the preview value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub preview: String,
}

/// Carries Paste Buffer Entry state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PasteBufferEntry {
    /// Stores the content value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) content: String,
    /// Stores the created at unix seconds value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) created_at_unix_seconds: u64,
    /// Stores the sequence value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) sequence: u64,
    /// Stores the origin value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) origin: Option<String>,
}

/// Carries Paste Buffers state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteBuffers {
    /// Stores the limit bytes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) limit_bytes: usize,
    /// Stores the next sequence value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) next_sequence: u64,
    /// Stores the buffers value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) buffers: BTreeMap<String, PasteBufferEntry>,
}

impl PasteBuffers {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(limit_bytes: usize) -> Result<Self> {
        if limit_bytes == 0 {
            return Err(MuxError::invalid_args(
                "paste buffer byte limit must be greater than zero",
            ));
        }
        Ok(Self {
            limit_bytes,
            next_sequence: 1,
            buffers: BTreeMap::new(),
        })
    }

    /// Runs the default limit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn default_limit() -> Self {
        Self::new(DEFAULT_PASTE_BUFFER_LIMIT_BYTES).expect("default paste buffer limit is non-zero")
    }

    /// Runs the set operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn set(&mut self, name: impl Into<String>, content: impl Into<String>) -> Result<()> {
        self.set_with_origin(name, content, None)
    }

    /// Runs the set with origin operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn set_with_origin(
        &mut self,
        name: impl Into<String>,
        content: impl Into<String>,
        origin: Option<String>,
    ) -> Result<()> {
        let name = name.into();
        validate_paste_buffer_name(&name)?;
        let content = content.into();
        if content.len() > self.limit_bytes {
            return Err(MuxError::invalid_args(
                "paste buffer content exceeds configured byte limit",
            ));
        }
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.buffers.insert(
            name,
            PasteBufferEntry {
                content,
                created_at_unix_seconds: current_unix_seconds(),
                sequence,
                origin,
            },
        );
        Ok(())
    }

    /// Creates a paste buffer without overwriting an existing buffer.
    ///
    /// The buffer name is validated with the same rules as `set_with_origin`.
    /// The return value is `true` when a new buffer was inserted and `false`
    /// when a buffer with that name already existed.
    pub fn create_with_origin(
        &mut self,
        name: impl Into<String>,
        content: impl Into<String>,
        origin: Option<String>,
    ) -> Result<bool> {
        let name = name.into();
        validate_paste_buffer_name(&name)?;
        if self.buffers.contains_key(&name) {
            return Ok(false);
        }
        let content = content.into();
        if content.len() > self.limit_bytes {
            return Err(MuxError::invalid_args(
                "paste buffer content exceeds configured byte limit",
            ));
        }
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.buffers.insert(
            name,
            PasteBufferEntry {
                content,
                created_at_unix_seconds: current_unix_seconds(),
                sequence,
                origin,
            },
        );
        Ok(true)
    }

    /// Runs the get operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.buffers.get(name).map(|entry| entry.content.as_str())
    }

    /// Runs the delete operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn delete(&mut self, name: &str) -> bool {
        self.buffers.remove(name).is_some()
    }

    /// Runs the most recent name operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn most_recent_name(&self) -> Option<&str> {
        self.buffers
            .iter()
            .max_by_key(|(_, entry)| entry.sequence)
            .map(|(name, _)| name.as_str())
    }

    /// Runs the delete most recent operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn delete_most_recent(&mut self) -> Option<String> {
        let name = self.most_recent_name()?.to_string();
        if self.delete(&name) { Some(name) } else { None }
    }

    /// Runs the list operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn list(&self) -> Vec<PasteBuffer> {
        self.buffers
            .iter()
            .map(|(name, entry)| PasteBuffer {
                name: name.clone(),
                bytes: entry.content.len(),
                created_at_unix_seconds: entry.created_at_unix_seconds,
                origin: entry.origin.clone(),
                preview: paste_buffer_preview(&entry.content),
            })
            .collect()
    }
}

/// Runs the current unix seconds operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn current_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// Runs the paste buffer preview operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn paste_buffer_preview(content: &str) -> String {
    let mut preview = content
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .take(40)
        .collect::<String>();
    if content.chars().count() > 40 {
        preview.push_str("...");
    }
    preview
}

/// Runs the validate paste buffer name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_paste_buffer_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(MuxError::invalid_args("paste buffer name is invalid"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::PasteBuffers;

    /// Verifies paste-buffer state preserves content, metadata, ordering, and bounded previews.
    #[test]
    fn paste_buffers_store_and_list_bounded_content() {
        let mut buffers = PasteBuffers::new(64).unwrap();
        buffers
            .set_with_origin(
                "selection",
                "line\nwith control",
                Some("copy-mode".to_string()),
            )
            .unwrap();

        assert_eq!(buffers.get("selection"), Some("line\nwith control"));
        let listed = buffers.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "selection");
        assert_eq!(listed[0].origin.as_deref(), Some("copy-mode"));
        assert_eq!(listed[0].preview, "line with control");
    }

    /// Verifies invalid names and over-limit content fail before mutating paste-buffer state.
    #[test]
    fn paste_buffers_reject_invalid_names_and_oversized_content() {
        let mut buffers = PasteBuffers::new(4).unwrap();

        assert!(buffers.set("bad/name", "ok").is_err());
        assert!(buffers.set("valid", "12345").is_err());
        assert!(buffers.list().is_empty());
    }
}
