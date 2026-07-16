//! Compatibility service loop for a standalone pane driver.

use super::*;

/// Configuration for a pane process driver service loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsyncPaneProcessDriverServiceConfig {
    /// Maximum output polls before the service returns.
    pub max_polls: u64,
    /// Sleep interval used after an empty output poll.
    pub idle_interval: Duration,
}

impl Default for AsyncPaneProcessDriverServiceConfig {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self {
            max_polls: u64::MAX,
            idle_interval: Duration::from_millis(16),
        }
    }
}

impl AsyncPaneProcessDriverServiceConfig {
    /// Validates service loop bounds.
    pub fn validate(self) -> Result<()> {
        if self.max_polls == 0 {
            return Err(MezError::invalid_args(
                "async pane driver service max_polls must be greater than zero",
            ));
        }
        if self.idle_interval.is_zero() {
            return Err(MezError::invalid_args(
                "async pane driver service idle interval must be greater than zero",
            ));
        }
        Ok(())
    }
}

/// Report returned by one pane process driver service loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncPaneProcessDriverServiceReport {
    /// Number of output polls attempted.
    pub polls: u64,
    /// Number of runtime events submitted to the actor.
    pub submitted_events: usize,
    /// Number of submitted events applied to runtime state.
    pub applied_events: usize,
}

/// Runs one pane driver until stopped, submitting output events to the actor.
pub async fn run_async_pane_process_driver_service<B, F>(
    handle: &AsyncRuntimeSessionHandle,
    driver: &mut AsyncPaneProcessDriver<B>,
    config: AsyncPaneProcessDriverServiceConfig,
    mut should_stop: F,
) -> Result<AsyncPaneProcessDriverServiceReport>
where
    B: AsyncPaneProcessIo,
    F: FnMut(u64) -> bool,
{
    config.validate()?;
    let mut side_effect_watcher = handle.side_effect_delivery_watcher();
    let mut report = AsyncPaneProcessDriverServiceReport {
        polls: 0,
        submitted_events: 0,
        applied_events: 0,
    };

    while report.polls < config.max_polls {
        if should_stop(report.polls) {
            return Ok(report);
        }
        report.polls = report.polls.saturating_add(1);
        let Some(event) = driver.poll_output_event().await? else {
            if report.polls >= config.max_polls {
                return Ok(report);
            }
            if should_stop(report.polls) {
                return Ok(report);
            }
            let bounded_idle = (config.max_polls != u64::MAX).then_some(config.idle_interval);
            match (driver.output_activity(), bounded_idle) {
                (Some(output_activity), Some(idle_interval)) => {
                    tokio::select! {
                        result = output_activity => result?,
                        _ = handle.wait_for_event_delivery() => {}
                        result = side_effect_watcher.changed() => {
                            let _ = result;
                        }
                        _ = sleep(idle_interval) => {}
                    }
                }
                (Some(output_activity), None) => {
                    tokio::select! {
                        result = output_activity => result?,
                        _ = handle.wait_for_event_delivery() => {}
                        result = side_effect_watcher.changed() => {
                            let _ = result;
                        }
                    }
                }
                (None, Some(idle_interval)) => {
                    tokio::select! {
                        _ = handle.wait_for_event_delivery() => {}
                        result = side_effect_watcher.changed() => {
                            let _ = result;
                        }
                        _ = sleep(idle_interval) => {}
                    }
                }
                (None, None) => {
                    tokio::select! {
                        _ = handle.wait_for_event_delivery() => {}
                        result = side_effect_watcher.changed() => {
                            let _ = result;
                        }
                    }
                }
            }
            continue;
        };
        let mut batch = RuntimeEventBatch::new();
        batch.push(event);
        let ingress = handle.submit_runtime_events(batch).await?;
        report.submitted_events = report.submitted_events.saturating_add(ingress.accepted);
        report.applied_events = report.applied_events.saturating_add(ingress.applied);
    }

    Ok(report)
}
