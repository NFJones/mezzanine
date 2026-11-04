//! Actual transactional lifetime provenance, without inferred retirement.

use super::*;

/// Publishes the actual captured request through the original concrete port.
fn persist(
    store: &mut MmpCustodyStore,
    request: &MessageNativeContinuityRequest,
) -> NativeContinuityPlan {
    let write = request.write();
    let plan = store
        .native_continuity_plan(&write.snapshot().native.agent_id)
        .unwrap();
    write
        .persist(&mut store.native_continuity_port(&plan))
        .unwrap();
    plan
}

/// Reads under this exact store connection's coherent snapshot.
fn history(store: &MmpCustodyStore, key: &str) -> super::super::lifetime::StoredNativeLifetime {
    let transaction = store.connection.unchecked_transaction().unwrap();
    store
        .read_native_lifetime_in_snapshot(&transaction, key)
        .unwrap()
        .unwrap()
}

/// Same exact native publication is one generation/batch even after lost reply.
/// Its fixed record contains no identity capability vector or availability and
/// remains only descriptive evidence; the original lower request owns adoption.
#[test]
fn native_lifetime_guarded_publication_is_bounded_and_exactly_reconciled() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let (mut service, request) =
        capture_with_policy(vec!["private-capability".repeat(100)], 0, true);
    let plan = persist(&mut store, &request);
    let inventory = store.inventory_custody(limits()).unwrap();
    assert_eq!(inventory.native_lifetime_keys.len(), 1);
    let key = &inventory.native_lifetime_keys[0];
    let bytes = store.read(key).unwrap().unwrap();
    assert!(bytes.len() <= 4096);
    assert!(!String::from_utf8_lossy(&bytes).contains("private-capability"));
    let lifetime = history(&store, key);
    assert_eq!(lifetime.publication_generation, 1);
    assert!(!lifetime.native.admission_available);
    assert!(!lifetime.native.delivery_available);
    let generation = store.current_state().unwrap().generation;
    let verified = request
        .write()
        .persist(&mut store.native_continuity_port(&plan))
        .unwrap();
    assert_eq!(store.current_state().unwrap().generation, generation);
    assert_eq!(store.read(key).unwrap().unwrap(), bytes);
    assert!(request.finish(&mut service, verified).unwrap());
}

/// Actual replacement retains both bounded namespace/epoch tuples before latest
/// n1 changes. A copied old n1 is backfilled only because that guarded original
/// still exists, not because a missing older owner can be guessed from labels.
#[test]
fn native_lifetime_backfills_only_present_guarded_legacy_observation() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let (_, first) = capture_with_policy(vec![], 0, true);
    persist(&mut store, &first);
    let first_key = store
        .inventory_custody(limits())
        .unwrap()
        .native_lifetime_keys
        .remove(0);
    let before = store.read(&first_key).unwrap().unwrap();
    let digest: [u8; 32] = Sha256::digest(&before).into();
    store
        .commit(
            store.current_state().unwrap().generation as u64,
            &[CustodyWrite {
                key: &first_key,
                previous: Some(&digest),
                value: None,
            }],
        )
        .unwrap();
    let (_, second) = capture_with_policy(vec![], 0, true);
    persist(&mut store, &second);
    let inventory = store.inventory_custody(limits()).unwrap();
    assert_eq!(inventory.native_lifetime_keys.len(), 2);
    let first_again = history(&store, &first_key);
    assert_eq!(first_again.publication_generation, 1);
    let latest = store
        .read_native_continuity(&second.write().snapshot().native.agent_id)
        .unwrap()
        .unwrap();
    assert_eq!(latest.snapshot, *second.write().snapshot());
    assert_ne!(latest.snapshot.native.owner, first_again.native.owner);
}

/// Valid checksums cannot change tuple, first publication or availability. Both
/// direct reads and inventory refuse malformed history, and the original writer
/// must refuse before overwriting n1 or repairing hostile bytes into new facts.
#[test]
fn native_lifetime_corruption_refuses_inventory_and_native_overwrite() {
    for fault in [
        "version",
        "allocation",
        "future",
        "zero",
        "agent",
        "owner",
        "namespace",
        "epoch",
        "floor",
        "available",
        "unknown",
    ] {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let (_, request) = capture_with_policy(vec![], 0, true);
        persist(&mut store, &request);
        let key = store
            .inventory_custody(limits())
            .unwrap()
            .native_lifetime_keys
            .remove(0);
        let old = store.read(&key).unwrap().unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&old).unwrap();
        match fault {
            "version" => value["format_version"] = 2.into(),
            "allocation" => {
                value["store_id"][0] = (value["store_id"][0].as_u64().unwrap() ^ 1).into()
            }
            "future" => value["publication_generation"] = 2.into(),
            "zero" => value["publication_generation"] = 0.into(),
            "agent" => value["native"]["agent_id"] = "different-owner".into(),
            "owner" => value["native"]["owner"]["conversation_id"] = "other".into(),
            "namespace" => {
                value["native"]["registration_namespace"][0] =
                    (value["native"]["registration_namespace"][0]
                        .as_u64()
                        .unwrap()
                        ^ 1)
                    .into()
            }
            "epoch" => {
                value["native"]["shell_incarnation"][0] =
                    (value["native"]["shell_incarnation"][0].as_u64().unwrap() ^ 1).into()
            }
            "floor" => value["registered_after_sequence"] = 1.into(),
            "available" => value["native"]["delivery_available"] = true.into(),
            "unknown" => value["retired"] = true.into(),
            _ => unreachable!(),
        }
        let bytes = serde_json::to_vec(&value).unwrap();
        store
            .connection
            .execute(
                "UPDATE records SET value=?1, digest=?2 WHERE key=?3",
                rusqlite::params![&bytes, &Sha256::digest(&bytes)[..], &key],
            )
            .unwrap();
        assert!(store.inventory_custody(limits()).is_err(), "{fault}");
        assert!(
            store
                .native_continuity_plan(&request.write().snapshot().native.agent_id)
                .is_err(),
            "{fault}"
        );
        assert_eq!(store.read(&key).unwrap().unwrap(), bytes);
    }
}

/// History must come from this connection's actual snapshot. A second handle to
/// the same physical store cannot supply an unrelated SELECT boundary.
#[test]
fn native_lifetime_reader_requires_actual_snapshot() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let (_, request) = capture_with_policy(vec![], 0, true);
    persist(&mut store, &request);
    let key = store
        .inventory_custody(limits())
        .unwrap()
        .native_lifetime_keys
        .remove(0);
    let other = fixture.open();
    let transaction = other.connection.unchecked_transaction().unwrap();
    assert!(
        store
            .read_native_lifetime_in_snapshot(&transaction, &key)
            .is_err()
    );
}

/// A native owner never published under the guard cannot acquire historical
/// qualification merely by being the previous n1 when a new owner arrives.
#[test]
fn native_lifetime_does_not_upgrade_unguarded_native_history() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let (_, unguarded) = capture(vec![]);
    persist(&mut store, &unguarded);
    assert!(
        store
            .inventory_custody(limits())
            .unwrap()
            .native_lifetime_keys
            .is_empty()
    );
    let (_, guarded) = capture_with_policy(vec![], 0, true);
    persist(&mut store, &guarded);
    let inventory = store.inventory_custody(limits()).unwrap();
    assert_eq!(inventory.native_lifetime_keys.len(), 1);
    let kept = history(&store, &inventory.native_lifetime_keys[0]);
    assert_eq!(kept.native.owner, guarded.write().snapshot().native.owner);
    assert_ne!(kept.native.owner, unguarded.write().snapshot().native.owner);
}

/// Already overwritten legacy observations are unavailable evidence. A real
/// old-style n1 replacement with no retained history must not make the next
/// publisher fabricate an even earlier namespace, despite equal agent labels.
#[test]
fn native_lifetime_missing_overwritten_legacy_epoch_is_not_invented() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let (_, first) = capture_with_policy(vec![], 0, true);
    persist(&mut store, &first);
    let first_key = store
        .inventory_custody(limits())
        .unwrap()
        .native_lifetime_keys
        .remove(0);
    let first_bytes = store.read(&first_key).unwrap().unwrap();
    let first_digest: [u8; 32] = Sha256::digest(&first_bytes).into();
    store
        .commit(
            1,
            &[CustodyWrite {
                key: &first_key,
                previous: Some(&first_digest),
                value: None,
            }],
        )
        .unwrap();
    let (_, legacy) = capture_with_policy(vec![], 0, true);
    let key = record_key(&legacy.write().snapshot().native.agent_id);
    let previous = store.read(&key).unwrap().unwrap();
    let previous_digest: [u8; 32] = Sha256::digest(&previous).into();
    let bytes = serde_json::to_vec(&RecordRef {
        format_version: 2,
        store_id: &store.store_id,
        generation: 3,
        snapshot: legacy.write().snapshot(),
    })
    .unwrap();
    store
        .commit(
            2,
            &[CustodyWrite {
                key: &key,
                previous: Some(&previous_digest),
                value: Some(&bytes),
            }],
        )
        .unwrap();
    let (_, next) = capture_with_policy(vec![], 0, true);
    persist(&mut store, &next);
    let inventory = store.inventory_custody(limits()).unwrap();
    assert_eq!(inventory.native_lifetime_keys.len(), 2);
    assert!(store.read(&first_key).unwrap().is_none());
    assert!(
        inventory
            .native_lifetime_keys
            .iter()
            .map(|key| history(&store, key))
            .all(|h| h.native.owner != first.write().snapshot().native.owner)
    );
}

/// A real SQL effect during a new history insert must not alter an unrelated
/// prior lifetime, even with a freshly computed checksum. The entire n1/history
/// batch rolls back and its frozen original plan can retry after repair.
#[test]
fn native_lifetime_insert_trigger_preserves_unrelated_original_history() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let (_, first) = capture_with_policy(vec![], 0, true);
    persist(&mut store, &first);
    let protected_key = store
        .inventory_custody(limits())
        .unwrap()
        .native_lifetime_keys
        .remove(0);
    let protected = store.read(&protected_key).unwrap().unwrap();
    let mut tampered: serde_json::Value = serde_json::from_slice(&protected).unwrap();
    tampered["publication_generation"] = 0.into();
    let tampered = serde_json::to_vec(&tampered).unwrap();
    let (_, next) = capture_with_policy(vec![], 1, true);
    let write = next.write();
    let target_key = format!("b1:{}", write.snapshot().native.owner.incarnation());
    store.connection.execute_batch(&format!("CREATE TRIGGER corrupt_lifetime AFTER INSERT ON records WHEN NEW.key='{target_key}' BEGIN UPDATE records SET value=X'{}',digest=X'{}' WHERE key='{protected_key}'; END;", hex(&tampered), hex(&Sha256::digest(&tampered)))).unwrap();
    let plan = store
        .native_continuity_plan(&write.snapshot().native.agent_id)
        .unwrap();
    let error = write
        .persist(&mut store.native_continuity_port(&plan))
        .err()
        .unwrap();
    assert!(
        error
            .to_string()
            .contains("lifetime write changed unrelated rows"),
        "{error}"
    );
    assert_eq!(store.current_state().unwrap().generation, 1);
    assert_eq!(store.read(&protected_key).unwrap().unwrap(), protected);
    assert!(store.read(&target_key).unwrap().is_none());
    store
        .connection
        .execute_batch("DROP TRIGGER corrupt_lifetime")
        .unwrap();
    next.write()
        .persist(&mut store.native_continuity_port(&plan))
        .unwrap();
    assert_eq!(store.current_state().unwrap().generation, 2);
    assert_eq!(store.read(&protected_key).unwrap().unwrap(), protected);
    assert_eq!(
        store
            .inventory_custody(limits())
            .unwrap()
            .native_lifetime_keys
            .len(),
        2
    );
}

/// Own permitted target keys are not permission to change their captured bytes.
/// Rechecksummed mutation of n1 availability or a new b1 record must roll back
/// before any positive, and the unchanged first plan must still recover once.
#[test]
fn native_lifetime_own_target_trigger_cannot_rewrite_captured_publication() {
    for target in ["n1", "b1"] {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let (_, request) = capture_with_policy(vec![], 0, true);
        let write = request.write();
        let (key, mut value) = if target == "n1" {
            (
                record_key(&write.snapshot().native.agent_id),
                serde_json::to_value(RecordRef {
                    format_version: 2,
                    store_id: &store.store_id,
                    generation: 1,
                    snapshot: write.snapshot(),
                })
                .unwrap(),
            )
        } else {
            let mut native = write.snapshot().native.clone();
            native.admission_available = false;
            native.delivery_available = false;
            (
                format!("b1:{}", native.owner.incarnation()),
                serde_json::json!({"format_version":1,"store_id":store.store_id,"publication_generation":1,"registered_after_sequence":0,"native":native}),
            )
        };
        if target == "n1" {
            value["snapshot"]["native"]["delivery_available"] = false.into();
        } else {
            value["native"]["delivery_available"] = true.into();
        }
        let bytes = serde_json::to_vec(&value).unwrap();
        store.connection.execute_batch(&format!("CREATE TRIGGER corrupt_own AFTER INSERT ON records WHEN NEW.key='{key}' BEGIN UPDATE records SET value=X'{}',digest=X'{}' WHERE key='{key}'; END;", hex(&bytes), hex(&Sha256::digest(&bytes)))).unwrap();
        let plan = store
            .native_continuity_plan(&write.snapshot().native.agent_id)
            .unwrap();
        let error = write
            .persist(&mut store.native_continuity_port(&plan))
            .err()
            .unwrap();
        assert!(
            error
                .to_string()
                .contains("MMP custody committed postimage changed"),
            "{target}: {error}"
        );
        assert_eq!(store.current_state().unwrap().generation, 0);
        assert!(
            store
                .inventory_custody(limits())
                .unwrap()
                .native_lifetime_keys
                .is_empty()
        );
        store
            .connection
            .execute_batch("DROP TRIGGER corrupt_own")
            .unwrap();
        request
            .write()
            .persist(&mut store.native_continuity_port(&plan))
            .unwrap();
        assert_eq!(store.current_state().unwrap().generation, 1);
        assert_eq!(
            store
                .inventory_custody(limits())
                .unwrap()
                .native_lifetime_keys
                .len(),
            1
        );
    }
}

/// SQL fixture literals contain only hex bytes from generated record images.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
