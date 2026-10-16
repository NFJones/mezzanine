//! Fresh raw-owner retirement preserves canonical mail and snapshot provenance.
//!
//! These tests deliberately keep reusable pane floors separate from generated
//! raw identities: bounded disconnect cleanup must not weaken replay fences.

use super::*;

/// Creates fixed sender evidence shared by retention and receipt regressions.
fn mail(sender: &SenderIdentity, target: &AgentId, id: &str) -> Envelope {
    Envelope {
        protocol: MMP_PROTOCOL,
        id: id.to_string(),
        message_type: "send".to_string(),
        time: "message:test".to_string(),
        sender: sender.clone(),
        recipient: Recipient::Agent(target.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain; charset=utf-8".to_string(),
        payload: "retained unchanged".to_string(),
        extension_fields: Vec::new(),
    }
}

/// A disconnected raw sender must leave byte-equivalent queue/receipt evidence
/// for other recipients. Restore must reserve its retained numeric identity,
/// allocate a distinct hello identity and preserve exact receipt idempotency.
#[test]
fn fresh_retirement_preserves_receipts_and_restored_id_allocation() {
    let mut service = MessageService::default();
    let target = service.register_agent(None, None, "independent", Vec::new());
    let cursor = service.subscribe(&target.agent_id).unwrap();
    let raw = service
        .register_agent_with_objective(None, None, "raw", Vec::new(), Some("fixed evidence"))
        .unwrap();
    service.subscribe(&raw.agent_id).unwrap();
    let envelope = mail(&raw, &target.agent_id, "raw-authored");
    let delivery = service
        .accept_at_with_scope(&raw.agent_id, envelope.clone(), MessageScope::Session, 10)
        .unwrap();
    let before = service.snapshot_state();
    assert!(
        service
            .retire_fresh_observational_identity(&raw.agent_id)
            .unwrap()
    );
    assert!(
        !service
            .retire_fresh_observational_identity(&raw.agent_id)
            .unwrap()
    );
    let after = service.snapshot_state();
    assert_eq!(after.retained_messages, before.retained_messages);
    assert_eq!(after.accepted_messages, before.accepted_messages);
    assert_eq!(service.subscription(&target.agent_id), Some(&cursor));
    assert_eq!(after.retired_delivery_floors.len(), 1);
    assert_eq!(
        after.retired_delivery_floors[0].identity.agent_id,
        raw.agent_id.as_str()
    );
    assert_eq!(
        after.retired_delivery_floors[0].last_sequence,
        delivery.sequence
    );
    let mut restored = MessageService::from_snapshot_state(&after).unwrap();
    let next = restored.register_agent(None, None, "replacement", Vec::new());
    assert_ne!(
        next.agent_id, raw.agent_id,
        "restore reused retained sender provenance"
    );
    MessageService::from_snapshot_state(&restored.snapshot_state()).unwrap();
    // Explicit service-level rebind is only a receipt control, not a raw socket
    // reconnect operation. Its reusable floor is consumed by subscription.
    restored.ensure_agent_identity(raw.clone(), 11).unwrap();
    restored
        .subscribe_from_retained_start(&raw.agent_id)
        .unwrap();
    assert_eq!(
        restored
            .accept_at_with_scope(&raw.agent_id, envelope, MessageScope::Session, 12)
            .unwrap(),
        delivery
    );
    assert_eq!(
        restored.snapshot_state().accepted_messages,
        before.accepted_messages
    );
}

/// Fresh sender provenance ends when its final authored envelope evicts, even
/// while unrelated retained mail pins a later reusable pane delivery interval.
/// This prevents stationary mail from pinning disconnect churn indefinitely.
#[test]
fn fresh_authored_floor_evicts_without_weakening_reusable_pane_floor() {
    let mut service = MessageService::with_limits(2, 4096);
    let target = service.register_agent(None, None, "target", Vec::new());
    let raw = service.register_agent(None, None, "raw", Vec::new());
    let pane = service.register_agent(Some(PaneId::new('%', 1)), None, "pane", Vec::new());
    service.subscribe(&pane.agent_id).unwrap();
    let first = service
        .accept_at_with_scope(
            &raw.agent_id,
            mail(&raw, &target.agent_id, "raw-first"),
            MessageScope::Session,
            10,
        )
        .unwrap();
    let second = service
        .accept_at_with_scope(
            &target.agent_id,
            mail(&target, &target.agent_id, "unrelated-second"),
            MessageScope::Session,
            11,
        )
        .unwrap();
    assert!(service.retire_agent_identity(&pane.agent_id));
    assert!(
        service
            .retire_fresh_observational_identity(&raw.agent_id)
            .unwrap()
    );
    assert_eq!(
        service.retired_delivery_floors[&raw.agent_id].last_sequence,
        first.sequence
    );
    assert_eq!(
        service.retired_delivery_floors[&pane.agent_id].last_sequence,
        second.sequence
    );
    service
        .accept_at_with_scope(
            &target.agent_id,
            mail(&target, &target.agent_id, "unrelated-third"),
            MessageScope::Session,
            12,
        )
        .unwrap();
    assert!(!service.retired_delivery_floors.contains_key(&raw.agent_id));
    assert_eq!(
        service.retired_delivery_floors[&pane.agent_id].last_sequence,
        second.sequence
    );
    let after = service.snapshot_state();
    assert_eq!(after.retained_messages.len(), 2);
    assert!(
        after
            .retained_messages
            .iter()
            .any(|retained| retained.envelope.id == "unrelated-second")
    );
    assert!(
        !after
            .accepted_messages
            .iter()
            .any(|accepted| accepted.envelope.id == "raw-first")
    );
    MessageService::from_snapshot_state(&after).unwrap();
}

/// A changed unsent objective must not corrupt the old sent sender evidence.
/// This independently qualifies single-version drift before cleanup changes.
#[test]
fn fresh_retirement_preserves_single_sent_objective_version() {
    assert_fresh_objective_versions(false);
}

/// Two sent objective versions and a later unsent objective must all retain
/// their proper roles: exact historical evidence versus current telemetry.
/// This independently qualifies multi-version drift rather than skipping it
/// when the single-version regression fails first.
#[test]
fn fresh_retirement_preserves_multiple_sent_objective_versions() {
    assert_fresh_objective_versions(true);
}

/// Retires genuine authored objective evidence and checks exact queue, receipt
/// and snapshot preservation without rewriting any historical sender value.
fn assert_fresh_objective_versions(send_changed: bool) {
    let mut service = MessageService::default();
    let target = service.register_agent(None, None, "independent", Vec::new());
    let raw = service
        .register_agent_with_objective(None, None, "raw", Vec::new(), Some("objective A"))
        .unwrap();
    service.subscribe(&raw.agent_id).unwrap();
    service
        .accept_at_with_scope(
            &raw.agent_id,
            mail(&raw, &target.agent_id, "objective-A-mail"),
            MessageScope::Session,
            10,
        )
        .unwrap();
    service
        .update_agent_objective(&raw.agent_id, Some("objective B"), 11)
        .unwrap();
    if send_changed {
        let changed = service.registered_identity(&raw.agent_id).unwrap().clone();
        service
            .accept_at_with_scope(
                &raw.agent_id,
                mail(&changed, &target.agent_id, "objective-B-mail"),
                MessageScope::Session,
                12,
            )
            .unwrap();
    }
    service
        .update_agent_objective(&raw.agent_id, Some("objective C unsent"), 13)
        .unwrap();
    let before = service.snapshot_state();
    assert!(
        service
            .retire_fresh_observational_identity(&raw.agent_id)
            .unwrap(),
        "valid objective drift must not become a critical disconnect"
    );
    let after = service.snapshot_state();
    assert_eq!(after.retained_messages, before.retained_messages);
    assert_eq!(after.accepted_messages, before.accepted_messages);
    assert!(service.registered_identity(&raw.agent_id).is_none());
    assert!(service.subscription(&raw.agent_id).is_none());
    let restored = MessageService::from_snapshot_state(&after).unwrap();
    assert_eq!(restored.snapshot_state(), after);
}

/// A caller claiming fresh ownership of pane state or changed immutable sender metadata
/// must receive an integrity error BEFORE any destructive cleanup. This keeps
/// generic registration and snapshot-validation contracts strict, not permissive.
#[test]
fn fresh_retirement_rejects_changed_ownership_and_sender_evidence() {
    let mut service = MessageService::default();
    let pane = service.register_agent(Some(PaneId::new('%', 1)), None, "pane", Vec::new());
    service.subscribe(&pane.agent_id).unwrap();
    let before = service.snapshot_state();
    assert!(
        service
            .retire_fresh_observational_identity(&pane.agent_id)
            .is_err()
    );
    assert_eq!(service.snapshot_state(), before);
    let raw = service.register_agent(None, None, "raw", Vec::new());
    service
        .accept_at_with_scope(
            &raw.agent_id,
            mail(&raw, &pane.agent_id, "immutable-evidence"),
            MessageScope::Session,
            10,
        )
        .unwrap();
    // Test-local shared-state corruption, not a supported objective update.
    service.registered.get_mut(&raw.agent_id).unwrap().role = Some("foreign role".to_string());
    let before = service.snapshot_state();
    assert!(
        service
            .retire_fresh_observational_identity(&raw.agent_id)
            .is_err()
    );
    assert_eq!(service.snapshot_state(), before);
}

/// Builds two genuine objective versions followed by unrelated retained mail.
/// Capacity three lets the FIFO regressions evict each raw version separately.
fn objective_history_fixture() -> (MessageService, SenderIdentity) {
    let mut service = MessageService::with_limits(3, 4096);
    let target = service.register_agent(None, None, "independent", Vec::new());
    let raw = service
        .register_agent_with_objective(
            None,
            None,
            "raw",
            vec!["one".to_string(), "two".to_string()],
            Some("A"),
        )
        .unwrap();
    service
        .accept_at_with_scope(
            &raw.agent_id,
            mail(&raw, &target.agent_id, "raw-A"),
            MessageScope::Session,
            10,
        )
        .unwrap();
    service
        .update_agent_objective(&raw.agent_id, Some("B"), 11)
        .unwrap();
    let changed = service.registered_identity(&raw.agent_id).unwrap().clone();
    service
        .accept_at_with_scope(
            &raw.agent_id,
            mail(&changed, &target.agent_id, "raw-B"),
            MessageScope::Session,
            12,
        )
        .unwrap();
    service
        .update_agent_objective(&raw.agent_id, Some("C unsent"), 13)
        .unwrap();
    service
        .accept_at_with_scope(
            &target.agent_id,
            mail(&target, &target.agent_id, "unrelated-C"),
            MessageScope::Session,
            14,
        )
        .unwrap();
    service
        .retire_fresh_observational_identity(&raw.agent_id)
        .unwrap();
    (service, target)
}

/// Evicting A must leave only exact B provenance with B as the new anchor;
/// evicting B must remove that fresh floor even while unrelated mail remains.
/// Pruning removes evidence only, never inventing a version from current state.
#[test]
fn fresh_objective_versions_prune_monotonically_with_authored_fifo() {
    let (mut service, target) = objective_history_fixture();
    let before = service.snapshot_state();
    let floor = &before.retired_delivery_floors[0];
    let versions = floor.fresh_sender_versions.as_ref().unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].objective.as_deref(), Some("A"));
    assert_eq!(versions[1].objective.as_deref(), Some("B"));
    assert_eq!(floor.last_sequence, 2);
    service
        .accept_at_with_scope(
            &target.agent_id,
            mail(&target, &target.agent_id, "unrelated-D"),
            MessageScope::Session,
            15,
        )
        .unwrap();
    let after_a = service.snapshot_state();
    assert_eq!(
        after_a.retired_delivery_floors[0].fresh_sender_versions,
        Some(vec![versions[1].clone()])
    );
    assert_eq!(after_a.retired_delivery_floors[0].identity, versions[1]);
    assert_eq!(after_a.retired_delivery_floors[0].last_sequence, 2);
    assert_eq!(
        MessageService::from_snapshot_state(&after_a)
            .unwrap()
            .snapshot_state(),
        after_a
    );
    service
        .accept_at_with_scope(
            &target.agent_id,
            mail(&target, &target.agent_id, "unrelated-E"),
            MessageScope::Session,
            16,
        )
        .unwrap();
    let after_b = service.snapshot_state();
    assert!(after_b.retired_delivery_floors.is_empty());
    assert_eq!(after_b.retained_messages.len(), 3);
    assert_eq!(after_b.accepted_messages.len(), 3);
    assert_eq!(
        MessageService::from_snapshot_state(&after_b)
            .unwrap()
            .snapshot_state(),
        after_b
    );
}

/// A recurring objective is one exact version, but its first surviving authored
/// occurrence can move after another version when FIFO evicts a prefix. Reorder
/// only the still-recorded versions, preserving mail, receipts and foreign cursor.
#[test]
fn fresh_recurring_objective_reorders_after_prefix_eviction() {
    let mut service = MessageService::with_limits(3, 4096);
    let target = service.register_agent(None, None, "target", Vec::new());
    let cursor = service.subscribe(&target.agent_id).unwrap();
    let raw = service
        .register_agent_with_objective(None, None, "raw", Vec::new(), Some("A"))
        .unwrap();
    for (index, objective) in ["A", "B", "A"].into_iter().enumerate() {
        service
            .update_agent_objective(&raw.agent_id, Some(objective), 10 + index as u64)
            .unwrap();
        let sender = service.registered_identity(&raw.agent_id).unwrap().clone();
        service
            .accept_at_with_scope(
                &raw.agent_id,
                mail(&sender, &target.agent_id, &format!("recurring-{index}")),
                MessageScope::Session,
                10 + index as u64,
            )
            .unwrap();
    }
    service
        .retire_fresh_observational_identity(&raw.agent_id)
        .unwrap();
    let before = service.snapshot_state();
    MessageService::from_snapshot_state(&before).unwrap();
    service
        .accept_at_with_scope(
            &target.agent_id,
            mail(&target, &target.agent_id, "unrelated-after-recurrence"),
            MessageScope::Session,
            13,
        )
        .unwrap();
    let after = service.snapshot_state();
    assert_eq!(after.retained_messages[..2], before.retained_messages[1..]);
    for accepted in before
        .accepted_messages
        .iter()
        .filter(|accepted| accepted.envelope.id != "recurring-0")
    {
        assert!(after.accepted_messages.contains(accepted));
    }
    assert_eq!(service.subscription(&target.agent_id), Some(&cursor));
    let floor = &after.retired_delivery_floors[0];
    assert_eq!(
        floor
            .fresh_sender_versions
            .as_ref()
            .unwrap()
            .iter()
            .map(|version| version.objective.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("B"), Some("A")]
    );
    assert_eq!(floor.identity.objective.as_deref(), Some("B"));
    assert_eq!(floor.last_sequence, 3);
    assert_eq!(
        MessageService::from_snapshot_state(&after)
            .unwrap()
            .snapshot_state(),
        after
    );
}

/// Schema3 generic reusable floors remain independently restorable after the
/// schema4 bump. The fresh-version field must be absent rather than silently
/// adopting new provenance into legacy delivery-interval semantics.
#[test]
fn legacy_schema3_generic_floor_roundtrip_is_unchanged() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", Vec::new());
    let pane = service.register_agent(Some(PaneId::new('%', 1)), None, "pane", Vec::new());
    service
        .accept_at_with_scope(
            &sender.agent_id,
            mail(&sender, &sender.agent_id, "unrelated-retained"),
            MessageScope::Session,
            10,
        )
        .unwrap();
    service.retire_agent_identity(&pane.agent_id);
    let mut legacy = service.snapshot_state();
    legacy.schema_version = 3;
    assert_eq!(legacy.retired_delivery_floors.len(), 1);
    assert!(
        legacy.retired_delivery_floors[0]
            .fresh_sender_versions
            .is_none()
    );
    let encoded = serde_json::to_value(&legacy).unwrap();
    assert!(
        encoded["retired_delivery_floors"][0]
            .get("fresh_sender_versions")
            .is_none()
    );
    let decoded: MessageServiceSnapshot = serde_json::from_value(encoded).unwrap();
    let restored = MessageService::from_snapshot_state(&decoded).unwrap();
    let mut roundtrip = restored.snapshot_state();
    roundtrip.schema_version = 3;
    assert_eq!(roundtrip, legacy);
}

/// Fresh schema4 evidence must be an exact nonempty unique projection of real
/// retained authored senders. Forgery rejection is non-destructive and does not
/// loosen legacy floors, active identities, receipts or frozen project audience.
#[test]
fn fresh_snapshot_rejects_forged_or_unreferenced_versions() {
    let (service, _) = objective_history_fixture();
    let before = service.snapshot_state();
    assert_eq!(before.schema_version, 4);
    let mut corruptions = Vec::new();
    let mut empty = before.clone();
    empty.retired_delivery_floors[0].fresh_sender_versions = Some(Vec::new());
    corruptions.push(empty);
    let mut duplicate = before.clone();
    let versions = duplicate.retired_delivery_floors[0]
        .fresh_sender_versions
        .as_mut()
        .unwrap();
    versions.push(versions[0].clone());
    corruptions.push(duplicate);
    let mut missing = before.clone();
    missing.retired_delivery_floors[0]
        .fresh_sender_versions
        .as_mut()
        .unwrap()
        .pop();
    corruptions.push(missing);
    let mut absent = before.clone();
    absent.retired_delivery_floors[0].fresh_sender_versions = None;
    corruptions.push(absent);
    let mut unused = before.clone();
    let versions = unused.retired_delivery_floors[0]
        .fresh_sender_versions
        .as_mut()
        .unwrap();
    let mut invented = versions[0].clone();
    invented.objective = Some("unsent invented version".to_string());
    versions.push(invented);
    corruptions.push(unused);
    let mut reordered = before.clone();
    reordered.retired_delivery_floors[0]
        .fresh_sender_versions
        .as_mut()
        .unwrap()
        .reverse();
    corruptions.push(reordered);
    let mut floor = before.clone();
    floor.retired_delivery_floors[0].last_sequence = 3;
    corruptions.push(floor);
    let mut receipt = before.clone();
    receipt.accepted_messages[0]
        .envelope
        .payload
        .push_str("forged");
    corruptions.push(receipt);
    let mut audience = before.clone();
    let foreign_scope = ProjectScopeId::from_canonical_root_bytes(b"/foreign")
        .snapshot_value()
        .to_string();
    audience.retained_messages[0].audience = Some(MessageAudienceSnapshot {
        kind: "project".to_string(),
        project_scope: Some(foreign_scope.clone()),
    });
    let id = audience.retained_messages[0].envelope.id.clone();
    audience
        .accepted_messages
        .iter_mut()
        .find(|accepted| accepted.envelope.id == id)
        .unwrap()
        .audience = Some(MessageAudienceSnapshot {
        kind: "project".to_string(),
        project_scope: Some(foreign_scope),
    });
    corruptions.push(audience);
    let mut unordered = before.clone();
    unordered.retained_messages.swap(0, 1);
    corruptions.push(unordered);
    let mut unsent_anchor = before.clone();
    unsent_anchor.retired_delivery_floors[0].identity.objective = Some("C unsent".to_string());
    corruptions.push(unsent_anchor);
    let mut unsupported = before.clone();
    unsupported.schema_version = 5;
    corruptions.push(unsupported);
    let mut active = before.clone();
    active
        .registered_agents
        .push(active.retired_delivery_floors[0].identity.clone());
    corruptions.push(active);
    for schema in 1..=3 {
        let mut legacy = before.clone();
        legacy.schema_version = schema;
        corruptions.push(legacy.clone());
        legacy.retired_delivery_floors[0].fresh_sender_versions = Some(Vec::new());
        corruptions.push(legacy);
    }
    // Every authority-bearing field must compare exactly, including ordered
    // capabilities and a syntactically valid but foreign project membership.
    for field in ["agent", "project", "pane", "window", "role", "capabilities"] {
        let mut forged = before.clone();
        let version = &mut forged.retired_delivery_floors[0]
            .fresh_sender_versions
            .as_mut()
            .unwrap()[1];
        match field {
            "agent" => version.agent_id = "a999".to_string(),
            "project" => {
                version.project_scope = Some(
                    ProjectScopeId::from_canonical_root_bytes(b"/foreign")
                        .snapshot_value()
                        .to_string(),
                )
            }
            "pane" => version.pane_id = Some("%1".to_string()),
            "window" => version.window_id = Some("@1".to_string()),
            "role" => version.role = Some("foreign".to_string()),
            "capabilities" => version.capabilities.reverse(),
            _ => unreachable!(),
        }
        corruptions.push(forged);
    }
    for (index, candidate) in corruptions.into_iter().enumerate() {
        let encoded = serde_json::to_vec(&candidate).unwrap();
        let decoded: MessageServiceSnapshot = serde_json::from_slice(&encoded).unwrap();
        assert!(
            MessageService::from_snapshot_state(&decoded).is_err(),
            "forgery {index} was accepted"
        );
        assert_eq!(
            service.snapshot_state(),
            before,
            "forgery mutated live state"
        );
    }
}
