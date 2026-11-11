//! Pure root-session accounting contracts, not an executor or persistence store.
//!
//! Product owners allocate identities, authenticate membership/policy, durably
//! reconcile receipts and serialize admission with revision checks. A decision
//! here is a qualified observation, never a reusable provider/effect permit.
//! Counters are inclusive input and reported output: cache and reasoning subsets
//! MUST NOT be added again. Unknown coverage alone is not numeric exhaustion.

mod checkpoint;
mod quantity;
mod usage;
pub use checkpoint::BudgetCheckpointReference;
pub use quantity::{MAX_TOKEN_LIMIT, TokenQuantityError, parse_token_quantity};
pub use usage::{AttemptUsage, BudgetUsageError, UsageCoverage, UsageSnapshot, UsageTotals};

use std::fmt;

/// Invalid opaque accounting identity; diagnostics never echo supplied values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetIdentityError;

impl fmt::Display for BudgetIdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("budget identity must be nonempty, bounded and control/whitespace-free")
    }
}
impl std::error::Error for BudgetIdentityError {}

macro_rules! identity {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);
        impl $name {
            /// Validates opaque text only; does not allocate or prove ownership.
            pub fn new(value: impl Into<String>) -> Result<Self, BudgetIdentityError> {
                let value = value.into();
                if value.is_empty() || value.len() > 256
                    || value.chars().any(|c| c.is_control() || c.is_whitespace()) {
                    return Err(BudgetIdentityError);
                }
                Ok(Self(value))
            }
            /// Returns opaque identity text, not a filesystem path or authority.
            pub fn as_str(&self) -> &str { &self.0 }
        }
        impl TryFrom<String> for $name {
            type Error = BudgetIdentityError;
            fn try_from(value: String) -> Result<Self, Self::Error> { Self::new(value) }
        }
        impl From<$name> for String {
            fn from(value: $name) -> Self { value.0 }
        }
    };
}
identity!(
    BudgetRootId,
    "Immutable accounting root, unrelated to current pane/depth/working directory."
);
identity!(
    BudgetAttemptId,
    "One concrete inference send; retries require distinct attempt identities."
);
identity!(
    BudgetReceiptId,
    "Immutable receipt occurrence; exact replay and conflicts are checked by storage."
);

/// An independent absolute cumulative limit, not remaining allowance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TokenLimit {
    /// No numeric threshold in this dimension.
    #[default]
    Unlimited,
    /// Inclusive exhaustion threshold. Zero blocks immediately. Trusted Rust
    /// construction must respect MAX_TOKEN_LIMIT; use `limited` for checked
    /// values. Both serde directions independently enforce that external bound.
    Limited(
        #[serde(
            deserialize_with = "deserialize_limit",
            serialize_with = "serialize_limit"
        )]
        u64,
    ),
}

impl TokenLimit {
    /// Checks the common external ceiling for numeric ingress without text.
    pub fn limited(value: u64) -> Result<Self, TokenQuantityError> {
        if value > MAX_TOKEN_LIMIT {
            return Err(TokenQuantityError::Overflow);
        }
        Ok(Self::Limited(value))
    }
    /// Parses a canonical lowercase keyword or the common exact quantity grammar.
    pub fn parse(value: &str) -> Result<Self, TokenQuantityError> {
        if value == "unlimited" {
            Ok(Self::Unlimited)
        } else {
            parse_token_quantity(value).map(Self::Limited)
        }
    }
    /// Reports observed exhaustion independently of accounting coverage.
    pub fn exhausted(self, known_used: u64) -> bool {
        matches!(self, Self::Limited(limit) if known_used >= limit)
    }
    /// Remaining known allowance, clamped at zero; unlimited has no denominator.
    pub fn remaining(self, known_used: u64) -> Option<u64> {
        match self {
            Self::Unlimited => None,
            Self::Limited(n) => Some(n.saturating_sub(known_used)),
        }
    }
    /// Known overshoot is separate from remaining allowance, never negative.
    pub fn excess(self, known_used: u64) -> u64 {
        match self {
            Self::Unlimited => 0,
            Self::Limited(n) => known_used.saturating_sub(n),
        }
    }
    /// Exact decimal suffix display; no rounded value can conceal a threshold.
    pub fn compact(self) -> String {
        let Self::Limited(value) = self else {
            return self.to_string();
        };
        for (power, suffix) in [
            (18, 'E'),
            (15, 'P'),
            (12, 'T'),
            (9, 'B'),
            (6, 'M'),
            (3, 'K'),
        ] {
            let scale = 10_u64.pow(power);
            if value >= scale {
                let remainder = value % scale;
                if remainder == 0 {
                    return format!("{}{suffix}", value / scale);
                }
                let fraction = format!("{remainder:0width$}", width = power as usize);
                return format!(
                    "{}.{}{suffix}",
                    value / scale,
                    fraction.trim_end_matches('0')
                );
            }
        }
        value.to_string()
    }
}
impl fmt::Display for TokenLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unlimited => f.write_str("unlimited"),
            Self::Limited(n) => write!(f, "{n}"),
        }
    }
}

/// Keep nested override/limit records on the same numeric boundary as text.
fn deserialize_limit<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    let value = <u64 as serde::Deserialize>::deserialize(deserializer)?;
    TokenLimit::limited(value).map_err(serde::de::Error::custom)?;
    Ok(value)
}

/// Reject invalid trusted construction rather than emitting unparseable policy.
fn serialize_limit<S: serde::Serializer>(value: &u64, serializer: S) -> Result<S::Ok, S::Error> {
    TokenLimit::limited(*value).map_err(serde::ser::Error::custom)?;
    serializer.serialize_u64(*value)
}

/// Per-field authored root override; explicit unlimited masks finite defaults.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BudgetOverride {
    /// Use the current primary default (ultimately unlimited).
    #[default]
    Inherit,
    /// Preserve an explicit choice, even when equal to today's default.
    Explicit(TokenLimit),
}
impl BudgetOverride {
    /// Resolves one dimension without changing usage or authored overrides.
    pub fn resolve(self, default: TokenLimit) -> TokenLimit {
        match self {
            Self::Inherit => default,
            Self::Explicit(limit) => limit,
        }
    }
}

/// Effective independent thresholds, after per-field precedence resolution.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BudgetLimits {
    /// Inclusive input, including cache exactly once.
    pub input: TokenLimit,
    /// Provider-reported output, without adding reasoning again.
    pub output: TokenLimit,
}
impl BudgetLimits {
    /// Initially finite roots permit only one locally outstanding inference.
    /// This exposure policy is not a numerical or remote billing guarantee.
    pub fn requires_serial_inference(self) -> bool {
        self.input != TokenLimit::Unlimited || self.output != TokenLimit::Unlimited
    }
}

/// Operational admission holds are distinct from numeric exhaustion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetOperationalHold {
    /// An issued observation has not yet durably settled.
    AccountingPending,
    /// Required accounting/policy storage cannot currently be consulted.
    AccountingUnavailable,
    /// Membership, provenance, monotonicity or arithmetic is inconsistent.
    Integrity,
}

/// Exact revision pair to revalidate at actual admission, not only preparation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BudgetRevisions {
    /// Committed effective-policy revision.
    pub policy: u64,
    /// Committed lifetime-accounting revision.
    pub accounting: u64,
}

/// Pure observed policy result. Other pause/approval/Stop gates remain separate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetDecision {
    /// Original immutable root.
    pub root: BudgetRootId,
    /// Both policy and accounting must still match at claim time.
    pub revisions: BudgetRevisions,
    /// Independent inclusive-input exhaustion fact.
    pub input_exhausted: bool,
    /// Independent output exhaustion fact.
    pub output_exhausted: bool,
    /// A nonnumeric hold must not be described as a reached token limit.
    pub operational_hold: Option<BudgetOperationalHold>,
}
impl BudgetDecision {
    /// Computes facts without changing usage, authority, scheduler or persistence.
    pub fn evaluate(
        root: BudgetRootId,
        revisions: BudgetRevisions,
        limits: BudgetLimits,
        used: UsageTotals,
        operational_hold: Option<BudgetOperationalHold>,
    ) -> Self {
        Self {
            root,
            revisions,
            input_exhausted: limits.input.exhausted(used.input),
            output_exhausted: limits.output.exhausted(used.output),
            operational_hold,
        }
    }
    /// True only for this budget's reasons; not a bypass of any other gate.
    pub fn permits_budget_admission(&self) -> bool {
        !self.input_exhausted && !self.output_exhausted && self.operational_hold.is_none()
    }
}

#[cfg(test)]
mod tests;
