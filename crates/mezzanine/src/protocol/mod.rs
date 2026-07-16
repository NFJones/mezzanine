//! Product protocol records, framing, messaging adapters, and identifiers.
//!
//! This boundary owns Mezzanine-specific wire and event behavior. Canonical
//! agent messaging policy remains in `mez-agent`; these modules bind it to the
//! product's framing, fanout, and observer surfaces.

pub(crate) mod event;
pub(crate) mod framing;
pub(crate) mod identifiers;
pub(crate) mod message;
