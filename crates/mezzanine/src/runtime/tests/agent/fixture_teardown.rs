//! Shared retirement barrier for runtime fixtures with actual native processes
//! and selector writers. Independent stages always run before the caller may
//! remove exclusively owned storage or rethrow its original scenario panic.

use super::RuntimeSessionService;
use std::any::Any;

/// Retains typed failures and panic ownership without interpreting payloads.
pub(super) enum CleanupFailure {
    Native(crate::MezError),
    Panic(&'static str, Box<dyn Any + Send>),
    Boundary(&'static str),
    Filesystem(std::io::Error),
}

impl std::fmt::Debug for CleanupFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Native(error) => formatter.debug_tuple("Native").field(error).finish(),
            Self::Panic(stage, payload) => formatter
                .debug_tuple("Panic")
                .field(stage)
                .field(&payload.as_ref().type_id())
                .finish(),
            Self::Boundary(reason) => formatter.debug_tuple("Boundary").field(reason).finish(),
            Self::Filesystem(error) => formatter.debug_tuple("Filesystem").field(error).finish(),
        }
    }
}

/// Retires one service without deleting storage needed for reopen. A failure
/// in native teardown cannot skip the independent selector shutdown/join.
pub(super) fn settle(service: &mut RuntimeSessionService) -> Result<(), Vec<CleanupFailure>> {
    let mut failures = Vec::new();
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        service.terminate_all_pane_processes()
    })) {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => failures.push(CleanupFailure::Native(error)),
        Err(payload) => failures.push(CleanupFailure::Panic("native fixture cleanup", payload)),
    }
    if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        service.clear_agent_prompt_inputs()
    })) {
        failures.push(CleanupFailure::Panic("selector fixture cleanup", payload));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures)
    }
}
