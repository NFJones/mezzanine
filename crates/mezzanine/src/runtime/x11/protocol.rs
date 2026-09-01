//! Strict bounded parsing for the X11 client setup request.
//!
//! The parser supports both X11 byte orders, requires protocol 11.0 and an
//! exact 16-byte MIT-MAGIC-COOKIE-1 credential, uses checked padding
//! arithmetic, and reports incomplete input without allocating from untrusted
//! lengths. Callers enforce the configured setup deadline while receiving an
//! `Incomplete` result.

use std::ops::Range;

use super::contracts::{X11_AUTH_PROTOCOL_NAME, X11_COOKIE_BYTES, X11Cookie};

/// X11 protocol major version accepted by the forwarding proxy.
pub(crate) const X11_PROTOCOL_MAJOR: u16 = 11;
/// X11 protocol minor version accepted by the forwarding proxy.
pub(crate) const X11_PROTOCOL_MINOR: u16 = 0;
/// Maximum complete X11 setup request accepted before raw relay begins.
pub(crate) const X11_MAX_SETUP_BYTES: usize = 4 * 1024;
/// Fixed X11 setup request header length.
const X11_SETUP_HEADER_BYTES: usize = 12;

/// X11 setup integer byte order selected by the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum X11ByteOrder {
    /// Least-significant byte first (`l`).
    LittleEndian,
    /// Most-significant byte first (`B`).
    BigEndian,
}

/// Validated boundaries of one complete X11 setup request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct X11SetupPacket {
    /// Byte order used by all setup integer fields.
    pub(crate) byte_order: X11ByteOrder,
    /// Complete padded setup request length; later bytes are application data.
    pub(crate) packet_len: usize,
    /// Exact unpadded authorization protocol name range.
    pub(crate) auth_name_range: Range<usize>,
    /// Exact unpadded authorization credential range.
    pub(crate) auth_data_range: Range<usize>,
}

/// Incremental result for a bounded X11 setup buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum X11SetupProgress {
    /// More bytes are required; the caller may continue only until its deadline.
    Incomplete {
        /// Total bytes required to complete the current parse stage.
        required_len: usize,
    },
    /// One complete validated setup request is available at the buffer prefix.
    Complete(X11SetupPacket),
}

/// Privacy-safe X11 setup rejection classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum X11SetupError {
    /// The setup byte-order marker is neither `l` nor `B`.
    #[error("invalid X11 setup byte order")]
    InvalidByteOrder,
    /// Reserved setup bytes are nonzero.
    #[error("invalid X11 setup reserved bytes")]
    NonZeroReserved,
    /// The setup does not request protocol 11.0.
    #[error("unsupported X11 protocol version")]
    UnsupportedVersion,
    /// The setup lengths overflowed or exceed the fixed 4 KiB boundary.
    #[error("X11 setup packet exceeds the fixed limit")]
    Oversized,
    /// The authorization protocol is not exact MIT-MAGIC-COOKIE-1.
    #[error("unsupported X11 authorization protocol")]
    UnsupportedAuthProtocol,
    /// MIT-MAGIC-COOKIE-1 data is not exactly 16 bytes.
    #[error("invalid X11 authorization credential length")]
    InvalidCredentialLength,
    /// The setup credential does not authenticate the active route.
    #[error("invalid X11 authorization credential")]
    InvalidCredential,
    /// A complete setup packet was required but the input remains partial.
    #[error("incomplete X11 setup packet")]
    Incomplete,
}

/// Parses one setup request prefix without consuming any later X11 bytes.
pub(crate) fn parse_x11_setup(bytes: &[u8]) -> Result<X11SetupProgress, X11SetupError> {
    if bytes.len() < X11_SETUP_HEADER_BYTES {
        return Ok(X11SetupProgress::Incomplete {
            required_len: X11_SETUP_HEADER_BYTES,
        });
    }
    let byte_order = match bytes[0] {
        b'l' => X11ByteOrder::LittleEndian,
        b'B' => X11ByteOrder::BigEndian,
        _ => return Err(X11SetupError::InvalidByteOrder),
    };
    if bytes[1] != 0 || bytes[10] != 0 || bytes[11] != 0 {
        return Err(X11SetupError::NonZeroReserved);
    }
    let major = read_u16(byte_order, &bytes[2..4]);
    let minor = read_u16(byte_order, &bytes[4..6]);
    if major != X11_PROTOCOL_MAJOR || minor != X11_PROTOCOL_MINOR {
        return Err(X11SetupError::UnsupportedVersion);
    }
    let auth_name_len = usize::from(read_u16(byte_order, &bytes[6..8]));
    let auth_data_len = usize::from(read_u16(byte_order, &bytes[8..10]));
    let auth_name_padded = padded_len(auth_name_len)?;
    let auth_data_padded = padded_len(auth_data_len)?;
    let auth_name_start = X11_SETUP_HEADER_BYTES;
    let auth_name_end = auth_name_start
        .checked_add(auth_name_len)
        .ok_or(X11SetupError::Oversized)?;
    let auth_data_start = auth_name_start
        .checked_add(auth_name_padded)
        .ok_or(X11SetupError::Oversized)?;
    let auth_data_end = auth_data_start
        .checked_add(auth_data_len)
        .ok_or(X11SetupError::Oversized)?;
    let packet_len = auth_data_start
        .checked_add(auth_data_padded)
        .ok_or(X11SetupError::Oversized)?;
    if packet_len > X11_MAX_SETUP_BYTES {
        return Err(X11SetupError::Oversized);
    }
    if auth_name_len != X11_AUTH_PROTOCOL_NAME.len() {
        return Err(X11SetupError::UnsupportedAuthProtocol);
    }
    if auth_data_len != X11_COOKIE_BYTES {
        return Err(X11SetupError::InvalidCredentialLength);
    }
    if bytes.len() < packet_len {
        return Ok(X11SetupProgress::Incomplete {
            required_len: packet_len,
        });
    }
    if &bytes[auth_name_start..auth_name_end] != X11_AUTH_PROTOCOL_NAME.as_bytes() {
        return Err(X11SetupError::UnsupportedAuthProtocol);
    }
    Ok(X11SetupProgress::Complete(X11SetupPacket {
        byte_order,
        packet_len,
        auth_name_range: auth_name_start..auth_name_end,
        auth_data_range: auth_data_start..auth_data_end,
    }))
}

/// Validates the fake credential presented to the server-side proxy.
pub(crate) fn validate_x11_setup_cookie(
    bytes: &[u8],
    expected: &X11Cookie,
) -> Result<X11SetupPacket, X11SetupError> {
    let packet = require_complete(parse_x11_setup(bytes)?)?;
    let supplied: &[u8; X11_COOKIE_BYTES] = bytes[packet.auth_data_range.clone()]
        .try_into()
        .map_err(|_| X11SetupError::InvalidCredentialLength)?;
    if !constant_time_eq(supplied, expected.as_bytes()) {
        return Err(X11SetupError::InvalidCredential);
    }
    Ok(packet)
}

/// Replaces a validated fake credential with the client-local real credential.
pub(crate) fn rewrite_x11_setup_cookie(
    bytes: &mut [u8],
    expected_fake: &X11Cookie,
    real: &X11Cookie,
) -> Result<X11SetupPacket, X11SetupError> {
    let packet = validate_x11_setup_cookie(bytes, expected_fake)?;
    bytes[packet.auth_data_range.clone()].copy_from_slice(real.as_bytes());
    Ok(packet)
}

/// Converts one incremental result into the complete-packet contract.
fn require_complete(progress: X11SetupProgress) -> Result<X11SetupPacket, X11SetupError> {
    match progress {
        X11SetupProgress::Incomplete { .. } => Err(X11SetupError::Incomplete),
        X11SetupProgress::Complete(packet) => Ok(packet),
    }
}

/// Reads one setup integer according to the client byte-order marker.
fn read_u16(byte_order: X11ByteOrder, bytes: &[u8]) -> u16 {
    match byte_order {
        X11ByteOrder::LittleEndian => u16::from_le_bytes([bytes[0], bytes[1]]),
        X11ByteOrder::BigEndian => u16::from_be_bytes([bytes[0], bytes[1]]),
    }
}

/// Rounds an untrusted X11 field length to its four-byte wire boundary.
fn padded_len(length: usize) -> Result<usize, X11SetupError> {
    length
        .checked_add(3)
        .map(|length| length & !3)
        .ok_or(X11SetupError::Oversized)
}

/// Compares exact credential bytes without data-dependent early return.
fn constant_time_eq(left: &[u8; X11_COOKIE_BYTES], right: &[u8; X11_COOKIE_BYTES]) -> bool {
    left.iter()
        .zip(right)
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Little- and big-endian setup requests must produce identical validated
    /// boundaries and preserve bytes following the setup packet.
    #[test]
    fn parses_both_x11_byte_orders_and_retains_trailing_data() {
        let cookie = X11Cookie::new([0x11; X11_COOKIE_BYTES]);
        for order in [X11ByteOrder::LittleEndian, X11ByteOrder::BigEndian] {
            let mut setup = setup_packet(order, cookie.as_bytes());
            setup.extend_from_slice(b"application-data");

            let parsed = validate_x11_setup_cookie(&setup, &cookie).unwrap();

            assert_eq!(parsed.byte_order, order);
            assert_eq!(parsed.packet_len, 48);
            assert_eq!(&setup[parsed.packet_len..], b"application-data");
        }
    }

    /// Incremental parsing must request only the bounded header and calculated
    /// packet lengths so callers can enforce one setup deadline.
    #[test]
    fn reports_bounded_incomplete_setup_progress() {
        let setup = setup_packet(X11ByteOrder::LittleEndian, &[0x22; X11_COOKIE_BYTES]);

        assert_eq!(
            parse_x11_setup(&setup[..5]).unwrap(),
            X11SetupProgress::Incomplete { required_len: 12 }
        );
        assert_eq!(
            parse_x11_setup(&setup[..20]).unwrap(),
            X11SetupProgress::Incomplete { required_len: 48 }
        );
        assert_eq!(
            validate_x11_setup_cookie(&setup[..20], &X11Cookie::new([0x22; X11_COOKIE_BYTES])),
            Err(X11SetupError::Incomplete)
        );
    }

    /// Rewriting must be length-preserving, modify only the exact credential
    /// range, and reject a stale fake credential without disclosing it.
    #[test]
    fn rewrites_only_an_authenticated_cookie() {
        let fake = X11Cookie::new([0x33; X11_COOKIE_BYTES]);
        let real = X11Cookie::new([0x44; X11_COOKIE_BYTES]);
        let mut setup = setup_packet(X11ByteOrder::BigEndian, fake.as_bytes());
        let original_len = setup.len();

        let parsed = rewrite_x11_setup_cookie(&mut setup, &fake, &real).unwrap();

        assert_eq!(setup.len(), original_len);
        assert_eq!(&setup[parsed.auth_data_range], real.as_bytes());
        let error = validate_x11_setup_cookie(&setup, &fake).unwrap_err();
        assert_eq!(error, X11SetupError::InvalidCredential);
        assert!(!error.to_string().contains("33"));
    }

    /// Unsupported versions, reserved bytes, authorization names, cookie
    /// lengths, and oversized calculated packets must fail before relay.
    #[test]
    fn rejects_malformed_or_unsupported_setup_headers() {
        let valid = setup_packet(X11ByteOrder::LittleEndian, &[0x55; X11_COOKIE_BYTES]);

        let mut malformed = valid.clone();
        malformed[0] = b'?';
        assert_eq!(
            parse_x11_setup(&malformed),
            Err(X11SetupError::InvalidByteOrder)
        );
        let mut malformed = valid.clone();
        malformed[1] = 1;
        assert_eq!(
            parse_x11_setup(&malformed),
            Err(X11SetupError::NonZeroReserved)
        );
        let mut malformed = valid.clone();
        malformed[2..4].copy_from_slice(&12u16.to_le_bytes());
        assert_eq!(
            parse_x11_setup(&malformed),
            Err(X11SetupError::UnsupportedVersion)
        );
        let mut malformed = valid.clone();
        malformed[6..8].copy_from_slice(&17u16.to_le_bytes());
        assert_eq!(
            parse_x11_setup(&malformed),
            Err(X11SetupError::UnsupportedAuthProtocol)
        );
        let mut malformed = valid.clone();
        malformed[8..10].copy_from_slice(&15u16.to_le_bytes());
        assert_eq!(
            parse_x11_setup(&malformed),
            Err(X11SetupError::InvalidCredentialLength)
        );

        let mut oversized = valid;
        oversized[6..8].copy_from_slice(&4084u16.to_le_bytes());
        assert_eq!(parse_x11_setup(&oversized), Err(X11SetupError::Oversized));
    }

    /// Constructs one exact padded setup request for parser tests.
    fn setup_packet(order: X11ByteOrder, cookie: &[u8; X11_COOKIE_BYTES]) -> Vec<u8> {
        let mut bytes = vec![0u8; 48];
        bytes[0] = match order {
            X11ByteOrder::LittleEndian => b'l',
            X11ByteOrder::BigEndian => b'B',
        };
        write_u16(order, &mut bytes[2..4], X11_PROTOCOL_MAJOR);
        write_u16(order, &mut bytes[4..6], X11_PROTOCOL_MINOR);
        write_u16(
            order,
            &mut bytes[6..8],
            u16::try_from(X11_AUTH_PROTOCOL_NAME.len()).unwrap(),
        );
        write_u16(
            order,
            &mut bytes[8..10],
            u16::try_from(X11_COOKIE_BYTES).unwrap(),
        );
        bytes[12..30].copy_from_slice(X11_AUTH_PROTOCOL_NAME.as_bytes());
        bytes[32..48].copy_from_slice(cookie);
        bytes
    }

    /// Writes one test header integer with the selected X11 byte order.
    fn write_u16(order: X11ByteOrder, target: &mut [u8], value: u16) {
        let encoded = match order {
            X11ByteOrder::LittleEndian => value.to_le_bytes(),
            X11ByteOrder::BigEndian => value.to_be_bytes(),
        };
        target.copy_from_slice(&encoded);
    }
}
