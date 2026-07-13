//! Product adapter for agent-owned semantic-patch parsing contracts.
//!
//! Deterministic syntax parsing and path validation live in `mez-agent`.
//! Mezzanine retains matching against filesystem snapshots and shell
//! transaction generation, adapting parse failures into product errors here.

use crate::error::Result;

pub use mez_agent::semantic_patch::try_convert_unified_diff_to_mez_patch;
pub(super) use mez_agent::semantic_patch::{
    MezPatch, MezPatchHunk, MezPatchHunkLine, MezPatchOperation, MezPatchRangeHint,
};

/// Parses one semantic patch and converts agent-owned syntax failures into the
/// product error aggregate used by filesystem planning.
pub(super) fn parse_mez_patch(text: &str) -> Result<MezPatch> {
    mez_agent::semantic_patch::parse_mez_patch(text).map_err(Into::into)
}
