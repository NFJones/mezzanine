//! Product MMP regressions grouped by the invariant they protect.
//!
//! Behavior owners share only multi-consumer fixtures. Receive commit and
//! presentation settlement remain separate from turn scheduling and dispatch;
//! all owners exercise the product's canonical message service and context.

use super::*;
use crate::config::{ConfigFormat, ConfigLayer, ConfigScope};
use crate::runtime::ControlIdempotencyCache;
use crate::runtime::{current_unix_millis, current_unix_seconds};
use mez_agent::messaging::{Envelope, MessageScope, MessageService};
use mez_core::ids::PaneId;

mod action_dispatch;
mod delivery_lifecycle;
mod discovery_and_approval;
mod fixtures;
mod identity_validation;
mod loop_episodes;
mod objectives;
mod presentation;
mod receive_commit;
mod snapshot_recovery;
mod turn_lifecycle;
