//! Framed MMP fanout writes at the product transport boundary.
//!
//! The agent crate selects delivery batches and owns cursor semantics. This
//! adapter converts those batches into framed bytes and writes them through a
//! product sink, acknowledging only successful writes.

use mez_agent::messaging::{MessageService, delivery_batch_json};
use mez_core::ids::AgentId;

use super::framing::encode_mmp_body;
use crate::error::Result;

/// Product sink for one framed MMP delivery batch.
pub trait MessageFanoutSink {
    /// Writes one complete frame for the selected recipient.
    fn send_frame(&mut self, recipient: &AgentId, frame: &[u8]) -> Result<()>;
}

/// Writes all currently ready subscriber batches and acknowledges each success.
#[cfg(test)]
pub fn flush_message_fanout(
    service: &mut MessageService,
    now_ms: u64,
    limit_per_recipient: usize,
    sink: &mut impl MessageFanoutSink,
) -> Result<usize> {
    let batches = service.fanout_ready(now_ms, limit_per_recipient);
    let mut sent = 0usize;
    for batch in batches {
        let body = delivery_batch_json(&batch.batch);
        let frame = encode_mmp_body(&body);
        sink.send_frame(&batch.recipient, &frame)?;
        service.acknowledge_fanout_batch(&batch)?;
        sent += batch.batch.messages.len();
    }
    Ok(sent)
}

/// Writes the ready batch for one recipient and acknowledges a successful write.
#[cfg(test)]
pub fn flush_message_fanout_for(
    service: &mut MessageService,
    recipient: &AgentId,
    now_ms: u64,
    limit: usize,
    sink: &mut impl MessageFanoutSink,
) -> Result<usize> {
    let Some(batch) = service.fanout_ready_for(recipient, now_ms, limit)? else {
        return Ok(0);
    };
    let body = delivery_batch_json(&batch.batch);
    let frame = encode_mmp_body(&body);
    sink.send_frame(&batch.recipient, &frame)?;
    service.acknowledge_fanout_batch(&batch)?;
    Ok(batch.batch.messages.len())
}
