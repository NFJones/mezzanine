//! Immutable original-source backing carried between epoch publications.
//!
//! This is per-publication audit escrow, not selectable model history or an
//! append journal. Covered source remains available to recognize its later
//! ordinary row without restoring it to the model. Original turn provenance,
//! occurrence, canonical framing and group ordinal must all agree. Only actual
//! original append images inside the committed archive cut qualify retirement;
//! current source and whole still-required groups retain their backing. This
//! does not acknowledge pending receipts or remove archive/payload files.

use super::{Payload, SOURCE_LIMIT, context_error, invalid};
use crate::Result;
use mez_agent::{
    ContextBlockOccurrenceId, ContextBlockRecord, ContextSourceKind, ReplayEpochContext,
    TranscriptEntry, TranscriptRole,
    transcript::{TranscriptContextEvent, TranscriptContextRecord},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Original controller turn that owns newly authored, not yet archived source.
/// This is descriptive provenance, never an execution or append acknowledgment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReplaySourceOrigin {
    turn_id: String,
    agent_id: String,
    pane_id: String,
}

impl ReplaySourceOrigin {
    /// Captures actual original turn fields. Empty/oversized/control-bearing
    /// identifiers refuse; a later pane lookup cannot replace this provenance.
    pub(crate) fn new(turn_id: &str, agent_id: &str, pane_id: &str) -> Result<Self> {
        let origin = Self {
            turn_id: turn_id.into(),
            agent_id: agent_id.into(),
            pane_id: pane_id.into(),
        };
        origin.validate()?;
        Ok(origin)
    }

    fn validate(&self) -> Result<()> {
        if [&self.turn_id, &self.agent_id, &self.pane_id]
            .into_iter()
            .any(|value| {
                value.is_empty()
                    || value.trim() != value
                    || value.len() > 4096
                    || value.chars().any(char::is_control)
            })
        {
            return Err(invalid("invalid replay source turn provenance"));
        }
        Ok(())
    }
}

/// One original occurrence, independent of summary anchors and row positions.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EscrowOccurrence {
    record: ContextBlockRecord,
    source: ContextSourceKind,
    label: String,
    content: Payload,
    sequence: u64,
    group: Option<String>,
    ordinal: Option<u64>,
    provider: Option<(String, String)>,
    origin: ReplaySourceOrigin,
    /// Version-seven explicit task boundary; absent in older source formats.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    task_start: Option<ContextBlockOccurrenceId>,
    /// Original ordinary image is covered by this manifest's durable cut.
    #[serde(default, skip_serializing_if = "is_false")]
    archived: bool,
}

impl std::fmt::Debug for EscrowOccurrence {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("EscrowOccurrence")
    }
}

/// Retains inherited originals and adds only actual current raw occurrences.
/// Inherited origin and ordinal cannot be reassigned by a newer caller. No
/// settlement claim is inferred here; publication qualifies retirement separately.
pub(super) fn extend(
    inherited: &[EscrowOccurrence],
    source: &ReplayEpochContext,
    origin: &ReplaySourceOrigin,
    captured_rows: &[TranscriptEntry],
) -> Result<Vec<EscrowOccurrence>> {
    extend_with_limit(inherited, source, origin, captured_rows, SOURCE_LIMIT)
}

fn extend_with_limit(
    inherited: &[EscrowOccurrence],
    source: &ReplayEpochContext,
    origin: &ReplaySourceOrigin,
    captured_rows: &[TranscriptEntry],
    limit: usize,
) -> Result<Vec<EscrowOccurrence>> {
    validate(inherited)?;
    origin.validate()?;
    let mut originals = inherited
        .iter()
        .map(|item| (item.record.occurrence_id(), item.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut captured = BTreeMap::new();
    for row in captured_rows {
        let authored = TranscriptContextRecord::from_transcript_content(&row.content)?
            .ok_or_else(|| invalid("captured replay arrival has no authored occurrence"))?;
        if captured
            .insert(authored.record().occurrence_id(), (row, authored))
            .is_some()
        {
            return Err(invalid("captured replay arrival occurrence is duplicated"));
        }
    }
    for event in source
        .context()
        .chronology()
        .iter()
        .skip(usize::from(source.head().is_some()))
    {
        let item = EscrowOccurrence {
            record: event.record(),
            source: event.block().source,
            label: event.block().label.clone(),
            content: Payload::new(event.block().content.as_bytes()),
            sequence: event.sequence().get(),
            group: event
                .execution_group_id()
                .map(|group| group.as_str().into()),
            ordinal: None,
            provider: event
                .provider_owner()
                .map(|owner| (owner.api().as_str().into(), owner.provider_id().into())),
            origin: captured.get(&event.record().occurrence_id()).map_or_else(
                || Ok(origin.clone()),
                |(row, _)| ReplaySourceOrigin::new(&row.turn_id, &row.agent_id, &row.pane_id),
            )?,
            task_start: source
                .task_admission_start(event.record().occurrence_id())
                .or_else(|| {
                    originals
                        .get(&event.record().occurrence_id())
                        .and_then(|item| item.task_start)
                }),
            archived: false,
        };
        match originals.get(&item.record.occurrence_id()) {
            Some(previous) => {
                let mut expected = item;
                expected.origin = previous.origin.clone();
                expected.ordinal = previous.ordinal;
                expected.archived = previous.archived;
                if &expected != previous {
                    return Err(invalid("replay escrow original occurrence changed"));
                }
            }
            None => {
                originals.insert(item.record.occurrence_id(), item);
            }
        }
    }
    let mut result = originals.into_values().collect::<Vec<_>>();
    result.sort_by_key(|item| item.sequence);
    let mut ordinals = BTreeMap::<String, u64>::new();
    for item in &mut result {
        if let Some(group) = &item.group {
            let ordinal = ordinals.entry(group.clone()).or_default();
            *ordinal = ordinal
                .checked_add(1)
                .ok_or_else(|| invalid("replay escrow ordinal exhausted"))?;
            if item.ordinal.is_some_and(|old| old != *ordinal) {
                return Err(invalid("replay escrow original ordinal changed"));
            }
            item.ordinal = Some(*ordinal);
        }
    }
    validate_with_limit(&result, limit)?;
    if !captured.is_empty() {
        let by_id = result
            .iter()
            .map(|item| (item.occurrence_id(), item))
            .collect::<BTreeMap<_, _>>();
        let source_by_id = source
            .context()
            .chronology()
            .iter()
            .map(|event| (event.record().occurrence_id(), event))
            .collect::<BTreeMap<_, _>>();
        for (id, (row, authored)) in captured {
            let item = by_id
                .get(&id)
                .ok_or_else(|| invalid("captured replay arrival is absent from source"))?;
            let event = source_by_id
                .get(&id)
                .ok_or_else(|| invalid("captured replay arrival is absent from current source"))?;
            if !item.matches(row, &authored, &event.block().content) {
                return Err(invalid("captured replay arrival changed original framing"));
            }
        }
    }
    Ok(result)
}

/// Validates bounded metadata before payload lookup or canonical row matching.
pub(super) fn validate(items: &[EscrowOccurrence]) -> Result<()> {
    validate_with_limit(items, SOURCE_LIMIT)
}

fn validate_with_limit(items: &[EscrowOccurrence], limit: usize) -> Result<()> {
    let mut ids = BTreeSet::new();
    let mut sequence = 0;
    let mut bytes = 0usize;
    let mut ordinals = BTreeMap::<&str, u64>::new();
    for item in items {
        item.origin.validate()?;
        item.record.validate_format().map_err(context_error)?;
        item.content.validate_format()?;
        bytes = bytes
            .checked_add(item.content.bytes)
            .filter(|bytes| *bytes <= limit)
            .ok_or_else(|| invalid("unsettled replay source exceeds backing limit"))?;
        if !ids.insert(item.record.occurrence_id())
            || item.sequence == 0
            || item.sequence < sequence
            || item.label.is_empty()
            || item.label.len() > SOURCE_LIMIT
            || item.group.is_some() != item.ordinal.is_some()
            || matches!(
                item.source,
                ContextSourceKind::CompactionPrefix | ContextSourceKind::CompactionSelective
            )
        {
            return Err(invalid("invalid replay escrow occurrence framing"));
        }
        if let Some(group) = &item.group {
            mez_agent::ContextExecutionGroupId::new(group.clone()).map_err(context_error)?;
            let next = ordinals.entry(group.as_str()).or_default();
            *next = next
                .checked_add(1)
                .ok_or_else(|| invalid("replay escrow ordinal exhausted"))?;
            if item.ordinal != Some(*next) {
                return Err(invalid("invalid replay escrow group ordinal"));
            }
        }
        sequence = item.sequence;
    }
    let by_id = items
        .iter()
        .map(|item| (item.occurrence_id(), item))
        .collect::<BTreeMap<_, _>>();
    for item in items {
        if let Some(first) = item.task_start {
            let first = by_id
                .get(&first)
                .ok_or_else(|| invalid("task boundary source is missing"))?;
            if item.source != ContextSourceKind::UserInstruction
                || first.sequence > item.sequence
                || first.origin != item.origin
            {
                return Err(invalid("invalid task boundary source"));
            }
        }
    }
    Ok(())
}

/// Selects current raw members and their source-only dependencies. Task roots
/// may retain an earlier covered suffix boundary; group metadata must keep its
/// original ordinal baseline. Neither dependency becomes model membership.
pub(super) fn copy_selection(
    items: &[EscrowOccurrence],
    current: &BTreeSet<ContextBlockOccurrenceId>,
) -> Result<Vec<EscrowOccurrence>> {
    let by_id = items
        .iter()
        .map(|item| (item.occurrence_id(), item))
        .collect::<BTreeMap<_, _>>();
    let mut groups = BTreeMap::<&str, Vec<ContextBlockOccurrenceId>>::new();
    for item in items {
        if let Some(group) = item.group.as_deref() {
            groups.entry(group).or_default().push(item.occurrence_id());
        }
    }
    let mut needed = current.clone();
    let mut expanded_groups = BTreeSet::new();
    let mut pending = current
        .iter()
        .copied()
        .collect::<std::collections::VecDeque<_>>();
    while let Some(id) = pending.pop_front() {
        let item = by_id
            .get(&id)
            .ok_or_else(|| invalid("copy source dependency is missing"))?;
        if let Some(first) = item.task_start
            && needed.insert(first)
        {
            pending.push_back(first);
        }
        if let Some(group) = item.group.as_deref()
            && expanded_groups.insert(group)
        {
            for dependency in &groups[group] {
                if needed.insert(*dependency) {
                    pending.push_back(*dependency);
                }
            }
        }
    }
    let selected = items
        .iter()
        .filter(|item| needed.contains(&item.occurrence_id()))
        .cloned()
        .collect::<Vec<_>>();
    validate(&selected)?;
    Ok(selected)
}

/// Freezes only non-model copy dependencies on the storage worker. No later
/// actor read or parent-store lookup may be needed after a NoStore binding.
pub(super) fn copy_backing(
    items: &[EscrowOccurrence],
    owner: &ReplayEpochContext,
    mut resolve: impl FnMut(&Payload) -> Result<String>,
) -> Result<BTreeMap<String, String>> {
    // Original v4 readers have no escrow. Copy admission still requires its
    // explicit migration; this optional capture must not invent old backing.
    if items.is_empty() {
        return Ok(BTreeMap::new());
    }
    let current = owner
        .context()
        .chronology()
        .iter()
        .skip(usize::from(owner.head().is_some()))
        .map(|event| event.record().occurrence_id())
        .collect::<BTreeSet<_>>();
    let mut payloads = BTreeMap::new();
    for item in copy_selection(items, &current)? {
        if !current.contains(&item.occurrence_id()) && !payloads.contains_key(&item.content.sha256)
        {
            let content = resolve(&item.content)?;
            item.content.validate(content.as_bytes())?;
            payloads.insert(item.content.sha256.clone(), content);
        }
    }
    Ok(payloads)
}

/// Every current raw member must already have its exact original backing.
/// A summary anchor is not an original source identity or a second membership.
pub(super) fn validate_current(
    items: &[EscrowOccurrence],
    source: &ReplayEpochContext,
    covered: &[ContextBlockOccurrenceId],
) -> Result<()> {
    // A copied head-only root has no covered or retained originals to back.
    // Ordinary rewrites still require escrow for their covered membership.
    if items.is_empty()
        && covered.is_empty()
        && source.head().is_some()
        && source.context().chronology().len() == 1
    {
        return Ok(());
    }
    let first = items
        .first()
        .ok_or_else(|| invalid("required source escrow is empty"))?;
    let ids = items
        .iter()
        .map(EscrowOccurrence::occurrence_id)
        .collect::<BTreeSet<_>>();
    if extend(items, source, &first.origin, &[])? != items
        || covered.iter().any(|id| !ids.contains(id))
        || source
            .head()
            .is_some_and(|head| ids.contains(&head.occurrence_id()))
    {
        return Err(invalid(
            "replay escrow does not back current original source",
        ));
    }
    Ok(())
}

impl EscrowOccurrence {
    /// Projects an original, still-unarchived occurrence into a copied child's
    /// physical raw history. This is descriptive source, never parent append
    /// settlement. Native records require their actual captured wire image.
    pub(super) fn copy_row(&self, content: String, conversation: &str) -> Result<TranscriptEntry> {
        let event = match self.source {
            ContextSourceKind::UserInstruction | ContextSourceKind::TranscriptUser => {
                if let Some(start) = self.task_start {
                    TranscriptContextEvent::task_admission(
                        self.sequence,
                        start,
                        self.label.clone(),
                        content,
                    )
                } else {
                    TranscriptContextEvent::user_event(self.sequence, self.label.clone(), content)
                }
            }
            ContextSourceKind::McpCatalogSnapshot => {
                TranscriptContextEvent::mcp_catalog_snapshot(content, self.sequence)
            }
            ContextSourceKind::LocalMessage
            | ContextSourceKind::Policy
            | ContextSourceKind::Configuration
            | ContextSourceKind::RuntimeHint
                if self.group.is_none() =>
            {
                TranscriptContextEvent::prompt_boundary_with_event_sequence(
                    self.source,
                    self.sequence,
                    self.label.clone(),
                    content,
                )
            }
            _ => match (&self.group, self.ordinal) {
                (Some(group), Some(ordinal)) => {
                    let provider = self
                        .provider
                        .as_ref()
                        .map(|(api, id)| {
                            mez_agent::ProviderApiCompatibility::from_id(api)
                                .and_then(|api| {
                                    mez_agent::ProviderContinuityOwner::new(api, id.clone())
                                })
                                .ok_or_else(|| invalid("fork source provider owner is invalid"))
                        })
                        .transpose()?;
                    TranscriptContextEvent::execution_block_with_metadata(
                        self.source,
                        self.label.clone(),
                        content,
                        mez_agent::ContextExecutionGroupId::new(group.clone())?,
                        ordinal,
                        provider,
                    )
                }
                (None, None) => TranscriptContextEvent::execution_block(
                    self.source,
                    self.label.clone(),
                    content,
                ),
                _ => None,
            },
        }
        .ok_or_else(|| invalid("fork source requires its original authored wire image"))?;
        let authored = TranscriptContextRecord::new(event, self.record, None)?;
        let row = TranscriptEntry {
            conversation_id: conversation.into(),
            sequence: self.sequence,
            created_at_unix_seconds: 1,
            role: TranscriptRole::System,
            turn_id: self.origin.turn_id.clone(),
            agent_id: self.origin.agent_id.clone(),
            pane_id: self.origin.pane_id.clone(),
            content: authored.to_transcript_content()?,
        };
        row.validate()?;
        let actual = match authored.event() {
            TranscriptContextEvent::ExecutionBlock { content, .. }
            | TranscriptContextEvent::UserEvent { content, .. }
            | TranscriptContextEvent::TaskAdmission { content, .. }
            | TranscriptContextEvent::PromptBoundary { content, .. }
            | TranscriptContextEvent::McpCatalogSnapshot { content, .. } => content,
            _ => return Err(invalid("fork source projection changed semantic framing")),
        };
        if !self.matches(&row, &authored, actual) {
            return Err(invalid(
                "fork source projection changed original occurrence",
            ));
        }
        Ok(row)
    }

    pub(super) fn original_origin(&self) -> &ReplaySourceOrigin {
        &self.origin
    }

    pub(super) fn for_unsettled_copy(mut self) -> Self {
        self.archived = false;
        self
    }

    pub(super) fn has_task_admission(&self) -> bool {
        self.task_start.is_some()
    }
    pub(super) fn is_archived(&self) -> bool {
        self.archived
    }
    pub(super) fn occurrence_id(&self) -> ContextBlockOccurrenceId {
        self.record.occurrence_id()
    }

    pub(super) fn payload(&self) -> &Payload {
        &self.content
    }

    /// A known ID must match its exact original producer and semantic bytes.
    /// Saved stable carriers on delayed copies cannot become new current slots.
    pub(super) fn matches(
        &self,
        row: &TranscriptEntry,
        authored: &TranscriptContextRecord,
        content: &str,
    ) -> bool {
        if row.role != TranscriptRole::System
            || row.turn_id != self.origin.turn_id
            || row.agent_id != self.origin.agent_id
            || row.pane_id != self.origin.pane_id
            || authored.record() != self.record
        {
            return false;
        }
        match authored.event() {
            TranscriptContextEvent::UserEvent {
                event_sequence,
                label,
                content: actual,
            } => {
                self.task_start.is_none()
                    && matches!(
                        self.source,
                        ContextSourceKind::UserInstruction | ContextSourceKind::TranscriptUser
                    )
                    && *event_sequence == self.sequence
                    && label == &self.label
                    && actual == content
            }
            TranscriptContextEvent::TaskAdmission {
                event_sequence,
                task_start,
                label,
                content: actual,
            } => {
                self.source == ContextSourceKind::UserInstruction
                    && self.task_start == Some(*task_start)
                    && *event_sequence == self.sequence
                    && label == &self.label
                    && actual == content
            }
            TranscriptContextEvent::PromptBoundary {
                source,
                event_sequence,
                label,
                content: actual,
                ..
            } => {
                *source == self.source
                    && *event_sequence == Some(self.sequence)
                    && label == &self.label
                    && actual == content
                    && self.group.is_none()
            }
            TranscriptContextEvent::ExecutionBlock {
                source,
                execution_group_id,
                ordinal,
                provider_owner,
                label,
                content: actual,
                ..
            } => {
                *source == self.source
                    && execution_group_id.as_ref().map(|group| group.as_str())
                        == self.group.as_deref()
                    && *ordinal == self.ordinal
                    && provider_owner
                        .as_ref()
                        .map(|owner| (owner.api().as_str(), owner.provider_id()))
                        == self
                            .provider
                            .as_ref()
                            .map(|(api, provider)| (api.as_str(), provider.as_str()))
                    && label == &self.label
                    && actual == content
            }
            TranscriptContextEvent::EnvironmentSnapshot {
                content: actual, ..
            } => {
                self.source == ContextSourceKind::Configuration
                    && self.label == "task environment snapshot"
                    && actual == content
            }
            TranscriptContextEvent::McpCatalogSnapshot {
                event_sequence,
                content: actual,
                ..
            } => {
                self.source == ContextSourceKind::McpCatalogSnapshot
                    && *event_sequence == self.sequence
                    && self.label == mez_agent::MCP_CATALOG_SNAPSHOT_CONTEXT_LABEL
                    && actual == content
            }
            TranscriptContextEvent::NativePeerMessage { event } => {
                self.source == ContextSourceKind::PeerMessage
                    && event.label == self.label
                    && event.content == content
            }
            TranscriptContextEvent::TerminalHandoff { event } => {
                self.source == ContextSourceKind::RuntimeHint
                    && event.label == self.label
                    && event.content == content
            }
            TranscriptContextEvent::RoutedHandoff { content: actual } => {
                self.source == ContextSourceKind::RoutedHandoff
                    && self.label == "routed worker handoff context"
                    && actual == content
            }
            _ => false,
        }
    }
}

/// An unpublished union may contain the already bounded parent backing plus
/// already bounded current source while an actual archive cut is qualified.
/// It is never serialized or acknowledged at this temporary two-source bound.
pub(super) fn qualification_source(
    inherited: &[EscrowOccurrence],
    source: &ReplayEpochContext,
    origin: &ReplaySourceOrigin,
    captured: &[TranscriptEntry],
) -> Result<Vec<EscrowOccurrence>> {
    validate(inherited)?;
    let mut inherited = inherited.to_vec();
    retire_archived(&mut inherited, source);
    extend_with_limit(&inherited, source, origin, captured, SOURCE_LIMIT * 2)
}

/// Retires only originals actually qualified by the frozen archive cut. Current
/// pre-rewrite source remains backed. Whole groups retain their ordinal baseline
/// until every old member is archived and none belongs to the current source.
pub(super) fn publication_source(
    inherited: &[EscrowOccurrence],
    source: &ReplayEpochContext,
    origin: &ReplaySourceOrigin,
    captured: &[TranscriptEntry],
    archived: &BTreeSet<ContextBlockOccurrenceId>,
) -> Result<Vec<EscrowOccurrence>> {
    let mut items = qualification_source(inherited, source, origin, captured)?;
    let ids = items
        .iter()
        .map(EscrowOccurrence::occurrence_id)
        .collect::<BTreeSet<_>>();
    if archived.iter().any(|id| !ids.contains(id)) {
        return Err(invalid("archive cut names unknown source"));
    }
    for item in &mut items {
        item.archived |= archived.contains(&item.occurrence_id());
    }
    retire_archived(&mut items, source);
    validate(&items)?;
    Ok(items)
}

fn retire_archived(items: &mut Vec<EscrowOccurrence>, source: &ReplayEpochContext) {
    let mut required = source
        .context()
        .chronology()
        .iter()
        .skip(usize::from(source.head().is_some()))
        .map(|event| event.record().occurrence_id())
        .collect::<BTreeSet<_>>();
    // A retained/unsettled task root still requires its exact original boundary,
    // including after that prelude itself entered the durable archive cut.
    for item in items.iter() {
        if (!item.archived || required.contains(&item.occurrence_id()))
            && let Some(first) = item.task_start
        {
            required.insert(first);
        }
    }
    let groups = items
        .iter()
        .filter(|item| !item.archived || required.contains(&item.occurrence_id()))
        .filter_map(|item| item.group.clone())
        .collect::<BTreeSet<_>>();
    items.retain(|item| {
        !item.archived
            || required.contains(&item.occurrence_id())
            || item
                .group
                .as_ref()
                .is_some_and(|group| groups.contains(group))
    });
}

fn is_false(value: &bool) -> bool {
    !value
}

/// Whether classification is preparing new authored source or checking a
/// publication whose complete semantic source must already be escrow-backed.
pub(super) enum RowAdmission {
    KnownCopies,
    AuthoredArrivals,
}

/// Exact copies and original new rows are separate source domains. New rows
/// remain unadopted until the runtime validates accounting and combined native
/// source; classification alone grants no execution or append authority.
pub(super) struct QualifiedRows {
    pub(super) copies: BTreeSet<u64>,
    pub(super) arrivals: Vec<TranscriptEntry>,
}

/// Qualifies zero-model lifecycle controls against original escrow and actual
/// authored rows in this same frozen logical view. New rows must precede their
/// control; an escrow-backed member may still await ordinary append. Every
/// descriptor names the complete ordinal prefix of its original producer group.
/// This proves source custody, never an effect or ordinary append receipt.
pub(super) fn qualify_controls(
    items: &[EscrowOccurrence],
    conversation: &str,
    rows: &[TranscriptEntry],
    indexed: &BTreeMap<ContextBlockOccurrenceId, super::super::row_origins::IndexedExecutionSource>,
) -> Result<Vec<mez_agent::ReplayExecutionState>> {
    struct Member {
        group: String,
        ordinal: u64,
        sequence: Option<u64>,
        physical: Option<u64>,
        origin: ReplaySourceOrigin,
    }
    let mut members = items
        .iter()
        .filter_map(|item| {
            Some((
                item.occurrence_id(),
                Member {
                    group: item.group.clone()?,
                    ordinal: item.ordinal?,
                    sequence: Some(item.sequence),
                    physical: None,
                    origin: item.origin.clone(),
                },
            ))
        })
        .collect::<BTreeMap<_, _>>();
    for (id, source) in indexed {
        let origin = ReplaySourceOrigin::new(&source.turn_id, &source.agent_id, &source.pane_id)?;
        if let Some(member) = members.get_mut(id) {
            if member.group != source.group
                || member.ordinal != source.ordinal
                || member.origin != origin
            {
                return Err(invalid("indexed execution conflicts with original escrow"));
            }
            member.physical = Some(source.image.sequence);
        } else {
            members.insert(
                *id,
                Member {
                    group: source.group.clone(),
                    ordinal: source.ordinal,
                    sequence: None,
                    physical: Some(source.image.sequence),
                    origin,
                },
            );
        }
    }
    let mut seen = BTreeSet::new();
    let mut controls = Vec::new();
    for row in rows {
        if row.conversation_id != conversation {
            return Err(invalid(
                "execution control source has a foreign conversation",
            ));
        }
        if row.role != TranscriptRole::System {
            continue;
        }
        if let Some(control) =
            mez_agent::transcript::TranscriptExecutionStateRecord::from_transcript_content(
                &row.content,
            )?
        {
            controls.push((row, control));
            continue;
        }
        let Some(record) = TranscriptContextRecord::from_transcript_content(&row.content)? else {
            continue;
        };
        let TranscriptContextEvent::ExecutionBlock {
            execution_group_id: Some(group),
            ordinal: Some(ordinal),
            ..
        } = record.event()
        else {
            continue;
        };
        let id = record.record().occurrence_id();
        if !seen.insert(id) {
            return Err(invalid("execution control source repeats an occurrence"));
        }
        let origin = ReplaySourceOrigin::new(&row.turn_id, &row.agent_id, &row.pane_id)?;
        if let Some(original) = members.get(&id) {
            if original.group != group.as_str()
                || original.ordinal != *ordinal
                || original.origin != origin
            {
                return Err(invalid(
                    "execution control source changed original ownership",
                ));
            }
        } else {
            members.insert(
                id,
                Member {
                    group: group.as_str().into(),
                    ordinal: *ordinal,
                    sequence: None,
                    physical: Some(row.sequence),
                    origin,
                },
            );
        }
    }
    let mut result = Vec::new();
    for (row, control) in controls {
        let origin = ReplaySourceOrigin::new(&row.turn_id, &row.agent_id, &row.pane_id)?;
        for (index, (id, sequence)) in control.state().members().iter().enumerate() {
            let original = members
                .get(id)
                .ok_or_else(|| invalid("execution control has no original member custody"))?;
            if original.group != control.state().group()
                || original.ordinal != index as u64 + 1
                || original.origin != origin
                || original.sequence.is_some_and(|actual| actual != *sequence)
                || original
                    .physical
                    .is_some_and(|physical| physical >= row.sequence)
            {
                return Err(invalid(
                    "execution control changed original group frontier or producer",
                ));
            }
        }
        result.push(control.state().clone());
    }
    Ok(result)
}

/// Checks original semantic copies and zero-model display/audit rows from one
/// already validated archive view. The resolver uses proposed bytes before
/// publication or required immutable files during replay; no lock is acquired.
pub(super) fn qualify_rows(
    items: &[EscrowOccurrence],
    conversation: &str,
    rows: &[TranscriptEntry],
    indexed: &BTreeMap<ContextBlockOccurrenceId, super::super::row_origins::IndexedExecutionSource>,
    admission: RowAdmission,
    mut resolve: impl FnMut(&Payload) -> Result<String>,
) -> Result<QualifiedRows> {
    let index = items
        .iter()
        .map(|item| (item.occurrence_id(), item))
        .collect::<BTreeMap<_, _>>();
    let mut seen = BTreeSet::new();
    let mut qualified = BTreeSet::new();
    let mut events = Vec::with_capacity(rows.len());
    let mut groups = BTreeSet::new();
    let mut arrivals = Vec::new();
    let mut arrival_sequences = BTreeSet::new();
    if rows.iter().any(|row| {
        row.role == TranscriptRole::System
            && row
                .content
                .starts_with(mez_agent::transcript::EXECUTION_STATE_RECORD_MARKER)
    }) {
        drop(qualify_controls(items, conversation, rows, indexed)?);
    }
    let mut control_sequences = BTreeSet::new();
    for row in rows {
        if row.conversation_id != conversation {
            return Err(invalid("replay source copy has a foreign conversation"));
        }
        if row.role != TranscriptRole::System {
            events.push(None);
            continue;
        }
        if mez_agent::transcript::TranscriptExecutionStateRecord::from_transcript_content(
            &row.content,
        )?
        .is_some()
        {
            control_sequences.insert(row.sequence);
            events.push(None);
            continue;
        }
        let Some(authored) = TranscriptContextRecord::from_transcript_content(&row.content)? else {
            if TranscriptContextEvent::from_transcript_content(&row.content).is_some()
                || row.content.starts_with("[mez-transcript-context-event/")
            {
                return Err(invalid(
                    "unaccounted replay semantic source lacks arrival custody",
                ));
            }
            events.push(None);
            continue;
        };
        if !seen.insert(authored.record().occurrence_id()) {
            return Err(invalid("replay row repeats an original source occurrence"));
        }
        if let Some(original) = index.get(&authored.record().occurrence_id()) {
            let content = resolve(original.payload())?;
            if !original.matches(row, &authored, &content) {
                return Err(invalid(
                    "late replay row conflicts with its original source escrow",
                ));
            }
            qualified.insert(row.sequence);
        } else if matches!(admission, RowAdmission::AuthoredArrivals)
            && matches!(
                authored.event(),
                TranscriptContextEvent::UserEvent { .. }
                    | TranscriptContextEvent::TaskAdmission { .. }
                    | TranscriptContextEvent::PromptBoundary {
                        event_sequence: Some(_),
                        ..
                    }
                    | TranscriptContextEvent::ExecutionBlock {
                        execution_group_id: Some(_),
                        ordinal: Some(_),
                        ..
                    }
                    | TranscriptContextEvent::EnvironmentSnapshot { .. }
                    | TranscriptContextEvent::McpCatalogSnapshot { .. }
                    | TranscriptContextEvent::NativePeerMessage { .. }
                    | TranscriptContextEvent::TerminalHandoff { .. }
                    | TranscriptContextEvent::RoutedHandoff { .. }
            )
        {
            ReplaySourceOrigin::new(&row.turn_id, &row.agent_id, &row.pane_id)?;
            arrival_sequences.insert(row.sequence);
            arrivals.push(row.clone());
        } else {
            return Err(invalid(
                "new replay semantic source lacks original arrival custody",
            ));
        }
        if let TranscriptContextEvent::ExecutionBlock {
            execution_group_id: Some(group),
            ..
        } = authored.event()
        {
            groups.insert(group.clone());
        }
        events.push(Some(authored.into_event()));
    }
    let zero = mez_agent::transcript::transcript_zero_projection_indices(rows, &events, &groups);
    if rows.iter().enumerate().any(|(index, row)| {
        !qualified.contains(&row.sequence)
            && !arrival_sequences.contains(&row.sequence)
            && !control_sequences.contains(&row.sequence)
            && !zero.contains(&index)
    }) {
        return Err(invalid("replay archive has unqualified model source"));
    }
    Ok(QualifiedRows {
        copies: qualified,
        arrivals,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mez_agent::{AgentContext, ReplayConversationId};

    /// Partially archived groups keep their original ordinal baseline. Even a
    /// fully archived group stays while it backs the current pre-rewrite source.
    #[test]
    fn escrow_retirement_requires_whole_groups_and_preserves_current_source() {
        let origin = ReplaySourceOrigin::new("turn", "agent", "pane").unwrap();
        let mut source = ReplayEpochContext::new(
            ReplayConversationId::new("group-escrow").unwrap(),
            AgentContext::empty(),
        )
        .unwrap();
        let group = mez_agent::ContextExecutionGroupId::new("original-group").unwrap();
        source
            .append_execution_events([
                mez_agent::ContextConversationAppend::assistant("one", "first", group.clone()),
                mez_agent::ContextConversationAppend::assistant("two", "second", group),
            ])
            .unwrap();
        let items = extend(&[], &source, &origin, &[]).unwrap();
        let first = BTreeSet::from([items[0].occurrence_id()]);
        let empty = ReplayEpochContext::new(
            ReplayConversationId::new("group-escrow").unwrap(),
            AgentContext::empty(),
        )
        .unwrap();
        let partial = publication_source(&items, &empty, &origin, &[], &first).unwrap();
        assert_eq!(partial.len(), 2);
        assert_eq!(partial[0].ordinal, Some(1));
        assert_eq!(partial[1].ordinal, Some(2));
        assert!(partial[0].is_archived());
        assert!(!partial[1].is_archived());
        let all = items.iter().map(EscrowOccurrence::occurrence_id).collect();
        let current = publication_source(&items, &source, &origin, &[], &all).unwrap();
        assert_eq!(current.len(), 2);
        assert!(current.iter().all(EscrowOccurrence::is_archived));
        assert!(
            publication_source(&partial, &empty, &origin, &[], &all)
                .unwrap()
                .is_empty()
        );
    }

    /// Descriptor-only bounds must allow qualifying a bounded parent plus new
    /// current source before retirement, but never publish that larger union.
    #[test]
    fn escrow_temporary_union_does_not_weaken_final_unsettled_limit() {
        let origin = ReplaySourceOrigin::new("turn", "agent", "pane").unwrap();
        let mut source = ReplayEpochContext::new(
            ReplayConversationId::new("bounded-escrow").unwrap(),
            AgentContext::empty(),
        )
        .unwrap();
        source.append_fact("old", "old source").unwrap();
        let mut old = extend(&[], &source, &origin, &[]).unwrap();
        // Synthetic declared size exercises metadata admission without allocating
        // 64 MiB. This helper test does not establish physical payload custody.
        old[0].content.bytes = SOURCE_LIMIT;
        let mut current = ReplayEpochContext::new(
            ReplayConversationId::new("bounded-escrow").unwrap(),
            AgentContext::empty(),
        )
        .unwrap();
        current.append_fact("new", "new source").unwrap();
        assert_eq!(
            qualification_source(&old, &current, &origin, &[])
                .unwrap()
                .len(),
            2
        );
        assert!(publication_source(&old, &current, &origin, &[], &BTreeSet::new()).is_err());
        let settled = BTreeSet::from([old[0].occurrence_id()]);
        let final_source = publication_source(&old, &current, &origin, &[], &settled).unwrap();
        assert_eq!(final_source.len(), 1);
        assert_eq!(
            final_source[0].occurrence_id(),
            current.context().chronology()[0].record().occurrence_id()
        );
    }

    /// Equal text remains separate backing; inherited provenance cannot be
    /// rebound to the next turn, and the logical backing limit is checked from
    /// metadata before any large payload allocation or file read.
    #[test]
    fn escrow_keeps_original_producers_distinct_and_bounds_unsettled_backing() {
        let origin = ReplaySourceOrigin::new("first-turn", "agent", "pane").unwrap();
        let other = ReplaySourceOrigin::new("next-turn", "agent", "pane").unwrap();
        let mut source = ReplayEpochContext::new(
            ReplayConversationId::new("escrow").unwrap(),
            AgentContext::empty(),
        )
        .unwrap();
        source.append_fact("fact", "same text").unwrap();
        let first = extend(&[], &source, &origin, &[]).unwrap();
        source.append_fact("fact", "same text").unwrap();
        let both = extend(&first, &source, &other, &[]).unwrap();
        assert_eq!(both[0].origin, origin);
        assert_eq!(both[1].origin, other);
        assert_ne!(
            both[0].record.occurrence_id(),
            both[1].record.occurrence_id()
        );
        assert_eq!(both[0].content, both[1].content);
        let mut oversized = both.clone();
        oversized[0].content.bytes = SOURCE_LIMIT;
        assert!(validate(&oversized).is_err());
        let mut duplicated = both;
        duplicated[1].record = duplicated[0].record;
        assert!(validate(&duplicated).is_err());
        assert!(ReplaySourceOrigin::new("", "agent", "pane").is_err());
    }

    /// An archived prelude still backs an unsettled root. Delayed source must
    /// carry its exact task boundary, not a changed source with equal text.
    #[test]
    fn escrow_task_boundary_survives_partial_settlement_and_rejects_changed_copy() {
        let origin = ReplaySourceOrigin::new("task", "agent", "pane").unwrap();
        let mut source = ReplayEpochContext::new(
            ReplayConversationId::new("task-escrow").unwrap(),
            AgentContext::empty(),
        )
        .unwrap();
        source
            .admit_instruction_task(
                vec![
                    mez_agent::ContextBlock::task_prelude(
                        ContextSourceKind::SkillInstruction,
                        "skill",
                        "exact skill",
                    ),
                    mez_agent::ContextBlock::user_event("user prompt", "exact task"),
                ],
                &[],
                &[],
                1,
            )
            .unwrap();
        let items = extend(&[], &source, &origin, &[]).unwrap();
        assert_eq!(items[1].task_start, Some(items[0].occurrence_id()));
        let empty = ReplayEpochContext::new(
            ReplayConversationId::new("task-escrow").unwrap(),
            AgentContext::empty(),
        )
        .unwrap();
        let partial = publication_source(
            &items,
            &empty,
            &origin,
            &[],
            &BTreeSet::from([items[0].occurrence_id()]),
        )
        .unwrap();
        assert_eq!(partial.len(), 2);
        assert!(partial[0].archived);
        assert!(!partial[1].archived);
        assert!(
            publication_source(
                &partial,
                &empty,
                &origin,
                &[],
                &BTreeSet::from([items[1].occurrence_id()])
            )
            .unwrap()
            .is_empty()
        );
        let root = &source.context().chronology()[1];
        let row = TranscriptEntry {
            conversation_id: "task-escrow".into(),
            sequence: 2,
            created_at_unix_seconds: 123,
            role: TranscriptRole::System,
            turn_id: "task".into(),
            agent_id: "agent".into(),
            pane_id: "pane".into(),
            content: String::new(),
        };
        for (first, expected) in [
            (items[0].occurrence_id(), true),
            (items[1].occurrence_id(), false),
        ] {
            let event = TranscriptContextEvent::task_admission(
                root.sequence().get(),
                first,
                &root.block().label,
                &root.block().content,
            )
            .unwrap();
            let authored = TranscriptContextRecord::new(event, root.record(), None).unwrap();
            assert_eq!(
                items[1].matches(&row, &authored, &root.block().content),
                expected
            );
        }
    }
}
