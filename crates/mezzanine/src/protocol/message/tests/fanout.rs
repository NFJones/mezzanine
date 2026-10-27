//! Messaging fanout tests.

use super::*;
use std::sync::Arc;

/// A framed accepted broadcast remains available to a slow subscribed peer
/// after another matching reusable identity retires and unrelated traffic evicts
/// its replay reference. Actual frame encoding and
/// cursor settlement must deliver the same original occurrence exactly once,
/// without a resend or a second sender acceptance.
#[test]
fn framed_fanout_survives_generic_recipient_retirement() {
    let mut service = MessageService::with_limits(1, 1024);
    let mut sender = MessageConnection::default();
    handle_mmp_frame(
        &encode_mmp_body(r#"{"protocol":"mmp/1","type":"hello","role":"sender"}"#),
        4096,
        &mut service,
        &mut sender,
        10,
    )
    .unwrap();
    let first = service.register_agent(None, None, "worker", Vec::new());
    let second = service.register_agent(None, None, "worker", Vec::new());
    for target in [&first, &second] {
        service.subscribe(&target.agent_id).unwrap();
    }
    let body = serde_json::json!({
        "protocol": "mmp/1", "type": "send", "id": "framed-original",
        "time": "fixture:retirement", "sender": {
            "agent_id": sender.agent_id.as_ref().unwrap().as_str(), "role": "sender"
        },
        "recipient": {"role": "worker"}, "scope": "session",
        "correlation_id": "original-request", "ttl_ms": 100,
        "content_type": "text/plain; charset=utf-8", "payload": "original fanout body"
    });
    let (reply, _) = handle_mmp_frame(
        &encode_mmp_body(&body.to_string()),
        4096,
        &mut service,
        &mut sender,
        11,
    )
    .unwrap();
    let (reply, _) = decode_mmp_frame(&reply, 4096).unwrap();
    let accepted: serde_json::Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(accepted["status"], "accepted");
    assert_eq!(accepted["queued_recipients"], 2);
    let sequence = service.snapshot_state().accepted_messages[0]
        .delivery
        .sequence;
    let mut sink = CollectingFanoutSink::default();
    assert_eq!(
        flush_message_fanout_for(&mut service, &first.agent_id, 12, 10, &mut sink).unwrap(),
        1
    );
    assert!(service.retire_agent_identity(&first.agent_id));
    let mut pressure = body.clone();
    pressure["id"] = serde_json::json!("framed-pressure");
    pressure["recipient"] =
        serde_json::json!({"agent_id": sender.agent_id.as_ref().unwrap().as_str()});
    let (reply, _) = handle_mmp_frame(
        &encode_mmp_body(&pressure.to_string()),
        4096,
        &mut service,
        &mut sender,
        12,
    )
    .unwrap();
    let (reply, _) = decode_mmp_frame(&reply, 4096).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&reply).unwrap()["status"],
        "accepted"
    );
    let snapshot = service.snapshot_state();
    assert_eq!(snapshot.retained_messages.len(), 1);
    assert_eq!(snapshot.retained_messages[0].envelope.id, "framed-pressure");
    assert_eq!(
        snapshot.custody_messages.as_ref().unwrap()[0].envelope.id,
        "framed-original"
    );
    let mut service = MessageService::from_snapshot_state(&snapshot).unwrap();
    assert_eq!(
        service
            .subscription(&second.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
    assert_eq!(
        flush_message_fanout_for(&mut service, &second.agent_id, 13, 10, &mut sink).unwrap(),
        1
    );
    assert_eq!(sink.frames.len(), 2);
    let mut delivered = Vec::new();
    for (target, frame) in &sink.frames {
        assert!([&first.agent_id, &second.agent_id].contains(&target));
        let (body, _) = decode_mmp_frame(frame, 4096).unwrap();
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        let message = &body["messages"][0];
        assert_eq!(message["sequence"], sequence);
        assert_eq!(message["envelope"]["id"], "framed-original");
        assert_eq!(message["envelope"]["payload"], "original fanout body");
        delivered.push(message.clone());
    }
    assert_eq!(delivered[0], delivered[1]);
    assert_eq!(
        service
            .subscription(&second.agent_id)
            .unwrap()
            .last_sequence,
        sequence
    );
    assert_eq!(
        flush_message_fanout_for(&mut service, &second.agent_id, 14, 10, &mut sink).unwrap(),
        0
    );
    assert_eq!(
        service.snapshot_state().accepted_messages,
        snapshot
            .accepted_messages
            .into_iter()
            .filter(|accepted| accepted.envelope.id != "framed-original")
            .collect::<Vec<_>>()
    );
    assert!(
        service
            .snapshot_state()
            .custody_messages
            .unwrap()
            .is_empty()
    );
}

/// Verifies fanout ready batches subscribed recipients without advancing.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn fanout_ready_batches_subscribed_recipients_without_advancing() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let first = service.register_agent(None, None, "worker", Vec::new());
    let second = service.register_agent(None, None, "reviewer", Vec::new());
    service.subscribe(&second.agent_id).unwrap();
    service.subscribe(&first.agent_id).unwrap();

    let mut fanout = envelope(sender.clone());
    fanout.id = "fanout".to_string();
    fanout.recipient = Recipient::Session;
    service
        .accept_at_with_scope(&sender.agent_id, fanout, MessageScope::Session, 10)
        .unwrap();

    let ready = service.fanout_ready(11, 10);

    assert_eq!(
        ready
            .iter()
            .map(|batch| batch.recipient.as_str())
            .collect::<Vec<_>>(),
        vec![first.agent_id.as_str(), second.agent_id.as_str()]
    );
    assert_eq!(ready[0].batch.messages[0].envelope.id, "fanout");
    assert_eq!(
        service
            .receive_subscribed(&first.agent_id, 12, usize::MAX)
            .unwrap()
            .messages
            .len(),
        1
    );
}

/// Verifies acknowledging fanout batch advances only that recipient.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn acknowledging_fanout_batch_advances_only_that_recipient() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let first = service.register_agent(None, None, "worker", Vec::new());
    let second = service.register_agent(None, None, "reviewer", Vec::new());
    service.subscribe(&first.agent_id).unwrap();
    service.subscribe(&second.agent_id).unwrap();

    let mut fanout = envelope(sender.clone());
    fanout.id = "fanout".to_string();
    fanout.recipient = Recipient::Session;
    let delivery = service
        .accept_at_with_scope(&sender.agent_id, fanout, MessageScope::Session, 10)
        .unwrap();
    let ready = service.fanout_ready(11, 10);

    service.acknowledge_fanout_batch(&ready[0]).unwrap();

    assert!(
        service
            .receive_subscribed(&first.agent_id, 12, usize::MAX)
            .unwrap()
            .messages
            .is_empty()
    );
    let second_batch = service
        .receive_subscribed(&second.agent_id, 12, usize::MAX)
        .unwrap();
    assert_eq!(second_batch.messages[0].sequence, delivery.sequence);
}

/// Verifies flush message fanout writes frames and advances cursors.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn flush_message_fanout_writes_frames_and_advances_cursors() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let target = service.register_agent(None, None, "worker", Vec::new());
    service.subscribe(&target.agent_id).unwrap();
    let mut message = envelope(sender.clone());
    message.recipient = Recipient::Agent(target.agent_id.clone());
    service
        .accept_at_with_scope(&sender.agent_id, message, MessageScope::Session, 10)
        .unwrap();
    let mut sink = CollectingFanoutSink::default();

    let sent = flush_message_fanout(&mut service, 11, 10, &mut sink).unwrap();

    assert_eq!(sent, 1);
    assert_eq!(sink.frames.len(), 1);
    assert_eq!(sink.frames[0].0, target.agent_id);
    let (body, _) = decode_mmp_frame(&sink.frames[0].1, 4096).unwrap();
    assert!(body.contains(r#""type":"deliver""#));
    assert!(body.contains(r#""payload":"hello""#));
    assert!(body.contains(r#""envelope":{"protocol":"mmp/1""#));
    assert!(body.contains(r#""sequence":1"#));
    assert!(body.contains(r#""time":"message:test""#));
    assert!(
        service
            .receive_subscribed(&target.agent_id, 12, usize::MAX)
            .unwrap()
            .messages
            .is_empty()
    );
}

/// Verifies flush message fanout for writes only requested recipient.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn flush_message_fanout_for_writes_only_requested_recipient() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let first = service.register_agent(None, None, "worker", Vec::new());
    let second = service.register_agent(None, None, "reviewer", Vec::new());
    service.subscribe(&first.agent_id).unwrap();
    service.subscribe(&second.agent_id).unwrap();
    let mut message = envelope(sender.clone());
    message.recipient = Recipient::Session;
    service
        .accept_at_with_scope(&sender.agent_id, message, MessageScope::Session, 10)
        .unwrap();
    let mut sink = CollectingFanoutSink::default();

    let sent = flush_message_fanout_for(&mut service, &second.agent_id, 11, 10, &mut sink).unwrap();

    assert_eq!(sent, 1);
    assert_eq!(sink.frames.len(), 1);
    assert_eq!(sink.frames[0].0, second.agent_id);
    assert_eq!(
        service
            .receive_subscribed(&first.agent_id, 12, usize::MAX)
            .unwrap()
            .messages
            .len(),
        1
    );
    assert!(
        service
            .receive_subscribed(&second.agent_id, 12, usize::MAX)
            .unwrap()
            .messages
            .is_empty()
    );
}

/// Verifies failed fanout write does not advance cursor.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn failed_fanout_write_does_not_advance_cursor() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let target = service.register_agent(None, None, "worker", Vec::new());
    service.subscribe(&target.agent_id).unwrap();
    let mut message = envelope(sender.clone());
    message.recipient = Recipient::Agent(target.agent_id.clone());
    service
        .accept_at_with_scope(&sender.agent_id, message, MessageScope::Session, 10)
        .unwrap();
    let mut sink = FailingFanoutSink;

    let error = flush_message_fanout(&mut service, 11, 10, &mut sink).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Io);
    assert_eq!(
        service
            .receive_subscribed(&target.agent_id, 12, usize::MAX)
            .unwrap()
            .messages
            .len(),
        1
    );
}

/// Verifies one retained envelope is shared across subscriber batches rather
/// than deep-cloned once per recipient. Delivery metadata remains independent,
/// while both batches point at the same immutable message allocation.
#[test]
fn fanout_batches_share_retained_envelopes() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let first = service.register_agent(None, None, "worker", Vec::new());
    let second = service.register_agent(None, None, "reviewer", Vec::new());
    service.subscribe(&first.agent_id).unwrap();
    service.subscribe(&second.agent_id).unwrap();
    let mut message = envelope(sender.clone());
    message.recipient = Recipient::Session;
    service
        .accept_at_with_scope(&sender.agent_id, message, MessageScope::Session, 10)
        .unwrap();

    let ready = service.fanout_ready(11, 10);

    assert_eq!(ready.len(), 2);
    assert!(Arc::ptr_eq(
        &ready[0].batch.messages[0].envelope,
        &ready[1].batch.messages[0].envelope,
    ));
}

/// Verifies a one-recipient aggregate budget resumes after the last recipient
/// considered, so repeated bounded cycles serve every subscriber in stable
/// order instead of repeatedly selecting the first subscriber.
#[test]
fn bounded_fanout_resumes_fairly_across_subscribers() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let first = service.register_agent(None, None, "worker", Vec::new());
    let second = service.register_agent(None, None, "reviewer", Vec::new());
    let third = service.register_agent(None, None, "observer", Vec::new());
    for recipient in [&third.agent_id, &first.agent_id, &second.agent_id] {
        service.subscribe(recipient).unwrap();
    }
    let mut message = envelope(sender.clone());
    message.recipient = Recipient::Session;
    service
        .accept_at_with_scope(&sender.agent_id, message, MessageScope::Session, 10)
        .unwrap();
    let budget = FanoutBudget {
        max_recipients: 1,
        max_messages: 1,
        max_payload_bytes: usize::MAX,
    };

    let recipients = (0..3)
        .map(|_| {
            service.fanout_ready_with_budget(11, 10, budget)[0]
                .recipient
                .clone()
        })
        .collect::<Vec<_>>();

    assert_eq!(
        recipients,
        vec![first.agent_id, second.agent_id, third.agent_id]
    );
    let diagnostics = service.fanout_diagnostics();
    assert_eq!(diagnostics.cycles, 3);
    assert_eq!(diagnostics.recipients_considered, 3);
    assert_eq!(diagnostics.messages_selected, 3);
}

/// Verifies aggregate message and payload limits bound one fanout cycle even
/// when per-recipient limits are larger. The next cycle resumes with the next
/// subscriber rather than performing subscriber × retained-message work.
#[test]
fn fanout_honors_aggregate_message_and_payload_budgets() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let first = service.register_agent(None, None, "worker", Vec::new());
    let second = service.register_agent(None, None, "reviewer", Vec::new());
    service.subscribe(&first.agent_id).unwrap();
    service.subscribe(&second.agent_id).unwrap();
    for id in ["first-message", "second-message"] {
        let mut message = envelope(sender.clone());
        message.id = id.to_string();
        message.recipient = Recipient::Session;
        service
            .accept_at_with_scope(&sender.agent_id, message, MessageScope::Session, 10)
            .unwrap();
    }
    let budget = FanoutBudget {
        max_recipients: 2,
        max_messages: 1,
        max_payload_bytes: "hello".len(),
    };

    let first_cycle = service.fanout_ready_with_budget(11, 10, budget);
    let second_cycle = service.fanout_ready_with_budget(11, 10, budget);

    assert_eq!(first_cycle.len(), 1);
    assert_eq!(first_cycle[0].recipient, first.agent_id);
    assert_eq!(first_cycle[0].batch.messages.len(), 1);
    assert_eq!(second_cycle.len(), 1);
    assert_eq!(second_cycle[0].recipient, second.agent_id);
    assert_eq!(second_cycle[0].batch.messages.len(), 1);
    let diagnostics = service.fanout_diagnostics();
    assert_eq!(diagnostics.messages_selected, 2);
    assert_eq!(diagnostics.payload_bytes_selected, 2 * "hello".len() as u64);
}

/// Verifies direct-recipient fanout performs retained sequence lookups only
/// for that recipient's index, regardless of unrelated retained traffic.
#[test]
fn direct_fanout_uses_recipient_index_for_lookup_work() {
    let mut service = MessageService::with_limits(256, 1024 * 1024);
    let sender = service.register_agent(None, None, "default", Vec::new());
    let target = service.register_agent(None, None, "target", Vec::new());
    let mut unrelated = Vec::new();
    for index in 0..64 {
        unrelated.push(service.register_agent(None, None, format!("worker-{index}"), Vec::new()));
    }
    service.subscribe(&target.agent_id).unwrap();
    for (index, recipient) in unrelated.iter().enumerate() {
        let mut message = envelope(sender.clone());
        message.id = format!("unrelated-{index}");
        message.recipient = Recipient::Agent(recipient.agent_id.clone());
        service
            .accept_at_with_scope(&sender.agent_id, message, MessageScope::Session, 10)
            .unwrap();
    }
    let mut target_message = envelope(sender.clone());
    target_message.id = "target-message".to_string();
    target_message.recipient = Recipient::Agent(target.agent_id.clone());
    service
        .accept_at_with_scope(&sender.agent_id, target_message, MessageScope::Session, 10)
        .unwrap();

    let ready = service.fanout_ready_with_budget(
        11,
        10,
        FanoutBudget {
            max_recipients: 1,
            max_messages: 10,
            max_payload_bytes: usize::MAX,
        },
    );

    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].batch.messages.len(), 1);
    assert_eq!(ready[0].batch.messages[0].envelope.id, "target-message");
    assert_eq!(service.fanout_diagnostics().sequence_lookups, 1);
}

/// Verifies overlapping group and capability selectors do not enqueue the
/// same retained sequence twice for one subscriber batch.
#[test]
fn indexed_fanout_deduplicates_overlapping_recipient_selectors() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let target = service.register_agent(
        None,
        None,
        "worker",
        vec!["session".to_string(), "session".to_string()],
    );
    service.subscribe(&target.agent_id).unwrap();
    let mut message = envelope(sender.clone());
    message.recipient = Recipient::Group("session".to_string());
    service
        .accept_at_with_scope(&sender.agent_id, message, MessageScope::Session, 10)
        .unwrap();

    let ready = service.fanout_ready(11, 10);

    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].batch.messages.len(), 1);
}
