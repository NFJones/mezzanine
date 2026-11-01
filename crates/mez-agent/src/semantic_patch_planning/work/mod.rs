//! Explicit cooperative CPU work ownership for semantic patch preparation.
//!
//! Native callers supply the original action fence and a finite byte/visit
//! allowance. Pane callers use the same algorithms with an unlimited control.
//! No control grants filesystem authority. A first terminal failure is sticky:
//! internal search helpers may abandon candidates, but public planning owners
//! must check the control before returning diagnostics, changes or partial plans.
//! Byte scans and copies check between bounded quanta, including long lines;
//! charging an entire operation in advance is not a cancellation checkpoint.

use std::cell::{Cell, RefCell};

use crate::native_action::{NativePatchOperation, PatchEffect};
use crate::semantic_patch::{
    MezPatch, MezPatchHunk, MezPatchHunkLine, MezPatchOperation, SemanticPatchPlanningError,
    SemanticPatchPlanningResult as Result,
};

/// Maximum charged byte/visit quantum before another original-fence check.
pub const PATCH_WORK_QUANTUM: usize = 64 * 1024;
/// Cumulative native byte/visit allowance, independent of elapsed time.
pub const NATIVE_PATCH_WORK_UNITS: usize = 1024 * 1024 * 1024;
/// Maximum text lines per native file, bounding String/vector bookkeeping too.
pub const NATIVE_PATCH_TEXT_LINES: usize = 262_144;

/// Caller-owned original work admission, without globals or renewed deadlines.
pub struct PatchWorkControl<'a> {
    /// None selects existing pane behavior without a new native resource policy.
    checkpoint: Option<&'a dyn Fn() -> Result<()>>,
    /// Remaining total work; irrelevant for the unlimited pane adapter.
    remaining: Cell<usize>,
    /// Bytes/visits until another original-fence check is required.
    quantum: Cell<usize>,
    /// First failure, retained even if a helper abandons its candidate search.
    failure: RefCell<Option<SemanticPatchPlanningError>>,
}

impl<'a> PatchWorkControl<'a> {
    /// Uses a caller's unchanged native action fence and finite work allowance.
    pub fn native(checkpoint: &'a dyn Fn() -> Result<()>) -> Self {
        Self {
            checkpoint: Some(checkpoint),
            remaining: Cell::new(NATIVE_PATCH_WORK_UNITS),
            quantum: Cell::new(0),
            failure: RefCell::new(None),
        }
    }

    /// Preserves the pane adapter's existing matching policy and resource rules.
    pub(super) fn unlimited() -> Self {
        Self {
            checkpoint: None,
            remaining: Cell::new(0),
            quantum: Cell::new(0),
            failure: RefCell::new(None),
        }
    }

    /// Records only the first terminal cause; subsequent work cannot revive it.
    pub(super) fn stop(&self, error: SemanticPatchPlanningError) {
        let mut failure = self.failure.borrow_mut();
        if failure.is_none() {
            *failure = Some(error);
        }
    }

    /// Reports sticky failure without calling or renewing the original fence.
    pub(super) fn running(&self) -> bool {
        self.failure.borrow().is_none()
    }

    /// Returns the first failure, or checks the original action right now.
    pub fn check(&self) -> Result<()> {
        if let Some(error) = self.failure.borrow().as_ref() {
            return Err(error.clone());
        }
        if let Some(checkpoint) = self.checkpoint
            && let Err(error) = checkpoint()
        {
            self.stop(error.clone());
            return Err(error);
        }
        Ok(())
    }

    /// Charges bounded actual work immediately before performing that work.
    /// False means callers must stop scanning, not try another matching mode.
    pub(super) fn spend(&self, units: usize) -> bool {
        if !self.running() {
            return false;
        }
        if self.checkpoint.is_none() {
            return true;
        }
        let Some(remaining) = self.remaining.get().checked_sub(units) else {
            self.stop(SemanticPatchPlanningError::invalid_args(
                "apply_patch: native computational work budget exceeded",
            ));
            return false;
        };
        self.remaining.set(remaining);
        if units > PATCH_WORK_QUANTUM {
            self.stop(SemanticPatchPlanningError::invalid_args(
                "apply_patch: unbounded computational work quantum",
            ));
            return false;
        }
        if units > self.quantum.get() {
            if self.check().is_err() {
                return false;
            }
            self.quantum.set(PATCH_WORK_QUANTUM);
        }
        self.quantum.set(self.quantum.get() - units);
        true
    }

    /// Replaces an owned working range with cooperative String-owner transfers.
    /// Native tail movement is charged by actual owner size, not just new text.
    /// Failure may empty the disposable working buffer; retained source snapshots
    /// remain separate. Dropping abandoned buffers is mandatory bounded cleanup,
    /// not successful work or a guarantee of interruptible allocator operations.
    pub(super) fn replace_lines(
        &self,
        lines: &mut Vec<String>,
        range: std::ops::Range<usize>,
        replacement: Vec<String>,
    ) -> Result<()> {
        self.check()?;
        if range.start > range.end || range.end > lines.len() {
            return Err(SemanticPatchPlanningError::invalid_args(
                "apply_patch: invalid working line range",
            ));
        }
        if self.checkpoint.is_none() {
            lines.splice(range, replacement);
            return Ok(());
        }
        let next_length = lines.len() - range.len() + replacement.len();
        if next_length > 0 && !self.line_allowed(next_length - 1) {
            self.check()?;
        }
        let mut original = std::mem::take(lines).into_iter();
        let mut prepared = Vec::with_capacity(next_length);
        for line in original.by_ref().take(range.start) {
            if !self.spend(std::mem::size_of::<String>()) {
                self.check()?;
            }
            prepared.push(line);
        }
        for line in replacement {
            if !self.spend(std::mem::size_of::<String>()) {
                self.check()?;
            }
            prepared.push(line);
        }
        for line in original.by_ref().take(range.len()) {
            if !self.spend(std::mem::size_of::<String>()) {
                self.check()?;
            }
            drop(line);
        }
        for line in original {
            if !self.spend(std::mem::size_of::<String>()) {
                self.check()?;
            }
            prepared.push(line);
        }
        self.check()?;
        *lines = prepared;
        Ok(())
    }

    /// Copies the exact retained descriptor through controlled semantic fields.
    /// Source text and parsed duplicates share the same remaining action budget.
    pub(super) fn copy_native_operation(
        &self,
        operation: &NativePatchOperation,
    ) -> Result<NativePatchOperation> {
        self.check()?;
        let exact_patch = self.copy_text(&operation.exact_patch)?;
        let mut operations = Vec::new();
        for operation in &operation.patch.operations {
            operations.push(self.copy_parsed_operation(operation)?);
        }
        let effects = operation
            .effects
            .iter()
            .map(|effect| self.copy_effect(effect))
            .collect::<Result<_>>()?;
        Ok(NativePatchOperation {
            exact_patch,
            patch: MezPatch { operations },
            effects,
        })
    }

    /// Copies one semantic intent's paths without unbudgeted deep String clones.
    pub(super) fn copy_effect(&self, effect: &PatchEffect) -> Result<PatchEffect> {
        if !self.spend(std::mem::size_of::<PatchEffect>()) {
            self.check()?;
        }
        Ok(match effect {
            PatchEffect::Create(path) => PatchEffect::Create(self.copy_text(path)?),
            PatchEffect::Update(path) => PatchEffect::Update(self.copy_text(path)?),
            PatchEffect::Delete(path) => PatchEffect::Delete(self.copy_text(path)?),
            PatchEffect::Move {
                source,
                destination,
            } => PatchEffect::Move {
                source: self.copy_text(source)?,
                destination: self.copy_text(destination)?,
            },
        })
    }

    /// Copies one parsed operation's retained text, checking during long fields.
    /// Failed copies never expose a partially owned semantic operation.
    pub(super) fn copy_parsed_operation(
        &self,
        operation: &MezPatchOperation,
    ) -> Result<MezPatchOperation> {
        if !self.spend(std::mem::size_of::<MezPatchOperation>()) {
            self.check()?;
        }
        Ok(match operation {
            MezPatchOperation::Add { path, content } => MezPatchOperation::Add {
                path: self.copy_text(path)?,
                content: self.copy_lines(content)?,
            },
            MezPatchOperation::Delete { path } => MezPatchOperation::Delete {
                path: self.copy_text(path)?,
            },
            MezPatchOperation::Update {
                path,
                move_to,
                hunks,
                trailing_newline,
            } => {
                let mut copied = Vec::new();
                for hunk in hunks {
                    if !self.spend(std::mem::size_of::<MezPatchHunk>()) {
                        self.check()?;
                    }
                    let anchors = self.copy_lines(&hunk.anchors)?;
                    let old = self.copy_lines(&hunk.old)?;
                    let new = self.copy_lines(&hunk.new)?;
                    let mut lines = Vec::new();
                    for line in &hunk.lines {
                        if !self.spend(std::mem::size_of::<MezPatchHunkLine>()) {
                            self.check()?;
                        }
                        lines.push(match line {
                            MezPatchHunkLine::Add(text) => {
                                MezPatchHunkLine::Add(self.copy_text(text)?)
                            }
                            MezPatchHunkLine::Remove(text) => {
                                MezPatchHunkLine::Remove(self.copy_text(text)?)
                            }
                            MezPatchHunkLine::Context(text) => {
                                MezPatchHunkLine::Context(self.copy_text(text)?)
                            }
                        });
                    }
                    copied.push(MezPatchHunk {
                        anchors,
                        old,
                        new,
                        lines,
                        range_hint: hunk.range_hint,
                        replace_whole_file: hunk.replace_whole_file,
                    });
                }
                MezPatchOperation::Update {
                    path: self.copy_text(path)?,
                    move_to: move_to
                        .as_deref()
                        .map(|path| self.copy_text(path))
                        .transpose()?,
                    hunks: copied,
                    trailing_newline: *trailing_newline,
                }
            }
        })
    }

    /// Copies bytes in actual bounded pieces, preserving exact raw preimages.
    pub fn copy_bytes(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        self.check()?;
        let mut copied = Vec::with_capacity(bytes.len());
        for chunk in bytes.chunks(PATCH_WORK_QUANTUM) {
            if !self.spend(chunk.len()) {
                self.check()?;
            }
            copied.extend_from_slice(chunk);
        }
        self.check()?;
        Ok(copied)
    }

    /// Copies valid UTF-8 with byte checks; no lossy or normalized conversion.
    pub(super) fn copy_text(&self, text: &str) -> Result<String> {
        self.decode_text(text.as_bytes())
    }

    /// Clones bounded text-line ownership, checking before each new owner.
    pub(super) fn copy_lines(&self, source: &[String]) -> Result<Vec<String>> {
        self.check()?;
        let mut lines = Vec::new();
        for line in source {
            if !self.line_allowed(lines.len()) {
                self.check()?;
            }
            lines.push(self.copy_text(line)?);
        }
        Ok(lines)
    }

    /// Trims only the leading Unicode whitespace, preserving prefix semantics.
    pub(super) fn trim_start<'b>(&self, text: &'b str) -> &'b str {
        if self.checkpoint.is_none() {
            return text.trim_start();
        }
        for (offset, character) in text.char_indices() {
            if !self.spend(character.len_utf8()) {
                return "";
            }
            if !character.is_whitespace() {
                return &text[offset..];
            }
        }
        ""
    }

    /// Compares exact bytes in chunks rather than concealing a whole long line.
    pub(super) fn equal(&self, left: &str, right: &str) -> bool {
        if !self.spend(1) || left.len() != right.len() {
            return false;
        }
        left.as_bytes()
            .chunks(PATCH_WORK_QUANTUM)
            .zip(right.as_bytes().chunks(PATCH_WORK_QUANTUM))
            .all(|(left, right)| self.spend(left.len().max(1)) && left == right)
    }

    /// Returns Unicode-trimmed text with cooperative scans at both boundaries.
    pub(super) fn trim<'b>(&self, text: &'b str, start: bool) -> &'b str {
        if self.checkpoint.is_none() {
            return if start { text.trim() } else { text.trim_end() };
        }
        let mut begin = 0;
        let mut end = text.len();
        if start {
            for (offset, character) in text.char_indices() {
                if !self.spend(character.len_utf8()) {
                    return "";
                }
                if !character.is_whitespace() {
                    begin = offset;
                    break;
                }
                begin = offset + character.len_utf8();
            }
        }
        for (offset, character) in text[begin..].char_indices().rev() {
            if !self.spend(character.len_utf8()) {
                return "";
            }
            if !character.is_whitespace() {
                end = begin + offset + character.len_utf8();
                break;
            }
            end = begin + offset;
        }
        &text[begin..end]
    }

    /// Finds a literal byte substring with linear KMP scans and bounded checks.
    /// Valid UTF-8 strings have the same substring relation as their byte slices.
    pub(super) fn contains(&self, text: &str, needle: &str) -> bool {
        if self.checkpoint.is_none() {
            return text.contains(needle);
        }
        if !self.spend(1) {
            return false;
        }
        if needle.is_empty() {
            return self.running();
        }
        let needle = needle.as_bytes();
        if needle.len() > text.len() {
            return false;
        }
        let mut prefix = Vec::with_capacity(needle.len());
        prefix.push(0);
        let mut matched = 0;
        for index in 1..needle.len() {
            if !self.spend(1) {
                return false;
            }
            while matched > 0 && needle[index] != needle[matched] {
                if !self.spend(1) {
                    return false;
                }
                matched = prefix[matched - 1];
            }
            if needle[index] == needle[matched] {
                matched += 1;
            }
            prefix.push(matched);
        }
        matched = 0;
        for byte in text.bytes() {
            if !self.spend(1) {
                return false;
            }
            while matched > 0 && byte != needle[matched] {
                if !self.spend(1) {
                    return false;
                }
                matched = prefix[matched - 1];
            }
            if byte == needle[matched] {
                matched += 1;
            }
            if matched == needle.len() {
                return true;
            }
        }
        false
    }

    /// Decodes UTF-8 in bounded chunks, carrying incomplete boundary characters.
    pub(super) fn decode_text(&self, bytes: &[u8]) -> Result<String> {
        self.check()?;
        let mut text = String::with_capacity(bytes.len());
        let mut offset = 0;
        while offset < bytes.len() {
            let mut end = bytes.len().min(offset + PATCH_WORK_QUANTUM);
            if !self.spend(end - offset) {
                self.check()?;
            }
            let fragment = match std::str::from_utf8(&bytes[offset..end]) {
                Ok(fragment) => fragment,
                Err(error) if error.error_len().is_none() && end < bytes.len() => {
                    end = offset + error.valid_up_to();
                    std::str::from_utf8(&bytes[offset..end]).map_err(|_| {
                        SemanticPatchPlanningError::invalid_args("apply_patch: invalid UTF-8")
                    })?
                }
                Err(_) => {
                    return Err(SemanticPatchPlanningError::invalid_args(
                        "apply_patch: invalid UTF-8",
                    ));
                }
            };
            text.push_str(fragment);
            offset = end;
        }
        self.check()?;
        Ok(text)
    }

    /// Splits the same LF/CRLF lines as str::lines, with bounded actual scans.
    pub(super) fn text_lines(&self, text: &str) -> Result<Vec<String>> {
        self.check()?;
        let mut lines = Vec::new();
        let mut start = 0;
        for (index, byte) in text.bytes().enumerate() {
            if !self.spend(1) {
                self.check()?;
            }
            if byte == b'\n' {
                if !self.line_allowed(lines.len()) {
                    self.check()?;
                }
                let line = text[start..index]
                    .strip_suffix('\r')
                    .unwrap_or(&text[start..index]);
                lines.push(self.copy_text(line)?);
                start = index + 1;
            }
        }
        if start < text.len() {
            if !self.line_allowed(lines.len()) {
                self.check()?;
            }
            lines.push(self.copy_text(&text[start..])?);
        }
        self.check()?;
        Ok(lines)
    }

    /// Extends authoritative output without an unchecked whole-file copy.
    pub(super) fn append_bytes(&self, output: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
        if self.checkpoint.is_some()
            && output.len().saturating_add(bytes.len()) > super::native::NATIVE_PATCH_FILE_BYTES
        {
            self.stop(SemanticPatchPlanningError::invalid_args(
                "apply_patch: native file byte budget exceeded",
            ));
            self.check()?;
        }
        for chunk in bytes.chunks(PATCH_WORK_QUANTUM) {
            if !self.spend(chunk.len()) {
                self.check()?;
            }
            output.extend_from_slice(chunk);
        }
        Ok(())
    }

    /// Bounds text bookkeeping before allocating another native line owner.
    pub(super) fn line_allowed(&self, count: usize) -> bool {
        if self.checkpoint.is_some() && count >= NATIVE_PATCH_TEXT_LINES {
            self.stop(SemanticPatchPlanningError::invalid_args(
                "apply_patch: native text line budget exceeded",
            ));
            return false;
        }
        self.spend(std::mem::size_of::<String>())
    }
}

#[cfg(test)]
mod tests;
