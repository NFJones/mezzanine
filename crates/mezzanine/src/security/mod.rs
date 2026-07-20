//! Product credential, audit, permission, and project-trust boundaries.
//!
//! Concrete secret stores, OAuth callbacks, filesystem trust, audit
//! persistence, and live permission bindings remain application-owned here.

pub(crate) mod audit;
pub(crate) mod auth;
pub(crate) mod permissions;
pub(crate) mod project;
#[allow(
    dead_code,
    reason = "typed sandbox plans are consumed by the dependent pane-transaction integration"
)]
pub(crate) mod sandbox;
