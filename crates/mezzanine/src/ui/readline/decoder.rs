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
    #[cfg(test)]
    pub fn pending_len(&self) -> usize {
        self.inner.pending_len()
    }

    /// Decodes complete terminal input items while preserving incomplete input.
    pub fn decode(&mut self, input: &[u8]) -> Result<Vec<ReadlineDecodedInput>> {
        Ok(self.inner.decode(input)?)
    }

    /// Applies one decoded terminal input item to a prompt.
    pub fn apply_decoded_to_prompt(
        prompt: &mut ReadlinePrompt,
        decoded: ReadlineDecodedInput,
    ) -> Result<ReadlineOutcome> {
        match decoded {
            ReadlineDecodedInput::Sequence(sequence) => prompt.apply_terminal_input(&sequence),
            ReadlineDecodedInput::BracketedPaste(text) => {
                prompt.selector = None;
                Ok(prompt.buffer.apply(ReadlineEdit::InsertText(text)))
            }
        }
    }

    /// Apply a terminal byte batch to a prompt, preserving incomplete input.
    pub fn apply_to_prompt(
        &mut self,
        prompt: &mut ReadlinePrompt,
        input: &[u8],
    ) -> Result<Vec<ReadlineOutcome>> {
        let mut outcomes = Vec::new();
        for decoded in self.decode(input)? {
            outcomes.push(Self::apply_decoded_to_prompt(prompt, decoded)?);
        }
        Ok(outcomes)
    }
}
