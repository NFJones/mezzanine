//! Project-root and trust helpers.
//!
//! Project trust is rooted at the nearest repository marker or the current
//! directory when no marker exists. This module keeps trust records and overlay
//! discovery rules separate from general user configuration.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::{MezError, Result};

/// Exposes the discovery module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod discovery;
/// Exposes the encoding module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod encoding;
/// Exposes the store module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod store;
/// Exposes the types module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod types;

pub use discovery::{
    default_trust_database_path, discover_existing_overlays, discover_project_root,
};
#[cfg(test)]
pub use discovery::{
    discover_project_trust_prompt, select_overlay_for_directory, summarize_overlay_capabilities,
};
pub use types::{
    OVERLAY_FILENAMES, ProjectTrustPrompt, ProjectTrustRecord, ProjectTrustStore, TrustDecision,
};

use encoding::{
    canonicalize_existing_or_original, parse_record_line, set_private_file_permissions,
    unix_now_seconds,
};

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
