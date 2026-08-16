//! Agent tests for shell transport behavior.
//!
//! This bounded leaf owns the scenarios for this concern while shared
//! fixtures remain in the parent module.

use super::super::transaction::{
    fish_shell_history_restore, fish_shell_history_suppression_start, fish_typed_child_launch_words,
};
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
    let bash_transport =
        agent_subshell_enter_command(Path::new("/bin/bash"), ShellClassification::Bash).unwrap();
    let zsh_transport =
        agent_subshell_enter_command(Path::new("/bin/zsh"), ShellClassification::Zsh).unwrap();
    let bash = decoded_posix_wrapper_source(&bash_transport);
    let zsh = decoded_posix_wrapper_source(&zsh_transport);
    let fish =
        agent_subshell_enter_command(Path::new("/bin/fish"), ShellClassification::Fish).unwrap();

    assert!(
        bash.contains("command env \\\n  -u BASH_ENV \\\n  -u ENV \\\n  -u ZDOTDIR"),
        "{bash}"
    );
    assert!(bash.contains("PROMPT_COMMAND=''"), "{bash}");
    assert!(bash.contains("'/bin/bash' --noprofile --norc"), "{bash}");
    assert!(
        zsh.contains("command env \\\n  -u BASH_ENV \\\n  -u ENV \\\n  -u ZDOTDIR"),
        "{zsh}"
    );
    assert!(zsh.contains("'/bin/zsh' -f"), "{zsh}");
    assert!(zsh.contains("HISTFILE=/dev/null"), "{zsh}");
    assert!(
        fish.contains("command env \\\n  -u BASH_ENV \\\n  -u ENV \\\n  -u ZDOTDIR"),
        "{fish}"
    );
    assert!(fish.contains("fish_private_mode=1"), "{fish}");
    assert!(fish.contains("'/bin/fish' --no-config"), "{fish}");
}

#[test]
/// Verifies ordinary managed Bash work keeps the proofless RX1 wire format.
///
/// Actions and child bootstrap stages do not authenticate parent restoration,
/// so adding the persistent handoff protocol must not change their framing or
/// place a parent-only proof into evaluated source.
fn managed_bash_private_source_remains_proofless_rx1() {
    let token = MarkerToken::new("0123456789abcdef0123456789abcdef").unwrap();
    let input = bash_private_source_input("printf ordinary", &token, "ordinary-marker");

    assert!(
        input.wrapper.contains("MEZ_BASH_RX1_BEGIN"),
        "{}",
        input.wrapper
    );
    assert!(!input.wrapper.contains("MEZ_BASH_RX2"), "{}", input.wrapper);
    assert!(input.receiver_payload.contains("MEZ_BASH_RX1_DATA"));
    assert!(input.receiver_payload.contains("MEZ_BASH_RX1_END"));
    assert!(!input.receiver_payload.contains("MEZ_BASH_RX2"));
}

#[test]
/// Verifies persistent Bash handoff framing isolates its parent-ready proof.
///
/// RX2 carries the proof only in the parent callback's source-free admission
/// header and authenticated cancellation record. The proof must never enter
/// DATA, END, evaluated source, child arguments, or child environment.
fn managed_bash_handoff_rx2_keeps_parent_proof_out_of_payload() {
    let token = MarkerToken::new("0123456789abcdef0123456789abcdef").unwrap();
    let proof = MarkerToken::new("fedcba9876543210fedcba9876543210").unwrap();
    let input = bash_private_handoff_source_input(
        "printf persistent-child",
        &token,
        "handoff-marker",
        &proof,
    );

    assert!(
        input.wrapper.contains("MEZ_BASH_RX2_BEGIN"),
        "{}",
        input.wrapper
    );
    assert!(input.wrapper.contains(proof.as_str()), "{}", input.wrapper);
    assert!(input.receiver_payload.contains("MEZ_BASH_RX2_DATA"));
    assert!(input.receiver_payload.contains("MEZ_BASH_RX2_END"));
    assert!(!input.receiver_payload.contains(proof.as_str()));
    assert!(input.payload.is_empty());
    assert_eq!(
        bash_private_handoff_cancel_input(&token, "handoff-marker", &proof),
        format!(
            "MEZ_BASH_RX2_CANCEL {} handoff-marker {}\n",
            token.as_str(),
            proof.as_str()
        )
    );
}

#[test]
/// Verifies a managed zsh child receives immutable startup state and starts as
/// one direct interactive process without replaying login startup.
///
/// The parent startup shim removes its temporary managed-directory variable
/// before agent admission. The handoff must therefore embed the runtime-owned
/// directory rather than expand that expired variable. The managed `.zshenv`
/// and `.zshrc` install the child receiver without re-running `.zprofile` or
/// `.zlogin` from the already-established parent login environment.
fn managed_zsh_agent_subshell_uses_runtime_owned_interactive_startup() {
    let token = marker();
    let managed = ManagedZshShell::new(
        token.clone(),
        "/tmp/mez-managed-zsh",
        ManagedZshTrigger::EscapeM,
    )
    .unwrap();
    let transport = agent_subshell_enter_command_with_shell_compatibility_and_exit_marker(
        Path::new("/bin/zsh"),
        ShellClassification::Zsh,
        Some(&token),
        Some(&managed),
        None,
        None,
        None,
        Some("bootstrap-marker"),
        None,
    )
    .unwrap();
    let source = decoded_posix_wrapper_source(&transport);

    assert!(
        source.contains("ZDOTDIR='/tmp/mez-managed-zsh'"),
        "{source}"
    );
    assert!(source.contains("'/bin/zsh' -i"), "{source}");
    assert!(!source.contains("'/bin/zsh' -l -i"), "{source}");
    assert!(!source.contains("$MEZ_ZSH_MANAGED_ZDOTDIR"), "{source}");
    assert!(!source.contains("'/bin/zsh' -c"), "{source}");
}

#[test]
/// Verifies managed zsh admission uses only typed fixed triggers and emits an
/// authenticated source-free cancellation record.
///
/// Runtime must never turn a shell-provided trigger string into arbitrary pane
/// input. Hiding agent mode before BEGIN must also restore the parent without
/// retaining or evaluating any generated child source.
fn managed_zsh_trigger_and_cancellation_transport_are_bounded() {
    let token = marker();
    let input = zsh_private_source_input(
        "print -r -- SHOULD_NOT_BE_IN_CANCEL\n",
        &token,
        "zsh-cancel-marker",
        ManagedZshTrigger::EscapeN,
    )
    .unwrap();
    let cancellation = zsh_private_source_cancel_input(&token, "zsh-cancel-marker");

    assert_eq!(input.wrapper, "\u{1b}[27;9;110~");
    assert_eq!(
        input.receiver_hold,
        format!("MEZ_ZSH_RX2_HOLD {} zsh-cancel-marker\n", token.as_str())
    );
    assert!(input.receiver_admission.starts_with(&format!(
        "MEZ_ZSH_RX2_BEGIN {} zsh-cancel-marker ",
        token.as_str()
    )));
    assert!(!input.receiver_admission.contains("SHOULD_NOT_BE_IN_CANCEL"));
    assert!(input.receiver_payload.contains("MEZ_ZSH_RX2_FRAME"));
    assert!(input.receiver_payload.contains("MEZ_ZSH_RX2_DATA"));
    assert!(input.receiver_payload.contains("MEZ_ZSH_RX2_FRAME_END"));
    assert!(input.receiver_payload.contains("MEZ_ZSH_RX2_END"));
    assert_eq!(
        ManagedZshTrigger::from_protocol_str("escape-m"),
        Some(ManagedZshTrigger::EscapeM)
    );
    assert_eq!(
        ManagedZshTrigger::from_protocol_str("escape-n"),
        Some(ManagedZshTrigger::EscapeN)
    );
    assert_eq!(ManagedZshTrigger::from_protocol_str("arbitrary"), None);
    assert_eq!(
        cancellation,
        format!("MEZ_ZSH_RX2_CANCEL {} zsh-cancel-marker\n", token.as_str())
    );
    assert!(!cancellation.contains("SHOULD_NOT_BE_IN_CANCEL"));
    assert!(
        zsh_private_source_input(
            &"x".repeat(ZSH_PRIVATE_SOURCE_MAX_BYTES + 1),
            &token,
            "zsh-oversized-marker",
            ManagedZshTrigger::EscapeM,
        )
        .is_err()
    );
}

#[test]
/// Verifies maximum-size managed zsh source uses bounded logical frames.
///
/// Darwin keeps short physical DATA records, but strict delivery must wait
/// only for validated frame ends and the final authenticated source end. A
/// one-mebibyte source therefore remains below fifty acknowledgement waits
/// instead of requiring one stop-and-wait round trip for every DATA record.
fn managed_zsh_maximum_source_uses_bounded_acknowledgement_frames() {
    let token = marker();
    let input = zsh_private_source_input(
        &"x".repeat(ZSH_PRIVATE_SOURCE_MAX_BYTES),
        &token,
        "zsh-maximum-source-marker",
        ManagedZshTrigger::EscapeM,
    )
    .unwrap();
    let frame_ends = input
        .receiver_payload
        .lines()
        .filter(|line| line.starts_with("MEZ_ZSH_RX2_FRAME_END "))
        .count();
    let source_ends = input
        .receiver_payload
        .lines()
        .filter(|line| line.starts_with("MEZ_ZSH_RX2_END "))
        .count();
    let physical_data_records = input
        .receiver_payload
        .lines()
        .filter(|line| line.starts_with("MEZ_ZSH_RX2_DATA "))
        .count();

    assert_eq!(frame_ends, ZSH_PRIVATE_SOURCE_MAX_FRAMES);
    assert_eq!(source_ends, 1);
    assert!(frame_ends + source_ends < 50);
    assert!(physical_data_records > 1_000);
    assert!(
        input
            .receiver_payload
            .lines()
            .all(|line| line.len() <= ZSH_PRIVATE_SOURCE_MAX_RECORD_BYTES)
    );
}

#[test]
/// Verifies persistent-subshell handoffs remain safe for canonical Unix PTYs.
///
/// Darwin accepts substantially shorter canonical input lines than Linux. The
/// parent pane shell must therefore parse a multiline function definition
/// before launching the child, keeping every physical line at or below the
/// portable bound while retaining cleanup in parent-owned parsed input.
fn agent_subshell_enter_command_keeps_physical_lines_pty_safe() {
    for (path, classification) in [
        ("/bin/sh", ShellClassification::PosixSh),
        ("/bin/bash", ShellClassification::Bash),
        ("/bin/zsh", ShellClassification::Zsh),
        ("/bin/fish", ShellClassification::Fish),
    ] {
        let handoff = agent_subshell_enter_command(Path::new(path), classification).unwrap();

        assert!(
            handoff.lines().all(|line| line.len() <= 700),
            "{classification:?} contains an oversized physical line (max={}):\n{handoff}",
            handoff.lines().map(str::len).max().unwrap_or(0)
        );
        let source = if classification == ShellClassification::Fish {
            handoff.clone()
        } else {
            decoded_posix_wrapper_source(&handoff)
        };
        if classification == ShellClassification::Fish {
            assert!(source.starts_with("begin\n"), "{source}");
        } else {
            assert!(source.contains("__mez_agent_subshell_handoff"), "{source}");
        }
    }
}

#[test]
/// Verifies every supported agent subshell transport emits an opaque parent
/// boundary only after the child handoff returns.
///
/// Shells disagree about EOF text and prompt ordering, so runtime filtering
/// must synchronize on a parent-owned marker rather than an `exit` line. This
/// regression covers direct Fish and managed Bash source as well as encoded
/// POSIX and Zsh wrappers.
fn agent_subshell_exit_boundary_follows_child_cleanup_for_every_shell() {
    let exit_marker = marker();
    let expected_marker = format!(
        "printf '\\033]133;mez_agent_subshell_exit={}\\033\\\\'",
        exit_marker.as_str()
    );
    assert_eq!(
        agent_subshell_exit_marker_bytes(&exit_marker),
        format!(
            "\x1b]133;mez_agent_subshell_exit={}\x1b\\",
            exit_marker.as_str()
        )
        .into_bytes()
    );

    for (path, classification, managed_bash) in [
        ("/bin/sh", ShellClassification::PosixSh, false),
        ("/bin/bash", ShellClassification::Bash, true),
        ("/bin/zsh", ShellClassification::Zsh, false),
        ("/bin/fish", ShellClassification::Fish, false),
    ] {
        let handoff = agent_subshell_enter_command_with_shell_compatibility_and_exit_marker(
            Path::new(path),
            classification,
            (classification == ShellClassification::Zsh).then_some(&exit_marker),
            None,
            managed_bash.then_some(Path::new("/tmp/mez-managed-bashrc")),
            managed_bash.then_some("bootstrap-marker"),
            (classification == ShellClassification::Fish)
                .then_some((&exit_marker, "bootstrap-marker")),
            (classification == ShellClassification::Zsh).then_some("bootstrap-marker"),
            Some(&exit_marker),
        )
        .unwrap();
        let source = if classification == ShellClassification::Fish || managed_bash {
            handoff
        } else {
            decoded_posix_wrapper_source(&handoff)
        };
        let cleanup = if classification == ShellClassification::Fish {
            source.find("set -e MEZ_SHELL_STTY_STATE")
        } else {
            source.rfind("unset -f __mez_agent_subshell_handoff")
        }
        .unwrap_or_else(|| panic!("{classification:?}: {source}"));
        let boundary = source
            .rfind(&expected_marker)
            .unwrap_or_else(|| panic!("{classification:?}: {source}"));

        assert!(cleanup < boundary, "{classification:?}: {source}");
        assert_eq!(
            source.matches(&expected_marker).count(),
            1,
            "{classification:?}: {source}"
        );
    }
}

#[cfg(unix)]
#[test]
/// Verifies Fish handoffs restore parent-local private-mode state after both a
/// successful child exit and a failed child launch.
///
/// The generated handoff must keep state capture and cleanup in one lexical
/// scope. A following parent command proves failed cleanup does not strand or
/// terminate the shell before it resumes ordinary input.
fn fish_agent_subshell_handoff_restores_parent_state_after_child_exit() {
    use std::os::unix::fs::PermissionsExt;

    let Some(fish_path) = fish_path_for_tests() else {
        eprintln!("skipping real-Fish handoff assertion because fish is unavailable");
        return;
    };
    let temp = test_temp_dir("fish-agent-handoff-state");
    let successful_child = temp.join("successful-child");
    std::fs::write(&successful_child, "#!/bin/sh\nexit 0\n")
        .expect("the successful child fixture should be written");
    let mut permissions = std::fs::metadata(&successful_child)
        .expect("the successful child fixture should have metadata")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&successful_child, permissions)
        .expect("the successful child fixture should be executable");

    for (setup, expected_state) in [
        ("set -e fish_private_mode", "__MEZ_HANDOFF_STATE__unset"),
        (
            "set -g fish_private_mode original",
            "__MEZ_HANDOFF_STATE__set:original",
        ),
    ] {
        for child in [&successful_child, &temp.join("missing-child")] {
            let handoff = agent_subshell_enter_command(child, ShellClassification::Fish).unwrap();
            let script = format!(
                "{setup}\n{handoff}if set -q fish_private_mode\n  printf '__MEZ_HANDOFF_STATE__set:%s\\n' \"$fish_private_mode\"\nelse\n  printf '__MEZ_HANDOFF_STATE__unset\\n'\nend\nprintf '__MEZ_HANDOFF_PARENT_ALIVE__\\n'\n"
            );
            let mut fish = Command::new(&fish_path);
            fish.arg("--no-config");
            let output = run_optional_command_stdin_bounded(
                &mut fish,
                &script,
                "Fish agent handoff state probe",
            )
            .expect("the resolved Fish executable should spawn");
            let stdout = String::from_utf8_lossy(&output.stdout);

            assert!(output.status.success(), "child={child:?} output={output:?}");
            assert!(
                stdout.contains(expected_state),
                "child={child:?} {stdout:?}"
            );
            assert!(
                stdout.contains("__MEZ_HANDOFF_PARENT_ALIVE__"),
                "child={child:?} {stdout:?}"
            );
        }
    }

    std::fs::remove_dir_all(temp).unwrap();
}

#[test]
/// Verifies zsh transactions isolate the complete physical transport with
/// zsh-native history controls instead of the Bash history option path.
///
/// Immediate and shared zsh history process each submitted record before it
/// executes, so the authenticated initiating record must precede every
/// `MEZ_WRAPPER_*` assignment and the private history frame must be restored
/// before the transaction completion marker is emitted.
fn zsh_wrapper_uses_native_full_transport_history_isolation() {
    let history_token = marker();
    let transaction =
        ShellTransaction::new(marker(), "t1", "a1", "p1", Path::new("/bin/zsh"), "pwd")
            .unwrap()
            .with_zsh_history_token(history_token.clone());

    let transport = transaction.render_for_classification(ShellClassification::Zsh);
    let source = decoded_posix_wrapper_source(&transport);
    let first_line = transport.lines().next().unwrap_or_default();

    assert!(
        first_line.contains(history_token.as_str()) && first_line.contains("fc -p"),
        "{first_line}"
    );
    assert!(first_line.ends_with("printf '\\036'"), "{first_line}");
    assert!(
        transport.find("fc -p").unwrap() < transport.find("MEZ_WRAPPER_STTY").unwrap(),
        "{transport}"
    );
    assert!(!source.contains("set +o history"), "{source}");
    assert!(!source.contains("history -d"), "{source}");
    assert!(!source.contains("HISTFILE=/dev/null"), "{source}");
    assert!(
        source.find("fc -P").unwrap() < source.find("]133;D;").unwrap(),
        "{source}"
    );
}

#[test]
/// Verifies unmanaged Bash rendering fails closed instead of falling back to
/// ordinary Readline command injection or history cleanup heuristics.
fn bash_wrapper_requires_private_receiver_compatibility() {
    let transaction =
        ShellTransaction::new(marker(), "t1", "a1", "p1", Path::new("/bin/bash"), "pwd").unwrap();

    let input = transaction.render_for_classification_input(ShellClassification::Bash);

    assert!(input.is_empty(), "{input:?}");
}

#[test]
/// Verifies Bash shell transactions ignore inherited `BASH_ENV` startup hooks.
///
/// `BASH_ENV` is a common non-interactive startup vector. Agent actions should
/// inherit ordinary pane environment values while removing this hook before
/// invoking the child command shell.
fn bash_wrapper_unsets_bash_env_before_child_shell_startup() {
    let transaction = ShellTransaction::new(
        marker(),
        "t1",
        "a1",
        "p1",
        Path::new("/bin/bash"),
        "printf '%s\\n' ACTION_RAN",
    )
    .unwrap()
    .with_bash_receiver_token(MarkerToken::new("abcdef0123456789abcdef0123456789").unwrap());
    let input = transaction.render_for_classification_input(ShellClassification::Bash);
    let encoded = input
        .receiver_payload
        .lines()
        .filter(|line| line.starts_with("MEZ_BASH_RX1_DATA "))
        .map(|line| {
            line.split_whitespace()
                .nth(4)
                .expect("private Bash data frame should contain encoded source")
        })
        .collect::<String>();
    let wrapper_source = String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap(),
    )
    .unwrap();

    assert!(input.wrapper.starts_with('\u{7}'), "{:?}", input.wrapper);
    assert!(
        wrapper_source.contains("'/bin/bash' --noprofile --norc \"$MEZ_COMMAND_FILE\""),
        "{wrapper_source}"
    );
    assert!(wrapper_source.contains("-u BASH_ENV"), "{wrapper_source}");
    for obsolete in [
        "MEZ_BASH_HISTORY_OUTER",
        "history -d $((HISTCMD-1))",
        "HISTFILE=/dev/null",
        "set +o history",
    ] {
        assert!(!wrapper_source.contains(obsolete), "{wrapper_source}");
    }
}

#[test]
/// Verifies a managed Bash subshell handoff relies exclusively on private
/// receiver admission and does not mutate the parent shell's history state.
fn managed_bash_subshell_handoff_contains_no_history_fallback() {
    let source = agent_subshell_enter_command_with_shell_compatibility(
        Path::new("/bin/bash"),
        ShellClassification::Bash,
        None,
        Some(Path::new("/tmp/mez-managed-bashrc")),
        Some("bootstrap-marker"),
    )
    .unwrap();

    assert!(source.contains("MEZ_BASH_RECEIVER_INSTALL_MARKER='bootstrap-marker'"));
    assert!(source.contains("--rcfile '/tmp/mez-managed-bashrc' -i"));
    for obsolete in [
        "MEZ_BASH_HISTORY_OUTER",
        "history -d $((HISTCMD-1))",
        "HISTFILE=/dev/null",
        "set +o history",
    ] {
        assert!(!source.contains(obsolete), "{source}");
    }
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

    let input = transaction.render_fish_input();
    let wrapper = decoded_fish_wrapper_source(&input.wrapper);

    assert!(wrapper.contains("set -l MEZ_MARKER_TOKEN '"));
    assert!(wrapper.contains("fish_private_mode"));
    assert!(
        wrapper.contains("builtin history delete --exact --case-sensitive"),
        "{wrapper}"
    );
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
    let start_marker = wrapper.find("printf '\\033]133;C;").unwrap();
    let receiver_ready_marker = wrapper
        .find("printf '\\033]133;R;mez_payload_receiver=ready;")
        .unwrap();
    let payload_receiver = wrapper.find("while read -l MEZ_COMMAND_LINE").unwrap();
    let isolated_child = wrapper.find("command setsid -w env").unwrap();
    assert!(
        start_marker < receiver_ready_marker
            && receiver_ready_marker < payload_receiver
            && payload_receiver < isolated_child,
        "{wrapper}"
    );
    assert!(
        wrapper[payload_receiver..isolated_child].contains("printf '\\n'"),
        "ordinary Fish scripts need a line boundary after payload reception: {wrapper}"
    );
    assert!(!wrapper.contains("'/opt/homebrew/bin/fish' -c"));
    assert!(!wrapper.contains("echo \\'hello fish\\'"));
    assert!(!wrapper.contains("echo 'hello fish'"));
    assert!(
        input.wrapper.lines().all(|line| {
            line.len() <= crate::shell::transaction::SHELL_WRAPPER_BASE64_LINE_BYTES + 420
        }),
        "{}",
        input.wrapper
    );
    assert!(
        !input.wrapper.contains("MEZ_COMMAND_FILE"),
        "{}",
        input.wrapper
    );
    assert!(!wrapper.contains("fish <<"));
    assert!(!wrapper.contains("command cat > \"$MEZ_COMMAND_FILE\""));
    assert!(!wrapper.contains("env -u MEZ_MARKER_TOKEN"));
}

#[test]
/// Verifies every non-stateful Fish payload-receiver variant is a complete,
/// balanced Fish program rather than merely containing plausible fragments.
///
/// Both ordinary and record-acknowledging wrappers share command-file
/// materialization, so parsing the complete generated source catches missing
/// `end` statements at boundaries hidden from fragment assertions.
fn non_stateful_fish_wrappers_parse_as_complete_programs() {
    for acknowledge_payload_records in [false, true] {
        let input = ShellTransaction::new(
            marker(),
            "t1",
            "a1",
            "p1",
            Path::new("/bin/fish"),
            "printf '%s\\n' parsed",
        )
        .unwrap()
        .with_payload_receiver_acknowledgements(acknowledge_payload_records)
        .render_fish_input();
        let wrapper = decoded_fish_wrapper_source(&input.wrapper);
        let Some(output) = parse_fish_wrapper(&wrapper) else {
            eprintln!("skipping real-Fish parser assertion because fish is unavailable");
            return;
        };

        assert!(
            output.status.success(),
            "acknowledge_payload_records={acknowledge_payload_records} status={:?} stdout={:?} stderr={:?}\n{wrapper}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
/// Executes complete non-stateful Fish wrappers through the streamed command
/// payload protocol for both success and failure commands.
///
/// The command uses Fish-only syntax so this also proves the materialized file
/// is dispatched through the declared Fish interpreter. Marker pacing verifies
/// every payload record acknowledgement, status propagation, parent-state
/// restoration, and temporary-file cleanup under one bounded real process.
fn non_stateful_fish_wrappers_execute_complete_marker_paced_transactions() {
    let Some(fish_path) = fish_path_for_tests() else {
        eprintln!("skipping real-Fish transaction assertion because fish is unavailable");
        return;
    };
    for (command_source, expected_status, expected_output) in [
        (
            "set -l dialect fish-native; printf '__MEZ_FISH_COMMAND__%s\\n' $dialect",
            0,
            "__MEZ_FISH_COMMAND__fish-native",
        ),
        (
            "printf '__MEZ_FISH_COMMAND__failure\\n'; false",
            1,
            "__MEZ_FISH_COMMAND__failure",
        ),
    ] {
        let temp = test_temp_dir(&format!("fish-complete-transaction-{expected_status}"));
        let mut input =
            ShellTransaction::new(marker(), "t1", "a1", "p1", &fish_path, command_source)
                .unwrap()
                .with_payload_receiver_acknowledgements(true)
                .render_for_classification_input(ShellClassification::Fish);
        input.wrapper.insert_str(0, "set -e fish_private_mode\n");
        let suffix = "if set -q fish_private_mode; printf '__MEZ_PRIVATE_STATE__set\\n'; else; printf '__MEZ_PRIVATE_STATE__unset\\n'; end\n";
        let mut fish = Command::new(&fish_path);
        fish.arg("--no-config").env("TMPDIR", &temp);
        let output =
            run_fish_transaction_bounded(&mut fish, &input, suffix, "complete Fish transaction");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let start = stdout
            .find("\u{1b}]133;C;")
            .expect("the Fish start marker should be emitted");
        let command = stdout
            .find(expected_output)
            .expect("the Fish command output should be emitted");
        let completion = stdout
            .find(&format!("\u{1b}]133;D;{expected_status};"))
            .expect("the Fish completion marker should carry command status");

        assert!(output.status.success(), "{output:?}");
        assert!(start < command && command < completion, "{stdout:?}");
        assert!(stdout.contains("__MEZ_PRIVATE_STATE__unset"), "{stdout:?}");
        assert_eq!(
            output.stdout.iter().filter(|byte| **byte == 0x1e).count(),
            input.payload.lines().count(),
            "{stdout:?}"
        );
        assert!(
            std::fs::read_dir(&temp)
                .expect("the Fish transaction temp directory should remain readable")
                .next()
                .is_none(),
            "transaction temporary files should be removed: {temp:?}"
        );
        std::fs::remove_dir_all(temp).unwrap();
    }
}

#[cfg(unix)]
#[test]
/// Verifies a complete Fish transaction invokes the exact resolved executable
/// even when its basename no longer identifies Fish.
///
/// The outer real-Fish runner receives the generated wrapper while the isolated
/// command file is executed through a renamed symlink, covering the executable
/// half of renamed-Fish identity and dialect dispatch.
fn non_stateful_fish_wrapper_executes_through_renamed_fish_path() {
    use std::os::unix::fs::symlink;

    let Some(fish_path) = fish_path_for_tests() else {
        eprintln!("skipping renamed-Fish transaction assertion because fish is unavailable");
        return;
    };
    let temp = test_temp_dir("renamed-fish-transaction");
    let renamed_fish = temp.join("custom-shell");
    symlink(&fish_path, &renamed_fish).expect("the renamed Fish symlink should be created");
    let input = ShellTransaction::new(
        marker(),
        "t1",
        "a1",
        "p1",
        &renamed_fish,
        "printf '__MEZ_RENAMED_FISH__ok\\n'",
    )
    .unwrap()
    .with_payload_receiver_acknowledgements(true)
    .render_for_classification_input(ShellClassification::Fish);
    let mut fish = Command::new(&fish_path);
    fish.arg("--no-config");
    let output = run_fish_transaction_bounded(&mut fish, &input, "", "renamed Fish transaction");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "{output:?}");
    assert!(stdout.contains("__MEZ_RENAMED_FISH__ok"), "{stdout:?}");
    assert!(stdout.contains("\u{1b}]133;D;0;"), "{stdout:?}");
    std::fs::remove_dir_all(temp).unwrap();
}

#[test]
/// Verifies Fish wrappers save private-mode state in a surviving scope, use
/// noninteractive exact history deletion, and restore shell state before OSC D.
///
/// Completion is a post-cleanup protocol guarantee. Both stateful and isolated
/// renderers must therefore finish history, terminal, and private-mode cleanup
/// before announcing the saved command status.
fn fish_wrappers_complete_only_after_deterministic_cleanup() {
    let transaction =
        ShellTransaction::new(marker(), "t1", "a1", "p1", Path::new("/bin/fish"), "true").unwrap();
    let wrappers = [
        decoded_fish_wrapper_source(&transaction.render_fish_input().wrapper),
        transaction.render_fish_stateful(),
    ];

    for wrapper in wrappers {
        let saved_declaration = wrapper
            .find("set -l MEZ_FISH_PRIVATE_SAVED;")
            .expect("private-mode saved value should be predeclared");
        let private_probe = wrapper
            .find("if set -q fish_private_mode")
            .expect("private-mode presence should be probed");
        let exact_delete = wrapper
            .rfind("builtin history delete --exact --case-sensitive")
            .expect("wrapper-owned history records should be deleted exactly");
        let private_restore = wrapper
            .rfind("set -g fish_private_mode $MEZ_FISH_PRIVATE_SAVED")
            .expect("the original private-mode value should be restored");
        let completion = wrapper
            .find("printf '\\033]133;D;")
            .expect("the completion marker should be rendered");

        assert!(saved_declaration < private_probe, "{wrapper}");
        assert!(!wrapper.contains("history delete --prefix"), "{wrapper}");
        assert!(exact_delete < completion, "{wrapper}");
        assert!(private_restore < completion, "{wrapper}");
    }
}

#[test]
/// Verifies complete stateful Fish transactions restore private mode when it
/// began unset, set to an empty value, or set to a nonempty value.
///
/// Success and failure commands are both exercised because cleanup must use
/// the saved command status without allowing that status to skip restoration.
fn fish_stateful_cleanup_restores_private_mode_for_success_and_failure() {
    for (setup, expected_state) in [
        ("set -e fish_private_mode", "__MEZ_STATE__unset"),
        (
            "set -g fish_private_mode ''",
            "__MEZ_STATE__set count=1 length=0",
        ),
        (
            "set -g fish_private_mode original",
            "__MEZ_STATE__set count=1 length=8",
        ),
    ] {
        for (command_source, expected_status) in [("true", 0), ("false", 1)] {
            let wrapper = ShellTransaction::new(
                marker(),
                "t1",
                "a1",
                "p1",
                Path::new("/bin/fish"),
                command_source,
            )
            .unwrap()
            .render_fish_stateful();
            let script = format!(
                "{setup}\n{wrapper}\nif set -q fish_private_mode\n  printf '__MEZ_STATE__set count=%s length=%s\\n' (count $fish_private_mode) (string length -- \"$fish_private_mode\")\nelse\n  printf '__MEZ_STATE__unset\\n'\nend\n"
            );
            let mut fish = Command::new("fish");
            fish.arg("--no-config");
            let Some(output) =
                run_optional_command_stdin_bounded(&mut fish, &script, "Fish cleanup probe")
            else {
                eprintln!("skipping real-Fish cleanup assertion because fish is unavailable");
                return;
            };
            let stdout = String::from_utf8_lossy(&output.stdout);

            assert!(
                output.status.success(),
                "setup={setup:?} command={command_source:?} output={output:?}"
            );
            assert!(stdout.contains(expected_state), "{stdout:?}");
            assert!(
                stdout.contains(&format!("\u{1b}]133;D;{expected_status};")),
                "{stdout:?}"
            );
        }
    }
}

#[test]
/// Verifies Fish's noninteractive exact deletion removes only the owned history
/// record while preserving a user record with the same prefix.
///
/// Cleanup must not broaden a wrapper-owned record into a prefix deletion,
/// because the latter both prompts and can select unrelated user commands.
fn fish_exact_history_cleanup_preserves_similarly_prefixed_user_record() {
    let temp = test_temp_dir("fish-exact-history-cleanup");
    let fish_data = temp.join("fish");
    std::fs::create_dir_all(&fish_data).expect("the Fish data directory should be created");
    let history_path = fish_data.join("mez_cleanup_history");
    let owned_record = fish_shell_history_suppression_start()
        .trim_end()
        .to_string();
    let user_record = format!("{owned_record}; printf user-owned");
    std::fs::write(
        &history_path,
        format!("- cmd: {owned_record}\n  when: 1\n- cmd: {user_record}\n  when: 2\n"),
    )
    .expect("the isolated Fish history should be seeded");

    let exact_delete = fish_shell_history_restore()
        .lines()
        .next()
        .expect("Fish cleanup should begin with exact history deletion")
        .to_string();
    let cleanup = format!("set -g fish_history mez_cleanup; {exact_delete}; builtin history save");
    let mut fish = Command::new("fish");
    fish.args(["--no-config", "-c", &cleanup])
        .env("XDG_DATA_HOME", &temp);
    let Some(output) =
        run_optional_command_stdin_bounded(&mut fish, "", "Fish exact history cleanup probe")
    else {
        eprintln!("skipping real-Fish history assertion because fish is unavailable");
        std::fs::remove_dir_all(temp).unwrap();
        return;
    };
    let history = std::fs::read_to_string(&history_path)
        .expect("the isolated Fish history should remain readable");
    std::fs::remove_dir_all(temp).unwrap();

    assert!(output.status.success(), "{output:?}");
    assert!(
        !history.contains(&format!("- cmd: {owned_record}\n")),
        "{history}"
    );
    assert!(
        history.contains(&format!("- cmd: {user_record}\n")),
        "{history}"
    );
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
    let handoff_transport =
        agent_subshell_enter_command(Path::new("/bin/sh"), ShellClassification::PosixSh).unwrap();
    let handoff = decoded_posix_wrapper_source(&handoff_transport);
    let script = format!(
        "set -eu\n{handoff_transport}case $- in *e*u*|*u*e*) printf '%s\\n' STRICT_PARENT_ALIVE;; *) printf '%s\\n' STRICT_PARENT_LOST:$-;; esac\n"
    );

    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "{output:?}");
    assert!(
        handoff.contains("command env \\\n  -u BASH_ENV \\\n  -u ENV \\\n  -u ZDOTDIR"),
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

    let transport = transaction.render_posix();
    let wrapper = decoded_posix_wrapper_source(&transport);

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
    assert!(wrapper.contains("os.getpid()==os.getpgrp()"), "{wrapper}");
    assert!(wrapper.contains("os.fork()"), "{wrapper}");
    let start_marker = wrapper.find("command printf '\\033]133;C;").unwrap();
    let payload_receiver = wrapper
        .find("while IFS= read -r MEZ_COMMAND_LINE; do")
        .unwrap();
    let isolated_child = wrapper.find("command setsid -w").unwrap();
    assert!(
        start_marker < payload_receiver && payload_receiver < isolated_child,
        "{wrapper}"
    );
    assert!(wrapper.contains("MEZ_COMMAND_B64"));
    assert!(wrapper.contains("base64 -d < \"$MEZ_COMMAND_B64\""));
    assert!(wrapper.contains("base64 -D < \"$MEZ_COMMAND_B64\""));
    assert!(wrapper.contains("__mez_tx_"), "{wrapper}");
    let invocation = "__mez_tx_0123456789abcdef";
    let payload_end = "__MEZ_COMMAND_PAYLOAD_END_0123456789abcdef0123456789abcdef__";
    assert!(wrapper.contains(&format!("\n{invocation}\n")), "{wrapper}");
    assert_eq!(transport.trim_end().lines().last(), Some(payload_end));
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
/// Verifies optional transaction sidecar records cross the PTY exactly once
/// and are available as inert data in the materialized command file.
///
/// Semantic writes must not embed final file bytes in generated shell source,
/// because command materialization Base64-encodes that source again. This test
/// keeps the sidecar record printable and confirms the child can decode it
/// from its own script without the wrapper recursively encoding the record.
fn posix_wrapper_materializes_single_encoded_input_sidecar() {
    let encoded = "U0lERUNBUl9PSwo=";
    let command = "sed -n 's/^# __MEZ_INPUT_SIDECAR_V1__ 0 //p' \"$0\" | base64 -d";
    let transaction =
        ShellTransaction::new(marker(), "t1", "a1", "p1", Path::new("/bin/sh"), command)
            .unwrap()
            .with_input_sidecar(Some(format!("0 {encoded}\n")));
    let input = transaction.render_for_classification_input(ShellClassification::PosixSh);

    assert_eq!(
        input.payload.matches(encoded).count(),
        1,
        "{}",
        input.payload
    );
    assert!(!input.wrapper.contains(encoded), "{}", input.wrapper);
    assert!(
        input
            .payload
            .lines()
            .all(|line| line.len() <= SHELL_TRANSACTION_COMMAND_BASE64_LINE_BYTES + 2),
        "{}",
        input.payload
    );

    let output = run_sh_transaction(&input, "");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("SIDECAR_OK"),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
/// Verifies large sidecars are divided into bounded sequenced logical frames
/// while every physical receiver line remains safe for constrained PTYs.
///
/// The frame count, ordering, and physical-line bound protect the throughput
/// contract without allowing one logical frame to grow without limit.
fn posix_wrapper_frames_large_sidecars_with_bounded_physical_records() {
    let sidecar_record = format!(
        "0 {}\n",
        "A".repeat(SHELL_TRANSACTION_COMMAND_BASE64_LINE_BYTES - 3)
    );
    let sidecar = sidecar_record.repeat(
        SHELL_TRANSACTION_SIDECAR_FRAME_BYTES
            .saturating_mul(2)
            .div_ceil(sidecar_record.len()),
    );
    let input = ShellTransaction::new(marker(), "t1", "a1", "p1", Path::new("/bin/sh"), "true")
        .unwrap()
        .with_input_sidecar(Some(sidecar))
        .render_for_classification_input(ShellClassification::PosixSh);

    let frame_begins = input
        .payload
        .lines()
        .filter(|line| line.starts_with("S1B "))
        .collect::<Vec<_>>();
    assert!(frame_begins.len() >= 2, "{}", input.payload);
    for (sequence, line) in frame_begins.iter().enumerate() {
        assert!(line.starts_with(&format!("S1B {sequence} ")), "{line}");
        let declared_len = line
            .split_ascii_whitespace()
            .nth(2)
            .and_then(|value| value.parse::<usize>().ok())
            .expect("frame header should contain a byte count");
        assert!(
            declared_len <= SHELL_TRANSACTION_SIDECAR_FRAME_BYTES,
            "frame {sequence} declared {declared_len} bytes"
        );
    }
    assert_eq!(
        input.payload.matches("\nS1E ").count(),
        frame_begins.len(),
        "{}",
        input.payload
    );
    assert!(
        input
            .payload
            .lines()
            .all(|line| line.len() <= SHELL_TRANSACTION_COMMAND_BASE64_LINE_BYTES + 80),
        "{}",
        input.payload
    );
}

#[test]
/// Verifies a sidecar frame with corrupted content fails closed after the
/// receiver drains the authenticated sentinel and restores the parent shell.
///
/// A frame digest mismatch must not execute the materialized command or leave
/// later same-pane input trapped behind an incomplete receiver transaction.
fn posix_receiver_rejects_corrupt_sidecar_frame_and_releases_parent_input() {
    let encoded = "U0lERUNBUl9PSwo=";
    let transaction = ShellTransaction::new(
        marker(),
        "t1",
        "a1",
        "p1",
        Path::new("/bin/sh"),
        "printf '%s\n' SHOULD_NOT_RUN",
    )
    .unwrap()
    .with_input_sidecar(Some(format!("0 {encoded}\n")))
    .with_payload_receiver_acknowledgements(true);
    let mut input = transaction.render_for_classification_input(ShellClassification::PosixSh);
    input.payload = input.payload.replacen(encoded, "V0lERUNBUl9PSwo=", 1);

    let output = run_sh_transaction(&input, "printf '%s\n' PARENT_AFTER_CORRUPT_FRAME\n");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "{output:?}");
    assert!(stdout.contains("\u{1b}]133;D;1;"), "{stdout:?}");
    assert!(stdout.contains("PARENT_AFTER_CORRUPT_FRAME"), "{stdout:?}");
    assert!(!stdout.contains("SHOULD_NOT_RUN"), "{stdout:?}");
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

    let wrapper_source = decoded_posix_wrapper_source(&input.wrapper);
    assert!(wrapper_source.len() < 8 * 1024, "{}", wrapper_source.len());
    assert!(input.payload.len() > wrapper_source.len());
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
/// Verifies acknowledged POSIX and zsh receivers emit exactly one raw record
/// separator for every base64 data record and for the authenticated sentinel.
///
/// Strict Darwin pacing depends on this count rather than arbitrary terminal
/// output, so the rendered transaction must also advertise the capability.
fn posix_and_zsh_receivers_acknowledge_every_payload_record() {
    for classification in [ShellClassification::PosixSh, ShellClassification::Zsh] {
        let transaction = ShellTransaction::new(
            marker(),
            "t1",
            "a1",
            "p1",
            Path::new("/bin/sh"),
            "printf '%s\\n' reconstructed",
        )
        .unwrap()
        .with_payload_receiver_acknowledgements(true);
        let input = transaction.render_for_classification_input(classification);
        let expected_acknowledgements = input.payload.lines().count();

        assert!(input.payload_receiver_acknowledgements);
        assert!(
            decoded_posix_wrapper_source(&input.wrapper).contains("command printf '\\036'"),
            "{classification:?}: {}",
            input.wrapper
        );
        let output = run_sh_transaction(&input, "");
        assert!(output.status.success(), "{classification:?}: {output:?}");
        let receiver_output = output
            .stdout
            .windows(b"\x1b]133;C;".len())
            .position(|window| window == b"\x1b]133;C;")
            .and_then(|start| {
                let tail = &output.stdout[start..];
                tail.windows(b"\x1b\\".len())
                    .position(|window| window == b"\x1b\\")
                    .map(|end| &tail[end + b"\x1b\\".len()..])
            })
            .expect("transaction start marker should precede receiver acknowledgements");
        assert_eq!(
            receiver_output.iter().filter(|byte| **byte == 0x1e).count(),
            expected_acknowledgements,
            "{classification:?}: {:?}",
            output.stdout
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("reconstructed"),
            "{classification:?}: {:?}",
            output.stdout
        );
    }
}

#[test]
/// Verifies ordinary unpaced transactions neither advertise receiver
/// acknowledgements nor emit acknowledgement commands in POSIX or Fish source.
fn unpaced_receivers_do_not_emit_payload_acknowledgements() {
    let transaction =
        ShellTransaction::new(marker(), "t1", "a1", "p1", Path::new("/bin/sh"), "true").unwrap();
    let posix = transaction.render_for_classification_input(ShellClassification::PosixSh);
    let fish = transaction.render_for_classification_input(ShellClassification::Fish);

    assert!(!posix.payload_receiver_acknowledgements);
    assert!(!fish.payload_receiver_acknowledgements);
    assert!(!decoded_posix_wrapper_source(&posix.wrapper).contains("printf '\\036'"));
    assert!(!decoded_fish_wrapper_source(&fish.wrapper).contains("printf '\\036'"));
}

#[test]
/// Verifies Fish advertises and renders the same per-record acknowledgement
/// contract as POSIX and zsh without changing its existing start marker.
fn fish_receiver_renders_acknowledged_payload_contract() {
    let input = ShellTransaction::new(
        marker(),
        "t1",
        "a1",
        "p1",
        Path::new("/opt/homebrew/bin/fish"),
        "printf '%s\\n' fish",
    )
    .unwrap()
    .with_payload_receiver_acknowledgements(true)
    .render_for_classification_input(ShellClassification::Fish);
    let wrapper = decoded_fish_wrapper_source(&input.wrapper);

    assert!(input.payload_receiver_acknowledgements);
    assert_eq!(wrapper.matches("printf '\\036'").count(), 2);
    assert!(wrapper.contains("printf '\\033]133;C;"));
    assert!(wrapper.contains("set MEZ_COMMAND_SEEN_END 1"));
    assert!(!wrapper.contains("set MEZ_WRITE_STATUS $status; break"));
}

#[test]
/// Verifies a POSIX append failure retains the first error while continuing to
/// consume and acknowledge every inert payload record through the sentinel.
///
/// The test redirects the receiver sidecar to `/dev/full`, then confirms the
/// parent shell resumes after the failed transaction instead of interpreting a
/// leftover payload record as interactive shell input.
fn posix_receiver_failure_drains_and_acknowledges_through_sentinel() {
    let transaction = ShellTransaction::new(
        marker(),
        "t1",
        "a1",
        "p1",
        Path::new("/bin/sh"),
        format!("printf '%s\\n' '{}'", "payload".repeat(512)),
    )
    .unwrap()
    .with_payload_receiver_acknowledgements(true);
    let mut input = transaction.render_for_classification_input(ShellClassification::PosixSh);
    let expected_acknowledgements = input.payload.lines().count();
    let source = decoded_posix_wrapper_source(&input.wrapper).replace(
        "MEZ_COMMAND_B64=$(mktemp) || MEZ_WRITE_STATUS=1",
        "MEZ_COMMAND_B64=/dev/full",
    );
    input.wrapper =
        super::posix_shell_wrapper_transport(&source, ShellClassification::PosixSh, None);

    let output = run_sh_transaction(&input, "printf '%s\\n' PARENT_AFTER_FAILURE\n");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let receiver_output = output
        .stdout
        .windows(b"\x1b]133;C;".len())
        .position(|window| window == b"\x1b]133;C;")
        .and_then(|start| {
            let tail = &output.stdout[start..];
            tail.windows(b"\x1b\\".len())
                .position(|window| window == b"\x1b\\")
                .map(|end| &tail[end + b"\x1b\\".len()..])
        })
        .expect("transaction start marker should precede receiver acknowledgements");

    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        receiver_output.iter().filter(|byte| **byte == 0x1e).count(),
        expected_acknowledgements,
        "{:?}",
        output.stdout
    );
    assert!(stdout.contains("\u{1b}]133;D;1;"), "{stdout:?}");
    assert!(stdout.contains("PARENT_AFTER_FAILURE"), "{stdout:?}");
    assert!(!stdout.contains("payloadpayload"), "{stdout:?}");
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

    let posix_transport = transaction
        .render_for_classification_input(ShellClassification::PosixSh)
        .wrapper;
    let posix = decoded_posix_wrapper_source(&posix_transport);
    assert!(
        posix.contains("'/usr/bin/sandbox helper' '--label'"),
        "{posix}"
    );
    assert!(
        posix.contains("'space '\"'\"' quote $HOME $(false)' \"$MEZ_COMMAND_FILE\" 'tail; false'"),
        "{posix}"
    );
    assert!(!posix.contains("TERM='dumb'"), "{posix}");

    let fish_transport = transaction
        .render_for_classification_input(ShellClassification::Fish)
        .wrapper;
    let fish = decoded_fish_wrapper_source(&fish_transport);
    assert!(
        fish.contains("'/usr/bin/sandbox helper' \\\n'--label'"),
        "{fish}"
    );
    let payload_receiver = fish
        .find("while read -l MEZ_COMMAND_LINE")
        .expect("typed Fish launch should receive its command payload");
    let typed_child = fish
        .find("'/usr/bin/sandbox helper'")
        .expect("typed Fish launch should contain the resolved executable");
    assert!(
        !fish[payload_receiver..typed_child].contains("printf '\\n'"),
        "typed Fish child output must begin without wrapper-added bytes: {fish}"
    );
    assert!(
        fish.contains(
            "'space \\' quote $HOME $(false)' \\\n\"$MEZ_COMMAND_FILE\" \\\n'tail; false'"
        ),
        "{fish}"
    );
    assert!(fish.lines().all(|line| line.len() <= 700), "{fish}");
    assert!(!fish.contains("TERM='dumb'"), "{fish}");
}

#[test]
/// Verifies a literal-only typed Fish child launch starts directly without an
/// unused command-file receiver or synthetic empty-command payload.
fn typed_fish_child_launch_without_command_file_skips_payload_receiver() {
    let launch = ShellChildLaunch::new(
        "/bin/sh",
        vec![
            ShellChildArgument::Literal("-c".to_string()),
            ShellChildArgument::Literal("printf mez-bubblewrap-capability-v6".to_string()),
        ],
    )
    .unwrap();
    let input = ShellTransaction::new(marker(), "t1", "a1", "p1", Path::new("/usr/bin/fish"), "")
        .unwrap()
        .with_child_launch(launch)
        .render_for_classification_input(ShellClassification::Fish);
    let fish = decoded_fish_wrapper_source(&input.wrapper);

    assert!(input.payload.is_empty(), "{input:?}");
    assert!(!fish.contains("while read -l MEZ_COMMAND_LINE"), "{fish}");
    assert!(!fish.contains("mez_payload_receiver=ready"), "{fish}");
    let start_marker = fish.find("printf '\\033]133;C;").unwrap();
    let typed_child = fish.find("'/bin/sh' \\\n'-c'").unwrap();
    assert!(start_marker < typed_child, "{fish}");
}

#[test]
/// Verifies a long typed POSIX child argument is split across bounded physical
/// wrapper lines while preserving its one-argument argv boundary at execution.
/// Forwarded sandbox environment values can exceed terminal line-discipline
/// limits, so the wrapper must remain executable after source-line splitting.
/// The bound includes command syntax around each configured base64 chunk and
/// is 128 bytes on Darwin, protecting the Bash 3.2 interactive input path.
fn typed_child_launch_bounds_long_posix_argument_lines() {
    let long_argument = "sandbox-path-segment:".repeat(200);
    let launch = ShellChildLaunch::new(
        "/bin/sh",
        vec![
            ShellChildArgument::Literal("-c".to_string()),
            ShellChildArgument::Literal("test \"$2\" = \"$3\"".to_string()),
            ShellChildArgument::Literal("sh".to_string()),
            ShellChildArgument::MaterializedCommandFile,
            ShellChildArgument::Literal(long_argument.clone()),
            ShellChildArgument::Literal(long_argument),
        ],
    )
    .unwrap();
    let transaction =
        ShellTransaction::new(marker(), "t1", "a1", "p1", Path::new("/bin/sh"), "true")
            .unwrap()
            .with_child_launch(launch);
    let input = transaction.render_for_classification_input(ShellClassification::PosixSh);

    assert!(
        input.wrapper.lines().all(|line| {
            line.len() <= crate::shell::transaction::SHELL_WRAPPER_BASE64_LINE_BYTES + 64
        }),
        "{}",
        input.wrapper
    );
    let output = run_sh_transaction(&input, "");
    assert!(
        output.status.success(),
        "status={:?} stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
/// Verifies a large typed Fish child launch stays below the portable PTY line
/// bound while preserving every literal as exactly one argv element.
///
/// The launch covers empty, quoted, whitespace, backslash, Unicode, and
/// command-substitution-like literals in addition to a long repeated value.
/// Real Fish execution proves source chunking does not split or evaluate argv.
fn typed_child_launch_bounds_fish_lines_and_preserves_argv() {
    if Command::new("fish").arg("--version").output().is_err() {
        eprintln!("skipping real-Fish argv assertion because fish is unavailable");
        return;
    }

    let long_argument = "sandbox-path-segment:".repeat(200);
    let launch = ShellChildLaunch::new(
        "/bin/sh",
        vec![
            ShellChildArgument::Literal("-c".to_string()),
            ShellChildArgument::Literal(
                "test -f \"$1\" && test \"$2\" = '' && test \"$3\" = \"space ' quote\" && test \"$4\" = 'back\\slash' && test \"$5\" = 'snowman-☃' && test \"$6\" = \"$7\" && test \"$8\" = '$(false)'"
                    .to_string(),
            ),
            ShellChildArgument::Literal("sh".to_string()),
            ShellChildArgument::MaterializedCommandFile,
            ShellChildArgument::Literal(String::new()),
            ShellChildArgument::Literal("space ' quote".to_string()),
            ShellChildArgument::Literal("back\\slash".to_string()),
            ShellChildArgument::Literal("snowman-☃".to_string()),
            ShellChildArgument::Literal(long_argument.clone()),
            ShellChildArgument::Literal(long_argument),
            ShellChildArgument::Literal("$(false)".to_string()),
        ],
    )
    .unwrap();
    let launch_words = fish_typed_child_launch_words(&launch);
    let input = ShellTransaction::new(
        marker(),
        "t1",
        "a1",
        "p1",
        Path::new("/bin/fish"),
        "printf typed-fish-launch",
    )
    .unwrap()
    .with_child_launch(launch)
    .render_for_classification_input(ShellClassification::Fish);

    assert!(
        input.wrapper.lines().all(|line| line.len() <= 700),
        "max={}\n{}",
        input.wrapper.lines().map(str::len).max().unwrap_or(0),
        input.wrapper
    );

    let temp = test_temp_dir("fish-typed-child-launch");
    let command_file = temp.join("command.fish");
    std::fs::write(&command_file, "printf typed-fish-launch\n")
        .expect("the materialized command fixture should be written");
    let source = format!(
        "set -l MEZ_COMMAND_FILE {}\n{launch_words}\n",
        fish_quote(&command_file.to_string_lossy())
    );
    let mut fish = Command::new("fish");
    fish.args(["--no-config", "-c", &source]);
    let output = run_optional_command_stdin_bounded(&mut fish, "", "Fish typed child launch")
        .expect("Fish availability was checked before executing the typed launch");
    std::fs::remove_dir_all(temp).unwrap();
    assert!(
        output.status.success(),
        "status={:?} stdout={:?} stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
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
