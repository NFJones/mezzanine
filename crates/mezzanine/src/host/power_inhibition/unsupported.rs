//! Unsupported-platform implementation for host power-inhibition leases.

use super::{PowerInhibitionBackend, PowerInhibitionResource};

/// Backend used on platforms without a native implementation in this release.
#[derive(Debug, Default)]
pub(crate) struct UnsupportedPowerInhibitionBackend;

impl PowerInhibitionBackend for UnsupportedPowerInhibitionBackend {
    fn acquire(&mut self, _: PowerInhibitionResource) -> std::result::Result<u32, String> {
        Err("host power inhibition is unavailable on this platform".to_string())
    }

    fn release(&mut self, _: u32) -> std::result::Result<(), String> {
        Ok(())
    }
}
