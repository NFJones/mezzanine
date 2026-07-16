//! Messaging fanout tests.

use super::*;

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
    service.accept_at(&sender.agent_id, fanout, 10).unwrap();

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
    let delivery = service.accept_at(&sender.agent_id, fanout, 10).unwrap();
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
    service.accept_at(&sender.agent_id, message, 10).unwrap();
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
    service.accept_at(&sender.agent_id, message, 10).unwrap();
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
    service.accept_at(&sender.agent_id, message, 10).unwrap();
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
