//! History task spawning. Production tasks retain their existing detached
//! lifetime; isolated tests may opt into an original-handle cleanup owner.

use super::AsyncRuntimeSessionActor;

#[cfg(test)]
#[derive(Clone, Debug, Default)]
pub(crate) struct HistoryWorkerFixture {
    handles: std::sync::Arc<tokio::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}

#[cfg(test)]
impl HistoryWorkerFixture {
    fn retain(&self, handle: tokio::task::JoinHandle<()>) {
        // Registration belongs to the actor; observation starts only after it
        // exits, so these phases must not compete for the handle collection.
        self.handles
            .try_lock()
            .expect("fixture history observation started before actor exit")
            .push(handle);
    }

    /// Joins every original task after actor exit, while its store is retained.
    /// Cancellation borrows rather than removes the currently observed handle.
    /// This is test cleanup, not transcript adoption or production shutdown.
    pub(crate) async fn join(&self) -> crate::Result<usize> {
        let mut handles = self.handles.lock().await;
        let count = handles.len();
        let mut failure = None;
        while let Some(handle) = handles.last_mut() {
            let result = handle.await;
            handles.pop();
            if result.is_err() {
                failure.get_or_insert_with(|| {
                    crate::MezError::invalid_state("fixture history task did not exit normally")
                });
            }
        }
        failure.map_or(Ok(count), Err)
    }
}

impl AsyncRuntimeSessionActor {
    /// Opts an isolated fixture into joining its original history tasks. The
    /// caller must stop/join the actor before observing this cleanup owner.
    #[cfg(test)]
    pub(crate) fn retain_history_workers_for_tests(&mut self) -> HistoryWorkerFixture {
        assert!(self.history_workers_for_tests.is_none());
        let owner = HistoryWorkerFixture::default();
        self.history_workers_for_tests = Some(owner.clone());
        owner
    }

    pub(super) fn spawn_history_worker(
        &self,
        work: impl std::future::Future<Output = ()> + Send + 'static,
    ) {
        let handle = tokio::spawn(work);
        #[cfg(test)]
        if let Some(owner) = &self.history_workers_for_tests {
            owner.retain(handle);
            return;
        }
        std::mem::drop(handle);
    }
}

#[cfg(test)]
mod tests {
    use super::HistoryWorkerFixture;

    /// A cancelled cleanup observer cannot detach the original blocking read.
    /// The physical worker is released and observed before verdict assertions.
    #[tokio::test]
    async fn fixture_history_join_retains_original_handle_across_observer_cancellation() {
        let owner = HistoryWorkerFixture::default();
        let started = std::sync::Arc::new(tokio::sync::Notify::new());
        let entered = started.clone();
        let (release, blocked) = std::sync::mpsc::channel();
        let (finished, retired) = tokio::sync::oneshot::channel();
        owner.retain(tokio::spawn(async move {
            tokio::task::spawn_blocking(move || {
                entered.notify_one();
                blocked.recv().unwrap();
            })
            .await
            .unwrap();
            let _ = finished.send(());
        }));
        started.notified().await;
        let mut observation = Box::pin(owner.join());
        let pending = futures_util::poll!(observation.as_mut()).is_pending();
        drop(observation);
        release.send(()).unwrap();
        let joined = tokio::time::timeout(std::time::Duration::from_secs(5), owner.join())
            .await
            .unwrap()
            .unwrap();
        retired.await.unwrap();
        assert!(pending);
        assert_eq!(joined, 1);
        assert_eq!(owner.join().await.unwrap(), 0);
    }
}
