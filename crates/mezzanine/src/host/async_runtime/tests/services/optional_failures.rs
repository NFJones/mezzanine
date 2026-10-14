//! Optional-worker panic fixtures use per-service executors, never global hooks.

use super::super::*;
use crate::host::async_runtime::side_effects::run_status_pill_service_with_executors;

/// Real control ingress must stay usable after optional work fails, including
/// agent presentation requests; lifecycle observation alone cannot prove that.
async fn assert_control_progress(handle: &AsyncRuntimeSessionHandle) {
    use crate::control::{ControlConnectionState, decode_control_frame, encode_control_body};
    let initialized = handle.handle_control_input_for_connection(
        encode_control_body(r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"requested_role":"primary","requested_version":2,"client_name":"panic-survivor","client":{"name":"panic-survivor","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm"}}}}"#),
        1024 * 1024, ControlConnectionState::new(true, true),
    ).await.unwrap();
    let shown = handle.handle_control_input_for_connection(
        encode_control_body(r#"{"jsonrpc":"2.0","id":"show","method":"agent/shell/show","params":{"target":{"pane_id":"%1"},"idempotency_key":"panic-survivor-show"}}"#),
        1024 * 1024, initialized.connection,
    ).await.unwrap();
    let (body, _) = decode_control_frame(&shown.output, 1024 * 1024).unwrap();
    assert!(body.contains(r#""visible":true"#), "{body}");
}

/// A panicking async window child and twelve panicking blocking pane children
/// must settle once without stopping a real actor or two live pane processes.
/// The legacy window cache must apply its configured error behavior rather
/// than retaining the pending boot value; the pane queue must reuse all four
/// slots, cancel its batch fences, and finish without a restart or hang.
#[tokio::test]
async fn status_child_panics_settle_and_preserve_live_siblings() {
    let mut service = test_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "optional-panic".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r##"[frames.window]
right_status = "#{pill.failed}"
[frames.window.pills.failed]
command = "printf unused"
initial = "boot"
interval_seconds = 60
error_behavior = "show_error"
"##
            .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .split_pane_with_process(
            &primary,
            mez_mux::layout::SplitDirection::Vertical,
            Some("cat >/dev/null"),
        )
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let actor_task = tokio::spawn(actor.run());

    let initial = handle
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            TerminalClientLoopConfig::default(),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(initial.lines.iter().any(|line| line.contains("boot")));
    let plans = (0..12)
        .map(|index| {
            crate::runtime::RuntimePaneStatusProviderRefreshPlan::for_tests(
                &format!("panic-{index}"),
                "printf unused",
                &std::env::temp_dir(),
                1_000,
            )
        })
        .collect::<Vec<_>>();
    let cancellations = plans
        .iter()
        .map(|plan| plan.cancellation.clone())
        .collect::<Vec<_>>();
    handle
        .queue_runtime_side_effects(
            plans
                .into_iter()
                .map(|plan| RuntimeSideEffect::RefreshPaneStatusProvider {
                    plan: Box::new(plan),
                })
                .collect(),
        )
        .await
        .unwrap();

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        run_status_pill_service_with_executors(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 1,
                drain_limit: 16,
                idle_interval: Duration::from_millis(1),
            },
            |_, _| false,
            |_, _| async { panic!("controlled window child panic") },
            |_| panic!("controlled blocking pane child panic"),
        ),
    )
    .await
    .unwrap();

    // Clean real children even when the baseline containment assertion fails.
    let view = handle
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            TerminalClientLoopConfig::default(),
        )
        .await
        .unwrap()
        .unwrap();
    let mut processes = handle
        .take_running_pane_process_instances_for_adapter(8)
        .await
        .unwrap();
    assert_eq!(processes.len(), 2);
    for (instance, process) in &mut processes {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::PaneProcess {
            instance: instance.clone(),
            event: PaneProcessEvent::Pane(PaneEvent::Output {
                pane_id: instance.pane_id.clone(),
                bytes: b"healthy optional-failure sibling".to_vec(),
            }),
        });
        assert_eq!(
            handle.submit_runtime_events(batch).await.unwrap().applied,
            1
        );
        process.terminate(Duration::from_millis(10)).unwrap();
    }
    assert_eq!(
        handle.lifecycle_state().await.unwrap(),
        RuntimeLifecycleState::Running
    );
    assert_control_progress(&handle).await;
    handle.shutdown().await.unwrap();
    actor_task
        .await
        .unwrap()
        .service
        .terminate_all_pane_processes()
        .unwrap();

    let report = result.expect("optional status child panic must not fail the service");
    assert_eq!(report.drained, 13);
    assert_eq!(report.submitted_events, 13);
    assert_eq!(report.applied_events, 1);
    assert!(!view.lines.iter().any(|line| line.contains("boot")));
    assert!(
        cancellations
            .iter()
            .all(|cancellation| cancellation.is_cancelled())
    );
}

/// Actual proxy infrastructure failure must complete only the optional X11
/// service while the supervisor retains its live actor and real PTY siblings.
/// No X server is used; a reserved route exercises authority revocation.
#[tokio::test]
async fn x11_infrastructure_failure_preserves_supervised_actor_panes_and_control() {
    use crate::runtime::x11::{
        RuntimeX11Proxy, RuntimeX11RouteOwner, X11_FORWARDING_VERSION, X11AuthProtocol, X11Cookie,
        X11ForwardingMode, X11ForwardingOffer,
    };
    let root = std::env::temp_dir().join(format!(
        "mez-optional-x11-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    let proxy = RuntimeX11Proxy::prepare(&root).unwrap();
    let proxy_handle = proxy.handle();
    let mut service = test_service();
    service.set_runtime_x11_proxy(proxy_handle.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .split_pane_with_process(
            &primary,
            mez_mux::layout::SplitDirection::Vertical,
            Some("cat >/dev/null"),
        )
        .unwrap();
    let owner = RuntimeX11RouteOwner {
        session_id: "$test".to_string(),
        client_id: primary.to_string(),
        endpoint_id: "test-endpoint".to_string(),
        principal_id: None,
        connection_id: "test-connection".to_string(),
    };
    let (_, lease) = proxy_handle
        .reserve_route(
            owner,
            X11ForwardingOffer {
                version: X11_FORWARDING_VERSION,
                mode: X11ForwardingMode::Untrusted,
                auth_protocol: X11AuthProtocol::MitMagicCookie1,
                fake_cookie: X11Cookie::new([0x75; 16]),
                takeover: false,
            },
        )
        .unwrap();
    lease.activate_without_transport().unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let actor_task = tokio::spawn(actor.run());
    let (tx, rx) = tokio::sync::oneshot::channel();
    let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let supervision = tokio::spawn(supervise_async_runtime_services(
        vec![
            AsyncRuntimeService::new_auxiliary("x11-proxy", async move {
                let result = proxy.serve_with_test_faults(None, Some(rx)).await;
                let _ = finished_tx.send(());
                Ok(AsyncRuntimeServiceExit::completed(result?))
            }),
            AsyncRuntimeService::new("live-control-lifetime", async move {
                let _ = stop_rx.await;
                Ok(AsyncRuntimeServiceExit::completed(0))
            }),
        ],
        std::future::pending(),
    ));
    tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), finished_rx)
        .await
        .unwrap()
        .unwrap();
    assert_control_progress(&handle).await;
    let mut processes = handle
        .take_running_pane_process_instances_for_adapter(8)
        .await
        .unwrap();
    assert_eq!(processes.len(), 2);
    for (instance, process) in &mut processes {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::PaneProcess {
            instance: instance.clone(),
            event: PaneProcessEvent::Pane(PaneEvent::Output {
                pane_id: instance.pane_id.clone(),
                bytes: b"X11 failure sibling progress".to_vec(),
            }),
        });
        assert_eq!(
            handle.submit_runtime_events(batch).await.unwrap().applied,
            1
        );
        process.terminate(Duration::from_millis(10)).unwrap();
    }
    let stayed_supervised = !supervision.is_finished();
    let revoked = !proxy_handle.diagnostics().route_active;
    let _ = stop_tx.send(());
    let report = supervision.await.unwrap();
    handle.shutdown().await.unwrap();
    actor_task
        .await
        .unwrap()
        .service
        .terminate_all_pane_processes()
        .unwrap();
    lease.deactivate().unwrap();
    drop(lease);
    drop(proxy_handle);
    let _ = std::fs::remove_dir_all(root);
    assert!(
        stayed_supervised,
        "X11 failure ended live sibling supervision"
    );
    assert!(revoked, "X11 failure retained authority");
    assert!(!report.unwrap().shutdown_requested);
}

/// Optional child containment must never conceal loss of actor ownership: no
/// actor means even the first drain must propagate its typed ingress error.
#[tokio::test]
async fn status_worker_actor_ingress_failure_remains_fatal() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    drop(actor);
    let error = crate::host::async_runtime::run_async_status_pill_side_effect_service(
        &handle,
        AsyncRuntimeSideEffectServiceConfig {
            max_polls: 1,
            drain_limit: 8,
            idle_interval: Duration::from_millis(1),
        },
        |_, _| false,
    )
    .await
    .unwrap_err();
    assert!(error.message().contains("actor is closed"), "{error}");
}
