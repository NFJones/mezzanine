//! Exact logical presentation identity shared by terminal transport adapters.
//!
//! A digest identifies server-owned view bytes, not authorization or a committed
//! local output frame. Consumers must retain session ownership and separately
//! prove output commitment before using an identity for conditional rendering.

/// Accepts only the existing lowercase SHA-256 wire digest shape. This check
/// validates representation, not provenance or any implied execution authority.
pub(crate) fn valid_view_identity(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
