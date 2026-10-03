//! Scoped history conservation, category coverage and attribution replay tests.
//!
//! Tests qualify the storage reader independently of producer integration. No
//! legacy expense is reconstructed from current cwd or transcript state.

use super::*;
use crate::security::project::{ProjectTrustStore, TrustDecision};
use mez_agent::{ModelTokenUsage, ModelTokenUsageKey};

/// Partitioned rows may each fit while their legacy same-model display cannot.
/// Validate the entire display fold before returning tables, and check known
/// cache categories independently of ordinary input/output counters.
#[test]
fn attributed_history_checks_cross_partition_and_optional_overflow() {
    let (store, projects) = fixture();
    for (index, project) in [projects[0].id.clone(), projects[1].id.clone(), None]
        .into_iter()
        .enumerate()
    {
        store
            .append(&TokenUsageEvent {
                id: format!("partition-overflow-{index}"),
                project,
                observed_at_unix_seconds: 100,
                model: ModelTokenUsageKey::new("provider", "model"),
                usage: ModelTokenUsage {
                    input_tokens: i64::MAX as u64,
                    ..Default::default()
                },
            })
            .unwrap();
    }
    let snapshot = store
        .history_snapshot(
            100,
            &[1],
            &TokenHistoryScope {
                native_only: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(snapshot.native_model_windows().is_err());
    let (store, _) = fixture();
    for index in 0..3 {
        store
            .append(&TokenUsageEvent {
                id: format!("cache-overflow-{index}"),
                project: None,
                observed_at_unix_seconds: 100,
                model: ModelTokenUsageKey::new("provider", "model"),
                usage: ModelTokenUsage {
                    input_tokens: 1,
                    cache_write_input_tokens: Some(i64::MAX as u64),
                    ..Default::default()
                },
            })
            .unwrap();
    }
    assert!(
        store
            .history_snapshot(100, &[1], &TokenHistoryScope::default())
            .is_err()
    );
}

/// Individually valid SQLite counters can overflow a window total. The reader
/// must report unavailable exact history rather than return a saturated sum.
#[test]
fn attributed_history_rejects_aggregate_overflow() {
    let (store, projects) = fixture();
    for index in 0..3 {
        store
            .append(&TokenUsageEvent {
                id: format!("overflow-{index}"),
                project: projects[0].id.clone(),
                observed_at_unix_seconds: 100,
                model: ModelTokenUsageKey::new("provider", "model"),
                usage: ModelTokenUsage {
                    input_tokens: i64::MAX as u64,
                    cached_input_tokens: Some(i64::MAX as u64),
                    ..Default::default()
                },
            })
            .unwrap();
    }
    assert!(
        store
            .history_snapshot(100, &[1], &TokenHistoryScope::default())
            .is_err()
    );
}

/// A populated v3 database upgrades without attributing legacy expense. A
/// migration failure rolls back both added columns and the version; the old
/// external receipt fingerprint remains valid after successful recovery.
#[test]
fn attributed_migration_preserves_legacy_receipts_and_rolls_back_failure() {
    let (store, _) = fixture();
    let report = ExternalUsageReport {
        owner: "migration-owner".to_string(),
        project: None,
        harness: "codex".to_string(),
        epoch: "migration-epoch".to_string(),
        event_id: "migration-event".to_string(),
        sequence: 1,
        mode: "delta".to_string(),
        baseline: false,
        observed_at: 100,
        model: ModelTokenUsageKey::new("provider", "model"),
        counters: ExternalCounters {
            input_tokens: 7,
            output_tokens: 2,
            ..Default::default()
        },
    };
    store.ingest_external(&report, 100).unwrap();
    let connection = rusqlite::Connection::open(store.path()).unwrap();
    connection
        .execute_batch(
            "DROP INDEX token_usage_events_project_time;
        ALTER TABLE token_usage_events DROP COLUMN project_id;
        ALTER TABLE external_usage_streams DROP COLUMN project_id;
        PRAGMA user_version=3;
        CREATE INDEX token_usage_events_project_time ON token_usage_events(observed_at);",
        )
        .unwrap();
    assert!(store.initialize(100).is_err());
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        3
    );
    assert!(
        connection
            .prepare("SELECT project_id FROM token_usage_events")
            .is_err()
    );
    assert!(
        connection
            .prepare("SELECT project_id FROM external_usage_streams")
            .is_err()
    );
    connection
        .execute_batch("DROP INDEX token_usage_events_project_time;")
        .unwrap();
    store.initialize(100).unwrap();
    assert!(!store.ingest_external(&report, 100).unwrap().applied);
    let snapshot = store
        .history_snapshot(100, &[1], &TokenHistoryScope::default())
        .unwrap();
    assert_eq!(
        snapshot.windows[&1]
            .values()
            .next()
            .unwrap()
            .usage
            .input_tokens,
        7
    );
    assert!(snapshot.windows[&1].keys().all(|key| key.project.is_none()));
}

/// Prepares two opaque mapped projects and an isolated usage store.
fn fixture() -> (TokenUsageStore, Vec<AccountingProjectRecord>) {
    let base = std::env::temp_dir().join(format!("mez-history-{}", new_token_usage_event_id()));
    std::fs::create_dir_all(base.join("a")).unwrap();
    std::fs::create_dir_all(base.join("b")).unwrap();
    let store = TokenUsageStore::new(base.join("usage.sqlite"));
    let mut trust = ProjectTrustStore::default();
    for name in ["a", "b"] {
        trust
            .decide(base.join(name), TrustDecision::Trusted, None)
            .unwrap();
    }
    let rows = store
        .prepare_accounting_projects(&trust.records().cloned().collect::<Vec<_>>())
        .unwrap();
    (store, rows)
}

/// Exact lower cutoffs are included and future events excluded. Project sums
/// plus unattributed expense equal global raw totals; identical model names
/// across harnesses stay distinct and omitted reasoning/cache remain unknown.
#[test]
fn attributed_history_conserves_partitions_and_unknown_coverage() {
    let (store, projects) = fixture();
    let now = 100 * 86_400;
    let model = ModelTokenUsageKey::new("provider", "model");
    for (id, project, input, at) in [
        ("a", projects[0].id.clone(), 7, now - 86_400),
        ("b", projects[1].id.clone(), 11, now),
        ("legacy", None, 13, now),
        ("future", None, 99, now + 1),
    ] {
        store
            .append(&TokenUsageEvent {
                id: id.to_string(),
                project,
                observed_at_unix_seconds: at,
                model: model.clone(),
                usage: ModelTokenUsage {
                    input_tokens: input,
                    output_tokens: 2,
                    reasoning_tokens: 1,
                    cached_input_tokens: Some(0),
                    cache_write_input_tokens: None,
                },
            })
            .unwrap();
    }
    let external = ExternalUsageReport {
        owner: "owner".to_string(),
        project: projects[0].id.clone(),
        harness: "codex".to_string(),
        epoch: "epoch".to_string(),
        event_id: "external".to_string(),
        sequence: 1,
        mode: "delta".to_string(),
        baseline: false,
        observed_at: now,
        model,
        counters: ExternalCounters {
            input_tokens: 17,
            output_tokens: 2,
            ..Default::default()
        },
    };
    store.ingest_external(&external, now).unwrap();
    let global = store
        .history_snapshot(now, &[1, 7], &TokenHistoryScope::default())
        .unwrap();
    assert_eq!(global.now, now);
    assert_eq!(global.oldest_observed_at, Some(now - 86_400));
    let sum = |window: &std::collections::BTreeMap<_, super::history::TokenHistoryUsage>| {
        window
            .values()
            .map(|value| value.usage.input_tokens)
            .sum::<u64>()
    };
    assert_eq!(sum(&global.windows[&1]), 48);
    let mut partitioned = 0;
    for project in &projects {
        let snapshot = store
            .history_snapshot(
                now,
                &[1],
                &TokenHistoryScope {
                    project: project.id.clone(),
                    ..Default::default()
                },
            )
            .unwrap();
        partitioned += sum(&snapshot.windows[&1]);
    }
    partitioned += sum(&store
        .history_snapshot(
            now,
            &[1],
            &TokenHistoryScope {
                unattributed_only: true,
                ..Default::default()
            },
        )
        .unwrap()
        .windows[&1]);
    assert_eq!(partitioned, 48);
    let external = global.windows[&1]
        .iter()
        .find(|(key, _)| key.harness == "codex")
        .unwrap()
        .1;
    assert!(!external.reasoning_known);
    assert_eq!(external.usage.cached_input_tokens, None);
    assert_eq!(global.windows[&1].len(), 4);
}

/// Reusing an immutable native ID under another project must fail, preserving
/// its original partition rather than acknowledging an attribution rewrite.
#[test]
fn attributed_event_replay_cannot_change_project() {
    let (store, projects) = fixture();
    let first = TokenUsageEvent {
        id: "immutable".to_string(),
        project: projects[0].id.clone(),
        observed_at_unix_seconds: 100,
        model: ModelTokenUsageKey::new("provider", "model"),
        usage: ModelTokenUsage {
            input_tokens: 7,
            ..Default::default()
        },
    };
    assert!(store.append(&first).unwrap());
    assert!(!store.append(&first).unwrap());
    let mut changed = first.clone();
    changed.project = projects[1].id.clone();
    assert!(store.append(&changed).is_err());
    let snapshot = store
        .history_snapshot(100, &[1], &TokenHistoryScope::default())
        .unwrap();
    assert_eq!(
        snapshot.windows[&1].keys().next().unwrap().project,
        first.project
    );
}
