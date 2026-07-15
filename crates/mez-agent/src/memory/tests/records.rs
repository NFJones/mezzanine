//! Canonical record validation tests.

use super::record;
use crate::memory::MemoryScope;

/// Verifies persistent memory accepts user-managed sensitive content.
///
/// This regression scenario documents that canonical validation does not use
/// heuristic secret detection to reject user-controlled memory content.
#[test]
fn persistent_memory_accepts_sensitive_content_without_heuristic_rejection() {
    let record = record("m1", MemoryScope::Global, "api_key = sk-secret");

    record.validate_for_persistence().unwrap();
}
