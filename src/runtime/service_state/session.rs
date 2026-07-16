//! Long-lived runtime session-service aggregate and owned subsystem stores.

use crate::runtime::{
    RuntimeAgentComponent, RuntimeControlComponent, RuntimeIntegrationComponent,
    RuntimePersistenceComponent, RuntimePresentationComponent, RuntimeProcessComponent,
    RuntimeSessionComponent,
};

/// Carries Runtime Session Service state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug)]
pub struct RuntimeSessionService {
    /// Private state owner for terminal presentation and client interaction.
    pub(in crate::runtime) presentation: RuntimePresentationComponent,
    /// Private state owner for pane process metadata and lifecycle invariants.
    pub(in crate::runtime) process: RuntimeProcessComponent,
    /// Private state owner for application-side agent execution.
    pub(in crate::runtime) agent: RuntimeAgentComponent,
    /// Private state owner for repositories and deferred external effects.
    pub(in crate::runtime) persistence: RuntimePersistenceComponent,
    /// Private state owner for control replay, messaging, and event fanout.
    pub(in crate::runtime) control: RuntimeControlComponent,
    /// Private state owner for concrete application integration bindings.
    pub(in crate::runtime) integration: RuntimeIntegrationComponent,
    /// Private owner for the mux session and application lifecycle metadata.
    pub(in crate::runtime) session: RuntimeSessionComponent,
}
