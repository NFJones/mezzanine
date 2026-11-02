//! Physical storage delay must consume the original raw acceptance TTL.
//!
//! These controls use a real native conversation, actor and private SQLite root.
//! No provider executes; the native context and canonical delivery cursor expose
//! whether the original source crossed the input boundary after the worker gate.

use super::raw_actor::{body, hello};
use super::*;
use crate::protocol::message::encode_mmp_body;

/// Holds an actual acceptance write beyond a short TTL, then checks historical
/// acceptance separately from native input. Live finite and infinite controls
/// prove the storage completion path still delivers eligible original messages.
async fn storage_delay_control(ttl_ms: Option<u64>, should_deliver: bool) {
    let fixture = Fixture::new(false);
    let (handle, actor, _, turn, _) = super::native_actor::actor_fixture(&fixture).await;
    let recipient = mez_core::ids::AgentId::opaque(turn.agent_id.clone()).unwrap();
    let initial_cursor = actor
        .service
        .message_service()
        .subscription(&recipient)
        .unwrap()
        .last_sequence;
    let runner = tokio::spawn(actor.run());
    let (connection, owner, _lease) = hello(&handle).await;
    let now_ms = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let input = encode_mmp_body(&serde_json::json!({
        "protocol":"mmp/1","type":"send","id":"storage-delay-native-input",
        "time":"immutable-wire-time","scope":"session",
        "sender":{"agent_id":connection.agent_id.as_ref().unwrap().as_str()},
        "recipient":{"agent_id":recipient.as_str()},"ttl_ms":ttl_ms,"correlation_id":null,
        "content_type":"text/plain; charset=utf-8","payload":"original storage-delayed native input"
    }).to_string());
    let sender = handle.clone();
    let caller = tokio::spawn(async move {
        sender
            .handle_message_input_with_owner(input, 4096, connection, now_ms, Some(owner))
            .await
    });
    fixture.probe.started.notified().await;
    assert!(!caller.is_finished());
    assert_eq!(fixture.disk(), (0, 0));
    assert!(handle.lifecycle_state().await.is_ok());
    tokio::time::sleep(Duration::from_millis(30)).await;
    fixture.probe.release();
    let response = caller.await.unwrap().unwrap();
    assert_eq!(
        body(&response.output)["type"],
        "ack",
        "actual committed acceptance stays historical truth"
    );
    handle.shutdown().await.unwrap();
    let exited = runner.await.unwrap();
    let cursor = exited
        .service
        .message_service()
        .subscription(&recipient)
        .unwrap()
        .last_sequence;
    assert_eq!(
        cursor > initial_cursor,
        should_deliver,
        "only actual native input may advance its canonical cursor"
    );
    let blocks = exited
        .service
        .agent_turn_contexts()
        .values()
        .flat_map(|context| context.blocks())
        .filter(|block| block.label.contains("storage-delay-native-input"))
        .count();
    assert_eq!(
        blocks,
        usize::from(should_deliver),
        "expired mail must not become native context"
    );
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 1);
    assert!(
        fixture.disk().1 > 0,
        "actual immutable acceptance remains on disk"
    );
}

/// A real write held beyond its original one-millisecond TTL must still settle
/// physical acceptance, but cannot inject that expired source into native input
/// or advance the recipient cursor as if input had committed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_storage_delay_expires_before_native_input() {
    storage_delay_control(Some(1), false).await;
}

/// A finite original TTL with remaining time must reach native input exactly once
/// after the same real write gate. Delayed delivery must not become blanket loss.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_storage_delay_preserves_live_native_input() {
    storage_delay_control(Some(60_000), true).await;
}

/// An original infinite-TTL source remains eligible despite physical storage
/// delay. The elapsed-time boundary cannot invent a finite deadline for it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_storage_delay_preserves_infinite_native_input() {
    storage_delay_control(None, true).await;
}
