//! Actual actor and ordered writer for assembled native patch/shell regressions.
//!
//! Callers retain each original producer through its qualified transfer before
//! finishing these tasks. Dropped test observers request stop only; task abortion
//! on unwind is not a worker join or a successful canonical/storage handoff.

use super::*;
use crate::host::async_runtime::{
    AsyncRuntimeActorExit, AsyncRuntimeSessionActor, AsyncRuntimeSessionHandle,
};
use crate::runtime::native_filesystem::NativeFilesystemProducer;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// Original actor and writer handles shared by the native and sibling test owners.
pub(super) struct FilesystemActor {
    /// Actual actor destination for claim/lease/progress/storage requests.
    pub(super) handle: AsyncRuntimeSessionHandle,
    /// Actual actor join, consumed only by normal fixture finish.
    actor: Option<tokio::task::JoinHandle<AsyncRuntimeActorExit>>,
    /// Original ordered writer, kept alive through native transfer.
    writer: Option<tokio::task::JoinHandle<()>>,
    /// Stop is independent of actor request and cannot fabricate completion.
    stop: Arc<AtomicBool>,
}
impl FilesystemActor {
    /// Starts the real actor and ordered persistence service, without mutation.
    pub(super) fn new(service: RuntimeSessionService) -> Self {
        let (handle, actor) = AsyncRuntimeSessionActor::new(service, Default::default()).unwrap();
        let actor = tokio::spawn(Box::pin(actor.run()));
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let context = handle.clone();
        let writer = tokio::spawn(Box::pin(async move {
            let report = crate::host::async_runtime::run_async_persistence_side_effect_service(
                &context,
                crate::host::async_runtime::AsyncRuntimeSideEffectServiceConfig {
                    max_polls: u64::MAX,
                    drain_limit: 64,
                    idle_interval: Duration::from_millis(1),
                },
                |_, _| stopping.load(Ordering::Acquire),
            )
            .await
            .unwrap();
            assert_eq!(report.failed, 0);
        }));
        Self {
            handle,
            actor: Some(actor),
            writer: Some(writer),
            stop,
        }
    }
    /// Claims and starts the already retained original in-process producer.
    pub(super) async fn start(&self, producer: &NativeFilesystemProducer) {
        assert!(crate::host::async_runtime::native_filesystem::claim_and_start_native_filesystem_producer(self.handle.clone(), producer).await.unwrap());
    }
    /// Observes actual native/storage joins and the original qualified handoff.
    pub(super) async fn settle(&self, producer: &NativeFilesystemProducer) {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(ack) = crate::host::async_runtime::native_filesystem::transfer_joined_native_filesystem_producer(&self.handle, producer).await.unwrap() {
                    assert!(ack.acknowledges(producer)); return;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }).await.unwrap();
    }
    /// Joins the writer after caller-owned producers settle, then the actor.
    pub(super) async fn finish(mut self) -> RuntimeSessionService {
        self.stop.store(true, Ordering::Release);
        self.writer.take().unwrap().await.unwrap();
        self.handle.shutdown().await.unwrap();
        self.actor.take().unwrap().await.unwrap().service
    }
}
impl Drop for FilesystemActor {
    /// Unwind cleanup aborts observers without claiming native or storage joins.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(writer) = &self.writer {
            writer.abort();
        }
        if let Some(actor) = &self.actor {
            actor.abort();
        }
    }
}
