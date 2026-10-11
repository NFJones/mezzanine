//! Fixed neutral Cursor lifecycle helper, not a runtime session/usage producer.
//! Only the literal code-owned lifecycle topics are accepted before startup.
//! Shared finite I/O returns no environment/context/follow-up/permission fields.

use super::{Args, Result, Write};

/// Fixed compiled Cursor lifecycle topic, not an arbitrary method or target.
#[derive(Debug, Clone, Args)]
pub(super) struct CursorHookArgs {
    /// Event selected by compiled hook members; it grants no source authority.
    #[arg(value_parser = ["sessionStart", "stop", "sessionEnd"])]
    event: String,
}

/// Runs only exact fixed argv before HOME/config/CPU discovery. Other forms keep
/// ordinary CLI validation; no vendor invocation or route inference is performed.
pub(crate) fn run_internal_process(arguments: &[std::ffi::OsString]) -> Option<u8> {
    if arguments.len() != 3
        || arguments[1] != "cursor-hook"
        || !matches!(
            arguments[2].to_str(),
            Some("sessionStart" | "stop" | "sessionEnd")
        )
    {
        return None;
    }
    Some(u8::from(run(&mut std::io::stdout()).is_err()))
}

/// Returns only neutral output; deployment does not grant session association.
pub(super) fn run<W: Write>(stdout: &mut W) -> Result<()> {
    super::neutral_hook::run(stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Arbitrary permission/control topics and option-bearing forms must not
    /// receive the exact early helper bypass. This exercises grammar without
    /// consuming process-global stdin or granting any control/transport role.
    #[test]
    fn cursor_hook_exact_argv_rejects_unknown_control_topics() {
        for argv in [
            vec!["mez", "cursor-hook"],
            vec!["mez", "cursor-hook", "preToolUse"],
            vec!["mez", "cursor-hook", "sessionStart", "extra"],
            vec!["mez", "--json", "cursor-hook", "stop"],
        ] {
            let argv = argv
                .into_iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>();
            assert_eq!(run_internal_process(&argv), None);
        }
    }
}
