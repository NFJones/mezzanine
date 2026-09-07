//! Unsupported-platform implementation for host power-inhibition leases.

use super::{
    PowerInhibitionBackend, PowerInhibitionBackendKind, PowerInhibitionLease,
    PowerInhibitionResource,
};

/// Backend used on platforms without a native implementation in this release.
#[derive(Debug, Default)]
pub(crate) struct UnsupportedPowerInhibitionBackend;

impl PowerInhibitionBackend for UnsupportedPowerInhibitionBackend {
    fn kind(&self) -> PowerInhibitionBackendKind {
        PowerInhibitionBackendKind::Unsupported
    }

    fn acquire(
        &mut self,
        _: PowerInhibitionResource,
    ) -> std::result::Result<Box<dyn PowerInhibitionLease>, String> {
        Err("host power inhibition is unavailable on this platform".to_string())
    }
}
