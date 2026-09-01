//! Versioned X11 negotiation and Iroh stream-preface contracts.
//!
//! Secret-bearing values use fixed-size storage, redact `Debug`, compare in
//! constant time, and zero their bytes on drop. The real local X credential is
//! deliberately absent from every network-facing type in this module.

use std::fmt;

use rand::Rng;
use zeroize::{Zeroize, Zeroizing};

/// Initial X11-over-Iroh contract version.
pub(crate) const X11_FORWARDING_VERSION: u8 = 2;
/// Exact X11 authorization protocol accepted by the forwarding boundary.
pub(crate) const X11_AUTH_PROTOCOL_NAME: &str = "MIT-MAGIC-COOKIE-1";
/// Required MIT-MAGIC-COOKIE-1 credential length.
pub(crate) const X11_COOKIE_BYTES: usize = 16;
/// Route-token length used to authenticate server-opened X11 streams.
pub(crate) const X11_ROUTE_TOKEN_BYTES: usize = 32;
/// Encoded fixed stream-preface length.
pub(crate) const X11_STREAM_PREFACE_BYTES: usize = 56;

/// Fixed stream magic, separate from every control/event framing prefix.
const X11_STREAM_MAGIC: [u8; 8] = *b"MZX11STR";
/// Reserved bytes in the v2 preface that must remain zero.
const X11_STREAM_RESERVED_BYTES: usize = 7;

/// Supported X11 authorization protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum X11AuthProtocol {
    /// The only credential protocol supported by the forwarding boundary.
    MitMagicCookie1,
}

impl X11AuthProtocol {
    /// Returns the exact X11 wire name for this authorization protocol.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::MitMagicCookie1 => X11_AUTH_PROTOCOL_NAME,
        }
    }
}

/// Client-selected X11 credential trust mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum X11ForwardingMode {
    /// Short-lived X SECURITY credential with untrusted restrictions.
    Untrusted,
    /// Existing full local X credential, permitted only by explicit host policy.
    Trusted,
}

impl X11ForwardingMode {
    /// Returns the stable control-protocol name for this mode.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Untrusted => "untrusted",
            Self::Trusted => "trusted",
        }
    }
}

/// Redacted fixed-size MIT-MAGIC-COOKIE-1 credential.
pub(crate) struct X11Cookie([u8; X11_COOKIE_BYTES]);

impl X11Cookie {
    /// Wraps exactly one 16-byte X11 credential.
    pub(crate) const fn new(bytes: [u8; X11_COOKIE_BYTES]) -> Self {
        Self(bytes)
    }

    /// Generates a fresh credential using the process cryptographic RNG.
    pub(crate) fn random() -> Self {
        let mut bytes = [0u8; X11_COOKIE_BYTES];
        rand::rng().fill_bytes(&mut bytes);
        Self(bytes)
    }

    /// Borrows the credential for bounded protocol operations.
    pub(crate) const fn as_bytes(&self) -> &[u8; X11_COOKIE_BYTES] {
        &self.0
    }
}

impl Clone for X11Cookie {
    fn clone(&self) -> Self {
        Self(self.0)
    }
}

impl fmt::Debug for X11Cookie {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("X11Cookie([REDACTED])")
    }
}

impl PartialEq for X11Cookie {
    fn eq(&self, other: &Self) -> bool {
        constant_time_eq(&self.0, &other.0)
    }
}

impl Eq for X11Cookie {}

impl Drop for X11Cookie {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Redacted random token binding X11 streams to one route generation.
pub(crate) struct X11RouteToken([u8; X11_ROUTE_TOKEN_BYTES]);

impl X11RouteToken {
    /// Wraps exactly one 32-byte route token.
    pub(crate) const fn new(bytes: [u8; X11_ROUTE_TOKEN_BYTES]) -> Self {
        Self(bytes)
    }

    /// Generates a fresh route token using the process cryptographic RNG.
    pub(crate) fn random() -> Self {
        let mut bytes = [0u8; X11_ROUTE_TOKEN_BYTES];
        rand::rng().fill_bytes(&mut bytes);
        Self(bytes)
    }

    /// Borrows the token for preface encoding and validation.
    pub(crate) const fn as_bytes(&self) -> &[u8; X11_ROUTE_TOKEN_BYTES] {
        &self.0
    }
}

impl Clone for X11RouteToken {
    fn clone(&self) -> Self {
        Self(self.0)
    }
}

impl fmt::Debug for X11RouteToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("X11RouteToken([REDACTED])")
    }
}

impl PartialEq for X11RouteToken {
    fn eq(&self, other: &Self) -> bool {
        constant_time_eq(&self.0, &other.0)
    }
}

impl Eq for X11RouteToken {}

impl Drop for X11RouteToken {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Versioned client offer carried by `control/initialize` integration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct X11ForwardingOffer {
    /// Exact forwarding protocol version requested by the client.
    pub(crate) version: u8,
    /// Requested trusted or untrusted credential mode.
    pub(crate) mode: X11ForwardingMode,
    /// Exact authorization protocol; v2 accepts only MIT-MAGIC-COOKIE-1.
    pub(crate) auth_protocol: X11AuthProtocol,
    /// Client-generated fake credential published only on the server proxy.
    pub(crate) fake_cookie: X11Cookie,
    /// Whether an existing owner may be explicitly displaced.
    pub(crate) takeover: bool,
}

/// Successful route result returned by initialization integration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct X11ForwardingResult {
    /// Negotiated forwarding protocol version.
    pub(crate) version: u8,
    /// Negotiated credential trust mode.
    pub(crate) mode: X11ForwardingMode,
    /// Monotonic session-local route generation.
    pub(crate) generation: u64,
    /// Random token authenticating streams for this exact generation.
    pub(crate) route_token: X11RouteToken,
}

/// Fixed preface sent before negotiated X11 records on each Iroh stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct X11StreamPreface {
    /// Monotonic route generation expected by the attaching client.
    pub(crate) generation: u64,
    /// Random token for the exact route generation.
    pub(crate) route_token: X11RouteToken,
}

impl X11StreamPreface {
    /// Encodes the fixed v2 preface without exposing token material in text.
    pub(crate) fn encode(&self) -> [u8; X11_STREAM_PREFACE_BYTES] {
        let mut encoded = [0u8; X11_STREAM_PREFACE_BYTES];
        encoded[..8].copy_from_slice(&X11_STREAM_MAGIC);
        encoded[8] = X11_FORWARDING_VERSION;
        encoded[16..24].copy_from_slice(&self.generation.to_be_bytes());
        encoded[24..].copy_from_slice(self.route_token.as_bytes());
        encoded
    }

    /// Decodes and validates one exact fixed-size v2 stream preface.
    pub(crate) fn decode(encoded: &[u8]) -> Result<Self, X11StreamPrefaceError> {
        if encoded.len() != X11_STREAM_PREFACE_BYTES {
            return Err(X11StreamPrefaceError::InvalidLength);
        }
        if encoded[..8] != X11_STREAM_MAGIC {
            return Err(X11StreamPrefaceError::InvalidMagic);
        }
        if encoded[8] != X11_FORWARDING_VERSION {
            return Err(X11StreamPrefaceError::UnsupportedVersion);
        }
        if encoded[9..9 + X11_STREAM_RESERVED_BYTES]
            .iter()
            .any(|byte| *byte != 0)
        {
            return Err(X11StreamPrefaceError::NonZeroReserved);
        }
        let generation = u64::from_be_bytes(
            encoded[16..24]
                .try_into()
                .map_err(|_| X11StreamPrefaceError::InvalidLength)?,
        );
        let token: Zeroizing<[u8; X11_ROUTE_TOKEN_BYTES]> = Zeroizing::new(
            encoded[24..]
                .try_into()
                .map_err(|_| X11StreamPrefaceError::InvalidLength)?,
        );
        Ok(Self {
            generation,
            route_token: X11RouteToken::new(*token),
        })
    }
}

/// Privacy-safe rejection classes for malformed X11 stream prefaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum X11StreamPrefaceError {
    /// The stream did not provide exactly one complete fixed preface.
    #[error("invalid X11 stream preface length")]
    InvalidLength,
    /// The stream does not use the Mezzanine X11 stream family.
    #[error("invalid X11 stream preface magic")]
    InvalidMagic,
    /// The stream requests an unsupported preface version.
    #[error("unsupported X11 stream preface version")]
    UnsupportedVersion,
    /// Reserved v1 bytes were nonzero.
    #[error("invalid X11 stream preface reserved bytes")]
    NonZeroReserved,
}

/// Compares equal-length secret bytes without data-dependent early return.
fn constant_time_eq<const N: usize>(left: &[u8; N], right: &[u8; N]) -> bool {
    left.iter()
        .zip(right)
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stream preface must round-trip its generation and secret token while
    /// keeping both secret wrapper debug representations redacted.
    #[test]
    fn stream_preface_round_trips_and_redacts_secrets() {
        let token = X11RouteToken::new([0x5a; X11_ROUTE_TOKEN_BYTES]);
        let preface = X11StreamPreface {
            generation: 42,
            route_token: token.clone(),
        };

        let decoded = X11StreamPreface::decode(&preface.encode()).unwrap();

        assert_eq!(decoded, preface);
        assert!(!format!("{token:?}").contains("5a"));
        assert!(!format!("{:?}", X11Cookie::new([0xa5; X11_COOKIE_BYTES])).contains("a5"));
    }

    /// Malformed magic, versions, reserved bytes, and lengths must produce
    /// reason classes without including route-token material.
    #[test]
    fn stream_preface_rejects_malformed_inputs_without_secret_text() {
        let preface = X11StreamPreface {
            generation: 7,
            route_token: X11RouteToken::new([0x33; X11_ROUTE_TOKEN_BYTES]),
        };
        let mut encoded = preface.encode();
        encoded[0] ^= 1;
        assert_eq!(
            X11StreamPreface::decode(&encoded),
            Err(X11StreamPrefaceError::InvalidMagic)
        );

        let mut encoded = preface.encode();
        encoded[8] = 1;
        assert_eq!(
            X11StreamPreface::decode(&encoded),
            Err(X11StreamPrefaceError::UnsupportedVersion)
        );

        let mut encoded = preface.encode();
        encoded[9] = 1;
        assert_eq!(
            X11StreamPreface::decode(&encoded),
            Err(X11StreamPrefaceError::NonZeroReserved)
        );
        assert_eq!(
            X11StreamPreface::decode(&encoded[..12]),
            Err(X11StreamPrefaceError::InvalidLength)
        );
        assert!(
            !X11StreamPrefaceError::InvalidMagic
                .to_string()
                .contains("33")
        );
    }
}
