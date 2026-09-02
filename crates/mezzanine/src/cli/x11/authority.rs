//! Client-local Xauthority selection and untrusted credential leases.
//!
//! Trusted credentials are selected from a bounded owner-private binary
//! Xauthority database using the exact frozen endpoint. Untrusted credentials
//! are generated through argv-only `xauth` execution against a private copy of
//! that database, have a short X SECURITY timeout, and never modify the user's
//! authority file.

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use rand::Rng;
use rustix::fs::{Mode, OFlags, open};
use rustix::process::geteuid;
use zeroize::Zeroizing;

use crate::error::{MezError, Result};
use crate::runtime::x11::{X11_AUTH_PROTOCOL_NAME, X11_COOKIE_BYTES, X11Cookie};

use super::display::ResolvedX11Display;

/// Maximum authority database accepted from a local file.
const XAUTHORITY_MAX_BYTES: u64 = 1024 * 1024;
/// X SECURITY idle lifetime requested for one untrusted attach credential.
///
/// The server retains the authorization while a client uses it, then purges it
/// after this bounded idle period. Local cleanup separately removes the private
/// authority record immediately.
const X11_UNTRUSTED_TIMEOUT_SECONDS: u64 = 20 * 60;
/// Maximum part of one xauth lifecycle budget reserved for termination and reap.
const XAUTH_TERMINATION_GRACE_MAX: Duration = Duration::from_millis(250);

/// Attach-lifetime credential cleanup state.
pub(super) enum X11CredentialLease {
    /// Trusted lookup did not create temporary client state.
    Trusted,
    /// Untrusted X SECURITY state and its private authority copy.
    Untrusted(UntrustedX11CredentialLease),
}

impl X11CredentialLease {
    /// Performs bounded explicit cleanup. Drop still removes private files if
    /// attach cancellation prevents this method from running.
    pub(super) async fn close(&mut self) -> Result<()> {
        match self {
            Self::Trusted => Ok(()),
            Self::Untrusted(lease) => lease.close().await,
        }
    }
}

/// Private generated credential state used only by the local attach client.
pub(super) struct UntrustedX11CredentialLease {
    cleanup: Option<XauthCleanup>,
}

impl UntrustedX11CredentialLease {
    /// Removes the generated local database entry and private artifacts.
    async fn close(&mut self) -> Result<()> {
        let Some(cleanup) = self.cleanup.take() else {
            return Ok(());
        };
        let remove_result = run_xauth(
            &cleanup.executable,
            &cleanup.authority_path,
            &[
                OsString::from("-n"),
                OsString::from("-q"),
                OsString::from("-f"),
                cleanup.authority_path.as_os_str().to_os_string(),
                OsString::from("remove"),
                OsString::from(&cleanup.display_name),
            ],
            cleanup.command_timeout,
        )
        .await;
        let _ = fs::remove_dir_all(&cleanup.directory);
        remove_result
    }

    #[cfg(test)]
    fn directory(&self) -> &Path {
        self.cleanup
            .as_ref()
            .map(|cleanup| cleanup.directory.as_path())
            .unwrap_or_else(|| Path::new(""))
    }
}

impl Drop for UntrustedX11CredentialLease {
    fn drop(&mut self) {
        if let Some(cleanup) = self.cleanup.take() {
            let _ = fs::remove_dir_all(cleanup.directory);
        }
    }
}

/// Owned arguments needed for explicit untrusted cleanup.
struct XauthCleanup {
    executable: OsString,
    authority_path: PathBuf,
    directory: PathBuf,
    display_name: String,
    command_timeout: Duration,
}

/// Resolves the process authority path without formatting it into diagnostics.
pub(super) fn process_xauthority_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("XAUTHORITY").filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .ok_or_else(|| MezError::invalid_state("XAUTHORITY and HOME are unavailable"))?;
    Ok(PathBuf::from(home).join(".Xauthority"))
}

/// Loads one exact trusted MIT credential from a private authority database.
pub(super) fn load_trusted_x11_cookie(
    path: &Path,
    display: &ResolvedX11Display,
) -> Result<X11Cookie> {
    let bytes = read_private_authority(path)?;
    select_x11_cookie(&bytes, display)
}

/// Generates one short-lived untrusted credential without modifying the
/// user's authority database.
pub(super) async fn generate_untrusted_x11_cookie(
    source_path: &Path,
    display: &ResolvedX11Display,
    executable: &OsStr,
    command_timeout: Duration,
) -> Result<(X11Cookie, UntrustedX11CredentialLease)> {
    let trusted = load_trusted_x11_cookie(source_path, display)?;
    let directory = create_private_lease_directory()?;
    let authority_path = directory.join("authority");
    let seed = encode_seed_authority(display, &trusted)?;
    write_private_file(&authority_path, &seed)?;
    let mut lease = UntrustedX11CredentialLease {
        cleanup: Some(XauthCleanup {
            executable: executable.to_os_string(),
            authority_path: authority_path.clone(),
            directory,
            display_name: display.display_name().to_string(),
            command_timeout,
        }),
    };
    let generate_result = run_xauth(
        executable,
        &authority_path,
        &[
            OsString::from("-n"),
            OsString::from("-q"),
            OsString::from("-f"),
            authority_path.as_os_str().to_os_string(),
            OsString::from("generate"),
            OsString::from(display.display_name()),
            OsString::from(X11_AUTH_PROTOCOL_NAME),
            OsString::from("untrusted"),
            OsString::from("timeout"),
            OsString::from(X11_UNTRUSTED_TIMEOUT_SECONDS.to_string()),
        ],
        command_timeout,
    )
    .await;
    if let Err(error) = generate_result {
        let _ = lease.close().await;
        return Err(error);
    }
    let generated = load_trusted_x11_cookie(&authority_path, display)?;
    if generated == trusted {
        let _ = lease.close().await;
        return Err(MezError::invalid_state(
            "xauth did not produce a distinct untrusted X11 credential",
        ));
    }
    Ok((generated, lease))
}

/// Runs one bounded argv-only xauth command without retaining command output.
async fn run_xauth(
    executable: &OsStr,
    authority_path: &Path,
    arguments: &[OsString],
    timeout: Duration,
) -> Result<()> {
    let started = tokio::time::Instant::now();
    let termination_grace = (timeout / 4).min(XAUTH_TERMINATION_GRACE_MAX);
    let execution_deadline = started + timeout.saturating_sub(termination_grace);
    let lifecycle_deadline = started + timeout;
    let mut command = tokio::process::Command::new(executable);
    command
        .args(arguments)
        .env("XAUTHORITY", authority_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|_| MezError::invalid_state("xauth is unavailable for X11 credential setup"))?;
    let status = match tokio::time::timeout_at(execution_deadline, child.wait()).await {
        Ok(result) => {
            result.map_err(|_| MezError::invalid_state("xauth credential setup failed"))?
        }
        Err(_) => {
            let _ = child.start_kill();
            let _ = tokio::time::timeout_at(lifecycle_deadline, child.wait()).await;
            return Err(MezError::invalid_state("xauth credential setup timed out"));
        }
    };
    if !status.success() {
        return Err(MezError::invalid_state(
            "xauth rejected X11 credential setup",
        ));
    }
    Ok(())
}

/// Reads a bounded owner-private regular authority file without following a
/// symlink.
fn read_private_authority(path: &Path) -> Result<Zeroizing<Vec<u8>>> {
    let descriptor = open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| MezError::invalid_state("local Xauthority file is unavailable"))?;
    let file = File::from(descriptor);
    let metadata = file
        .metadata()
        .map_err(|_| MezError::invalid_state("local Xauthority metadata is unavailable"))?;
    if !metadata.is_file()
        || metadata.uid() != geteuid().as_raw()
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.len() > XAUTHORITY_MAX_BYTES
    {
        return Err(MezError::forbidden(
            "local Xauthority file must be an owner-private bounded regular file",
        ));
    }
    let mut bytes = Zeroizing::new(Vec::with_capacity(metadata.len() as usize));
    file.take(XAUTHORITY_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| MezError::invalid_state("local Xauthority file could not be read"))?;
    if bytes.len() as u64 > XAUTHORITY_MAX_BYTES {
        return Err(MezError::forbidden(
            "local Xauthority file exceeds the fixed size limit",
        ));
    }
    Ok(bytes)
}

/// Selects exactly one endpoint-matching MIT credential from binary records.
fn select_x11_cookie(bytes: &[u8], display: &ResolvedX11Display) -> Result<X11Cookie> {
    let mut offset = 0usize;
    let mut selected: Option<X11Cookie> = None;
    while offset < bytes.len() {
        let family = read_u16(bytes, &mut offset)?;
        let address = read_counted(bytes, &mut offset)?;
        let number = read_counted(bytes, &mut offset)?;
        let name = read_counted(bytes, &mut offset)?;
        let data = read_counted(bytes, &mut offset)?;
        if !display.matches_authority(family, address, number)
            || name != X11_AUTH_PROTOCOL_NAME.as_bytes()
        {
            continue;
        }
        let cookie_bytes: [u8; X11_COOKIE_BYTES] = data.try_into().map_err(|_| {
            MezError::invalid_state("matching X11 authority credential has an invalid length")
        })?;
        let candidate = X11Cookie::new(cookie_bytes);
        match selected.as_ref() {
            None => selected = Some(candidate),
            Some(existing) if existing == &candidate => {}
            Some(_) => {
                return Err(MezError::conflict(
                    "multiple distinct X11 authority credentials match the local display",
                ));
            }
        }
    }
    selected.ok_or_else(|| {
        MezError::new(
            crate::error::MezErrorKind::NotFound,
            "no matching MIT-MAGIC-COOKIE-1 credential exists for the local display",
        )
    })
}

/// Reads one big-endian Xauthority integer.
fn read_u16(bytes: &[u8], offset: &mut usize) -> Result<u16> {
    let end = offset
        .checked_add(2)
        .ok_or_else(invalid_authority_database)?;
    let encoded: [u8; 2] = bytes
        .get(*offset..end)
        .ok_or_else(invalid_authority_database)?
        .try_into()
        .map_err(|_| invalid_authority_database())?;
    *offset = end;
    Ok(u16::from_be_bytes(encoded))
}

/// Reads one bounded Xauthority counted field.
fn read_counted<'a>(bytes: &'a [u8], offset: &mut usize) -> Result<&'a [u8]> {
    let length = usize::from(read_u16(bytes, offset)?);
    let end = offset
        .checked_add(length)
        .ok_or_else(invalid_authority_database)?;
    let field = bytes
        .get(*offset..end)
        .ok_or_else(invalid_authority_database)?;
    *offset = end;
    Ok(field)
}

/// Returns a content-free malformed-database failure.
fn invalid_authority_database() -> MezError {
    MezError::invalid_state("local Xauthority database is malformed")
}

/// Encodes the sole trusted selector needed by `xauth generate`.
fn encode_seed_authority(
    display: &ResolvedX11Display,
    cookie: &X11Cookie,
) -> Result<Zeroizing<Vec<u8>>> {
    let display_number = display.display_number().to_string();
    let (family, address) = display.seed_authority_selector();
    let mut record = Zeroizing::new(Vec::with_capacity(64));
    record.extend_from_slice(&family.to_be_bytes());
    append_authority_field(&mut record, address)?;
    append_authority_field(&mut record, display_number.as_bytes())?;
    append_authority_field(&mut record, X11_AUTH_PROTOCOL_NAME.as_bytes())?;
    append_authority_field(&mut record, cookie.as_bytes())?;
    Ok(record)
}

/// Appends one big-endian Xauthority counted field.
fn append_authority_field(target: &mut Vec<u8>, field: &[u8]) -> Result<()> {
    let length = u16::try_from(field.len())
        .map_err(|_| MezError::invalid_state("local Xauthority field exceeds its format limit"))?;
    target.extend_from_slice(&length.to_be_bytes());
    target.extend_from_slice(field);
    Ok(())
}

/// Creates one owner-private temporary directory for an untrusted lease.
fn create_private_lease_directory() -> Result<PathBuf> {
    for _ in 0..8 {
        let mut random = [0u8; 12];
        rand::rng().fill_bytes(&mut random);
        let mut suffix = String::with_capacity(24);
        for byte in random {
            let _ = write!(&mut suffix, "{byte:02x}");
        }
        let directory =
            std::env::temp_dir().join(format!("mez-x11-client-{}-{suffix}", std::process::id()));
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        match builder.create(&directory) {
            Ok(()) => {
                fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
                return Ok(directory);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(MezError::invalid_state(
        "failed to allocate private X11 credential state",
    ))
}

/// Writes one private seed authority file without replacing an existing path.
fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::x11::display::{XAUTH_FAMILY_INTERNET, resolve_local_x11_display};

    /// Exact endpoint matching must select one 16-byte MIT credential while
    /// malformed, missing, and ambiguous databases fail without secret text.
    #[test]
    fn selects_exact_binary_xauthority_records() {
        let display = resolve_local_x11_display(":17").unwrap();
        let mut database = authority_record(&display, [0x11; X11_COOKIE_BYTES]);
        let selected = select_x11_cookie(&database, &display).unwrap();
        assert_eq!(selected, X11Cookie::new([0x11; X11_COOKIE_BYTES]));
        assert_eq!(format!("{selected:?}"), "X11Cookie([REDACTED])");

        database.extend_from_slice(&authority_record(&display, [0x22; X11_COOKIE_BYTES]));
        let error = select_x11_cookie(&database, &display).unwrap_err();
        assert_eq!(error.kind(), crate::error::MezErrorKind::Conflict);
        assert_eq!(
            error.to_string(),
            "Conflict: multiple distinct X11 authority credentials match the local display"
        );
        assert!(!error.to_string().contains("1111111111111111"));
        assert!(!error.to_string().contains("2222222222222222"));

        assert!(select_x11_cookie(&database[..8], &display).is_err());
    }

    /// Local xauth canonicalizes loopback TCP displays to FamilyLocal records,
    /// so trusted lookup must accept that selector without broadening dialing.
    #[test]
    fn selects_family_local_records_for_loopback_tcp_displays() {
        for display_name in ["localhost:18", "127.0.0.1:19", "[::1]:20"] {
            let display = resolve_local_x11_display(display_name).unwrap();
            let database = authority_record(&display, [0x33; X11_COOKIE_BYTES]);

            let selected = select_x11_cookie(&database, &display).unwrap();

            assert_eq!(selected, X11Cookie::new([0x33; X11_COOKIE_BYTES]));
        }
    }

    /// A user-selected non-loopback server must use its resolved Internet
    /// selector rather than leaking a separate FamilyLocal credential.
    #[test]
    fn selects_and_seeds_nonlocal_tcp_authority_records() {
        let display = resolve_local_x11_display("172.26.128.1:0").unwrap();
        let database = authority_record(&display, [0x44; X11_COOKIE_BYTES]);

        let selected = select_x11_cookie(&database, &display).unwrap();
        let seed = encode_seed_authority(&display, &selected).unwrap();

        assert_eq!(selected, X11Cookie::new([0x44; X11_COOKIE_BYTES]));
        assert_eq!(
            u16::from_be_bytes([seed[0], seed[1]]),
            XAUTH_FAMILY_INTERNET
        );
        assert_eq!(&seed[4..8], &[172, 26, 128, 1]);
    }

    /// The untrusted adapter must use argv-only generation against a private
    /// authority copy, return a distinct cookie, and remove lease artifacts.
    #[tokio::test]
    async fn generates_and_cleans_private_untrusted_credentials() {
        let root = test_root("generate");
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let display = resolve_local_x11_display(":17").unwrap();
        let source = root.join("source");
        write_private_file(
            &source,
            &authority_record(&display, [0x11; X11_COOKIE_BYTES]),
        )
        .unwrap();
        let generated = root.join("generated");
        write_private_file(
            &generated,
            &authority_record(&display, [0x22; X11_COOKIE_BYTES]),
        )
        .unwrap();
        let script = root.join("fake-xauth");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\n[ \"$XAUTHORITY\" = \"$4\" ] || exit 3\nif [ \"$5\" = generate ]; then cp '{}' \"$4\"; exit 0; fi\nif [ \"$5\" = remove ]; then : > \"$4\"; exit 0; fi\nexit 2\n",
                generated.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();

        let (cookie, mut lease) = generate_untrusted_x11_cookie(
            &source,
            &display,
            script.as_os_str(),
            Duration::from_secs(2),
        )
        .await
        .unwrap();

        assert_eq!(cookie, X11Cookie::new([0x22; X11_COOKIE_BYTES]));
        let lease_directory = lease.directory().to_path_buf();
        assert!(lease_directory.is_dir());
        assert_eq!(
            fs::metadata(lease_directory.join("authority"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        lease.close().await.unwrap();
        assert!(!lease_directory.exists());
        let _ = fs::remove_dir_all(root);
    }

    /// Nonzero xauth exit must remain a distinct bounded setup rejection.
    #[tokio::test]
    async fn rejects_nonzero_xauth_exit() {
        let root = test_root("nonzero");
        fs::create_dir_all(&root).unwrap();
        let authority = root.join("authority");
        write_private_file(&authority, &[]).unwrap();
        let script = root.join("fake-xauth");
        fs::write(&script, "#!/bin/sh\nexit 7\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();

        let error = run_xauth(script.as_os_str(), &authority, &[], Duration::from_secs(1))
            .await
            .unwrap_err();

        assert!(error.message().contains("rejected"), "{error:?}");
        let _ = fs::remove_dir_all(root);
    }

    /// One absolute deadline must cover normal execution, kill signalling,
    /// and reap so a non-cooperating helper cannot keep the client alive.
    #[tokio::test]
    async fn xauth_timeout_bounds_termination_and_reap() {
        let root = test_root("timeout");
        fs::create_dir_all(&root).unwrap();
        let authority = root.join("authority");
        write_private_file(&authority, &[]).unwrap();
        let script = root.join("fake-xauth");
        fs::write(
            &script,
            "#!/bin/sh\ntrap '' TERM\nwhile :; do sleep 1; done\n",
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let budget = Duration::from_millis(200);
        let started = tokio::time::Instant::now();

        let error = run_xauth(script.as_os_str(), &authority, &[], budget)
            .await
            .unwrap_err();

        assert!(error.message().contains("timed out"), "{error:?}");
        assert!(
            started.elapsed() <= Duration::from_millis(500),
            "xauth timeout recovery exceeded one lifecycle budget: {:?}",
            started.elapsed()
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Timed-out explicit cleanup must still remove private credential files
    /// before returning the bounded xauth error.
    #[tokio::test]
    async fn timed_out_untrusted_cleanup_removes_private_artifacts() {
        let root = test_root("cleanup-timeout");
        fs::create_dir_all(&root).unwrap();
        let authority_path = root.join("authority");
        write_private_file(&authority_path, &[]).unwrap();
        let script = root.join("fake-xauth");
        fs::write(
            &script,
            "#!/bin/sh\ntrap '' TERM\nwhile :; do sleep 1; done\n",
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let mut lease = UntrustedX11CredentialLease {
            cleanup: Some(XauthCleanup {
                executable: script.as_os_str().to_os_string(),
                authority_path,
                directory: root.clone(),
                display_name: ":17".to_string(),
                command_timeout: Duration::from_millis(200),
            }),
        };

        let error = lease.close().await.unwrap_err();

        assert!(error.message().contains("timed out"), "{error:?}");
        assert!(!root.exists());
    }

    /// Builds one exact endpoint-matching Xauthority record for tests.
    fn authority_record(display: &ResolvedX11Display, cookie: [u8; X11_COOKIE_BYTES]) -> Vec<u8> {
        let (family, address) = display.seed_authority_selector();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&family.to_be_bytes());
        append_counted(&mut bytes, address);
        append_counted(&mut bytes, display.display_number().to_string().as_bytes());
        append_counted(&mut bytes, X11_AUTH_PROTOCOL_NAME.as_bytes());
        append_counted(&mut bytes, &cookie);
        bytes
    }

    /// Appends one test authority field.
    fn append_counted(target: &mut Vec<u8>, field: &[u8]) {
        target.extend_from_slice(&u16::try_from(field.len()).unwrap().to_be_bytes());
        target.extend_from_slice(field);
    }

    /// Allocates one process-local private test root.
    fn test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "mez-cli-x11-authority-{name}-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ))
    }
}
