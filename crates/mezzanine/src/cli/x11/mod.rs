//! Attach-lifetime client preparation for X11 forwarding over Iroh.
//!
//! Preparation happens before network initialization. It freezes a local-only
//! X target, acquires an explicit trusted or short-lived untrusted credential,
//! generates a separate fake route cookie, and owns bounded cleanup. The real
//! cookie, display name, authority path, and local destination are absent from
//! all network-facing contracts.

use std::ffi::OsStr;
use std::fmt;
use std::path::Path;
use std::time::Duration;

use crate::error::{MezError, Result};
use crate::runtime::x11::{
    X11_FORWARDING_VERSION, X11AuthProtocol, X11Cookie, X11ForwardingMode, X11ForwardingOffer,
};

mod authority;
mod display;
mod forwarder;

pub(crate) use display::{ResolvedX11Display, resolve_local_x11_display};
pub(crate) use forwarder::{X11LocalStream, connect_local_x11, rewrite_local_x11_setup};

use authority::{
    X11CredentialLease, generate_untrusted_x11_cookie, load_trusted_x11_cookie,
    process_xauthority_path,
};

/// Fully prepared client-local state for one requested X11 attachment.
pub(crate) struct PreparedX11Client {
    mode: X11ForwardingMode,
    display: ResolvedX11Display,
    fake_cookie: X11Cookie,
    real_cookie: X11Cookie,
    lease: X11CredentialLease,
}

/// Cloneable credential boundary used by supervised X11 stream workers.
#[derive(Clone)]
pub(crate) struct X11ClientForwarder {
    display: ResolvedX11Display,
    fake_cookie: X11Cookie,
    real_cookie: X11Cookie,
}

impl fmt::Debug for X11ClientForwarder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("X11ClientForwarder")
            .field("display", &"[LOCAL DISPLAY REDACTED]")
            .field("fake_cookie", &"[REDACTED]")
            .field("real_cookie", &"[REDACTED]")
            .finish()
    }
}

impl X11ClientForwarder {
    /// Connects one forwarding stream only to the frozen local target.
    pub(crate) async fn connect(&self, timeout: Duration) -> Result<X11LocalStream> {
        connect_local_x11(&self.display, timeout).await
    }

    /// Revalidates and substitutes the setup cookie before local delivery.
    pub(crate) fn rewrite_setup(
        &self,
        setup: &mut [u8],
    ) -> Result<crate::runtime::x11::X11SetupPacket> {
        rewrite_local_x11_setup(setup, &self.fake_cookie, &self.real_cookie)
    }

    /// Builds an explicit frozen forwarding boundary for relay tests.
    #[cfg(test)]
    pub(crate) fn new_for_test(
        display: ResolvedX11Display,
        fake_cookie: X11Cookie,
        real_cookie: X11Cookie,
    ) -> Self {
        Self {
            display,
            fake_cookie,
            real_cookie,
        }
    }
}

impl fmt::Debug for PreparedX11Client {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedX11Client")
            .field("mode", &self.mode)
            .field("display", &"[LOCAL DISPLAY REDACTED]")
            .field("fake_cookie", &"[REDACTED]")
            .field("real_cookie", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl PreparedX11Client {
    /// Builds the network-safe initialize offer for this prepared state.
    pub(crate) fn offer(&self, takeover: bool) -> X11ForwardingOffer {
        X11ForwardingOffer {
            version: X11_FORWARDING_VERSION,
            mode: self.mode,
            auth_protocol: X11AuthProtocol::MitMagicCookie1,
            fake_cookie: self.fake_cookie.clone(),
            takeover,
        }
    }

    /// Clones only the frozen local forwarding state, never the cleanup lease.
    pub(crate) fn forwarder(&self) -> X11ClientForwarder {
        X11ClientForwarder {
            display: self.display.clone(),
            fake_cookie: self.fake_cookie.clone(),
            real_cookie: self.real_cookie.clone(),
        }
    }

    /// Connects one forwarding stream only to the frozen local target.
    pub(crate) async fn connect(&self, timeout: Duration) -> Result<X11LocalStream> {
        self.forwarder().connect(timeout).await
    }

    /// Revalidates and substitutes the setup cookie before local delivery.
    pub(crate) fn rewrite_setup(
        &self,
        setup: &mut [u8],
    ) -> Result<crate::runtime::x11::X11SetupPacket> {
        self.forwarder().rewrite_setup(setup)
    }

    /// Performs bounded explicit credential cleanup.
    pub(crate) async fn close(mut self) -> Result<()> {
        self.lease.close().await
    }
}

/// Prepares trusted or untrusted X11 state from the current process
/// environment before any Iroh connection is opened.
pub(crate) async fn prepare_x11_client(mode: X11ForwardingMode) -> Result<PreparedX11Client> {
    let display = std::env::var("DISPLAY")
        .ok()
        .filter(|display| !display.is_empty())
        .ok_or_else(|| MezError::invalid_state("DISPLAY is unavailable for X11 forwarding"))?;
    let authority_path = process_xauthority_path()?;
    prepare_x11_client_with(
        mode,
        &display,
        &authority_path,
        OsStr::new("xauth"),
        Duration::from_secs(5),
    )
    .await
}

/// Testable preparation boundary with explicit local process inputs.
async fn prepare_x11_client_with(
    mode: X11ForwardingMode,
    display_name: &str,
    authority_path: &Path,
    xauth_executable: &OsStr,
    command_timeout: Duration,
) -> Result<PreparedX11Client> {
    let display = resolve_local_x11_display(display_name)?;
    let (real_cookie, lease) = match mode {
        X11ForwardingMode::Trusted => (
            load_trusted_x11_cookie(authority_path, &display)?,
            X11CredentialLease::Trusted,
        ),
        X11ForwardingMode::Untrusted => {
            let (cookie, lease) = generate_untrusted_x11_cookie(
                authority_path,
                &display,
                xauth_executable,
                command_timeout,
            )
            .await?;
            (cookie, X11CredentialLease::Untrusted(lease))
        }
    };
    Ok(PreparedX11Client {
        mode,
        display,
        fake_cookie: X11Cookie::random(),
        real_cookie,
        lease,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    /// Trusted preparation must keep the real credential and local target out
    /// of Debug and the network-facing offer.
    #[tokio::test]
    async fn prepares_redacted_trusted_client_state() {
        let root = std::env::temp_dir().join(format!(
            "mez-cli-x11-prepare-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let authority_path = root.join("authority");
        let display = resolve_local_x11_display(":19").unwrap();
        let mut record = Vec::new();
        record.extend_from_slice(&display::XAUTH_FAMILY_LOCAL.to_be_bytes());
        append_counted(&mut record, display.authority_address());
        append_counted(&mut record, b"19");
        append_counted(
            &mut record,
            crate::runtime::x11::X11_AUTH_PROTOCOL_NAME.as_bytes(),
        );
        append_counted(&mut record, &[0x5a; 16]);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&authority_path)
            .unwrap();
        file.write_all(&record).unwrap();

        let prepared = prepare_x11_client_with(
            X11ForwardingMode::Trusted,
            ":19",
            &authority_path,
            OsStr::new("unused-xauth"),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        let offer = prepared.offer(false);
        let debug = format!("{prepared:?}");

        assert_eq!(offer.mode, X11ForwardingMode::Trusted);
        assert!(!debug.contains(":19"));
        assert!(!debug.contains("5a"));
        prepared.close().await.unwrap();
        let _ = fs::remove_dir_all(root);
    }

    /// Prepared state must independently validate and rewrite both X11 byte
    /// orders without changing packet length or exposing the real credential.
    #[test]
    fn prepared_state_rewrites_both_setup_byte_orders() {
        for byte_order in *b"lB" {
            let prepared = PreparedX11Client {
                mode: X11ForwardingMode::Trusted,
                display: resolve_local_x11_display(":21").unwrap(),
                fake_cookie: X11Cookie::new([0x41; 16]),
                real_cookie: X11Cookie::new([0x52; 16]),
                lease: X11CredentialLease::Trusted,
            };
            let mut setup = setup_packet(byte_order, prepared.fake_cookie.as_bytes());
            let original_len = setup.len();

            let packet = prepared.rewrite_setup(&mut setup).unwrap();

            assert_eq!(setup.len(), original_len);
            assert_eq!(&setup[packet.auth_data_range], &[0x52; 16]);
        }
    }

    /// Builds one exact MIT setup request with the selected byte order.
    fn setup_packet(byte_order: u8, cookie: &[u8; 16]) -> Vec<u8> {
        let mut setup = vec![0u8; 48];
        setup[0] = byte_order;
        let encode = |value: u16| {
            if byte_order == b'l' {
                value.to_le_bytes()
            } else {
                value.to_be_bytes()
            }
        };
        setup[2..4].copy_from_slice(&encode(11));
        setup[4..6].copy_from_slice(&encode(0));
        setup[6..8].copy_from_slice(&encode(18));
        setup[8..10].copy_from_slice(&encode(16));
        setup[12..30].copy_from_slice(b"MIT-MAGIC-COOKIE-1");
        setup[32..48].copy_from_slice(cookie);
        setup
    }

    /// Appends one counted authority field for preparation tests.
    fn append_counted(target: &mut Vec<u8>, field: &[u8]) {
        target.extend_from_slice(&u16::try_from(field.len()).unwrap().to_be_bytes());
        target.extend_from_slice(field);
    }
}
