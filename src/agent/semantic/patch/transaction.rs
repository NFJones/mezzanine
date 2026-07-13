//! Shell transaction generation for semantic apply-patch actions.
//!
//! The semantic patch pipeline verifies desired file mutations before shell
//! execution. This module owns only the generated shell source used to read
//! remote file snapshots, write verified content bytes, and present unified
//! diffs after the write phase succeeds.

use super::{
    APPLY_PATCH_CONTENT_BEGIN_MARKER, APPLY_PATCH_CONTENT_END_MARKER,
    APPLY_PATCH_FILE_BEGIN_MARKER, APPLY_PATCH_FILE_END_MARKER, APPLY_PATCH_READ_BEGIN_MARKER,
    APPLY_PATCH_READ_END_MARKER, APPLY_PATCH_READ_PHASE_MARKER, ApplyPatchFileChange,
    ApplyPatchOriginalState,
};
use base64::Engine;
use mez_agent::shell_quote;
use std::collections::BTreeSet;

/// Maximum base64 payload bytes emitted on one generated shell-source line.
///
/// File mutations cross the pane PTY as shell input. Keeping individual lines
/// well below common canonical-line limits prevents large content writes from
/// filling the line discipline before the newline is accepted.
pub(super) const FILE_CONTENT_BASE64_SHELL_LINE_BYTES: usize = 768;

/// One shell-backed phase used to complete an `apply_patch` action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyPatchTransactionPhase {
    /// The action is reading remote file snapshots.
    Read,
    /// The action is verifying and writing patched bytes.
    Write,
}

pub(super) fn shell_print_line(line: &str) -> String {
    format!("printf '%s\\n' {}", shell_quote(line))
}

pub(super) fn unified_diff_lines(
    title: &str,
    old_label: &str,
    new_label: &str,
    old_path: &str,
    new_path: &str,
) -> Vec<String> {
    vec![
        shell_print_line(&format!("diff -- {title}")),
        format!(
            "diff -u --label {old_label} --label {new_label} -- {old_path} {new_path}",
            old_label = shell_quote(old_label),
            new_label = shell_quote(new_label)
        ),
        "MEZ_DIFF_STATUS=$?".to_string(),
        "case \"$MEZ_DIFF_STATUS\" in 0|1) :;; *) exit \"$MEZ_DIFF_STATUS\";; esac".to_string(),
    ]
}

/// Builds shell lines that write exact content bytes without embedding the raw
/// payload in the generated shell source.
pub(super) fn write_content_lines(content: &str, target: &str, append: bool) -> Vec<String> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(content.as_bytes());
    let redirect = if append { ">>" } else { ">" };
    let mut lines = vec![
        "command -v base64 >/dev/null || { printf '%s\\n' 'base64 is required for semantic file content actions' >&2; exit 127; }"
            .to_string(),
        "MEZ_CONTENT_B64=$(mktemp) || exit 1".to_string(),
        "{".to_string(),
    ];
    if encoded.is_empty() {
        lines.push("  :".to_string());
    } else {
        for chunk in encoded
            .as_bytes()
            .chunks(FILE_CONTENT_BASE64_SHELL_LINE_BYTES)
        {
            let chunk = std::str::from_utf8(chunk)
                .expect("standard base64 output should always be valid UTF-8");
            lines.push(format!("  printf '%s' {}", shell_quote(chunk)));
        }
    }
    lines.extend([
        "} > \"$MEZ_CONTENT_B64\"".to_string(),
        format!(
            "if base64 -d < \"$MEZ_CONTENT_B64\" {redirect} {target} 2>/dev/null; then MEZ_CONTENT_STATUS=0; else base64 -D < \"$MEZ_CONTENT_B64\" {redirect} {target}; MEZ_CONTENT_STATUS=$?; fi"
        ),
        "rm -f -- \"$MEZ_CONTENT_B64\"".to_string(),
        "if [ \"$MEZ_CONTENT_STATUS\" != 0 ]; then exit \"$MEZ_CONTENT_STATUS\"; fi".to_string(),
    ]);
    lines
}

pub(super) fn mez_apply_patch_read_command(paths: &BTreeSet<String>) -> String {
    let mut lines = vec![
        format!("# {APPLY_PATCH_READ_PHASE_MARKER}"),
        "command -v base64 >/dev/null || { printf '%s\\n' 'apply_patch: base64 is required for apply_patch actions' >&2; exit 127; }".to_string(),
        "command -v realpath >/dev/null || { printf '%s\\n' 'apply_patch: coreutils realpath is required for apply_patch actions' >&2; exit 127; }".to_string(),
        "command -v tr >/dev/null || { printf '%s\\n' 'apply_patch: tr is required for apply_patch actions' >&2; exit 127; }".to_string(),
        "if ! realpath -m -- . >/dev/null 2>&1; then printf '%s\\n' 'apply_patch: coreutils realpath -m is required for apply_patch actions' >&2; exit 127; fi".to_string(),
        "MEZ_APPLY_CWD=$(pwd -P) || exit 1".to_string(),
        "MEZ_APPLY_CWD_PREFIX=${MEZ_APPLY_CWD%/}".to_string(),
        "if [ -z \"$MEZ_APPLY_CWD_PREFIX\" ]; then MEZ_APPLY_CWD_PREFIX=/; fi".to_string(),
        "mez_apply_patch_b64() { printf '%s' \"$1\" | base64 | tr -d '\\n'; }".to_string(),
        "mez_apply_patch_emit_path() {".to_string(),
        "MEZ_APPLY_PATH=$1".to_string(),
        "MEZ_APPLY_RESOLVED=$(realpath -m -- \"$MEZ_APPLY_PATH\" 2>/dev/null) || MEZ_APPLY_RESOLVED=".to_string(),
        "MEZ_APPLY_STATUS=error".to_string(),
        "if [ -n \"$MEZ_APPLY_RESOLVED\" ]; then".to_string(),
        "  case \"$MEZ_APPLY_RESOLVED\" in \"$MEZ_APPLY_CWD\"|\"$MEZ_APPLY_CWD_PREFIX\"/*)".to_string(),
        "    if [ -e \"$MEZ_APPLY_PATH\" ] || [ -L \"$MEZ_APPLY_PATH\" ]; then".to_string(),
        "      if [ -f \"$MEZ_APPLY_RESOLVED\" ]; then MEZ_APPLY_STATUS=regular; else MEZ_APPLY_STATUS=non_regular; fi".to_string(),
        "    else".to_string(),
        "      MEZ_APPLY_STATUS=missing".to_string(),
        "    fi".to_string(),
        "    ;;".to_string(),
        "  *) MEZ_APPLY_STATUS=outside_cwd ;;".to_string(),
        "  esac".to_string(),
        "fi".to_string(),
        format!("printf '%s\\n' {}", shell_quote(APPLY_PATCH_FILE_BEGIN_MARKER)),
        "printf 'PATH_B64 %s\\n' \"$(mez_apply_patch_b64 \"$MEZ_APPLY_PATH\")\"".to_string(),
        "printf 'RESOLVED_B64 %s\\n' \"$(mez_apply_patch_b64 \"$MEZ_APPLY_RESOLVED\")\"".to_string(),
        "printf 'STATUS %s\\n' \"$MEZ_APPLY_STATUS\"".to_string(),
        "if [ \"$MEZ_APPLY_STATUS\" = regular ]; then".to_string(),
        format!("  printf '%s\\n' {}", shell_quote(APPLY_PATCH_CONTENT_BEGIN_MARKER)),
        "  base64 < \"$MEZ_APPLY_RESOLVED\"".to_string(),
        format!("  printf '%s\\n' {}", shell_quote(APPLY_PATCH_CONTENT_END_MARKER)),
        "fi".to_string(),
        format!("printf '%s\\n' {}", shell_quote(APPLY_PATCH_FILE_END_MARKER)),
        "}".to_string(),
        format!("printf '%s\\n' {}", shell_quote(APPLY_PATCH_READ_BEGIN_MARKER)),
    ];
    for path in paths {
        lines.push(format!("mez_apply_patch_emit_path {}", shell_quote(path)));
    }
    lines.extend([
        format!("printf '%s\\n' {}", shell_quote(APPLY_PATCH_READ_END_MARKER)),
        "unset -f mez_apply_patch_emit_path mez_apply_patch_b64 2>/dev/null || :".to_string(),
        "unset MEZ_APPLY_CWD MEZ_APPLY_CWD_PREFIX MEZ_APPLY_PATH MEZ_APPLY_RESOLVED MEZ_APPLY_STATUS".to_string(),
    ]);
    lines.join("\n")
}

pub(super) fn apply_patch_write_command_prelude() -> String {
    [
        "command -v base64 >/dev/null || { printf '%s\\n' 'apply_patch: base64 is required for apply_patch actions' >&2; exit 127; }",
        "command -v realpath >/dev/null || { printf '%s\\n' 'apply_patch: coreutils realpath is required for apply_patch actions' >&2; exit 127; }",
        "command -v cmp >/dev/null || { printf '%s\\n' 'apply_patch: cmp is required for apply_patch actions' >&2; exit 127; }",
        "command -v dirname >/dev/null || { printf '%s\\n' 'apply_patch: dirname is required for apply_patch actions' >&2; exit 127; }",
        "if ! realpath -m -- . >/dev/null 2>&1; then printf '%s\\n' 'apply_patch: coreutils realpath -m is required for apply_patch actions' >&2; exit 127; fi",
        "MEZ_APPLY_CWD=$(pwd -P) || exit 1",
        "MEZ_APPLY_CWD_PREFIX=${MEZ_APPLY_CWD%/}",
        "if [ -z \"$MEZ_APPLY_CWD_PREFIX\" ]; then MEZ_APPLY_CWD_PREFIX=/; fi",
        "mez_apply_patch_resolve_checked() {",
        "MEZ_APPLY_PATH=$1",
        "MEZ_APPLY_EXPECTED_RESOLVED=$2",
        "MEZ_APPLY_RESOLVED=$(realpath -m -- \"$MEZ_APPLY_PATH\" 2>/dev/null) || { printf '%s\\n' \"apply_patch: failed to resolve path: $MEZ_APPLY_PATH\" >&2; exit 1; }",
        "case \"$MEZ_APPLY_RESOLVED\" in \"$MEZ_APPLY_CWD\"|\"$MEZ_APPLY_CWD_PREFIX\"/*) ;; *) printf '%s\\n' \"apply_patch: resolved path is outside current working directory: $MEZ_APPLY_PATH\" >&2; exit 1;; esac",
        "if [ \"$MEZ_APPLY_RESOLVED\" != \"$MEZ_APPLY_EXPECTED_RESOLVED\" ]; then printf '%s\\n' \"apply_patch: resolved path changed before apply: $MEZ_APPLY_PATH\" >&2; exit 1; fi",
        "}",
        "",
    ]
    .join("\n")
}

pub(super) fn apply_patch_write_change_command(
    index: usize,
    change: &ApplyPatchFileChange,
) -> String {
    let expected_var = format!("MEZ_APPLY_EXPECTED_{index}");
    let new_var = format!("MEZ_APPLY_NEW_{index}");
    let original_is_regular = matches!(&change.original, ApplyPatchOriginalState::Regular(_));
    let mut lines = vec![format!(
        "mez_apply_patch_resolve_checked {} {}",
        shell_quote(&change.path),
        shell_quote(&change.resolved_path)
    )];
    match &change.original {
        ApplyPatchOriginalState::Regular(bytes) => {
            lines.push(format!("{expected_var}=$(mktemp) || exit 1"));
            lines.extend(write_content_lines(
                &String::from_utf8_lossy(bytes),
                &format!("\"${expected_var}\""),
                false,
            ));
            lines.push(format!(
                "if [ ! -f \"$MEZ_APPLY_RESOLVED\" ]; then printf '%s\\n' {} >&2; rm -f -- \"${expected_var}\"; exit 1; fi",
                shell_quote(&format!(
                    "apply_patch: refusing to patch non-regular file: {}",
                    change.path
                ))
            ));
            lines.push(format!(
                "if ! cmp -s -- \"${expected_var}\" \"$MEZ_APPLY_RESOLVED\"; then printf '%s\\n' {} >&2; rm -f -- \"${expected_var}\"; exit 1; fi",
                shell_quote(&format!("apply_patch: file changed before apply: {}", change.path))
            ));
        }
        ApplyPatchOriginalState::Missing => {
            lines.push(format!(
                "if [ -e {} ] || [ -L {} ] || [ -e \"$MEZ_APPLY_RESOLVED\" ] || [ -L \"$MEZ_APPLY_RESOLVED\" ]; then printf '%s\\n' {} >&2; exit 1; fi",
                shell_quote(&change.path),
                shell_quote(&change.path),
                shell_quote(&format!("apply_patch: refusing to add existing path: {}", change.path))
            ));
        }
    }
    if let Some(bytes) = &change.final_bytes {
        lines.push(format!("{new_var}=$(mktemp) || exit 1"));
        lines.extend(write_content_lines(
            &String::from_utf8_lossy(bytes),
            &format!("\"${new_var}\""),
            false,
        ));
        let old_label = if original_is_regular {
            format!("a/{}", change.path)
        } else {
            "/dev/null".to_string()
        };
        let old_path = if original_is_regular {
            format!("\"${expected_var}\"")
        } else {
            shell_quote("/dev/null")
        };
        lines.extend(unified_diff_lines(
            "apply patch",
            &old_label,
            &format!("b/{}", change.path),
            &old_path,
            &format!("\"${new_var}\""),
        ));
        lines.push("mkdir -p -- \"$(dirname -- \"$MEZ_APPLY_RESOLVED\")\"".to_string());
        lines.push(format!("mv -f -- \"${new_var}\" \"$MEZ_APPLY_RESOLVED\""));
    } else {
        lines.extend(unified_diff_lines(
            "apply patch",
            &format!("a/{}", change.path),
            "/dev/null",
            &format!("\"${expected_var}\""),
            &shell_quote("/dev/null"),
        ));
        lines.push("rm -f -- \"$MEZ_APPLY_RESOLVED\"".to_string());
    }
    if original_is_regular {
        lines.push(format!("rm -f -- \"${expected_var}\""));
    }
    lines.join("\n") + "\n"
}
