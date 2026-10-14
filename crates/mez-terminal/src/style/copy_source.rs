//! Immutable source-copy ownership attached to the existing terminal timeline.
//!
//! The product declares semantic source groups before physical wrapping. This
//! neutral owner validates identity and shares one exact payload among retained
//! rows; it never parses copy-marker strings, samples clocks or holds a second
//! event registry. Removing the last row releases the source naturally. Source
//! identity is occurrence plus semantic group, never text or pointer equality.

use super::PresentationEntry;
use std::sync::Arc;

/// Immutable source version within an already proven component owner. This is
/// not ordering/admission authority and never replaces actor freshness fences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalCopySourceGeneration {
    /// Accepted static or semantically reconstructed durable source.
    Sealed,
    /// Existing producer projection revision, not a new clock/counter.
    Revision(u64),
}

/// Semantic producer source group, independent of displayed row coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TerminalCopySourceGroup {
    /// One explicitly declared whole-entry source, such as a Markdown peer.
    WholeEntry,
    /// Original authored source-line ordinal from a typed copy carrier.
    SourceLine(u64),
    /// Original code/diagram block declaration ordinal, before wrapping.
    RawBlock(u64),
}

/// Validated entry-owned source identity with a bounded original owner ID.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TerminalCopySourceId {
    owner: String,
    group: TerminalCopySourceGroup,
}

impl TerminalCopySourceId {
    /// Creates identity only from coherent, bounded producer provenance. The
    /// group is semantic; no current time, display text or row number is read.
    pub fn new(entry: &PresentationEntry, group: TerminalCopySourceGroup) -> Option<Self> {
        entry.is_valid().then(|| Self {
            owner: entry.id.clone(),
            group,
        })
    }

    /// Returns the proven entry owner used to validate assignment targets.
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// Returns the original semantic group, never a physical row ordinal.
    pub fn group(&self) -> TerminalCopySourceGroup {
        self.group
    }
}

/// One immutable exact source payload shared by every associated retained row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalCopySource {
    /// Validated semantic identity; equal text entries remain distinct.
    pub id: TerminalCopySourceId,
    /// Existing immutable producer version. Conflicting versions with the same
    /// semantic ID in one copy snapshot fail closed, not duplicate emission.
    pub generation: TerminalCopySourceGeneration,
    /// Original copy bytes, including controls/newlines retained by its contract.
    pub text: Arc<str>,
}

impl TerminalCopySource {
    /// Creates a shared immutable generation. A changed payload with the same
    /// identity must be installed atomically with all corresponding rows.
    pub fn new(id: TerminalCopySourceId, text: impl Into<Arc<str>>) -> Arc<Self> {
        Arc::new(Self {
            id,
            generation: TerminalCopySourceGeneration::Sealed,
            text: text.into(),
        })
    }

    /// Captures one already existing source revision. Exact promoted rows may
    /// retain this inert version; promotion does not rewrite frozen snapshots.
    pub fn projected(
        id: TerminalCopySourceId,
        text: impl Into<Arc<str>>,
        revision: u64,
    ) -> Arc<Self> {
        Arc::new(Self {
            id,
            generation: TerminalCopySourceGeneration::Revision(revision),
            text: text.into(),
        })
    }
}
