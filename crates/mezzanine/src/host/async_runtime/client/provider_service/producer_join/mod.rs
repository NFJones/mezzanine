//! Actual provider task joins retained outside abortable monitor futures.
//!
//! This wraps an already-spawned task, not a new scheduler or task. Monitor
//! cancellation drops only a mutex borrow; the original drain keeps the handle
//! and its exact result until actual join and one result consumer. Destruction
//! notices, abort requests and last-owner drop are not join acknowledgments.

use crate::error::{MezError, Result};
use std::{future::Future, pin::Pin, sync::Arc};
use tokio::{
    sync::Mutex,
    task::{AbortHandle, JoinError, JoinHandle},
};

/// Type-erased join observation; the monitor retains the typed result consumer.
pub(super) trait ProviderProducerJoin: Send + Sync {
    fn observe(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

struct State<T> {
    handle: Option<JoinHandle<T>>,
    result: Option<std::result::Result<T, JoinError>>,
}

impl<T> Drop for State<T> {
    fn drop(&mut self) {
        if let Some(handle) = &self.handle {
            handle.abort();
            eprintln!("mezzanine: provider producer dropped before actual join observation");
        }
    }
}

struct Source<T>(Mutex<State<T>>);

impl<T: Send> Source<T> {
    async fn observe_locked(state: &mut State<T>) {
        if let Some(handle) = &mut state.handle {
            let result = handle.await;
            // No await separates observing the actual join from retaining its
            // sole result. A failed join remains the original typed JoinError.
            state.result = Some(result);
            state.handle = None;
        }
    }
}

impl<T: Send> ProviderProducerJoin for Source<T> {
    fn observe(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            let mut state = self.0.lock().await;
            Self::observe_locked(&mut state).await;
        })
    }
}

/// One original producer with an independent drain-owned join observer.
pub(super) struct ProviderProducer<T> {
    source: Arc<Source<T>>,
    abort: AbortHandle,
}

impl<T> Clone for ProviderProducer<T> {
    fn clone(&self) -> Self {
        Self {
            source: self.source.clone(),
            abort: self.abort.clone(),
        }
    }
}

impl<T: Send + 'static> ProviderProducer<T> {
    pub(super) fn new(handle: JoinHandle<T>) -> Self {
        let abort = handle.abort_handle();
        Self {
            source: Arc::new(Source(Mutex::new(State {
                handle: Some(handle),
                result: None,
            }))),
            abort,
        }
    }

    pub(super) fn retirement(&self) -> Arc<dyn ProviderProducerJoin> {
        self.source.clone()
    }

    pub(super) fn abort_handle(&self) -> AbortHandle {
        self.abort.clone()
    }
    pub(super) fn abort(&self) {
        self.abort.abort();
    }

    #[cfg(test)]
    pub(super) fn is_finished(&self) -> bool {
        self.abort.is_finished()
    }

    /// Consume the original task result once. Repeated retirement observation
    /// is inert, but cannot manufacture a second result or another execution.
    pub(super) async fn result(&self) -> Result<std::result::Result<T, JoinError>> {
        let mut state = self.source.0.lock().await;
        Source::observe_locked(&mut state).await;
        state
            .result
            .take()
            .ok_or_else(|| MezError::invalid_state("provider producer result already consumed"))
    }
}

#[cfg(test)]
mod tests;
