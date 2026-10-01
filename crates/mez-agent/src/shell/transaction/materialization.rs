//! Command-file receiver setup for POSIX and Fish transaction renderers.
//!
//! Both dialects consume the canonical payload encoder and one validated artifact
//! contract. Setup takes stdin ownership before data arrives, checks sidecar frame
//! identity, length and digest, and restores terminal state before execution.
//! These helpers construct source only; adapters own admission and pane I/O.

use super::payload::{command_payload_end_marker, command_payload_lines};
use super::{SHELL_TRANSACTION_SIDECAR_FRAME_BYTES, ShellLaunchArtifact, fish_quote, shell_quote};

/// Shell-source setup plus data payload used to materialize one command file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CommandMaterialization {
    /// Shell code that starts the payload receiver and decodes the command.
    pub(super) setup: String,
    /// Base64 command payload lines consumed by the setup receiver.
    pub(super) payload: String,
}

/// Renders POSIX shell lines that materialize a transaction command file.
///
/// The generated code avoids heredocs entirely. It writes standard-base64
/// chunks to a temporary sidecar file through a receiver that starts before the
/// payload bytes are sent. This keeps large action payloads out of the
/// persistent pane shell's parsed source and lets the shell drain PTY input as
/// data instead of waiting for an entire generated wrapper to arrive.
pub(super) fn posix_command_file_materialization(
    command: &str,
    input_sidecar: Option<&str>,
    artifacts: &[ShellLaunchArtifact],
    marker: &str,
    start_marker_line: &str,
    acknowledge_payload_records: bool,
) -> CommandMaterialization {
    let end_marker = command_payload_end_marker(marker);
    let acknowledge = if acknowledge_payload_records {
        "command printf '\\036'"
    } else {
        ":"
    };
    let receive_record = if input_sidecar.is_some() {
        format!(
            "case \"$MEZ_COMMAND_LINE\" in C\\ *) if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then printf '%s\\n' \"${{MEZ_COMMAND_LINE#C }}\" >> \"$MEZ_COMMAND_B64\" || MEZ_WRITE_STATUS=$?; fi; {acknowledge} ;; S1B\\ *) if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then MEZ_SIDECAR_FRAME_HEADER=${{MEZ_COMMAND_LINE#S1B }}; MEZ_SIDECAR_FRAME_HEADER_SEQUENCE=${{MEZ_SIDECAR_FRAME_HEADER%% *}}; MEZ_SIDECAR_FRAME_HEADER=${{MEZ_SIDECAR_FRAME_HEADER#* }}; MEZ_SIDECAR_FRAME_LENGTH=${{MEZ_SIDECAR_FRAME_HEADER%% *}}; MEZ_SIDECAR_FRAME_DIGEST=${{MEZ_SIDECAR_FRAME_HEADER#* }}; case \"$MEZ_SIDECAR_FRAME_LENGTH\" in ''|*[!0-9]*) MEZ_WRITE_STATUS=1;; esac; case \"$MEZ_SIDECAR_FRAME_DIGEST\" in *[!0-9a-f]*|???????????????????????????????????????????????????????????????|?????????????????????????????????????????????????????????????????*) MEZ_WRITE_STATUS=1;; esac; if [ \"$MEZ_SIDECAR_FRAME_OPEN\" != 0 ] || [ \"$MEZ_SIDECAR_FRAME_HEADER_SEQUENCE\" != \"$MEZ_SIDECAR_FRAME_SEQUENCE\" ] || [ \"$MEZ_SIDECAR_FRAME_LENGTH\" -gt {frame_bytes} ] 2>/dev/null || [ \"$MEZ_SIDECAR_FRAME_DIGEST\" != \"${{MEZ_SIDECAR_FRAME_DIGEST%% *}}\" ]; then MEZ_WRITE_STATUS=1; fi; if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then MEZ_SIDECAR_FRAME_OPEN=1; : > \"$MEZ_SIDECAR_FRAME\" || MEZ_WRITE_STATUS=$?; fi; fi ;; S1D\\ *) if [ \"$MEZ_WRITE_STATUS\" -eq 0 ] && [ \"$MEZ_SIDECAR_FRAME_OPEN\" = 1 ]; then printf '%s\\n' \"${{MEZ_COMMAND_LINE#S1D }}\" >> \"$MEZ_SIDECAR_FRAME\" || MEZ_WRITE_STATUS=$?; else MEZ_WRITE_STATUS=1; fi ;; S1E\\ *) if [ \"$MEZ_WRITE_STATUS\" -eq 0 ] && [ \"$MEZ_SIDECAR_FRAME_OPEN\" = 1 ]; then MEZ_SIDECAR_FRAME_END_SEQUENCE=${{MEZ_COMMAND_LINE#S1E }}; MEZ_SIDECAR_FRAME_COUNT=$(wc -c < \"$MEZ_SIDECAR_FRAME\" | tr -d '[:space:]') || MEZ_WRITE_STATUS=$?; if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then if [ \"$MEZ_SIDECAR_SHA256\" = sha256sum ]; then MEZ_SIDECAR_FRAME_ACTUAL=$(sha256sum -- \"$MEZ_SIDECAR_FRAME\"); else MEZ_SIDECAR_FRAME_ACTUAL=$(shasum -a 256 -- \"$MEZ_SIDECAR_FRAME\"); fi; MEZ_SIDECAR_FRAME_ACTUAL=${{MEZ_SIDECAR_FRAME_ACTUAL%%[[:space:]]*}}; fi; if [ \"$MEZ_SIDECAR_FRAME_END_SEQUENCE\" != \"$MEZ_SIDECAR_FRAME_SEQUENCE\" ] || [ \"$MEZ_SIDECAR_FRAME_END_SEQUENCE\" != \"${{MEZ_SIDECAR_FRAME_END_SEQUENCE%% *}}\" ] || [ \"$MEZ_SIDECAR_FRAME_COUNT\" != \"$MEZ_SIDECAR_FRAME_LENGTH\" ] || [ \"$MEZ_SIDECAR_FRAME_ACTUAL\" != \"$MEZ_SIDECAR_FRAME_DIGEST\" ]; then MEZ_WRITE_STATUS=1; else sed 's/^/# __MEZ_INPUT_SIDECAR_V1__ /' \"$MEZ_SIDECAR_FRAME\" >> \"$MEZ_SIDECAR_DATA\" || MEZ_WRITE_STATUS=$?; MEZ_SIDECAR_FRAME_SEQUENCE=$((MEZ_SIDECAR_FRAME_SEQUENCE + 1)); MEZ_SIDECAR_FRAME_OPEN=0; fi; else MEZ_WRITE_STATUS=1; fi; {acknowledge} ;; *) MEZ_WRITE_STATUS=1; {acknowledge} ;; esac",
            frame_bytes = SHELL_TRANSACTION_SIDECAR_FRAME_BYTES,
        )
    } else {
        let artifact_cases = artifacts
            .iter()
            .enumerate()
            .map(|(index, _)| {
                format!(
                    "A{index}\\ *) if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then printf '%s\\n' \"${{MEZ_COMMAND_LINE#A{index} }}\" >> \"$MEZ_ARTIFACT_DIR/{index}.b64\" || MEZ_WRITE_STATUS=$?; fi ;; "
                )
            })
            .collect::<String>();
        format!(
            "if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then case \"$MEZ_COMMAND_LINE\" in C\\ *) printf '%s\\n' \"${{MEZ_COMMAND_LINE#C }}\" >> \"$MEZ_COMMAND_B64\" || MEZ_WRITE_STATUS=$? ;; {artifact_cases}*) MEZ_WRITE_STATUS=1 ;; esac; fi; {acknowledge}"
        )
    };
    let artifact_cases = artifacts
        .iter()
        .enumerate()
        .map(|(index, _)| {
            format!(
                "A{index}\\ *) if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then printf '%s\\n' \"${{MEZ_COMMAND_LINE#A{index} }}\" >> \"$MEZ_ARTIFACT_DIR/{index}.b64\" || MEZ_WRITE_STATUS=$?; fi; {acknowledge} ;; "
            )
        })
        .collect::<String>();
    let catch_all = format!("*) MEZ_WRITE_STATUS=1; {acknowledge} ;;");
    let receive_record = if artifacts.is_empty() {
        receive_record
    } else {
        receive_record.replacen(&catch_all, &format!("{artifact_cases}{catch_all}"), 1)
    };
    let terminal_mode = if input_sidecar.is_some() {
        "stty -icanon min 1 time 0 -echo 2>/dev/null || :"
    } else {
        "stty -echo 2>/dev/null || :"
    };
    let mut lines = vec![
        "MEZ_COMMAND_FILE=$(mktemp) || MEZ_COMMAND_FILE=".to_string(),
        "MEZ_COMMAND_B64=".to_string(),
        "MEZ_SIDECAR_DATA=".to_string(),
        "MEZ_SIDECAR_FRAME=".to_string(),
        "MEZ_SIDECAR_FRAME_SEQUENCE=0".to_string(),
        "MEZ_SIDECAR_FRAME_OPEN=0".to_string(),
        "MEZ_ARTIFACT_DIR=".to_string(),
        format!("MEZ_COMMAND_END={}", shell_quote(&end_marker)),
        "MEZ_COMMAND_SEEN_END=0".to_string(),
        "MEZ_STTY_STATE=".to_string(),
        "MEZ_WRITE_STATUS=0".to_string(),
        "if [ -n \"$MEZ_COMMAND_FILE\" ]; then".to_string(),
        "command -v base64 >/dev/null 2>&1 || { printf '%s\\n' 'base64 is required for Mezzanine shell transaction wrappers' >&2; MEZ_WRITE_STATUS=127; }".to_string(),
        "if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then MEZ_COMMAND_B64=$(mktemp) || MEZ_WRITE_STATUS=1; fi".to_string(),
        "if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then : > \"$MEZ_COMMAND_B64\" || MEZ_WRITE_STATUS=$?; fi".to_string(),
    ];
    if !artifacts.is_empty() {
        lines.extend([
            "if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then MEZ_ARTIFACT_DIR=$(mktemp -d) || MEZ_WRITE_STATUS=1; fi".to_string(),
            "if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then chmod 700 \"$MEZ_ARTIFACT_DIR\" || MEZ_WRITE_STATUS=$?; fi".to_string(),
            "if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then MEZ_ARTIFACT_DIR=$(CDPATH= cd -P -- \"$MEZ_ARTIFACT_DIR\" 2>/dev/null && pwd -P) || MEZ_WRITE_STATUS=$?; fi".to_string(),
        ]);
        for (index, _) in artifacts.iter().enumerate() {
            lines.push(format!(
                "if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then : > \"$MEZ_ARTIFACT_DIR/{index}.b64\" || MEZ_WRITE_STATUS=$?; fi"
            ));
        }
    }
    if input_sidecar.is_some() {
        lines.push(
            "if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then MEZ_SIDECAR_DATA=$(mktemp) || MEZ_WRITE_STATUS=1; fi"
                .to_string(),
        );
        lines.push(
            "if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then MEZ_SIDECAR_FRAME=$(mktemp) || MEZ_WRITE_STATUS=1; fi"
                .to_string(),
        );
        lines.push(
            "if command -v sha256sum >/dev/null 2>&1; then MEZ_SIDECAR_SHA256=sha256sum; elif command -v shasum >/dev/null 2>&1; then MEZ_SIDECAR_SHA256=shasum; else MEZ_WRITE_STATUS=127; fi"
                .to_string(),
        );
    }
    lines.extend([
        "MEZ_STTY_STATE=$(stty -g 2>/dev/null) || MEZ_STTY_STATE=".to_string(),
        format!("if [ -n \"$MEZ_STTY_STATE\" ]; then {terminal_mode}; fi"),
        start_marker_line.to_string(),
        "while IFS= read -r MEZ_COMMAND_LINE; do".to_string(),
        format!("if [ \"$MEZ_COMMAND_LINE\" = \"$MEZ_COMMAND_END\" ]; then if [ \"${{MEZ_SIDECAR_FRAME_OPEN:-0}}\" != 0 ]; then MEZ_WRITE_STATUS=1; fi; MEZ_COMMAND_SEEN_END=1; {acknowledge}; break; fi"),
        receive_record,
        "done".to_string(),
        "if [ \"$MEZ_WRITE_STATUS\" -eq 0 ] && [ \"$MEZ_COMMAND_SEEN_END\" != 1 ]; then printf '%s\\n' 'Mezzanine shell transaction command payload ended before sentinel' >&2; MEZ_WRITE_STATUS=1; fi".to_string(),
        "if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then if base64 -d < \"$MEZ_COMMAND_B64\" > \"$MEZ_COMMAND_FILE\" 2>/dev/null; then MEZ_WRITE_STATUS=0; else base64 -D < \"$MEZ_COMMAND_B64\" > \"$MEZ_COMMAND_FILE\"; MEZ_WRITE_STATUS=$?; fi; fi".to_string(),
        "if [ \"$MEZ_WRITE_STATUS\" -eq 0 ] && [ -n \"$MEZ_SIDECAR_DATA\" ]; then cat \"$MEZ_SIDECAR_DATA\" >> \"$MEZ_COMMAND_FILE\" || MEZ_WRITE_STATUS=$?; fi".to_string(),
    ]);
    for (index, artifact) in artifacts.iter().enumerate() {
        lines.push(format!(
            "if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then if base64 -d < \"$MEZ_ARTIFACT_DIR/{index}.b64\" > \"$MEZ_ARTIFACT_DIR/{index}\" 2>/dev/null; then MEZ_WRITE_STATUS=0; else base64 -D < \"$MEZ_ARTIFACT_DIR/{index}.b64\" > \"$MEZ_ARTIFACT_DIR/{index}\"; MEZ_WRITE_STATUS=$?; fi; fi"
        ));
        lines.push(format!(
            "if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then chmod {:o} \"$MEZ_ARTIFACT_DIR/{index}\" || MEZ_WRITE_STATUS=$?; fi",
            artifact.mode
        ));
        lines.push(format!(
            "command rm -f -- \"$MEZ_ARTIFACT_DIR/{index}.b64\" >/dev/null 2>&1 || :"
        ));
    }
    lines.extend([
        "else".to_string(),
        "MEZ_WRITE_STATUS=1".to_string(),
        "fi".to_string(),
    ]);
    lines.push(
        "if [ -n \"$MEZ_STTY_STATE\" ]; then stty \"$MEZ_STTY_STATE\" 2>/dev/null || :; MEZ_STTY_STATE=; fi"
            .to_string(),
    );
    CommandMaterialization {
        setup: lines.join("\n") + "\n",
        payload: command_payload_lines(command, &end_marker, input_sidecar, artifacts),
    }
}

/// Renders Fish syntax that writes a shell transaction command through short
/// base64 chunks into a temporary script file.
///
/// Fish wrappers cannot safely embed model-authored or runtime-generated
/// scripts as one large `-c` argument. Materializing the script keeps payload
/// bytes inert until the configured Fish shell reads them from a file.
pub(super) fn fish_command_file_materialization(
    command: &str,
    input_sidecar: Option<&str>,
    artifacts: &[ShellLaunchArtifact],
    marker: &str,
    start_marker_line: &str,
    receiver_ready_marker_line: &str,
    acknowledge_payload_records: bool,
) -> CommandMaterialization {
    let end_marker = command_payload_end_marker(marker);
    let acknowledge = if acknowledge_payload_records {
        "printf '\\036'"
    } else {
        "true"
    };
    let posix_payload_reader = (acknowledge_payload_records
        && input_sidecar.is_none()
        && artifacts.is_empty())
    .then(|| {
        let source = "end=$1; output=$2; receive_status=$3; seen_end=0; while IFS= read -r record; do if [ \"$record\" = \"$end\" ]; then seen_end=1; printf '\\036'; break; fi; case \"$record\" in 'C '*) if [ \"$receive_status\" -eq 0 ]; then printf '%s\\n' \"${record#C }\" >>\"$output\" || receive_status=$?; fi ;; *) receive_status=1 ;; esac; printf '\\036'; done; [ \"$seen_end\" -eq 1 ] || receive_status=1; exit \"$receive_status\"";
        format!(
            "command /bin/sh -c {} sh \"$MEZ_COMMAND_END\" \"$MEZ_COMMAND_B64\" \"$MEZ_WRITE_STATUS\"",
            fish_quote(source)
        )
    });
    let acknowledge_command_record = format!(
        "string replace -r '^C ' '' -- \"$MEZ_COMMAND_LINE\" >> \"$MEZ_COMMAND_B64\"; or set MEZ_WRITE_STATUS $status; {acknowledge}"
    );
    let mut lines = vec![
        "set -l MEZ_COMMAND_FILE (mktemp); or set -l MEZ_COMMAND_FILE ''".to_string(),
        "set -l MEZ_COMMAND_B64 ''".to_string(),
        "set -l MEZ_SIDECAR_DATA ''".to_string(),
        "set -l MEZ_ARTIFACT_DIR ''".to_string(),
        format!("set -l MEZ_COMMAND_END {}", fish_quote(&end_marker)),
        "set -l MEZ_COMMAND_SEEN_END 0".to_string(),
        "set -l MEZ_STTY_STATE ''".to_string(),
        "set -l MEZ_WRITE_STATUS 0".to_string(),
        "if test -n \"$MEZ_COMMAND_FILE\"".to_string(),
        "command -q base64; or begin; printf '%s\\n' 'base64 is required for Mezzanine shell transaction wrappers' >&2; set MEZ_WRITE_STATUS 127; end".to_string(),
        "if test \"$MEZ_WRITE_STATUS\" -eq 0; set MEZ_COMMAND_B64 (mktemp); or set MEZ_WRITE_STATUS 1; end".to_string(),
        "if test \"$MEZ_WRITE_STATUS\" -eq 0; : > \"$MEZ_COMMAND_B64\"; or set MEZ_WRITE_STATUS $status; end".to_string(),
    ];
    if !artifacts.is_empty() {
        lines.extend([
            "if test \"$MEZ_WRITE_STATUS\" -eq 0; set MEZ_ARTIFACT_DIR (mktemp -d); or set MEZ_WRITE_STATUS 1; end".to_string(),
            "if test \"$MEZ_WRITE_STATUS\" -eq 0; chmod 700 \"$MEZ_ARTIFACT_DIR\"; or set MEZ_WRITE_STATUS $status; end".to_string(),
            "if test \"$MEZ_WRITE_STATUS\" -eq 0; set MEZ_ARTIFACT_DIR (cd \"$MEZ_ARTIFACT_DIR\" 2>/dev/null; and pwd -P); or set MEZ_WRITE_STATUS $status; end".to_string(),
        ]);
        for (index, _) in artifacts.iter().enumerate() {
            lines.push(format!(
                "if test \"$MEZ_WRITE_STATUS\" -eq 0; : > \"$MEZ_ARTIFACT_DIR/{index}.b64\"; or set MEZ_WRITE_STATUS $status; end"
            ));
        }
    }
    if input_sidecar.is_some() {
        lines.push("set -l MEZ_SIDECAR_FRAME ''".to_string());
        lines.push("set -l MEZ_SIDECAR_FRAME_SEQUENCE 0".to_string());
        lines.push("set -l MEZ_SIDECAR_FRAME_OPEN 0".to_string());
        lines.push(
            "if test \"$MEZ_WRITE_STATUS\" -eq 0; set MEZ_SIDECAR_DATA (mktemp); or set MEZ_WRITE_STATUS 1; end"
                .to_string(),
        );
        lines.push(
            "if test \"$MEZ_WRITE_STATUS\" -eq 0; set MEZ_SIDECAR_FRAME (mktemp); or set MEZ_WRITE_STATUS 1; end"
                .to_string(),
        );
        lines.push(
            "if command -q sha256sum; set MEZ_SIDECAR_SHA256 sha256sum; else if command -q shasum; set MEZ_SIDECAR_SHA256 shasum; else; set MEZ_WRITE_STATUS 127; end"
                .to_string(),
        );
    }
    let terminal_mode = if input_sidecar.is_some() {
        "stty -icanon min 1 time 0 -echo 2>/dev/null; or true"
    } else {
        "stty -echo 2>/dev/null; or true"
    };
    let open_frame_check = input_sidecar
        .is_some()
        .then_some("if test \"$MEZ_SIDECAR_FRAME_OPEN\" -ne 0; set MEZ_WRITE_STATUS 1; end");
    lines.extend([
        "set MEZ_STTY_STATE (stty -g 2>/dev/null); or set MEZ_STTY_STATE ''".to_string(),
        "if test -n \"$MEZ_STTY_STATE\"".to_string(),
        terminal_mode.to_string(),
        "end".to_string(),
        start_marker_line.to_string(),
        receiver_ready_marker_line.to_string(),
    ]);
    if let Some(posix_payload_reader) = posix_payload_reader {
        lines.extend([
            posix_payload_reader,
            "set MEZ_WRITE_STATUS $status".to_string(),
            "if test \"$MEZ_WRITE_STATUS\" -eq 0".to_string(),
            "set MEZ_COMMAND_SEEN_END 1".to_string(),
            "end".to_string(),
        ]);
    } else {
        lines.extend([
            "while read -l MEZ_COMMAND_LINE".to_string(),
            "if test \"$MEZ_COMMAND_LINE\" = \"$MEZ_COMMAND_END\"".to_string(),
        ]);
        if let Some(open_frame_check) = open_frame_check {
            lines.push(open_frame_check.to_string());
        }
        lines.extend([
            "set MEZ_COMMAND_SEEN_END 1".to_string(),
            acknowledge.to_string(),
            "break".to_string(),
            "end".to_string(),
        ]);
        if input_sidecar.is_some() {
            lines.extend([
                "switch \"$MEZ_COMMAND_LINE\"".to_string(),
                "case 'C *'".to_string(),
                format!("if test \"$MEZ_WRITE_STATUS\" -eq 0; {acknowledge_command_record}; else; {acknowledge}; end"),
                "case 'S1B *'".to_string(),
                "if test \"$MEZ_WRITE_STATUS\" -eq 0; set MEZ_SIDECAR_FRAME_FIELDS (string split ' ' -- \"$MEZ_COMMAND_LINE\"); if test (count $MEZ_SIDECAR_FRAME_FIELDS) -ne 4; or test \"$MEZ_SIDECAR_FRAME_OPEN\" -ne 0; or test \"$MEZ_SIDECAR_FRAME_FIELDS[2]\" != \"$MEZ_SIDECAR_FRAME_SEQUENCE\"; or not string match -rq '^[0-9]+$' -- \"$MEZ_SIDECAR_FRAME_FIELDS[3]\"; or test \"$MEZ_SIDECAR_FRAME_FIELDS[3]\" -gt 32768; or not string match -rq '^[0-9a-f]{64}$' -- \"$MEZ_SIDECAR_FRAME_FIELDS[4]\"; set MEZ_WRITE_STATUS 1; else; set MEZ_SIDECAR_FRAME_LENGTH $MEZ_SIDECAR_FRAME_FIELDS[3]; set MEZ_SIDECAR_FRAME_DIGEST $MEZ_SIDECAR_FRAME_FIELDS[4]; set MEZ_SIDECAR_FRAME_OPEN 1; : > \"$MEZ_SIDECAR_FRAME\"; or set MEZ_WRITE_STATUS $status; end; end".to_string(),
                "case 'S1D *'".to_string(),
                "if test \"$MEZ_WRITE_STATUS\" -eq 0; and test \"$MEZ_SIDECAR_FRAME_OPEN\" -eq 1; string replace -r '^S1D ' '' -- \"$MEZ_COMMAND_LINE\" >> \"$MEZ_SIDECAR_FRAME\"; or set MEZ_WRITE_STATUS $status; else; set MEZ_WRITE_STATUS 1; end".to_string(),
                "case 'S1E *'".to_string(),
                "if test \"$MEZ_WRITE_STATUS\" -eq 0; set MEZ_SIDECAR_FRAME_FIELDS (string split ' ' -- \"$MEZ_COMMAND_LINE\"); set MEZ_SIDECAR_FRAME_COUNT (wc -c < \"$MEZ_SIDECAR_FRAME\" | string trim); if test \"$MEZ_SIDECAR_SHA256\" = sha256sum; set MEZ_SIDECAR_FRAME_ACTUAL (sha256sum -- \"$MEZ_SIDECAR_FRAME\" | string split -f 1 ' '); else; set MEZ_SIDECAR_FRAME_ACTUAL (shasum -a 256 -- \"$MEZ_SIDECAR_FRAME\" | string split -f 1 ' '); end; if test (count $MEZ_SIDECAR_FRAME_FIELDS) -ne 2; or test \"$MEZ_SIDECAR_FRAME_OPEN\" -ne 1; or test \"$MEZ_SIDECAR_FRAME_FIELDS[2]\" != \"$MEZ_SIDECAR_FRAME_SEQUENCE\"; or test \"$MEZ_SIDECAR_FRAME_COUNT\" != \"$MEZ_SIDECAR_FRAME_LENGTH\"; or test \"$MEZ_SIDECAR_FRAME_ACTUAL\" != \"$MEZ_SIDECAR_FRAME_DIGEST\"; set MEZ_WRITE_STATUS 1; else; sed 's/^/# __MEZ_INPUT_SIDECAR_V1__ /' \"$MEZ_SIDECAR_FRAME\" >> \"$MEZ_SIDECAR_DATA\"; or set MEZ_WRITE_STATUS $status; set MEZ_SIDECAR_FRAME_SEQUENCE (math $MEZ_SIDECAR_FRAME_SEQUENCE + 1); set MEZ_SIDECAR_FRAME_OPEN 0; end; end".to_string(),
                acknowledge.to_string(),
            ]);
            for (index, _) in artifacts.iter().enumerate() {
                lines.extend([
                    format!("case 'A{index} *'"),
                    format!("if test \"$MEZ_WRITE_STATUS\" -eq 0; string replace -r '^A{index} ' '' -- \"$MEZ_COMMAND_LINE\" >> \"$MEZ_ARTIFACT_DIR/{index}.b64\"; or set MEZ_WRITE_STATUS $status; end"),
                    acknowledge.to_string(),
                ]);
            }
            lines.extend([
                "case '*'".to_string(),
                "set MEZ_WRITE_STATUS 1".to_string(),
                acknowledge.to_string(),
                "end".to_string(),
            ]);
        } else {
            lines.extend([
                "if test \"$MEZ_WRITE_STATUS\" -eq 0".to_string(),
                "switch \"$MEZ_COMMAND_LINE\"".to_string(),
                "case 'C *'".to_string(),
                "string replace -r '^C ' '' -- \"$MEZ_COMMAND_LINE\" >> \"$MEZ_COMMAND_B64\"; or set MEZ_WRITE_STATUS $status".to_string(),
            ]);
            for (index, _) in artifacts.iter().enumerate() {
                lines.extend([
                    format!("case 'A{index} *'"),
                    format!("string replace -r '^A{index} ' '' -- \"$MEZ_COMMAND_LINE\" >> \"$MEZ_ARTIFACT_DIR/{index}.b64\"; or set MEZ_WRITE_STATUS $status"),
                ]);
            }
            lines.extend([
                "case '*'".to_string(),
                "set MEZ_WRITE_STATUS 1".to_string(),
                "end".to_string(),
                "end".to_string(),
                acknowledge.to_string(),
            ]);
        }
        lines.push("end".to_string());
    }
    lines.extend([
        "if test \"$MEZ_WRITE_STATUS\" -eq 0; and test \"$MEZ_COMMAND_SEEN_END\" != 1".to_string(),
        "printf '%s\\n' 'Mezzanine shell transaction command payload ended before sentinel' >&2".to_string(),
        "set MEZ_WRITE_STATUS 1".to_string(),
        "end".to_string(),
        "if test \"$MEZ_WRITE_STATUS\" -eq 0".to_string(),
        "if base64 -d < \"$MEZ_COMMAND_B64\" > \"$MEZ_COMMAND_FILE\" 2>/dev/null".to_string(),
        "set MEZ_WRITE_STATUS 0".to_string(),
        "else".to_string(),
        "base64 -D < \"$MEZ_COMMAND_B64\" > \"$MEZ_COMMAND_FILE\"".to_string(),
        "set MEZ_WRITE_STATUS $status".to_string(),
        "end".to_string(),
        "if test \"$MEZ_WRITE_STATUS\" -eq 0; and test -n \"$MEZ_SIDECAR_DATA\"; cat \"$MEZ_SIDECAR_DATA\" >> \"$MEZ_COMMAND_FILE\"; or set MEZ_WRITE_STATUS $status; end".to_string(),
    ]);
    for (index, artifact) in artifacts.iter().enumerate() {
        lines.extend([
            format!("if test \"$MEZ_WRITE_STATUS\" -eq 0; if base64 -d < \"$MEZ_ARTIFACT_DIR/{index}.b64\" > \"$MEZ_ARTIFACT_DIR/{index}\" 2>/dev/null; set MEZ_WRITE_STATUS 0; else; base64 -D < \"$MEZ_ARTIFACT_DIR/{index}.b64\" > \"$MEZ_ARTIFACT_DIR/{index}\"; set MEZ_WRITE_STATUS $status; end; end"),
            format!("if test \"$MEZ_WRITE_STATUS\" -eq 0; chmod {:o} \"$MEZ_ARTIFACT_DIR/{index}\"; or set MEZ_WRITE_STATUS $status; end", artifact.mode),
            format!("command rm -f -- \"$MEZ_ARTIFACT_DIR/{index}.b64\" >/dev/null 2>&1; or true"),
        ]);
    }
    lines.extend([
        "else".to_string(),
        "set MEZ_WRITE_STATUS 1".to_string(),
        "end".to_string(),
        "end".to_string(),
    ]);
    lines.extend([
        "if test -n \"$MEZ_STTY_STATE\"".to_string(),
        "stty \"$MEZ_STTY_STATE\" 2>/dev/null; or true".to_string(),
        "set MEZ_STTY_STATE ''".to_string(),
        "end".to_string(),
    ]);
    CommandMaterialization {
        setup: lines.join("\n") + "\n",
        payload: command_payload_lines(command, &end_marker, input_sidecar, artifacts),
    }
}
