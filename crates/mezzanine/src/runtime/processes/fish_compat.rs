//! Managed passive shell integration for interactive Fish pane processes.
//!
//! Ordinary Fish panes receive process-local `fish_preexec` and
//! `fish_postexec` handlers through Fish's `--init-command` launch contract.
//! The handlers emit advisory OSC 133 command boundaries without modifying
//! user startup files or visible prompt text. Stable function names are erased
//! before registration so repeated initialization replaces rather than stacks
//! handlers, and the entire integration disappears with the owning pane
//! process generation.

use mez_agent::{MarkerToken, shell::fish_wrapper_receiver_init_command};
use mez_mux::process::PaneProcessLaunch;

/// Pane-process-scoped Fish passive integration state.
#[derive(Debug)]
pub(super) struct ManagedFishCompatibility {
    /// Opaque owner retained in Fish state for generation diagnostics.
    _owner: MarkerToken,
    /// Fish source evaluated after user configuration and before interaction.
    init_command: String,
}

impl ManagedFishCompatibility {
    /// Creates one process-local Fish integration owner.
    pub(super) fn new(owner: MarkerToken) -> Self {
        let init_command = managed_fish_init_command(&owner);
        Self {
            _owner: owner,
            init_command,
        }
    }

    /// Adds passive integration initialization to an ordinary Fish launch.
    pub(super) fn configure_launch(&self, launch: PaneProcessLaunch) -> PaneProcessLaunch {
        launch.with_interactive_arguments(["--init-command", self.init_command.as_str(), "-i"])
    }

    #[cfg(test)]
    /// Returns the process owner token for lifecycle tests.
    fn owner(&self) -> &MarkerToken {
        &self._owner
    }

    #[cfg(test)]
    /// Returns the generated Fish initialization source for execution tests.
    fn init_command(&self) -> &str {
        &self.init_command
    }
}

/// Renders stable Fish handlers for passive OSC 133 prompt and command boundaries.
fn managed_fish_init_command(owner: &MarkerToken) -> String {
    format!(
        r#"{}
if not functions --query __mez_fish_user_prompt
    if functions --query fish_prompt
        functions --copy fish_prompt __mez_fish_user_prompt
    else
        function __mez_fish_user_prompt
        end
    end
end
if not functions --query __mez_fish_user_right_prompt
    if functions --query fish_right_prompt
        functions --copy fish_right_prompt __mez_fish_user_right_prompt
    else
        function __mez_fish_user_right_prompt
        end
    end
end
functions --erase __mez_fish_passive_command_is_internal __mez_fish_passive_prompt_start __mez_fish_passive_preexec __mez_fish_passive_postexec fish_prompt fish_right_prompt 2>/dev/null
set -g __MEZ_FISH_INTEGRATION_OWNER {}
set -e __MEZ_FISH_PASSIVE_COMMAND_ACTIVE __MEZ_FISH_PASSIVE_SKIP_POSTEXEC
function __mez_fish_passive_command_is_internal --argument-names command_line
    if string match --quiet '*mez_marker=*' -- "$command_line"; and string match --quiet '*mez_turn=*' -- "$command_line"
        return 0
    end
    if string match --quiet '*__mez_agent_subshell_handoff*' -- "$command_line"
        return 0
    end
    if string match --quiet '*__mez_fish_passive_*' -- "$command_line"
        return 0
    end
    return 1
end
function __mez_fish_passive_prompt_start --on-event fish_prompt
    builtin printf '\033]133;A\033\\'
end
function fish_prompt
    __mez_fish_user_prompt
    builtin printf '\033]133;B\033\\'
end
function fish_right_prompt
    __mez_fish_user_right_prompt
end
function __mez_fish_passive_preexec --on-event fish_preexec
    set -l command_line "$argv[1]"
    if __mez_fish_passive_command_is_internal "$command_line"
        builtin history delete --exact --case-sensitive "$command_line" >/dev/null 2>&1
        set -g __MEZ_FISH_PASSIVE_SKIP_POSTEXEC 1
        set -e __MEZ_FISH_PASSIVE_COMMAND_ACTIVE
        return 0
    end
    set -e __MEZ_FISH_PASSIVE_SKIP_POSTEXEC
    set -g __MEZ_FISH_PASSIVE_COMMAND_ACTIVE 1
    builtin printf '\033]133;C\033\\'
end
function __mez_fish_passive_postexec --on-event fish_postexec
    set -l command_status $status
    if set -q __MEZ_FISH_PASSIVE_SKIP_POSTEXEC
        set -e __MEZ_FISH_PASSIVE_SKIP_POSTEXEC __MEZ_FISH_PASSIVE_COMMAND_ACTIVE
        return 0
    end
    if not set -q __MEZ_FISH_PASSIVE_COMMAND_ACTIVE
        return 0
    end
    set -e __MEZ_FISH_PASSIVE_COMMAND_ACTIVE
    builtin printf '\033]133;D;%s\033\\' "$command_status"
end
"#,
        fish_wrapper_receiver_init_command(),
        mez_agent::fish_quote(owner.as_str())
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use mez_agent::shell::{
        PanePathResolutionRequest, ShellClassification, ShellTransaction,
        pane_path_resolution_command, parse_pane_path_resolution_output,
        shell_identity_probe_command,
    };
    use mez_mux::layout::Size;
    use mez_mux::process::{
        PaneProcess, PaneProcessEnvironment, pane_command_plan, spawn_pane_process,
    };
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    /// Resolves Fish from common Linux and macOS installation paths.
    fn fish_path_for_tests() -> Option<PathBuf> {
        std::env::var_os("PATH")
            .into_iter()
            .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
            .map(|directory| directory.join("fish"))
            .chain(
                [
                    "/usr/bin/fish",
                    "/usr/local/bin/fish",
                    "/opt/homebrew/bin/fish",
                ]
                .into_iter()
                .map(PathBuf::from),
            )
            .find(|candidate| candidate.is_file())
    }

    /// Builds the dependency-neutral pane environment for one Fish PTY test.
    fn test_environment() -> PaneProcessEnvironment {
        PaneProcessEnvironment {
            mez: "socket=/tmp/mez-test.sock;session=s1;window=w1;pane=p1;term=xterm-256color"
                .to_string(),
            session: "s1".to_string(),
            window: "w1".to_string(),
            pane: "p1".to_string(),
            term: "xterm-256color".to_string(),
        }
    }

    /// Reads one Fish PTY until a predicate succeeds or a bounded deadline expires.
    fn read_fish_output_until(
        process: &mut PaneProcess,
        predicate: impl Fn(&[u8]) -> bool,
    ) -> Vec<u8> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output = Vec::new();
        loop {
            output.extend(
                process
                    .read_available_output(64 * 1024)
                    .expect("managed Fish output should remain readable"),
            );
            if predicate(&output) {
                return output;
            }
            if Instant::now() >= deadline {
                let _ = process.terminate(Duration::from_millis(100));
                panic!(
                    "managed Fish output did not reach its expected boundary: {:?}",
                    String::from_utf8_lossy(&output)
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Spawns one isolated ordinary Fish pane with managed passive integration.
    fn spawn_managed_fish(
        fish: &Path,
        compatibility: &ManagedFishCompatibility,
        home: &Path,
    ) -> PaneProcess {
        let config_home = home.join("config");
        let fish_config = config_home.join("fish");
        std::fs::create_dir_all(&fish_config)
            .expect("the isolated Fish config directory should be created");
        std::fs::write(
            fish_config.join("config.fish"),
            "function fish_prompt\n    printf '__MEZ_USER_PROMPT__status=%s>' $status\nend\nfunction fish_right_prompt\n    printf '__MEZ_USER_RIGHT_PROMPT__'\nend\n",
        )
        .expect("the isolated Fish prompt configuration should be written");
        let launch = PaneProcessLaunch::new(fish.to_path_buf())
            .with_interactive_arguments(["--init-command", compatibility.init_command(), "-i"])
            .with_environment_variable("HOME", home.as_os_str())
            .with_environment_variable("XDG_CONFIG_HOME", config_home.as_os_str())
            .with_environment_variable("XDG_DATA_HOME", home.join("data").as_os_str());
        spawn_pane_process(
            &launch,
            None,
            &test_environment(),
            Size::new(80, 24).expect("the Fish PTY size should be valid"),
        )
        .expect("the managed Fish pane should spawn")
    }

    #[test]
    /// Verifies the launch contract preserves user configuration while running
    /// managed initialization after it and before interactive input.
    fn managed_fish_compatibility_configures_process_local_initialization() {
        let owner = MarkerToken::new("0123456789abcdef0123456789abcdef").unwrap();
        let compatibility = ManagedFishCompatibility::new(owner.clone());
        let launch =
            compatibility.configure_launch(PaneProcessLaunch::new(PathBuf::from("/bin/fish")));
        let plan = pane_command_plan(&launch, None).unwrap();

        assert_eq!(compatibility.owner(), &owner);
        assert_eq!(plan.args[0], "--init-command");
        assert_eq!(plan.args[1], compatibility.init_command());
        assert_eq!(plan.args[2], "-i");
        assert!(!plan.args.iter().any(|argument| argument == "--no-config"));
        assert!(
            compatibility
                .init_command()
                .contains("function __mez_agent_wrapper_receive")
        );
        assert!(compatibility.init_command().contains("functions --erase"));
        assert!(
            compatibility
                .init_command()
                .contains("function fish_prompt")
        );
        assert!(
            compatibility
                .init_command()
                .contains("function fish_right_prompt")
        );
        assert!(
            compatibility
                .init_command()
                .contains("--on-event fish_prompt")
        );
        assert!(
            compatibility
                .init_command()
                .contains("--on-event fish_preexec")
        );
        assert!(
            compatibility
                .init_command()
                .contains("--on-event fish_postexec")
        );
    }

    #[test]
    /// Verifies real Fish emits exactly one passive pre/post pair per user
    /// command, preserves status, replaces repeated registration, suppresses
    /// Mezzanine-owned records, and drops handlers with the process generation.
    fn managed_fish_compatibility_emits_scoped_passive_boundaries() {
        let Some(fish) = fish_path_for_tests() else {
            eprintln!("skipping managed Fish PTY assertion because fish is unavailable");
            return;
        };
        let root = std::env::temp_dir().join(format!(
            "mez-managed-fish-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let first = ManagedFishCompatibility::new(
            MarkerToken::new("11111111111111111111111111111111").unwrap(),
        );
        let mut first_process = spawn_managed_fish(&fish, &first, &root.join("first"));
        std::thread::sleep(Duration::from_millis(100));
        let _ = first_process.read_available_output(64 * 1024);
        first_process
            .write_input(b"true; printf '__MEZ_FIRST_DONE__\\n'\n")
            .unwrap();
        let first_output = read_fish_output_until(&mut first_process, |output| {
            output
                .windows(b"__MEZ_FIRST_DONE__".len())
                .any(|window| window == b"__MEZ_FIRST_DONE__")
                && output
                    .windows(b"\x1b]133;D;0\x1b\\".len())
                    .any(|window| window == b"\x1b]133;D;0\x1b\\")
        });
        assert_eq!(
            first_output
                .windows(b"\x1b]133;C\x1b\\".len())
                .filter(|window| *window == b"\x1b]133;C\x1b\\")
                .count(),
            1,
            "{:?}",
            String::from_utf8_lossy(&first_output)
        );
        let first_text = String::from_utf8_lossy(&first_output);
        let completion = first_text
            .find("\u{1b}]133;D;0\u{1b}\\")
            .expect("the first command completion boundary should be present");
        let prompt_start = first_text
            .find("\u{1b}]133;A\u{1b}\\")
            .expect("the first prompt-start boundary should be present");
        let user_prompt = first_text
            .find("__MEZ_USER_PROMPT__status=0>")
            .expect("the configured user prompt should be preserved");
        let prompt_end = first_text
            .find("\u{1b}]133;B\u{1b}\\")
            .expect("the first prompt-end boundary should be present");
        let user_right_prompt = first_text[prompt_end..]
            .find("__MEZ_USER_RIGHT_PROMPT__")
            .map(|offset| prompt_end + offset)
            .expect("the configured user right prompt should be preserved after prompt end");
        assert!(
            completion < prompt_start
                && prompt_start < user_prompt
                && user_prompt < prompt_end
                && prompt_end < user_right_prompt,
            "{first_text:?}"
        );
        first_process.terminate(Duration::from_millis(100)).unwrap();

        let second = ManagedFishCompatibility::new(
            MarkerToken::new("22222222222222222222222222222222").unwrap(),
        );
        let mut second_process = spawn_managed_fish(&fish, &second, &root.join("second"));
        std::thread::sleep(Duration::from_millis(100));
        let _ = second_process.read_available_output(64 * 1024);
        let replacement = format!("eval {}\n", mez_agent::fish_quote(second.init_command()));
        second_process.write_input(b"true\nfalse\n").unwrap();
        second_process
            .write_input(b"printf '__MEZ_INTERNAL__\\n' # mez_marker=x mez_turn=y\n")
            .unwrap();
        second_process.write_input(replacement.as_bytes()).unwrap();
        second_process
            .write_input(b"true; printf '__MEZ_SECOND_DONE__\\n'\n")
            .unwrap();
        let second_output = read_fish_output_until(&mut second_process, |output| {
            output
                .windows(b"__MEZ_SECOND_DONE__".len())
                .any(|window| window == b"__MEZ_SECOND_DONE__")
                && output
                    .windows(b"\x1b]133;D;0\x1b\\".len())
                    .filter(|window| *window == b"\x1b]133;D;0\x1b\\")
                    .count()
                    >= 2
                && output
                    .windows(b"\x1b]133;D;1\x1b\\".len())
                    .any(|window| window == b"\x1b]133;D;1\x1b\\")
        });
        assert_eq!(
            second_output
                .windows(b"\x1b]133;C\x1b\\".len())
                .filter(|window| *window == b"\x1b]133;C\x1b\\")
                .count(),
            3,
            "{:?}",
            String::from_utf8_lossy(&second_output)
        );
        assert_eq!(
            second_output
                .windows(b"\x1b]133;D;0\x1b\\".len())
                .filter(|window| *window == b"\x1b]133;D;0\x1b\\")
                .count(),
            2,
            "{:?}",
            String::from_utf8_lossy(&second_output)
        );
        assert_eq!(
            second_output
                .windows(b"\x1b]133;D;1\x1b\\".len())
                .filter(|window| *window == b"\x1b]133;D;1\x1b\\")
                .count(),
            1,
            "{:?}",
            String::from_utf8_lossy(&second_output)
        );
        for boundary in [
            b"\x1b]133;A\x1b\\".as_slice(),
            b"\x1b]133;B\x1b\\".as_slice(),
        ] {
            assert_eq!(
                second_output
                    .windows(boundary.len())
                    .filter(|window| *window == boundary)
                    .count(),
                5,
                "{:?}",
                String::from_utf8_lossy(&second_output)
            );
        }
        let second_text = String::from_utf8_lossy(&second_output);
        assert!(
            second_text.contains("__MEZ_USER_PROMPT__status=1>"),
            "{second_text:?}"
        );
        assert!(
            second_text.contains("__MEZ_USER_RIGHT_PROMPT__"),
            "{second_text:?}"
        );
        assert!(
            String::from_utf8_lossy(&second_output).contains("__MEZ_INTERNAL__"),
            "{:?}",
            String::from_utf8_lossy(&second_output)
        );
        second_process
            .terminate(Duration::from_millis(100))
            .unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    /// Verifies a managed interactive Fish pane accepts the deferred command
    /// payload used by runtime path resolution, emits parseable evidence, and
    /// returns to ordinary input without replaying transaction source.
    fn managed_fish_compatibility_executes_deferred_path_resolution_transaction() {
        let Some(fish) = fish_path_for_tests() else {
            eprintln!("skipping managed Fish resolver assertion because fish is unavailable");
            return;
        };
        let root = std::env::temp_dir().join(format!(
            "mez-managed-fish-resolver-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let compatibility = ManagedFishCompatibility::new(
            MarkerToken::new("33333333333333333333333333333333").unwrap(),
        );
        let mut process = spawn_managed_fish(&fish, &compatibility, &root);
        std::thread::sleep(Duration::from_millis(100));
        let _ = process.read_available_output(64 * 1024);

        let request =
            PanePathResolutionRequest::new(vec![".".to_string()], Vec::new(), Vec::new()).unwrap();
        let command = pane_path_resolution_command(&request, ShellClassification::Fish).unwrap();
        let marker = MarkerToken::new("44444444444444444444444444444444").unwrap();
        let input = ShellTransaction::new(
            marker.clone(),
            "resolver-turn",
            "resolver-agent",
            "p1",
            &fish,
            command,
        )
        .unwrap()
        .render_for_classification_input(ShellClassification::Fish);
        process.write_input(input.wrapper.as_bytes()).unwrap();
        let start_marker = format!("\x1b]133;C;mez_marker={};", marker.as_str());
        let mut output = read_fish_output_until(&mut process, |output| {
            output
                .windows(start_marker.len())
                .any(|window| window == start_marker.as_bytes())
        });
        process.write_input(input.payload.as_bytes()).unwrap();
        let protocol_marker = b"MEZ_PATH_RESOLUTION_V2\t";
        let end_marker = format!("\x1b]133;D;0;mez_marker={};", marker.as_str());
        output.extend(read_fish_output_until(&mut process, |output| {
            output
                .windows(protocol_marker.len())
                .any(|window| window == protocol_marker)
                && output
                    .windows(end_marker.len())
                    .any(|window| window == end_marker.as_bytes())
        }));

        let start = output
            .windows(start_marker.len())
            .position(|window| window == start_marker.as_bytes())
            .expect("the transaction start marker should be retained");
        let transaction_start = output[start..]
            .windows(b"\x1b\\".len())
            .position(|window| window == b"\x1b\\")
            .map(|offset| start + offset + b"\x1b\\".len())
            .expect("the transaction start marker should be terminated");
        let transaction_end = output[transaction_start..]
            .windows(end_marker.len())
            .position(|window| window == end_marker.as_bytes())
            .map(|offset| transaction_start + offset)
            .expect("the transaction end marker should be retained");
        let transaction_text =
            String::from_utf8_lossy(&output[transaction_start..transaction_end]).replace('\r', "");
        assert!(
            !String::from_utf8_lossy(&output[..start]).contains("MEZ_COMMAND_FILE"),
            "generated Fish wrapper source was exposed before its start marker"
        );
        parse_pane_path_resolution_output(&transaction_text, &request).unwrap();
        process
            .write_input(b"printf '__MEZ_AFTER_RESOLVER__\\n'\n")
            .unwrap();
        let after = read_fish_output_until(&mut process, |output| {
            output
                .windows(b"__MEZ_AFTER_RESOLVER__".len())
                .any(|window| window == b"__MEZ_AFTER_RESOLVER__")
        });
        assert!(
            String::from_utf8_lossy(&after).contains("__MEZ_AFTER_RESOLVER__"),
            "{:?}",
            String::from_utf8_lossy(&after)
        );

        process.terminate(Duration::from_millis(100)).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    /// Verifies the syntax-neutral shell identity probe is removed from Fish
    /// history before a later shell exit can persist the command source.
    ///
    /// The probe runs before dialect-specific transaction wrappers exist, so
    /// this protects the initial Fish bootstrap path that previously leaked
    /// the complete `/bin/sh -c` command into user history.
    fn managed_fish_compatibility_does_not_persist_shell_identity_probe() {
        let Some(fish) = fish_path_for_tests() else {
            eprintln!("skipping managed Fish history assertion because fish is unavailable");
            return;
        };
        let root = std::env::temp_dir().join(format!(
            "mez-managed-fish-identity-history-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let compatibility = ManagedFishCompatibility::new(
            MarkerToken::new("55555555555555555555555555555555").unwrap(),
        );
        let mut process = spawn_managed_fish(&fish, &compatibility, &root);
        std::thread::sleep(Duration::from_millis(100));
        let _ = process.read_available_output(64 * 1024);

        let marker = MarkerToken::new("66666666666666666666666666666666").unwrap();
        let probe =
            shell_identity_probe_command(marker.as_str(), "identity-turn", "agent-p1", "p1")
                .unwrap();
        process
            .write_input(format!("{probe}\n").as_bytes())
            .unwrap();
        let end_marker = format!("\x1b]133;D;0;mez_marker={};", marker.as_str());
        let _ = read_fish_output_until(&mut process, |output| {
            output
                .windows(end_marker.len())
                .any(|window| window == end_marker.as_bytes())
        });
        process
            .write_input(b"history save\nprintf '__MEZ_HISTORY_SAVED__\\n'\n")
            .unwrap();
        let _ = read_fish_output_until(&mut process, |output| {
            output
                .windows(b"__MEZ_HISTORY_SAVED__".len())
                .any(|window| window == b"__MEZ_HISTORY_SAVED__")
        });

        let history = std::fs::read_to_string(root.join("data/fish/fish_history"))
            .expect("Fish should persist the isolated history file");
        assert!(
            !history.contains("mez_marker=66666666666666666666666666666666"),
            "shell identity probe leaked into Fish history: {history:?}"
        );

        process.terminate(Duration::from_millis(100)).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
