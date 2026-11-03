//! Reused sender addresses retain old source facts without old delivery rights.

use super::*;

/// Constructs an actual direct peer occurrence with independent immutable text.
fn mail(sender: &SenderIdentity, recipient: &AgentId, id: &str) -> Envelope {
    Envelope {
        protocol: MMP_PROTOCOL,
        id: id.into(),
        message_type: "send".into(),
        time: "fixture:sender-restore".into(),
        sender: sender.clone(),
        recipient: Recipient::Agent(recipient.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain".into(),
        payload: id.into(),
        extension_fields: Vec::new(),
    }
}

/// A successful same-address registration may coexist with unread authored
/// source. A snapshot taken before the replacement subscribes must restore the
/// old recipient's original occurrence, expose only new-interval replies to the
/// replacement and preserve exact source bytes rather than redispatching mail.
#[test]
fn reused_sender_with_pending_authored_source_and_new_reply_roundtrips() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    let original = mail(&sender, &target.agent_id, "original-request");
    let accepted = service
        .accept_at_with_scope(&sender.agent_id, original.clone(), MessageScope::Session, 1)
        .unwrap();
    service
        .accept_at_with_scope(
            &target.agent_id,
            mail(&target, &sender.agent_id, "old-owner-mail"),
            MessageScope::Session,
            2,
        )
        .unwrap();
    assert!(service.retire_observational_identity(&sender.agent_id));
    service.ensure_agent_identity(sender.clone(), 3).unwrap();
    service
        .accept_at_with_scope(
            &target.agent_id,
            mail(&target, &sender.agent_id, "replacement-reply"),
            MessageScope::Session,
            4,
        )
        .unwrap();
    let snapshot = service.snapshot_state().unwrap();
    let mut restored = MessageService::from_snapshot_state(&snapshot).unwrap();
    assert_eq!(restored.snapshot_state().unwrap(), snapshot);
    assert_eq!(&restored.receive_for(&target.agent_id, 5)[0], &original);
    restored
        .subscribe_from_retained_start(&sender.agent_id)
        .unwrap();
    let visible = restored
        .receive_subscribed(&sender.agent_id, 5, 10)
        .unwrap();
    assert_eq!(
        visible
            .messages
            .iter()
            .map(|m| m.envelope.id.as_str())
            .collect::<Vec<_>>(),
        ["replacement-reply"]
    );
    assert_eq!(
        restored
            .accept_at_with_scope(&sender.agent_id, original, MessageScope::Session, 5)
            .unwrap(),
        accepted
    );
}

/// Multiple replacements with different discovery metadata cannot rewrite old
/// authors or acquire their pending input. Custody-only originals, stable IDs
/// and the current interval survive each restore without a second send.
#[test]
fn changed_sender_metadata_and_multiple_reuse_keep_custody_only_sources() {
    let mut service = MessageService::with_limits(1, 4096);
    let mut sender = service.register_agent(None, None, "first", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    service
        .subscribe_from_retained_start(&target.agent_id)
        .unwrap();
    let mut originals = Vec::new();
    for (index, role) in ["first", "second", "third"].iter().enumerate() {
        if index > 0 {
            assert!(service.retire_agent_identity(&sender.agent_id));
            sender.role = Some((*role).into());
            sender.objective = Some(format!("task-{index}"));
            service
                .ensure_agent_identity(sender.clone(), index as u64)
                .unwrap();
        }
        let input = mail(&target, &sender.agent_id, &format!("input-{index}"));
        service
            .accept_at_with_scope(
                &target.agent_id,
                input,
                MessageScope::Session,
                10 + index as u64,
            )
            .unwrap();
        let outgoing = mail(&sender, &target.agent_id, &format!("source-{index}"));
        service
            .accept_at_with_scope(
                &sender.agent_id,
                outgoing.clone(),
                MessageScope::Session,
                10 + index as u64,
            )
            .unwrap();
        originals.push(outgoing);
        let snapshot = service.snapshot_state().unwrap();
        assert!(!snapshot.custody_messages.as_ref().unwrap().is_empty());
        service = MessageService::from_snapshot_state(&snapshot).unwrap();
        assert_eq!(
            service
                .receive_subscribed(&target.agent_id, 20, 100)
                .unwrap()
                .messages
                .iter()
                .map(|row| row.envelope.as_ref().clone())
                .collect::<Vec<_>>(),
            originals
        );
        service
            .subscribe_from_retained_start(&sender.agent_id)
            .unwrap();
        assert_eq!(
            service
                .receive_subscribed(&sender.agent_id, 20, 100)
                .unwrap()
                .messages
                .iter()
                .map(|m| m.envelope.id.as_str())
                .collect::<Vec<_>>(),
            [format!("input-{index}")]
        );
        assert!(
            service
                .acceptance_proof(&sender.agent_id, service.last_sequence())
                .is_none(),
            "restore is historical, not new acceptance"
        );
    }
}

/// Schema9's metadata-only table is complete exact source evidence, not a way
/// to invent sender addresses, reorder occurrences or supply a live capability.
/// Corruptions must reject while leaving the original service untouched.
#[test]
fn sender_provenance_corruption_future_schema_and_legacy_smuggling_fail_closed() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    service
        .accept_at_with_scope(
            &sender.agent_id,
            mail(&sender, &target.agent_id, "original"),
            MessageScope::Session,
            1,
        )
        .unwrap();
    let before = service.snapshot_state().unwrap();
    let mut cases = Vec::new();
    let mut bad = before.clone();
    bad.sender_provenance = None;
    cases.push(bad);
    let mut bad = before.clone();
    bad.sender_provenance.as_mut().unwrap().clear();
    cases.push(bad);
    let mut bad = before.clone();
    bad.sender_provenance
        .as_mut()
        .unwrap()
        .push(before.sender_provenance.as_ref().unwrap()[0].clone());
    cases.push(bad);
    let mut bad = before.clone();
    bad.sender_provenance.as_mut().unwrap()[0].sequence += 1;
    cases.push(bad);
    let mut bad = before.clone();
    bad.sender_provenance.as_mut().unwrap()[0].sender_sha256[0] ^= 1;
    cases.push(bad);
    let mut bad = before.clone();
    bad.sender_provenance.as_mut().unwrap()[0].registered_after_sequence = Some(1);
    cases.push(bad);
    let mut bad = before.clone();
    bad.schema_version = 10;
    cases.push(bad);
    let mut bad = before.clone();
    bad.schema_version = 8;
    cases.push(bad);
    let mut bad = before.clone();
    bad.sender_provenance.as_mut().unwrap()[0].fresh_retirement = true;
    cases.push(bad);
    let mut bad = before.clone();
    let mut foreign = bad.retained_messages[0].envelope.sender.clone();
    foreign.agent_id = "unknown-sender".into();
    bad.retained_messages[0].envelope.sender = foreign.clone();
    bad.accepted_messages[0].envelope.sender = foreign.clone();
    bad.sender_provenance.as_mut().unwrap()[0].sender_sha256 =
        super::super::sender_provenance::identity_digest(&foreign);
    cases.push(bad);
    for bad in cases {
        assert!(MessageService::from_snapshot_state(&bad).is_err());
    }
    assert_eq!(service.snapshot_state().unwrap(), before);
}

/// A legacy snapshot with an exact current identity and a redundant older
/// delivery floor can be normalized without inventing any source metadata.
/// Unknown identity/interval history remains an error and the input is intact.
#[test]
fn legacy_exact_reused_floor_migrates_but_ambiguous_history_does_not() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    service
        .accept_at_with_scope(
            &sender.agent_id,
            mail(&sender, &target.agent_id, "authored"),
            MessageScope::Session,
            1,
        )
        .unwrap();
    service
        .accept_at_with_scope(
            &target.agent_id,
            mail(&target, &sender.agent_id, "old-input"),
            MessageScope::Session,
            2,
        )
        .unwrap();
    service.retire_agent_identity(&sender.agent_id);
    let retired = service.snapshot_state().unwrap().retired_delivery_floors;
    service.ensure_agent_identity(sender.clone(), 3).unwrap();
    let mut legacy = service.snapshot_state().unwrap();
    legacy.schema_version = 8;
    legacy.sender_provenance = None;
    legacy.retired_delivery_floors = retired;
    let original = legacy.clone();
    let mut restored = MessageService::from_snapshot_state(&legacy).unwrap();
    assert_eq!(legacy, original);
    assert_eq!(restored.snapshot_state().unwrap().schema_version, 9);
    restored
        .subscribe_from_retained_start(&sender.agent_id)
        .unwrap();
    assert!(
        restored
            .receive_subscribed(&sender.agent_id, 4, 10)
            .unwrap()
            .messages
            .is_empty()
    );
    let mut duplicate = legacy.clone();
    duplicate
        .retired_delivery_floors
        .push(duplicate.retired_delivery_floors[0].clone());
    assert!(MessageService::from_snapshot_state(&duplicate).is_err());
    let mut duplicate_versions = legacy.clone();
    let identity = duplicate_versions.retired_delivery_floors[0]
        .identity
        .clone();
    duplicate_versions.retired_delivery_floors[0].fresh_sender_versions =
        Some(vec![identity.clone(), identity]);
    assert!(MessageService::from_snapshot_state(&duplicate_versions).is_err());
    let mut ambiguous = legacy.clone();
    ambiguous.retired_delivery_floors[0].identity.role = Some("different-old-role".into());
    assert!(MessageService::from_snapshot_state(&ambiguous).is_err());
    let mut unbounded = legacy;
    unbounded.retired_delivery_floors[0].last_sequence += 1;
    assert!(MessageService::from_snapshot_state(&unbounded).is_err());
}

/// Fresh raw provenance is not reconnect authority. An explicitly trusted lower
/// re-registration can snapshot before and after subscribing without retaining
/// an active retired-floor contradiction or changing immutable old source facts.
#[test]
fn fresh_and_reusable_re_registration_snapshots_are_stable_before_subscribe() {
    for fresh in [false, true] {
        let mut service = MessageService::default();
        let sender = service.register_agent(None, None, "raw", vec![]);
        let target = service.register_agent(None, None, "target", vec![]);
        let accepted = service
            .accept_at_with_scope(
                &sender.agent_id,
                mail(&sender, &target.agent_id, "authored"),
                MessageScope::Session,
                1,
            )
            .unwrap();
        let proof = service
            .acceptance_proof(&sender.agent_id, accepted.sequence)
            .unwrap();
        if fresh {
            service
                .retire_fresh_observational_identity(&sender.agent_id)
                .unwrap();
        } else {
            service.retire_agent_identity(&sender.agent_id);
        }
        service.ensure_agent_identity(sender.clone(), 2).unwrap();
        assert!(!service.acceptance_sender_is_current(&proof));
        let snapshot = service.snapshot_state().unwrap();
        let mut restored = MessageService::from_snapshot_state(&snapshot).unwrap();
        assert_eq!(restored.snapshot_state().unwrap(), snapshot);
        restored
            .subscribe_from_retained_start(&sender.agent_id)
            .unwrap();
        MessageService::from_snapshot_state(&restored.snapshot_state().unwrap()).unwrap();
        assert!(
            restored
                .acceptance_proof(&sender.agent_id, accepted.sequence)
                .is_none()
        );
    }
}

/// An objective refresh affects discovery, not previously authored messages.
/// Snapshot recovery must retain the old exact sender alongside the current
/// registered objective, without manufacturing a new acceptance occurrence.
#[test]
fn current_objective_refresh_cannot_rewrite_retained_sender_provenance() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    let original = mail(&sender, &target.agent_id, "before-objective-refresh");
    service
        .accept_at_with_scope(&sender.agent_id, original.clone(), MessageScope::Session, 1)
        .unwrap();
    service
        .update_agent_objective(&sender.agent_id, Some("new task"), 2)
        .unwrap();
    let snapshot = service.snapshot_state().unwrap();
    let restored = MessageService::from_snapshot_state(&snapshot).unwrap();
    assert_eq!(restored.receive_for(&target.agent_id, 3), [original]);
    assert_eq!(
        restored
            .registered_identity(&sender.agent_id)
            .unwrap()
            .objective
            .as_deref(),
        Some("new task")
    );
}

/// Historical project metadata survives a trusted current project rebind, but
/// cannot authorize new mail into the old project. The frozen audience continues
/// to protect the original occurrence and current acceptance uses current scope.
#[test]
fn historical_sender_scope_does_not_authorize_current_cross_project_send() {
    let mut service = MessageService::default();
    let mut sender = service.register_agent(None, None, "sender", vec![]);
    let mut target = service.register_agent(None, None, "target", vec![]);
    let old = ProjectScopeId::from_canonical_root_bytes(b"/old-project");
    let new = ProjectScopeId::from_canonical_root_bytes(b"/new-project");
    sender.project_scope = Some(old.clone());
    target.project_scope = Some(old);
    service.ensure_agent_identity(sender.clone(), 0).unwrap();
    service.ensure_agent_identity(target.clone(), 0).unwrap();
    let original = mail(&sender, &target.agent_id, "old-project-source");
    service
        .accept_at_with_scope(&sender.agent_id, original.clone(), MessageScope::Project, 1)
        .unwrap();
    let current = service
        .rebind_agent_project_scope(&sender.agent_id, Some(new))
        .unwrap();
    let mut restored =
        MessageService::from_snapshot_state(&service.snapshot_state().unwrap()).unwrap();
    assert_eq!(restored.receive_for(&target.agent_id, 2), [original]);
    let before = restored.snapshot_state().unwrap();
    assert!(
        restored
            .accept_at_with_scope(
                &current.agent_id,
                mail(&current, &target.agent_id, "foreign-new-send"),
                MessageScope::Project,
                2
            )
            .is_err()
    );
    assert_eq!(restored.snapshot_state().unwrap(), before);
}

/// The schema9 identity codec is a stable, allocation-free field encoding.
/// A pinned independent SHA256 fixture and changes to every field protect
/// optional/empty distinctions, vector order/cardinality and byte boundaries.
#[test]
fn sender_identity_digest_pins_all_fields_and_unambiguous_framing() {
    let base = MessageIdentitySnapshot {
        agent_id: "agent".into(),
        pane_id: None,
        window_id: None,
        role: Some("role".into()),
        capabilities: vec!["ab".into(), "c".into()],
        objective: None,
        project_scope: None,
    };
    let digest = super::super::sender_provenance::identity_digest(&base);
    assert_eq!(
        digest,
        [
            211, 124, 135, 87, 95, 246, 68, 83, 185, 3, 39, 86, 151, 101, 100, 207, 54, 171, 233,
            226, 119, 20, 115, 117, 202, 4, 203, 105, 32, 173, 97, 171
        ]
    );
    let mut variants = Vec::new();
    let mut changed = base.clone();
    changed.agent_id = "other".into();
    variants.push(changed);
    let mut changed = base.clone();
    changed.project_scope = Some("project".into());
    variants.push(changed);
    let mut changed = base.clone();
    changed.pane_id = Some("%1".into());
    variants.push(changed);
    let mut changed = base.clone();
    changed.window_id = Some("@1".into());
    variants.push(changed);
    let mut changed = base.clone();
    changed.role = None;
    variants.push(changed);
    let mut changed = base.clone();
    changed.role = Some(String::new());
    variants.push(changed);
    let mut changed = base.clone();
    changed.objective = Some(String::new());
    variants.push(changed);
    let mut changed = base.clone();
    changed.capabilities = vec!["a".into(), "bc".into()];
    variants.push(changed);
    let mut changed = base.clone();
    changed.capabilities.reverse();
    variants.push(changed);
    let mut changed = base.clone();
    changed.capabilities.push(String::new());
    variants.push(changed);
    let mut seen = HashSet::new();
    seen.insert(digest);
    for variant in variants {
        assert!(seen.insert(super::super::sender_provenance::identity_digest(&variant)));
    }
}
