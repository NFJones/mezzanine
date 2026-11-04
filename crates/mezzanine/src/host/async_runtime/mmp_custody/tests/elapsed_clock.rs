//! Actual registered root, owned worker loss and clock-qualified source custody.

use super::*;

/// Normal registered setup installs actual native elapsed time. Extreme wall
/// values cannot expire its source. An actual postcommit worker panic then
/// reconciles the same source after native clock failure: the root must retain
/// its original failed reader and positive write, never capture another epoch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn elapsed_clock_registered_root_keeps_original_time_and_failed_reader_after_worker_loss() {
    let fixture = Fixture::new(true);
    let root = MmpCustodyRoot::construct_policy(
        Destination {
            clock_source: Default::default(),
            root: fixture.path.clone(),
            owner: "original-session".into(),
            limits: Default::default(),
        },
        Handle::current(),
        true,
        Some(fixture.probe.clone()),
        None,
    );
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    let (mut service, sender, target, _) = fixture_service();
    root.configure_payload_spill(&mut service, Default::default())
        .unwrap();
    service.subscribe(&target.agent_id).unwrap();
    let scope = root.storage_scope();
    let before = service.storage_observation_time(scope, 0).unwrap();
    let mut original = envelope(&sender, &target, "elapsed-root");
    original.payload = "original clocked source".into();
    original.ttl_ms = Some(60_000);
    let healthy_ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            original.clone(),
            MessageScope::Session,
            u64::MAX,
            4096,
            None,
        )
        .unwrap();
    root.start(&mut service, &healthy_ticket, None).unwrap();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::AwaitingAdoption
    );
    root.adopt(&mut service, None).unwrap();
    assert_eq!(
        service
            .receive_subscribed(&target.agent_id, u64::MAX, 1)
            .unwrap()
            .messages
            .len(),
        1
    );
    original.id = "elapsed-root-lost-worker".into();
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            original.clone(),
            MessageScope::Session,
            u64::MAX,
            4096,
            None,
        )
        .unwrap();
    fixture
        .probe
        .panic_after_commit
        .store(true, Ordering::SeqCst);
    root.start(&mut service, &ticket, None).unwrap();
    let CustodyWorkerStatus::RetryAt(due) = root.observe().await.unwrap() else {
        panic!("actual postcommit join must fail")
    };
    let before_retry = fixture.disk();
    let clock_bytes: Vec<u8> = fixture
        .connection()
        .query_row("SELECT elapsed_clock FROM state", [], |row| row.get(0))
        .unwrap();
    // The only surviving store is on disk. Fail the root's actual original
    // reader through the same retained capability held by the canonical owner.
    let owner = root.0.ownership.lock().unwrap().as_ref().unwrap().clone();
    let mut store =
        MmpCustodyStore::open_existing(&fixture.path, "original-session", Default::default())
            .unwrap();
    {
        let bound = owner.bind_store(&mut store).unwrap();
        drop(bound);
    }
    store.invalidate_elapsed_clock_for_tests();
    drop(store);
    assert!(service.storage_observation_time(scope, 0).is_err());
    assert!(root.retry(&mut service, due).unwrap());
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::AwaitingAdoption
    );
    let accepted = root.adopt(&mut service, None).unwrap();
    assert!(accepted.accepted);
    assert_eq!(
        fixture.disk(),
        before_retry,
        "reconciliation must not write a second clock/source"
    );
    assert_eq!(
        fixture
            .connection()
            .query_row::<Vec<u8>, _, _>("SELECT elapsed_clock FROM state", [], |row| row.get(0))
            .unwrap(),
        clock_bytes
    );
    assert!(
        service.storage_observation_time(scope, u64::MAX).is_err(),
        "worker loss replaced failed original clock"
    );
    assert!(service.receive_subscribed(&target.agent_id, 0, 1).is_err());
    assert!(
        service
            .prepare_acceptance(
                &sender.agent_id,
                envelope(&sender, &target, "forbidden"),
                MessageScope::Session,
                0,
                4096,
                None
            )
            .is_err()
    );
    root.request_stop();
    drop(root);
    drop(service);
    drop(owner);
    wait_until_unlocked(&fixture.path).await;
    let store =
        MmpCustodyStore::open_existing(&fixture.path, "original-session", Default::default())
            .unwrap();
    let (source, _, _) = store
        .read_accepted_source(
            accepted.sequence,
            mez_agent::messaging::MessageAcceptanceDecodeLimits {
                payload_bytes: 1024,
                auxiliary_bytes: 8192,
                recipient_count: 16,
                recipient_bytes: 8192,
            },
        )
        .unwrap();
    assert!(source.accepted_at_ms() >= before);
    assert_ne!(source.accepted_at_ms(), u64::MAX);
    assert!(source.custody_clock_domain().is_some());
}
