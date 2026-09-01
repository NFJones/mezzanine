//! Security-sensitive foundations for X11 forwarding over Iroh.
//!
//! This subsystem owns the protocol constants, secret-bearing route contracts,
//! bounded X11 setup parsing and cookie rewriting, and private Xauthority file
//! encoding. Session proxying, control negotiation, Iroh stream ownership, and
//! client-local display dialing build on these primitives in later layers.
//!
//! X11 bytes are never control frames. The fixed stream preface authenticates
//! one server-opened bidirectional stream before the negotiated Iroh codec is
//! applied to bounded X11 setup and application records.

mod authority;
mod contracts;
mod protocol;
mod proxy;
mod transport;

pub(crate) use authority::{
    encode_xauthority_record, write_empty_private_xauthority, write_private_xauthority,
};
pub(crate) use contracts::{
    X11_AUTH_PROTOCOL_NAME, X11_COOKIE_BYTES, X11_FORWARDING_VERSION, X11_ROUTE_TOKEN_BYTES,
    X11_STREAM_PREFACE_BYTES, X11AuthProtocol, X11Cookie, X11ForwardingMode, X11ForwardingOffer,
    X11ForwardingResult, X11RouteToken, X11StreamPreface, X11StreamPrefaceError,
};
pub(crate) use protocol::{
    X11_MAX_SETUP_BYTES, X11_PROTOCOL_MAJOR, X11_PROTOCOL_MINOR, X11ByteOrder, X11SetupError,
    X11SetupPacket, X11SetupProgress, parse_x11_setup, rewrite_x11_setup_cookie,
    validate_x11_setup_cookie,
};
pub(crate) use proxy::{
    RuntimeX11Proxy, RuntimeX11ProxyDiagnosticsSnapshot, RuntimeX11ProxyHandle,
    RuntimeX11RouteLease, RuntimeX11RouteOwner,
};
pub(crate) use transport::{X11IrohDecoder, X11IrohEncoder};
