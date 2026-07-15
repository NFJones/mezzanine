//! Terminal Keys implementation.
//!
//! This module owns the terminal keys boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

/// Defines the DEFAULT PASTE BUFFER LIMIT BYTES const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const DEFAULT_PASTE_BUFFER_LIMIT_BYTES: usize = 1_048_576;
