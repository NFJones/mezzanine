//! Actor-owned reconciliation of worker-published OpenCode checkpoint groups.
//!
//! Plans validate exact scope and monotonic checkpoint members before replacing
//! cache entries. Reset baselines move with their original projected members;
//! delayed replies route through a bounded member index and never recreate a
//! second projection. This module performs no storage, admission or source
//! recovery and never treats a snapshot as new expense or producer authority.

use super::external_usage::{ExternalUsageProjection, ExternalUsageWork, ProjectedStream};
use crate::error::{MezError, MezErrorKind, Result};
use crate::storage::token_usage::{
    ExternalCounters, ExternalUsageCommit, ExternalUsageGroupSnapshot,
};
use std::collections::BTreeMap;

/// Mirrors the durable global observation limit; no index evidence is evicted.
const MAX_MEMBERS: usize = 1_048_576;

/// Checks original presentation provenance rather than current focus/root.
pub(super) fn same_scope(stream: &ProjectedStream, work: &ExternalUsageWork) -> bool {
    stream.pane_id == work.pane_id
        && stream.process.same_incarnation(&work.process)
        && stream.harness == work.report.harness
        && stream.model == work.report.model
        && stream.project == work.report.project
        && stream.totals.reasoning_tokens.is_some()
            == work.report.counters.reasoning_tokens.is_some()
        && stream.totals.cached_input_tokens.is_some()
            == work.report.counters.cached_input_tokens.is_some()
        && stream.totals.cache_write_input_tokens.is_some()
            == work.report.counters.cache_write_input_tokens.is_some()
}

/// Retains highest absolute member evidence; equal revisions must agree exactly.
fn merge(
    members: &mut BTreeMap<String, ExternalUsageCommit>,
    next: &ExternalUsageCommit,
) -> Result<()> {
    if let Some(old) = members.get(&next.stream_id) {
        if old.revision == next.revision && old.totals != next.totals {
            return Err(MezError::conflict("external group member revision changed"));
        }
        if old.revision >= next.revision {
            return Ok(());
        }
        next.totals.difference(old.totals)?;
    }
    let mut next = next.clone();
    next.applied = false;
    members.insert(next.stream_id.clone(), next);
    Ok(())
}

/// Derives checked absolute aggregate state from unique current members.
fn aggregate(
    members: &BTreeMap<String, ExternalUsageCommit>,
    zero: ExternalCounters,
) -> Result<(u64, ExternalCounters)> {
    let mut revision = 0u64;
    let mut totals = zero;
    for member in members.values() {
        revision = revision
            .checked_add(member.revision)
            .filter(|value| *value <= i64::MAX as u64)
            .ok_or_else(|| MezError::invalid_state("external group revision exhausted"))?;
        totals = totals.add(member.totals)?;
    }
    Ok((revision, totals))
}

/// Validates and atomically installs a coherent snapshot without summing deltas.
pub(super) fn reconcile(
    projection: &mut ExternalUsageProjection,
    work: &ExternalUsageWork,
    snapshot: &ExternalUsageGroupSnapshot,
    limit: usize,
) -> Result<()> {
    let id = snapshot.stream_id();
    let existing = projection.streams.get(id);
    if existing.is_some_and(|stream| !same_scope(stream, work)) {
        return Err(MezError::conflict(
            "external group projection scope changed",
        ));
    }
    let mut members = existing
        .and_then(|stream| stream.members.clone())
        .unwrap_or_default();
    let mut baseline = existing
        .map(|stream| stream.reset_baseline)
        .unwrap_or(snapshot.totals().zero_like());
    if let Some(stream) = existing.filter(|stream| stream.members.is_none()) {
        merge(
            &mut members,
            &ExternalUsageCommit {
                stream_id: id.to_owned(),
                revision: stream.revision,
                totals: stream.totals,
                applied: false,
            },
        )?;
    }
    let mut remove = Vec::new();
    for member in snapshot.members() {
        if projection
            .member_groups
            .get(&member.stream_id)
            .is_some_and(|group| group != id)
        {
            return Err(MezError::conflict(
                "external checkpoint belongs to another group",
            ));
        }
        if member.stream_id != id
            && let Some(stream) = projection.streams.get(&member.stream_id)
        {
            if !same_scope(stream, work) || stream.members.is_some() {
                return Err(MezError::conflict(
                    "external member projection scope changed",
                ));
            }
            merge(
                &mut members,
                &ExternalUsageCommit {
                    stream_id: member.stream_id.clone(),
                    revision: stream.revision,
                    totals: stream.totals,
                    applied: false,
                },
            )?;
            baseline = baseline.add(stream.reset_baseline)?;
            remove.push(member.stream_id.clone());
        }
        merge(&mut members, member)?;
    }
    let additions = members
        .keys()
        .filter(|key| !projection.member_groups.contains_key(*key))
        .count();
    let reserved = projection
        .reservations
        .keys()
        .filter(|key| {
            key.as_str() != id && !projection.streams.contains_key(*key) && !remove.contains(key)
        })
        .count();
    let final_size = projection.streams.len() - remove.len() + usize::from(existing.is_none());
    if final_size + reserved > limit || projection.member_groups.len() + additions > MAX_MEMBERS {
        return Err(MezError::new(
            MezErrorKind::RateLimited,
            "external group projection capacity exhausted",
        ));
    }
    let (revision, totals) = aggregate(&members, snapshot.totals().zero_like())?;
    totals.difference(baseline)?;
    let replacement = ProjectedStream {
        pane_id: work.pane_id.clone(),
        process: work.process.clone(),
        project: work.report.project.clone(),
        harness: work.report.harness.clone(),
        model: work.report.model.clone(),
        revision,
        totals,
        reset_baseline: baseline,
        members: Some(members),
    };
    for member in replacement
        .members
        .as_ref()
        .into_iter()
        .flat_map(|members| members.keys())
    {
        projection
            .member_groups
            .insert(member.clone(), id.to_owned());
    }
    for member in remove {
        projection.streams.remove(&member);
    }
    projection.streams.insert(id.to_owned(), replacement);
    Ok(())
}

/// Updates a previously consolidated member in place; stale replies are inert.
pub(super) fn complete_member(
    projection: &mut ExternalUsageProjection,
    work: &ExternalUsageWork,
    commit: &ExternalUsageCommit,
) -> Result<bool> {
    // A generic checkpoint can arrive after the group was installed but before
    // any alias/self-member existed. Its canonical ID is already occupied by
    // the aggregate; it must never enter the ordinary checkpoint fallback.
    let id = projection
        .member_groups
        .get(&commit.stream_id)
        .cloned()
        .or_else(|| {
            projection
                .streams
                .get(&commit.stream_id)
                .filter(|stream| stream.members.is_some())
                .map(|_| commit.stream_id.clone())
        });
    let Some(id) = id else {
        return Ok(false);
    };
    let addition = !projection.member_groups.contains_key(&commit.stream_id);
    if addition && projection.member_groups.len() >= MAX_MEMBERS {
        return Err(MezError::new(
            MezErrorKind::RateLimited,
            "external group member capacity exhausted",
        ));
    }
    let stream = projection
        .streams
        .get_mut(&id)
        .ok_or_else(|| MezError::invalid_state("external group projection disappeared"))?;
    if !same_scope(stream, work) {
        return Err(MezError::conflict("external late member scope changed"));
    }
    let mut members = stream
        .members
        .clone()
        .ok_or_else(|| MezError::invalid_state("external group members unavailable"))?;
    merge(&mut members, commit)?;
    let (revision, totals) = aggregate(&members, stream.totals.zero_like())?;
    totals.difference(stream.reset_baseline)?;
    stream.members = Some(members);
    stream.revision = revision;
    stream.totals = totals;
    if addition {
        projection
            .member_groups
            .insert(commit.stream_id.clone(), id);
    }
    Ok(true)
}
