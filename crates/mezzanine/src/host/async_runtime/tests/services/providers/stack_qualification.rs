//! Layout witnesses for default-stack routed/native qualification.
//!
//! These diagnostics infer future types without constructing or polling them.
//! Future storage size is distinct from poll-frame depth and production crash
//! reachability; actual subprocess execution remains the stack-safety oracle.
//! No stack size, scheduler concurrency, native capability or policy is changed.

use super::*;

/// Infers a future layout without invoking its factory or constructing runtime
/// owners. This performs no provider/native work and cannot establish a bound
/// on the compiler's nested poll frames.
fn future_bytes<A, F: std::future::Future>(_: impl FnOnce(A) -> F) -> usize {
    std::mem::size_of::<F>()
}

/// Reports current debug/release future layouts separately from actual default
/// thread-stack execution. No arbitrary size threshold stands in for the real
/// routing, sandbox, result and healthy-owner acceptance assertions.
#[test]
#[ignore = "diagnostic layout witness; execute explicitly with stack qualification"]
fn routed_native_future_layout_witness() {
    let routed = future_bytes(|(): ()| {
        assert_routed_subagent_settles_after_in_place_selection(
            std::path::Path::new("/not-executed"),
            "not-executed",
            "not-executed",
        )
    });
    let boxed_wrapper = future_bytes(|(): ()| async {
        Box::pin(assert_routed_subagent_settles_after_in_place_selection(
            std::path::Path::new("/not-executed"),
            "not-executed",
            "not-executed",
        ))
        .await;
    });
    let actor =
        future_bytes(|actor: crate::host::async_runtime::AsyncRuntimeSessionActor| actor.run());
    let provider = future_bytes(
        |handle: &'static crate::host::async_runtime::AsyncRuntimeSessionHandle| {
            run_async_agent_provider_service(
                handle,
                AsyncAgentProviderServiceConfig::new(1).unwrap(),
                |_, _| false,
            )
        },
    );
    eprintln!(
        "STACK_LAYOUT routed_future_bytes={routed} boxed_wrapper_future_bytes={boxed_wrapper} actor_future_bytes={actor} provider_future_bytes={provider}"
    );
}
