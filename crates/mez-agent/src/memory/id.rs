//! Canonical UUID identities for memory records.
//!
//! New independent records use random version-four UUIDs. Records that need a
//! stable identity across retries, and legacy identifiers rewritten during
//! migration, use a deterministic RFC 9562 version-eight UUID derived from a
//! domain-separated SHA-256 digest.

use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Allocates a lowercase, hyphenated version-four UUID for a new memory.
pub fn new_memory_uuid() -> String {
    Uuid::new_v4().to_string()
}

/// Returns a canonical UUID for either a UUID or a legacy memory identifier.
///
/// Existing UUIDs are normalized to lowercase hyphenated form. Legacy values
/// are mapped deterministically so callers can continue addressing records
/// after the SQLite identity migration.
pub fn canonical_memory_uuid(value: &str) -> String {
    Uuid::parse_str(value).map_or_else(
        |_| deterministic_memory_uuid("legacy-memory-id", value),
        |uuid| uuid.to_string(),
    )
}

/// Derives a stable lowercase, hyphenated version-eight UUID.
///
/// `domain` separates independent identity sources, while `value` contains
/// the source identity that must remain idempotent across retries or migration.
pub fn deterministic_memory_uuid(domain: &str, value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update([0]);
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes).to_string()
}

/// Reports whether a value is a syntactically valid UUID.
pub fn is_memory_uuid(value: &str) -> bool {
    Uuid::parse_str(value).is_ok()
}

#[cfg(test)]
mod tests {
    use super::{
        canonical_memory_uuid, deterministic_memory_uuid, is_memory_uuid, new_memory_uuid,
    };

    /// Verifies random and deterministic memory identities are real UUIDs.
    #[test]
    fn memory_identifiers_are_uuids() {
        assert!(is_memory_uuid(&new_memory_uuid()));
        let first = deterministic_memory_uuid("test", "legacy-id");
        let second = deterministic_memory_uuid("test", "legacy-id");
        assert!(is_memory_uuid(&first));
        assert_eq!(first, second);
        assert_eq!(
            canonical_memory_uuid("legacy-id"),
            canonical_memory_uuid("legacy-id")
        );
        assert!(is_memory_uuid(&canonical_memory_uuid("legacy-id")));
    }
}
