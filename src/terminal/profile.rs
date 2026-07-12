//! Product compatibility facade for terminal profiles.
//!
//! The compatibility contracts live in `mez-terminal`. This facade preserves
//! the product crate's error type while root call sites migrate to the lower
//! crate API.

pub use mez_terminal::{
    CapabilitySupport, DEFAULT_TERMINAL_PROFILE_NAME, DecPrivateModeCapabilities,
    MEZZANINE_TERMINFO_PROFILES, SaveRestoreCapabilities, SgrCapabilities,
    TERMINFO_FALLBACK_PROFILES, TerminalCapabilities, TerminalCompatibilityProfile,
    TerminalDiagnostic, TerminalDiagnosticSeverity, TerminalProfile, TerminfoCapabilityProfile,
    TerminfoSelection, TerminfoSource, select_installed_terminfo, select_terminfo,
};

use crate::error::{MezError, Result};

/// Resolves a compatibility profile while preserving product error reporting.
pub fn terminal_profile_named(name: &str) -> Result<TerminalCompatibilityProfile> {
    mez_terminal::terminal_profile_named(name)
        .map_err(|error| MezError::invalid_args(error.message()))
}
