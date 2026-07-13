//! Product prompt adapter for mux-owned terminal input decoding.

use crate::error::Result;
use mez_mux::readline::{ReadlineDecodedInput, ReadlineEdit, ReadlineOutcome};

use super::types::{ReadlineInputDecoder, ReadlinePrompt};

impl ReadlineInputDecoder {
    /// Create a decoder with no buffered partial sequence.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of bytes retained because they might complete on a later read.
    pub fn pending_len(&self) -> usize {
        self.inner.pending_len()
    }

    /// Converts a pending standalone Escape key into prompt cancellation.
    ///
    /// Escape-prefixed terminal sequences are buffered so split arrow-key and
    /// function-key reads can still complete. Interactive prompt loops call this
    /// after a readiness poll with no new input so a literal Escape key can still
    /// leave the prompt instead of waiting forever for a sequence suffix.
    pub fn flush_pending_escape_as_cancel(&mut self) -> Option<ReadlineOutcome> {
        self.inner
            .flush_pending_escape()
            .then_some(ReadlineOutcome::Cancelled)
    }

    /// Apply a terminal byte batch to a prompt, preserving incomplete input.
    pub fn apply_to_prompt(
        &mut self,
        prompt: &mut ReadlinePrompt,
        input: &[u8],
    ) -> Result<Vec<ReadlineOutcome>> {
        let mut outcomes = Vec::new();
        for decoded in self.inner.decode(input)? {
            match decoded {
                ReadlineDecodedInput::Sequence(sequence) => {
                    outcomes.push(prompt.apply_terminal_input(&sequence)?);
                }
                ReadlineDecodedInput::BracketedPaste(text) => {
                    prompt.selector = None;
                    outcomes.push(prompt.buffer.apply(ReadlineEdit::InsertText(text)));
                }
            }
        }
        Ok(outcomes)
    }
}
