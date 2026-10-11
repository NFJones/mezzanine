//! Shared finite nonretaining input for deployment-only observation hooks.
//!
//! Copilot and Cursor fixed argv owners select their own literal event grammar.
//! This module neither interprets vendor payloads nor routes/enrolls a source.
//! At most64KiB are consumed without EOF read-ahead; private bytes are zeroized.
//! Input loss remains neutral and only response-write failure reaches callers.

use super::{MezError, Result, Write};
use std::time::{Duration, Instant};

/// Returns neutral JSON after a bounded drain, never vendor control/context.
pub(super) fn run<W: Write>(stdout: &mut W) -> Result<()> {
    let _ = drain_input_from(&std::io::stdin());
    writeln!(stdout, "{{}}")?;
    Ok(())
}

/// Strict nonretaining drain under one finite polling deadline; reaching the
/// cap needs no EOF probe because no payload is parsed or forwarded.
pub(super) fn drain_input_from(input: &impl std::os::fd::AsFd) -> Result<()> {
    let deadline = Instant::now() + Duration::from_millis(250);
    let mut consumed = 0;
    let mut buffer = zeroize::Zeroizing::new([0u8; 4096]);
    while consumed < 64 * 1024 {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| MezError::invalid_state("hook input unavailable"))?;
        let timeout = rustix::event::Timespec::try_from(remaining)
            .map_err(|_| MezError::invalid_state("hook input unavailable"))?;
        let mut fds = [rustix::event::PollFd::new(
            input,
            rustix::event::PollFlags::IN,
        )];
        match rustix::event::poll(&mut fds, Some(&timeout)) {
            Ok(0) => return Err(MezError::invalid_state("hook input unavailable")),
            Err(rustix::io::Errno::INTR) => continue,
            Err(error) => return Err(std::io::Error::from(error).into()),
            Ok(_) => {}
        }
        let length = buffer.len().min(64 * 1024 - consumed);
        let count = rustix::io::read(input, &mut buffer[..length]).map_err(std::io::Error::from)?;
        if count == 0 {
            return Ok(());
        }
        consumed += count;
    }
    Ok(())
}
