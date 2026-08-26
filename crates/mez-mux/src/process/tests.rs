//! Unit tests for pane process command planning and PTY lifecycle behavior.

use super::pane::{
    append_output_chunk_to_backlog, drain_output_backlog, foreground_process_group_id_from_raw,
    shell_input_acknowledgement_count,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use super::process_metadata::process_environment_for_pid;
#[cfg(target_os = "linux")]
use super::process_metadata::process_executable_path_for_pid;
use super::process_metadata::{
    parse_environment_bytes, parse_macos_environment_bytes, process_credentials_for_pid,
};
use super::{
    PTY_INPUT_WRITE_CHUNK_BYTES, PaneProcessEnvironment, PaneProcessLaunch, PaneProcessManager,
    ShellInputDelivery, pane_command_plan, shell_command_from_argv, spawn_pane_process,
    spawn_pane_process_with_start_directory,
};
use mez_terminal::TerminalSize as Size;
use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

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
fn test_environment() -> PaneProcessEnvironment {
    PaneProcessEnvironment {
        mez: "/tmp/mez-1000/default.sock,$1,@1,%1,mez-pane".to_string(),
        session: "$1".to_string(),
        window: "@1".to_string(),
        pane: "%1".to_string(),
        term: "mez-pane".to_string(),
    }
}

/// Verifies absent or invalid host foreground groups are not treated as PIDs.
///
/// Darwin can report zero from `tcgetpgrp` while a PTY temporarily has no
/// foreground owner. The PTY adapter must represent that lifecycle state as
/// unavailable instead of passing it to an API that requires a positive PID.
#[test]
fn foreground_process_group_conversion_rejects_non_positive_values() {
    assert_eq!(foreground_process_group_id_from_raw(-1), None);
    assert_eq!(foreground_process_group_id_from_raw(0), None);
    assert_eq!(foreground_process_group_id_from_raw(42), Some(42));
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

    assert_eq!(error.kind(), crate::MuxErrorKind::InvalidArgs);
}

/// Verifies explicit command must not be empty.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn explicit_command_must_not_be_empty() {
    let error = pane_command_plan(&test_shell(), Some(" ")).unwrap_err();

    assert_eq!(error.kind(), crate::MuxErrorKind::InvalidArgs);
}

/// Verifies that pane output backlog buffering refuses chunks that would exceed
/// its byte limit instead of growing without bound or dropping partial terminal
/// escape sequences. The caller can keep the returned chunk pending until later
/// polls free enough backlog capacity.
#[test]
fn output_backlog_keeps_over_limit_chunks_pending() {
    let mut backlog = VecDeque::from(vec![b'a', b'b', b'c']);

    let appended = append_output_chunk_to_backlog(&mut backlog, b"de", 4);

    assert!(!appended);
    assert_eq!(
        backlog.into_iter().collect::<Vec<_>>(),
        vec![b'a', b'b', b'c']
    );
}

/// Verifies bulk output draining preserves order across the two contiguous
/// slices of a wrapped deque and leaves the unread suffix buffered.
#[test]
fn output_backlog_bulk_drain_preserves_wraparound_order() {
    let mut backlog = VecDeque::with_capacity(5);
    backlog.extend(*b"abcd");
    assert_eq!(drain_output_backlog(&mut backlog, 3), b"abc");
    backlog.extend(*b"efg");

    assert_eq!(drain_output_backlog(&mut backlog, 3), b"def");
    assert_eq!(backlog.into_iter().collect::<Vec<_>>(), b"g");
}

/// Verifies incremental shell acknowledgement accounting observes only raw
/// receiver bytes in each new PTY chunk, independent of backlog wrap or drain.
#[test]
fn shell_input_acknowledgements_are_counted_per_new_chunk() {
    assert_eq!(shell_input_acknowledgement_count(b"visible"), 0);
    assert_eq!(
        shell_input_acknowledgement_count(&[
            b'a',
            super::SHELL_INPUT_RECORD_ACK_BYTE,
            b'b',
            super::SHELL_INPUT_RECORD_ACK_BYTE,
        ]),
        2
    );
}

/// Verifies typed shell deliveries reject an oversized newline-delimited
/// record before any pane owner can mistake a physical PTY chunk for a semantic
/// shell boundary. Diagnostics must identify the contract violation without
/// retaining or rendering the generated payload.
#[test]
fn shell_input_delivery_rejects_oversized_logical_records_without_payload() {
    let mut bytes = b"private-generated-source:".to_vec();
    bytes.resize(PTY_INPUT_WRITE_CHUNK_BYTES + 1, b'x');
    let delivery = ShellInputDelivery::generated_source_for_transaction(bytes, "delivery-1");

    let error = delivery.validate_logical_records().unwrap_err();

    assert_eq!(error.record_index(), 0);
    assert_eq!(error.record_len(), PTY_INPUT_WRITE_CHUNK_BYTES + 1);
    assert_eq!(error.max_record_len(), PTY_INPUT_WRITE_CHUNK_BYTES);
    assert_eq!(error.delivery_id(), Some("delivery-1"));
    assert!(!error.to_string().contains("private-generated-source"));
}

/// Verifies logical-record validation accepts multiple bounded records and a
/// final unterminated record. The final suffix is still one complete delivery
/// record even though it has no trailing interactive newline.
#[test]
fn shell_input_delivery_accepts_bounded_final_unterminated_record() {
    let mut bytes = vec![b'a'; PTY_INPUT_WRITE_CHUNK_BYTES - 1];
    bytes.push(b'\n');
    bytes.extend(std::iter::repeat_n(b'b', PTY_INPUT_WRITE_CHUNK_BYTES));
    let delivery = ShellInputDelivery::generated_source(bytes);

    delivery.validate_logical_records().unwrap();
}

/// Verifies a delayed acknowledgement from earlier generated work cannot be
/// attributed to the next Darwin shell-input record.
///
/// The stale byte is made readable before a three-record delivery starts. The
/// first two records each delay their own acknowledgement, and the second also
/// creates a file. Delivery may return only after that second acknowledgement,
/// so the file proves pacing did not advance one record ahead by consuming the
/// pre-existing byte.
#[cfg(target_os = "macos")]
#[test]
fn macos_shell_input_pacing_drains_stale_acknowledgement_before_record_boundary() {
    let root = std::env::temp_dir().join(format!(
        "mez-shell-ack-boundary-test-{}",
        std::process::id()
    ));
    let stale_acknowledgement_ready = root.join("stale-acknowledgement-ready");
    let side_effect = root.join("second-record-complete");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let mut process = spawn_pane_process(
        &test_shell(),
        None,
        &test_environment(),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();

    let startup_sequence = process.output_activity_sequence();
    assert!(process.wait_for_output_activity_after(startup_sequence, Duration::from_secs(2)));
    let _ = process.read_available_output(4096).unwrap();
    process
        .write_input(
            format!(
                "printf '\\036'; printf ready > '{}'\n",
                stale_acknowledgement_ready.display()
            )
            .as_bytes(),
        )
        .unwrap();
    let stale_deadline = Instant::now() + Duration::from_secs(2);
    while !stale_acknowledgement_ready.is_file() && Instant::now() < stale_deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        stale_acknowledgement_ready.is_file(),
        "shell did not publish the stale acknowledgement boundary"
    );

    let delivery = ShellInputDelivery::generated_source(
        format!(
            "sleep 0.2; printf '\\036'\nsleep 0.4; printf done > '{}'; printf '\\036'\n:\n",
            side_effect.display()
        )
        .into_bytes(),
    );
    process.write_shell_delivery(&delivery).unwrap();

    assert!(
        side_effect.is_file(),
        "delivery returned before the second record acknowledgement"
    );
    process.terminate(Duration::from_millis(100)).unwrap();
    let _ = fs::remove_dir_all(root);
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
    let canonical_cwd = fs::canonicalize(&cwd).unwrap();

    assert!(status.success());
    assert_eq!(actual.trim_end(), canonical_cwd.to_string_lossy());
    let _ = fs::remove_dir_all(root);
}

/// Verifies that live pane processes can expose the host-reported process name,
/// which feeds `pane.process_name` frame fields when the platform supports it.
#[cfg(any(target_os = "linux", target_os = "macos"))]
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
#[cfg(any(target_os = "linux", target_os = "macos"))]
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
    assert_eq!(term, "mez-pane");
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
    let deadline = Instant::now() + Duration::from_secs(2);
    let status = loop {
        let activity_sequence = manager.output_activity_sequence(pane_id);
        let _ = manager.read_available_output(4096).unwrap();
        if let Some(exited) = manager.poll_exited().unwrap().into_iter().next() {
            break exited.status;
        }
        assert!(
            Instant::now() < deadline,
            "pane process did not exit after receiving terminal EOF"
        );
        wait_for_manager_output_activity(&manager, pane_id, activity_sequence);
    };

    assert!(status.success());
    manager.remove_exited(pane_id).unwrap();
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

    let deadline = Instant::now() + Duration::from_secs(1);
    let mut exited = Vec::new();
    while Instant::now() < deadline {
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
            Some("sleep 30"),
            &test_environment(),
            Size::new(80, 24).unwrap(),
        )
        .unwrap();

    let error = manager.remove_exited(pane_id).unwrap_err();
    assert_eq!(error.kind(), crate::MuxErrorKind::InvalidState);

    let terminated = manager
        .terminate_pane_with_grace(pane_id, Duration::from_millis(10))
        .unwrap();
    assert!(terminated.is_some());
    assert!(manager.is_empty());
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

    let deadline = Instant::now() + Duration::from_secs(1);
    let mut exited = Vec::new();
    while Instant::now() < deadline {
        let activity_sequence = manager.output_activity_sequence(pane_id);
        exited = manager.poll_exited().unwrap();
        if !exited.is_empty() {
            break;
        }
        wait_for_manager_output_activity(&manager, pane_id, activity_sequence);
    }

    assert_eq!(exited.len(), 1);
    let error = manager.take_running_pane_process(pane_id).unwrap_err();

    assert_eq!(error.kind(), crate::MuxErrorKind::InvalidState);
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

    assert_eq!(error.kind(), crate::MuxErrorKind::InvalidState);
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

/// Verifies raw environment parsing handles the procfs NUL-separated format.
///
/// The parser must keep non-UTF-8 value bytes intact, preserve empty values,
/// and drop malformed segments instead of fabricating entries.
#[test]
fn environment_parsing_preserves_non_utf8_values_and_drops_malformed() {
    let bytes = b"PATH=/usr/bin\0LANG=en_US.UTF-8\0BROKEN_SEGMENT\0ODD=\xFF\xFE\0=EMPTY_KEY\0EMPTY_VALUE=\0\0";
    let entries = parse_environment_bytes(bytes);
    assert_eq!(entries.len(), 4);
    assert_eq!(entries[0].key, b"PATH");
    assert_eq!(entries[0].value, b"/usr/bin");
    assert_eq!(entries[1].key, b"LANG");
    assert_eq!(entries[1].value, b"en_US.UTF-8");
    assert_eq!(entries[2].key, b"ODD");
    assert_eq!(entries[2].value, b"\xFF\xFE");
    assert_eq!(entries[3].key, b"EMPTY_VALUE");
    assert!(entries[3].value.is_empty());
}

/// Verifies the macOS parser follows the real `KERN_PROCARGS2` layout.
///
/// The parser must skip the leading argc field, the separately stored
/// executable path and its padding, then exactly argc argv strings before
/// returning the environment region.
#[test]
fn macos_environment_parser_skips_executable_padding_and_argv() {
    let mut buffer = 3_i32.to_ne_bytes().to_vec();
    buffer.extend_from_slice(b"/bin/zsh\0");
    buffer.extend_from_slice(&[0; 7]);
    buffer.extend_from_slice(b"/bin/zsh\0-l\0-c\0");
    buffer.extend_from_slice(b"PATH=/usr/bin\0HOME=/home/neil\0\0");

    let region = parse_macos_environment_bytes(&buffer).expect("real layout parses");
    assert_eq!(region, b"PATH=/usr/bin\0HOME=/home/neil\0\0");

    let mut truncated = 2_i32.to_ne_bytes().to_vec();
    truncated.extend_from_slice(b"/bin/zsh\0\0/bin/zsh\0");
    assert_eq!(parse_macos_environment_bytes(&truncated), None);
}

/// Verifies the Linux environment reader reports the test process's own
/// exec-time environment.
#[cfg(target_os = "linux")]
#[test]
fn linux_environment_reader_reports_own_process_environment() {
    let entries =
        process_environment_for_pid(std::process::id()).expect("own environ readable on Linux");
    assert!(
        !entries.is_empty(),
        "test process always has an environment"
    );
    assert!(entries.iter().all(|entry| !entry.key.is_empty()));
}

/// Verifies the Darwin environment reader handles the live kernel layout.
#[cfg(target_os = "macos")]
#[test]
fn macos_environment_reader_reports_own_process_environment() {
    let entries =
        process_environment_for_pid(std::process::id()).expect("own environ readable on macOS");
    assert!(
        !entries.is_empty(),
        "the test process always has an environment"
    );
    assert!(entries.iter().all(|entry| !entry.key.is_empty()));
}

/// Verifies the Linux executable reader reports a real absolute path for the
/// test process itself.
#[cfg(target_os = "linux")]
#[test]
fn linux_executable_reader_reports_own_process_path() {
    let path = process_executable_path_for_pid(std::process::id())
        .expect("own executable readable on Linux");
    assert!(path.is_absolute());
}

/// Verifies the native credential reader reports the current process identity.
///
/// Native Bubblewrap dispatch derives the pane root process's effective UID
/// and primary GID through this boundary. Reading the test process provides a
/// portable live-kernel regression for both the procfs and Darwin libproc
/// implementations without relying on an external process's visibility.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn native_credential_reader_reports_own_process_identity() {
    let credentials = process_credentials_for_pid(std::process::id())
        .expect("own process credentials are readable on supported hosts");

    assert_eq!(credentials.user_id, unsafe { libc::geteuid() });
    assert_eq!(credentials.primary_group_id, unsafe { libc::getegid() });
    assert!(
        !credentials
            .supplementary_group_ids
            .contains(&credentials.primary_group_id),
        "the primary group must not be duplicated among supplementary groups"
    );
}
