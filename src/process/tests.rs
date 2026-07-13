//! Unit tests for pane process command planning and PTY lifecycle behavior.

use super::pane::append_output_chunk_to_backlog;
use super::{
    PaneProcessLaunch, PaneProcessManager, pane_command_plan, shell_command_from_argv,
    spawn_pane_process, spawn_pane_process_with_start_directory,
};
use crate::ids::IdFactory;
use crate::layout::Size;
use crate::runtime::PaneEnvironment;
use crate::runtime::pane_environment;
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Runs the test shell operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn test_shell() -> PaneProcessLaunch {
    PaneProcessLaunch::new(PathBuf::from("/bin/sh"))
}

/// Runs the test environment operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn test_environment() -> PaneEnvironment {
    let mut ids = IdFactory::default();
    pane_environment(
        Path::new("/tmp/mez-1000/default.sock"),
        &ids.session(),
        &ids.window(),
        &ids.pane(),
    )
    .unwrap()
}

/// Runs the wait for manager output activity operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn wait_for_manager_output_activity(
    manager: &PaneProcessManager,
    pane_id: &str,
    activity_sequence: Option<u64>,
) {
    if let Some(activity_sequence) = activity_sequence {
        let _ = manager.wait_for_output_activity_after(
            pane_id,
            activity_sequence,
            Duration::from_millis(10),
        );
    }
}

/// Verifies command plan uses interactive shell without explicit command.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn command_plan_uses_interactive_shell_without_explicit_command() {
    let plan = pane_command_plan(&test_shell(), None).unwrap();

    assert_eq!(plan.program, "/bin/sh");
    assert_eq!(plan.args, vec!["-i"]);
}

/// Verifies command plan execs explicit command inside shell.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn command_plan_execs_explicit_command_inside_shell() {
    let plan = pane_command_plan(&test_shell(), Some("printf ok")).unwrap();

    assert_eq!(plan.args, vec!["-c", "exec printf ok"]);
}

/// Verifies argv shell command quotes each argument for exec semantics.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn argv_shell_command_quotes_each_argument_for_exec_semantics() {
    let command = shell_command_from_argv(&[
        "printf".to_string(),
        "%s\\n".to_string(),
        "hello world".to_string(),
    ])
    .unwrap();
    let plan = pane_command_plan(&test_shell(), Some(&command)).unwrap();

    assert_eq!(command, "printf \"%s\\\\n\" 'hello world'");
    assert_eq!(
        plan.args,
        vec!["-c", "exec printf \"%s\\\\n\" 'hello world'"]
    );
}

/// Verifies argv shell command rejects empty program.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn argv_shell_command_rejects_empty_program() {
    let error = shell_command_from_argv(&["".to_string()]).unwrap_err();

    assert_eq!(error.kind(), mez_mux::MuxErrorKind::InvalidArgs);
}

/// Verifies explicit command must not be empty.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn explicit_command_must_not_be_empty() {
    let error = pane_command_plan(&test_shell(), Some(" ")).unwrap_err();

    assert_eq!(error.kind(), mez_mux::MuxErrorKind::InvalidArgs);
}

/// Verifies that pane output backlog buffering refuses chunks that would exceed
/// its byte limit instead of growing without bound or dropping partial terminal
/// escape sequences. The caller can keep the returned chunk pending until later
/// polls free enough backlog capacity.
#[test]
fn output_backlog_keeps_over_limit_chunks_pending() {
    let mut backlog = VecDeque::from(vec![b'a', b'b', b'c']);

    let pending = append_output_chunk_to_backlog(&mut backlog, vec![b'd', b'e'], 4);

    assert_eq!(pending, Some(vec![b'd', b'e']));
    assert_eq!(
        backlog.into_iter().collect::<Vec<_>>(),
        vec![b'a', b'b', b'c']
    );
}

/// Verifies spawns explicit command on pty.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn spawns_explicit_command_on_pty() {
    let mut process = spawn_pane_process(
        &test_shell(),
        Some("true"),
        &test_environment(),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();

    let status = process.wait().unwrap();

    assert!(status.success());
    assert!(process.primary_pid() > 0);
}

/// Verifies spawns explicit command from start directory.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn spawns_explicit_command_from_start_directory() {
    let root = std::env::temp_dir().join(format!("mez-pwd-test-{}", std::process::id()));
    let cwd = root.join("cwd");
    let output = root.join("pwd.txt");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&cwd).unwrap();
    let command = format!("pwd > {}", output.display());
    let mut process = spawn_pane_process_with_start_directory(
        &test_shell(),
        Some(&command),
        &test_environment(),
        Size::new(80, 24).unwrap(),
        Some(&cwd),
    )
    .unwrap();

    let status = process.wait().unwrap();
    let actual = fs::read_to_string(&output).unwrap();

    assert!(status.success());
    assert_eq!(actual.trim_end(), cwd.to_string_lossy());
    let _ = fs::remove_dir_all(root);
}

/// Verifies that live pane processes can expose the host-reported process name,
/// which feeds `pane.process_name` frame fields when the platform supports it.
#[cfg(target_os = "linux")]
/// Verifies pane process exposes host process name when available.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn pane_process_exposes_host_process_name_when_available() {
    let mut process = spawn_pane_process(
        &test_shell(),
        Some("sleep 2"),
        &test_environment(),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();

    let mut process_name = None;
    for _ in 0..10_000 {
        process_name = process.process_name();
        if process_name.as_deref() == Some("sleep") {
            break;
        }
        std::thread::yield_now();
    }

    assert_eq!(process_name.as_deref(), Some("sleep"));
    let _status = process.terminate(Duration::from_millis(100)).unwrap();
}

/// Verifies that PTY foreground process-group metadata can identify the active
/// program even when the program does not emit output or OSC title sequences.
/// Runtime pane/window titles use this path for mux-like automatic title
/// updates while foreground jobs are running.
#[cfg(target_os = "linux")]
/// Verifies pane process exposes foreground process name when available.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn pane_process_exposes_foreground_process_name_when_available() {
    let mut process = spawn_pane_process(
        &test_shell(),
        Some("sleep 2"),
        &test_environment(),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();

    let mut foreground_name = None;
    for _ in 0..10_000 {
        foreground_name = process.foreground_process_name();
        if foreground_name.as_deref() == Some("sleep") {
            break;
        }
        std::thread::yield_now();
    }

    assert_eq!(foreground_name.as_deref(), Some("sleep"));
    let _status = process.terminate(Duration::from_millis(100)).unwrap();
}

/// Verifies pane process reads available pty output without blocking.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn pane_process_reads_available_pty_output_without_blocking() {
    let mut process = spawn_pane_process(
        &test_shell(),
        Some("printf pane-output"),
        &test_environment(),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();

    let mut output = Vec::new();
    for _ in 0..50 {
        let activity_sequence = process.output_activity_sequence();
        output.extend(process.read_available_output(4096).unwrap());
        if output
            .windows("pane-output".len())
            .any(|window| window == b"pane-output")
        {
            break;
        }
        let _ =
            process.wait_for_output_activity_after(activity_sequence, Duration::from_millis(10));
    }

    let status = process.wait().unwrap();
    assert!(status.success());
    assert!(String::from_utf8_lossy(&output).contains("pane-output"));
}

/// Verifies pane process sets term from environment.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn pane_process_sets_term_from_environment() {
    let output = std::env::temp_dir().join(format!("mez-term-test-{}", std::process::id()));
    let _ = fs::remove_file(&output);
    let command = format!("printf %s \"$TERM\" > {}", output.display());
    let mut process = spawn_pane_process(
        &test_shell(),
        Some(&command),
        &test_environment(),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();

    let status = process.wait().unwrap();
    let term = fs::read_to_string(&output).unwrap();

    assert!(status.success());
    assert_eq!(term, crate::terminal::DEFAULT_PANE_TERM);
    let _ = fs::remove_file(output);
}

/// Verifies pane process manager owns process by pane id until exit.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn pane_process_manager_owns_process_by_pane_id_until_exit() {
    let mut manager = PaneProcessManager::new();
    let pane_id = "%1";

    let pid = manager
        .spawn_for_pane(
            pane_id,
            &test_shell(),
            Some("true"),
            &test_environment(),
            Size::new(80, 24).unwrap(),
        )
        .unwrap();

    assert_eq!(manager.primary_pid(pane_id), Some(pid));
    assert_eq!(manager.len(), 1);
    let status = manager.wait_and_remove(pane_id).unwrap();
    assert!(status.success());
    assert!(manager.is_empty());
}

/// Verifies pane process manager writes input to pane pty.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn pane_process_manager_writes_input_to_pane_pty() {
    let mut manager = PaneProcessManager::new();
    let pane_id = "%1";
    manager
        .spawn_for_pane(
            pane_id,
            &test_shell(),
            Some("cat >/dev/null"),
            &test_environment(),
            Size::new(80, 24).unwrap(),
        )
        .unwrap();

    manager.write_pane_input(pane_id, b"hello\n").unwrap();
    manager.write_pane_input(pane_id, b"\x04").unwrap();
    let status = manager.wait_and_remove(pane_id).unwrap();

    assert!(status.success());
}

/// Verifies pane process manager polls exits without forgetting process.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn pane_process_manager_polls_exits_without_forgetting_process() {
    let mut manager = PaneProcessManager::new();
    let pane_id = "%1";
    let pid = manager
        .spawn_for_pane(
            pane_id,
            &test_shell(),
            Some("true"),
            &test_environment(),
            Size::new(80, 24).unwrap(),
        )
        .unwrap();

    let mut exited = Vec::new();
    for _ in 0..50 {
        let activity_sequence = manager.output_activity_sequence(pane_id);
        exited = manager.poll_exited().unwrap();
        if !exited.is_empty() {
            break;
        }
        wait_for_manager_output_activity(&manager, pane_id, activity_sequence);
    }

    assert_eq!(exited.len(), 1);
    assert_eq!(exited[0].pane_id, pane_id);
    assert_eq!(exited[0].primary_pid, pid);
    assert!(exited[0].status.success);
    assert_eq!(manager.len(), 1);

    manager.remove_exited(pane_id).unwrap();
    assert!(manager.is_empty());
}

/// Verifies pane process manager rejects removing running process.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn pane_process_manager_rejects_removing_running_process() {
    let mut manager = PaneProcessManager::new();
    let pane_id = "%1";
    manager
        .spawn_for_pane(
            pane_id,
            &test_shell(),
            Some("sleep 1"),
            &test_environment(),
            Size::new(80, 24).unwrap(),
        )
        .unwrap();

    let error = manager.remove_exited(pane_id).unwrap_err();
    assert_eq!(error.kind(), mez_mux::MuxErrorKind::InvalidState);

    let status = manager.wait_and_remove(pane_id).unwrap();
    assert!(status.success());
}

/// Verifies that live pane processes can be removed from manager ownership and
/// restored without changing their process identity. Async pane tasks need this
/// handoff boundary before production can stop polling PTYs through the
/// synchronous manager.
#[test]
fn pane_process_manager_can_handoff_running_process() {
    let mut manager = PaneProcessManager::new();
    let pane_id = "%1";
    let pid = manager
        .spawn_for_pane(
            pane_id,
            &test_shell(),
            Some("sleep 30"),
            &test_environment(),
            Size::new(80, 24).unwrap(),
        )
        .unwrap();

    let process = manager.take_running_pane_process(pane_id).unwrap();

    assert!(manager.is_empty());
    assert_eq!(
        manager
            .insert_running_pane_process(pane_id, process)
            .unwrap(),
        pid
    );
    assert_eq!(manager.primary_pid(pane_id), Some(pid));
    let terminated = manager
        .terminate_pane_with_grace(pane_id, Duration::from_millis(50))
        .unwrap()
        .unwrap();
    assert_eq!(terminated.primary_pid, pid);
}

/// Verifies that exited processes cannot be handed to an async owner through
/// the running-process handoff path. Exited processes must keep using the
/// lifecycle settlement path so exit records are not silently skipped.
#[test]
fn pane_process_manager_rejects_async_handoff_after_exit() {
    let mut manager = PaneProcessManager::new();
    let pane_id = "%1";
    manager
        .spawn_for_pane(
            pane_id,
            &test_shell(),
            Some("true"),
            &test_environment(),
            Size::new(80, 24).unwrap(),
        )
        .unwrap();

    for _ in 0..50 {
        let activity_sequence = manager.output_activity_sequence(pane_id);
        if !manager.poll_exited().unwrap().is_empty() {
            break;
        }
        wait_for_manager_output_activity(&manager, pane_id, activity_sequence);
    }

    let error = manager.take_running_pane_process(pane_id).unwrap_err();

    assert_eq!(error.kind(), mez_mux::MuxErrorKind::InvalidState);
    manager.remove_exited(pane_id).unwrap();
}

/// Verifies pane process manager terminates tracked pane.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn pane_process_manager_terminates_tracked_pane() {
    let mut manager = PaneProcessManager::new();
    let pane_id = "%1";
    let pid = manager
        .spawn_for_pane(
            pane_id,
            &test_shell(),
            Some("sleep 30"),
            &test_environment(),
            Size::new(80, 24).unwrap(),
        )
        .unwrap();

    let terminated = manager
        .terminate_pane_with_grace(pane_id, Duration::from_millis(50))
        .unwrap()
        .unwrap();

    assert_eq!(terminated.pane_id, pane_id);
    assert_eq!(terminated.primary_pid, pid);
    assert!(manager.is_empty());
}

/// Verifies pane process manager retains ownership when termination fails.
///
/// Teardown may fail while signaling or waiting on the pane process. The
/// manager must keep the process handle in that case so callers can inspect,
/// retry, or clean up the pane through the normal lifecycle path instead of
/// losing ownership of a still-live child.
#[test]
fn pane_process_manager_retains_process_when_termination_fails() {
    let mut manager = PaneProcessManager::new();
    let pane_id = "%1";
    let pid = manager
        .spawn_for_pane(
            pane_id,
            &test_shell(),
            Some("sleep 30"),
            &test_environment(),
            Size::new(80, 24).unwrap(),
        )
        .unwrap();

    manager
        .processes
        .get_mut(pane_id)
        .unwrap()
        .process_group_leader = Some(-1);
    let error = manager
        .terminate_pane_with_grace(pane_id, Duration::from_millis(10))
        .unwrap_err();

    assert_eq!(error.kind(), mez_mux::MuxErrorKind::InvalidState);
    assert!(manager.contains_pane(pane_id));
    assert_eq!(manager.primary_pid(pane_id), Some(pid));

    manager
        .processes
        .get_mut(pane_id)
        .unwrap()
        .process_group_leader = None;
    let terminated = manager
        .terminate_pane_with_grace(pane_id, Duration::from_millis(50))
        .unwrap()
        .unwrap();
    assert_eq!(terminated.primary_pid, pid);
    assert!(manager.is_empty());
}

/// Verifies pane process manager terminate all removes every tracked process.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn pane_process_manager_terminate_all_removes_every_tracked_process() {
    let mut manager = PaneProcessManager::new();
    for pane_id in ["%1", "%2"] {
        manager
            .spawn_for_pane(
                pane_id,
                &test_shell(),
                Some("sleep 30"),
                &test_environment(),
                Size::new(80, 24).unwrap(),
            )
            .unwrap();
    }

    let terminated = manager.terminate_all().unwrap();

    assert_eq!(terminated.len(), 2);
    assert!(manager.is_empty());
}
