//! Neutral bounded Copilot user-hook entry, not a guessed telemetry producer.
//!
//! Official CLI documentation supports direct exec/args, but SDK session identity
//! is separate invocation context, not proof of classic-hook stdin association.
//! This checkpoint drains only bounded stdin and returns neutral JSON. It makes
//! no connection, reads no transcript/config/exporter/credential, emits no user
//! content, and grants no approval/continuation or daemon/native task authority.

use super::{Args, Result, Write};

/// Fixed known argv topics supplied by compiled hook definitions only.
#[derive(Debug, Clone, Args)]
pub(super) struct CopilotHookArgs {
    /// Known compiled hook topic; it grants no session or transport authority.
    #[arg(value_parser = ["sessionStart", "agentStop", "sessionEnd"])]
    event: String,
}

/// Recognizes exact fixed argv before HOME/config/CPU startup. Unknown topics
/// and option-bearing forms retain normal CLI validation; no route is guessed.
pub(crate) fn run_internal_process(arguments: &[std::ffi::OsString]) -> Option<u8> {
    if arguments.len() != 3
        || arguments[1] != "copilot-hook"
        || !matches!(
            arguments[2].to_str(),
            Some("sessionStart" | "agentStop" | "sessionEnd")
        )
    {
        return None;
    }
    Some(u8::from(run(&mut std::io::stdout()).is_err()))
}

/// Vendor outcomes stay untouched. Input loss/time/size limits and unsupported
/// association are neutral; only inability to write the response is an error.
pub(super) fn run<W: Write>(stdout: &mut W) -> Result<()> {
    super::neutral_hook::run(stdout)
}

/// Consumes at most64KiB without retaining payload or reading an extra block.
/// EOF is unnecessary at the byte cap because no payload is interpreted. The
/// finite wall-clock deadline applies to every poll/read and interrupted retry.
#[cfg(test)]
use super::neutral_hook::drain_input_from;

#[cfg(test)]
mod tests {
    use super::*;
    /// Exact recognition does not bypass ordinary CLI validation or capture
    /// arbitrary event/method/credential selectors. These cases perform no I/O.
    #[test]
    fn copilot_hook_exact_argv_rejects_other_modes() {
        for argv in [
            vec!["mez", "copilot-hook"],
            vec!["mez", "copilot-hook", "permissionRequest"],
            vec!["mez", "copilot-hook", "sessionStart", "extra"],
            vec!["mez", "--json", "copilot-hook", "sessionStart"],
        ] {
            let argv = argv
                .into_iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>();
            assert_eq!(run_internal_process(&argv), None);
        }
    }

    /// The neutral helper must consume no more than64KiB, not retain that amount
    /// while reading another full block. A concurrent writer works with portable
    /// small socket buffers; remaining bytes are consumed only by this fixture.
    #[test]
    fn copilot_hook_drain_never_consumes_beyond_exact_limit() {
        use std::io::{Read, Write};
        let (mut input, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let task = std::thread::spawn(move || writer.write_all(&vec![b'X'; 73_728]).unwrap());
        let _ = drain_input_from(&input);
        let mut remaining = Vec::new();
        input.read_to_end(&mut remaining).unwrap();
        task.join().unwrap();
        assert_eq!(remaining.len(), 8192);
    }
}
