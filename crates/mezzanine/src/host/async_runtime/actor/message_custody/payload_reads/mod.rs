//! Bounded cold-read continuations for actual recipient demand.
//!
//! A cold fanout request keeps only its original registration, clock and reply;
//! it does not pollute the actor queue with payload copies. The existing root
//! owns the actual read. After join/publication this owner constructs the reply
//! while bytes are still resident, preventing competing prefetch from evicting
//! them before the requesting consumer can obtain its batch. Cancelled queued
//! readers are discarded; cancelled active readers cannot detach physical I/O.

use mez_agent::messaging::{
    MESSAGE_PAYLOAD_DEMAND_BYTES, MESSAGE_PAYLOAD_DEMAND_RECIPIENTS, MessageSenderRegistration,
    delivery_batch_json,
};
use mez_core::ids::AgentId;

use super::*;
use crate::host::async_runtime::AsyncMessageFanout;
use crate::protocol::message::encode_mmp_body;

/// Payload-free original requester, fenced against retirement/rebinding/restore.
pub(super) struct PayloadRead {
    registration: MessageSenderRegistration,
    now_ms: u64,
    queued_at: Option<Instant>,
    limit: usize,
    reply: oneshot::Sender<Result<Option<AsyncMessageFanout>>>,
}

impl PayloadRead {
    /// Charges descriptor overhead and its one owned public recipient string.
    fn metadata_bytes(&self) -> usize {
        128usize.saturating_add(self.registration.agent_id().as_str().len())
    }

    /// Delayed I/O cannot renew the original TTL or use a stale selection clock.
    fn observation_now_ms(&self) -> u64 {
        self.now_ms
            .saturating_add(self.queued_at.map_or(0, |started| {
                u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
            }))
    }
}

impl AsyncRuntimeSessionActor {
    /// Serves hot/no-mail requests immediately; real cold demand acquires one
    /// bounded continuation whose reply is completed after the actual reload.
    pub(in crate::host::async_runtime::actor) fn message_fanout_request(
        &mut self,
        recipient: AgentId,
        now_ms: u64,
        limit: usize,
        reply: oneshot::Sender<Result<Option<AsyncMessageFanout>>>,
    ) {
        if reply.is_closed() {
            return;
        }
        let service = self.service.message_service();
        let needed = match service.payload_needed_for(&recipient, now_ms) {
            Ok(needed) => needed,
            Err(error) => {
                let _ = reply.send(Err(error.into()));
                return;
            }
        };
        if needed.is_none() || limit == 0 || self.message_custody.is_none() {
            self.reply_message_fanout(recipient, now_ms, limit, reply);
            return;
        }
        let Some(registration) = service.capture_sender_registration(&recipient) else {
            let _ = reply.send(Err(MessageError::forbidden(
                "MMP delivery registration is unavailable",
            )
            .into()));
            return;
        };
        let mut read = PayloadRead {
            registration,
            now_ms,
            queued_at: None,
            limit,
            reply,
        };
        let Some(owner) = self.message_custody.as_mut() else {
            return;
        };
        if owner.shutdown.is_some() {
            let _ = read.reply.send(Ok(None));
            return;
        }
        let bytes = owner
            .payload_reads
            .iter()
            .fold(read.metadata_bytes(), |sum, queued| {
                sum.saturating_add(queued.metadata_bytes())
            });
        if owner.payload_reads.len() >= MESSAGE_PAYLOAD_DEMAND_RECIPIENTS
            || bytes > MESSAGE_PAYLOAD_DEMAND_BYTES
        {
            // The normal connection loop retries on this bounded notification;
            // no unbounded waiter registry or payload side queue is created.
            owner.payload_retry_at = Some(Instant::now() + std::time::Duration::from_secs(1));
            let _ = read.reply.send(Ok(None));
            return;
        }
        read.queued_at = Some(Instant::now());
        owner.payload_reads.push_back(read);
    }

    /// Completes only the original registration's read, with its original clock
    /// plus elapsed I/O time. An abandoned observer creates no delivery offer.
    pub(super) fn complete_payload_read(&self, read: PayloadRead) {
        if read.reply.is_closed() {
            return;
        }
        let service = self.service.message_service();
        if !service.sender_registration_is_current(&read.registration) {
            let _ = read.reply.send(Ok(None));
            return;
        }
        let recipient = read.registration.agent_id().clone();
        let now_ms = read.observation_now_ms();
        self.reply_message_fanout(recipient, now_ms, read.limit, read.reply);
    }

    /// Preserves the ordinary hot/no-mail response and error contracts.
    fn reply_message_fanout(
        &self,
        recipient: AgentId,
        now_ms: u64,
        limit: usize,
        reply: oneshot::Sender<Result<Option<AsyncMessageFanout>>>,
    ) {
        let result = self
            .service
            .message_service()
            .fanout_ready_for(&recipient, now_ms, limit)
            .map(|fanout| {
                fanout.map(|batch| {
                    let frame = encode_mmp_body(&delivery_batch_json(&batch.batch));
                    let messages = batch.batch.messages.len();
                    AsyncMessageFanout {
                        recipient,
                        frame,
                        messages,
                        batch,
                    }
                })
            })
            .map_err(Into::into);
        let _ = reply.send(result);
    }

    /// Gives one pending reader a ready batch or the sole physical lane. Cache
    /// refusals rotate through the bounded queue so a smaller healthy recipient
    /// may progress; actual issued reads retain their original continuation.
    pub(super) fn pump_message_payload_reads(&mut self) {
        let Some(owner) = self.message_custody.as_mut() else {
            return;
        };
        if owner.shutdown.is_some() {
            while let Some(read) = owner.payload_reads.pop_front() {
                let _ = read.reply.send(Ok(None));
            }
            return;
        }
        // A queued obsolete reader owns no physical I/O. Retire it before a
        // busy end/terminal lane or admission backoff can hold its reply and
        // descriptor quota. Preserve the order of all still-current readers;
        // an actually issued read stays with its original pending continuation.
        let service = self.service.message_service();
        for _ in 0..owner.payload_reads.len() {
            let Some(read) = owner.payload_reads.pop_front() else {
                break;
            };
            if read.reply.is_closed() {
                continue;
            }
            if !service.sender_registration_is_current(&read.registration) {
                let _ = read.reply.send(Ok(None));
            } else {
                owner.payload_reads.push_back(read);
            }
        }
        if owner.payload_reads.is_empty() {
            owner.payload_retry_at = None;
        }
        if owner.pending.is_some()
            || owner.wake.is_some()
            || owner.failure.is_some()
            || owner.payload_retry_at.is_some_and(|at| at > Instant::now())
        {
            return;
        }
        owner.payload_retry_at = None;
        let count = owner.payload_reads.len();
        for _ in 0..count {
            let read = match self
                .message_custody
                .as_mut()
                .and_then(|owner| owner.payload_reads.pop_front())
            {
                Some(read) => read,
                None => break,
            };
            if read.reply.is_closed() {
                continue;
            }
            if !self
                .service
                .message_service()
                .sender_registration_is_current(&read.registration)
            {
                self.complete_payload_read(read);
                continue;
            }
            let needed = self
                .service
                .message_service()
                .payload_needed_for(read.registration.agent_id(), read.observation_now_ms());
            match needed {
                Ok(None) => {
                    self.complete_payload_read(read);
                    return;
                }
                Err(error) => {
                    let _ = read.reply.send(Err(error.into()));
                    continue;
                }
                Ok(Some(sequence)) => {
                    let Some(owner) = self.message_custody.as_mut() else {
                        return;
                    };
                    match owner
                        .root
                        .start_payload_load(self.service.message_service_mut(), sequence)
                    {
                        Ok(true) => {
                            owner.pending = Some(Pending::PayloadLoad(read));
                            owner.wake = Some(Wake::Observe);
                            return;
                        }
                        Ok(false) => owner.payload_reads.push_back(read),
                        Err(error) => {
                            let _ = read.reply.send(Err(error));
                        }
                    }
                }
            }
        }
        if let Some(owner) = self.message_custody.as_mut()
            && !owner.payload_reads.is_empty()
        {
            owner.payload_retry_at = Some(Instant::now() + std::time::Duration::from_secs(1));
        }
    }
}

#[cfg(test)]
mod tests;
