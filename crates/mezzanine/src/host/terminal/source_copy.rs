//! Product-owned semantic source declarations for neutral terminal associations.
//!
//! Raw peer payloads and code/diagram declarations are explicit producer inputs,
//! never inferred from visible text, whitespace or timestamp-looking content.
//! Only generated encoded copy carriers are decoded here. The lower terminal
//! consumes validated IDs and immutable payloads without marker knowledge.
//! Declarations are captured before physical wrapping; an existing projection
//! revision versions streamed source, while accepted static source is sealed.

use crate::error::{MezError, Result};
use mez_core::presentation::PresentationEntry;
use mez_mux::copy::{
    COPY_SKIP_LINE, COPY_SOURCE_LINE_PREFIX, COPY_SOURCE_REFERENCE_PREFIX, COPY_WRAP_CONTINUATION,
};
use mez_mux::render::{RichTextLine, RichTextLineKind};
use mez_terminal::{TerminalCopySource, TerminalCopySourceGroup, TerminalCopySourceId};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Explicit producer source declaration for one completed logical projection.
pub(crate) enum CopySourcePlan<'a> {
    /// Generated encoded source carriers; specialized raw declarations require
    /// their own pre-wrap plan rather than classification of arbitrary strings.
    Declared,
    /// Specialized raw block owners declared before word/physical wrapping.
    Blocks(&'a [Arc<TerminalCopySource>]),
    /// A known whole-entry raw producer such as a Markdown peer message.
    WholeEntry(&'a str),
}

/// Captures code/diagram owners once per semantic declaration before wrapping.
/// Skip/continuation control values are known renderer metadata in this branch;
/// whole-entry raw data never passes through this marker interpretation.
pub(crate) fn raw_block_declarations(
    entry: &PresentationEntry,
    lines: &[RichTextLine],
) -> Vec<Arc<TerminalCopySource>> {
    lines
        .iter()
        .filter_map(|line| {
            if !matches!(
                line.kind,
                RichTextLineKind::MarkdownCodeBlock | RichTextLineKind::MarkdownDiagram
            ) {
                return None;
            }
            line.copy_text
                .as_deref()
                .filter(|text| *text != COPY_SKIP_LINE && *text != COPY_WRAP_CONTINUATION)
        })
        .enumerate()
        .filter_map(|(ordinal, raw)| {
            let ordinal = u64::try_from(ordinal).ok()?;
            let id = TerminalCopySourceId::new(entry, TerminalCopySourceGroup::RawBlock(ordinal))?;
            Some(TerminalCopySource::new(id, Arc::<str>::from(raw)))
        })
        .collect()
}

/// Builds an aligned immutable logical-row plan from explicit producer metadata.
/// Wrong owner/length or undeclared raw block fails before screen installation.
pub(crate) fn source_associations(
    entry: &PresentationEntry,
    lines: &[RichTextLine],
    copies: &[String],
    plan: CopySourcePlan<'_>,
    raw_blocks: &[Arc<TerminalCopySource>],
) -> Result<Vec<Option<Arc<TerminalCopySource>>>> {
    if !entry.is_valid() {
        return Ok(vec![None; lines.len()]);
    }
    let raw_blocks = match plan {
        CopySourcePlan::WholeEntry(raw) => {
            let Some(id) = TerminalCopySourceId::new(entry, TerminalCopySourceGroup::WholeEntry)
            else {
                return Ok(vec![None; lines.len()]);
            };
            let source = TerminalCopySource::new(id, Arc::<str>::from(raw));
            return Ok(vec![Some(source); lines.len()]);
        }
        CopySourcePlan::Blocks(blocks) => blocks,
        CopySourcePlan::Declared => raw_blocks,
    };
    if copies.is_empty() {
        return Ok(vec![None; lines.len()]);
    }
    if copies.len() != lines.len() {
        return Err(MezError::invalid_state(
            "source association logical-row count mismatch",
        ));
    }
    let mut declarations = raw_blocks.iter();
    let mut encoded = BTreeMap::<u64, Arc<TerminalCopySource>>::new();
    let mut current = None;
    let mut sources = Vec::with_capacity(lines.len());
    for (line, copy) in lines.iter().zip(copies) {
        if copy == COPY_SKIP_LINE || copy == COPY_WRAP_CONTINUATION {
            sources.push(current.clone());
            continue;
        }
        if matches!(
            line.kind,
            RichTextLineKind::MarkdownCodeBlock | RichTextLineKind::MarkdownDiagram
        ) {
            let Some(source) = declarations.next() else {
                return Err(MezError::invalid_state(
                    "raw source block has no semantic producer declaration",
                ));
            };
            if source.id.owner() != entry.id || source.text.as_ref() != copy {
                return Err(MezError::invalid_state(
                    "raw source declaration changed after projection",
                ));
            }
            current = Some(source.clone());
            sources.push(current.clone());
            continue;
        }
        if let Some(carrier) = copy.strip_prefix(COPY_SOURCE_LINE_PREFIX) {
            let Some((identity, raw)) = carrier.split_once(':') else {
                return Err(MezError::invalid_state(
                    "generated copy-source carrier is malformed",
                ));
            };
            let ordinal = source_line_ordinal(identity)?;
            let Some(id) =
                TerminalCopySourceId::new(entry, TerminalCopySourceGroup::SourceLine(ordinal))
            else {
                return Err(MezError::invalid_state("copy-source owner is invalid"));
            };
            let source = encoded
                .entry(ordinal)
                .or_insert_with(|| TerminalCopySource::new(id, Arc::<str>::from(raw)));
            if source.text.as_ref() != raw {
                return Err(MezError::invalid_state(
                    "one semantic source line has conflicting payloads",
                ));
            }
            current = Some(source.clone());
            sources.push(current.clone());
        } else if let Some(identity) = copy.strip_prefix(COPY_SOURCE_REFERENCE_PREFIX) {
            current = encoded.get(&source_line_ordinal(identity)?).cloned();
            if current.is_none() {
                return Err(MezError::invalid_state(
                    "generated copy-source reference has no owner",
                ));
            }
            sources.push(current.clone());
        } else {
            // Legacy/chrome text without an explicit producer source declaration
            // stays unassociated; it is not upgraded by text resemblance.
            current = None;
            sources.push(None);
        }
    }
    Ok(sources)
}

/// Reads the original source-line ordinal only from generated typed carriers.
fn source_line_ordinal(identity: &str) -> Result<u64> {
    identity
        .rsplit('/')
        .next()
        .unwrap_or(identity)
        .parse()
        .map_err(|_| MezError::invalid_state("generated source-line ordinal is invalid"))
}
