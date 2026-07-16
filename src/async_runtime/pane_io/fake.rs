//! Deterministic fake pane backend used by async-runtime tests.

use super::{
    AsyncPaneForegroundProcess, AsyncPaneIoFuture, AsyncPaneProcessIo, MezError, ProcessEvent,
    Result, Size, VecDeque,
};

/// Deterministic fake backend for async pane driver tests.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct AsyncFakePaneProcessIo {
    /// Stores the output batches value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    output_batches: VecDeque<Result<Option<Vec<u8>>>>,
    /// Stores the exit results value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    exit_results: VecDeque<Result<Option<ProcessEvent>>>,
    /// Stores the foreground results value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    foreground_results: VecDeque<Result<Option<AsyncPaneForegroundProcess>>>,
    /// Stores the write results value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    write_results: VecDeque<Result<usize>>,
    /// Stores the resize results value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    resize_results: VecDeque<Result<()>>,
    /// Stores the terminate results value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    terminate_results: VecDeque<Result<ProcessEvent>>,
    /// Writes requested by the driver.
    pub writes: Vec<Vec<u8>>,
    /// Resizes requested by the driver.
    pub resizes: Vec<Size>,
    /// Termination requests made by the driver.
    pub terminations: Vec<bool>,
}

#[cfg(test)]
impl AsyncFakePaneProcessIo {
    /// Queues one output chunk.
    pub fn push_output(&mut self, bytes: impl Into<Vec<u8>>) {
        self.output_batches.push_back(Ok(Some(bytes.into())));
    }

    /// Queues an empty output poll.
    pub fn push_no_output(&mut self) {
        self.output_batches.push_back(Ok(None));
    }

    /// Queues one process-exit poll result.
    pub fn push_exit_result(&mut self, result: Result<Option<ProcessEvent>>) {
        self.exit_results.push_back(result);
    }

    /// Queues one foreground process metadata poll result.
    pub fn push_foreground_process_result(
        &mut self,
        result: Result<Option<AsyncPaneForegroundProcess>>,
    ) {
        self.foreground_results.push_back(result);
    }

    /// Queues one output read failure.
    pub fn push_output_error(&mut self, message: impl Into<String>) {
        self.output_batches
            .push_back(Err(MezError::invalid_state(message.into())));
    }

    /// Queues a write result.
    pub fn push_write_result(&mut self, result: Result<usize>) {
        self.write_results.push_back(result);
    }

    /// Queues a resize result.
    pub fn push_resize_result(&mut self, result: Result<()>) {
        self.resize_results.push_back(result);
    }

    /// Queues a termination result.
    pub fn push_terminate_result(&mut self, result: Result<ProcessEvent>) {
        self.terminate_results.push_back(result);
    }
}

#[cfg(test)]
impl AsyncPaneProcessIo for AsyncFakePaneProcessIo {
    /// Runs the read output operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn read_output<'a>(&'a mut self, max_bytes: usize) -> AsyncPaneIoFuture<'a, Option<Vec<u8>>> {
        Box::pin(async move {
            let mut output = self.output_batches.pop_front().unwrap_or(Ok(None))?;
            if let Some(bytes) = output.as_mut() {
                bytes.truncate(max_bytes);
            }
            Ok(output)
        })
    }

    /// Runs the write input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn write_input<'a>(&'a mut self, bytes: &'a [u8]) -> AsyncPaneIoFuture<'a, usize> {
        Box::pin(async move {
            self.writes.push(bytes.to_vec());
            self.write_results.pop_front().unwrap_or(Ok(bytes.len()))
        })
    }

    /// Runs the poll exit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn poll_exit<'a>(&'a mut self) -> AsyncPaneIoFuture<'a, Option<ProcessEvent>> {
        Box::pin(async move { self.exit_results.pop_front().unwrap_or(Ok(None)) })
    }

    /// Runs the foreground process operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn foreground_process<'a>(
        &'a mut self,
    ) -> AsyncPaneIoFuture<'a, Option<AsyncPaneForegroundProcess>> {
        Box::pin(async move { self.foreground_results.pop_front().unwrap_or(Ok(None)) })
    }

    /// Runs the resize operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn resize<'a>(&'a mut self, size: Size) -> AsyncPaneIoFuture<'a, ()> {
        Box::pin(async move {
            self.resizes.push(size);
            self.resize_results.pop_front().unwrap_or(Ok(()))
        })
    }

    /// Runs the terminate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn terminate<'a>(&'a mut self, force: bool) -> AsyncPaneIoFuture<'a, ProcessEvent> {
        Box::pin(async move {
            self.terminations.push(force);
            self.terminate_results.pop_front().unwrap_or_else(|| {
                Ok(ProcessEvent::Exited {
                    pane_id: String::new(),
                    primary_pid: None,
                    exit_code: None,
                    signal: Some(if force { "killed" } else { "terminated" }.to_string()),
                })
            })
        })
    }
}
