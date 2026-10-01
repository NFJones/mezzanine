//! Dialect-specific transaction composition over canonical transport contracts.
//!
//! Stateful commands preserve the active shell, while isolated commands execute
//! validated child launches. Both retain the same marker identity and cleanup
//! ordering; receiver encoders own framing and product adapters own pane I/O.

use super::*;

impl ShellTransaction {
    /// Renders a complete isolated POSIX transaction as staged input combined.
    pub fn render_posix(&self) -> String {
        self.render_posix_input_for_classification(ShellClassification::PosixSh)
            .combined()
    }

    /// Renders a POSIX-compatible shell transaction wrapper for one resolved
    /// shell classification.
    ///
    /// The wrapper is parsed by the persistent agent shell, then starts a
    /// startup-suppressed child shell to execute the materialized command file.
    fn render_posix_input_for_classification(
        &self,
        classification: ShellClassification,
    ) -> ShellTransactionInput {
        if classification == ShellClassification::Bash && self.bash_receiver_token.is_none() {
            return ShellTransactionInput {
                wrapper: String::new(),
                receiver_payload: String::new(),
                payload: String::new(),
                payload_receiver_acknowledgements: false,
            };
        }
        let function_name = transaction_function_name(self.marker.as_str());
        let command_materialization = posix_command_file_materialization(
            &self.command,
            self.input_sidecar.as_deref(),
            self.child_launch
                .as_ref()
                .map(|launch| launch.artifacts.as_slice())
                .unwrap_or_default(),
            self.marker.as_str(),
            "command printf '\\033]133;C;mez_marker=%s;mez_turn=%s;mez_agent=%s;mez_pane=%s\\033\\\\' \"$MEZ_MARKER_TOKEN\" \"$MEZ_TURN\" \"$MEZ_AGENT\" \"$MEZ_PANE\"",
            self.payload_receiver_acknowledgements,
        );
        let shell_invocation = self.child_launch.as_ref().map_or_else(
            || {
                posix_shell_script_invocation_words(
                    &self.shell_path,
                    classification,
                    "\"$MEZ_COMMAND_FILE\"",
                )
            },
            posix_typed_child_launch_words,
        );
        let child_env = if self.child_launch.is_some() {
            String::new()
        } else {
            posix_noninteractive_agent_env_command_words()
        };
        let child_invocation = posix_child_command_invocation_lines(
            self.output_transport,
            self.output_max_raw_bytes,
            &child_env,
            &shell_invocation,
            self.child_launch
                .as_ref()
                .and_then(|launch| launch.status_fd),
            self.child_launch
                .as_ref()
                .is_some_and(|launch| launch.inherited_terminal),
        );
        let (history_start, history_restore, history_marker_finish) =
            if classification == ShellClassification::Bash && self.bash_receiver_token.is_some() {
                (
                    posix_shell_state_suppression_start().to_string(),
                    String::new(),
                    posix_shell_state_marker_finish_prefix().to_string(),
                )
            } else if classification == ShellClassification::Zsh && self.zsh_history_token.is_some()
            {
                (
                    zsh_shell_history_suppression_start().to_string(),
                    String::new(),
                    zsh_shell_history_marker_finish_prefix(self.zsh_history_token.as_ref()),
                )
            } else {
                (
                    posix_shell_history_suppression_start_for_classification(classification),
                    posix_shell_history_file_restore().to_string(),
                    posix_shell_history_marker_finish_prefix_for_classification(classification),
                )
            };
        let sidecar_frame_cleanup = if self.input_sidecar.is_some() {
            "if [ -n \"$MEZ_SIDECAR_FRAME\" ]; then command rm -f -- \"$MEZ_SIDECAR_FRAME\" >/dev/null 2>&1 || :; fi\n\\
unset MEZ_SIDECAR_FRAME MEZ_SIDECAR_FRAME_SEQUENCE MEZ_SIDECAR_FRAME_LENGTH MEZ_SIDECAR_FRAME_DIGEST MEZ_SIDECAR_FRAME_COUNT MEZ_SIDECAR_FRAME_ACTUAL MEZ_SIDECAR_SHA256\n"
        } else {
            ""
        };
        let wrapper = format!(
            "{history_start}\
{function_name}() {{\n\
MEZ_MARKER_TOKEN={marker}\n\
MEZ_TURN={turn}\n\
MEZ_AGENT={agent}\n\
MEZ_PANE={pane}\n\
{command_file_lines}\
{child_invocation}\
command rm -f -- \"$MEZ_COMMAND_FILE\" \"$MEZ_COMMAND_B64\" \"$MEZ_SIDECAR_DATA\" >/dev/null 2>&1 || :\n\
{sidecar_frame_cleanup}\
if [ -n \"$MEZ_ARTIFACT_DIR\" ]; then command rm -rf -- \"$MEZ_ARTIFACT_DIR\" >/dev/null 2>&1 || :; fi\n\
if [ -n \"$MEZ_OUTPUT_FILE\" ]; then command rm -f -- \"$MEZ_OUTPUT_FILE\" >/dev/null 2>&1 || :; fi\n\
if [ -n \"$MEZ_STATUS_FILE\" ]; then command rm -f -- \"$MEZ_STATUS_FILE\" >/dev/null 2>&1 || :; fi\n\
unset MEZ_COMMAND_FILE MEZ_COMMAND_B64 MEZ_SIDECAR_DATA MEZ_ARTIFACT_DIR MEZ_COMMAND_END MEZ_COMMAND_LINE MEZ_COMMAND_SEEN_END MEZ_OUTPUT_FILE MEZ_STATUS_FILE MEZ_STTY_STATE MEZ_WRITE_STATUS\n\
unset -f {function_name} 2>/dev/null || :\n\
{history_restore}\
{history_marker_finish}command printf '\\033]133;D;%s;mez_marker=%s;mez_turn=%s;mez_agent=%s;mez_pane=%s\\033\\\\' \
\"$MEZ_STATUS\" \"$MEZ_MARKER_TOKEN\" \"$MEZ_TURN\" \"$MEZ_AGENT\" \"$MEZ_PANE\"; \
unset MEZ_MARKER_TOKEN MEZ_TURN MEZ_AGENT MEZ_PANE MEZ_STATUS; {errexit_restore}\n\
}}\n\
{function_name}\n",
            history_start = history_start,
            history_restore = history_restore,
            history_marker_finish = history_marker_finish,
            sidecar_frame_cleanup = sidecar_frame_cleanup,
            errexit_restore = posix_shell_errexit_restore_suffix(),
            function_name = function_name,
            marker = shell_quote(self.marker.as_str()),
            turn = shell_quote(&self.turn_id),
            agent = shell_quote(&self.agent_id),
            pane = shell_quote(&self.pane_id),
            command_file_lines = command_materialization.setup,
            child_invocation = child_invocation,
        );
        let bash_transport = bash_private_receiver_transport(
            &wrapper,
            classification,
            self.bash_receiver_token.as_ref(),
            self.marker.as_str(),
            None,
        );
        ShellTransactionInput {
            wrapper: bash_transport.as_ref().map_or_else(
                || {
                    posix_shell_wrapper_transport(
                        &wrapper,
                        classification,
                        self.zsh_history_token.as_ref(),
                    )
                },
                |transport| transport.trigger.clone(),
            ),
            receiver_payload: bash_transport
                .map(|transport| transport.payload)
                .unwrap_or_default(),
            payload: command_materialization.payload,
            payload_receiver_acknowledgements: self.payload_receiver_acknowledgements,
        }
    }

    /// Combines isolated staged input for the resolved shell dialect.
    pub fn render_for_classification(&self, classification: ShellClassification) -> String {
        self.render_for_classification_input(classification)
            .combined()
    }

    /// Renders the non-stateful shell transaction as a wrapper plus streamed
    /// payload.
    pub fn render_for_classification_input(
        &self,
        classification: ShellClassification,
    ) -> ShellTransactionInput {
        if classification == ShellClassification::Fish {
            self.render_fish_input()
        } else {
            self.render_posix_input_for_classification(classification)
        }
    }

    /// Renders a stateful shell command wrapper that executes directly in the
    /// interactive pane shell, preserving `cd`, environment, aliases, and
    /// shell options after the command completes.
    ///
    /// Stateful actions disclose in structured content that they may change
    /// the pane shell state. This wrapper skips the child-shell isolation so
    /// mutations persist in the interactive shell context.
    pub fn render_stateful(&self) -> String {
        self.render_stateful_for_classification_input(ShellClassification::PosixSh)
            .combined()
    }

    /// Renders one stateful POSIX-compatible transaction for a known shell.
    ///
    /// Zsh uses the bounded wrapper transport so its authenticated history
    /// record can push a private frame before any generated source is read.
    fn render_posix_stateful_for_classification(
        &self,
        classification: ShellClassification,
    ) -> String {
        let function_name = transaction_function_name(self.marker.as_str());
        let zsh_history_isolation =
            classification == ShellClassification::Zsh && self.zsh_history_token.is_some();
        let (history_start, history_restore, history_marker_finish) =
            if classification == ShellClassification::Bash && self.bash_receiver_token.is_some() {
                (
                    posix_shell_state_suppression_start().to_string(),
                    String::new(),
                    posix_shell_state_marker_finish_prefix().to_string(),
                )
            } else if zsh_history_isolation {
                (
                    zsh_shell_history_suppression_start().to_string(),
                    String::new(),
                    zsh_shell_history_marker_finish_prefix(self.zsh_history_token.as_ref()),
                )
            } else {
                (
                    posix_shell_history_suppression_start_for_classification(classification),
                    posix_shell_history_file_restore().to_string(),
                    posix_shell_history_marker_finish_prefix_for_classification(classification),
                )
            };
        let source = format!(
            "{history_start}\
{function_name}() {{\n\
command printf '\\033]133;C;mez_marker=%s;mez_turn=%s;mez_agent=%s;mez_pane=%s\\033\\\\' \
{marker} {turn} {agent} {pane}\n\
{{\n\
{command}\n\
}}\n\
MEZ_STATUS=$?\n\
unset -f {function_name} 2>/dev/null || :\n\
{history_restore}\
{history_marker_finish}command printf '\\033]133;D;%s;mez_marker=%s;mez_turn=%s;mez_agent=%s;mez_pane=%s\\033\\\\' \
\"$MEZ_STATUS\" {marker} {turn} {agent} {pane}; unset MEZ_STATUS; {errexit_restore}\n\
}}\n\
{function_name}\n",
            history_start = history_start,
            history_restore = history_restore,
            history_marker_finish = history_marker_finish,
            errexit_restore = posix_shell_errexit_restore_suffix(),
            function_name = function_name,
            marker = shell_quote(self.marker.as_str()),
            turn = shell_quote(&self.turn_id),
            agent = shell_quote(&self.agent_id),
            pane = shell_quote(&self.pane_id),
            command = self.command,
        );
        if zsh_history_isolation {
            posix_shell_wrapper_transport(&source, classification, self.zsh_history_token.as_ref())
        } else {
            source
        }
    }

    /// Combines stateful staged input for the resolved shell dialect.
    pub fn render_stateful_for_classification(
        &self,
        classification: ShellClassification,
    ) -> String {
        self.render_stateful_for_classification_input(classification)
            .combined()
    }

    /// Renders stateful shell input with a separately gated Bash receiver stage.
    pub fn render_stateful_for_classification_input(
        &self,
        classification: ShellClassification,
    ) -> ShellTransactionInput {
        if classification == ShellClassification::Bash && self.bash_receiver_token.is_none() {
            return ShellTransactionInput {
                wrapper: String::new(),
                receiver_payload: String::new(),
                payload: String::new(),
                payload_receiver_acknowledgements: false,
            };
        }
        let source = if classification == ShellClassification::Fish {
            self.render_fish_stateful()
        } else {
            self.render_posix_stateful_for_classification(classification)
        };
        let bash_transport = bash_private_receiver_transport(
            &source,
            classification,
            self.bash_receiver_token.as_ref(),
            self.marker.as_str(),
            None,
        );
        ShellTransactionInput {
            wrapper: bash_transport
                .as_ref()
                .map_or(source, |transport| transport.trigger.clone()),
            receiver_payload: bash_transport
                .map(|transport| transport.payload)
                .unwrap_or_default(),
            payload: String::new(),
            payload_receiver_acknowledgements: self.payload_receiver_acknowledgements,
        }
    }

    /// Renders a Fish shell transaction wrapper with fish-native block syntax
    /// (`begin`/`end`), `set` variable assignment, and `$status` for exit code
    /// capture. This preserves the same OSC 133 marker convention used by the
    /// POSIX wrapper.
    pub fn render_fish(&self) -> String {
        self.render_fish_input().combined()
    }

    /// Renders a Fish shell transaction as a wrapper plus streamed payload.
    pub fn render_fish_input(&self) -> ShellTransactionInput {
        let start_marker_line = "printf '\\033]133;C;mez_marker=%s;mez_turn=%s;mez_agent=%s;mez_pane=%s\\033\\\\' $MEZ_MARKER_TOKEN $MEZ_TURN $MEZ_AGENT $MEZ_PANE";
        let typed_child_uses_command_file = self.child_launch.as_ref().is_some_and(|launch| {
            launch
                .arguments
                .iter()
                .any(|argument| matches!(argument, ShellChildArgument::MaterializedCommandFile))
        });
        let command_materialization = if self.child_launch.is_some()
            && !typed_child_uses_command_file
            && self.input_sidecar.is_none()
            && self
                .child_launch
                .as_ref()
                .is_none_or(|launch| launch.artifacts.is_empty())
        {
            CommandMaterialization {
                setup: format!(
                    "set -l MEZ_COMMAND_FILE ''\n\
set -l MEZ_COMMAND_B64 ''\n\
set -l MEZ_SIDECAR_DATA ''\n\
set -l MEZ_STTY_STATE ''\n\
set -l MEZ_WRITE_STATUS 0\n\
{start_marker_line}\n"
                ),
                payload: String::new(),
            }
        } else {
            fish_command_file_materialization(
                &self.command,
                self.input_sidecar.as_deref(),
                self.child_launch
                    .as_ref()
                    .map(|launch| launch.artifacts.as_slice())
                    .unwrap_or_default(),
                self.marker.as_str(),
                start_marker_line,
                "printf '\\033]133;R;mez_payload_receiver=ready;mez_marker=%s;mez_turn=%s;mez_agent=%s;mez_pane=%s\\033\\\\' $MEZ_MARKER_TOKEN $MEZ_TURN $MEZ_AGENT $MEZ_PANE",
                self.payload_receiver_acknowledgements,
            )
        };
        let shell_invocation = self.child_launch.as_ref().map_or_else(
            || {
                fish_shell_script_invocation_words(
                    &self.shell_path,
                    ShellClassification::Fish,
                    "\"$MEZ_COMMAND_FILE\"",
                )
            },
            fish_typed_child_launch_words,
        );
        let child_env = if self.child_launch.is_some() {
            String::new()
        } else {
            fish_noninteractive_agent_env_words()
        };
        let child_invocation = fish_child_command_invocation_lines(
            self.output_transport,
            self.output_max_raw_bytes,
            &child_env,
            &shell_invocation,
            self.child_launch
                .as_ref()
                .and_then(|launch| launch.status_fd),
            self.child_launch
                .as_ref()
                .is_some_and(|launch| launch.inherited_terminal),
        );
        let child_output_separator = if self.child_launch.is_some() {
            ""
        } else {
            "printf '\\n'\n"
        };
        let sidecar_frame_cleanup = if self.input_sidecar.is_some() {
            "if test -n \"$MEZ_SIDECAR_FRAME\"; command rm -f -- \"$MEZ_SIDECAR_FRAME\" >/dev/null 2>&1; or true; end\n\\
set -e MEZ_SIDECAR_FRAME MEZ_SIDECAR_FRAME_SEQUENCE MEZ_SIDECAR_FRAME_LENGTH MEZ_SIDECAR_FRAME_DIGEST MEZ_SIDECAR_FRAME_COUNT MEZ_SIDECAR_FRAME_ACTUAL MEZ_SIDECAR_SHA256\n"
        } else {
            ""
        };
        let wrapper = format!(
            "{history_start}\
begin\n\
set -l MEZ_MARKER_TOKEN {marker}\n\
set -l MEZ_TURN {turn}\n\
set -l MEZ_AGENT {agent}\n\
set -l MEZ_PANE {pane}\n\
{command_file_lines}\
set -l MEZ_STATUS 0\n\
{child_output_separator}\
{child_invocation}\
if test -n \"$MEZ_COMMAND_FILE\"; command rm -f -- \"$MEZ_COMMAND_FILE\" >/dev/null 2>&1; or true; end\n\
if test -n \"$MEZ_COMMAND_B64\"; command rm -f -- \"$MEZ_COMMAND_B64\" >/dev/null 2>&1; or true; end\n\
if test -n \"$MEZ_SIDECAR_DATA\"; command rm -f -- \"$MEZ_SIDECAR_DATA\" >/dev/null 2>&1; or true; end\n\
{sidecar_frame_cleanup}\
if test -n \"$MEZ_ARTIFACT_DIR\"; command rm -rf -- \"$MEZ_ARTIFACT_DIR\" >/dev/null 2>&1; or true; end\n\
if test -n \"$MEZ_OUTPUT_FILE\"; command rm -f -- \"$MEZ_OUTPUT_FILE\" >/dev/null 2>&1; or true; end\n\
if test -n \"$MEZ_STATUS_FILE\"; command rm -f -- \"$MEZ_STATUS_FILE\" >/dev/null 2>&1; or true; end\n\
set -e MEZ_COMMAND_FILE MEZ_COMMAND_B64 MEZ_SIDECAR_DATA MEZ_ARTIFACT_DIR MEZ_COMMAND_END MEZ_COMMAND_LINE MEZ_COMMAND_SEEN_END MEZ_OUTPUT_FILE MEZ_STATUS_FILE MEZ_STTY_STATE MEZ_WRITE_STATUS\n\
{history_restore}\
printf '\\033]133;D;%s;mez_marker=%s;mez_turn=%s;mez_agent=%s;mez_pane=%s\\033\\\\' \
$MEZ_STATUS $MEZ_MARKER_TOKEN $MEZ_TURN $MEZ_AGENT $MEZ_PANE\n\
end\n",
            history_start = fish_shell_history_suppression_start(),
            history_restore = fish_shell_history_restore(),
            sidecar_frame_cleanup = sidecar_frame_cleanup,
            marker = fish_quote(self.marker.as_str()),
            turn = fish_quote(&self.turn_id),
            agent = fish_quote(&self.agent_id),
            pane = fish_quote(&self.pane_id),
            command_file_lines = command_materialization.setup,
            child_output_separator = child_output_separator,
            child_invocation = child_invocation,
        );
        ShellTransactionInput {
            wrapper: fish_shell_wrapper_transport(&wrapper, self.marker.as_str()),
            receiver_payload: String::new(),
            payload: command_materialization.payload,
            payload_receiver_acknowledgements: self.payload_receiver_acknowledgements,
        }
    }

    /// Renders a stateful Fish shell command wrapper that executes directly in
    /// the interactive pane shell using fish-native `begin`/`end` block syntax
    /// and `$status` for exit capture. Mutations persist in the interactive
    /// context.
    pub fn render_fish_stateful(&self) -> String {
        format!(
            "{history_start}\
begin\n\
set -l MEZ_MARKER_TOKEN {marker}\n\
set -l MEZ_TURN {turn}\n\
set -l MEZ_AGENT {agent}\n\
set -l MEZ_PANE {pane}\n\
printf '\\033]133;C;mez_marker=%s;mez_turn=%s;mez_agent=%s;mez_pane=%s\\033\\\\' \
$MEZ_MARKER_TOKEN $MEZ_TURN $MEZ_AGENT $MEZ_PANE\n\
begin\n\
eval {command}\n\
end\n\
set -l MEZ_STATUS $status\n\
{history_restore}\
printf '\\033]133;D;%s;mez_marker=%s;mez_turn=%s;mez_agent=%s;mez_pane=%s\\033\\\\' \
$MEZ_STATUS $MEZ_MARKER_TOKEN $MEZ_TURN $MEZ_AGENT $MEZ_PANE\n\
end\n",
            history_start = fish_shell_history_suppression_start(),
            history_restore = fish_shell_history_restore(),
            marker = fish_quote(self.marker.as_str()),
            turn = fish_quote(&self.turn_id),
            agent = fish_quote(&self.agent_id),
            pane = fish_quote(&self.pane_id),
            command = fish_quote(&self.command),
        )
    }
}
