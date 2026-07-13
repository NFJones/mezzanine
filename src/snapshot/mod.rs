//! Session snapshot metadata.
//!
//! Snapshot process resurrection is outside the current implementation. This
//! module models manifest safety rules, private persistence, and validation that
//! snapshots do not carry raw credentials or restored approval authority.

/// Exposes the encoding module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod encoding;
/// Exposes the manifest module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod manifest;
/// Exposes the payload module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod payload;
/// Exposes the repository module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod repository;
/// Exposes the restore module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod restore;
pub(crate) use restore::session_restore_input;
/// Exposes the types module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod types;

pub use types::{
    LayoutLoadPlan, PaneSnapshotPayload, SessionSnapshotPayload, SnapshotAgentSession,
    SnapshotApprovalGrantMetadata, SnapshotApprovalRequestMetadata, SnapshotConfigDiagnostic,
    SnapshotConfigLayerMetadata, SnapshotCreationContext, SnapshotFrameSettings,
    SnapshotFrameState, SnapshotKind, SnapshotLayoutNode, SnapshotManifest,
    SnapshotMcpExternalCapability, SnapshotMcpServerState, SnapshotMcpToolEffects,
    SnapshotMcpToolState, SnapshotPaneCapture, SnapshotPaneGeometry, SnapshotRepository,
    SnapshotRestoreResult, SnapshotRollbackPlan, SnapshotSessionState, SnapshotShellMetadata,
    SnapshotState, WindowGroupSnapshotPayload, WindowSnapshotPayload,
};

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
