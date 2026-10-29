//! Native navigation through the actual attached-input actor and worker service.
//!
//! Explicit gates separate command acceptance from blocking reads and actor
//! settlement. Tests observe both completion facts before shutdown, retain exact
//! native root evidence, and never infer process fate from display or cleanup.

use super::super::*;
use crate::host::async_runtime::{AsyncRuntimeSessionHandle, run_async_agent_command_service};
use crate::security::project::{ProjectTrustStore, TrustDecision};

/// User operation applied while the navigation worker is held after claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NavigationInput {
    /// Release the original navigation without cancelling it.
    Complete,
    /// Submit a slash cancellation across separate terminal packets.
    Stop,
    /// Interrupt with a control byte without hiding the conversation.
    CtrlC,
    /// Escape pending navigation instead of clearing an idle draft.
    Escape,
}

/// Removes only an exclusively generated fixture after its runtime is dropped.
struct NavigationDirectory(PathBuf);

impl Drop for NavigationDirectory {
    /// Filesystem cleanup is independent of the explicitly observed root exits.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Builds an ordinary attached-terminal byte-delivery plan.
fn input_plan(bytes: &[u8]) -> AttachedTerminalClientStepPlan {
    AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::ForwardToPane(bytes.to_vec())],
        output_lines: Vec::new(),
        output_line_style_spans: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    }
}

/// Renders through the actual handle while the command worker remains outside
/// actor ownership; no test reads mutable actor state through a side channel.
async fn navigation_view(handle: &AsyncRuntimeSessionHandle) -> String {
    handle
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(120, 32).unwrap(),
            TerminalClientLoopConfig::default(),
        )
        .await
        .unwrap()
        .unwrap()
        .lines
        .join("\n")
}

/// Waits for a manager-owned root to actually exec the bounded foreground
/// fixture. Spawn-time cached PID/executable evidence is insufficient here.
fn wait_for_sleep(pid: u32) -> mez_mux::process::ProcessInstanceIdentity {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Ok(identity) = mez_mux::process::process_executable_identity_for_pid(pid)
            && identity.executable_path.file_name() == Some(std::ffi::OsStr::new("sleep"))
        {
            return identity;
        }
        assert!(Instant::now() < deadline, "owned root did not reach sleep");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Exercises submit/claim/read/adopt-or-discard/worker-return through production
/// services. Assertions about cancellation run after receipt-driven worker and
/// actor completion and explicit owned-root cleanup, so a RED test cannot leave
/// an intentionally held worker or use a cleared banner as its success proof.
async fn navigation_worker_case(operation: NavigationInput) {
    let root = std::env::temp_dir().join(format!(
        "mez-async-native-navigation-{}-{}",
        std::process::id(),
        rand::random::<u128>()
    ));
    std::fs::create_dir(&root).unwrap();
    let directory = NavigationDirectory(root.canonicalize().unwrap());
    let source = directory.0.join("source");
    let target = directory.0.join("target");
    for path in [
        &source,
        &target,
        &directory.0.join(".git"),
        &directory.0.join("home"),
    ] {
        std::fs::create_dir(path).unwrap();
    }
    std::fs::write(target.join("AGENTS.md"), "async target guidance").unwrap();
    let daemon = std::env::current_dir().unwrap();
    let mut service = test_service();
    service.set_server_environment_for_tests(vec![
        mez_mux::process::RawEnvironmentEntry {
            key: b"HOME".to_vec(),
            value: directory
                .0
                .join("home")
                .to_str()
                .unwrap()
                .as_bytes()
                .to_vec(),
        },
        mez_mux::process::RawEnvironmentEntry {
            key: b"PATH".to_vec(),
            value: b"/usr/bin:/bin".to_vec(),
        },
    ]);
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "async-native-navigation".into(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\nshell_mode = \"native\"\n[permissions]\nsandbox = \"policy-only\"\n"
                .into(),
        }])
        .unwrap();
    let mut trust = ProjectTrustStore::default();
    trust
        .decide_at(directory.0.clone(), TrustDecision::Trusted, None, 1)
        .unwrap();
    service.set_project_trust_store(trust, None);
    let primary = service
        .attach_primary("primary", true, Size::new(120, 32).unwrap(), 120)
        .unwrap();
    let initial = service
        .start_initial_pane_process_with_start_directory(Some("sleep 120"), &source)
        .unwrap();
    let initial_identity = wait_for_sleep(initial.primary_pid);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let other_start = service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some("sleep 120"))
        .unwrap();
    let other_pid = other_start.primary_pid;
    let other = other_start.pane_id;
    let other_identity = wait_for_sleep(other_pid);
    let other_directory = mez_mux::process::current_working_directory_for_pid(other_pid).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume(other.as_str())
        .unwrap();
    service
        .session_mut_for_tests()
        .select_pane(&primary, "%1")
        .unwrap();
    let started = StdArc::new(tokio::sync::Notify::new());
    let release = StdArc::new(tokio::sync::Notify::new());
    let blocking_done = StdArc::new(tokio::sync::Notify::new());
    let settled = StdArc::new(tokio::sync::Notify::new());
    service.set_deferred_agent_command_probe_for_tests(started.clone(), release.clone());
    service.set_deferred_agent_command_completion_probe_for_tests(
        blocking_done.clone(),
        settled.clone(),
    );
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let stop = StdArc::new(AtomicBool::new(false));
    let client = async {
        let worker_handle = handle.clone();
        let worker_stop = stop.clone();
        let worker = tokio::spawn(async move {
            run_async_agent_command_service(
                &worker_handle,
                AsyncAgentProviderServiceConfig::new(1)
                    .unwrap()
                    .with_idle_interval(Duration::from_millis(5))
                    .unwrap(),
                move |_, state| {
                    worker_stop.load(Ordering::Acquire)
                        || matches!(state, RuntimeLifecycleState::Stopping)
                },
            )
            .await
        });
        handle
            .apply_attached_terminal_step_plan(primary.clone(), input_plan(b"/cd ../target\r"))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), started.notified())
            .await
            .expect("actual command service must claim navigation");
        let busy = navigation_view(&handle).await;
        handle
            .execute_terminal_command(primary.clone(), format!("select-pane -t {other}"))
            .await
            .unwrap();
        let other_input = handle
            .apply_attached_terminal_step_plan(primary.clone(), input_plan(b"draft"))
            .await
            .unwrap();
        let other_view = navigation_view(&handle).await;
        handle
            .execute_terminal_command(primary.clone(), "select-pane -t %1".into())
            .await
            .unwrap();
        match operation {
            NavigationInput::Complete => {}
            NavigationInput::Stop => {
                for bytes in [b"/st".as_slice(), b"op".as_slice(), b"\r".as_slice()] {
                    handle
                        .apply_attached_terminal_step_plan(primary.clone(), input_plan(bytes))
                        .await
                        .unwrap();
                }
            }
            NavigationInput::CtrlC => {
                handle
                    .apply_attached_terminal_step_plan(primary.clone(), input_plan(b"\x03"))
                    .await
                    .unwrap();
            }
            NavigationInput::Escape => {
                handle
                    .apply_attached_terminal_step_plan(primary.clone(), input_plan(b"\x1b"))
                    .await
                    .unwrap();
            }
        }
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(3), blocking_done.notified())
            .await
            .expect("blocking navigation read must actually return");
        tokio::time::timeout(Duration::from_secs(3), settled.notified())
            .await
            .expect("actor must answer navigation completion, including stale discard");
        stop.store(true, Ordering::Release);
        let completed = tokio::time::timeout(Duration::from_secs(3), worker)
            .await
            .expect("observed command worker must stop")
            .unwrap()
            .unwrap();
        handle.shutdown().await.unwrap();
        (
            busy,
            other_input.agent_prompt_inputs_applied,
            other_view,
            completed,
        )
    };
    let ((busy, other_inputs, other_view, completed), mut exit) = tokio::join!(client, actor.run());
    let effective = exit.service.agent_effective_working_directory("%1");
    let override_directory = exit
        .service
        .native_agent_working_directory("%1")
        .map(Path::to_path_buf);
    let phase = exit.service.agent_command_lifecycle_phase_for_tests("%1");
    let live_identity = mez_mux::process::process_executable_identity_for_pid(initial.primary_pid);
    let live_directory = mez_mux::process::current_working_directory_for_pid(initial.primary_pid);
    let live_other = mez_mux::process::process_executable_identity_for_pid(other_pid);
    let live_other_directory = mez_mux::process::current_working_directory_for_pid(other_pid);
    let current_conversation = exit
        .service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let providers = exit.service.pending_agent_provider_tasks();
    let exits = exit.service.terminate_all_pane_processes().unwrap();
    assert_eq!(exits.len(), 2);
    assert!(exit.service.tracked_runtime_pane_process_ids().is_empty());
    assert_eq!(completed, 1);
    assert!(busy.contains("command running"), "{busy}");
    assert_eq!(other_inputs, 1);
    assert!(other_view.contains("draft"), "{other_view}");
    assert_eq!(live_identity, Ok(initial_identity));
    assert_eq!(live_directory, Some(source.clone()));
    assert_eq!(live_other, Ok(other_identity));
    assert_eq!(live_other_directory, Some(other_directory));
    assert_eq!(std::env::current_dir().unwrap(), daemon);
    assert_eq!(current_conversation, conversation);
    assert!(providers.is_empty());
    if operation == NavigationInput::Complete {
        assert_eq!(effective, Some(target.clone()));
        assert_eq!(override_directory, Some(target));
        assert_eq!(
            phase,
            Some(crate::runtime::RuntimeAgentCommandLifecyclePhase::Completed)
        );
    } else {
        assert_eq!(
            effective,
            Some(source),
            "attached {operation:?} must cancel before the late worker can adopt"
        );
        assert_eq!(override_directory, None);
        assert_eq!(
            phase,
            Some(crate::runtime::RuntimeAgentCommandLifecyclePhase::Cancelled)
        );
    }
}

/// The real worker service must keep the actor and another pane responsive,
/// then adopt the target under its original command without moving either root.
#[tokio::test(flavor = "current_thread")]
async fn async_native_navigation_worker_keeps_other_panes_responsive() {
    navigation_worker_case(NavigationInput::Complete).await;
}

/// Typed Stop must survive packet boundaries; Ctrl+C and Escape must cancel
/// pending navigation without hiding the conversation. A late actual worker
/// result is observed and discarded, rather than being treated as relocation.
#[tokio::test(flavor = "current_thread")]
async fn async_native_navigation_attached_cancellation_discards_late_worker() {
    for operation in [
        NavigationInput::Stop,
        NavigationInput::CtrlC,
        NavigationInput::Escape,
    ] {
        navigation_worker_case(operation).await;
    }
}
