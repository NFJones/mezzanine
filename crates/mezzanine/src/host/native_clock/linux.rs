//! Captured zero-offset Linux time namespace and suspend-inclusive sampling.
//!
//! Qualification reads offsets only while all three namespace handles are held.
//! Offsets cannot change after a process enters that namespace (time_namespaces(7)).
//! Every later sample checks current-thread, leader and children identities on
//! both sides of CLOCK_BOOTTIME; retaining handles prevents inode reuse. These
//! are bounded kernel metadata observations, not clock/offset file reads.

use std::fs::File;
use std::io::Read;
use std::os::unix::fs::MetadataExt;

#[derive(Debug)]
pub(super) struct Clock {
    /// Pin original namespace objects through every sample.
    _namespaces: [File; 3],
    identity: (u64, u64),
}
impl Clock {
    /// Qualifies matching namespace objects and both immutable zero offsets.
    pub(super) fn capture() -> Option<Self> {
        let namespaces = [
            File::open("/proc/thread-self/ns/time").ok()?,
            File::open("/proc/self/ns/time").ok()?,
            File::open("/proc/self/ns/time_for_children").ok()?,
        ];
        let identity = identity(namespaces[0].metadata().ok()?);
        let observed = [
            identity,
            self::identity(namespaces[1].metadata().ok()?),
            self::identity(namespaces[2].metadata().ok()?),
        ];
        if !super::namespace_epoch_matches(identity, observed) {
            return None;
        }
        let mut bytes = Vec::new();
        File::open("/proc/self/timens_offsets")
            .ok()?
            .take(257)
            .read_to_end(&mut bytes)
            .ok()?;
        if !super::zero_offsets(&bytes) {
            return None;
        }
        let clock = Self {
            _namespaces: namespaces,
            identity,
        };
        clock.millis()?;
        Some(clock)
    }

    /// Samples the fixed clock only within the originally qualified namespace.
    pub(super) fn millis(&self) -> Option<u64> {
        if !super::namespace_epoch_matches(self.identity, current_namespaces()?) {
            return None;
        }
        let mut time = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: fixed read-only clock ID and initialized writable timespec.
        if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut time) } != 0 {
            return None;
        }
        if !super::namespace_epoch_matches(self.identity, current_namespaces()?) {
            return None;
        }
        super::timespec_millis(time.tv_sec, time.tv_nsec)
    }
}

/// Bounded stat observations bind the sampled clock to its captured namespace.
fn current_namespaces() -> Option<[(u64, u64); 3]> {
    Some([
        identity(std::fs::metadata("/proc/thread-self/ns/time").ok()?),
        identity(std::fs::metadata("/proc/self/ns/time").ok()?),
        identity(std::fs::metadata("/proc/self/ns/time_for_children").ok()?),
    ])
}

/// A retained handle pins both values; textual namespace names are not evidence.
fn identity(metadata: std::fs::Metadata) -> (u64, u64) {
    (metadata.dev(), metadata.ino())
}
