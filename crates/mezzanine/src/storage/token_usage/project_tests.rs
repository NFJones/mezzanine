//! Accounting mapping identity and historical root preservation regressions.
//!
//! These test metadata-only mapping, not trust or token-event backfill. Directory
//! replacement is explicit and keeps the old object alive to avoid inode reuse.

use super::*;
use crate::security::project::{ProjectTrustStore, TrustDecision};

/// Canonical aliases share opaque IDs across reopen. A replaced root cannot use
/// stale evidence, receives a new mapping, and preserves the historical ID even
/// after its directory disappears. Zero-use records need no token event.
#[test]
fn accounting_projects_aliases_replacement_and_history_are_distinct() {
    let base = std::env::temp_dir().join(format!(
        "mez-accounting-projects-{}",
        new_token_usage_event_id()
    ));
    let root = base.join("root");
    std::fs::create_dir_all(&root).unwrap();
    let alias = base.join("alias");
    std::os::unix::fs::symlink(&root, &alias).unwrap();
    let store = TokenUsageStore::new(base.join("usage.sqlite"));
    let mut trust = ProjectTrustStore::default();
    trust
        .decide(root.clone(), TrustDecision::Trusted, None)
        .unwrap();
    let records = trust.records().cloned().collect::<Vec<_>>();
    let first = store.prepare_accounting_projects(&records).unwrap();
    let original = first[0].id.clone().unwrap();
    assert_eq!(
        accounting_origin_for_root(Some(&alias), Some(&first)),
        AccountingOrigin::Project(original.clone())
    );
    assert_eq!(
        store.clone().prepare_accounting_projects(&records).unwrap()[0].id,
        Some(original.clone())
    );
    std::fs::rename(&root, base.join("old-root")).unwrap();
    std::fs::create_dir(&root).unwrap();
    assert_eq!(
        accounting_origin_for_root(Some(&root), Some(&first)),
        AccountingOrigin::Unattributed
    );
    let replacement = store.prepare_accounting_projects(&records).unwrap();
    assert_ne!(replacement[0].id, Some(original.clone()));
    std::fs::remove_dir(&root).unwrap();
    let missing = store.prepare_accounting_projects(&records).unwrap();
    assert!(missing[0].id.is_none());
    assert_eq!(store.historical_accounting_projects().unwrap().len(), 2);
    assert!(
        store
            .historical_accounting_projects()
            .unwrap()
            .iter()
            .any(|(id, path)| *id == original && *path == root)
    );
    assert!(store.aggregate_windows(i64::MAX as u64, &[1]).unwrap()[&1].is_empty());
    std::fs::remove_dir_all(base).unwrap();
}

/// Mapping all registered states does not grant attribution without an eligible
/// trusted-root input. Missing inventory remains unavailable rather than zero.
#[test]
fn accounting_projects_inventory_does_not_grant_trust() {
    let base = std::env::temp_dir().join(format!(
        "mez-accounting-inventory-{}",
        new_token_usage_event_id()
    ));
    std::fs::create_dir_all(base.join("revoked")).unwrap();
    let store = TokenUsageStore::new(base.join("usage.sqlite"));
    let mut trust = ProjectTrustStore::default();
    trust
        .decide(base.join("revoked"), TrustDecision::Revoked, None)
        .unwrap();
    let rows = store
        .prepare_accounting_projects(&trust.records().cloned().collect::<Vec<_>>())
        .unwrap();
    assert_eq!(rows[0].trust, TrustDecision::Revoked);
    assert!(rows[0].id.is_some());
    assert_eq!(
        accounting_origin_for_root(None, Some(&rows)),
        AccountingOrigin::Unattributed
    );
    assert_eq!(
        accounting_origin_for_root(Some(&base), None),
        AccountingOrigin::Unattributed
    );
    std::fs::remove_dir_all(base).unwrap();
}
