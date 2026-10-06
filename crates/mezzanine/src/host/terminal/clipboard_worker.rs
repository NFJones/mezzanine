//! Owned latest-value clipboard application for negotiated terminal adapters.
//!
//! Retains one asynchronous worker and a single latest pending value. The caller
//! supplies its local clipboard policy only after validating remote capability.
//! Disposal aborts queued async work; already-started blocking backend work and
//! any selection-owner child remain backend-owned and cannot be recalled. Queue
//! acceptance is not delivery evidence. Content never enters diagnostics.

use super::HostClipboard;

/// Cleanup owner for one client's best-effort, latest-value clipboard worker.
pub(crate) struct ClipboardWorker {
    sender: Option<tokio::sync::watch::Sender<Option<String>>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl ClipboardWorker {
    /// Starts the established clipboard adapter outside the terminal loop. Only
    /// a completed, independently validated effect should enter the sender.
    pub(crate) fn new(clipboard: HostClipboard) -> Self {
        let (sender, mut receiver) = tokio::sync::watch::channel(None::<String>);
        let task = tokio::spawn(async move {
            while receiver.changed().await.is_ok() {
                let Some(content) = receiver.borrow_and_update().clone() else {
                    continue;
                };
                let clipboard = clipboard.clone();
                let _ = tokio::task::spawn_blocking(move || clipboard.copy(content.as_str())).await;
            }
        });
        Self {
            sender: Some(sender),
            task: Some(task),
        }
    }

    /// Borrows the bounded latest-value queue without transferring cleanup
    /// ownership. Publishing replaces pending work; it does not confirm copying.
    pub(crate) fn sender(&self) -> Option<&tokio::sync::watch::Sender<Option<String>>> {
        self.sender.as_ref()
    }

    /// Disposes queued work and joins the async worker on explicit return. This
    /// does not await or cancel any backend operation already handed to blocking
    /// execution. Drop retains abortion responsibility if this wait is cancelled.
    pub(crate) async fn shutdown(mut self) {
        self.sender.take();
        if let Some(task) = self.task.as_mut() {
            task.abort();
            let _ = task.await;
        }
        self.task.take();
    }
}

impl Drop for ClipboardWorker {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests;
