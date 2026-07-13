//! Product adapter for provider-independent pane readiness contracts.
//!
//! The readiness state machine is owned by `mez-agent`. Mezzanine supplies
//! concrete shell environment signatures and converts typed readiness errors
//! into the product error aggregate at composition boundaries.

pub use mez_agent::{
    BootstrapDecision, PaneReadinessOverride, PaneReadinessOverrideStore, PaneReadinessState,
    ReadinessDecision, ReadinessOverrideRevocation, decide_bootstrap_before_user_prompt,
    readiness_decision,
};
