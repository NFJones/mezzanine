//! Fair bounded fanout selection and explicit acknowledgement.
//!
//! Selection reads the canonical queue and never advances delivery cursors;
//! only acknowledgement commits a recipient's monotonically advancing boundary.

use super::*;

impl MessageService {
    /// Selects ready subscriber batches using the default aggregate budget.
    pub fn fanout_ready(&mut self, now_ms: u64, limit_per_recipient: usize) -> Vec<FanoutBatch> {
        self.fanout_ready_with_budget(now_ms, limit_per_recipient, FanoutBudget::default())
    }

    /// Selects subscriber batches within one aggregate fair-work budget.
    pub fn fanout_ready_with_budget(
        &mut self,
        now_ms: u64,
        limit_per_recipient: usize,
        budget: FanoutBudget,
    ) -> Vec<FanoutBatch> {
        self.fanout_diagnostics.cycles = self.fanout_diagnostics.cycles.saturating_add(1);
        if budget.max_recipients == 0
            || budget.max_messages == 0
            || budget.max_payload_bytes == 0
            || limit_per_recipient == 0
            || self.subscription_order.is_empty()
        {
            return Vec::new();
        }

        let recipients = self.fanout_recipient_cycle(budget.max_recipients);
        let mut batches = Vec::new();
        let mut remaining_messages = budget.max_messages;
        let mut remaining_payload_bytes = budget.max_payload_bytes;
        for recipient in recipients.into_iter().take(budget.max_recipients) {
            self.fanout_after_recipient = Some(recipient.as_str().to_string());
            self.fanout_diagnostics.recipients_considered = self
                .fanout_diagnostics
                .recipients_considered
                .saturating_add(1);
            let Some(cursor) = self.subscriptions.get(&recipient).cloned() else {
                continue;
            };
            let Some(identity) = self.registered.get(&recipient) else {
                continue;
            };
            let selection = self.receive_after_indexed(
                &cursor,
                identity,
                now_ms,
                limit_per_recipient.min(remaining_messages),
                remaining_payload_bytes,
            );
            self.fanout_diagnostics.sequence_lookups = self
                .fanout_diagnostics
                .sequence_lookups
                .saturating_add(selection.sequence_lookups);
            if selection.batch.messages.is_empty() {
                continue;
            }
            let selected_messages = selection.batch.messages.len();
            remaining_messages = remaining_messages.saturating_sub(selected_messages);
            remaining_payload_bytes =
                remaining_payload_bytes.saturating_sub(selection.payload_bytes);
            self.fanout_diagnostics.messages_selected = self
                .fanout_diagnostics
                .messages_selected
                .saturating_add(u64::try_from(selected_messages).unwrap_or(u64::MAX));
            self.fanout_diagnostics.payload_bytes_selected = self
                .fanout_diagnostics
                .payload_bytes_selected
                .saturating_add(u64::try_from(selection.payload_bytes).unwrap_or(u64::MAX));
            batches.push(FanoutBatch {
                recipient,
                batch: selection.batch,
            });
            if remaining_messages == 0 || remaining_payload_bytes == 0 {
                break;
            }
        }
        batches
    }

    /// Returns cumulative bounded-fanout diagnostics.
    pub fn fanout_diagnostics(&self) -> MessageFanoutDiagnostics {
        self.fanout_diagnostics
    }

    /// Continues the stable recipient cycle without materializing all subscribers.
    fn fanout_recipient_cycle(&self, limit: usize) -> Vec<AgentId> {
        let mut recipients = Vec::with_capacity(limit.min(self.subscription_order.len()));
        if let Some(after) = self.fanout_after_recipient.as_ref() {
            recipients.extend(
                self.subscription_order
                    .range((
                        std::ops::Bound::Excluded(after.clone()),
                        std::ops::Bound::Unbounded,
                    ))
                    .take(limit)
                    .map(|(_, recipient)| recipient.clone()),
            );
            if recipients.len() < limit {
                recipients.extend(
                    self.subscription_order
                        .range(..=after.clone())
                        .take(limit - recipients.len())
                        .map(|(_, recipient)| recipient.clone()),
                );
            }
        } else {
            recipients.extend(self.subscription_order.values().take(limit).cloned());
        }
        recipients
    }

    /// Selects one recipient's ready batch without acknowledging delivery.
    pub fn fanout_ready_for(
        &self,
        recipient: &AgentId,
        now_ms: u64,
        limit: usize,
    ) -> Result<Option<FanoutBatch>> {
        let batch = self.receive_subscribed(recipient, now_ms, limit)?;
        if batch.messages.is_empty() {
            Ok(None)
        } else {
            Ok(Some(FanoutBatch {
                recipient: recipient.clone(),
                batch,
            }))
        }
    }

    /// Advances only the selected recipient's cursor after delivery commits.
    pub fn acknowledge_fanout_batch(&mut self, batch: &FanoutBatch) -> Result<DeliveryCursor> {
        let last_sequence = batch
            .batch
            .messages
            .last()
            .map(|message| message.sequence)
            .unwrap_or(batch.batch.cursor.last_sequence);
        self.advance_subscription(&batch.recipient, last_sequence)
    }
}
