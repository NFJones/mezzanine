//! Xauthority record encoding and private atomic publication.
//!
//! Session integration publishes an empty file before pane startup and later
//! atomically replaces it with one loopback MIT-MAGIC-COOKIE-1 record when a
//! route is active. Directories and files must remain owned by the effective
//! user, non-symlinked, and inaccessible to group or other users.

use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use rand::Rng;
use rustix::process::geteuid;

use crate::error::{MezError, Result};

use super::contracts::{X11_AUTH_PROTOCOL_NAME, X11Cookie};

/// Xauthority family value for an IPv4 Internet address.
const XAUTH_FAMILY_INTERNET: u16 = 0;
/// Loopback address used by the session-side TCP display proxy.
const XAUTH_LOOPBACK_ADDRESS: [u8; 4] = [127, 0, 0, 1];

/// Encodes one loopback MIT-MAGIC-COOKIE-1 Xauthority record.
pub(crate) fn encode_xauthority_record(display: u16, cookie: &X11Cookie) -> Result<Vec<u8>> {
    let display = display.to_string();
    let mut record = Vec::with_capacity(64);
    record.extend_from_slice(&XAUTH_FAMILY_INTERNET.to_be_bytes());
    append_counted(&mut record, &XAUTH_LOOPBACK_ADDRESS)?;
    append_counted(&mut record, display.as_bytes())?;
    append_counted(&mut record, X11_AUTH_PROTOCOL_NAME.as_bytes())?;
    append_counted(&mut record, cookie.as_bytes())?;
    Ok(record)
}

/// Atomically publishes one private Xauthority record at a stable path.
pub(crate) fn write_private_xauthority(
    path: &Path,
    display: u16,
    cookie: &X11Cookie,
) -> Result<()> {
    let record = encode_xauthority_record(display, cookie)?;
    write_private_atomic(path, &record)
}

/// Atomically publishes an empty authority database while no route is active.
pub(crate) fn write_empty_private_xauthority(path: &Path) -> Result<()> {
    write_private_atomic(path, &[])
}

/// Appends one Xauthority two-byte-length-prefixed byte string.
fn append_counted(target: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    let length = u16::try_from(bytes.len())
        .map_err(|_| MezError::invalid_args("Xauthority field exceeds the format limit"))?;
    target.extend_from_slice(&length.to_be_bytes());
    target.extend_from_slice(bytes);
    Ok(())
}

/// Replaces one user-private regular file using a sibling temporary file.
fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| MezError::invalid_args("Xauthority path has no parent directory"))?;
    ensure_private_directory(parent)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_private_file(path, &metadata)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let temporary = parent.join(temporary_name(path));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        validate_private_file(path, &fs::symlink_metadata(path)?)?;
        if let Ok(directory) = fs::File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Creates or validates the user-private authority directory.
pub(super) fn ensure_private_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_private_directory(path, &metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true).mode(0o700);
            builder.create(path)?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
            validate_private_directory(path, &fs::symlink_metadata(path)?)
        }
        Err(error) => Err(error.into()),
    }
}

/// Rejects symlinked, foreign-owned, or group/other-accessible directories.
fn validate_private_directory(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(MezError::forbidden(format!(
            "Xauthority directory {} must be a private directory",
            path.display()
        )));
    }
    validate_owner_and_mode(path, metadata, true)
}

/// Rejects symlinked, foreign-owned, or group/other-accessible files.
fn validate_private_file(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(MezError::forbidden(format!(
            "Xauthority path {} must be a private regular file",
            path.display()
        )));
    }
    validate_owner_and_mode(path, metadata, false)
}

/// Validates effective-user ownership and owner-only permission bits.
fn validate_owner_and_mode(path: &Path, metadata: &fs::Metadata, directory: bool) -> Result<()> {
    if metadata.uid() != geteuid().as_raw() {
        return Err(MezError::forbidden(format!(
            "Xauthority path {} has unsafe ownership",
            path.display()
        )));
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 || (directory && mode & 0o100 == 0) {
        return Err(MezError::forbidden(format!(
            "Xauthority path {} must be user-private",
            path.display()
        )));
    }
    Ok(())
}

/// Builds a collision-resistant sibling filename without credential material.
fn temporary_name(path: &Path) -> String {
    let mut random = [0u8; 8];
    rand::rng().fill_bytes(&mut random);
    let mut suffix = String::with_capacity(16);
    for byte in random {
        let _ = write!(&mut suffix, "{byte:02x}");
    }
    format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Xauthority"),
        suffix
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Xauthority encoding must use big-endian counted fields for IPv4
    /// loopback, decimal display number, auth name, and the exact cookie.
    #[test]
    fn encodes_loopback_mit_magic_cookie_record() {
        let cookie = X11Cookie::new([0x66; 16]);

        let encoded = encode_xauthority_record(17, &cookie).unwrap();

        let mut expected = Vec::new();
        expected.extend_from_slice(&0u16.to_be_bytes());
        expected.extend_from_slice(&4u16.to_be_bytes());
        expected.extend_from_slice(&[127, 0, 0, 1]);
        expected.extend_from_slice(&2u16.to_be_bytes());
        expected.extend_from_slice(b"17");
        expected.extend_from_slice(&18u16.to_be_bytes());
        expected.extend_from_slice(b"MIT-MAGIC-COOKIE-1");
        expected.extend_from_slice(&16u16.to_be_bytes());
        expected.extend_from_slice(&[0x66; 16]);
        assert_eq!(encoded, expected);
    }

    /// Publishing and invalidating authority data must retain owner-only modes
    /// and atomically replace the stable regular-file path.
    #[test]
    fn atomically_writes_private_authority_files() {
        let root = test_root("atomic");
        let path = root.join("Xauthority");

        write_private_xauthority(&path, 3, &X11Cookie::new([0x77; 16])).unwrap();
        let populated = fs::read(&path).unwrap();
        assert!(!populated.is_empty());
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        write_empty_private_xauthority(&path).unwrap();
        assert!(fs::read(&path).unwrap().is_empty());
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Existing symlinks must never be followed or replaced when publishing
    /// route credentials into the stable authority path.
    #[test]
    fn rejects_symlinked_authority_targets() {
        use std::os::unix::fs::symlink;

        let root = test_root("symlink");
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let outside = root.with_extension("outside");
        fs::write(&outside, b"unchanged").unwrap();
        let path = root.join("Xauthority");
        symlink(&outside, &path).unwrap();

        let error = write_empty_private_xauthority(&path).unwrap_err();

        assert!(error.to_string().contains("private regular file"));
        assert_eq!(fs::read(&outside).unwrap(), b"unchanged");
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_file(outside);
    }

    /// Allocates one process-local temporary path for authority tests.
    fn test_root(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "mez-x11-authority-{name}-{}-{}",
            std::process::id(),
            temporary_name(Path::new("test"))
        ));
        let _ = fs::remove_dir_all(&root);
        root
    }
}
