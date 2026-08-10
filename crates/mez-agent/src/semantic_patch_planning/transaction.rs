//! Shell transaction generation for semantic apply-patch actions.
//!
//! The semantic patch pipeline verifies desired file mutations before shell
//! execution. This module owns only the generated shell source used to read
//! remote file snapshots, write verified content bytes, and present unified
//! diffs after the write phase succeeds.

use super::{
    APPLY_PATCH_CONTENT_BEGIN_MARKER, APPLY_PATCH_CONTENT_END_MARKER,
    APPLY_PATCH_FILE_BEGIN_MARKER, APPLY_PATCH_FILE_END_MARKER, APPLY_PATCH_READ_BEGIN_MARKER,
    APPLY_PATCH_READ_END_MARKER, APPLY_PATCH_READ_PHASE_MARKER, APPLY_PATCH_RESULT_MARKER,
    ApplyPatchFileChange, ApplyPatchOriginalState, ApplyPatchPathBoundary,
    apply_patch_path_resolution_lines,
};
use crate::shell_quote;
use base64::Engine;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

/// Maximum Base64 payload bytes emitted in one semantic-write sidecar record.
///
/// Sidecar records cross the pane PTY as bounded data. Keeping individual
/// records below common canonical-line limits preserves portable receiver
/// behavior while avoiding recursive encoding of generated shell source.
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

/// Returns a lowercase SHA-256 digest for exact semantic-write preconditions.
fn content_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Renders the single-encoded final-byte records consumed by write commands.
pub(super) fn apply_patch_write_sidecar(changes: &[ApplyPatchFileChange]) -> Option<String> {
    let mut sidecar = String::new();
    for (index, change) in changes.iter().enumerate() {
        let Some(final_bytes) = &change.final_bytes else {
            continue;
        };
        let encoded = base64::engine::general_purpose::STANDARD.encode(final_bytes);
        if encoded.is_empty() {
            sidecar.push_str(&format!("{index} \n"));
            continue;
        }
        for chunk in encoded
            .as_bytes()
            .chunks(FILE_CONTENT_BASE64_SHELL_LINE_BYTES)
        {
            let chunk = std::str::from_utf8(chunk)
                .expect("standard base64 output should always be valid UTF-8");
            sidecar.push_str(&format!("{index} {chunk}\n"));
        }
    }
    (!sidecar.is_empty()).then_some(sidecar)
}

fn apply_patch_boundary_case_lines(
    boundary: &ApplyPatchPathBoundary,
    failure_command: &str,
) -> Vec<String> {
    match boundary {
        ApplyPatchPathBoundary::CurrentDirectoryOnly => vec![format!(
            "case \"$MEZ_APPLY_PATH\" in /*) {failure_command} ;; *) case \"$MEZ_APPLY_RESOLVED\" in \"$MEZ_APPLY_CWD\"|\"$MEZ_APPLY_CWD_PREFIX\"/*) ;; *) {failure_command} ;; esac ;; esac"
        )],
        ApplyPatchPathBoundary::SandboxWriteScopes(scopes) => {
            let patterns = scopes
                .iter()
                .flat_map(|scope| {
                    let quoted = shell_quote(scope);
                    [quoted.clone(), format!("{quoted}/*")]
                })
                .collect::<Vec<_>>()
                .join("|");
            if patterns.is_empty() {
                vec![failure_command.to_string()]
            } else {
                vec![format!(
                    "case \"$MEZ_APPLY_RESOLVED\" in {patterns}) ;; *) {failure_command} ;; esac"
                )]
            }
        }
    }
}

pub(super) fn mez_apply_patch_read_command(
    paths: &BTreeSet<String>,
    boundary: &ApplyPatchPathBoundary,
) -> String {
    let mut lines = vec![
        format!("# {APPLY_PATCH_READ_PHASE_MARKER}"),
        "command -v base64 >/dev/null || { printf '%s\\n' 'apply_patch: base64 is required for apply_patch actions' >&2; exit 127; }".to_string(),
        "command -v tr >/dev/null || { printf '%s\\n' 'apply_patch: tr is required for apply_patch actions' >&2; exit 127; }".to_string(),
        "MEZ_APPLY_CWD=$(pwd -P) || exit 1".to_string(),
        "MEZ_APPLY_CWD_PREFIX=${MEZ_APPLY_CWD%/}".to_string(),
        "if [ -z \"$MEZ_APPLY_CWD_PREFIX\" ]; then MEZ_APPLY_CWD_PREFIX=/; fi".to_string(),
    ];
    lines.extend(apply_patch_path_resolution_lines());
    lines.extend([
        "mez_apply_patch_b64() { printf '%s' \"$1\" | base64 | tr -d '\\n'; }".to_string(),
        "mez_apply_patch_emit_path() {".to_string(),
        "MEZ_APPLY_PATH=$1".to_string(),
        "MEZ_APPLY_RESOLVED=$(mez_apply_patch_resolve \"$MEZ_APPLY_PATH\" 2>/dev/null) || MEZ_APPLY_RESOLVED=".to_string(),
        "MEZ_APPLY_STATUS=error".to_string(),
        "if [ -n \"$MEZ_APPLY_RESOLVED\" ]; then".to_string(),
    ]);
    let outside_status = match boundary {
        ApplyPatchPathBoundary::CurrentDirectoryOnly => "outside_cwd",
        ApplyPatchPathBoundary::SandboxWriteScopes(_) => "outside_write_scopes",
    };
    lines.extend(apply_patch_boundary_case_lines(
        boundary,
        &format!("MEZ_APPLY_STATUS={outside_status}"),
    ));
    lines.extend([
        "  if [ \"$MEZ_APPLY_STATUS\" = error ]; then".to_string(),
        "    if [ -e \"$MEZ_APPLY_PATH\" ] || [ -L \"$MEZ_APPLY_PATH\" ]; then".to_string(),
        "      if [ -f \"$MEZ_APPLY_RESOLVED\" ]; then MEZ_APPLY_STATUS=regular; else MEZ_APPLY_STATUS=non_regular; fi".to_string(),
        "    else".to_string(),
        "      MEZ_APPLY_STATUS=missing".to_string(),
        "    fi".to_string(),
        "  fi".to_string(),
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
    ]);
    for path in paths {
        lines.push(format!("mez_apply_patch_emit_path {}", shell_quote(path)));
    }
    lines.extend([
        format!("printf '%s\\n' {}", shell_quote(APPLY_PATCH_READ_END_MARKER)),
        "unset -f mez_apply_patch_emit_path mez_apply_patch_b64 mez_apply_patch_resolve 2>/dev/null || :".to_string(),
        "unset MEZ_APPLY_CWD MEZ_APPLY_CWD_PREFIX MEZ_APPLY_PATH MEZ_APPLY_RESOLVED MEZ_APPLY_STATUS MEZ_APPLY_USE_REALPATH_M MEZ_APPLY_READLINK".to_string(),
    ]);
    lines.join("\n")
}

pub(super) fn apply_patch_write_command_prelude(boundary: &ApplyPatchPathBoundary) -> String {
    let mut lines = vec![
        "command -v base64 >/dev/null || { printf '%s\\n' 'apply_patch: base64 is required for apply_patch actions' >&2; exit 127; }".to_string(),
        "command -v dirname >/dev/null || { printf '%s\\n' 'apply_patch: dirname is required for apply_patch actions' >&2; exit 127; }".to_string(),
        "command -v sed >/dev/null || { printf '%s\\n' 'apply_patch: sed is required for apply_patch actions' >&2; exit 127; }".to_string(),
        "command -v tr >/dev/null || { printf '%s\\n' 'apply_patch: tr is required for apply_patch actions' >&2; exit 127; }".to_string(),
        "if command -v sha256sum >/dev/null 2>&1; then MEZ_APPLY_SHA256=sha256sum; elif command -v shasum >/dev/null 2>&1; then MEZ_APPLY_SHA256=shasum; else printf '%s\\n' 'apply_patch: sha256sum or shasum is required for apply_patch actions' >&2; exit 127; fi".to_string(),
        "MEZ_APPLY_SIDECAR_FILE=${MEZ_APPLY_SIDECAR_FILE:-$0}".to_string(),
        "MEZ_APPLY_CWD=$(pwd -P) || exit 1".to_string(),
        "MEZ_APPLY_CWD_PREFIX=${MEZ_APPLY_CWD%/}".to_string(),
        "if [ -z \"$MEZ_APPLY_CWD_PREFIX\" ]; then MEZ_APPLY_CWD_PREFIX=/; fi".to_string(),
    ];
    lines.extend(apply_patch_path_resolution_lines());
    lines.extend([
        "mez_apply_patch_resolve_checked() {".to_string(),
        "MEZ_APPLY_PATH=$1".to_string(),
        "MEZ_APPLY_EXPECTED_RESOLVED=$2".to_string(),
        "MEZ_APPLY_RESOLVED=".to_string(),
        "MEZ_APPLY_RESOLVED=$(mez_apply_patch_resolve \"$MEZ_APPLY_PATH\" 2>/dev/null) || { printf '%s\\n' \"apply_patch: failed to resolve path: $MEZ_APPLY_PATH\" >&2; return 1; }".to_string(),
    ]);
    let diagnostic = match boundary {
        ApplyPatchPathBoundary::CurrentDirectoryOnly => {
            "apply_patch: resolved path is outside current working directory: $MEZ_APPLY_PATH"
        }
        ApplyPatchPathBoundary::SandboxWriteScopes(_) => {
            "apply_patch: resolved path is outside configured sandbox write scopes: $MEZ_APPLY_PATH"
        }
    };
    lines.extend(apply_patch_boundary_case_lines(
        boundary,
        &format!("printf '%s\\n' {} >&2; return 1", shell_quote(diagnostic)),
    ));
    lines.extend([
        "if [ \"$MEZ_APPLY_RESOLVED\" != \"$MEZ_APPLY_EXPECTED_RESOLVED\" ]; then printf '%s\\n' \"apply_patch: resolved path changed before apply: $MEZ_APPLY_PATH\" >&2; return 1; fi".to_string(),
        "}".to_string(),
        "mez_apply_patch_sha256() { if [ \"$MEZ_APPLY_SHA256\" = sha256sum ]; then sha256sum -- \"$1\"; else shasum -a 256 -- \"$1\"; fi | sed 's/[[:space:]].*$//'; }".to_string(),
        "mez_apply_patch_verify_regular() { MEZ_APPLY_VERIFY_PATH=$1; MEZ_APPLY_VERIFY_COUNT=$2; MEZ_APPLY_VERIFY_DIGEST=$3; MEZ_APPLY_VERIFY_LABEL=$4; if [ ! -f \"$MEZ_APPLY_RESOLVED\" ]; then printf '%s\\n' \"apply_patch: refusing to patch non-regular file: $MEZ_APPLY_VERIFY_LABEL\" >&2; return 1; fi; MEZ_APPLY_ACTUAL_COUNT=$(wc -c < \"$MEZ_APPLY_RESOLVED\" | tr -d '[:space:]') || return 1; MEZ_APPLY_ACTUAL_DIGEST=$(mez_apply_patch_sha256 \"$MEZ_APPLY_RESOLVED\") || return 1; if [ \"$MEZ_APPLY_ACTUAL_COUNT\" != \"$MEZ_APPLY_VERIFY_COUNT\" ] || [ \"$MEZ_APPLY_ACTUAL_DIGEST\" != \"$MEZ_APPLY_VERIFY_DIGEST\" ]; then printf '%s\\n' \"apply_patch: file changed before apply: $MEZ_APPLY_VERIFY_LABEL\" >&2; return 1; fi; }".to_string(),
        String::new(),
    ]);
    lines.join("\n")
}

pub(super) fn apply_patch_write_change_command(
    index: usize,
    change: &ApplyPatchFileChange,
) -> String {
    let new_var = format!("MEZ_APPLY_NEW_{index}");
    let encoded_var = format!("MEZ_APPLY_ENCODED_{index}");
    let original_is_regular = matches!(&change.original, ApplyPatchOriginalState::Regular(_));
    let function_name = format!("mez_apply_patch_change_{index}");
    let error_var = format!("MEZ_APPLY_ERROR_{index}");
    let output_var = format!("MEZ_APPLY_OUTPUT_{index}");
    let mut lines = vec![
        format!("{function_name}() {{"),
        format!(
            "mez_apply_patch_resolve_checked {} {} || return 1",
            shell_quote(&change.path),
            shell_quote(&change.resolved_path)
        ),
    ];
    match &change.original {
        ApplyPatchOriginalState::Regular(bytes) => {
            lines.push(format!(
                "mez_apply_patch_verify_regular {} {} {} {} || return 1",
                shell_quote(&change.resolved_path),
                bytes.len(),
                shell_quote(&content_sha256(bytes)),
                shell_quote(&change.path),
            ));
        }
        ApplyPatchOriginalState::Missing => {
            lines.push(format!(
                "if [ -e {} ] || [ -L {} ] || [ -e \"$MEZ_APPLY_RESOLVED\" ] || [ -L \"$MEZ_APPLY_RESOLVED\" ]; then printf '%s\\n' {} >&2; return 1; fi",
                shell_quote(&change.path),
                shell_quote(&change.path),
                shell_quote(&format!("apply_patch: refusing to add existing path: {}", change.path))
            ));
        }
    }
    if let Some(bytes) = &change.final_bytes {
        lines.push("mkdir -p -- \"$(dirname -- \"$MEZ_APPLY_RESOLVED\")\" || return 1".to_string());
        lines.push(format!(
            "{new_var}=$(mktemp \"$(dirname -- \"$MEZ_APPLY_RESOLVED\")/.mez-apply-patch.XXXXXX\") || return 1"
        ));
        lines.push(format!(
            "{encoded_var}=$(mktemp) || {{ rm -f -- \"${new_var}\"; return 1; }}"
        ));
        lines.push(format!(
            "sed -n {} \"${{MEZ_APPLY_SIDECAR_FILE:-$0}}\" > \"${encoded_var}\" || {{ rm -f -- \"${new_var}\" \"${encoded_var}\"; return 1; }}",
            shell_quote(&format!(
                "s/^# __MEZ_INPUT_SIDECAR_V1__ {index} //p"
            ))
        ));
        lines.push(format!(
            "if [ ! -s \"${encoded_var}\" ]; then printf '%s\\n' {} >&2; rm -f -- \"${new_var}\" \"${encoded_var}\"; return 1; fi",
            shell_quote(&format!("apply_patch: missing final content sidecar: {}", change.path)),
        ));
        lines.push(format!(
            "if base64 -d < \"${encoded_var}\" > \"${new_var}\" 2>/dev/null; then MEZ_CONTENT_STATUS=0; else base64 -D < \"${encoded_var}\" > \"${new_var}\"; MEZ_CONTENT_STATUS=$?; fi; rm -f -- \"${encoded_var}\"; if [ \"$MEZ_CONTENT_STATUS\" != 0 ]; then rm -f -- \"${new_var}\"; return \"$MEZ_CONTENT_STATUS\"; fi"
        ));
        lines.push(format!(
            "MEZ_APPLY_FINAL_COUNT=$(wc -c < \"${new_var}\" | tr -d '[:space:]') || {{ rm -f -- \"${new_var}\"; return 1; }}; MEZ_APPLY_FINAL_DIGEST=$(mez_apply_patch_sha256 \"${new_var}\") || {{ rm -f -- \"${new_var}\"; return 1; }}; if [ \"$MEZ_APPLY_FINAL_COUNT\" != {} ] || [ \"$MEZ_APPLY_FINAL_DIGEST\" != {} ]; then printf '%s\\n' {} >&2; rm -f -- \"${new_var}\"; return 1; fi",
            bytes.len(),
            shell_quote(&content_sha256(bytes)),
            shell_quote(&format!("apply_patch: final content digest mismatch: {}", change.path)),
        ));
        let old_label = if original_is_regular {
            format!("a/{}", change.path)
        } else {
            "/dev/null".to_string()
        };
        let old_path = if original_is_regular {
            "\"$MEZ_APPLY_RESOLVED\"".to_string()
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
        lines.push(format!(
            "mez_apply_patch_resolve_checked {} {} || return 1",
            shell_quote(&change.path),
            shell_quote(&change.resolved_path)
        ));
        match &change.original {
            ApplyPatchOriginalState::Regular(bytes) => lines.push(format!(
                "mez_apply_patch_verify_regular {} {} {} {} || {{ rm -f -- \"${new_var}\"; return 1; }}",
                shell_quote(&change.resolved_path),
                bytes.len(),
                shell_quote(&content_sha256(bytes)),
                shell_quote(&change.path),
            )),
            ApplyPatchOriginalState::Missing => lines.push(format!(
                "if [ -e {} ] || [ -L {} ] || [ -e \"$MEZ_APPLY_RESOLVED\" ] || [ -L \"$MEZ_APPLY_RESOLVED\" ]; then printf '%s\\n' {} >&2; rm -f -- \"${new_var}\"; return 1; fi",
                shell_quote(&change.path),
                shell_quote(&change.path),
                shell_quote(&format!("apply_patch: refusing to add existing path: {}", change.path)),
            )),
        }
        lines.push(format!(
            "mez_apply_patch_resolve_checked {} {} || {{ rm -f -- \"${new_var}\"; return 1; }}",
            shell_quote(&change.path),
            shell_quote(&change.resolved_path)
        ));
        lines.push(format!(
            "mv -f -- \"${new_var}\" \"$MEZ_APPLY_RESOLVED\" || {{ rm -f -- \"${new_var}\"; return 1; }}"
        ));
    } else {
        lines.extend(unified_diff_lines(
            "apply patch",
            &format!("a/{}", change.path),
            "/dev/null",
            "\"$MEZ_APPLY_RESOLVED\"",
            &shell_quote("/dev/null"),
        ));
        lines.push(format!(
            "mez_apply_patch_resolve_checked {} {} || return 1",
            shell_quote(&change.path),
            shell_quote(&change.resolved_path)
        ));
        if let ApplyPatchOriginalState::Regular(bytes) = &change.original {
            lines.push(format!(
                "mez_apply_patch_verify_regular {} {} {} {} || return 1",
                shell_quote(&change.resolved_path),
                bytes.len(),
                shell_quote(&content_sha256(bytes)),
                shell_quote(&change.path),
            ));
        }
        lines.push(format!(
            "mez_apply_patch_resolve_checked {} {} || return 1",
            shell_quote(&change.path),
            shell_quote(&change.resolved_path)
        ));
        lines.push("rm -f -- \"$MEZ_APPLY_RESOLVED\" || return 1".to_string());
    }
    lines.push("}".to_string());
    lines.push(format!("{error_var}=$(mktemp) || exit 1"));
    lines.push(format!("{output_var}=$(mktemp) || exit 1"));
    lines.push(format!(
        "if {function_name} >\"${output_var}\" 2>\"${error_var}\"; then cat \"${output_var}\"; printf '%s %s %s\\n' {} APPLIED {}; else MEZ_APPLY_FAILED=1; cat \"${error_var}\" >&2; printf '%s %s %s %s\\n' {} FAILED {} \"$(base64 <\"${error_var}\" | tr -d '\\n')\"; fi",
        shell_quote(APPLY_PATCH_RESULT_MARKER),
        shell_quote(&base64::engine::general_purpose::STANDARD.encode(change.path.as_bytes())),
        shell_quote(APPLY_PATCH_RESULT_MARKER),
        shell_quote(&base64::engine::general_purpose::STANDARD.encode(change.path.as_bytes())),
    ));
    lines.push(format!("rm -f -- \"${error_var}\" \"${output_var}\""));
    lines.push(format!("unset -f {function_name} 2>/dev/null || :"));
    lines.join("\n") + "\n"
}
