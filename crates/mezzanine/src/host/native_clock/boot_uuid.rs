//! Fixed kernel boot UUID observations; saved bytes never authorize a clock.

/// Fixed Linux kernel boot UUID, bounded even if procfs behaves unexpectedly.
#[cfg(target_os = "linux")]
pub(crate) fn native_boot_uuid() -> Option<String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open("/proc/sys/kernel/random/boot_id")
        .ok()?
        .take(65)
        .read_to_end(&mut bytes)
        .ok()?;
    parse_boot_uuid(&bytes)
}

/// Darwin native boot-session UUID; unavailable interfaces fail closed rather
/// than substituting wall-clock boot time or a new daemon-lifetime random ID.
#[cfg(target_os = "macos")]
pub(crate) fn native_boot_uuid() -> Option<String> {
    let mut bytes = [0u8; 64];
    let mut len = bytes.len();
    // SAFETY: fixed NUL-terminated sysctl name and bounded writable buffer; the
    // kernel returns actual byte count and no mutation/new process is requested.
    let status = unsafe {
        libc::sysctlbyname(
            c"kern.bootsessionuuid".as_ptr(),
            bytes.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 || len > bytes.len() {
        return None;
    }
    parse_boot_uuid(&bytes[..len])
}

/// Unknown platforms cannot manufacture durable native boot identity.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn native_boot_uuid() -> Option<String> {
    None
}

/// Admits only one bounded UUID with a conventional kernel newline/NUL suffix.
/// Arbitrary whitespace/control bytes and malformed native output stay absent.
fn parse_boot_uuid(bytes: &[u8]) -> Option<String> {
    let bytes = bytes
        .strip_suffix(b"\n")
        .or_else(|| bytes.strip_suffix(b"\0"))
        .unwrap_or(bytes);
    if bytes.len() != 36
        || !bytes.iter().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                *b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
    {
        return None;
    }
    Some(std::str::from_utf8(bytes).ok()?.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Native UUID parsing accepts only fixed kernel output shape and preserves
    /// boot identity across case/suffix conventions without an invented fallback.
    #[test]
    fn external_source_boot_uuid_parser_is_bounded_and_exact() {
        let uuid = "01234567-89ab-cdef-0123-456789abcdef";
        for bytes in [
            uuid.as_bytes().to_vec(),
            format!("{uuid}\n").into_bytes(),
            format!("{}\0", uuid.to_ascii_uppercase()).into_bytes(),
        ] {
            assert_eq!(parse_boot_uuid(&bytes).as_deref(), Some(uuid));
        }
        for bytes in [
            b"".as_slice(),
            b"../private",
            b"01234567-89ab-cdef-0123-456789abcdef\n\0",
            b"01234567-89ab-cdef-0123-456789abcdef extra",
        ] {
            assert!(parse_boot_uuid(bytes).is_none());
        }
    }
}
