//! Exact decimal token quantities; no floats, rounding or locale dependence.

use std::fmt;

/// Common signed integer transport ceiling; lifetime totals remain unsigned.
pub const MAX_TOKEN_LIMIT: u64 = i64::MAX as u64;

/// Invalid syntax, fractional tokens or an externally unrepresentable limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenQuantityError {
    /// Only ASCII digits and optional decimal suffix/fraction are supported.
    Syntax,
    /// A decimal quantity does not resolve to a whole number of tokens.
    FractionalToken,
    /// The exact value exceeds the common external integer ceiling.
    Overflow,
}
impl fmt::Display for TokenQuantityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Syntax => "expected ASCII integer or exact decimal K/M/B/T/P/E quantity",
            Self::FractionalToken => "token quantity must resolve to whole tokens",
            Self::Overflow => "token limit exceeds i64::MAX",
        })
    }
}
impl std::error::Error for TokenQuantityError {}

/// Parses integer quantities and exact suffixed decimals in O(input bytes),
/// constant space. Arbitrarily many leading/trailing zeroes need no big integer.
/// Whitespace, signs, exponents, binary suffixes and unsuffixed fractions reject.
pub fn parse_token_quantity(value: &str) -> Result<u64, TokenQuantityError> {
    let (number, places) = match value.as_bytes().last().copied() {
        Some(b'k' | b'K') => (&value[..value.len() - 1], 3),
        Some(b'm' | b'M') => (&value[..value.len() - 1], 6),
        Some(b'b' | b'B') => (&value[..value.len() - 1], 9),
        Some(b't' | b'T') => (&value[..value.len() - 1], 12),
        Some(b'p' | b'P') => (&value[..value.len() - 1], 15),
        Some(b'e' | b'E') => (&value[..value.len() - 1], 18),
        _ => (value, 0),
    };
    let (whole, fraction) = match number.split_once('.') {
        Some((whole, fraction)) if places != 0 => (whole, Some(fraction)),
        Some(_) => return Err(TokenQuantityError::Syntax),
        None => (number, None),
    };
    let scale = 10_u64.pow(places);
    let mut result = digits(whole)?
        .checked_mul(scale)
        .ok_or(TokenQuantityError::Overflow)?;
    if let Some(fraction) = fraction {
        if fraction.is_empty() || !fraction.bytes().all(|c| c.is_ascii_digit()) {
            return Err(TokenQuantityError::Syntax);
        }
        let mut part = 0_u64;
        for (index, digit) in fraction.bytes().enumerate() {
            if index < places as usize {
                part = part * 10 + u64::from(digit - b'0');
            } else if digit != b'0' {
                return Err(TokenQuantityError::FractionalToken);
            }
        }
        for _ in fraction.len()..places as usize {
            part *= 10;
        }
        result = result
            .checked_add(part)
            .ok_or(TokenQuantityError::Overflow)?;
    }
    if result > MAX_TOKEN_LIMIT {
        return Err(TokenQuantityError::Overflow);
    }
    Ok(result)
}

fn digits(value: &str) -> Result<u64, TokenQuantityError> {
    if value.is_empty() {
        return Err(TokenQuantityError::Syntax);
    }
    value.bytes().try_fold(0_u64, |n, c| {
        if !c.is_ascii_digit() {
            return Err(TokenQuantityError::Syntax);
        }
        n.checked_mul(10)
            .and_then(|n| n.checked_add(u64::from(c - b'0')))
            .ok_or(TokenQuantityError::Overflow)
    })
}
