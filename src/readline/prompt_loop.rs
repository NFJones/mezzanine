//! Bounded readline prompt loop driver.
//!
//! The loop coordinates prompt rendering, IO readiness, terminal byte decoding,
//! and high-level reporting without depending on a concrete terminal backend.

use crate::error::{MezError, Result};

use super::types::{
    ReadlineInputDecoder, ReadlineOutcome, ReadlinePrompt, ReadlinePromptLoopConfig,
    ReadlinePromptLoopIo, ReadlinePromptLoopReport,
};

impl Default for ReadlinePromptLoopConfig {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self {
            max_iterations: 1024,
            max_input_bytes: 4096,
            redraw_on_noop: false,
        }
    }
}

/// Run a bounded prompt loop over a caller-provided IO surface.
pub fn run_readline_prompt_loop<I>(
    io: &mut I,
    prompt: &mut ReadlinePrompt,
    config: ReadlinePromptLoopConfig,
) -> Result<ReadlinePromptLoopReport>
where
    I: ReadlinePromptLoopIo,
{
    if config.max_iterations == 0 {
        return Err(MezError::invalid_args(
            "readline prompt loop max_iterations must be greater than zero",
        ));
    }
    if config.max_input_bytes == 0 {
        return Err(MezError::invalid_args(
            "readline prompt loop max_input_bytes must be greater than zero",
        ));
    }

    let mut report = ReadlinePromptLoopReport {
        iterations: 0,
        outcomes: Vec::new(),
        submissions: Vec::new(),
        cancelled: false,
        eof: false,
        prompts_rendered: 0,
        bytes_written: 0,
        pending_input_bytes: 0,
    };
    write_prompt_and_record(io, prompt, &mut report)?;

    let mut decoder = ReadlineInputDecoder::new();
    for _ in 0..config.max_iterations {
        report.iterations = report.iterations.saturating_add(1);
        if !io.input_ready()? {
            break;
        }

        let input = io.read_input(config.max_input_bytes)?;
        if input.is_empty() {
            report.eof = true;
            break;
        }

        let outcomes = decoder.apply_to_prompt(prompt, &input)?;
        let mut redraw = false;
        for outcome in outcomes {
            match &outcome {
                ReadlineOutcome::Edited
                | ReadlineOutcome::Submitted(_)
                | ReadlineOutcome::SubmittedWithDisplay { .. } => {
                    redraw = true;
                }
                ReadlineOutcome::Noop if config.redraw_on_noop => {
                    redraw = true;
                }
                ReadlineOutcome::Cancelled => {
                    report.cancelled = true;
                }
                ReadlineOutcome::Eof => {
                    report.eof = true;
                }
                ReadlineOutcome::Noop => {}
            }
            match &outcome {
                ReadlineOutcome::Submitted(submission) => {
                    report.submissions.push(submission.clone());
                }
                ReadlineOutcome::SubmittedWithDisplay { text, .. } => {
                    report.submissions.push(text.clone());
                }
                _ => {}
            }
            report.outcomes.push(outcome);
        }

        if redraw {
            write_prompt_and_record(io, prompt, &mut report)?;
        }
        if report.cancelled || report.eof {
            break;
        }
    }
    report.pending_input_bytes = decoder.pending_len();

    Ok(report)
}

/// Runs the write prompt and record operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn write_prompt_and_record<I>(
    io: &mut I,
    prompt: &ReadlinePrompt,
    report: &mut ReadlinePromptLoopReport,
) -> Result<()>
where
    I: ReadlinePromptLoopIo,
{
    report.bytes_written = report
        .bytes_written
        .saturating_add(io.write_prompt(prompt)?);
    report.prompts_rendered = report.prompts_rendered.saturating_add(1);
    Ok(())
}
