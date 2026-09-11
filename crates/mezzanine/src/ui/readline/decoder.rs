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

    /// Reports whether a rejected paste payload is still being discarded.
    ///
    /// A prompt surface uses this to explain why input is being ignored until
    /// the closing delimiter arrives instead of silently swallowing keystrokes.
    pub fn bracketed_paste_resynchronization_pending(&self) -> bool {
        self.inner.bracketed_paste_resynchronization_pending()
    }

    /// Takes the one-shot report that a decode rejected a paste payload.
    ///
    /// Prompt paths that apply a whole batch through one call use this to report
    /// a rejection whose closing delimiter arrived in the same read.
    pub fn take_bracketed_paste_rejection(&mut self) -> bool {
        self.inner.take_bracketed_paste_rejection()
    }

    /// Drops retained paste framing after a trusted prompt reset.
    ///
    /// Only a prompt lifecycle owner may call this: the state it clears exists
    /// because attacker-influenced bytes were retained, so those bytes are
    /// discarded rather than decoded or replayed, and ordinary decoding resumes
    /// immediately. Returns whether anything was dropped.
    pub fn abandon_bracketed_paste_framing(&mut self) -> bool {
        self.inner.abandon_bracketed_paste_framing()
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
                Ok(prompt.buffer.apply(ReadlineEdit::InsertPaste(text)))
            }
            ReadlineDecodedInput::BracketedPasteRejected(_) => Ok(ReadlineOutcome::Noop),
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
