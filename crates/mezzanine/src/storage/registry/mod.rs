//! Durable live-session registry metadata.
//!
//! The registry is not a session daemon. It is a small, structured index under
//! the private runtime directory that lets clients discover resumable sessions,
//! their control sockets, and the primary-attachment state needed for attach
//! decisions. Live process ownership remains with the future session service.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::error::{MezError, Result};
use crate::runtime::ensure_private_socket_directory;
use mez_mux::session::{Session, SessionState};

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

#[cfg(test)]
pub use encoding::session_record_index_aliases;
pub use encoding::{records_to_json, resolve_session_record_target};
pub use types::{RegistrySessionState, SessionRecord, SessionRegistry};

use encoding::{decode_records, set_private_file_permissions};

/// Defines the REGISTRY FILE NAME const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const REGISTRY_FILE_NAME: &str = "sessions.tsv";

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
