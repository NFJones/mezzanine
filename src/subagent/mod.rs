//! Product-friendly subagent presentation names.
//!
//! Canonical cooperation, scope coordination, validation, and enforcement live
//! in `mez_agent::subagent`. Root retains only display names assigned to product
//! panes and status lines.

/// Exposes product-friendly subagent display names.
///
/// The canonical subagent domain is owned by `mez-agent`; only product
/// presentation naming remains in this module.
mod names;
pub use names::SUBAGENT_FRIENDLY_NAMES;
