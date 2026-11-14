//! Actual auxiliary claims own send liveness independently of expense custody.
//! The final atomic read orders admission against retirement after native
//! binding validation, without actor I/O or an await before permit consumption.

#[derive(Debug, Clone)]
pub(crate) struct AuxiliaryClaimFence(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl AuxiliaryClaimFence {
    pub(crate) fn new() -> Self {
        Self(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
            true,
        )))
    }

    pub(crate) fn capture(&self) -> std::sync::Weak<std::sync::atomic::AtomicBool> {
        std::sync::Arc::downgrade(&self.0)
    }

    pub(crate) fn retire(&self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Only the actor's actual claimed task owns the strong fence; dispatch and
/// late-expense clones keep the task and a weak fence, never this claim.
#[derive(Debug, Clone)]
pub(crate) struct RememberClaim {
    pub(crate) task: crate::runtime::RuntimeAgentRememberTask,
    pub(crate) budget_fence: AuxiliaryClaimFence,
    pub(crate) budget_wait: Option<super::remember_wait::RememberWait>,
}

/// Exact compaction generation retains expense separately. Only this actor
/// allocation keeps its send fence alive; superseded generations stay revoked
/// even while their original result and accounting custody are retained.
#[derive(Debug, Clone)]
pub(crate) struct CompactionClaim {
    pub(crate) task: crate::runtime::RuntimeAgentCompactionTask,
    pub(crate) budget_fence: AuxiliaryClaimFence,
}
