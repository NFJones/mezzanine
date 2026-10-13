//! Retained scope layout diagnostics for composition with the runtime actor.
//!
//! A human-only query must not unnecessarily inflate every overlay and actor
//! future. This fixture reports type footprints without constructing or boxing
//! test futures, changing thread stacks, or altering MCP cancellation behavior.
//! The production-owned representation is compared under the same compiler and
//! target before/after a narrow immutable-origin placement repair.

use super::*;
use crate::host::async_runtime::AsyncRuntimeSessionActor;
use crate::runtime::control::agent_browser::{AgentBrowserOrigin, AgentBrowserQuery};
use crate::runtime::service_state::{RuntimeDisplayOverlay, RuntimeRecordBrowserOverlaySource};

/// Infers an async method's concrete future layout without invoking it or
/// allocating its state on this test's stack. No scheduling/authority is changed.
fn actor_future_bytes<F>(_: impl FnOnce(AsyncRuntimeSessionActor) -> F) -> usize {
    std::mem::size_of::<F>()
}

/// Reports retained query/source/service/actor footprints and guards the narrow
/// out-of-line query representation relative to pointer width, not hardcoded
/// platform/toolchain service sizes. The unchanged default-stack MCP test is the
/// behavioral oracle; footprint reduction alone does not prove composition safety.
#[test]
fn scoped_browser_layout_reports_retained_query_and_actor_footprints() {
    eprintln!(
        "scope layout bytes: origin={} query={} source={} overlay={} service={} actor={} actor_future={}",
        std::mem::size_of::<AgentBrowserOrigin>(),
        std::mem::size_of::<AgentBrowserQuery>(),
        std::mem::size_of::<RuntimeRecordBrowserOverlaySource>(),
        std::mem::size_of::<RuntimeDisplayOverlay>(),
        std::mem::size_of::<RuntimeSessionService>(),
        std::mem::size_of::<AsyncRuntimeSessionActor>(),
        actor_future_bytes(AsyncRuntimeSessionActor::run),
    );
    assert!(
        std::mem::size_of::<AgentBrowserQuery>() <= 2 * std::mem::size_of::<usize>(),
        "opaque scope origins must not inflate every retained overlay/actor future"
    );
}
