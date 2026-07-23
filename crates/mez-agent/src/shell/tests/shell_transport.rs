//! Agent tests for shell transport behavior.
//!
//! This bounded leaf owns the scenarios for this concern while shared
//! fixtures remain in the parent module.

use super::*;
use crate::{
    SHELL_OUTPUT_BASE64_DROPPED_BYTES_MARKER, decode_shell_output_transport_with_diagnostics,
};

#[test]
/// Verifies that the agent subshell handoff rejects unresolved shell paths.
///
/// Agent mode needs to launch the same resolved shell that owns the pane. A
/// relative path would make the child-shell boundary depend on mutable working
/// directory state and could silently enter a different executable.
fn agent_subshell_enter_command_rejects_relative_shell_path() {
    let error =
        agent_subshell_enter_command(Path::new("sh"), ShellClassification::PosixSh).unwrap_err();

    assert_eq!(error.kind(), AgentShellValidationErrorKind::InvalidArgs);
}

#[test]
/// Verifies startup-suppressed agent subshell handoffs for shells with known
/// rc-file bypass flags.
///
/// The persistent agent shell still inherits the pane environment, but the
/// handoff must remove startup and prompt-hook variables and use shell-specific
/// no-rc flags so user prompt customization cannot block agent delivery.
fn agent_subshell_enter_command_suppresses_shell_startup_hooks() {
    let bash =
        agent_subshell_enter_command(Path::new("/bin/bash"), ShellClassification::Bash).unwrap();
    let zsh =
        agent_subshell_enter_command(Path::new("/bin/zsh"), ShellClassification::Zsh).unwrap();
    let fish =
        agent_subshell_enter_command(Path::new("/bin/fish"), ShellClassification::Fish).unwrap();

    assert!(
        bash.contains("command env -u BASH_ENV -u ENV -u ZDOTDIR"),
        "{bash}"
    );
    assert!(bash.contains("PROMPT_COMMAND=''"), "{bash}");
    assert!(bash.contains("'/bin/bash' --noprofile --norc"), "{bash}");
    assert!(
        zsh.contains("command env -u BASH_ENV -u ENV -u ZDOTDIR"),
        "{zsh}"
    );
    assert!(zsh.contains("'/bin/zsh' -f"), "{zsh}");
    assert!(
        fish.contains("command env -u BASH_ENV -u ENV -u ZDOTDIR"),
        "{fish}"
    );
    assert!(fish.contains("fish_private_mode=1"), "{fish}");
    assert!(fish.contains("'/bin/fish' --no-config"), "{fish}");
}

#[test]
/// Verifies Bash shell transactions ignore inherited `BASH_ENV` startup hooks.
///
/// `BASH_ENV` is a common non-interactive startup vector. Agent actions should
/// inherit ordinary pane environment values while removing this hook before
/// invoking the child command shell.
fn bash_wrapper_unsets_bash_env_before_child_shell_startup() {
    if !Path::new("/bin/bash").exists() {
        return;
    }
    let temp = test_temp_dir("bash-env-suppression");
    let hook = temp.join("hook.bash");
    std::fs::write(&hook, "printf '%s\\n' BASH_ENV_RAN\n").unwrap();
    let transaction = ShellTransaction::new(
        marker(),
        "t1",
        "a1",
        "p1",
        Path::new("/bin/bash"),
        "printf '%s\\n' ACTION_RAN",
    )
    .unwrap();
    let input = transaction.render_for_classification_input(ShellClassification::Bash);
    let wrapper = input.combined();

    assert!(
        wrapper.contains("'/bin/bash' --noprofile --norc \"$MEZ_COMMAND_FILE\""),
        "{wrapper}"
    );
    let mut command = Command::new("env");
    command.arg(format!("BASH_ENV={}", hook.display()));
    command.arg("/bin/sh");
    let output = run_command_transaction_stdin(&mut command, &input, "");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "status={:?} stdout={stdout:?} stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("ACTION_RAN"), "{stdout:?}");
    assert!(!stdout.contains("BASH_ENV_RAN"), "{stdout:?}");
    std::fs::remove_dir_all(temp).unwrap();
}

#[test]
/// Verifies that the Fish transaction wrapper uses Fish syntax while
/// materializing isolated commands through a temporary script file. This
/// catches regressions where large action payloads are embedded as one `-c`
/// argument or emitted through heredoc-like shell input.
fn fish_wrapper_materializes_command_file_with_fish_syntax() {
    let transaction = ShellTransaction::new(
        marker(),
        "turn'1",
        "agent-%1",
        "%1",
        Path::new("/opt/homebrew/bin/fish"),
        "echo 'hello fish'",
    )
    .unwrap();

    let wrapper = transaction.render_fish();

    assert!(wrapper.contains("set -l MEZ_MARKER_TOKEN '"));
    assert!(wrapper.contains("fish_private_mode"));
    assert!(wrapper.contains("history delete --prefix --case-sensitive"));
    assert!(wrapper.contains("TERM='dumb'"), "{wrapper}");
    assert!(wrapper.contains("PAGER='cat'"), "{wrapper}");
    assert!(wrapper.contains("GIT_PAGER='cat'"), "{wrapper}");
    assert!(wrapper.contains("LESSSECURE='1'"), "{wrapper}");
    assert!(wrapper.contains("GIT_TERMINAL_PROMPT='0'"), "{wrapper}");
    assert!(
        wrapper.contains("command setsid -w env -u BASH_ENV -u ENV -u ZDOTDIR"),
        "{wrapper}"
    );
    assert!(
        wrapper.contains("command env -u BASH_ENV -u ENV -u ZDOTDIR"),
        "{wrapper}"
    );
    assert!(wrapper.contains("MEZ_COMMAND_B64"), "{wrapper}");
    assert!(
        wrapper.contains("base64 -d < \"$MEZ_COMMAND_B64\""),
        "{wrapper}"
    );
    assert!(
        wrapper.contains("base64 -D < \"$MEZ_COMMAND_B64\""),
        "{wrapper}"
    );
    assert!(wrapper.contains("'/opt/homebrew/bin/fish' --no-config \"$MEZ_COMMAND_FILE\""));
    assert!(!wrapper.contains("'/opt/homebrew/bin/fish' -c"));
    assert!(!wrapper.contains("echo \\'hello fish\\'"));
    assert!(!wrapper.contains("echo 'hello fish'"));
    assert!(
        wrapper
            .lines()
            .all(|line| line.len() <= SHELL_TRANSACTION_COMMAND_BASE64_LINE_BYTES + 180),
        "{wrapper}"
    );
    assert!(!wrapper.contains("fish <<"));
    assert!(!wrapper.contains("command cat > \"$MEZ_COMMAND_FILE\""));
    assert!(!wrapper.contains("env -u MEZ_MARKER_TOKEN"));
}

#[test]
/// Verifies marker token requires 128 bits of hex.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn marker_token_requires_128_bits_of_hex() {
    let error = MarkerToken::new("short").unwrap_err();

    assert_eq!(error.kind(), AgentShellValidationErrorKind::InvalidArgs);
}

#[test]
/// Verifies that the POSIX agent subshell handoff launches a child shell while
/// preserving strict parent-shell options and history suppression cleanup.
///
/// The parent shell parses the whole handoff line, waits for the child shell to
/// exit, then resumes with its previous `errexit` and `nounset` state. This is
/// the behavior that keeps agent-mode prompt mutations scoped away from the
/// user's original pane shell.
fn posix_agent_subshell_enter_command_preserves_parent_shell_after_child_exit() {
    let handoff =
        agent_subshell_enter_command(Path::new("/bin/sh"), ShellClassification::PosixSh).unwrap();
    let script = format!(
        "set -eu\n{handoff}case $- in *e*u*|*u*e*) printf '%s\\n' STRICT_PARENT_ALIVE;; *) printf '%s\\n' STRICT_PARENT_LOST:$-;; esac\n"
    );

    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "{output:?}");
    assert!(
        handoff.contains("command env -u BASH_ENV -u ENV -u ZDOTDIR"),
        "{handoff}"
    );
    assert!(handoff.contains("HISTFILE=/dev/null"), "{handoff}");
    assert!(handoff.contains("PROMPT_COMMAND=''"), "{handoff}");
    assert!(handoff.contains("PS1='$ '"), "{handoff}");
    assert!(handoff.contains("'/bin/sh'"), "{handoff}");
    assert!(handoff.contains("history -d $((HISTCMD-1))"), "{handoff}");
    assert!(stdout.contains("STRICT_PARENT_ALIVE"), "{stdout}");
}

#[test]
/// Verifies that a POSIX stateful shell transaction also protects the active
/// pane shell from strict options while preserving the status marker. Stateful
/// commands run directly in the pane shell, so this guard prevents a failed
/// agent command from closing the user's interactive session.
fn posix_stateful_wrapper_preserves_parent_shell_with_errexit_enabled() {
    let transaction =
        ShellTransaction::new(marker(), "t1", "a1", "p1", Path::new("/bin/sh"), "false").unwrap();
    let wrapper = transaction.render_stateful();
    let script = format!("set -eu\n{wrapper}\nprintf '%s\\n' PARENT_SHELL_ALIVE\n");

    let output = run_sh_stdin(&script);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "status={:?} stdout={stdout:?} stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("\u{1b}]133;D;1;"), "{stdout:?}");
    assert!(stdout.contains("PARENT_SHELL_ALIVE"), "{stdout:?}");
}

#[test]
/// Verifies that the POSIX stateful wrapper emits valid brace-group syntax and
/// passes shell-quoted marker metadata as shell words instead of embedding
/// literal quote characters inside double-quoted arguments.
fn posix_stateful_wrapper_uses_valid_brace_group_and_marker_words() {
    let transaction =
        ShellTransaction::new(marker(), "t1", "a1", "p1", Path::new("/bin/sh"), "cd /tmp").unwrap();

    let wrapper = transaction.render_stateful();

    assert!(wrapper.contains("{\ncd /tmp\n}\n"));
    assert!(wrapper.contains("MEZ_STATUS=$?"));
    assert!(wrapper.contains("'0123456789abcdef0123456789abcdef' 't1' 'a1' 'p1'"));
    assert!(!wrapper.contains("\"'0123456789abcdef0123456789abcdef'\""));
    assert!(wrapper.contains("unset MEZ_STATUS"));
}

#[test]
/// Verifies posix wrapper contains start and end markers.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn posix_wrapper_contains_start_and_end_markers() {
    let transaction =
        ShellTransaction::new(marker(), "t1", "a1", "p1", Path::new("/bin/sh"), "pwd").unwrap();

    let wrapper = transaction.render_posix();

    assert!(wrapper.contains("]133;C;mez_marker="));
    assert!(wrapper.contains("]133;D;%s;mez_marker="));
    assert!(wrapper.contains("env -u MEZ_MARKER_TOKEN"));
    assert!(wrapper.contains("command env -u MEZ_MARKER_TOKEN"));
    assert!(wrapper.contains("TERM='dumb'"), "{wrapper}");
    assert!(wrapper.contains("PAGER='cat'"), "{wrapper}");
    assert!(wrapper.contains("GIT_PAGER='cat'"), "{wrapper}");
    assert!(wrapper.contains("MANPAGER='cat'"), "{wrapper}");
    assert!(wrapper.contains("SYSTEMD_PAGER='cat'"), "{wrapper}");
    assert!(wrapper.contains("LESSSECURE='1'"), "{wrapper}");
    assert!(wrapper.contains("GIT_TERMINAL_PROMPT='0'"), "{wrapper}");
    assert!(wrapper.contains("GIT_EDITOR='true'"), "{wrapper}");
    assert!(
        wrapper.contains("DEBIAN_FRONTEND='noninteractive'"),
        "{wrapper}"
    );
    assert!(wrapper.contains("-u BASH_ENV"), "{wrapper}");
    assert!(wrapper.contains("-u ENV"), "{wrapper}");
    assert!(wrapper.contains("-u ZDOTDIR"), "{wrapper}");
    assert!(wrapper.contains("-u PROMPT_COMMAND"), "{wrapper}");
    assert!(wrapper.contains("command printf '\\033]133;C;"));
    assert!(wrapper.contains("/bin/sh"));
    assert!(wrapper.contains("command setsid -w"), "{wrapper}");
    assert!(wrapper.contains("MEZ_COMMAND_B64"));
    assert!(wrapper.contains("base64 -d < \"$MEZ_COMMAND_B64\""));
    assert!(wrapper.contains("base64 -D < \"$MEZ_COMMAND_B64\""));
    assert!(wrapper.contains("__mez_tx_"), "{wrapper}");
    let invocation = "__mez_tx_0123456789abcdef";
    let payload_end = "__MEZ_COMMAND_PAYLOAD_END_0123456789abcdef0123456789abcdef__";
    assert!(wrapper.contains(&format!("\n{invocation}\n")), "{wrapper}");
    assert_eq!(wrapper.trim_end().lines().last(), Some(payload_end));
    assert!(
        wrapper.find(invocation).unwrap() < wrapper.find(payload_end).unwrap(),
        "{wrapper}"
    );
    assert!(!wrapper.contains("command cat > \"$MEZ_COMMAND_FILE\""));
    assert!(!wrapper.contains("<<"));
    assert!(!wrapper.contains("\npwd\n"));
    assert!(wrapper.contains("HISTFILE=/dev/null"));
    assert!(wrapper.contains("MEZ_RESTORE_NOUNSET=0"));
    assert!(wrapper.contains("set +u"));
    assert!(wrapper.contains("set +o history"));
    assert!(wrapper.contains("history -d $((HISTCMD-1))"));
    assert!(wrapper.contains("set -o history"));
    assert!(
        wrapper.contains("-u MEZ_HISTORY_RESTORE -u MEZ_HISTORY_HISTFILE_WAS_SET"),
        "{wrapper}"
    );
    assert!(
        wrapper.find("MEZ_RESTORE_HISTORY_NOW").unwrap() < wrapper.find("]133;D;").unwrap(),
        "{wrapper}"
    );
    assert!(
        wrapper.find("]133;D;").unwrap() < wrapper.rfind("MEZ_RESTORE_ERREXIT_NOW").unwrap(),
        "{wrapper}"
    );
}

#[test]
/// Verifies that the POSIX transaction wrapper materializes commands through
/// base64 chunks rather than heredocs while still executing shell-sensitive
/// command text and emitting the completion marker.
///
/// This prevents regressions where file-backed actions can strand the pane
/// shell in heredoc input mode before Mezzanine observes an OSC end marker.
fn posix_wrapper_materializes_command_from_base64_without_heredoc() {
    let command = "printf '%s\\n' 'WRAPPER_OK:$HOME:$(nope)'";
    let transaction =
        ShellTransaction::new(marker(), "t1", "a1", "p1", Path::new("/bin/sh"), command).unwrap();
    let input = transaction.render_for_classification_input(ShellClassification::PosixSh);
    let wrapper = input.combined();

    assert!(!wrapper.contains(command), "{wrapper}");
    assert!(!wrapper.contains("<<"), "{wrapper}");
    assert!(
        !wrapper.contains("command cat > \"$MEZ_COMMAND_FILE\""),
        "{wrapper}"
    );
    assert!(
        wrapper
            .lines()
            .all(|line| line.len() <= SHELL_TRANSACTION_COMMAND_BASE64_LINE_BYTES + 160),
        "{wrapper}"
    );

    let output = run_sh_transaction(&input, "");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "status={:?} stdout={stdout:?} stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("WRAPPER_OK:$HOME:$(nope)"), "{stdout:?}");
    assert!(stdout.contains("\u{1b}]133;D;0;"), "{stdout:?}");
}

#[test]
/// Verifies that a POSIX isolated shell transaction captures a failing command
/// status without allowing strict shell options in the active pane shell to exit
/// the pane. Users often carry `errexit` or `nounset` from their dotfiles, and
/// Mez still needs the OSC end marker so the agent state machine can leave
/// `running` deterministically.
fn posix_wrapper_preserves_parent_shell_with_errexit_enabled() {
    let transaction =
        ShellTransaction::new(marker(), "t1", "a1", "p1", Path::new("/bin/sh"), "false").unwrap();
    let input = transaction.render_for_classification_input(ShellClassification::PosixSh);
    let script = "set -eu\n";
    let suffix = "printf '%s\\n' PARENT_SHELL_ALIVE\n";

    let mut command = Command::new("/bin/sh");
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdin = child.stdin.as_mut().unwrap();
    stdin.write_all(script.as_bytes()).unwrap();
    stdin.write_all(input.wrapper.as_bytes()).unwrap();
    thread::sleep(Duration::from_millis(50));
    stdin.write_all(input.payload.as_bytes()).unwrap();
    stdin.write_all(suffix.as_bytes()).unwrap();
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "status={:?} stdout={stdout:?} stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("\u{1b}]133;D;1;"), "{stdout:?}");
    assert!(stdout.contains("PARENT_SHELL_ALIVE"), "{stdout:?}");
}

#[test]
/// Verifies isolated POSIX shell transactions force non-interactive child
/// environment controls without leaking them back into the pane shell.
///
/// Agent commands run behind a PTY, so child programs can otherwise infer that
/// pagers, editors, or terminal prompts are safe to launch. The wrapper should
/// disable those affordances in the child command shell only.
fn posix_wrapper_sets_noninteractive_child_environment_without_persisting() {
    let transaction = ShellTransaction::new(
        marker(),
        "t1",
        "a1",
        "p1",
        Path::new("/bin/sh"),
        "printf 'CHILD:%s:%s:%s:%s:%s:%s\\n' \"$TERM\" \"$PAGER\" \"$GIT_PAGER\" \"$MANPAGER\" \"$SYSTEMD_PAGER\" \"$GIT_TERMINAL_PROMPT\"",
    )
    .unwrap();
    let input = transaction.render_for_classification_input(ShellClassification::PosixSh);
    let suffix = "printf 'PARENT:%s:%s\\n' \"${PAGER-unset}\" \"${GIT_PAGER-unset}\"\n";

    let mut command = Command::new("env");
    command
        .args(["-u", "PAGER", "-u", "GIT_PAGER", "-u", "MANPAGER"])
        .args(["-u", "SYSTEMD_PAGER", "-u", "GIT_TERMINAL_PROMPT"])
        .arg("/bin/sh");
    let output = run_command_transaction_stdin(&mut command, &input, suffix);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "status={:?} stdout={stdout:?} stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("CHILD:dumb:cat:cat:cat:cat:0"),
        "{stdout:?}"
    );
    assert!(stdout.contains("PARENT:unset:unset"), "{stdout:?}");
}

#[test]
/// Verifies large command payloads are streamed after the receiver starts.
///
/// The persistent pane shell should only parse a bounded wrapper before it can
/// begin draining payload bytes. This protects file actions whose generated
/// command scripts are much larger than ordinary terminal input.
fn posix_wrapper_streams_large_command_payload_after_receiver_start() {
    let command = format!("printf '%s\\n' '{}'", "payload".repeat(4096));
    let transaction =
        ShellTransaction::new(marker(), "t1", "a1", "p1", Path::new("/bin/sh"), &command).unwrap();
    let input = transaction.render_for_classification_input(ShellClassification::PosixSh);

    assert!(input.wrapper.len() < 8 * 1024, "{}", input.wrapper.len());
    assert!(input.payload.len() > input.wrapper.len());
    assert!(
        !input.wrapper.contains("payloadpayload"),
        "{}",
        input.wrapper
    );
    assert!(input.payload.contains("__MEZ_COMMAND_PAYLOAD_END_"));

    let output = run_sh_transaction(&input, "");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "status={:?} stdout={stdout:?} stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("payloadpayload"), "{stdout:?}");
    assert!(stdout.contains("\u{1b}]133;D;0;"), "{stdout:?}");
}

#[test]
/// Verifies internal protocols can retain output beyond the ordinary shell
/// result ceiling without removing the transaction's finite bound.
///
/// `apply_patch` read snapshots contain base64-encoded file bytes and can
/// exceed 256 KiB even when the target file is substantially smaller. The
/// selected raw-output limit must reach the generated POSIX wrapper so a
/// complete snapshot is emitted instead of a dropped-byte marker.
fn posix_wrapper_honors_custom_encoded_output_limit() {
    let output_bytes = SHELL_OUTPUT_BASE64_MAX_RAW_BYTES + 4096;
    let command = format!("head -c {output_bytes} /dev/zero | tr '\\0' x");
    let transaction =
        ShellTransaction::new(marker(), "t1", "a1", "p1", Path::new("/bin/sh"), command)
            .unwrap()
            .with_output_transport(ShellTransactionOutputTransport::Base64)
            .with_output_max_raw_bytes(output_bytes + 1024);
    let input = transaction.render_for_classification_input(ShellClassification::PosixSh);

    let output = run_sh_transaction(&input, "");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let decoded = decode_shell_output_transport_with_diagnostics(&stdout);

    assert!(
        output.status.success(),
        "status={:?} stdout_bytes={} stderr={:?}",
        output.status,
        output.stdout.len(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(decoded.output.len(), output_bytes);
    assert!(!decoded.diagnostics.output_truncated());
    assert!(!stdout.contains(SHELL_OUTPUT_BASE64_DROPPED_BYTES_MARKER));
}

#[test]
/// Verifies shell quote handles single quotes.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn shell_quote_handles_single_quotes() {
    assert_eq!(shell_quote("a'b"), "'a'\"'\"'b'");
}

#[test]
/// Verifies transaction rejects relative shell path.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn transaction_rejects_relative_shell_path() {
    let error =
        ShellTransaction::new(marker(), "t1", "a1", "p1", Path::new("sh"), "pwd").unwrap_err();

    assert_eq!(error.kind(), AgentShellValidationErrorKind::InvalidArgs);
}

#[test]
/// Verifies typed child launches preserve argv boundaries for POSIX and Fish
/// wrappers while substituting only the transaction-owned command file.
fn typed_child_launch_quotes_arguments_without_shell_fragments() {
    let launch = ShellChildLaunch::new(
        "/usr/bin/sandbox helper",
        vec![
            ShellChildArgument::Literal("--label".to_string()),
            ShellChildArgument::Literal("space ' quote $HOME $(false)".to_string()),
            ShellChildArgument::MaterializedCommandFile,
            ShellChildArgument::Literal("tail; false".to_string()),
        ],
    )
    .unwrap();
    let transaction = ShellTransaction::new(
        marker(),
        "t1",
        "a1",
        "p1",
        Path::new("/bin/sh"),
        "printf typed-launch",
    )
    .unwrap()
    .with_child_launch(launch);

    let posix = transaction
        .render_for_classification_input(ShellClassification::PosixSh)
        .wrapper;
    assert!(
        posix.contains("'/usr/bin/sandbox helper' '--label'"),
        "{posix}"
    );
    assert!(
        posix.contains("'space '\"'\"' quote $HOME $(false)' \"$MEZ_COMMAND_FILE\" 'tail; false'"),
        "{posix}"
    );
    assert!(!posix.contains("TERM='dumb'"), "{posix}");

    let fish = transaction
        .render_for_classification_input(ShellClassification::Fish)
        .wrapper;
    assert!(
        fish.contains("'/usr/bin/sandbox helper' '--label'"),
        "{fish}"
    );
    assert!(
        fish.contains("'space \\' quote $HOME $(false)' \"$MEZ_COMMAND_FILE\" 'tail; false'"),
        "{fish}"
    );
    assert!(!fish.contains("TERM='dumb'"), "{fish}");
}

#[test]
/// Verifies typed child launches reject control data and ambiguous command
/// file substitution before shell source is rendered.
fn typed_child_launch_rejects_invalid_argv_contracts() {
    assert!(
        ShellChildLaunch::new(
            "/usr/bin/bwrap",
            vec![ShellChildArgument::Literal("line\nfeed".to_string())]
        )
        .is_err()
    );
    assert!(
        ShellChildLaunch::new(
            "/usr/bin/bwrap",
            vec![
                ShellChildArgument::MaterializedCommandFile,
                ShellChildArgument::MaterializedCommandFile,
            ]
        )
        .is_err()
    );
    assert!(
        ShellChildLaunch::new(
            "relative-bwrap",
            vec![ShellChildArgument::MaterializedCommandFile]
        )
        .is_err()
    );

    assert!(
        ShellChildLaunch::new(
            "/usr/bin/bwrap",
            vec![ShellChildArgument::Literal("--version".to_string())]
        )
        .is_ok()
    );
    assert!(
        ShellChildLaunch::new("/usr/bin/bwrap", Vec::new())
            .unwrap()
            .with_status_fd(2)
            .is_err()
    );
    assert_eq!(
        ShellChildLaunch::new("/usr/bin/bwrap", Vec::new())
            .unwrap()
            .with_status_fd(3)
            .unwrap()
            .status_fd,
        Some(3)
    );
}
