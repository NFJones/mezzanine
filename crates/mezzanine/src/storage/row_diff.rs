//! Pure keyed row-diff planning shared by independently persisted stores.
//!
//! Callers validate unique row keys before planning. Deletions precede writes,
//! and each phase preserves its input order; database I/O remains store-owned.

use std::collections::{HashMap, HashSet};

/// One write needed to transform a validated collection of stored rows.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum RowWrite<'a, T> {
    /// A new row must be inserted.
    Insert(&'a T),
    /// An existing row has changed.
    Update(&'a T),
    /// A row no longer exists.
    Delete(&'a str),
}

/// Plans writes for uniquely keyed rows without changing either collection.
///
/// Deletes follow `before` order, then updates and inserts follow `after`
/// order. Unchanged rows are omitted. The caller must validate unique keys
/// in both collections; duplicate keys are not diagnosed by this planner.
pub(super) fn pending_writes<'a, T: PartialEq>(
    before: &'a [T],
    after: &'a [T],
    key: impl Fn(&T) -> &str,
) -> Vec<RowWrite<'a, T>> {
    let before_by_id: HashMap<&str, &T> = before.iter().map(|row| (key(row), row)).collect();
    let after_ids: HashSet<&str> = after.iter().map(&key).collect();
    let mut writes = Vec::new();
    for row in before {
        if !after_ids.contains(key(row)) {
            writes.push(RowWrite::Delete(key(row)));
        }
    }
    for row in after {
        match before_by_id.get(key(row)) {
            Some(existing) if *existing == row => {}
            Some(_) => writes.push(RowWrite::Update(row)),
            None => writes.push(RowWrite::Insert(row)),
        }
    }
    writes
}
