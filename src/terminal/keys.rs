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
/// Defines the DEFAULT PANE TERM const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const DEFAULT_PANE_TERM: &str = "screen-256color";
/// Defines the DEFAULT MEZZANINE TERMINFO const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const DEFAULT_MEZZANINE_TERMINFO: &str = "mez-256color";
/// Defines the MEZZANINE TERMINFO NAMES const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const MEZZANINE_TERMINFO_NAMES: &[&str] = &["mez-256color", "mezzanine-256color"];
/// Defines the TERMINFO FALLBACK ORDER const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const TERMINFO_FALLBACK_ORDER: &[&str] = &["screen-256color", "screen", "vt100", "dumb"];
