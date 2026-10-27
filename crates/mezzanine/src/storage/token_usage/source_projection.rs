//! Bounded read-only native-source checkpoint reconstruction, not a new ledger.
//!
//! Observation aliases and independently recomputed owner/epoch stream digests
//! establish membership. A single deferred read transaction freezes metadata and
//! absolute checkpoints; no event, alias, request or charge is created. Possession
//! of this snapshot grants no native admission. The cold worker separately owns
//! native proof, accepted lifetime/claim and final actor publication fences.

use super::{
    AccountingProjectId, ExternalCounters, ExternalSourceKey, ExternalUsageCommit,
    ExternalUsageReport, MezError, Result, TokenUsageStore, external_observation_epoch,
    external_usage_stream_id,
};
use rusqlite::{OptionalExtension, params};
use std::collections::BTreeMap;

/// One source-owned absolute checkpoint and immutable expense attribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceUsagePartition {
    /// Canonical checkpoint, not a newly applied expense delta.
    pub(crate) commit: ExternalUsageCommit,
    /// Durable view-only reset point, never subtracted from global expense.
    pub(crate) reset_baseline: ExternalCounters,
    /// Frozen request origin, including explicit unattributed state.
    pub(crate) project: Option<AccountingProjectId>,
    /// Original response provider/model, never current registration defaults.
    pub(crate) model: mez_agent::ModelTokenUsageKey,
}

/// Sealed coherent read evidence; neither an admission nor a durability ACK.
pub(crate) struct ExternalSourceUsageSnapshot {
    /// Exact server-issued namespace obtained from the original native key.
    pub(crate) owner: String,
    /// Harness bound by that same source mapping.
    pub(crate) harness: String,
    /// Unique bounded streams, not one projection per observation alias.
    pub(crate) partitions: Vec<SourceUsagePartition>,
}

impl std::fmt::Debug for ExternalSourceUsageSnapshot {
    /// Diagnostics expose only cardinality, not opaque ownership or attribution.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalSourceUsageSnapshot")
            .field("partitions", &self.partitions.len())
            .finish_non_exhaustive()
    }
}

/// Reads bounded raw bytes before decoding SQLite TEXT, including embedded NUL.
fn text(row: &rusqlite::Row<'_>, index: usize) -> Result<String> {
    String::from_utf8(row.get(index)?)
        .map_err(|_| MezError::invalid_state("external source checkpoint encoding invalid"))
}

/// Canonical stored digests cannot borrow an unrelated stream or alias.
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Completed request markers independently require their original same-owner
/// observation alias. Enumerating aliases alone cannot detect a deleted or
/// mis-owned member. This bounded scan shares the checkpoint read transaction;
/// genuinely uncompleted/no-request sources do not fabricate missing evidence.
fn require_completed_request_aliases(
    tx: &rusqlite::Transaction<'_>,
    owner: &str,
    harness: &str,
) -> Result<()> {
    let mut query=tx.prepare("SELECT substr(CAST(r.request_id AS BLOB),1,65),substr(CAST(r.harness AS BLOB),1,129),substr(CAST(r.observation_id AS BLOB),1,65),substr(CAST(o.request_id AS BLOB),1,65),substr(CAST(o.event_id AS BLOB),1,65) FROM external_usage_requests r LEFT JOIN external_usage_observations o ON o.owner=r.owner AND o.event_id=r.observation_id WHERE r.owner=?1 AND r.observation_id IS NOT NULL ORDER BY r.request_id LIMIT 65537")?;
    let mut rows = query.query([owner])?;
    let mut count = 0;
    while let Some(row) = rows.next()? {
        count += 1;
        if count > 65536 {
            return Err(MezError::new(
                crate::error::MezErrorKind::RateLimited,
                "external source completed request capacity unavailable",
            ));
        }
        let marker = text(row, 0)?;
        let stored_harness = text(row, 1)?;
        let event = text(row, 2)?;
        let alias_marker: Option<Vec<u8>> = row.get(3)?;
        let alias_event: Option<Vec<u8>> = row.get(4)?;
        if !digest(&marker)
            || !digest(&event)
            || stored_harness != harness
            || alias_marker.as_deref() != Some(marker.as_bytes())
            || alias_event.as_deref() != Some(event.as_bytes())
        {
            return Err(MezError::conflict(
                "external completed request lost its observation evidence",
            ));
        }
    }
    Ok(())
}

impl TokenUsageStore {
    /// Worker-only exact-source read. Missing/future/corrupt/legacy or over-cap
    /// evidence refuses; no namespace is allocated and no schema is migrated.
    pub(crate) fn external_source_usage_snapshot(
        &self,
        key: &ExternalSourceKey,
    ) -> Result<ExternalSourceUsageSnapshot> {
        let mut connection =
            crate::storage::shared_sqlite::open_shared_database_read_only(self.path())?
                .ok_or_else(|| {
                    MezError::conflict("external source checkpoint store unavailable")
                })?;
        connection.busy_timeout(std::time::Duration::from_millis(250))?;
        let tx = connection.transaction()?;
        let version: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version != super::store::SCHEMA_VERSION {
            return Err(MezError::invalid_state(
                "external source checkpoint schema unavailable",
            ));
        }
        let mapping:Option<(Vec<u8>,Vec<u8>)>=tx.query_row("SELECT substr(CAST(owner AS BLOB),1,129),substr(CAST(harness AS BLOB),1,129) FROM external_usage_sources WHERE witness=?1",[&key.digest],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
        let (owner, harness) = mapping
            .ok_or_else(|| MezError::conflict("external source checkpoint identity unavailable"))?;
        let owner = String::from_utf8(owner)
            .map_err(|_| MezError::invalid_state("external source owner encoding invalid"))?;
        if !super::external_sources::valid_uuid(&owner) || harness != key.harness.as_bytes() {
            return Err(MezError::conflict(
                "external source checkpoint identity changed",
            ));
        }
        require_completed_request_aliases(&tx, &owner, &key.harness)?;
        let mut partitions = BTreeMap::new();
        {
            // Per-source aliases already have a 65536-row durable bound. Read
            // one extra to detect corruption/exhaustion and decode only bounded
            // fields. LEFT JOIN keeps missing checkpoints visible as errors.
            let mut query=tx.prepare("SELECT substr(CAST(o.event_id AS BLOB),1,65),substr(CAST(o.fingerprint AS BLOB),1,65),substr(CAST(o.stream_id AS BLOB),1,65),substr(CAST(s.harness AS BLOB),1,129),substr(CAST(s.provider AS BLOB),1,129),substr(CAST(s.model AS BLOB),1,129),substr(CAST(s.mode AS BLOB),1,17),s.revision,substr(CAST(s.totals AS BLOB),1,1025),substr(CAST(s.project_id AS BLOB),1,129) FROM external_usage_observations o LEFT JOIN external_usage_streams s ON s.id=o.stream_id WHERE o.owner=?1 ORDER BY o.event_id LIMIT 65537")?;
            let mut rows = query.query(params![owner])?;
            let mut count = 0;
            while let Some(row) = rows.next()? {
                count += 1;
                if count > 65536 {
                    return Err(MezError::new(
                        crate::error::MezErrorKind::RateLimited,
                        "external source checkpoint alias capacity unavailable",
                    ));
                }
                let event = text(row, 0)?;
                let fingerprint = text(row, 1)?;
                let id = text(row, 2)?;
                if !digest(&event)
                    || !digest(&fingerprint)
                    || !digest(&id)
                    || text(row, 3)? != key.harness
                    || text(row, 6)? != "observation"
                {
                    return Err(MezError::conflict(
                        "external source checkpoint membership unavailable",
                    ));
                }
                let model = mez_agent::ModelTokenUsageKey {
                    provider: text(row, 4)?,
                    model: text(row, 5)?,
                };
                for value in [&model.provider, &model.model] {
                    if value.trim().is_empty()
                        || value.len() > 128
                        || value.chars().any(char::is_control)
                    {
                        return Err(MezError::invalid_state(
                            "external source checkpoint model invalid",
                        ));
                    }
                }
                let revision = super::store::row_u64(row, 7)?;
                let encoded = text(row, 8)?;
                if revision == 0 || encoded.len() > 1024 {
                    return Err(MezError::invalid_state(
                        "external source checkpoint revision invalid",
                    ));
                }
                let totals: ExternalCounters = serde_json::from_str(&encoded).map_err(|_| {
                    MezError::invalid_state("external source checkpoint counters invalid")
                })?;
                totals.validate()?;
                let project = row
                    .get::<_, Option<Vec<u8>>>(9)?
                    .map(|bytes| {
                        String::from_utf8(bytes).map_err(|_| {
                            MezError::invalid_state("external source checkpoint origin invalid")
                        })
                    })
                    .transpose()?
                    .map(AccountingProjectId::from_stored)
                    .transpose()?;
                let report = ExternalUsageReport {
                    owner: owner.clone(),
                    harness: key.harness.clone(),
                    project: project.clone(),
                    model: model.clone(),
                    counters: totals,
                    epoch: String::new(),
                    event_id: event,
                    sequence: 1,
                    mode: "observation".into(),
                    baseline: false,
                    observed_at: 1,
                };
                if external_usage_stream_id(&owner, &external_observation_epoch(&report)?)? != id {
                    return Err(MezError::conflict(
                        "external source checkpoint stream identity changed",
                    ));
                }
                let commit = ExternalUsageCommit {
                    stream_id: id.clone(),
                    revision,
                    totals,
                    applied: false,
                };
                let partition = SourceUsagePartition {
                    reset_baseline: super::view_reset::baseline(&tx, &commit)?,
                    commit,
                    project,
                    model,
                };
                if let Some(previous) = partitions.get(&id) {
                    if previous != &partition {
                        return Err(MezError::conflict(
                            "external source checkpoint alias disagreement",
                        ));
                    }
                } else {
                    if partitions.len() >= 4096 {
                        return Err(MezError::new(
                            crate::error::MezErrorKind::RateLimited,
                            "external source checkpoint stream capacity unavailable",
                        ));
                    }
                    partitions.insert(id, partition);
                }
            }
        }
        tx.commit()?;
        Ok(ExternalSourceUsageSnapshot {
            owner,
            harness: key.harness.clone(),
            partitions: partitions.into_values().collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pure storage fixture; synthetic keys NEVER stand in for native admission.
    struct Fixture {
        directory: std::path::PathBuf,
        store: TokenUsageStore,
        key: ExternalSourceKey,
        owner: String,
    }
    impl Fixture {
        /// Creates only a unique test-owned namespace and its real ledger rows.
        fn new() -> Self {
            let directory = std::env::temp_dir().join(format!(
                "mez-source-checkpoint-{}",
                crate::storage::token_usage::new_token_usage_event_id()
            ));
            std::fs::create_dir(&directory).unwrap();
            let store = TokenUsageStore::new(directory.join("usage.sqlite"));
            let key = ExternalSourceKey {
                digest: "a".repeat(64),
                harness: "pi".into(),
            };
            let owner = store.external_source_owner(&key).unwrap();
            Self {
                directory,
                store,
                key,
                owner,
            }
        }
        /// Persists actual normalized immutable observations, not mock commits.
        fn report(&self, event: char, model: &str, input: u64) -> ExternalUsageCommit {
            self.report_for_owner(&self.owner, event, model, input)
        }
        /// Writes under a separately mapped fixture namespace for isolation.
        fn report_for_owner(
            &self,
            owner: &str,
            event: char,
            model: &str,
            input: u64,
        ) -> ExternalUsageCommit {
            let mut report = ExternalUsageReport {
                owner: owner.into(),
                harness: "pi".into(),
                project: None,
                epoch: String::new(),
                event_id: event.to_string().repeat(64),
                sequence: 1,
                mode: "observation".into(),
                baseline: false,
                observed_at: 100,
                model: mez_agent::ModelTokenUsageKey {
                    provider: "fixture".into(),
                    model: model.into(),
                },
                counters: ExternalCounters {
                    input_tokens: input,
                    ..Default::default()
                },
            };
            report.epoch = external_observation_epoch(&report).unwrap();
            self.store.ingest_external(&report, 100).unwrap()
        }
    }
    impl Drop for Fixture {
        /// No native processes/tasks exist in this synchronous owned fixture.
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    /// Multiple event aliases for one absolute checkpoint count only once;
    /// models/projects/categories remain distinct and original bytes are kept.
    /// A real WAL writer can coexist with this read-only committed snapshot.
    #[test]
    fn source_projection_snapshot_deduplicates_aliases_and_reads_under_writer() {
        let fixture = Fixture::new();
        fixture.report('b', " model-a ", 10);
        fixture.report('c', " model-a ", 2);
        fixture.report('d', "other", 5);
        let writer = rusqlite::Connection::open(fixture.store.path()).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        let snapshot = fixture
            .store
            .external_source_usage_snapshot(&fixture.key)
            .unwrap();
        assert_eq!(snapshot.owner, fixture.owner);
        assert_eq!(snapshot.partitions.len(), 2);
        assert_eq!(
            snapshot
                .partitions
                .iter()
                .map(|partition| partition.commit.totals.input_tokens)
                .sum::<u64>(),
            17
        );
        assert!(
            snapshot
                .partitions
                .iter()
                .any(|partition| partition.model.model == " model-a "
                    && partition.commit.revision == 2
                    && partition.commit.totals.input_tokens == 12)
        );
        assert!(
            snapshot
                .partitions
                .iter()
                .all(|partition| !partition.commit.applied)
        );
        writer.execute_batch("ROLLBACK").unwrap();
        assert_eq!(
            writer
                .query_row(
                    "SELECT count(*),sum(input_tokens) FROM token_usage_events",
                    [],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
                )
                .unwrap(),
            (3, 17)
        );
    }

    /// Reset persists the captured point, not later incurred expense. Repeated
    /// and reversed reset workers cannot regress a newer accepted baseline;
    /// source snapshots recover unreplayed model partitions without new charges.
    #[test]
    fn source_projection_reset_is_captured_monotonic_and_expense_neutral() {
        let fixture = Fixture::new();
        let first = fixture.report('b', "model-a", 10);
        let other = fixture.report('c', "model-b", 5);
        let late = fixture.report('d', "model-a", 7);
        fixture
            .store
            .reset_external_usage_view(&[first.clone(), other.clone()])
            .unwrap();
        let snapshot = fixture
            .store
            .external_source_usage_snapshot(&fixture.key)
            .unwrap();
        assert_eq!(snapshot.partitions.len(), 2);
        assert_eq!(
            snapshot
                .partitions
                .iter()
                .map(|partition| partition.commit.totals.input_tokens)
                .sum::<u64>(),
            22
        );
        assert_eq!(
            snapshot
                .partitions
                .iter()
                .map(|partition| partition.reset_baseline.input_tokens)
                .sum::<u64>(),
            15
        );
        fixture
            .store
            .reset_external_usage_view(std::slice::from_ref(&late))
            .unwrap();
        fixture
            .store
            .reset_external_usage_view(&[first, other])
            .unwrap();
        let snapshot = fixture
            .store
            .external_source_usage_snapshot(&fixture.key)
            .unwrap();
        assert_eq!(
            snapshot
                .partitions
                .iter()
                .map(|partition| partition.reset_baseline.input_tokens)
                .sum::<u64>(),
            22
        );
        let connection = rusqlite::Connection::open(fixture.store.path()).unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*),sum(input_tokens) FROM token_usage_events",
                    [],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
                )
                .unwrap(),
            (3, 22)
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM external_usage_view_resets",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            2
        );
    }

    /// A failed second member rolls back the first baseline. Forged revision or
    /// counter evidence refuses; malformed persisted baseline cannot turn into
    /// a plausible zero pane view. Legacy migration adds no reset facts.
    #[test]
    fn source_projection_reset_refuses_partial_corrupt_and_legacy_evidence() {
        let fixture = Fixture::new();
        let first = fixture.report('b', "model-a", 10);
        let connection = rusqlite::Connection::open(fixture.store.path()).unwrap();
        let mut missing = first.clone();
        missing.stream_id = "f".repeat(64);
        assert!(
            fixture
                .store
                .reset_external_usage_view(&[first.clone(), missing])
                .is_err()
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM external_usage_view_resets",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        let mut forged = first.clone();
        forged.totals.input_tokens = 9;
        assert!(fixture.store.reset_external_usage_view(&[forged]).is_err());
        fixture
            .store
            .reset_external_usage_view(std::slice::from_ref(&first))
            .unwrap();
        connection
            .execute(
                "UPDATE external_usage_view_resets SET revision=revision+1",
                [],
            )
            .unwrap();
        assert!(
            fixture
                .store
                .external_source_usage_snapshot(&fixture.key)
                .is_err()
        );
        connection
            .execute_batch("DROP TABLE external_usage_view_resets; PRAGMA user_version=11;")
            .unwrap();
        assert!(
            fixture
                .store
                .external_source_usage_snapshot(&fixture.key)
                .is_err()
        );
        fixture.store.initialize(100).unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            12
        );
        assert_eq!(
            fixture
                .store
                .external_source_usage_snapshot(&fixture.key)
                .unwrap()
                .partitions[0]
                .reset_baseline
                .input_tokens,
            0
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT sum(input_tokens) FROM token_usage_events",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            10
        );
    }

    /// Missing stores and unknown future schemas must not be created/migrated;
    /// missing checkpoints, oversized/raw-control fields and foreign stream
    /// aliases refuse rather than returning a valid partial or empty snapshot.
    #[test]
    fn source_projection_snapshot_refuses_corruption_and_never_creates_store() {
        let absent = Fixture::new();
        let missing = absent.directory.join("absent.sqlite");
        assert!(
            TokenUsageStore::new(&missing)
                .external_source_usage_snapshot(&absent.key)
                .is_err()
        );
        assert!(!missing.exists());
        for corruption in 0..6 {
            let fixture = Fixture::new();
            let original = fixture.report('b', "model-a", 10);
            let connection = rusqlite::Connection::open(fixture.store.path()).unwrap();
            match corruption {
                0 => {
                    connection
                        .execute("DELETE FROM external_usage_streams", [])
                        .unwrap();
                }
                1 => {
                    connection
                        .execute(
                            "UPDATE external_usage_streams SET totals=?1",
                            ["x".repeat(2048)],
                        )
                        .unwrap();
                }
                2 => {
                    connection
                        .execute(
                            "UPDATE external_usage_streams SET provider=?1",
                            ["fixture\0private"],
                        )
                        .unwrap();
                }
                3 => {
                    connection
                        .execute_batch("PRAGMA user_version=9999")
                        .unwrap();
                }
                4 => {
                    let foreign_key = ExternalSourceKey {
                        digest: "f".repeat(64),
                        harness: "pi".into(),
                    };
                    let foreign_owner = fixture.store.external_source_owner(&foreign_key).unwrap();
                    let foreign = fixture.report_for_owner(&foreign_owner, 'c', "foreign-model", 7);
                    // The referenced checkpoint really EXISTS and is valid,
                    // but its digest must not select another source's spend.
                    connection
                        .execute(
                            "UPDATE external_usage_observations SET stream_id=?1 WHERE owner=?2",
                            params![foreign.stream_id, fixture.owner],
                        )
                        .unwrap();
                }
                _ => {
                    connection
                        .execute(
                            "UPDATE external_usage_streams SET mode='delta' WHERE id=?1",
                            [original.stream_id],
                        )
                        .unwrap();
                }
            }
            assert!(
                fixture
                    .store
                    .external_source_usage_snapshot(&fixture.key)
                    .is_err(),
                "accepted source corruption case{corruption}"
            );
            assert_eq!(
                connection
                    .query_row(
                        "SELECT count(*),sum(input_tokens) FROM token_usage_events",
                        [],
                        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
                    )
                    .unwrap(),
                if corruption == 4 { (2, 17) } else { (1, 10) }
            );
            if corruption == 3 {
                assert_eq!(
                    connection
                        .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                        .unwrap(),
                    9999
                );
            }
        }
    }
}
