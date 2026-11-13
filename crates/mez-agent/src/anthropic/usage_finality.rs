//! Final-counter evidence for Messages, not an expense ledger or output gate.
//! Start input is cache-exclusive and requires all three numeric components.
//! Output must come from a stop-reason delta, never start/prefix presence.

use super::{anthropic_usage_from_value, anthropic_usage_presence};
use crate::{ModelTokenUsage, ModelTokenUsagePresence};
use serde_json::Value;

type Report = (ModelTokenUsage, ModelTokenUsagePresence);

/// Missing cache components cannot be fabricated as zero when qualifying the
/// inclusive input total. Saturated sums retain expense but not completeness.
pub fn anthropic_final_usage_presence(value: Option<&Value>) -> ModelTokenUsagePresence {
    let mut presence = anthropic_usage_presence(value);
    presence.input = value.is_some_and(|v| {
        v["input_tokens"]
            .as_u64()
            .zip(v["cache_read_input_tokens"].as_u64())
            .zip(v["cache_creation_input_tokens"].as_u64())
            .and_then(|((input, read), write)| input.checked_add(read)?.checked_add(write))
            .is_some()
    });
    presence
}

#[derive(Debug, Default)]
pub(super) struct MessagesUsage {
    start: Option<Report>,
    delta: Option<Report>,
    terminal: Option<Report>,
    // Exact raw component evidence; never subtract from a saturated total.
    components: [Option<u64>; 3],
    input_changed: bool,
    stopped: bool,
    degraded: bool,
    conflicted: bool,
    regressed: bool,
}

impl MessagesUsage {
    /// Returns true to preserve the previous normalized expense on a numeric
    /// regression. This happens before any fallible output/callback suffix.
    pub(super) fn observe(
        &mut self,
        value: &Value,
        name: Option<&str>,
        previous: ModelTokenUsage,
    ) -> bool {
        let kind = if name == Some("error") {
            Some("error")
        } else {
            value["type"].as_str().or(name)
        };
        let raw = if kind == Some("message_start") {
            value.pointer("/message/usage")
        } else if matches!(kind, Some("message_delta" | "error")) {
            value.get("usage")
        } else {
            None
        };
        let candidate = anthropic_usage_from_value(raw);
        let presence = anthropic_final_usage_presence(raw);
        let primary = anthropic_usage_presence(raw);
        let components = [
            "input_tokens",
            "cache_read_input_tokens",
            "cache_creation_input_tokens",
        ]
        .map(|key| raw.and_then(|v| v[key].as_u64()));
        let input_changed = components
            .iter()
            .zip(self.components)
            .any(|(new, old)| new.is_some() && *new != old);
        let terminal_delta = kind == Some("message_delta")
            && value
                .pointer("/delta/stop_reason")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty());
        let decreases = (primary.output && candidate.output_tokens < previous.output_tokens)
            || components
                .iter()
                .zip(self.components)
                .any(|(new, old)| new.zip(old).is_some_and(|(a, b)| a < b));
        let regressed =
            decreases && (terminal_delta || self.delta.is_some() || self.terminal.is_some());
        self.regressed |= regressed;
        if self.stopped && matches!(kind, Some("message_start" | "message_delta")) {
            self.degraded = true;
            self.conflicted |= self.terminal.is_some();
        }
        self.stopped |= kind == Some("message_stop");
        if self.delta.is_some() && raw.is_some() {
            self.conflicted |= input_changed
                || (primary.output && candidate.output_tokens != previous.output_tokens);
        }
        if self.start.is_some() {
            self.input_changed |= input_changed;
        }
        if !regressed && matches!(kind, Some("message_start" | "message_delta" | "error")) {
            for (previous, current) in self.components.iter_mut().zip(components) {
                *previous = current.or(*previous);
            }
        }
        match kind {
            Some("message_start") => {
                if self.start.is_some()
                    || self.delta.is_some()
                    || self.terminal.is_some()
                    || value.pointer("/message/type").and_then(Value::as_str) != Some("message")
                    || value.pointer("/message/role").and_then(Value::as_str) != Some("assistant")
                {
                    self.degraded = true;
                    self.conflicted |= self.terminal.is_some();
                } else {
                    self.start = Some((candidate, presence));
                }
            }
            Some("message_delta") if terminal_delta => {
                let report = (candidate, presence);
                if let Some(original) = self.delta {
                    self.conflicted |= original != report;
                } else {
                    self.delta = Some(report);
                }
            }
            Some("message_stop") if !self.degraded && !self.regressed && !self.conflicted => {
                if let (Some(start), Some(delta)) = (self.start, self.delta) {
                    // Partial changed input invalidates only that dimension.
                    // A complete delta may supply its own inclusive input;
                    // otherwise only unchanged complete start fields qualify.
                    let input = if delta.1.input { delta } else { start };
                    let usage = ModelTokenUsage {
                        output_tokens: delta.0.output_tokens,
                        ..input.0
                    };
                    self.terminal = Some((
                        usage,
                        ModelTokenUsagePresence {
                            input: input.1.input && (delta.1.input || !self.input_changed),
                            output: delta.1.output,
                        },
                    ));
                }
            }
            Some("error") => {
                self.degraded = true;
                self.conflicted |= self.delta.is_some();
            }
            _ => {}
        }
        regressed
    }

    pub(super) fn terminal_usage(&self) -> Option<Report> {
        (!self.degraded && !self.conflicted && !self.regressed)
            .then_some(self.terminal)
            .flatten()
    }

    pub(super) fn regressed(&self) -> bool {
        self.regressed
    }

    pub(super) fn conflicting_report(&self) -> bool {
        self.conflicted
    }
}

#[cfg(test)]
mod tests;
