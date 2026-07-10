//! Async-runtime tests owned by supervision behavior.

use super::super::*;

/// Verifies that daemon supervision rejects invalid service sets before
/// spawning tasks. This matters because duplicate or missing listener names
/// would make later failure and shutdown reports ambiguous.
#[test]
fn async_runtime_service_supervisor_validates_service_set() {
    let empty_error = AsyncRuntimeServiceSupervisor::new(Vec::new()).unwrap_err();
    assert_eq!(empty_error.kind(), crate::error::MezErrorKind::InvalidArgs);

    let unnamed_error = AsyncRuntimeServiceSupervisor::new(vec![test_supervised_service(
        " ",
        AsyncRuntimeServiceExit::completed(0),
    )])
    .unwrap_err();
    assert_eq!(
        unnamed_error.kind(),
        crate::error::MezErrorKind::InvalidArgs
    );

    let duplicate_error = AsyncRuntimeServiceSupervisor::new(vec![
        test_supervised_service("control", AsyncRuntimeServiceExit::completed(0)),
        test_supervised_service("control", AsyncRuntimeServiceExit::completed(1)),
    ])
    .unwrap_err();
    assert_eq!(
        duplicate_error.kind(),
        crate::error::MezErrorKind::InvalidArgs
    );
    assert!(duplicate_error.message().contains("control"));
}

/// Exercises the successful path for multiple supervised services. The
/// assertion sorts by name so the test verifies task scheduling without
/// relying on Tokio's completion order for ready futures.
#[tokio::test(flavor = "current_thread")]
async fn async_runtime_service_supervisor_reports_named_completions() {
    let report = supervise_async_runtime_services(
        vec![
            test_supervised_service("control", AsyncRuntimeServiceExit::completed(1)),
            test_supervised_service("message", AsyncRuntimeServiceExit::completed(2)),
        ],
        std::future::pending(),
    )
    .await
    .unwrap();

    let mut services = report.services;
    services.sort_by(|left, right| left.name.cmp(&right.name));

    assert!(!report.shutdown_requested);
    assert_eq!(
        services,
        vec![
            AsyncRuntimeServiceReport {
                name: "control".to_string(),
                exit: AsyncRuntimeServiceExit::completed(1),
            },
            AsyncRuntimeServiceReport {
                name: "message".to_string(),
                exit: AsyncRuntimeServiceExit::completed(2),
            },
        ]
    );
}

/// Verifies that an auxiliary maintenance task does not keep supervision alive
/// after all primary services have completed. This protects daemon tests and
/// bounded listener runs from hanging behind the long-lived tick service while
/// still reporting that the tick task stopped without requesting shutdown.
#[tokio::test(flavor = "current_thread")]
async fn async_runtime_service_supervisor_stops_auxiliary_after_primary_completion() {
    let report = supervise_async_runtime_services(
        vec![
            test_supervised_service("control", AsyncRuntimeServiceExit::completed(1)),
            AsyncRuntimeService::new_auxiliary("tick", async {
                std::future::pending::<Result<AsyncRuntimeServiceExit>>().await
            }),
        ],
        std::future::pending(),
    )
    .await
    .unwrap();

    let mut services = report.services;
    services.sort_by(|left, right| left.name.cmp(&right.name));

    assert!(!report.shutdown_requested);
    assert_eq!(
        services,
        vec![
            AsyncRuntimeServiceReport {
                name: "control".to_string(),
                exit: AsyncRuntimeServiceExit::completed(1),
            },
            AsyncRuntimeServiceReport {
                name: "tick".to_string(),
                exit: AsyncRuntimeServiceExit::completed(0),
            },
        ]
    );
}

/// Ensures service task errors are propagated rather than hidden in a
/// nominal completion report. The service name is part of the diagnostic so
/// daemon startup can identify which listener failed.
#[tokio::test(flavor = "current_thread")]
async fn async_runtime_service_supervisor_propagates_named_failures() {
    let error = supervise_async_runtime_services(
        vec![AsyncRuntimeService::new("events", async {
            Err(MezError::invalid_state("listener exited unexpectedly"))
        })],
        std::future::pending(),
    )
    .await
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert!(error.message().contains("events"));
    assert!(error.message().contains("listener exited unexpectedly"));
}

/// Covers external cancellation of a long-lived listener task. The task
/// never completes on its own, so the cancellation future is the only route
/// to a bounded shutdown report.
#[tokio::test(flavor = "current_thread")]
async fn async_runtime_service_supervisor_reports_cancelled_services_as_shutdown() {
    use tokio::sync::oneshot;

    let (started_sender, started_receiver) = oneshot::channel();
    let (cancel_sender, cancel_receiver) = oneshot::channel();
    let pending_control = AsyncRuntimeService::new("control", async move {
        let _ = started_sender.send(());
        std::future::pending::<Result<AsyncRuntimeServiceExit>>().await
    });

    let supervision = supervise_async_runtime_services(vec![pending_control], async {
        let _ = cancel_receiver.await;
    });
    let canceller = async {
        started_receiver.await.unwrap();
        cancel_sender.send(()).unwrap();
    };

    let (report, ()) = tokio::join!(supervision, canceller);
    let report = report.unwrap();

    assert!(report.shutdown_requested);
    assert_eq!(
        report.services,
        vec![AsyncRuntimeServiceReport {
            name: "control".to_string(),
            exit: AsyncRuntimeServiceExit::shutdown(0),
        }]
    );
}
