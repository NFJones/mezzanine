//! Bounded Linux-kernel evidence used to exclude WSL before D-Bus access.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

const MAX_KERNEL_EVIDENCE_BYTES: u64 = 4096;
const KERNEL_EVIDENCE_PATHS: [&str; 2] = ["/proc/sys/kernel/osrelease", "/proc/version"];

/// Host boundary inferred from bounded, kernel-owned procfs evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LinuxHostKind {
    Native,
    Wsl,
    Unknown,
}

/// Detects WSL without commands, environment data, or Windows interop.
pub(super) fn detect() -> LinuxHostKind {
    let evidence = KERNEL_EVIDENCE_PATHS.map(|path| read_bounded(Path::new(path)));
    classify(evidence.iter().map(|entry| entry.as_deref()))
}

fn read_bounded(path: &Path) -> io::Result<Vec<u8>> {
    let file = File::open(path)?;
    let mut evidence = Vec::new();
    file.take(MAX_KERNEL_EVIDENCE_BYTES)
        .read_to_end(&mut evidence)?;
    Ok(evidence)
}

fn classify<'a>(
    evidence: impl IntoIterator<Item = Result<&'a [u8], &'a io::Error>>,
) -> LinuxHostKind {
    let mut observed = false;
    let mut unavailable = false;
    for item in evidence {
        let Ok(bytes) = item else {
            unavailable = true;
            continue;
        };
        observed = true;
        let bounded = &bytes[..bytes.len().min(MAX_KERNEL_EVIDENCE_BYTES as usize)];
        let lowercase = String::from_utf8_lossy(bounded).to_ascii_lowercase();
        if lowercase.contains("microsoft") || lowercase.contains("wsl") {
            return LinuxHostKind::Wsl;
        }
    }
    if observed && !unavailable {
        LinuxHostKind::Native
    } else {
        LinuxHostKind::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies either canonical WSL marker is recognized case-insensitively
    /// from either bounded kernel evidence source.
    #[test]
    fn kernel_evidence_recognizes_wsl_markers() {
        let error = io::Error::new(io::ErrorKind::NotFound, "not exposed");
        assert_eq!(
            classify([
                Ok(b"5.15.90.1-MICROSOFT-standard-WSL2".as_slice()),
                Err(&error)
            ]),
            LinuxHostKind::Wsl
        );
        assert_eq!(
            classify([Ok(b"Linux version with wSl kernel".as_slice())]),
            LinuxHostKind::Wsl
        );
    }

    /// Verifies unreadable procfs evidence fails closed instead of attempting
    /// a bus operation on an unverified host boundary.
    #[test]
    fn unavailable_kernel_evidence_is_unknown() {
        let first = io::Error::new(io::ErrorKind::PermissionDenied, "hidden");
        let second = io::Error::new(io::ErrorKind::NotFound, "missing");
        assert_eq!(
            classify([Err(&first), Err(&second)]),
            LinuxHostKind::Unknown
        );
        assert_eq!(
            classify([Ok(b"6.8.0-generic".as_slice()), Err(&second)]),
            LinuxHostKind::Unknown
        );
    }
}
