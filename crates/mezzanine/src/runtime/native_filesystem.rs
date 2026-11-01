//! Narrow product port for asynchronous native filesystem orchestration.
//!
//! The host sees exact attempt/producer/lease contracts through this boundary,
//! while actor ownership and process-worker implementation modules remain
//! private. Lower-crate semantic and permission contracts are imported directly
//! by callers; this port does not forward or reinterpret those domain types.

pub(crate) use super::agent::native_filesystem::{
    NativeFilesystemAttempt, RuntimeNativeFilesystemDispatch,
};
pub(crate) use super::processes::native_filesystem_producer::{
    NativeFilesystemProducer, NativeFilesystemProducerStop, NativeFilesystemProgress,
    NativeFilesystemProgressRecorder,
};
pub(crate) use super::processes::native_patch::commit::NativePatchCommitFence;
pub(crate) use super::processes::native_patch::lease::{
    NativePatchCommitLease, NativePatchCommitProposal,
};
pub(crate) use super::processes::native_patch::worker::{
    NativePatchAuthority, NativePatchWorkerOutcome, NativePatchWorkerRequest, execute_native_patch,
};

/// Opaque original checked archive admission, separate from producer transfer.
pub(crate) use super::agent::NativeFilesystemArchiveAcceptance;

/// Qualified storage gate, still separate from source/producer transfer.
pub(crate) use super::agent::NativeFilesystemArchiveSettlement;

/// Exact historical source and observed-producer transfer acknowledgement.
pub(crate) use super::agent::NativeFilesystemTransfer;

/// Positive no-dispatch source and distinct original canonical/storage handoff.
pub(crate) use super::agent::NativeFilesystemRefusalTransfer;
pub(crate) use super::processes::native_filesystem_producer::NativeFilesystemRefusal;
