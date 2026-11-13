//! Counter finality for the OpenAI/DeepSeek Chat Completions stream grammar.
//!
//! This is decoder state, not a ledger or transport witness. A usage report may
//! qualify only after all observed choices have explicit nonempty finish reasons.
//! Its own presence is retained separately from cumulative decoder counters.

use crate::{ModelTokenUsage, ModelTokenUsagePresence};
use std::collections::BTreeMap;

type Report = (ModelTokenUsage, ModelTokenUsagePresence);

#[derive(Debug, Default)]
pub(crate) struct ChatStreamUsage {
    choices: BTreeMap<u64, bool>,
    report: Option<Report>,
    degraded: bool,
    conflicted: bool,
    regressed: bool,
}

impl ChatStreamUsage {
    /// Record bounded choice completion before interpreting this report. Return
    /// true only for a regressing qualified report, so the caller preserves its
    /// prior expense instead of overlaying contradictory counters.
    pub(crate) fn observe(
        &mut self,
        value: &serde_json::Value,
        previous: ModelTokenUsage,
        report: Option<Report>,
    ) -> bool {
        let decreases = report.is_some_and(|(usage, presence)| {
            (presence.input && usage.input_tokens < previous.input_tokens)
                || (presence.output && usage.output_tokens < previous.output_tokens)
        });
        let prior_regression = self.report.is_some() && decreases;
        self.regressed |= prior_regression;
        let Some(choices) = value.get("choices").and_then(serde_json::Value::as_array) else {
            self.degraded = true;
            self.conflicted |= self.report.is_some();
            return prior_regression;
        };
        for choice in choices {
            let Some(index) = choice.get("index").and_then(serde_json::Value::as_u64) else {
                self.degraded = true;
                self.conflicted |= self.report.is_some();
                continue;
            };
            // Excess/malformed choice state degrades coverage, not output or
            // unrelated service availability. No unbounded auxiliary map.
            if !self.choices.contains_key(&index) && self.choices.len() >= 64 {
                self.degraded = true;
                self.conflicted |= self.report.is_some();
                continue;
            }
            let finished = choice
                .get("finish_reason")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|reason| !reason.is_empty());
            self.choices.insert(index, finished);
        }
        if value.get("error").is_some()
            || self.choices.is_empty()
            || !self.choices.values().all(|finished| *finished)
        {
            self.conflicted |= self.report.is_some();
            return prior_regression;
        }
        let Some(report) = report else {
            return prior_regression;
        };
        let regressed = decreases;
        // Sticky refusal is independent of conflict/absence. Later duplicates
        // cannot turn integrity failure into release-eligible unknown coverage.
        self.regressed |= regressed;
        if let Some(original) = self.report {
            self.conflicted |= original != report;
        } else if !self.degraded && !self.conflicted {
            self.report = Some(report);
        }
        regressed
    }

    pub(crate) fn terminal_usage(&self) -> Option<Report> {
        (!self.degraded && !self.conflicted && !self.regressed)
            .then_some(self.report)
            .flatten()
    }

    pub(crate) fn regressed(&self) -> bool {
        self.regressed
    }

    /// A positively captured terminal report was contradicted. Malformed
    /// pre-terminal metadata alone degrades coverage without changing output.
    pub(crate) fn conflicting_report(&self) -> bool {
        self.conflicted && self.report.is_some()
    }
}

#[cfg(test)]
mod tests;
