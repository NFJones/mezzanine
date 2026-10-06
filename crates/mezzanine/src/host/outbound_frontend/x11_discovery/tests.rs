//! Discovery cannot substitute directory paths or arbitrary socket names.
use super::*;

/// Fixed lowercase random names are representable; traversal, absolute paths,
/// Unicode, wrong length/case and other socket families fail without I/O.
#[test]
fn outbound_x11_discovery_names_are_closed() {
    validate_socket_name("x0123456789abcdef.sock").unwrap();
    for name in [
        "../x0123456789abcdef.sock",
        "/x0123456789abcdef.sock",
        "outbound.sock",
        "x0123456789ABCDEF.sock",
        "x0123456789abcde.sock",
        "x0123456789abcdeg.sock",
        "雪0123456789abcdef.sock",
    ] {
        assert!(validate_socket_name(name).is_err());
    }
}
