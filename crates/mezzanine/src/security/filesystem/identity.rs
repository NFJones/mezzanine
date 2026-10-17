//! Native stat device identities in the held-descriptor std metadata domain.
//!
//! Linux exposes an unsigned device number; Darwin exposes a signed i32.
//! Normalize only that representation boundary, without narrowing either value.
//! Callers still own no-follow observation, device AND inode comparison, and
//! revalidation; this conversion grants no authority or atomic unlink guarantee.

/// Returns the device identity in `std::os::unix::fs::MetadataExt::dev` form.
/// Darwin std widens signed `st_dev` to u64, so high-bit native values are
/// sign-extended, not zero-extended through u32 or rejected as invalid devices.
/// Linux retains all 64 bits. This infallible conversion performs no I/O.
pub(crate) fn stat_device(stat: &rustix::fs::Stat) -> u64 {
    #[cfg(target_os = "macos")]
    {
        signed_device(stat.st_dev)
    }
    #[cfg(not(target_os = "macos"))]
    {
        stat.st_dev
    }
}

/// Widens Darwin's signed device before preserving its full two's-complement
/// representation, matching std's signed-to-u64 policy without a narrowing cast.
/// Compiling this branch in tests also exercises Darwin high bits on Linux.
#[cfg(any(test, target_os = "macos"))]
fn signed_device(device: i32) -> u64 {
    u64::from_ne_bytes(i64::from(device).to_ne_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    /// Literal high-bit witnesses distinguish sign extension from a u32
    /// intermediate, rejection of negative devices, or accidental truncation.
    #[test]
    fn stat_device_signed_high_bits_match_std_policy() {
        for (native, expected) in [
            (0, 0),
            (1, 1),
            (i32::MAX, 0x0000_0000_7fff_ffff),
            (i32::MIN, 0xffff_ffff_8000_0000),
            (-1, u64::MAX),
        ] {
            assert_eq!(signed_device(native), expected);
        }
    }

    /// A held directory's native fstat and std metadata must agree on the
    /// device/inode pair on each supported host, including Darwin's real ABI.
    #[test]
    fn stat_device_matches_held_descriptor_metadata() {
        let directory = std::fs::File::open(std::env::temp_dir()).unwrap();
        let stat = rustix::fs::fstat(&directory).unwrap();
        let metadata = directory.metadata().unwrap();
        assert_eq!(stat_device(&stat), metadata.dev());
        assert_eq!(stat.st_ino, metadata.ino());
    }

    /// Exercise the actual Darwin Stat adapter as well as the shared signed
    /// policy, so accidentally bypassing normalization cannot pass native CI.
    #[cfg(target_os = "macos")]
    #[test]
    fn stat_device_darwin_adapter_preserves_signed_high_bits() {
        let directory = std::fs::File::open(std::env::temp_dir()).unwrap();
        let mut stat = rustix::fs::fstat(&directory).unwrap();
        stat.st_dev = i32::MIN;
        assert_eq!(stat_device(&stat), 0xffff_ffff_8000_0000);
        stat.st_dev = -1;
        assert_eq!(stat_device(&stat), u64::MAX);
    }

    /// Linux's unsigned native device is already full width; values above both
    /// u32::MAX and i64::MAX must survive rather than taking a narrowing path.
    #[cfg(target_os = "linux")]
    #[test]
    fn stat_device_linux_adapter_preserves_full_width() {
        let directory = std::fs::File::open(std::env::temp_dir()).unwrap();
        let mut stat = rustix::fs::fstat(&directory).unwrap();
        for device in [0, 0x0000_0001_0000_0000, 0x8000_0000_0000_0000, u64::MAX] {
            stat.st_dev = device;
            assert_eq!(stat_device(&stat), device);
        }
    }
}
