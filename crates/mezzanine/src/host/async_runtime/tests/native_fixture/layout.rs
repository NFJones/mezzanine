//! Type-only fixture layout diagnostics; no owner/future construction or polling.
//!
//! Function items are passed only to infer their return TYPES. The helpers never
//! call those items or create their arguments. Static Rust layout is not peak
//! stack use, a faulting instruction, a spawn-phase receipt, or causal proof for
//! the preserved stack overflow. This test is safe to select independently of
//! every real-shell/native fixture in this module.

use std::mem::size_of;

use super::*;

type ReadyUnit = std::future::Ready<Result<(), MezError>>;
type Completion = Option<tokio::sync::oneshot::Sender<()>>;

/// Infers a unary function's return layout WITHOUT invoking the function.
fn return_size<A, F>(_: fn(A) -> F) -> usize {
    size_of::<F>()
}

/// Infers the actual fixture-join return layout using only argument TYPES.
fn fixture_join_size<F>(
    _: fn(
        &'static NativeFixture,
        ReadyUnit,
        ReadyUnit,
        Box<AsyncRuntimeSessionActor>,
        &'static AsyncRuntimeSessionHandle,
        JoinControl,
    ) -> F,
) -> usize {
    size_of::<F>()
}

/// Names capture's argument/return signature without constructing its values.
type CaptureFunction<T, F> = fn(
    std::future::Ready<Result<T, MezError>>,
    Stage,
    Rc<RefCell<Option<StageResult<T>>>>,
    Rc<RefCell<Vec<Stage>>>,
    Arc<tokio::sync::Notify>,
    Arc<AtomicBool>,
    Completion,
) -> F;

/// Infers a capture future layout for a named ready-result type without calling
/// capture. This is one monomorphization, not all concrete client/worker futures.
fn capture_ready_size<T, F>(_: CaptureFunction<T, F>) -> usize {
    size_of::<F>()
}

#[test]
/// Reports named service/actor/result/future layouts without constructing any
/// native service, actor, result payload, or future. Only compile-time types and
/// small diagnostic JSON values exist; no function item supplied above is called.
fn fixture_owner_type_layouts() {
    let report = serde_json::json!({
        "schema": "fcc7-type-layout-v2",
        "pointer": size_of::<usize>(),
        "service": size_of::<RuntimeSessionService>(),
        "actor": size_of::<AsyncRuntimeSessionActor>(),
        "actor_exit": size_of::<AsyncRuntimeActorExit>(),
        "actor_stage_result": size_of::<StageResult<Box<AsyncRuntimeActorExit>>>(),
        "actor_capture_cell": size_of::<RefCell<Option<StageResult<Box<AsyncRuntimeActorExit>>>>>(),
        "joined_unit": size_of::<Joined<(), ()>>(),
        "build_result": size_of::<Result<(crate::runtime::PaneProcessStart, AsyncRuntimeSessionHandle, Box<AsyncRuntimeSessionActor>), Failure>>(),
        "finish_result": size_of::<Result<((), (), Box<AsyncRuntimeActorExit>), Box<FixtureFailure>>>(),
        "actor_run_future": return_size(AsyncRuntimeSessionActor::run),
        "fixture_join_ready_future": fixture_join_size(NativeFixture::join::<(), ()>),
        "capture_actor_ready_future": capture_ready_size::<Box<AsyncRuntimeActorExit>, _>(capture::<Box<AsyncRuntimeActorExit>>),
        "legacy_unboxed_capture_actor_ready_future": capture_ready_size::<AsyncRuntimeActorExit, _>(capture::<AsyncRuntimeActorExit>),
        "boxed_service": size_of::<Box<RuntimeSessionService>>(),
        "boxed_actor": size_of::<Box<AsyncRuntimeSessionActor>>(),
        "boxed_actor_exit": size_of::<Box<AsyncRuntimeActorExit>>(),
        "boxed_actor_stage_result": size_of::<StageResult<Box<AsyncRuntimeActorExit>>>(),
        "boxed_joined_unit": size_of::<Joined<(), (), Box<AsyncRuntimeActorExit>>>(),
    });
    println!("{report}");
    assert_eq!(size_of::<Box<AsyncRuntimeActorExit>>(), size_of::<usize>());
}
