//! Owner shutdown admission distinguishes actual transport boundaries from the
//! actor's nested reducer result. These fixtures inject classified actor results,
//! not OS faults, native completion, or permission to replay consumed effects.

use super::*;
use crate::host::async_runtime::ShutdownEventAdmission;

/// Takes only the one request admitted by this isolated fixture. It is removed
/// from the real ingress, so oneshot response/drop exercises the actual boundary.
fn take_shutdown_reply(
    actor: &AsyncRuntimeSessionActor,
) -> tokio::sync::oneshot::Sender<Result<crate::runtime::RuntimeEventIngressReport>> {
    let mut ingress = actor.request_ingress.lock().unwrap();
    let queued = ingress
        .urgent_requests
        .pop_front()
        .or_else(|| ingress.interactive_requests.pop_front())
        .or_else(|| ingress.normal_requests.pop_front())
        .or_else(|| ingress.maintenance_requests.pop_front())
        .unwrap();
    let AsyncRuntimeRequest::SubmitRuntimeEvents { reply, .. } = queued.envelope.request else {
        panic!("expected isolated owner shutdown event");
    };
    reply
}

/// Supplies one ordinary typed owner stop event for admission-only controls.
fn shutdown_event() -> crate::runtime::ShutdownEvent {
    crate::runtime::ShutdownEvent {
        reason: "shutdown admission fixture".into(),
        force: true,
        failed: false,
    }
}

/// An actor-produced nested error must remain the outer typed reducer failure;
/// it must never be relabelled as transport unavailability and treated benignly.
#[tokio::test(flavor = "current_thread")]
async fn owner_shutdown_keeps_nested_actor_failure_typed() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let admission = handle.admit_owner_shutdown(shutdown_event());
    tokio::pin!(admission);
    assert!(futures_util::poll!(&mut admission).is_pending());
    let reply = take_shutdown_reply(&actor);
    assert!(
        reply
            .send(Err(MezError::new(
                crate::MezErrorKind::Io,
                "classified actor reducer failure"
            )))
            .is_ok()
    );
    let error = admission.await.unwrap_err();
    assert_eq!(error.kind(), crate::MezErrorKind::Io);
    assert!(error.message().contains("classified actor reducer failure"));
}

/// Both send closure and lost response produce the dedicated transport outcome
/// at their actual boundaries. No actor state or native completion is inferred.
#[tokio::test(flavor = "current_thread")]
async fn owner_shutdown_distinguishes_send_close_and_reply_drop() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    drop(actor);
    assert!(matches!(
        handle.admit_owner_shutdown(shutdown_event()).await.unwrap(),
        ShutdownEventAdmission::TransportUnavailable(_)
    ));

    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let admission = handle.admit_owner_shutdown(shutdown_event());
    tokio::pin!(admission);
    assert!(futures_util::poll!(&mut admission).is_pending());
    drop(take_shutdown_reply(&actor));
    assert!(matches!(
        admission.await.unwrap(),
        ShutdownEventAdmission::TransportUnavailable(_)
    ));
}
