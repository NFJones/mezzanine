//! Snapshot payload construction, validation, encoding, and resume planning.
//!
//! Payloads capture restorable session topology plus safe terminal history and
//! transcript references. They do not contain raw credentials or live processes.

use crate::error::{MezError, Result};
use mez_agent::messaging::{MessageService, MessageServiceSnapshot};
use mez_mux::layout::LayoutPolicy;
use mez_mux::process::PaneExitStatus;
use mez_mux::session::{Session, SessionState};
use mez_terminal::{
    GraphicRendition, TerminalColor, TerminalCursorState, TerminalModeState,
    TerminalSavedDecPrivateMode, TerminalSavedState, TerminalStyleSpan, tracked_dec_private_mode,
};

use super::encoding::{
    escape_field, non_empty_string, parse_bool, parse_u16, parse_u32, parse_u64, parse_usize,
    split_fields,
};
#[cfg(test)]
use super::types::SnapshotPaneCapture;
use super::types::{
    LayoutLoadPlan, PaneSnapshotPayload, SessionSnapshotPayload, SnapshotAgentSession,
    SnapshotApprovalGrantMetadata, SnapshotApprovalRequestMetadata, SnapshotConfigDiagnostic,
    SnapshotConfigLayerMetadata, SnapshotCreationContext, SnapshotFrameSettings,
    SnapshotFrameState, SnapshotLayoutNode, SnapshotMcpExternalCapability, SnapshotMcpServerState,
    SnapshotMcpToolEffects, SnapshotMcpToolState, SnapshotPaneGeometry, SnapshotSessionState,
    SnapshotShellMetadata, WindowGroupSnapshotPayload, WindowSnapshotPayload,
};

/// Defines the SNAPSHOT PAYLOAD FORMAT VERSION const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const SNAPSHOT_PAYLOAD_FORMAT_VERSION: u32 = 4;
/// Defines the MIN SUPPORTED SNAPSHOT PAYLOAD FORMAT VERSION const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const MIN_SUPPORTED_SNAPSHOT_PAYLOAD_FORMAT_VERSION: u32 = 2;

mod build;
mod decode;
mod encode;
mod helpers;
mod validate;
