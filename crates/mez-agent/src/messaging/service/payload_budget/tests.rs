//! Allocation ownership must outlive cache rows, while dead tickets free space.

use super::*;

/// Constructs actual independently owned UTF-8 allocations for the ledger only.
fn envelope(bytes: usize) -> Arc<Envelope> {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "source", vec![]);
    Arc::new(Envelope {
        protocol: MMP_PROTOCOL,
        id: "fixture".into(),
        message_type: "send".into(),
        time: "original".into(),
        recipient: Recipient::Agent(sender.agent_id.clone()),
        sender,
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain".into(),
        payload: "x".repeat(bytes),
        extension_fields: vec![],
    })
}

/// Dropping the canonical cache/row owner does not free an external delivery
/// pin. A retained reservation handle likewise cannot keep truly dead payloads
/// charged after binding; weak allocation lifetime is the source of truth.
#[test]
fn external_payload_owner_remains_charged_after_cache_owner_drops() {
    let budget = PayloadBudget::new(1, 10);
    let reservation = budget.reserve(1, 10, 0).unwrap();
    let cached = envelope(10);
    reservation.bind(&cached).unwrap();
    let external = cached.clone();
    drop(cached);
    assert_eq!(budget.usage().unwrap().live_payload_bytes, 10);
    assert!(budget.reserve(1, 1, 0).is_err());
    assert!(reservation.retire_joined().is_err());
    drop(external);
    assert_eq!(budget.usage().unwrap(), MessagePayloadUsage::default());
    let _next = budget.reserve(1, 10, 0).unwrap();
    assert!(
        reservation.bind(&envelope(10)).is_err(),
        "retired slot cannot alias a new allocation"
    );
}

/// Original worker escrow keeps an unbound decoder reservation charged through
/// loss of the linear input. Only the qualified joined owner retires that slot;
/// a later bind through the obsolete input cannot steal newly reserved capacity.
#[test]
fn worker_escrow_retains_unbound_reservation_until_joined_retirement() {
    let budget = PayloadBudget::new(2, 10);
    let input = budget.reserve(1, 10, 0).unwrap();
    let escrow = input.clone();
    drop(input);
    assert_eq!(budget.usage().unwrap().reserved_payload_bytes, 10);
    assert!(budget.reserve(1, 1, 0).is_err());
    escrow.retire_joined().unwrap();
    let next = budget.reserve(1, 10, 0).unwrap();
    assert!(escrow.bind(&envelope(10)).is_err());
    assert_eq!(budget.usage().unwrap().reserved_allocations, 1);
    drop(next);
    assert_eq!(budget.usage().unwrap(), MessagePayloadUsage::default());
}

/// Empty bodies still consume an allocation slot, and arbitrary long-lived
/// histories cannot grow the ledger metadata after all data/reservations drop.
#[test]
fn empty_payloads_obey_entry_limit_and_dead_history_is_pruned() {
    let budget = PayloadBudget::new(1, 10);
    let data = envelope(0);
    let reservation = budget.reserve(1, 0, 0).unwrap();
    reservation.bind(&data).unwrap();
    assert!(budget.reserve(1, 0, 0).is_err());
    drop(data);
    for _ in 0..10_000 {
        let reservation = budget.reserve(1, 0, 0).unwrap();
        let data = envelope(0);
        reservation.bind(&data).unwrap();
    }
    assert_eq!(budget.usage().unwrap(), MessagePayloadUsage::default());
    assert!(budget.state.lock().unwrap().slots.is_empty());
}
