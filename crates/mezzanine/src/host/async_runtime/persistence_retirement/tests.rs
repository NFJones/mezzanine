//! Real actor identity, pre-poll teardown and cached original writer results.
//!
//! These controls use no provider inference or native filesystem effect. They
//! qualify registration and writer lifetime independently of the actual patch
//! retirement fixtures, including source already queued before wrapper polling.

use super::*;
use crate::host::async_runtime::{
    AsyncAgentProviderServiceConfig, AsyncRuntimeActorExit, AsyncRuntimeService,
    AsyncRuntimeServiceSupervisor, AsyncRuntimeSessionActor,
    build_async_persistence_side_effect_service,
};

/// Starts one empty real actor with an independent admission allocation.
fn actor() -> (
    AsyncRuntimeSessionHandle,
    tokio::task::JoinHandle<AsyncRuntimeActorExit>,
) {
    let service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    let (handle, actor) = AsyncRuntimeSessionActor::new(service, Default::default()).unwrap();
    (handle, tokio::spawn(actor.run()))
}

/// Closes only the owning actor after its actual writer was joined or never ran.
async fn finish(
    handle: &AsyncRuntimeSessionHandle,
    actor: tokio::task::JoinHandle<AsyncRuntimeActorExit>,
) {
    handle.shutdown().await.unwrap();
    let mut exit = actor.await.unwrap();
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Duplicate writers for an actual actor fail before either actual task starts.
/// Distinct names do not make two original ordered writers safe; rejecting this
/// service set must not leave dependency leases or unobserved writer tasks.
#[tokio::test(flavor = "current_thread")]
async fn persistence_retirement_rejects_duplicate_actor_writers_before_spawn() {
    let (handle, actor) = actor();
    let first =
        build_async_persistence_side_effect_service("first", handle.clone(), Default::default())
            .unwrap();
    let second =
        build_async_persistence_side_effect_service("second", handle.clone(), Default::default())
            .unwrap();
    let owners = [
        first.persistence_retirement.as_ref().unwrap().clone(),
        second.persistence_retirement.as_ref().unwrap().clone(),
    ];
    let error = AsyncRuntimeServiceSupervisor::new(vec![first, second]).unwrap_err();
    assert!(error.message().contains("one persistence writer per actor"));
    for owner in owners {
        assert!(!*owner.0.started.lock().unwrap());
        assert_eq!(owner.0.work.dependents.load(Ordering::Acquire), 0);
        assert!(matches!(
            *owner.0.state.try_lock().unwrap(),
            PersistenceDriverState::Unstarted
        ));
    }
    finish(&handle, actor).await;
}

/// Dependencies bind actual actor allocation and survive cancellation before any
/// wrapper's first poll. Only the matching provider adds a lease; the already
/// queued audit append is performed once by the original final-drain writer.
/// The final report and a repeated historical join must retain the actual count.
#[tokio::test(flavor = "current_thread")]
async fn persistence_retirement_matches_actor_and_drains_before_first_wrapper_poll() {
    use crate::runtime::{PersistenceTarget, PersistenceWriteMode, RuntimeSideEffect};
    let (first, first_actor) = actor();
    let (foreign, foreign_actor) = actor();
    /// Private one-consumer fixture directory, removed after real writer join.
    struct Directory(std::path::PathBuf);
    impl Drop for Directory {
        /// No worker outlives the normal test's owning directory scope.
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let directory = Directory(std::env::temp_dir().join(format!(
        "mez-persistence-retirement-{:032x}",
        rand::random::<u128>()
    )));
    std::fs::create_dir(&directory.0).unwrap();
    let path = directory.0.join("audit.jsonl");
    first
        .queue_runtime_side_effects(vec![RuntimeSideEffect::Persist {
            target: PersistenceTarget::AuditLog,
            path: path.clone(),
            bytes: b"pre-poll-source\n".to_vec(),
            mode: PersistenceWriteMode::Append,
        }])
        .await
        .unwrap();
    let writer =
        build_async_persistence_side_effect_service("writer", first.clone(), Default::default())
            .unwrap();
    let owner = writer.persistence_retirement.as_ref().unwrap().clone();
    assert!(owner.same_actor(&first.clone()));
    assert!(!owner.same_actor(&foreign));
    let local_provider = crate::host::async_runtime::provider::build_async_agent_provider_service(
        "matching-provider",
        first.clone(),
        AsyncAgentProviderServiceConfig::new(1).unwrap(),
    )
    .unwrap();
    let foreign_provider =
        crate::host::async_runtime::provider::build_async_agent_provider_service(
            "foreign-provider",
            foreign.clone(),
            AsyncAgentProviderServiceConfig::new(1).unwrap(),
        )
        .unwrap();
    let primary = AsyncRuntimeService::new("primary", async {
        std::future::pending::<()>().await;
        Ok(AsyncRuntimeServiceExit::completed(0))
    });
    let supervisor =
        AsyncRuntimeServiceSupervisor::new(vec![writer, local_provider, foreign_provider, primary])
            .unwrap();
    // Current-thread Tokio has not polled a spawned wrapper between constructor
    // return and this assertion: one supervisor lease plus one actual matching
    // provider lease exists, and the foreign actor contributes none.
    assert_eq!(owner.0.work.dependents.load(Ordering::Acquire), 2);
    assert!(*owner.0.started.lock().unwrap());
    let report = supervisor.run_until_shutdown(async {}).await.unwrap();
    assert!(report.shutdown_requested);
    assert_eq!(report.services.len(), 4);
    assert_eq!(
        report
            .services
            .iter()
            .find(|service| service.name == "writer")
            .unwrap()
            .exit
            .work_units,
        1
    );
    let actual = owner.retire().await.unwrap();
    let repeated = owner.retire().await.unwrap();
    assert_eq!(actual, repeated);
    assert_eq!(actual.completed, 1);
    assert_eq!(actual.failed, 0);
    assert_eq!(std::fs::read(&path).unwrap(), b"pre-poll-source\n");
    assert_eq!(owner.0.work.dependents.load(Ordering::Acquire), 0);
    assert!(
        owner
            .dependency()
            .unwrap_err()
            .message()
            .contains("before writer start")
    );
    finish(&first, first_actor).await;
    finish(&foreign, foreign_actor).await;
}

/// Actor loss yields an actual failed writer join retained as the original
/// historical error. Repeated retirement cannot recreate a driver, promote
/// success or admit a new publisher after the original stop/start boundary.
#[tokio::test(flavor = "current_thread")]
async fn persistence_retirement_caches_actual_actor_loss_without_restarting_writer() {
    let (handle, actor) = actor();
    let owner = PersistenceServiceRetirement::new(handle.clone(), Default::default());
    finish(&handle, actor).await;
    let first = owner.retire().await.unwrap_err();
    let repeated = owner.retire().await.unwrap_err();
    assert_eq!(first.kind(), repeated.kind());
    assert_eq!(first.message(), repeated.message());
    assert!(matches!(
        *owner.0.state.lock().await,
        PersistenceDriverState::Joined(Err(_))
    ));
    assert!(*owner.0.started.lock().unwrap());
    assert!(owner.dependency().is_err());
}

/// A requested final drain with a live publisher must service essential records
/// without claiming a new optional objective-cache job. The mirror is genuinely
/// eligible before start, and the writer necessarily polls that lane before its
/// causal essential write. Even a failed pre-fix admission is physically reaped
/// before asserting the captured cache state or removing this private directory.
#[tokio::test(flavor = "current_thread")]
async fn persistence_retirement_drains_essential_records_without_new_optional_work() {
    use crate::config::{ConfigFormat, ConfigLayer, ConfigScope};
    use crate::runtime::{PersistenceTarget, PersistenceWriteMode, RuntimeSideEffect};
    /// Owns only this test's files after actual writer/cache jobs are reaped.
    struct Directory(std::path::PathBuf);
    impl Drop for Directory {
        /// Cleanup cannot remove unrelated project or user cache paths.
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let root = Directory(std::env::temp_dir().join(format!(
        "mez-writer-drain-only-{:032x}",
        rand::random::<u128>()
    )));
    std::fs::create_dir(&root.0).unwrap();
    let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "writer-drain-only".into(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\nsession_title_policy=\"objective\"\n".into(),
        }])
        .unwrap();
    service.set_agent_transcript_store(crate::storage::transcript::AgentTranscriptStore::new(
        root.0.clone(),
    ));
    assert!(
        service.mirror_runtime_agent_objective("cache-only", Some("retained desired cache value"))
    );
    let (handle, actor) = AsyncRuntimeSessionActor::new(service, Default::default()).unwrap();
    let actor = tokio::spawn(actor.run());
    assert_eq!(
        handle
            .metrics()
            .await
            .unwrap()
            .objective_mirror
            .pending_keys,
        1
    );
    let owner = PersistenceServiceRetirement::new(
        handle.clone(),
        AsyncRuntimeSideEffectServiceConfig {
            max_polls: u64::MAX,
            drain_limit: 64,
            idle_interval: std::time::Duration::from_millis(1),
        },
    );
    let publisher = owner.dependency().unwrap();
    let mut observer = Box::pin(owner.retire());
    assert!(futures_util::poll!(observer.as_mut()).is_pending());
    drop(observer);
    let path = root.0.join("essential");
    handle
        .queue_runtime_side_effects(vec![RuntimeSideEffect::Persist {
            target: PersistenceTarget::AuditLog,
            path: path.clone(),
            bytes: b"essential once\n".to_vec(),
            mode: PersistenceWriteMode::Append,
        }])
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while std::fs::read(&path).ok().as_deref() != Some(b"essential once\n") {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    let captured = handle.metrics().await.unwrap().objective_mirror;
    drop(publisher);
    let actual = owner.retire().await.unwrap();
    // Reap an incorrectly admitted physical job too, so a failing control never
    // strands I/O behind its own cleanup or uses directory removal as a fence.
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            assert!(handle.poll_objective_mirror(false).await.unwrap().is_none());
            if handle.metrics().await.unwrap().objective_mirror.slot_phase == "idle" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    finish(&handle, actor).await;
    assert_eq!(std::fs::read(&path).unwrap(), b"essential once\n");
    assert_eq!(captured.pending_keys, 1, "{captured:?}");
    assert_eq!(captured.slot_phase, "idle", "{captured:?}");
    assert_eq!(captured.confirmed, 0, "{captured:?}");
    assert_eq!(captured.failed, 0, "{captured:?}");
    assert!(actual.completed >= 1);
    assert_eq!(actual.failed, 0);
}
