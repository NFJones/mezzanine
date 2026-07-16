//! Attached-client input boundary planning.
//!
//! This module finds special byte-sequence boundaries and determines how much
//! of a prefix-key sequence can be consumed. It does not map keys to product
//! commands or mutate live client state.

use crate::input::parse_key_chord_bytes;

/// Returns the first occurrence of a complete byte sequence.
pub fn input_sequence_start(input: &[u8], sequence: &[u8]) -> Option<usize> {
    if sequence.is_empty() || sequence.len() > input.len() {
        return None;
    }
    input
        .windows(sequence.len())
        .position(|window| window == sequence)
}

/// Returns the earliest available boundary among special input sequences.
pub fn earliest_sequence_start(starts: impl IntoIterator<Item = Option<usize>>) -> Option<usize> {
    starts.into_iter().flatten().min()
}

/// Returns bytes consumed by one leading prefix sequence.
///
/// A prefix with no following key consumes only the prefix and leaves pending
/// state to the product client. A decodable following key is consumed with the
/// prefix. An undecodable suffix is left for ordinary routing.
pub fn prefix_sequence_len(input: &[u8], prefix: &[u8]) -> Option<usize> {
    if !input.starts_with(prefix) {
        return None;
    }
    if input.len() == prefix.len() {
        return Some(prefix.len());
    }
    let second_len = parse_key_chord_bytes(&input[prefix.len()..])
        .map(|(_, length)| length)
        .unwrap_or(0);
    Some(prefix.len().saturating_add(second_len))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies special boundaries choose the earliest present byte sequence.
    #[test]
    fn special_sequence_boundary_uses_earliest_match() {
        assert_eq!(earliest_sequence_start([Some(8), None, Some(3)]), Some(3));
        assert_eq!(input_sequence_start(b"abc-prefix", b"prefix"), Some(4));
    }

    /// Verifies prefix planning consumes one complete following key only.
    #[test]
    fn prefix_sequence_length_bounds_one_following_key() {
        assert_eq!(prefix_sequence_len(b"\x01", b"\x01"), Some(1));
        assert_eq!(prefix_sequence_len(b"\x01\x1b[Arest", b"\x01"), Some(4));
        assert_eq!(prefix_sequence_len(b"ordinary", b"\x01"), None);
    }
}
