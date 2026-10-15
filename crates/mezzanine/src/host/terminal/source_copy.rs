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
    decode_copy_source_line,
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
    /// Explicit receiver anchor/reference rows whose physical continuations
    /// repeat their opaque carrier, not skip markers. Raw groups never opt in.
    DeclaredReferences,
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
        CopySourcePlan::Declared | CopySourcePlan::DeclaredReferences => raw_blocks,
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
    let mut encoded = BTreeMap::<String, Arc<TerminalCopySource>>::new();
    let mut ordinal_identities = BTreeMap::<u64, String>::new();
    // Trusted producer carriers may anchor at the tail to survive head pruning.
    // Validate the entire anchor set before resolving references or feeding any
    // candidate; raw whole-entry/block declarations never enter this decoder.
    for (line, copy) in lines.iter().zip(copies) {
        if matches!(
            line.kind,
            RichTextLineKind::MarkdownCodeBlock | RichTextLineKind::MarkdownDiagram
        ) || copy == COPY_SKIP_LINE
            || copy == COPY_WRAP_CONTINUATION
        {
            continue;
        }
        if !copy.starts_with(COPY_SOURCE_LINE_PREFIX) {
            continue;
        }
        let Some((identity, raw)) = decode_copy_source_line(copy) else {
            return Err(MezError::invalid_state(
                "generated copy-source carrier is malformed",
            ));
        };
        let ordinal = source_line_ordinal(identity)?;
        if let Some(previous) = ordinal_identities.insert(ordinal, identity.to_string())
            && previous != identity
        {
            return Err(MezError::invalid_state(
                "one semantic source ordinal has conflicting carrier identities",
            ));
        }
        let id = TerminalCopySourceId::new(entry, TerminalCopySourceGroup::SourceLine(ordinal))
            .ok_or_else(|| MezError::invalid_state("copy-source owner is invalid"))?;
        let source = encoded
            .entry(identity.to_string())
            .or_insert_with(|| TerminalCopySource::new(id, Arc::<str>::from(raw)));
        if source.text.as_ref() != raw {
            return Err(MezError::invalid_state(
                "one semantic source line has conflicting payloads",
            ));
        }
    }
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
        if copy.starts_with(COPY_SOURCE_LINE_PREFIX) {
            let Some((identity, raw)) = decode_copy_source_line(copy) else {
                return Err(MezError::invalid_state(
                    "generated copy-source carrier is malformed",
                ));
            };
            let source = encoded.get(identity).ok_or_else(|| {
                MezError::invalid_state("generated copy-source anchor has no declaration")
            })?;
            if source.text.as_ref() != raw {
                return Err(MezError::invalid_state(
                    "one semantic source line has conflicting payloads",
                ));
            }
            current = Some(source.clone());
            sources.push(current.clone());
        } else if let Some(identity) = copy.strip_prefix(COPY_SOURCE_REFERENCE_PREFIX) {
            current = encoded.get(identity).cloned();
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds an explicitly declared normal renderer row for plan-only tests.
    fn normal_rows() -> [RichTextLine; 2] {
        ["first", "last"].map(|display| RichTextLine {
            display: display.into(),
            style_spans: Vec::new(),
            copy_text: None,
            kind: RichTextLineKind::Normal,
        })
    }

    /// Steering intentionally retains one full payload at the tail so pruning
    /// cannot strand preceding references. The complete trusted plan resolves
    /// that forward reference without changing original CRLF/trailing bytes.
    #[test]
    fn declared_source_plan_resolves_tail_anchor_and_preserves_full_identity() {
        let entry = PresentationEntry {
            id: "receipt-owner".into(),
            occurrence: Default::default(),
            first_row: true,
        };
        let lines = normal_rows();
        let raw = "雪 exact\r\nsecond\n\n";
        let copies = [
            format!("{COPY_SOURCE_REFERENCE_PREFIX}receipt-owner/0"),
            format!("{COPY_SOURCE_LINE_PREFIX}receipt-owner/0:{raw}"),
        ];
        let plan =
            source_associations(&entry, &lines, &copies, CopySourcePlan::Declared, &[]).unwrap();
        assert_eq!(plan[0].as_ref().unwrap().text.as_ref(), raw);
        assert!(Arc::ptr_eq(
            plan[0].as_ref().unwrap(),
            plan[1].as_ref().unwrap()
        ));
    }

    /// Runtime peer receipt identities contain the turn/action colon separator.
    /// Generated carriers must retain the true ordinal and exact payload instead
    /// of treating part of that identity as raw text during live or replay plans.
    #[test]
    fn declared_source_plan_accepts_colon_bearing_peer_receipt_identity() {
        let entry = PresentationEntry {
            id: "peer-owner".into(),
            occurrence: Default::default(),
            first_row: true,
        };
        let group = "peer-message recipient=agent-%1 sequence=1 id=turn:action/雪%3A";
        let raw = "authored: text /0: exact\r\nsecond\n\n";
        let copy = mez_mux::copy::encode_copy_source_line_in_group(group, 0, raw);
        let plan = source_associations(
            &entry,
            &normal_rows(),
            &[copy.clone(), copy],
            CopySourcePlan::Declared,
            &[],
        )
        .unwrap();
        let source = plan[0].as_ref().unwrap();
        assert_eq!(source.text.as_ref(), raw);
        assert_eq!(
            source.id,
            TerminalCopySourceId::new(&entry, TerminalCopySourceGroup::SourceLine(0)).unwrap()
        );
        assert!(Arc::ptr_eq(source, plan[1].as_ref().unwrap()));
    }

    /// Colon-bearing steering occurrences use the same escaped identity for
    /// forward references and the tail anchor, preserving exact source bytes.
    #[test]
    fn declared_source_plan_resolves_escaped_tail_anchor() {
        let entry = PresentationEntry {
            id: "receipt:owner%3A".into(),
            occurrence: Default::default(),
            first_row: true,
        };
        let raw = "source: /0: 雪\r\n\n";
        let copies = [
            mez_mux::copy::encode_copy_source_reference_in_group(&entry.id, 0),
            mez_mux::copy::encode_copy_source_line_in_group(&entry.id, 0, raw),
        ];
        let plan = source_associations(
            &entry,
            &normal_rows(),
            &copies,
            CopySourcePlan::Declared,
            &[],
        )
        .unwrap();
        assert_eq!(plan[0].as_ref().unwrap().text.as_ref(), raw);
        assert!(Arc::ptr_eq(
            plan[0].as_ref().unwrap(),
            plan[1].as_ref().unwrap()
        ));
        let conflicting = [
            copies[1].clone(),
            mez_mux::copy::encode_copy_source_line_in_group("receipt%3Aowner%3A", 0, raw),
        ];
        assert!(
            source_associations(
                &entry,
                &normal_rows(),
                &conflicting,
                CopySourcePlan::Declared,
                &[],
            )
            .is_err()
        );
    }

    /// Whole-entry and block declarations are raw producer inputs even when
    /// their bytes equal a generated control or malformed carrier. They never
    /// enter the carrier parser; block owners and exact payloads remain checked.
    #[test]
    fn raw_source_plans_keep_literal_controls_independent_of_declared_carriers() {
        let entry = PresentationEntry {
            id: "raw-owner".into(),
            occurrence: Default::default(),
            first_row: true,
        };
        for raw in [
            COPY_SKIP_LINE,
            COPY_WRAP_CONTINUATION,
            COPY_SOURCE_LINE_PREFIX,
            COPY_SOURCE_REFERENCE_PREFIX,
        ] {
            let plan = source_associations(
                &entry,
                &normal_rows(),
                &[],
                CopySourcePlan::WholeEntry(raw),
                &[],
            )
            .unwrap();
            assert!(
                plan.iter()
                    .all(|source| source.as_ref().unwrap().text.as_ref() == raw)
            );
            assert_eq!(
                plan[0].as_ref().unwrap().id.group(),
                TerminalCopySourceGroup::WholeEntry
            );
        }
        let raw = format!("{COPY_SOURCE_LINE_PREFIX}literal:not-an-ordinal\r\n\n");
        let block = TerminalCopySource::new(
            TerminalCopySourceId::new(&entry, TerminalCopySourceGroup::RawBlock(0)).unwrap(),
            Arc::<str>::from(raw.as_str()),
        );
        let lines = [RichTextLine {
            display: "code".into(),
            style_spans: Vec::new(),
            copy_text: Some(raw.clone()),
            kind: RichTextLineKind::MarkdownCodeBlock,
        }];
        let blocks = [block.clone()];
        let plan = source_associations(
            &entry,
            &lines,
            std::slice::from_ref(&raw),
            CopySourcePlan::Blocks(&blocks),
            &[],
        )
        .unwrap();
        assert!(Arc::ptr_eq(plan[0].as_ref().unwrap(), &block));
        let mut foreign = entry.clone();
        foreign.id = "foreign-owner".into();
        assert!(
            source_associations(
                &foreign,
                &lines,
                &[raw],
                CopySourcePlan::Blocks(&blocks),
                &[]
            )
            .is_err()
        );
    }

    /// A same-ordinal foreign namespace cannot borrow a declared anchor, and
    /// missing/conflicting anchors reject before a candidate plan is returned.
    #[test]
    fn declared_source_plan_rejects_foreign_missing_and_conflicting_anchors() {
        let entry = PresentationEntry {
            id: "receipt-owner".into(),
            occurrence: Default::default(),
            first_row: true,
        };
        let lines = normal_rows();
        for copies in [
            [
                format!("{COPY_SOURCE_LINE_PREFIX}receipt-owner/0:exact"),
                format!("{COPY_SOURCE_REFERENCE_PREFIX}foreign/0"),
            ],
            [
                format!("{COPY_SOURCE_REFERENCE_PREFIX}receipt-owner/0"),
                COPY_SKIP_LINE.into(),
            ],
            [
                format!("{COPY_SOURCE_LINE_PREFIX}receipt-owner/0:first"),
                format!("{COPY_SOURCE_LINE_PREFIX}receipt-owner/0:conflict"),
            ],
        ] {
            for plan in [CopySourcePlan::Declared, CopySourcePlan::DeclaredReferences] {
                assert!(source_associations(&entry, &lines, &copies, plan, &[]).is_err());
            }
        }
    }
}
