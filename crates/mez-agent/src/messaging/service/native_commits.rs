//! Exact offered native commits at authoritative recipient retirement.
//!
//! The trusted native context owner supplies only occurrences it has already
//! canonically committed. This operation validates every frozen own offer
//! before changing any ACK, then ends the registration interval atomically.
//! It does not infer commit from a cumulative cursor, expire mail, disclose
//! payloads or release another recipient's shared obligation. The ended floor
//! makes individually committed rows coherent without a live cursor frontier.

use super::*;

impl MessageService {
    /// Ends an exact current registration after acknowledging only the supplied
    /// genuinely offered canonical native commits. The adapter owns proof of
    /// application commit; this API has no wire action. All claims validate
    /// before mutation. Foreign, missing, duplicate, unoffered or terminal
    /// claims fail; poisoned evidence returns InvalidState. Other recipient
    /// obligations remain pending for explicit terminal maintenance or ACK.
    pub fn retire_agent_identity_with_committed_occurrences(
        &mut self,
        recipient: &AgentId,
        committed: &[MessageSequence],
    ) -> Result<bool> {
        if committed.len() > self.pending_limits.per_recipient {
            return Err(MessageError::invalid_args(
                "native retirement commit batch exceeds recipient bound",
            ));
        }
        if committed.is_empty() {
            return Ok(self.retire_agent_identity(recipient));
        }
        let mut seen = HashSet::new();
        let mut guards = Vec::new();
        for sequence in committed {
            if !seen.insert(*sequence) {
                return Err(MessageError::invalid_args(
                    "native retirement commit is duplicated",
                ));
            }
            let row = self.pending.get(sequence).ok_or_else(|| {
                MessageError::invalid_args("native retirement occurrence is unavailable")
            })?;
            if !self.accepted_recipient(row, recipient) {
                return Err(MessageError::forbidden(
                    "native retirement recipient interval changed",
                ));
            }
            let index = row
                .recipient_bindings
                .iter()
                .position(|(id, _)| id == recipient)
                .ok_or_else(|| {
                    MessageError::forbidden("native retirement recipient is not accepted")
                })?;
            if super::offers::offered_at(row, recipient)?.is_none() {
                return Err(MessageError::invalid_args(
                    "native retirement commit was not offered",
                ));
            }
            let acknowledged = row.acknowledged.lock().map_err(|_| unavailable())?;
            let terminal = row.terminal.lock().map_err(|_| unavailable())?;
            if acknowledged.len() != row.recipient_bindings.len()
                || terminal.len() != acknowledged.len()
            {
                return Err(unavailable());
            }
            if terminal[index].is_some() {
                return Err(MessageError::invalid_args(
                    "terminal mail cannot become a native commit",
                ));
            }
            guards.push((acknowledged, index));
        }
        for (mut acknowledged, index) in guards {
            acknowledged[index] = true;
        }
        let retired = self.retire_agent_identity(recipient);
        self.release_completed_pending()?;
        Ok(retired)
    }
}

/// A poisoned reservation cannot authorize either positive commit or loss.
fn unavailable() -> MessageError {
    MessageError::invalid_state("native retirement commit evidence is unavailable")
}
