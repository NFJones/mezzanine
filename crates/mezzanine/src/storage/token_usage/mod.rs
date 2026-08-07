//! Durable rolling provider token-accounting storage.
//!
//! This module owns the private SQLite event log used to reconstruct exact
//! rolling usage windows across daemon and conversation lifetimes. It stores
//! immutable provider/model deltas and deliberately excludes pane or transcript
//! identity so telemetry retention does not expand into conversation history.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{MezError, Result};

mod store;

pub(crate) use store::{TokenUsageEvent, TokenUsageStore};

/// Exact rolling windows displayed by `/status --extended`.
pub(crate) const TOKEN_USAGE_WINDOWS_DAYS: [u16; 4] = [7, 30, 60, 90];

/// Raw-event retention, including one day of boundary safety margin.
pub(crate) const TOKEN_USAGE_RETENTION_DAYS: u64 = 91;

/// Returns the token-accounting database beneath a Mezzanine config root.
pub(crate) fn default_token_usage_database_path(config_root: impl AsRef<Path>) -> PathBuf {
    config_root.as_ref().join("token-usage.sqlite")
}

/// Generates a random UUID-shaped id for one immutable accounting event.
pub(crate) fn new_token_usage_event_id() -> String {
    let mut bytes = [0u8; 16];
    let mut rng = rand::rng();
    use rand::Rng;
    rng.fill_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15],
    )
}

fn ensure_private_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

fn set_private_file_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn sqlite_i64(value: u64, field: &str) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| MezError::invalid_args(format!("token usage {field} exceeded SQLite range")))
}

#[cfg(test)]
mod tests;
