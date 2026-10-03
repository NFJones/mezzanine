//! Transactional external telemetry streams and durable replay receipts.
//!
//! A server-issued owner namespaces each epoch. Modes never mix within an epoch;
//! cumulative attachment starts with an explicit uncharged baseline. Optional
//! counters cannot change availability midstream: a new epoch is required. A
//! durable sequence high-water mark prevents old replay after receipt pruning.
//! Normalized deltas, receipts and checkpoints commit together, without prompts,
//! transcript text, pane paths or vendor payloads.

use mez_agent::{ModelTokenUsage, ModelTokenUsageKey};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{MezError, Result, TokenUsageStore, sqlite_i64};

/// Content-free counters; omitted fields stay unknown rather than becoming zero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExternalCounters {
    /// Inclusive provider-visible input tokens.
    pub input_tokens: u64,
    /// Inclusive provider-visible output tokens.
    pub output_tokens: u64,
    /// Reasoning subset of output, or unknown.
    pub reasoning_tokens: Option<u64>,
    /// Cache-read subset of input, or unknown.
    pub cached_input_tokens: Option<u64>,
    /// Cache-write subset of input, or unknown.
    pub cache_write_input_tokens: Option<u64>,
}

impl ExternalCounters {
    /// Rejects overflow and inconsistent inclusive cache/reasoning subsets.
    pub(crate) fn validate(self) -> Result<()> {
        for value in [
            Some(self.input_tokens),
            Some(self.output_tokens),
            self.reasoning_tokens,
            self.cached_input_tokens,
            self.cache_write_input_tokens,
        ]
        .into_iter()
        .flatten()
        {
            sqlite_i64(value, "external counter")?;
        }
        if self
            .reasoning_tokens
            .is_some_and(|n| n > self.output_tokens)
            || self
                .cached_input_tokens
                .is_some_and(|n| n > self.input_tokens)
            || self
                .cache_write_input_tokens
                .is_some_and(|n| n > self.input_tokens)
        {
            return Err(MezError::invalid_args(
                "external counters must use inclusive input/output subsets",
            ));
        }
        Ok(())
    }

    /// Computes a monotonic difference without guessing unknown categories.
    pub(crate) fn difference(self, previous: Self) -> Result<Self> {
        fn scalar(next: u64, old: u64) -> Result<u64> {
            next.checked_sub(old).ok_or_else(|| {
                MezError::conflict("external cumulative counters regressed; start a new epoch")
            })
        }
        fn optional(next: Option<u64>, old: Option<u64>) -> Result<Option<u64>> {
            match (next, old) {
                (Some(next), Some(old)) => scalar(next, old).map(Some),
                (None, None) => Ok(None),
                _ => Err(MezError::conflict(
                    "external counter availability changed; start a new epoch",
                )),
            }
        }
        Ok(Self {
            input_tokens: scalar(self.input_tokens, previous.input_tokens)?,
            output_tokens: scalar(self.output_tokens, previous.output_tokens)?,
            reasoning_tokens: optional(self.reasoning_tokens, previous.reasoning_tokens)?,
            cached_input_tokens: optional(self.cached_input_tokens, previous.cached_input_tokens)?,
            cache_write_input_tokens: optional(
                self.cache_write_input_tokens,
                previous.cache_write_input_tokens,
            )?,
        })
    }

    /// Adds disjoint observations with checked arithmetic and unknown fidelity.
    fn add(self, delta: Self) -> Result<Self> {
        fn scalar(a: u64, b: u64) -> Result<u64> {
            a.checked_add(b)
                .filter(|n| *n <= i64::MAX as u64)
                .ok_or_else(|| {
                    MezError::invalid_args("external stream total exceeded SQLite range")
                })
        }
        fn optional(a: Option<u64>, b: Option<u64>) -> Result<Option<u64>> {
            match (a, b) {
                (Some(a), Some(b)) => scalar(a, b).map(Some),
                _ => Ok(None),
            }
        }
        Ok(Self {
            input_tokens: scalar(self.input_tokens, delta.input_tokens)?,
            output_tokens: scalar(self.output_tokens, delta.output_tokens)?,
            reasoning_tokens: optional(self.reasoning_tokens, delta.reasoning_tokens)?,
            cached_input_tokens: optional(self.cached_input_tokens, delta.cached_input_tokens)?,
            cache_write_input_tokens: optional(
                self.cache_write_input_tokens,
                delta.cache_write_input_tokens,
            )?,
        })
    }

    /// Creates a zero baseline retaining the sample's category availability.
    pub(crate) fn zero_like(self) -> Self {
        Self {
            reasoning_tokens: self.reasoning_tokens.map(|_| 0),
            cached_input_tokens: self.cached_input_tokens.map(|_| 0),
            cache_write_input_tokens: self.cache_write_input_tokens.map(|_| 0),
            ..Self::default()
        }
    }

    /// Adapts to existing normalized totals; coverage retains unknown reasoning.
    pub(crate) fn normalized(self) -> ModelTokenUsage {
        ModelTokenUsage {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            reasoning_tokens: self.reasoning_tokens.unwrap_or(0),
            cached_input_tokens: self.cached_input_tokens,
            cache_write_input_tokens: self.cache_write_input_tokens,
        }
    }
}

/// Exact actor-authorized immutable accounting report. Owner/harness are server-owned.
#[derive(Debug, Clone)]
pub(crate) struct ExternalUsageReport {
    /// Opaque server-issued accounting namespace, not client-selected identity.
    pub owner: String,
    /// Server-frozen project origin; never a hook-supplied label.
    pub project: Option<super::AccountingProjectId>,
    /// Harness fixed by launch authority.
    pub harness: String,
    /// Disjoint source epoch within the registered owner.
    pub epoch: String,
    /// Stable observation identity used for fingerprint comparison.
    pub event_id: String,
    /// Positive source sequence/high-water revision.
    pub sequence: u64,
    /// Delta or cumulative stream semantics, immutable within the epoch.
    pub mode: String,
    /// Whether the first cumulative observation is an uncharged attachment.
    pub baseline: bool,
    /// Observation time used for rolling windows and replay eligibility.
    pub observed_at: u64,
    /// Normalized model identity, immutable within the epoch.
    pub model: ModelTokenUsageKey,
    /// Allowlisted inclusive counters, never raw vendor telemetry.
    pub counters: ExternalCounters,
}

/// Durable checkpoint returned even on replay, allowing projection/reply-loss recovery.
#[derive(Debug, Clone)]
pub(crate) struct ExternalUsageCommit {
    /// Digest of the server owner and disjoint epoch.
    pub stream_id: String,
    /// Current durable source sequence.
    pub revision: u64,
    /// Absolute charged total, excluding attachment baseline.
    pub totals: ExternalCounters,
    /// Whether this invocation committed a new checkpoint.
    pub applied: bool,
}

/// Encodes bounded normalized state, never raw vendor data.
fn encode(value: ExternalCounters) -> Result<String> {
    serde_json::to_string(&value)
        .map_err(|_| MezError::invalid_state("external counter encoding failed"))
}

/// Decodes validated stored normalized state.
fn decode(value: &str) -> Result<ExternalCounters> {
    let counters: ExternalCounters = serde_json::from_str(value)
        .map_err(|_| MezError::invalid_state("stored external counters are invalid"))?;
    counters.validate()?;
    Ok(counters)
}

/// Returns the same opaque stream identity for admission and durable storage.
pub(crate) fn external_usage_stream_id(owner: &str, epoch: &str) -> Result<String> {
    Ok(Sha256::digest(
        serde_json::to_vec(&(owner, epoch))
            .map_err(|_| MezError::invalid_state("external stream encoding failed"))?,
    )
    .iter()
    .map(|byte| format!("{byte:02x}"))
    .collect())
}

impl TokenUsageStore {
    /// Commits receipt, checkpoint and normalized delta in one writer transaction.
    /// Identical replay is inert, conflicting IDs fail, stale sequences cannot
    /// revive pruned observations, and a cumulative baseline is never charged.
    pub(crate) fn ingest_external(
        &self,
        report: &ExternalUsageReport,
        now: u64,
    ) -> Result<ExternalUsageCommit> {
        report.counters.validate()?;
        for field in [
            &report.owner,
            &report.harness,
            &report.epoch,
            &report.event_id,
            &report.model.provider,
            &report.model.model,
        ] {
            if field.is_empty() || field.len() > 128 || field.chars().any(char::is_control) {
                return Err(MezError::invalid_args(
                    "external accounting identifiers must be bounded inert text",
                ));
            }
        }
        if !matches!(report.mode.as_str(), "delta" | "cumulative")
            || report.sequence == 0
            || (report.baseline && report.mode != "cumulative")
        {
            return Err(MezError::invalid_args(
                "invalid external accounting mode, sequence or baseline",
            ));
        }
        if report.observed_at > now || report.observed_at < now.saturating_sub(91 * 86_400) {
            return Err(MezError::invalid_args(
                "external usage is outside the 91-day replay horizon",
            ));
        }
        let stream_id = external_usage_stream_id(&report.owner, &report.epoch)?;
        let mut payload = serde_json::json!({"harness":report.harness,"provider":report.model.provider,
            "model":report.model.model,"sequence":report.sequence,"mode":report.mode,
            "baseline":report.baseline,"observed_at":report.observed_at,"counters":report.counters});
        // Preserve pre-v4 fingerprints for unattributed streams. A qualified
        // project is additional immutable provenance, not migration-time backfill.
        if let Some(project) = &report.project {
            payload["project"] = serde_json::json!(project.as_str());
        }
        let payload = payload.to_string();
        let fingerprint = Sha256::digest(payload.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let mut connection = self.open()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        // Each admitted report also maintains long-lived-session retention.
        // High-water checkpoints are never pruned with their raw receipts.
        let cutoff = sqlite_i64(now.saturating_sub(91 * 86_400), "external retention cutoff")?;
        tx.execute(
            "DELETE FROM external_usage_receipts WHERE observed_at < ?1",
            [cutoff],
        )?;
        tx.execute(
            "DELETE FROM token_usage_events WHERE event_source='external' AND observed_at < ?1",
            [cutoff],
        )?;
        let state = tx.query_row("SELECT harness, provider, model, mode, revision, sample, totals, project_id FROM external_usage_streams WHERE id=?1",
            [&stream_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
                row.get::<_, String>(3)?, super::store::row_u64(row, 4)?, row.get::<_, String>(5)?, row.get::<_, String>(6)?, row.get::<_, Option<String>>(7)?))).optional()?;
        let receipt = tx.query_row("SELECT fingerprint FROM external_usage_receipts WHERE stream_id=?1 AND event_id=?2",
            params![stream_id, report.event_id], |row| row.get::<_, String>(0)).optional()?;
        if let Some(receipt) = receipt {
            if receipt != fingerprint {
                return Err(MezError::conflict(
                    "external usage event id has conflicting payload",
                ));
            }
            let (_, _, _, _, revision, _, totals, _) = state
                .ok_or_else(|| MezError::invalid_state("external receipt lost its checkpoint"))?;
            return Ok(ExternalUsageCommit {
                stream_id,
                revision,
                totals: decode(&totals)?,
                applied: false,
            });
        }
        let (old_revision, old_sample, old_totals) = match state {
            Some((harness, provider, model, mode, revision, sample, totals, project)) => {
                if harness != report.harness
                    || project.as_deref()
                        != report
                            .project
                            .as_ref()
                            .map(super::AccountingProjectId::as_str)
                    || provider != report.model.provider
                    || model != report.model.model
                    || mode != report.mode
                {
                    return Err(MezError::conflict(
                        "external stream identity or mode changed; start a new epoch",
                    ));
                }
                if report.baseline {
                    return Err(MezError::conflict(
                        "external stream baseline already exists",
                    ));
                }
                if report.sequence <= revision {
                    if report.mode == "delta" {
                        return Err(MezError::conflict(
                            "external delta sequence is stale or outside replay horizon",
                        ));
                    }
                    return Ok(ExternalUsageCommit {
                        stream_id,
                        revision,
                        totals: decode(&totals)?,
                        applied: false,
                    });
                }
                (revision, decode(&sample)?, decode(&totals)?)
            }
            None => {
                if report.mode == "cumulative" && !report.baseline {
                    return Err(MezError::conflict(
                        "external cumulative attachment requires an explicit uncharged baseline",
                    ));
                }
                (0, report.counters.zero_like(), report.counters.zero_like())
            }
        };
        let delta = if report.baseline {
            report.counters.zero_like()
        } else if report.mode == "cumulative" {
            report.counters.difference(old_sample)?
        } else {
            if report.sequence != old_revision.saturating_add(1) {
                return Err(MezError::conflict(
                    "external delta sequence has a gap; submit missing observations first",
                ));
            }
            report.counters
        };
        if report.mode == "delta" && old_revision > 0 {
            report.counters.difference(old_sample.zero_like())?;
        }
        delta.validate()?;
        let totals = old_totals.add(delta)?;
        tx.execute("INSERT INTO external_usage_streams(id,harness,provider,model,mode,revision,sample,totals,project_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)
            ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,sample=excluded.sample,totals=excluded.totals",
            params![stream_id, report.harness, report.model.provider, report.model.model, report.mode,
                sqlite_i64(report.sequence, "external sequence")?, encode(report.counters)?, encode(totals)?, report.project.as_ref().map(super::AccountingProjectId::as_str)])?;
        tx.execute("INSERT INTO external_usage_receipts(stream_id,event_id,fingerprint,observed_at) VALUES(?1,?2,?3,?4)",
            params![stream_id, report.event_id, fingerprint, sqlite_i64(report.observed_at, "external timestamp")?])?;
        let normalized = delta.normalized();
        if !normalized.is_zero() {
            let id = format!("external:{stream_id}:{}", report.sequence);
            tx.execute("INSERT INTO token_usage_events(id,observed_at,provider,model,input_tokens,output_tokens,reasoning_tokens,cached_input_tokens,cache_write_input_tokens,harness,reasoning_known,event_source,project_id)
                VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'external',?12)", params![id, sqlite_i64(report.observed_at, "external timestamp")?,
                    report.model.provider, report.model.model, sqlite_i64(normalized.input_tokens,"input")?,
                    sqlite_i64(normalized.output_tokens,"output")?, sqlite_i64(normalized.reasoning_tokens,"reasoning")?,
                    normalized.cached_input_tokens.map(|n| sqlite_i64(n,"cache")).transpose()?,
                    normalized.cache_write_input_tokens.map(|n| sqlite_i64(n,"cache write")).transpose()?, report.harness,
                    i64::from(delta.reasoning_tokens.is_some()), report.project.as_ref().map(super::AccountingProjectId::as_str)])?;
        }
        tx.commit()?;
        Ok(ExternalUsageCommit {
            stream_id,
            revision: report.sequence,
            totals,
            applied: true,
        })
    }
}
