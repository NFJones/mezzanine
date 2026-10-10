//! Durable server-owned accounting namespaces for exact native source lifetimes.
//!
//! Native admission supplies boot, UID, root and producer incarnation evidence;
//! a vendor session is only a selector within that already-qualified lifetime.
//! Storage retains a digest and random owner, never paths, raw IDs, capabilities
//! or transcripts. Mapping possession grants no admission or reporting rights.
//! Existing stream receipts/checkpoints remain untouched. No cross-process
//! resume/fork continuity or ordinary usage activation is established here.

use super::{MezError, Result, TokenUsageStore};
use rusqlite::{OptionalExtension, params};
use sha2::{Digest, Sha256};

/// Durable mappings cannot be silently evicted and later become fresh expense.
const MAX_SOURCES: usize = 65_536;

/// Evidence kinds stay disjoint even for equal native metadata.
#[derive(Debug, Clone, Copy)]
pub(crate) enum NativeSourceKind {
    /// A persistent, kernel-qualified socket producer.
    Socket,
    /// A separately verified native creator of a curated command helper.
    CuratedCreator,
}

/// Content-free native facts supplied only by the admission worker. Construction
/// does not attest them: the actor must revalidate original held lifetime/root
/// evidence before adopting any mapped namespace.
pub(crate) struct NativeSourceWitness<'a> {
    /// Native boot-session UUID, not daemon startup time or client metadata.
    pub boot: &'a str,
    /// Authenticated native source UID.
    pub uid: u32,
    /// Exact local pane-root PID and kernel start token.
    pub root: (u32, u64),
    /// Exact native producer PID, parent PID and kernel start token.
    pub producer: (u32, u32, u64),
    /// Declared, independently qualified evidence kind.
    pub kind: NativeSourceKind,
    /// Vendor main-session selector within the qualified producer lifetime.
    pub session: &'a str,
}

/// Opaque digest of a canonical native witness plus its server-owned harness.
/// No RPC exposes a constructor or accepts this key as authority.
#[derive(Debug, Clone)]
pub(crate) struct ExternalSourceKey {
    digest: String,
    harness: String,
}

impl ExternalSourceKey {
    /// Hashes bounded admission-owned facts with explicit domain separation.
    /// Invalid boot/session/process metadata fails before any database I/O.
    pub(crate) fn from_native(harness: &str, witness: NativeSourceWitness<'_>) -> Result<Self> {
        crate::integrations::harness_policy::require_active_external_harness(harness)?;
        if !valid_uuid(witness.boot)
            || witness.root.0 == 0
            || witness.producer.0 == 0
            || witness.session.is_empty()
            || witness.session.len() > 128
            || witness.session.chars().any(char::is_control)
        {
            return Err(MezError::invalid_args(
                "external native source witness unavailable",
            ));
        }
        let kind = match witness.kind {
            NativeSourceKind::Socket => "socket",
            NativeSourceKind::CuratedCreator => "curated-creator",
        };
        let encoded = serde_json::to_vec(&(
            "mez-native-usage-source/1",
            harness,
            witness.boot.to_ascii_lowercase(),
            witness.uid,
            witness.root,
            witness.producer,
            kind,
            witness.session,
        ))
        .map_err(|_| MezError::invalid_state("external native source encoding unavailable"))?;
        Ok(Self {
            digest: Sha256::digest(encoded)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
            harness: harness.into(),
        })
    }
}

/// Validates only canonical UUID shape, without accepting arbitrary inert text.
fn valid_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

impl TokenUsageStore {
    /// Resolves one server-issued owner under bounded immediate writer admission.
    /// Reopen/concurrent callers recover the same owner; capacity exhaustion is
    /// explicit and never prunes replay authority. Call only off the actor.
    pub(crate) fn external_source_owner(&self, key: &ExternalSourceKey) -> Result<String> {
        self.external_source_owner_with_limit(key, MAX_SOURCES)
    }

    /// Shared transaction owner; the test-only small bound exercises exhaustion
    /// without allocating the production mapping capacity.
    fn external_source_owner_with_limit(
        &self,
        key: &ExternalSourceKey,
        limit: usize,
    ) -> Result<String> {
        let mut connection = self.open()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let existing: Option<(String, String)> = transaction
            .query_row(
                "SELECT harness,owner FROM external_usage_sources WHERE witness=?1",
                [&key.digest],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((harness, owner)) = existing {
            if harness != key.harness || !valid_uuid(&owner) {
                return Err(MezError::invalid_state(
                    "stored external source identity is invalid",
                ));
            }
            return Ok(owner);
        }
        let count: i64 =
            transaction.query_row("SELECT count(*) FROM external_usage_sources", [], |row| {
                row.get(0)
            })?;
        if usize::try_from(count).map_or(true, |count| count >= limit) {
            return Err(MezError::new(
                crate::error::MezErrorKind::RateLimited,
                "durable external source capacity unavailable",
            ));
        }
        let owner = super::new_token_usage_event_id();
        transaction.execute(
            "INSERT INTO external_usage_sources(witness,harness,owner) VALUES(?1,?2,?3)",
            params![key.digest, key.harness, owner],
        )?;
        transaction.commit()?;
        Ok(owner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pure synthetic witness for storage tests, not proof of native admission.
    fn key(session: &str) -> ExternalSourceKey {
        ExternalSourceKey::from_native(
            "pi",
            NativeSourceWitness {
                boot: "01234567-89ab-cdef-0123-456789abcdef",
                uid: 42,
                root: (100, 2),
                producer: (101, 100, 3),
                kind: NativeSourceKind::Socket,
                session,
            },
        )
        .unwrap()
    }

    /// Isolates SQLite schema/mapping tests from real user accounting/config.
    fn store() -> TokenUsageStore {
        TokenUsageStore::new(
            std::env::temp_dir()
                .join(format!(
                    "mez-source-{}",
                    super::super::new_token_usage_event_id()
                ))
                .join("usage.sqlite"),
        )
    }

    /// Reopened and concurrent mapping requests preserve one owner without
    /// persisting native IDs/session text or creating any usage delta.
    #[test]
    fn external_source_owner_reopen_and_concurrency_preserve_namespace() {
        let store = store();
        let key = key("private-session-selector");
        let barrier = std::sync::Barrier::new(4);
        let owners = std::thread::scope(|scope| {
            let tasks = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        store.external_source_owner(&key).unwrap()
                    })
                })
                .collect::<Vec<_>>();
            tasks
                .into_iter()
                .map(|task| task.join().unwrap())
                .collect::<Vec<_>>()
        });
        let first = owners[0].clone();
        assert!(owners.iter().all(|owner| owner == &first));
        let reopened = TokenUsageStore::new(store.path());
        assert_eq!(reopened.external_source_owner(&key).unwrap(), first);
        let connection = reopened.open().unwrap();
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM external_usage_sources", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM token_usage_events", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        let persisted: String = connection
            .query_row("SELECT witness FROM external_usage_sources", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(persisted.len(), 64);
        assert!(!persisted.contains("private-session-selector"));
        drop(connection);
        std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
    }

    /// Capacity exhaustion preserves all existing source namespaces and reports
    /// unavailable for a new witness rather than evicting dedup authority.
    #[test]
    fn external_source_owner_capacity_keeps_existing_mappings() {
        let store = store();
        let first = store
            .external_source_owner_with_limit(&key("one"), 1)
            .unwrap();
        assert!(
            store
                .external_source_owner_with_limit(&key("two"), 1)
                .is_err()
        );
        assert_eq!(
            store
                .external_source_owner_with_limit(&key("one"), 1)
                .unwrap(),
            first
        );
        std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
    }

    /// Boot/root/producer/kind/session are separate namespace boundaries. No
    /// daemon generation, observer epoch or current project enters the witness.
    #[test]
    fn external_source_native_witness_fields_are_domain_separated() {
        let original = key("one");
        for field in 0..7 {
            let mut witness = NativeSourceWitness {
                boot: "01234567-89ab-cdef-0123-456789abcdef",
                uid: 42,
                root: (100, 2),
                producer: (101, 100, 3),
                kind: NativeSourceKind::Socket,
                session: "one",
            };
            match field {
                0 => witness.boot = "fedcba98-7654-3210-fedc-ba9876543210",
                1 => witness.uid += 1,
                2 => witness.root.1 += 1,
                3 => witness.producer.0 += 1,
                4 => witness.producer.2 += 1,
                5 => witness.kind = NativeSourceKind::CuratedCreator,
                _ => witness.session = "two",
            }
            assert_ne!(
                ExternalSourceKey::from_native("pi", witness)
                    .unwrap()
                    .digest,
                original.digest
            );
        }
    }

    /// Version-4 migration adds only source mapping state, conserving existing
    /// event/receipt/checkpoint bytes and replay behavior. A failed migration
    /// keeps version4 and existing expense intact instead of partial adoption.
    #[test]
    fn external_source_schema_migration_conserves_ledger_and_rolls_back_failure() {
        for fail in [false, true] {
            let store = store();
            let report = super::super::ExternalUsageReport {
                owner: "old-owner".into(),
                project: None,
                harness: "pi".into(),
                epoch: "old-epoch".into(),
                event_id: "old-report".into(),
                sequence: 1,
                mode: "delta".into(),
                baseline: false,
                observed_at: 100,
                model: mez_agent::ModelTokenUsageKey::new("fixture", "model"),
                counters: super::super::ExternalCounters {
                    input_tokens: 7,
                    ..Default::default()
                },
            };
            store.ingest_external(&report, 100).unwrap();
            let connection = store.open().unwrap();
            connection
                .execute_batch("DROP TABLE external_usage_sources; PRAGMA user_version=4;")
                .unwrap();
            if fail {
                connection
                    .execute_batch("CREATE TABLE external_usage_sources(collision TEXT);")
                    .unwrap();
            }
            let result = store.external_source_owner(&key("one"));
            assert_eq!(result.is_err(), fail);
            assert_eq!(
                connection
                    .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                if fail { 4 } else { 5 }
            );
            assert_eq!(
                connection
                    .query_row(
                        "SELECT sum(input_tokens) FROM token_usage_events",
                        [],
                        |row| row.get::<_, i64>(0)
                    )
                    .unwrap(),
                7
            );
            assert_eq!(
                connection
                    .query_row("SELECT count(*) FROM external_usage_receipts", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .unwrap(),
                1
            );
            if !fail {
                assert!(!store.ingest_external(&report, 100).unwrap().applied);
            }
            drop(connection);
            std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
        }
    }

    /// Unknown future schema and malformed native witnesses cannot allocate a
    /// namespace or change existing data. This is storage format validation,
    /// not evidence that untrusted caller-supplied native fields are authority.
    #[test]
    fn external_source_future_schema_and_bad_native_witness_fail_closed() {
        let store = store();
        store.initialize(100).unwrap();
        let connection = store.open().unwrap();
        connection.execute_batch("PRAGMA user_version=6;").unwrap();
        assert!(store.external_source_owner(&key("one")).is_err());
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM external_usage_sources", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        for boot in ["", "daemon-1", "01234567-89ab-cdef-0123-456789abcdef\n"] {
            assert!(
                ExternalSourceKey::from_native(
                    "pi",
                    NativeSourceWitness {
                        boot,
                        uid: 42,
                        root: (100, 2),
                        producer: (101, 100, 3),
                        kind: NativeSourceKind::Socket,
                        session: "one"
                    }
                )
                .is_err()
            );
        }
        drop(connection);
        std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
    }
}
