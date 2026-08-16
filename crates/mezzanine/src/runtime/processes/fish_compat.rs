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

    /// Returns the pane-scoped token authenticating private Fish admission.
    pub(super) fn token(&self) -> &MarkerToken {
        &self._owner
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
functions --erase __mez_fish_hold_editor __mez_fish_restore_editor __mez_fish_private_trigger __mez_fish_private_receiver __mez_fish_publish_parent_restored __mez_fish_passive_command_is_internal __mez_fish_passive_prompt_start __mez_fish_passive_preexec __mez_fish_passive_postexec fish_prompt fish_right_prompt 2>/dev/null
set -g __MEZ_FISH_INTEGRATION_OWNER {}
set -e __MEZ_FISH_EDITOR_HELD __MEZ_FISH_HOLD_MARKER __MEZ_FISH_UNSUPPORTED_EDITOR_STATE __MEZ_FISH_SAVED_LINE __MEZ_FISH_SAVED_CURSOR __MEZ_FISH_SAVED_BIND_MODE __MEZ_FISH_PARENT_RESTORE_MARKER __MEZ_FISH_PARENT_RESTORE_STATUS __MEZ_FISH_PARENT_RESTORE_OUTCOME __MEZ_FISH_PASSIVE_COMMAND_ACTIVE __MEZ_FISH_PASSIVE_SKIP_POSTEXEC
function __mez_fish_hold_editor
    set -l hold_record
    read -l hold_record; or return 1
    set -l hold_fields (string split ' ' -- "$hold_record")
    if test (count $hold_fields) -ne 3; or test "$hold_fields[1]" != MEZ_FISH_RX1_HOLD; or test "$hold_fields[2]" != "$__MEZ_FISH_INTEGRATION_OWNER"; or test -z "$hold_fields[3]"
        return 1
    end
    set -g __MEZ_FISH_HOLD_MARKER "$hold_fields[3]"
    set -g __MEZ_FISH_UNSUPPORTED_EDITOR_STATE 0
    if commandline --search-mode; or commandline --paging-mode; or commandline --selection-start >/dev/null 2>&1; or commandline --selection-end >/dev/null 2>&1
        set -g __MEZ_FISH_UNSUPPORTED_EDITOR_STATE 1
    end
    set -g __MEZ_FISH_SAVED_LINE (commandline | string collect -N)
    set -g __MEZ_FISH_SAVED_CURSOR (commandline --cursor)
    set -g __MEZ_FISH_SAVED_BIND_MODE "$fish_bind_mode"
    set -g __MEZ_FISH_EDITOR_HELD 1
    if test "$__MEZ_FISH_UNSUPPORTED_EDITOR_STATE" -eq 0
        commandline --replace ''
    end
end
function __mez_fish_restore_editor
    if not set -q __MEZ_FISH_EDITOR_HELD
        return 0
    end
    commandline --replace -- "$__MEZ_FISH_SAVED_LINE"
    commandline --cursor "$__MEZ_FISH_SAVED_CURSOR"
    set fish_bind_mode "$__MEZ_FISH_SAVED_BIND_MODE"
    set -e __MEZ_FISH_EDITOR_HELD __MEZ_FISH_HOLD_MARKER __MEZ_FISH_UNSUPPORTED_EDITOR_STATE __MEZ_FISH_SAVED_LINE __MEZ_FISH_SAVED_CURSOR __MEZ_FISH_SAVED_BIND_MODE
end
function __mez_fish_private_receiver
    if not set -q __MEZ_FISH_EDITOR_HELD
        return 1
    end
    set -l begin_record
    read -l begin_record
    set -l begin_fields (string split ' ' -- "$begin_record")
    if test (count $begin_fields) -ne 6; or test "$begin_fields[1]" != MEZ_FISH_RX1_BEGIN; or test "$begin_fields[2]" != "$__MEZ_FISH_INTEGRATION_OWNER"; or test "$begin_fields[3]" != "$__MEZ_FISH_HOLD_MARKER"
        set -g __MEZ_FISH_PARENT_RESTORE_MARKER "$__MEZ_FISH_HOLD_MARKER"
        set -g __MEZ_FISH_PARENT_RESTORE_STATUS 65
        set -g __MEZ_FISH_PARENT_RESTORE_OUTCOME frame-rejected
        __mez_fish_restore_editor
        return 1
    end
    set -l marker "$begin_fields[3]"
    if test -z "$marker"; or not string match -rq '^[0-9]+$' -- "$begin_fields[4]"; or not string match -rq '^[0-9a-f]{{64}}$' -- "$begin_fields[5]"; or not string match -rq '^[0-9]+$' -- "$begin_fields[6]"
        set -g __MEZ_FISH_PARENT_RESTORE_MARKER "$marker"
        set -g __MEZ_FISH_PARENT_RESTORE_STATUS 65
        set -g __MEZ_FISH_PARENT_RESTORE_OUTCOME frame-rejected
        __mez_fish_restore_editor
        return 1
    end
    set -l expected_length "$begin_fields[4]"
    set -l expected_digest "$begin_fields[5]"
    set -l expected_chunks "$begin_fields[6]"
    if test "$expected_length" -gt 16777216; or test "$expected_chunks" -gt 294338; or test "$expected_length" -eq 0 -a "$expected_chunks" -ne 0; or test "$expected_length" -gt 0 -a "$expected_chunks" -eq 0
        set -g __MEZ_FISH_PARENT_RESTORE_MARKER "$marker"
        set -g __MEZ_FISH_PARENT_RESTORE_STATUS 65
        set -g __MEZ_FISH_PARENT_RESTORE_OUTCOME frame-rejected
        __mez_fish_restore_editor
        return 1
    end
    if test "$__MEZ_FISH_UNSUPPORTED_EDITOR_STATE" -eq 1
        set -g __MEZ_FISH_PARENT_RESTORE_MARKER "$marker"
        set -g __MEZ_FISH_PARENT_RESTORE_STATUS 65
        set -g __MEZ_FISH_PARENT_RESTORE_OUTCOME frame-rejected
        __mez_fish_restore_editor
        return 1
    end
    builtin printf '\033]133;R;mez_protocol=2;mez_shell=fish;mez_token=%s;mez_event=frame-admitted;mez_marker=%s\033\\' "$__MEZ_FISH_INTEGRATION_OWNER" "$marker"
    set -l source_status 1
    set -l source_file (command mktemp); or set source_file ''
    set -l encoded_file "$source_file.b64"
    set -l receive_status 0
    set -l sequence 0
    set -l encoded_bytes 0
    if test -z "$source_file"
        set receive_status 1
    else
        command printf '' > "$encoded_file"; or set receive_status $status
    end
    set -l cancelled 0
    while test "$sequence" -lt "$expected_chunks"
        set -l data_record
        read -l data_record; or begin; set receive_status 1; break; end
        set -l cancel_fields (string split ' ' -- "$data_record")
        if test (count $cancel_fields) -eq 3; and test "$cancel_fields[1]" = MEZ_FISH_RX1_CANCEL; and test "$cancel_fields[2]" = "$__MEZ_FISH_INTEGRATION_OWNER"; and test "$cancel_fields[3]" = "$marker"
            if test "$sequence" -eq 0
                set cancelled 1
                set source_status 130
                builtin printf '\036'
                break
            end
            set receive_status 1
            set sequence (math "$sequence + 1")
            builtin printf '\036'
            continue
        end
        set -l data_fields (string split -m 4 ' ' -- "$data_record")
        if test "$receive_status" -eq 0
            if test (count $data_fields) -ne 5; or test "$data_fields[1]" != MEZ_FISH_RX1_DATA; or test "$data_fields[2]" != "$__MEZ_FISH_INTEGRATION_OWNER"; or test "$data_fields[3]" != "$marker"; or test "$data_fields[4]" != "$sequence"; or test (string length -- "$data_fields[5]") -gt 640; or not string match -rq '^[A-Za-z0-9+/]*={{0,2}}$' -- "$data_fields[5]"
                set receive_status 1
            else
                set encoded_bytes (math "$encoded_bytes + "(string length -- "$data_fields[5]"))
                if test "$encoded_bytes" -gt 22369624
                    set receive_status 1
                else
                    command printf '%s' "$data_fields[5]" >> "$encoded_file"; or set receive_status $status
                end
            end
        end
        set sequence (math "$sequence + 1")
        builtin printf '\036'
    end
    if test "$cancelled" -eq 0
        set -l end_record
        read -l end_record; or set receive_status 1
        if test -n "$end_record"; and test "$receive_status" -eq 0
            set -l end_fields (string split ' ' -- "$end_record")
            if test (count $end_fields) -ne 6; or test "$end_fields[1]" != MEZ_FISH_RX1_END; or test "$end_fields[2]" != "$__MEZ_FISH_INTEGRATION_OWNER"; or test "$end_fields[3]" != "$marker"; or test "$end_fields[4]" != "$expected_chunks"; or test "$end_fields[5]" != "$expected_length"; or test "$end_fields[6]" != "$expected_digest"
                set receive_status 1
            end
        end
        builtin printf '\036'
    end
    if test "$cancelled" -eq 0; and test "$receive_status" -eq 0
        if command printf '' | command base64 -d >/dev/null 2>&1
            command base64 -d < "$encoded_file" > "$source_file" 2>/dev/null; or set receive_status $status
        else
            command base64 -D < "$encoded_file" > "$source_file"; or set receive_status $status
        end
    end
    if test "$cancelled" -eq 0; and test "$receive_status" -eq 0
        set -l expected_encoded (command cat -- "$encoded_file" | string collect -N); or set receive_status $status
        set -l actual_encoded (command base64 < "$source_file" | command tr -d '\r\n' | string collect -N); or set receive_status $status
        if test "$receive_status" -eq 0; and test "$actual_encoded" != "$expected_encoded"
            set receive_status 1
        end
    end
    if test "$cancelled" -eq 0; and test "$receive_status" -eq 0
        set -l actual_length (command wc -c < "$source_file" | string trim)
        set -l actual_digest ''
        if command -q sha256sum
            set actual_digest (command sha256sum -- "$source_file" | string split -f 1 ' ')
        else if command -q shasum
            set actual_digest (command shasum -a 256 -- "$source_file" | string split -f 1 ' ')
        else
            set receive_status 127
        end
        if test "$receive_status" -eq 0; and test "$actual_length" = "$expected_length"; and test "$actual_digest" = "$expected_digest"
            source "$source_file"
            set source_status $status
        else
            set source_status 1
        end
    end
    if test -n "$source_file"
        command rm -f -- "$source_file" "$encoded_file" >/dev/null 2>&1; or true
    end
    set -g __MEZ_FISH_PARENT_RESTORE_MARKER "$marker"
    set -g __MEZ_FISH_PARENT_RESTORE_STATUS "$source_status"
    if test "$cancelled" -eq 1
        set -g __MEZ_FISH_PARENT_RESTORE_OUTCOME cancelled
    else if test "$receive_status" -ne 0
        set -g __MEZ_FISH_PARENT_RESTORE_OUTCOME frame-rejected
        set -g __MEZ_FISH_PARENT_RESTORE_STATUS 65
    else if test "$source_status" -eq 0
        set -g __MEZ_FISH_PARENT_RESTORE_OUTCOME completed
    else
        set -g __MEZ_FISH_PARENT_RESTORE_OUTCOME source-failed
    end
    __mez_fish_restore_editor
    return $source_status
end
function __mez_fish_publish_parent_restored
    if not set -q __MEZ_FISH_PARENT_RESTORE_MARKER
        return 0
    end
    builtin printf '\033]133;R;mez_protocol=2;mez_shell=fish;mez_token=%s;mez_event=parent-ready;mez_marker=%s;mez_outcome=%s;mez_status=%s\033\\' "$__MEZ_FISH_INTEGRATION_OWNER" "$__MEZ_FISH_PARENT_RESTORE_MARKER" "$__MEZ_FISH_PARENT_RESTORE_OUTCOME" "$__MEZ_FISH_PARENT_RESTORE_STATUS"
    set -e __MEZ_FISH_PARENT_RESTORE_MARKER __MEZ_FISH_PARENT_RESTORE_STATUS __MEZ_FISH_PARENT_RESTORE_OUTCOME
end
function __mez_fish_private_trigger
    if not set -q __MEZ_FISH_EDITOR_HELD
        __mez_fish_hold_editor; or return $status
        commandline -f repaint
        builtin printf '\033]133;R;mez_protocol=2;mez_shell=fish;mez_token=%s;mez_event=editor-held;mez_marker=%s\033\\' "$__MEZ_FISH_INTEGRATION_OWNER" "$__MEZ_FISH_HOLD_MARKER"
        return 0
    end
    __mez_fish_private_receiver
    set -l receiver_status $status
    commandline -f repaint
    __mez_fish_publish_parent_restored
    return $receiver_status
end
bind -M default \e\cg __mez_fish_private_trigger
bind -M insert \e\cg __mez_fish_private_trigger
bind -M visual \e\cg __mez_fish_private_trigger
bind -M replace_one \e\cg __mez_fish_private_trigger
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
builtin printf '\033]133;R;mez_protocol=2;mez_shell=fish;mez_token=%s;mez_event=adapter-available\033\\' "$__MEZ_FISH_INTEGRATION_OWNER"
"#,
        fish_wrapper_receiver_init_command(),
        mez_agent::fish_quote(owner.as_str())
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use mez_agent::shell::{
        FishPrivateSourceInput, PanePathResolutionRequest, ShellClassification, ShellTransaction,
        agent_subshell_enter_command_with_shell_compatibility_and_exit_marker,
        fish_private_source_cancel_input, fish_private_source_input, pane_path_resolution_command,
        parse_pane_path_resolution_output, shell_identity_probe_command,
    };
    use mez_mux::process::{
        PaneProcess, PaneProcessEnvironment, ShellInputDelivery, pane_command_plan,
        spawn_pane_process,
    };
    use mez_mux::{Result, layout::Size};
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

    /// Drives one managed Fish PTY through the terminal response contract used
    /// by production pane output handling.
    struct ManagedFishTestPane {
        process: PaneProcess,
        terminal: mez_terminal::TerminalScreen,
    }

    impl ManagedFishTestPane {
        /// Reads available output, applies it to the terminal emulator, and
        /// writes any terminal-generated query responses back to Fish.
        fn read_available_output(&mut self, max_bytes: usize) -> Result<Vec<u8>> {
            let output = self.process.read_available_output(max_bytes)?;
            self.terminal.feed(&output);
            let responses = self.terminal.drain_terminal_response_bytes();
            if !responses.is_empty() {
                self.process.write_input(&responses)?;
            }
            Ok(output)
        }

        /// Writes user or generated shell input to the managed Fish process.
        fn write_input(&mut self, input: &[u8]) -> Result<()> {
            self.process.write_input(input)
        }

        /// Writes acknowledged Fish transport one record at a time while
        /// continuing to service terminal queries between records.
        fn write_shell_delivery(&mut self, delivery: &ShellInputDelivery) -> Vec<u8> {
            #[cfg(not(target_os = "macos"))]
            {
                self.process
                    .write_shell_delivery(delivery)
                    .expect("managed Fish shell delivery should remain writable");
                Vec::new()
            }

            #[cfg(target_os = "macos")]
            {
                let mut output = Vec::new();
                for record in delivery.bytes.split_inclusive(|byte| *byte == b'\n') {
                    self.process
                        .write_input(record)
                        .expect("managed Fish shell record should remain writable");
                    let deadline = Instant::now() + Duration::from_secs(5);
                    loop {
                        let record_output = self
                            .read_available_output(64 * 1024)
                            .expect("managed Fish shell acknowledgement should remain readable");
                        let acknowledged = record_output.contains(&0x1e);
                        output.extend(record_output);
                        if acknowledged {
                            break;
                        }
                        if Instant::now() >= deadline {
                            let _ = self.terminate(Duration::from_millis(100));
                            panic!(
                                "managed Fish shell record was not acknowledged: {:?}",
                                String::from_utf8_lossy(&output)
                            );
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
                output
            }
        }

        /// Terminates the managed Fish process within the supplied deadline.
        fn terminate(&mut self, timeout: Duration) -> Result<mez_mux::process::PaneExitStatus> {
            self.process.terminate(timeout)
        }
    }

    /// Extends captured Fish output until a predicate succeeds or a bounded
    /// deadline expires.
    fn extend_fish_output_until(
        process: &mut ManagedFishTestPane,
        output: &mut Vec<u8>,
        predicate: impl Fn(&[u8]) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            output.extend(
                process
                    .read_available_output(64 * 1024)
                    .expect("managed Fish output should remain readable"),
            );
            if predicate(output) {
                return;
            }
            if Instant::now() >= deadline {
                let _ = process.terminate(Duration::from_millis(100));
                panic!(
                    "managed Fish output did not reach its expected boundary: {:?}",
                    String::from_utf8_lossy(output)
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Reads one Fish PTY until a predicate succeeds or a bounded deadline expires.
    fn read_fish_output_until(
        process: &mut ManagedFishTestPane,
        predicate: impl Fn(&[u8]) -> bool,
    ) -> Vec<u8> {
        let mut output = Vec::new();
        extend_fish_output_until(process, &mut output, predicate);
        output
    }

    /// Drives the source-free Fish hold stage and waits for native editor ownership.
    fn hold_managed_fish_editor(
        process: &mut ManagedFishTestPane,
        admission: &FishPrivateSourceInput,
        owner: &MarkerToken,
        marker: &str,
    ) -> Vec<u8> {
        process.write_input(admission.wrapper.as_bytes()).unwrap();
        let editor_held = format!(
            "mez_protocol=2;mez_shell=fish;mez_token={};mez_event=editor-held;mez_marker={marker}",
            owner.as_str()
        );
        read_fish_output_until(process, |output| {
            output
                .windows(editor_held.len())
                .any(|window| window == editor_held.as_bytes())
        })
    }

    /// Releases Fish BEGIN after editor hold and waits for authenticated admission.
    fn admit_managed_fish_frame(
        process: &mut ManagedFishTestPane,
        admission: &FishPrivateSourceInput,
        owner: &MarkerToken,
        marker: &str,
    ) -> Vec<u8> {
        let mut output = hold_managed_fish_editor(process, admission, owner, marker);
        process
            .write_input(admission.receiver_admission.as_bytes())
            .unwrap();
        let frame_admitted = format!(
            "mez_protocol=2;mez_shell=fish;mez_token={};mez_event=frame-admitted;mez_marker={marker}",
            owner.as_str()
        );
        extend_fish_output_until(process, &mut output, |output| {
            output
                .windows(frame_admitted.len())
                .any(|window| window == frame_admitted.as_bytes())
        });
        output
    }

    /// Drives Fish startup through terminal capability negotiation and waits
    /// until both configured prompt functions have rendered completely.
    fn settle_managed_fish_startup(process: &mut ManagedFishTestPane) {
        let _ = read_fish_output_until(process, |output| {
            output
                .windows(b"__MEZ_USER_PROMPT__status=0>".len())
                .any(|window| window == b"__MEZ_USER_PROMPT__status=0>")
                && output
                    .windows(b"__MEZ_USER_RIGHT_PROMPT__".len())
                    .any(|window| window == b"__MEZ_USER_RIGHT_PROMPT__")
        });
    }

    /// Spawns one isolated ordinary Fish pane with managed passive integration.
    fn spawn_managed_fish(
        fish: &Path,
        compatibility: &ManagedFishCompatibility,
        home: &Path,
    ) -> ManagedFishTestPane {
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
        let size = Size::new(80, 24).expect("the Fish PTY size should be valid");
        let process = spawn_pane_process(&launch, None, &test_environment(), size)
            .expect("the managed Fish pane should spawn");
        let terminal = mez_terminal::TerminalScreen::new(size, 1_000)
            .expect("the managed Fish terminal screen should initialize");
        ManagedFishTestPane { process, terminal }
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
    /// Verifies the persistent Fish child announces its authenticated semantic
    /// installation event from the first interactive prompt under real PTY semantics.
    ///
    /// Bootstrap input remains deferred until this boundary, so the generated
    /// child handoff must emit it before accepting any agent transaction.
    fn managed_fish_agent_child_emits_child_installed_at_first_prompt() {
        let Some(fish) = fish_path_for_tests() else {
            eprintln!("skipping managed Fish child assertion because fish is unavailable");
            return;
        };
        let root = std::env::temp_dir().join(format!(
            "mez-managed-fish-child-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let compatibility = ManagedFishCompatibility::new(
            MarkerToken::new("77777777777777777777777777777777").unwrap(),
        );
        let mut process = spawn_managed_fish(&fish, &compatibility, &root);
        settle_managed_fish_startup(&mut process);

        let receiver_token = MarkerToken::new("88888888888888888888888888888888").unwrap();
        let bootstrap_marker = "fish-child-bootstrap-marker";
        let handoff = agent_subshell_enter_command_with_shell_compatibility_and_exit_marker(
            &fish,
            ShellClassification::Fish,
            None,
            None,
            None,
            None,
            Some((&receiver_token, bootstrap_marker)),
            None,
            None,
        )
        .unwrap();
        process.write_input(handoff.as_bytes()).unwrap();
        let expected = format!(
            "mez_protocol=2;mez_shell=fish;mez_token={};mez_event=child-installed;mez_marker={bootstrap_marker}",
            receiver_token.as_str()
        );
        let output = read_fish_output_until(&mut process, |output| {
            output
                .windows(expected.len())
                .any(|window| window == expected.as_bytes())
        });
        assert!(
            output
                .windows(expected.len())
                .any(|window| window == expected.as_bytes()),
            "{:?}",
            String::from_utf8_lossy(&output)
        );

        let transaction_marker = MarkerToken::new("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap();
        let input = ShellTransaction::new(
            transaction_marker.clone(),
            "fish-child-turn",
            "fish-child-agent",
            "p1",
            &fish,
            mez_agent::fish_bootstrap_script(),
        )
        .unwrap()
        .with_payload_receiver_acknowledgements(cfg!(target_os = "macos"))
        .render_for_classification_input(ShellClassification::Fish);
        let mut transaction_output =
            process.write_shell_delivery(&ShellInputDelivery::generated_source_for_transaction(
                input.wrapper.as_bytes().to_vec(),
                transaction_marker.as_str(),
            ));
        let start_marker = format!("\x1b]133;C;mez_marker={};", transaction_marker.as_str());
        extend_fish_output_until(&mut process, &mut transaction_output, |output| {
            output
                .windows(start_marker.len())
                .any(|window| window == start_marker.as_bytes())
        });
        transaction_output.extend(process.write_shell_delivery(
            &ShellInputDelivery::receiver_acknowledged(
                input.payload.as_bytes().to_vec(),
                transaction_marker.as_str(),
                input.payload_receiver_acknowledgements,
            ),
        ));
        let end_marker = format!("\x1b]133;D;0;mez_marker={};", transaction_marker.as_str());
        extend_fish_output_until(&mut process, &mut transaction_output, |output| {
            output
                .windows(end_marker.len())
                .any(|window| window == end_marker.as_bytes())
        });
        assert!(
            String::from_utf8_lossy(&transaction_output).contains("bootstrap\tcomplete\t"),
            "{:?}",
            String::from_utf8_lossy(&transaction_output)
        );

        process.write_input(b"exit\n").unwrap();
        process.terminate(Duration::from_millis(100)).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    /// Verifies bounded private Fish admission sources the authenticated child
    /// handoff and reaches its first-prompt receiver-installed boundary.
    ///
    /// This covers the complete parent receiver path used to preserve an
    /// unsubmitted editor draft before runtime transfers ownership to a child.
    fn managed_fish_private_admission_starts_authenticated_child() {
        let Some(fish) = fish_path_for_tests() else {
            eprintln!(
                "skipping managed Fish private admission assertion because fish is unavailable"
            );
            return;
        };
        let root = std::env::temp_dir().join(format!(
            "mez-managed-fish-private-child-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let owner = MarkerToken::new("99999999999999999999999999999999").unwrap();
        let compatibility = ManagedFishCompatibility::new(owner.clone());
        let mut process = spawn_managed_fish(&fish, &compatibility, &root);
        settle_managed_fish_startup(&mut process);

        let bootstrap_marker = "fish-private-child-bootstrap-marker";
        let handoff = agent_subshell_enter_command_with_shell_compatibility_and_exit_marker(
            &fish,
            ShellClassification::Fish,
            None,
            None,
            None,
            None,
            Some((&owner, bootstrap_marker)),
            None,
            None,
        )
        .unwrap();
        let instrumented_handoff =
            format!("builtin printf '__MEZ_PRIVATE_SOURCE_ENTERED__\\n'\n{handoff}");
        let admission = fish_private_source_input(&instrumented_handoff, &owner, bootstrap_marker);
        let mut output =
            admit_managed_fish_frame(&mut process, &admission, &owner, bootstrap_marker);
        output.extend(
            process.write_shell_delivery(&ShellInputDelivery::receiver_acknowledged(
                admission.receiver_payload.as_bytes().to_vec(),
                bootstrap_marker,
                admission.payload_receiver_acknowledgements,
            )),
        );
        let installed = format!(
            "mez_protocol=2;mez_shell=fish;mez_token={};mez_event=child-installed;mez_marker={bootstrap_marker}",
            owner.as_str()
        );
        extend_fish_output_until(&mut process, &mut output, |output| {
            output
                .windows(installed.len())
                .any(|window| window == installed.as_bytes())
        });
        assert!(
            output
                .windows(b"__MEZ_PRIVATE_SOURCE_ENTERED__".len())
                .any(|window| window == b"__MEZ_PRIVATE_SOURCE_ENTERED__"),
            "{:?}",
            String::from_utf8_lossy(&output)
        );

        process.write_input(b"exit\n").unwrap();
        process.terminate(Duration::from_millis(100)).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    /// Verifies bounded private Fish admission validates and executes a minimal
    /// authenticated source payload before reporting receiver completion.
    ///
    /// This isolates frame decoding, length, and digest validation from the
    /// persistent child-shell handoff exercised by the broader admission test.
    fn managed_fish_private_admission_executes_minimal_source() {
        let Some(fish) = fish_path_for_tests() else {
            eprintln!("skipping managed Fish private source assertion because fish is unavailable");
            return;
        };
        let root = std::env::temp_dir().join(format!(
            "mez-managed-fish-private-source-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let owner = MarkerToken::new("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        let compatibility = ManagedFishCompatibility::new(owner.clone());
        let mut process = spawn_managed_fish(&fish, &compatibility, &root);
        settle_managed_fish_startup(&mut process);

        let marker = "fish-private-minimal-marker";
        let admission = fish_private_source_input(
            "builtin printf '__MEZ_PRIVATE_MINIMAL_EXECUTED__\\n'\n",
            &owner,
            marker,
        );
        let mut output = admit_managed_fish_frame(&mut process, &admission, &owner, marker);
        output.extend(
            process.write_shell_delivery(&ShellInputDelivery::receiver_acknowledged(
                admission.receiver_payload.as_bytes().to_vec(),
                marker,
                admission.payload_receiver_acknowledgements,
            )),
        );
        extend_fish_output_until(&mut process, &mut output, |output| {
            output
                .windows(b"__MEZ_PRIVATE_MINIMAL_EXECUTED__".len())
                .any(|window| window == b"__MEZ_PRIVATE_MINIMAL_EXECUTED__")
        });

        process.terminate(Duration::from_millis(100)).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    /// Verifies private Fish admission fails closed while vi visual selection
    /// is active and leaves the selected draft available for normal execution.
    ///
    /// The source-free HOLD trigger must not publish editor ownership or admit
    /// deferred source while selection boundaries cannot be restored exactly.
    fn managed_fish_private_admission_rejects_active_selection_without_clearing_draft() {
        let Some(fish) = fish_path_for_tests() else {
            eprintln!("skipping managed Fish selection assertion because fish is unavailable");
            return;
        };
        let root = std::env::temp_dir().join(format!(
            "mez-managed-fish-private-selection-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let owner = MarkerToken::new("cccccccccccccccccccccccccccccccc").unwrap();
        let compatibility = ManagedFishCompatibility::new(owner.clone());
        let mut process = spawn_managed_fish(&fish, &compatibility, &root);
        settle_managed_fish_startup(&mut process);

        process
            .write_input(
                b"fish_vi_key_bindings; bind -M insert -m visual \\co begin-selection; printf '__MEZ_SELECTION_READY__\\n'\n",
            )
            .unwrap();
        let _ = read_fish_output_until(&mut process, |output| {
            output
                .windows(b"__MEZ_SELECTION_READY__".len())
                .any(|window| window == b"__MEZ_SELECTION_READY__")
        });
        process
            .write_input(b"printf '__MEZ_SELECTION_DRAFT__\\n'\x0f")
            .unwrap();
        let marker = "fish-private-selection-marker";
        let admission = fish_private_source_input(
            "builtin printf '__MEZ_SELECTION_SOURCE_RAN__\\n'\n",
            &owner,
            marker,
        );
        process.write_input(admission.wrapper.as_bytes()).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        let mut output = process.read_available_output(64 * 1024).unwrap();
        assert!(
            !String::from_utf8_lossy(&output).contains("mez_event=editor-held")
                && !String::from_utf8_lossy(&output).contains("mez_event=frame-admitted"),
            "{:?}",
            String::from_utf8_lossy(&output)
        );
        process.write_input(b"\x1b\n").unwrap();
        extend_fish_output_until(&mut process, &mut output, |output| {
            output
                .windows(b"__MEZ_SELECTION_DRAFT__".len())
                .any(|window| window == b"__MEZ_SELECTION_DRAFT__")
        });
        assert!(
            !String::from_utf8_lossy(&output).contains("__MEZ_SELECTION_SOURCE_RAN__"),
            "{:?}",
            String::from_utf8_lossy(&output)
        );

        process.terminate(Duration::from_millis(100)).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    /// Verifies a sourced Fish admission failure restores the exact parent
    /// draft and reports failure without executing or discarding that draft.
    fn managed_fish_private_admission_source_failure_restores_draft() {
        let Some(fish) = fish_path_for_tests() else {
            eprintln!("skipping managed Fish source-failure assertion because fish is unavailable");
            return;
        };
        let root = std::env::temp_dir().join(format!(
            "mez-managed-fish-private-failure-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let owner = MarkerToken::new("dddddddddddddddddddddddddddddddd").unwrap();
        let compatibility = ManagedFishCompatibility::new(owner.clone());
        let mut process = spawn_managed_fish(&fish, &compatibility, &root);
        settle_managed_fish_startup(&mut process);

        process
            .write_input(b"printf '__MEZ_FAILURE_DRAFT_RESTORED__\\n'")
            .unwrap();
        let marker = "fish-private-source-failure-marker";
        let admission = fish_private_source_input("false\n", &owner, marker);
        let mut output = admit_managed_fish_frame(&mut process, &admission, &owner, marker);
        output.extend(
            process.write_shell_delivery(&ShellInputDelivery::receiver_acknowledged(
                admission.receiver_payload.as_bytes().to_vec(),
                marker,
                admission.payload_receiver_acknowledgements,
            )),
        );
        let restored = format!(
            "mez_protocol=2;mez_shell=fish;mez_token={};mez_event=parent-ready;mez_marker={marker};mez_outcome=source-failed;mez_status=1",
            owner.as_str()
        );
        extend_fish_output_until(&mut process, &mut output, |output| {
            output
                .windows(restored.len())
                .any(|window| window == restored.as_bytes())
        });
        process.write_input(b"\n").unwrap();
        extend_fish_output_until(&mut process, &mut output, |output| {
            output
                .windows(b"__MEZ_FAILURE_DRAFT_RESTORED__".len())
                .any(|window| window == b"__MEZ_FAILURE_DRAFT_RESTORED__")
        });

        process.terminate(Duration::from_millis(100)).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    /// Verifies an authenticated cancellation received before the first DATA
    /// record restores the exact Fish draft without evaluating handoff source.
    ///
    /// This is the early agent-exit boundary: Fish has already saved and
    /// cleared its editor, but runtime no longer wants to launch the child.
    fn managed_fish_private_admission_cancellation_restores_draft_without_launching_child() {
        let Some(fish) = fish_path_for_tests() else {
            eprintln!("skipping managed Fish cancellation assertion because fish is unavailable");
            return;
        };
        let root = std::env::temp_dir().join(format!(
            "mez-managed-fish-private-cancel-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let owner = MarkerToken::new("abababababababababababababababab").unwrap();
        let compatibility = ManagedFishCompatibility::new(owner.clone());
        let mut process = spawn_managed_fish(&fish, &compatibility, &root);
        settle_managed_fish_startup(&mut process);

        process
            .write_input(b"printf '__MEZ_CANCELLED_DRAFT_RESTORED__\\n'")
            .unwrap();
        let marker = "fish-private-cancel-marker";
        let admission = fish_private_source_input(
            "builtin printf '__MEZ_CANCELLED_SOURCE_RAN__\\n'\n",
            &owner,
            marker,
        );
        let mut output = admit_managed_fish_frame(&mut process, &admission, &owner, marker);
        assert!(
            !process
                .terminal
                .visible_lines()
                .join("\n")
                .contains("__MEZ_CANCELLED_DRAFT_RESTORED__"),
            "Fish must visibly clear the saved draft before frame admission; screen={:?}; output={:?}",
            process.terminal.visible_lines(),
            String::from_utf8_lossy(&output)
        );
        process
            .write_input(fish_private_source_cancel_input(&owner, marker).as_bytes())
            .unwrap();
        let restored = format!(
            "mez_protocol=2;mez_shell=fish;mez_token={};mez_event=parent-ready;mez_marker={marker};mez_outcome=cancelled;mez_status=130",
            owner.as_str()
        );
        extend_fish_output_until(&mut process, &mut output, |output| {
            output
                .windows(restored.len())
                .any(|window| window == restored.as_bytes())
        });
        process.write_input(b"\n").unwrap();
        extend_fish_output_until(&mut process, &mut output, |output| {
            output
                .windows(b"__MEZ_CANCELLED_DRAFT_RESTORED__".len())
                .any(|window| window == b"__MEZ_CANCELLED_DRAFT_RESTORED__")
        });
        assert!(
            !String::from_utf8_lossy(&output).contains("__MEZ_CANCELLED_SOURCE_RAN__"),
            "cancelled private source must not launch the child handoff"
        );

        process.terminate(Duration::from_millis(100)).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    /// Verifies cancellation received after DATA starts is rejected only after
    /// Fish drains the remaining declared records and matching END boundary.
    ///
    /// A late cancellation occupies one DATA record slot but cannot shorten
    /// the admitted frame. Counting it incorrectly strands acknowledgement-
    /// paced delivery or leaves END waiting in the ordinary line editor.
    fn managed_fish_private_admission_drains_late_cancellation_through_end() {
        let Some(fish) = fish_path_for_tests() else {
            eprintln!(
                "skipping managed Fish late-cancellation assertion because fish is unavailable"
            );
            return;
        };
        let root = std::env::temp_dir().join(format!(
            "mez-managed-fish-private-late-cancel-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let owner = MarkerToken::new("cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd").unwrap();
        let compatibility = ManagedFishCompatibility::new(owner.clone());
        let mut process = spawn_managed_fish(&fish, &compatibility, &root);
        settle_managed_fish_startup(&mut process);

        let marker = "fish-private-late-cancel-marker";
        let source = format!(
            "builtin printf '__MEZ_LATE_CANCEL_SOURCE_RAN__\\n'\n# {}\n",
            "padding".repeat(256)
        );
        let admission = fish_private_source_input(&source, &owner, marker);
        let mut output = admit_managed_fish_frame(&mut process, &admission, &owner, marker);
        let mut records = admission
            .receiver_payload
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert!(
            records.len() >= 4,
            "fixture must contain multiple DATA records"
        );
        records[1] = fish_private_source_cancel_input(&owner, marker)
            .trim_end()
            .to_string();
        let late_cancellation_payload = records.join("\n") + "\n";
        output.extend(
            process.write_shell_delivery(&ShellInputDelivery::receiver_acknowledged(
                late_cancellation_payload.into_bytes(),
                marker,
                true,
            )),
        );
        let restored = format!(
            "mez_protocol=2;mez_shell=fish;mez_token={};mez_event=parent-ready;mez_marker={marker};mez_outcome=frame-rejected;mez_status=65",
            owner.as_str()
        );
        extend_fish_output_until(&mut process, &mut output, |output| {
            output
                .windows(restored.len())
                .any(|window| window == restored.as_bytes())
        });
        process
            .write_input(b"builtin printf '__MEZ_AFTER_LATE_CANCEL__\\n'\n")
            .unwrap();
        extend_fish_output_until(&mut process, &mut output, |output| {
            output
                .windows(b"__MEZ_AFTER_LATE_CANCEL__".len())
                .any(|window| window == b"__MEZ_AFTER_LATE_CANCEL__")
        });
        assert!(
            !String::from_utf8_lossy(&output).contains("__MEZ_LATE_CANCEL_SOURCE_RAN__"),
            "late-cancelled private source must not execute"
        );

        process.terminate(Duration::from_millis(100)).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    /// Verifies a malformed DATA record is rejected only after Fish drains and
    /// acknowledges the complete admitted frame through its END record.
    ///
    /// Draining prevents remaining Base64 records from entering the editable
    /// command line and prevents paced macOS delivery from waiting forever for
    /// an acknowledgement the receiver abandoned after the first defect.
    fn managed_fish_private_admission_drains_malformed_frame_before_rejecting() {
        let Some(fish) = fish_path_for_tests() else {
            eprintln!(
                "skipping managed Fish malformed-frame assertion because fish is unavailable"
            );
            return;
        };
        let root = std::env::temp_dir().join(format!(
            "mez-managed-fish-private-malformed-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let owner = MarkerToken::new("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee").unwrap();
        let compatibility = ManagedFishCompatibility::new(owner.clone());
        let mut process = spawn_managed_fish(&fish, &compatibility, &root);
        settle_managed_fish_startup(&mut process);

        let marker = "fish-private-malformed-marker";
        let source = format!(
            "builtin printf '__MEZ_MALFORMED_SOURCE_RAN__\\n'\n# {}\n",
            "padding".repeat(256)
        );
        let admission = fish_private_source_input(&source, &owner, marker);
        let mut output = admit_managed_fish_frame(&mut process, &admission, &owner, marker);
        let mut records = admission
            .receiver_payload
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert!(
            records.len() >= 4,
            "fixture must contain multiple DATA records"
        );
        let malformed = records[1]
            .split_whitespace()
            .take(4)
            .collect::<Vec<_>>()
            .join(" ");
        records[1] = format!("{malformed} not-base64!");
        let malformed_payload = records.join("\n") + "\n";
        output.extend(
            process.write_shell_delivery(&ShellInputDelivery::receiver_acknowledged(
                malformed_payload.into_bytes(),
                marker,
                true,
            )),
        );
        let restored = format!(
            "mez_protocol=2;mez_shell=fish;mez_token={};mez_event=parent-ready;mez_marker={marker};mez_outcome=frame-rejected;mez_status=65",
            owner.as_str()
        );
        extend_fish_output_until(&mut process, &mut output, |output| {
            output
                .windows(restored.len())
                .any(|window| window == restored.as_bytes())
        });
        process
            .write_input(b"builtin printf '__MEZ_AFTER_MALFORMED_FRAME__\\n'\n")
            .unwrap();
        extend_fish_output_until(&mut process, &mut output, |output| {
            output
                .windows(b"__MEZ_AFTER_MALFORMED_FRAME__".len())
                .any(|window| window == b"__MEZ_AFTER_MALFORMED_FRAME__")
        });
        assert!(
            !String::from_utf8_lossy(&output).contains("__MEZ_MALFORMED_SOURCE_RAN__"),
            "malformed private source must not execute"
        );

        process.terminate(Duration::from_millis(100)).unwrap();
        std::fs::remove_dir_all(root).unwrap();
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
        settle_managed_fish_startup(&mut first_process);
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
                && output
                    .windows(b"__MEZ_USER_PROMPT__status=0>".len())
                    .any(|window| window == b"__MEZ_USER_PROMPT__status=0>")
                && output
                    .windows(b"__MEZ_USER_RIGHT_PROMPT__".len())
                    .any(|window| window == b"__MEZ_USER_RIGHT_PROMPT__")
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
        let prompt_start = first_text[completion..]
            .find("\u{1b}]133;A\u{1b}\\")
            .map(|offset| completion + offset)
            .expect("the first prompt-start boundary should be present");
        let user_prompt = first_text[prompt_start..]
            .find("__MEZ_USER_PROMPT__status=0>")
            .map(|offset| prompt_start + offset)
            .expect("the configured user prompt should be preserved");
        let prompt_end = first_text[user_prompt..]
            .find("\u{1b}]133;B\u{1b}\\")
            .map(|offset| user_prompt + offset)
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
        settle_managed_fish_startup(&mut second_process);
        let replacement_path = root.join("second/replacement.fish");
        std::fs::write(&replacement_path, second.init_command())
            .expect("the replacement Fish integration source should be written");
        let replacement = format!(
            "source {} # mez_marker=replacement mez_turn=replacement\n",
            mez_agent::fish_quote(&replacement_path.to_string_lossy())
        );
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
                && output
                    .windows(b"\x1b]133;A\x1b\\".len())
                    .filter(|window| *window == b"\x1b]133;A\x1b\\")
                    .count()
                    >= 5
                && output
                    .windows(b"\x1b]133;B\x1b\\".len())
                    .filter(|window| *window == b"\x1b]133;B\x1b\\")
                    .count()
                    >= 5
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
        let successful_completions = second_output
            .windows(b"\x1b]133;D;0\x1b\\".len())
            .filter(|window| *window == b"\x1b]133;D;0\x1b\\")
            .count();
        let failed_completions = second_output
            .windows(b"\x1b]133;D;1\x1b\\".len())
            .filter(|window| *window == b"\x1b]133;D;1\x1b\\")
            .count();
        assert!(
            successful_completions >= 2 && failed_completions >= 1,
            "Fish and Mezzanine completion boundaries should both preserve command status: {:?}",
            String::from_utf8_lossy(&second_output)
        );
        for boundary in [
            b"\x1b]133;A\x1b\\".as_slice(),
            b"\x1b]133;B\x1b\\".as_slice(),
        ] {
            assert!(
                second_output
                    .windows(boundary.len())
                    .filter(|window| *window == boundary)
                    .count()
                    >= 5,
                "Fish should publish a prompt boundary after every completed command: {:?}",
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
        settle_managed_fish_startup(&mut process);

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
        .with_payload_receiver_acknowledgements(cfg!(target_os = "macos"))
        .render_for_classification_input(ShellClassification::Fish);
        let mut output =
            process.write_shell_delivery(&ShellInputDelivery::generated_source_for_transaction(
                input.wrapper.as_bytes().to_vec(),
                marker.as_str(),
            ));
        let start_marker = format!("\x1b]133;C;mez_marker={};", marker.as_str());
        extend_fish_output_until(&mut process, &mut output, |output| {
            output
                .windows(start_marker.len())
                .any(|window| window == start_marker.as_bytes())
        });
        output.extend(
            process.write_shell_delivery(&ShellInputDelivery::receiver_acknowledged(
                input.payload.as_bytes().to_vec(),
                marker.as_str(),
                input.payload_receiver_acknowledgements,
            )),
        );
        let protocol_marker = b"MEZ_PATH_RESOLUTION_V2\t";
        let end_marker = format!("\x1b]133;D;0;mez_marker={};", marker.as_str());
        extend_fish_output_until(&mut process, &mut output, |output| {
            output
                .windows(protocol_marker.len())
                .any(|window| window == protocol_marker)
                && output
                    .windows(end_marker.len())
                    .any(|window| window == end_marker.as_bytes())
        });

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
        settle_managed_fish_startup(&mut process);

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
