//! Bounded daemon-owned selectors for callbacks to existing ordinary producers.
//!
//! Only enrollment, observer rotation and retirement maintain this index. Its
//! cardinality is bounded by the existing registration pool; no callback scans
//! that pool, adds an owner, or receives a credential. A selector is a candidate,
//! never authority: native parent/ancestry and actor-generation fences still run.
//! Multiple live producers with the same UID/harness/session remain ambiguous
//! rather than choosing by focus, pane environment, PID claims or first arrival.

use super::*;
use std::collections::BTreeMap;

/// Existing live ordinary binding digests indexed by nonsecret selector tuple.
#[derive(Debug, Default)]
pub(in crate::runtime::control) struct HelperTargets {
    entries: BTreeMap<(u32, String, String), BTreeSet<[u8; 32]>>,
}

impl HelperTargets {
    /// Adds only one successfully registered ordinary owner. Registry admission
    /// supplies the finite bound; identical insertions consume no extra entry.
    pub(in crate::runtime::control) fn insert(
        &mut self,
        uid: u32,
        harness: &str,
        session: &str,
        digest: [u8; 32],
    ) {
        self.entries
            .entry((uid, harness.to_owned(), session.to_owned()))
            .or_default()
            .insert(digest);
    }

    /// Removes one exact retired/replaced target, preserving unrelated owners
    /// and removing empty keys so bounded tombstones cannot retain index slots.
    pub(in crate::runtime::control) fn remove(
        &mut self,
        uid: u32,
        harness: &str,
        session: &str,
        digest: [u8; 32],
    ) {
        let key = (uid, harness.to_owned(), session.to_owned());
        if let Some(targets) = self.entries.get_mut(&key) {
            targets.remove(&digest);
            if targets.is_empty() {
                self.entries.remove(&key);
            }
        }
    }

    /// Copies only this selector's bounded candidates for exact-target cleanup,
    /// never enumerating unrelated registrations or retaining more descriptors.
    pub(super) fn candidates(&self, uid: u32, harness: &str, session: &str) -> Vec<[u8; 32]> {
        self.entries
            .get(&(uid, harness.to_owned(), session.to_owned()))
            .map(|targets| targets.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Selects exactly one candidate without native I/O or a registration scan.
    /// Missing and colliding targets fail closed before reserving observation.
    pub(super) fn select(&self, uid: u32, harness: &str, session: &str) -> Result<[u8; 32]> {
        let targets = self
            .entries
            .get(&(uid, harness.to_owned(), session.to_owned()))
            .ok_or_else(|| MezError::forbidden("external helper target unavailable"))?;
        if targets.len() != 1 {
            return Err(MezError::conflict("external helper target ambiguous"));
        }
        targets
            .first()
            .copied()
            .ok_or_else(|| MezError::forbidden("external helper target unavailable"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exact lifecycle removal/replacement cannot clear another UID or session,
    /// retain empty slots or silently select one of two colliding live targets.
    #[test]
    fn helper_targets_exact_cleanup_preserves_unrelated_keys() {
        let mut index = HelperTargets::default();
        index.insert(1, "pi", "session", [1; 32]);
        index.insert(1, "pi", "session", [2; 32]);
        index.insert(2, "pi", "session", [3; 32]);
        index.insert(1, "pi", "other", [4; 32]);
        assert!(index.select(1, "pi", "session").is_err());
        assert_eq!(index.candidates(1, "pi", "session").len(), 2);
        index.remove(1, "pi", "session", [2; 32]);
        assert_eq!(index.select(1, "pi", "session").unwrap(), [1; 32]);
        index.remove(1, "pi", "session", [1; 32]);
        assert!(index.select(1, "pi", "session").is_err());
        assert!(index.candidates(1, "pi", "session").is_empty());
        assert_eq!(index.entries.len(), 2);
        assert_eq!(index.select(2, "pi", "session").unwrap(), [3; 32]);
        assert_eq!(index.select(1, "pi", "other").unwrap(), [4; 32]);
    }
}
