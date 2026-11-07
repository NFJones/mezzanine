//! Original lifecycle rows are qualified separately from model occurrence/cost
//! transport. Their exact reservations enter canonical import only after source
//! custody passes; no control becomes a block, user task or persistence ACK.

use super::*;

impl RuntimeContextRecords {
    pub(in crate::runtime::control) fn qualify_execution_controls(
        &mut self,
        snapshot: Option<&crate::storage::transcript::replay_epoch::ReplayEpochSnapshot>,
        conversation: &str,
        rows: &[TranscriptEntry],
    ) -> Result<()> {
        if self.control_sequences.is_empty() {
            return Ok(());
        }
        if self
            .control_sequences
            .iter()
            .any(|sequence| !self.row_origins.contains_key(sequence))
        {
            return Err(invalid());
        }
        let controls = crate::storage::transcript::replay_epoch::qualify_execution_controls(
            snapshot,
            conversation,
            rows,
        )?;
        if controls.len() != self.control_sequences.len() {
            return Err(invalid());
        }
        for control in &controls {
            for (id, sequence) in control.members() {
                let sequence = mez_agent::ContextEventSequence::new(*sequence)?;
                if self
                    .source_sequences
                    .insert(*id, sequence)
                    .is_some_and(|previous| previous != sequence)
                {
                    return Err(invalid());
                }
            }
        }
        self.execution_controls = controls;
        self.controls_qualified = true;
        Ok(())
    }
}
