//! Private model work for one frozen source through existing auxiliary claims.
//!
//! Each worker generation sends a bounded wave, then returns to actor admission
//! before further work. Only the provider port owns packing and invalid-output
//! repair. Transport retries retain exact requests and accepted intermediates.
//! Rendering, packing and completion run outside the actor; blocking closures
//! retain the original finite preparation permit through physical retirement.

use crate::integrations::agent::provider::{AsyncModelProvider, provider_error_retry_class};
use crate::{MezError, Result};
use futures_util::{StreamExt, stream::FuturesUnordered};
use mez_agent::epoch_compaction::provider::{
    EpochCompactionOutputDefect, EpochCompactionProviderCompletion, EpochCompactionProviderSession,
    EpochCompactionProviderStep, EpochCompactionProviderWork, render_epoch_source,
};
use mez_agent::epoch_compaction::{
    AcceptedEpochCompaction, EpochCompactionLimits, EpochCompactionWorkId, MAX_SUMMARY_BYTES,
};
use mez_agent::{
    ModelRequest, ModelResponse, ModelTokenUsage, ProviderApiCompatibility,
    ProviderErrorRetryClass, ReplayCompactionPlan,
};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::{Mutex, OwnedMutexGuard, OwnedSemaphorePermit};

mod progress;
use progress::Progress;
pub(crate) use progress::Snapshot;

/// Product memory and concurrency bounds, independent of semantic output size.
/// The original shared preparation pool bounds simultaneous logical operations.
const SOURCE_BYTES: usize = 64 * 1024 * 1024;
const BUFFERED_SUMMARY_BYTES: usize = 8 * 1024 * 1024;
const WORK_NODES: usize = 65_536;
const IN_FLIGHT: usize = 4;

/// One engine operation, cloned only as an opaque identity by actor task waves.
#[derive(Clone)]
pub(super) struct Operation {
    state: Arc<Mutex<State>>,
    progress: Arc<Progress>,
}

impl std::fmt::Debug for Operation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Do not lock worker state or expose source/provider payloads to Debug.
        f.debug_struct("EpochCompactionOperation")
            .finish_non_exhaustive()
    }
}

struct State {
    plan: ReplayCompactionPlan,
    session: Option<EpochCompactionProviderSession>,
    /// Repair and failed transport claims, bounded by the last wave size.
    pending: Vec<EpochCompactionWorkId>,
    shape: Option<(ProviderApiCompatibility, BTreeMap<String, String>, bool)>,
    input_tokens: usize,
    outcome: Option<(u64, WaveOutcome)>,
    failed_wave: Option<u64>,
    failed: bool,
    /// A blocking closure owns this state even if its async wrapper is aborted.
    _permit: Option<Arc<OwnedSemaphorePermit>>,
}

/// A worker receipt for exact actor generation admission. It carries no new
/// source authority; publication still compares its typed plan with the source.
#[derive(Clone)]
pub(in crate::runtime::commands::compaction) enum WaveOutcome {
    Continue,
    Complete(Arc<AcceptedEpochCompaction>),
}

impl Operation {
    pub(super) fn new(
        plan: ReplayCompactionPlan,
        permit: Option<Arc<OwnedSemaphorePermit>>,
    ) -> Self {
        Self {
            progress: Arc::new(Progress::default()),
            state: Arc::new(Mutex::new(State {
                plan,
                session: None,
                pending: Vec::new(),
                shape: None,
                input_tokens: 0,
                outcome: None,
                failed_wave: None,
                failed: false,
                _permit: permit,
            })),
        }
    }

    pub(super) fn progress(&self) -> Snapshot {
        self.progress.snapshot()
    }

    /// Reads only a finished wave. The actor never waits for private packing,
    /// model I/O, or a blocking closure which outlived its cancelled wrapper.
    pub(super) fn outcome(&self, generation: u64) -> Result<WaveOutcome> {
        let state = self
            .state
            .try_lock()
            .map_err(|_| MezError::conflict("compaction model work has not physically retired"))?;
        state
            .outcome
            .as_ref()
            .filter(|(issued, _)| *issued == generation)
            .map(|(_, outcome)| outcome.clone())
            .ok_or_else(|| MezError::invalid_state("compaction wave receipt unavailable"))
    }

    /// A terminal sibling failure cannot be revived by transport retry of
    /// another sibling. The actor only probes this after producer retirement.
    pub(super) fn can_retry(&self, generation: u64) -> bool {
        self.state.try_lock().is_ok_and(|state| {
            !state.failed && state.failed_wave == Some(generation) && !state.pending.is_empty()
        })
    }

    /// Executes at most one physical request per admitted node. The supplied
    /// provider is the existing observed adapter: it settles each numeric wire
    /// receipt through actor acknowledgment before this method sees the result.
    pub(super) async fn execute<P: AsyncModelProvider>(
        &self,
        provider: &P,
        generation: u64,
        template: ModelRequest,
        shape: (ProviderApiCompatibility, BTreeMap<String, String>, bool),
        input_tokens: usize,
    ) -> Result<ModelResponse> {
        let state = self.state.clone().try_lock_owned().map_err(|_| {
            MezError::conflict("compaction model operation already has a physical worker")
        })?;
        let provider_id = template.provider.clone();
        let model = template.model.clone();
        self.progress.preparing();
        let (state, work) = tokio::task::spawn_blocking(move || {
            prepare_wave(
                state,
                generation,
                &template,
                shape.0,
                shape.1,
                shape.2,
                input_tokens,
            )
        })
        .await
        .map_err(|_| MezError::invalid_state("compaction preparation worker failed"))??;

        let mut calls = work
            .into_iter()
            .map(|work| async move {
                self.progress.invoking(work.serial_depth());
                let result = provider.send_request_async(work.request()).await;
                (work, result)
            })
            .collect::<FuturesUnordered<_>>();
        let mut completed = Vec::with_capacity(IN_FLIGHT);
        let mut usage = ModelTokenUsage::default();
        let mut quotas = Vec::new();
        // Drain all admitted siblings before returning a transport failure.
        // Their accepted intermediates and reported expense survive the retry.
        while let Some((work, result)) = calls.next().await {
            match &result {
                Ok(response) => {
                    usage.add_assign(response.usage);
                    quotas.extend(response.quota_usage.iter().cloned());
                }
                Err(error) => usage.add_assign(error.provider_incurred_usage()),
            }
            completed.push((work, result));
        }
        self.progress.phase(3);
        let progress = self.progress.clone();
        let result = tokio::task::spawn_blocking(move || {
            finish_wave(state, generation, completed, progress)
        })
        .await
        .map_err(|_| MezError::invalid_state("compaction completion worker failed"))
        .and_then(|result| result);
        result.map_err(|error| error.with_provider_incurred_usage(usage))?;
        // This event carries current-wave accounting only. Accepted history is
        // the typed private result, never this empty carrier's response text.
        Ok(ModelResponse {
            provider: provider_id,
            model,
            raw_text: String::new(),
            usage,
            latest_request_usage: None,
            quota_usage: quotas,
            action_batch: None,
            provider_transcript_events: Vec::new(),
        })
    }
}

type Wave = (OwnedMutexGuard<State>, Vec<EpochCompactionProviderWork>);

/// Called only on a blocking worker; template and shape are frozen once.
#[allow(
    clippy::too_many_arguments,
    reason = "explicit frozen request and claim inputs"
)]
fn prepare_wave(
    mut state: OwnedMutexGuard<State>,
    generation: u64,
    template: &ModelRequest,
    api: ProviderApiCompatibility,
    options: BTreeMap<String, String>,
    stream: bool,
    input_tokens: usize,
) -> Result<Wave> {
    if state.failed
        || matches!(state.outcome, Some((_, WaveOutcome::Complete(_))))
        || state
            .outcome
            .as_ref()
            .is_some_and(|(issued, _)| *issued == generation)
    {
        return Err(MezError::conflict(
            "compaction model wave is stale or retired",
        ));
    }
    state.outcome = None;
    state.failed_wave = None;
    let shape = (api, options, stream);
    if let Some(original) = &state.shape {
        if original != &shape {
            return Err(MezError::conflict(
                "compaction frozen provider shape changed",
            ));
        }
    } else {
        let source = render_epoch_source(state.plan.clone(), SOURCE_BYTES)?;
        let limits = EpochCompactionLimits {
            input_tokens,
            framing_tokens: 0,
            source_bytes: SOURCE_BYTES,
            summary_bytes: MAX_SUMMARY_BYTES,
            buffered_summary_bytes: BUFFERED_SUMMARY_BYTES,
            work_nodes: WORK_NODES,
            in_flight: IN_FLIGHT,
            expected_summary_tokens: 1024,
        };
        state.session = Some(EpochCompactionProviderSession::new(
            source,
            template.clone(),
            api,
            shape.1.clone(),
            stream,
            limits,
        )?);
        state.shape = Some(shape);
        state.input_tokens = input_tokens;
    }
    let pending = std::mem::take(&mut state.pending);
    let session = state
        .session
        .as_mut()
        .ok_or_else(|| MezError::invalid_state("compaction private engine is unavailable"))?;
    let mut work = pending
        .into_iter()
        .map(|id| session.retry(id).map_err(MezError::from))
        .collect::<Result<Vec<_>>>()?;
    while work.len() < IN_FLIGHT {
        match session.claim()? {
            EpochCompactionProviderStep::Work(next) => work.push(next),
            EpochCompactionProviderStep::Waiting | EpochCompactionProviderStep::Complete => break,
        }
    }
    Ok((state, work))
}

/// Finishes all disjoint results before a wave receipt crosses actor ingress.
/// Genuine input rejection lowers actual auxiliary admission only; a valid final
/// never goes through size/ratio/ordinary-request-fit correction.
fn finish_wave(
    mut state: OwnedMutexGuard<State>,
    generation: u64,
    completed: Vec<(EpochCompactionProviderWork, Result<ModelResponse>)>,
    progress: Arc<Progress>,
) -> Result<()> {
    let mut failure = None;
    for (work, result) in completed {
        let outcome = match result {
            Ok(response) => state
                .session
                .as_mut()
                .ok_or_else(|| MezError::invalid_state("compaction engine disappeared"))?
                .complete(work.id(), &response)
                .map_err(MezError::from),
            Err(error) => {
                let class = provider_error_retry_class(&error);
                let malformed = error.provider_failure_json().is_some_and(|payload| {
                    serde_json::from_str::<serde_json::Value>(payload)
                        .ok()
                        .and_then(|value| value.get("type").cloned())
                        .and_then(|kind| kind.as_str().map(str::to_owned))
                        .as_deref()
                        == Some("malformed_model_output")
                });
                if class == ProviderErrorRetryClass::ContextLimit {
                    let Some((api, options, stream)) = state.shape.as_ref() else {
                        return Err(MezError::invalid_state(
                            "compaction frozen shape disappeared",
                        ));
                    };
                    let measured = mez_agent::provider_request_input_estimate(
                        work.request(),
                        *api,
                        options,
                        *stream,
                    )?
                    .input_tokens;
                    // Fresh rejected work halves actual admission instead of
                    // retrying nearly identical prefixes one token at a time.
                    // Older in-flight siblings use their own measured size;
                    // keep the stricter ceiling with the minimum decrement
                    // required to replan, without halving that ceiling again.
                    let next = (measured / 2).min(state.input_tokens.saturating_sub(1));
                    let rejected = state
                        .session
                        .as_mut()
                        .ok_or_else(|| MezError::invalid_state("compaction engine disappeared"))?
                        .reject_input(work.id(), next)
                        .map_err(MezError::from);
                    if rejected.is_ok() {
                        state.input_tokens = next;
                    }
                    rejected.map(|()| EpochCompactionProviderCompletion::Accepted)
                } else if class != ProviderErrorRetryClass::BudgetSuspended
                    && (malformed || class == ProviderErrorRetryClass::OutputLimit)
                {
                    state
                        .session
                        .as_mut()
                        .ok_or_else(|| MezError::invalid_state("compaction engine disappeared"))?
                        .repair(
                            work.id(),
                            if malformed {
                                EpochCompactionOutputDefect::Malformed
                            } else {
                                EpochCompactionOutputDefect::Incomplete
                            },
                        )
                        .map_err(MezError::from)
                } else {
                    state.pending.push(work.id());
                    Err(error)
                }
            }
        };
        match outcome {
            Ok(EpochCompactionProviderCompletion::Repair(work)) => state.pending.push(work.id()),
            Ok(EpochCompactionProviderCompletion::Accepted) => {}
            Err(error) => {
                if !matches!(
                    provider_error_retry_class(&error),
                    ProviderErrorRetryClass::RetryableTransport
                        | ProviderErrorRetryClass::BudgetSuspended
                ) {
                    state.failed = true;
                }
                // Every issued sibling has settled before this fold. A local
                // hold must fence recovery for the whole wave even if an
                // earlier sibling completed with a retryable transport error.
                // Keep the first hold, including its original diagnostics.
                if failure.is_none()
                    || (provider_error_retry_class(&error)
                        == ProviderErrorRetryClass::BudgetSuspended
                        && failure.as_ref().is_some_and(|original| {
                            provider_error_retry_class(original)
                                != ProviderErrorRetryClass::BudgetSuspended
                        }))
                {
                    failure = Some(error);
                }
            }
        }
    }
    if let Some(error) = failure {
        state.failed_wave = Some(generation);
        return Err(error);
    }
    // Claiming here would consume next wave work before pause/freshness
    // admission. Only a non-mutating final probe belongs at this boundary.
    let final_result = state
        .session
        .as_ref()
        .is_some_and(|session| session.is_complete());
    let outcome = if final_result {
        let session = state
            .session
            .take()
            .ok_or_else(|| MezError::invalid_state("compaction completed engine disappeared"))?;
        WaveOutcome::Complete(Arc::new(session.into_result()?))
    } else {
        WaveOutcome::Continue
    };
    state.outcome = Some((generation, outcome));
    progress.phase(if final_result { 5 } else { 4 });
    Ok(())
}
