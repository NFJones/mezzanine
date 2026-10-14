//! Ephemeral slash-browser layout diagnostics at the production owner boundary.
//!
//! A browser capture remains local to synchronous attached prompt rendering.
//! Moving only its optional owned payload out of line must not grow retained
//! service/source/actor state. These probes infer concrete future types without
//! constructing, pinning or boxing executed test futures; default-stack MCP and
//! real browser workflows remain separate unchanged behavioral oracles.

use super::*;
use crate::host::async_runtime::AsyncRuntimeSessionActor;
use crate::runtime::commands::list_agents::AgentPromptCommandCapture;
use crate::runtime::service_state::RuntimeRecordBrowserOverlayState;
use mez_core::ids::ClientId;

/// Infers the actual production field's type instead of asserting an unrelated
/// pointer alias; no capture or browser needs to be constructed for the probe.
fn browser_field_bytes<T>(_: fn(&AgentPromptCommandCapture) -> &T) -> usize {
    std::mem::size_of::<T>()
}

/// Infers the consuming actor method's future layout without scheduling it.
fn actor_future_bytes<F>(_: impl FnOnce(AsyncRuntimeSessionActor) -> F) -> usize {
    std::mem::size_of::<F>()
}

/// Infers the borrowed dispatcher future using type-only static references.
/// No references are manufactured or dereferenced and the function is not run.
fn shell_future_bytes<F>(
    _: impl FnOnce(&'static mut RuntimeSessionService, &'static ClientId, &'static str) -> F,
) -> usize {
    std::mem::size_of::<F>()
}

/// Guards pointer-sized optional owned browser storage portably while reporting
/// production frame inputs under the same compiler/profile. Size reduction alone
/// never qualifies the unrelated MCP cancellation acceptance behavior.
#[test]
fn list_agents_slash_layout_reports_local_capture_and_dispatch_footprints() {
    let field_bytes = browser_field_bytes(|capture| &capture.browser);
    eprintln!(
        "slash layout bytes: capture={} browser_field={} state={} service={} actor={} actor_future={} shell_future={}",
        std::mem::size_of::<AgentPromptCommandCapture>(),
        field_bytes,
        std::mem::size_of::<RuntimeRecordBrowserOverlayState>(),
        std::mem::size_of::<RuntimeSessionService>(),
        std::mem::size_of::<AsyncRuntimeSessionActor>(),
        actor_future_bytes(AsyncRuntimeSessionActor::run),
        shell_future_bytes(RuntimeSessionService::execute_agent_shell_command_async),
    );
    assert_eq!(
        field_bytes,
        std::mem::size_of::<usize>(),
        "local optional owned browser must stay out of line"
    );
}
