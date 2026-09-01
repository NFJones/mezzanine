//! Strict client-local X11 display parsing and target freezing.
//!
//! Only conventional local Unix displays, constrained XQuartz launchd
//! sockets, and numeric loopback TCP endpoints are accepted. Parsing resolves
//! the complete destination before any Iroh connection is attempted; no
//! server-provided value can influence the local X target.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Component, Path, PathBuf};

use crate::error::{MezError, Result};

/// Xauthority family for an IPv4 endpoint.
pub(super) const XAUTH_FAMILY_INTERNET: u16 = 0;
/// Xauthority family for an IPv6 endpoint.
pub(super) const XAUTH_FAMILY_INTERNET6: u16 = 6;
/// Xauthority family for a local Unix endpoint.
pub(super) const XAUTH_FAMILY_LOCAL: u16 = 256;
/// Base TCP port assigned to X display zero.
const X11_TCP_BASE_PORT: u16 = 6000;

/// One frozen local socket destination.
#[derive(Clone, PartialEq, Eq)]
pub(super) enum X11LocalTarget {
    /// Conventional or XQuartz Unix-domain socket.
    Unix(PathBuf),
    /// Numeric loopback TCP endpoint.
    Tcp(SocketAddr),
}

impl fmt::Debug for X11LocalTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("X11LocalTarget([LOCAL TARGET REDACTED])")
    }
}

/// Parsed local display with the exact Xauthority selector for its target.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ResolvedX11Display {
    display_name: String,
    display_number: u16,
    screen_number: u16,
    target: X11LocalTarget,
    authority_family: u16,
    authority_address: Vec<u8>,
    local_authority_address: Vec<u8>,
}

impl fmt::Debug for ResolvedX11Display {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ResolvedX11Display([LOCAL DISPLAY REDACTED])")
    }
}

impl ResolvedX11Display {
    /// Returns the original validated display name for local `xauth` use.
    pub(super) fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Returns the decimal display number used by Xauthority records.
    pub(super) const fn display_number(&self) -> u16 {
        self.display_number
    }

    /// Borrows the exact binary address used for Xauthority selection.
    pub(super) fn authority_address(&self) -> &[u8] {
        &self.authority_address
    }

    /// Borrows the canonical FamilyLocal hostname selector used by xauth.
    pub(super) fn local_authority_address(&self) -> &[u8] {
        &self.local_authority_address
    }

    /// Returns the parsed screen number without changing the socket target.
    pub(crate) const fn screen_number(&self) -> u16 {
        self.screen_number
    }

    /// Borrows the frozen local socket target.
    pub(super) const fn target(&self) -> &X11LocalTarget {
        &self.target
    }

    /// Matches one exact Xauthority endpoint selector.
    pub(super) fn matches_authority(&self, family: u16, address: &[u8], number: &[u8]) -> bool {
        let endpoint_matches = (family == self.authority_family
            && address == self.authority_address)
            || (family == XAUTH_FAMILY_LOCAL && address == self.local_authority_address);
        endpoint_matches && number == self.display_number.to_string().as_bytes()
    }
}

/// Parses and freezes one supported local DISPLAY value.
pub(crate) fn resolve_local_x11_display(display: &str) -> Result<ResolvedX11Display> {
    if display.is_empty() || display.chars().any(char::is_control) {
        return Err(invalid_display());
    }
    let (host, suffix) = split_host_and_suffix(display)?;
    let (display_number, screen_number) = parse_display_suffix(suffix)?;
    let port = X11_TCP_BASE_PORT
        .checked_add(display_number)
        .ok_or_else(invalid_display)?;
    let local_authority_address = current_hostname()?;

    let (target, authority_family, authority_address) = match host {
        "" | "unix" => (
            X11LocalTarget::Unix(PathBuf::from(format!("/tmp/.X11-unix/X{display_number}"))),
            XAUTH_FAMILY_LOCAL,
            local_authority_address.clone(),
        ),
        "localhost" | "127.0.0.1" => (
            X11LocalTarget::Tcp(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)),
            XAUTH_FAMILY_INTERNET,
            Ipv4Addr::LOCALHOST.octets().to_vec(),
        ),
        "::1" => (
            X11LocalTarget::Tcp(SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port)),
            XAUTH_FAMILY_INTERNET6,
            Ipv6Addr::LOCALHOST.octets().to_vec(),
        ),
        path if path.starts_with('/') && valid_xquartz_socket_path(Path::new(path)) => (
            X11LocalTarget::Unix(PathBuf::from(path)),
            XAUTH_FAMILY_LOCAL,
            local_authority_address.clone(),
        ),
        _ => return Err(invalid_display()),
    };

    Ok(ResolvedX11Display {
        display_name: display.to_string(),
        display_number,
        screen_number,
        target,
        authority_family,
        authority_address,
        local_authority_address,
    })
}

/// Splits bracketed IPv6 and ordinary display forms at the display suffix.
fn split_host_and_suffix(display: &str) -> Result<(&str, &str)> {
    if let Some(rest) = display.strip_prefix('[') {
        let (host, suffix) = rest.split_once("]:").ok_or_else(invalid_display)?;
        if host != "::1" {
            return Err(invalid_display());
        }
        return Ok((host, suffix));
    }
    display.rsplit_once(':').ok_or_else(invalid_display)
}

/// Parses `display[.screen]` with a port-safe display number.
fn parse_display_suffix(suffix: &str) -> Result<(u16, u16)> {
    let (display, screen) = match suffix.split_once('.') {
        Some((display, screen)) if !screen.contains('.') => (display, screen),
        Some(_) => return Err(invalid_display()),
        None => (suffix, "0"),
    };
    if display.is_empty()
        || screen.is_empty()
        || !display.bytes().all(|byte| byte.is_ascii_digit())
        || !screen.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid_display());
    }
    let display_number = display.parse::<u16>().map_err(|_| invalid_display())?;
    let screen_number = screen.parse::<u16>().map_err(|_| invalid_display())?;
    X11_TCP_BASE_PORT
        .checked_add(display_number)
        .ok_or_else(invalid_display)?;
    Ok((display_number, screen_number))
}

/// Restricts launchd socket values to XQuartz's local private-temp shape.
fn valid_xquartz_socket_path(path: &Path) -> bool {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
        || path.file_name().and_then(|name| name.to_str()) != Some("org.xquartz")
    {
        return false;
    }
    let Some(launchd_directory) = path.parent() else {
        return false;
    };
    if !launchd_directory
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("com.apple.launchd.") && name.len() > 19)
    {
        return false;
    }
    matches!(
        launchd_directory.parent().and_then(Path::to_str),
        Some("/private/tmp" | "/tmp")
    )
}

/// Reads the local hostname used by FamilyLocal Xauthority records.
fn current_hostname() -> Result<Vec<u8>> {
    let mut hostname = [0u8; 256];
    // SAFETY: `hostname` is writable for its full declared length and remains
    // alive for the duration of the libc call.
    let result =
        unsafe { libc::gethostname(hostname.as_mut_ptr().cast::<libc::c_char>(), hostname.len()) };
    if result != 0 {
        return Err(MezError::invalid_state(
            "local hostname is unavailable for X11 authority matching",
        ));
    }
    let length = hostname
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(|| MezError::invalid_state("local hostname exceeds the X11 match limit"))?;
    if length == 0 {
        return Err(MezError::invalid_state(
            "local hostname is empty for X11 authority matching",
        ));
    }
    Ok(hostname[..length].to_vec())
}

/// Returns one privacy-safe DISPLAY parse failure.
fn invalid_display() -> MezError {
    MezError::invalid_args("DISPLAY is not a supported local X11 endpoint")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Conventional Unix, loopback TCP, and constrained XQuartz display names
    /// must resolve without retaining an arbitrary hostname or socket path.
    #[test]
    fn resolves_supported_local_display_forms() {
        let unix = resolve_local_x11_display(":7.2").unwrap();
        assert_eq!(unix.display_number(), 7);
        assert_eq!(unix.screen_number(), 2);
        assert!(
            matches!(unix.target(), X11LocalTarget::Unix(path) if path == Path::new("/tmp/.X11-unix/X7"))
        );

        let named_unix = resolve_local_x11_display("unix:8").unwrap();
        assert!(matches!(named_unix.target(), X11LocalTarget::Unix(_)));

        let ipv4 = resolve_local_x11_display("localhost:9.0").unwrap();
        assert!(
            matches!(ipv4.target(), X11LocalTarget::Tcp(address) if *address == "127.0.0.1:6009".parse().unwrap())
        );

        let ipv6 = resolve_local_x11_display("[::1]:10").unwrap();
        assert!(
            matches!(ipv6.target(), X11LocalTarget::Tcp(address) if *address == "[::1]:6010".parse().unwrap())
        );

        let xquartz =
            resolve_local_x11_display("/private/tmp/com.apple.launchd.ABC123/org.xquartz:0")
                .unwrap();
        assert!(
            matches!(xquartz.target(), X11LocalTarget::Unix(path) if path == Path::new("/private/tmp/com.apple.launchd.ABC123/org.xquartz"))
        );
        assert!(!format!("{xquartz:?}").contains("launchd"));
    }

    /// Remote hosts, arbitrary paths, malformed suffixes, and port overflow
    /// must fail with a target-free diagnostic.
    #[test]
    fn rejects_nonlocal_or_malformed_display_forms() {
        for display in [
            "example.com:0",
            "192.0.2.1:0",
            "[2001:db8::1]:0",
            "/tmp/arbitrary:0",
            "/private/tmp/com.apple.launchd.ABC/other:0",
            ":",
            ":1.",
            ":1.2.3",
            ":59536",
            "tcp/localhost:0",
        ] {
            let error = resolve_local_x11_display(display).unwrap_err();
            assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
            assert!(
                error
                    .to_string()
                    .ends_with("DISPLAY is not a supported local X11 endpoint")
            );
            assert_eq!(
                error.to_string(),
                "InvalidArgs: DISPLAY is not a supported local X11 endpoint"
            );
        }
    }
}
