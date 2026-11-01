//! Real private-store transaction tests, independent of provider/network state.

use super::*;
use sha2::{Digest, Sha256};
use std::os::unix::fs::PermissionsExt;

/// Removes each generated private store even when a regression assertion fails.
struct Fixture(std::path::PathBuf);
impl Fixture {
    /// Allocates a unique, initially absent store directory.
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "mez-mmp-custody-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        )))
    }
}
impl Drop for Fixture {
    /// Cleans up only this fixture's generated store directory.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A commit that loses its actor reply must be reconciled after an actual
/// connection restart using the same generation and complete immutable batch.
/// Changed content cannot masquerade as the successful original attempt.
#[test]
fn committed_original_survives_restart_and_lost_reply_without_second_write() {
    let fixture = Fixture::new();
    let writes = [CustodyWrite {
        key: "occurrence-1",
        previous: None,
        value: Some(b"original payload and frozen recipients"),
    }];
    let mut store = MmpCustodyStore::open(&fixture.0, "session-original").unwrap();
    let committed = store.commit(0, &writes).unwrap();
    drop(store);
    let mut recovered = MmpCustodyStore::open(&fixture.0, "session-original").unwrap();
    assert_eq!(
        recovered.read("occurrence-1").unwrap().as_deref(),
        writes[0].value
    );
    assert_eq!(recovered.commit(0, &writes).unwrap(), committed);
    assert!(
        recovered
            .commit(
                0,
                &[CustodyWrite {
                    key: "occurrence-1",
                    previous: None,
                    value: Some(b"changed")
                }]
            )
            .is_err()
    );
    assert!(MmpCustodyStore::open(&fixture.0, "new-pane-same-address").is_err());
}

/// Failure of a later expected preimage must roll back every earlier mutation
/// in the same batch. A stale actor generation cannot delete accepted evidence.
#[test]
fn stale_generation_and_conflicting_later_row_preserve_whole_original_batch() {
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    store
        .commit(
            0,
            &[CustodyWrite {
                key: "one",
                previous: None,
                value: Some(b"original"),
            }],
        )
        .unwrap();
    assert!(
        store
            .commit(
                1,
                &[
                    CustodyWrite {
                        key: "two",
                        previous: None,
                        value: Some(b"must roll back")
                    },
                    CustodyWrite {
                        key: "one",
                        previous: None,
                        value: Some(b"conflict")
                    },
                ]
            )
            .is_err()
    );
    assert_eq!(store.read("two").unwrap(), None);
    assert_eq!(
        store.read("one").unwrap().as_deref(),
        Some(b"original".as_slice())
    );
    let digest = Sha256::digest(b"original");
    assert!(
        store
            .commit(
                0,
                &[CustodyWrite {
                    key: "one",
                    previous: Some(&digest),
                    value: None
                }]
            )
            .is_err()
    );
    store
        .commit(
            1,
            &[CustodyWrite {
                key: "one",
                previous: Some(&digest),
                value: None,
            }],
        )
        .unwrap();
    assert_eq!(store.read("one").unwrap(), None);
}

/// Corrupted canonical bytes must fail closed on read rather than presenting an
/// empty mailbox or returning payload whose stored acceptance digest disagrees.
#[test]
fn corrupted_payload_and_future_store_schema_are_not_empty_recovery() {
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    store
        .commit(
            0,
            &[CustodyWrite {
                key: "one",
                previous: None,
                value: Some(b"original"),
            }],
        )
        .unwrap();
    drop(store);
    let raw = rusqlite::Connection::open(fixture.0.join("custody.sqlite")).unwrap();
    raw.execute("UPDATE records SET value = ?1", [b"corrupt".as_slice()])
        .unwrap();
    drop(raw);
    let mut recovered = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    assert!(recovered.read("one").is_err());
    let original_digest = Sha256::digest(b"original");
    assert!(
        recovered
            .commit(
                1,
                &[CustodyWrite {
                    key: "one",
                    previous: Some(&original_digest),
                    value: None
                }]
            )
            .is_err()
    );
    drop(recovered);
    let raw = rusqlite::Connection::open(fixture.0.join("custody.sqlite")).unwrap();
    raw.execute_batch("PRAGMA user_version = 999").unwrap();
    drop(raw);
    assert!(MmpCustodyStore::open(&fixture.0, "session").is_err());
}

/// Narrowed logical and row capacity must reject a whole batch while retaining
/// accepted originals. Settlement frees actual capacity for a later generation;
/// merely retrying the rejected batch cannot silently consume a generation.
#[test]
fn tiny_capacity_rolls_back_then_recovers_after_exact_settlement() {
    let fixture = Fixture::new();
    let limits = CustodyStoreLimits {
        records: 1,
        logical_bytes: 256,
        database_pages: 64,
    };
    let mut store = MmpCustodyStore::open_with_limits(&fixture.0, "session", limits).unwrap();
    store
        .commit(
            0,
            &[CustodyWrite {
                key: "one",
                previous: None,
                value: Some(b"original"),
            }],
        )
        .unwrap();
    let second = [CustodyWrite {
        key: "two",
        previous: None,
        value: Some(b"second"),
    }];
    assert!(store.commit(1, &second).is_err());
    assert_eq!(store.read("two").unwrap(), None);
    let digest = Sha256::digest(b"original");
    store
        .commit(
            1,
            &[CustodyWrite {
                key: "one",
                previous: Some(&digest),
                value: None,
            }],
        )
        .unwrap();
    store.commit(2, &second).unwrap();
    drop(store);
    let recovered = MmpCustodyStore::open_with_limits(&fixture.0, "session", limits).unwrap();
    assert_eq!(recovered.read("one").unwrap(), None);
    assert_eq!(
        recovered.read("two").unwrap().as_deref(),
        Some(b"second".as_slice())
    );
}

/// Oversized or duplicate-key batches must fail before the store advances, and
/// opening a symlinked database must never follow it or modify the target file.
#[test]
fn bounded_batch_and_private_path_failures_preserve_original_store() {
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let oversized = vec![b'x'; MAX_BATCH_BYTES];
    assert!(
        store
            .commit(
                0,
                &[CustodyWrite {
                    key: "large",
                    previous: None,
                    value: Some(&oversized)
                }]
            )
            .is_err()
    );
    assert!(
        store
            .commit(
                0,
                &[
                    CustodyWrite {
                        key: "same",
                        previous: None,
                        value: Some(b"a")
                    },
                    CustodyWrite {
                        key: "same",
                        previous: None,
                        value: Some(b"b")
                    },
                ]
            )
            .is_err()
    );
    store
        .commit(
            0,
            &[CustodyWrite {
                key: "original",
                previous: None,
                value: Some(b"retained"),
            }],
        )
        .unwrap();
    drop(store);
    let other = Fixture::new();
    std::fs::create_dir(&other.0).unwrap();
    std::fs::set_permissions(&other.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::os::unix::fs::symlink(
        fixture.0.join("custody.sqlite"),
        other.0.join("custody.sqlite"),
    )
    .unwrap();
    assert!(MmpCustodyStore::open(&other.0, "session").is_err());
    assert_eq!(
        MmpCustodyStore::open(&fixture.0, "session")
            .unwrap()
            .read("original")
            .unwrap()
            .as_deref(),
        Some(b"retained".as_slice())
    );
}

/// SQLite's own page exhaustion may occur before a logical budget fills. That
/// infrastructure failure must roll back the batch and leave the original
/// generation usable for a later smaller exact write, including after reopen.
#[test]
fn physical_page_exhaustion_keeps_original_and_allows_smaller_retry() {
    let fixture = Fixture::new();
    let limits = CustodyStoreLimits {
        records: 10,
        logical_bytes: 128 * 1024,
        database_pages: 8,
    };
    let mut store = MmpCustodyStore::open_with_limits(&fixture.0, "session", limits).unwrap();
    store
        .commit(
            0,
            &[CustodyWrite {
                key: "one",
                previous: None,
                value: Some(b"original"),
            }],
        )
        .unwrap();
    let large = vec![b'x'; 64 * 1024];
    assert!(
        store
            .commit(
                1,
                &[CustodyWrite {
                    key: "too-large",
                    previous: None,
                    value: Some(&large)
                }]
            )
            .is_err()
    );
    drop(store);
    let mut store = MmpCustodyStore::open_with_limits(&fixture.0, "session", limits).unwrap();
    assert_eq!(
        store.read("one").unwrap().as_deref(),
        Some(b"original".as_slice())
    );
    assert_eq!(store.read("too-large").unwrap(), None);
    store
        .commit(
            1,
            &[CustodyWrite {
                key: "small",
                previous: None,
                value: Some(b"fits"),
            }],
        )
        .unwrap();
}

/// A receipt belongs to the actual store allocation, even when a new store has
/// the same trusted session label and receives byte-identical batch contents.
/// It cannot be confused with the original store after directory replacement.
#[test]
fn identical_owner_text_does_not_recreate_original_store_receipt() {
    let original = Fixture::new();
    let replacement = Fixture::new();
    let writes = [CustodyWrite {
        key: "one",
        previous: None,
        value: Some(b"payload"),
    }];
    let first = MmpCustodyStore::open(&original.0, "session")
        .unwrap()
        .commit(0, &writes)
        .unwrap();
    let second = MmpCustodyStore::open(&replacement.0, "session")
        .unwrap()
        .commit(0, &writes)
        .unwrap();
    assert_eq!(first.generation, second.generation);
    assert_eq!(first.digest, second.digest);
    assert_ne!(first.store_id, second.store_id);
}

/// Independent connections cannot both admit different batches at the same
/// original generation. The second connection may reconcile the exact first
/// batch, then progress only from the generation that actually committed.
#[test]
fn two_connections_reconcile_original_then_reject_conflicting_stale_writer() {
    let fixture = Fixture::new();
    let mut first = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let mut second = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let batch = [CustodyWrite {
        key: "one",
        previous: None,
        value: Some(b"original"),
    }];
    let receipt = first.commit(0, &batch).unwrap();
    assert_eq!(second.commit(0, &batch).unwrap(), receipt);
    assert!(
        second
            .commit(
                0,
                &[CustodyWrite {
                    key: "two",
                    previous: None,
                    value: Some(b"stale")
                }]
            )
            .is_err()
    );
    assert_eq!(first.read("two").unwrap(), None);
    second
        .commit(
            receipt.generation(),
            &[CustodyWrite {
                key: "two",
                previous: None,
                value: Some(b"current"),
            }],
        )
        .unwrap();
    assert_eq!(
        first.read("two").unwrap().as_deref(),
        Some(b"current".as_slice())
    );
}

/// A saved batch label cannot establish current durable custody when its row
/// disappeared or its bytes changed after commit. Exact retry must fail closed,
/// retain the actor's unresolved original and never reconstruct the mutation.
#[test]
fn exact_retry_refuses_corrupted_or_missing_committed_postimage() {
    for corrupt in [true, false] {
        let fixture = Fixture::new();
        let batch = [CustodyWrite {
            key: "one",
            previous: None,
            value: Some(b"original"),
        }];
        MmpCustodyStore::open(&fixture.0, "session")
            .unwrap()
            .commit(0, &batch)
            .unwrap();
        let raw = rusqlite::Connection::open(fixture.0.join("custody.sqlite")).unwrap();
        if corrupt {
            raw.execute("UPDATE records SET value=?1", [b"corrupted".as_slice()])
                .unwrap();
        } else {
            raw.execute("DELETE FROM records", []).unwrap();
        }
        drop(raw);
        let mut recovered = MmpCustodyStore::open(&fixture.0, "session").unwrap();
        assert!(
            recovered.commit(0, &batch).is_err(),
            "saved receipt must not acknowledge corrupt={corrupt}"
        );
        if corrupt {
            assert!(recovered.read("one").is_err());
        } else {
            assert_eq!(recovered.read("one").unwrap(), None);
        }
    }
}

/// An exactly retried settlement receipt requires its original removal to
/// remain true. Reappearing data cannot be silently deleted again or certified
/// absent merely because the original deletion's batch digest was retained.
#[test]
fn exact_retry_refuses_resurrected_removed_postimage_without_deleting_again() {
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    store
        .commit(
            0,
            &[CustodyWrite {
                key: "one",
                previous: None,
                value: Some(b"original"),
            }],
        )
        .unwrap();
    let digest = Sha256::digest(b"original");
    let removal = [CustodyWrite {
        key: "one",
        previous: Some(&digest),
        value: None,
    }];
    store.commit(1, &removal).unwrap();
    drop(store);
    let raw = rusqlite::Connection::open(fixture.0.join("custody.sqlite")).unwrap();
    raw.execute(
        "INSERT INTO records VALUES(?1,?2,?3)",
        rusqlite::params!["one", b"original".as_slice(), digest.as_slice()],
    )
    .unwrap();
    drop(raw);
    let mut recovered = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    assert!(recovered.commit(1, &removal).is_err());
    assert_eq!(
        recovered.read("one").unwrap().as_deref(),
        Some(b"original".as_slice())
    );
}

/// A real disk commit may succeed before its worker panics and loses the reply.
/// The lower original must remain unpublished/uncancellable until an actual
/// join-qualified retry reconciles that same SQLite generation and record once.
/// This composes the storage/core boundary, not the unfinished product ingress.
#[test]
fn admission_worker_loss_after_real_commit_reconciles_same_disk_occurrence_once() {
    use mez_agent::messaging::{
        Envelope, MessageAcceptanceAttempt, MessageAcceptanceJoin, MessageAcceptanceStore,
        MessageAcceptanceStoreError, MessageAcceptanceWrite, MessageAcceptanceWriteOutcome,
        MessageError, MessageScope, MessageService, Recipient,
    };
    struct Port {
        root: std::path::PathBuf,
        lose_reply: bool,
    }
    impl MessageAcceptanceStore for Port {
        /// Commits the exact serialized original before the injected worker loss.
        fn commit(
            &mut self,
            record: &MessageAcceptanceWrite,
        ) -> std::result::Result<(), MessageAcceptanceStoreError> {
            let bytes = serde_json::to_vec(record).unwrap();
            let key = format!("occurrence-{}", record.sequence());
            let mut store = MmpCustodyStore::open(&self.root, "session").map_err(|error| {
                MessageAcceptanceStoreError::Unknown(MessageError::invalid_state(error.to_string()))
            })?;
            store
                .commit(
                    0,
                    &[CustodyWrite {
                        key: &key,
                        previous: None,
                        value: Some(&bytes),
                    }],
                )
                .map_err(|error| {
                    MessageAcceptanceStoreError::Unknown(MessageError::invalid_state(
                        error.to_string(),
                    ))
                })?;
            assert!(!self.lose_reply, "injected loss after actual commit");
            Ok(())
        }
    }
    struct Joined {
        expected: MessageAcceptanceAttempt,
        observed: bool,
    }
    impl MessageAcceptanceJoin for Joined {
        /// Qualifies only the exact original after the real thread join completed.
        fn qualify_joined(
            &mut self,
            attempt: &MessageAcceptanceAttempt,
        ) -> mez_agent::messaging::Result<()> {
            if self.observed && self.expected.same_attempt(attempt) {
                Ok(())
            } else {
                Err(MessageError::invalid_state(
                    "original worker has not joined",
                ))
            }
        }
    }
    let fixture = Fixture::new();
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    service
        .subscribe_from_retained_start(&target.agent_id)
        .unwrap();
    let input = Envelope {
        protocol: "mmp/1",
        id: "disk-original".into(),
        message_type: "send".into(),
        time: "fixture:disk".into(),
        sender: sender.clone(),
        recipient: Recipient::Agent(target.agent_id.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain".into(),
        payload: "committed before worker reply".into(),
        extension_fields: vec![],
    };
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            input,
            MessageScope::Session,
            17,
            4096,
            None,
        )
        .unwrap();
    let mut foreign = MessageService::from_snapshot_state(&service.snapshot_state()).unwrap();
    let write = service.begin_acceptance_write(&ticket).unwrap();
    let attempt = service.acceptance_write_attempt(&ticket).unwrap();
    let root = fixture.0.clone();
    let worker = std::thread::spawn(move || {
        write.persist(&mut Port {
            root,
            lose_reply: true,
        })
    });
    let mut joined = Joined {
        expected: attempt.clone(),
        observed: false,
    };
    assert!(
        service
            .recover_joined_acceptance_write(&attempt, &mut joined)
            .is_err()
    );
    assert!(worker.join().is_err());
    joined.observed = true;
    let store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let before = store
        .read(&format!("occurrence-{}", ticket.sequence()))
        .unwrap()
        .unwrap();
    drop(store);
    assert!(service.snapshot_state().accepted_messages.is_empty());
    assert!(
        service
            .receive_subscribed(&target.agent_id, 18, 10)
            .unwrap()
            .messages
            .is_empty()
    );
    assert!(service.cancel_acceptance(&ticket).is_err());
    service
        .update_agent_objective(
            &sender.agent_id,
            Some("changed while original reply was lost"),
            18,
        )
        .unwrap();
    let retry = service
        .recover_joined_acceptance_write(&attempt, &mut joined)
        .unwrap();
    let MessageAcceptanceWriteOutcome::Committed(committed) = retry.persist(&mut Port {
        root: fixture.0.clone(),
        lose_reply: false,
    }) else {
        panic!("exact reconciliation should succeed")
    };
    assert!(foreign.publish_acceptance(&committed).is_err());
    let receipt = service.publish_acceptance(&committed).unwrap().0;
    assert_eq!(receipt.sequence, ticket.sequence());
    assert_eq!(service.publish_acceptance(&committed).unwrap().0, receipt);
    assert_eq!(
        service
            .receive_subscribed(&target.agent_id, 19, 10)
            .unwrap()
            .messages
            .len(),
        1
    );
    let store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    assert_eq!(
        store
            .read(&format!("occurrence-{}", ticket.sequence()))
            .unwrap()
            .unwrap(),
        before
    );
    assert_eq!(
        store
            .connection
            .query_row("SELECT generation FROM state", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .connection
            .query_row("SELECT COUNT(*) FROM records", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    MessageService::from_snapshot_state(&service.snapshot_state()).unwrap();
}
