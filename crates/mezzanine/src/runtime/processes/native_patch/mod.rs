//! Process-free patch execution components owned by the runtime actor.
//!
//! Commit fencing is separate from permission authorization, physical file
//! capabilities and worker/result custody. A fence can prevent later commits;
//! it cannot interrupt a filesystem syscall or prove that an earlier effect
//! settled. The assembled dispatcher must supply current authority separately.

#[allow(
    dead_code,
    reason = "commit fencing is being integrated into the native patch dispatcher"
)]
pub(crate) mod commit;

#[allow(
    dead_code,
    reason = "filesystem worker is being connected to actor dispatch"
)]
pub(crate) mod worker;
