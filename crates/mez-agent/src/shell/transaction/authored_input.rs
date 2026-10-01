//! Lexical admission policy for model-authored shell commands.
//!
//! This scan rejects unquoted heredoc and here-string redirections before any
//! generated wrapper is constructed. It neither evaluates shell text nor grants
//! filesystem/process permission; those effects remain product-owned.

use super::{AgentShellValidationError, AgentShellValidationResult};

/// Validates model-authored shell input before Mezzanine wraps it for pane
/// execution.
///
/// Model-authored heredoc and here-string redirections are disabled because
/// they are easy to leave unterminated and can strand the shell transaction.
/// Runtime-generated wrappers use bounded shell syntax and base64 command
/// materialization instead. Filesystem effects from other shell syntax are
/// evaluated by the permission policy and sandbox layers.
pub fn validate_agent_authored_shell_command(command: &str) -> AgentShellValidationResult<()> {
    if shell_command_contains_unquoted_heredoc(command) {
        return Err(AgentShellValidationError::invalid_args(
            "shell_command heredoc redirection is disabled for agent-authored commands",
        ));
    }
    Ok(())
}

/// Returns whether a shell command contains an unquoted heredoc or here-string
/// redirection token.
///
/// This is a conservative lexical scan. It ignores tokens inside single and
/// double quoted strings and comments, while treating any unquoted `<<`, `<<-`,
/// or `<<<` occurrence as disabled shell input.
pub fn shell_command_contains_unquoted_heredoc(command: &str) -> bool {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ScanState {
        Normal,
        SingleQuoted,
        DoubleQuoted,
    }

    let mut chars = command.chars().peekable();
    let mut state = ScanState::Normal;
    while let Some(ch) = chars.next() {
        match state {
            ScanState::Normal => match ch {
                '\\' => {
                    let _ = chars.next();
                }
                '\'' => state = ScanState::SingleQuoted,
                '"' => state = ScanState::DoubleQuoted,
                '#' => {
                    for comment_ch in chars.by_ref() {
                        if comment_ch == '\n' {
                            break;
                        }
                    }
                }
                '<' if chars.peek() == Some(&'<') => return true,
                _ => {}
            },
            ScanState::SingleQuoted => {
                if ch == '\'' {
                    state = ScanState::Normal;
                }
            }
            ScanState::DoubleQuoted => match ch {
                '\\' => {
                    let _ = chars.next();
                }
                '"' => state = ScanState::Normal,
                _ => {}
            },
        }
    }
    false
}
