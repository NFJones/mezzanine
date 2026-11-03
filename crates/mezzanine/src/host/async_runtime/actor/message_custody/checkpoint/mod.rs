//! Host-checkpoint continuation for original-store verification.
//!
//! The session and lower metadata are captured synchronously at one actor point.
//! Only joined store qualification supplies the message state to the deferred
//! context constructor. Observer cancellation cannot detach verification or
//! suppress the already requested atomic snapshot write. No provider permit is
//! acquired and no payload is materialized by this continuation.

use super::*;
use crate::runtime::RuntimeSnapshotOwnedCreationContext;
use crate::storage::snapshot::{SnapshotRepository, SnapshotState};
use mez_agent::messaging::MessageServiceSnapshot;

type Reply = oneshot::Sender<Result<SnapshotState>>;
type Build = Box<dyn FnOnce(MessageServiceSnapshot) -> RuntimeSnapshotOwnedCreationContext + Send>;

/// Original session point and administrative observer retained by the actor.
pub(super) struct Checkpoint {
    session: mez_mux::session::Session,
    build: Build,
    snapshots: SnapshotRepository,
    snapshot_id: String,
    name: Option<String>,
    reply: Reply,
}
impl Checkpoint {
    /// Starts existing atomic snapshot persistence after original-store adoption.
    pub(super) fn persist(
        self,
        message_state: MessageServiceSnapshot,
        lease: mez_agent::messaging::MessageStorageSnapshotLease,
    ) {
        let context = (self.build)(message_state);
        persist(
            self.snapshots,
            self.snapshot_id,
            self.name,
            self.session,
            context,
            self.reply,
            Some(lease),
        );
    }
}
impl AsyncRuntimeSessionActor {
    /// Captures one coherent point and either writes resident state or secures
    /// the custody root before installing the original cold continuation.
    pub(in crate::host::async_runtime::actor) fn create_host_checkpoint(
        &mut self,
        snapshots: SnapshotRepository,
        snapshot_id: String,
        name: Option<String>,
        reply: Reply,
    ) {
        if self.service.message_service().payload_observer().is_none() {
            match self.service.host_checkpoint_snapshot() {
                Ok((session, context)) => {
                    persist(snapshots, snapshot_id, name, session, context, reply, None)
                }
                Err(error) => {
                    let _ = reply.send(Err(error));
                }
            }
            return;
        }
        let result = (|| {
            self.service.refresh_native_message_bindings()?;
            let owner = self.message_custody.as_mut().ok_or_else(|| {
                MezError::invalid_state("MMP checkpoint original root unavailable")
            })?;
            if owner.pending.is_some() || owner.shutdown.is_some() || owner.failure.is_some() {
                return Err(MezError::conflict(
                    "MMP checkpoint custody lane busy or unavailable",
                ));
            }
            let (session, build) = self.service.host_checkpoint_capture();
            owner
                .root
                .start_checkpoint(self.service.message_service())?;
            Ok((session, Box::new(build) as Build))
        })();
        match result {
            Ok((session, build)) => {
                // Root registration was checked above in this synchronous actor turn.
                if let Some(owner) = self.message_custody.as_mut() {
                    owner.pending = Some(Pending::Checkpoint(Box::new(Checkpoint {
                        session,
                        build,
                        snapshots,
                        snapshot_id,
                        name,
                        reply,
                    })));
                    owner.wake = Some(Wake::Observe);
                }
            }
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
    }
}

/// Persists the immutable owned point even if its administrative observer left.
fn persist(
    snapshots: SnapshotRepository,
    snapshot_id: String,
    name: Option<String>,
    session: mez_mux::session::Session,
    context: RuntimeSnapshotOwnedCreationContext,
    reply: Reply,
    lease: Option<mez_agent::messaging::MessageStorageSnapshotLease>,
) {
    let task = tokio::spawn(async move {
        let result = snapshots
            .create_from_session_with_context_async(
                &snapshot_id,
                name,
                &session,
                context.as_creation_context(),
            )
            .await;
        // Release the last owned message-state copy before returning its budget
        // to concurrent actor admission. Repository temporaries are already gone.
        drop(context);
        drop(lease);
        let _ = reply.send(result);
    });
    drop(task);
}
