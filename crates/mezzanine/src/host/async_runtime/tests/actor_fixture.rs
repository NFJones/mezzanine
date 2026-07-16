//! Async runtime actor fixture owned by the async-runtime tests.
//!
//! Async runtime tests repeatedly build the same default session actor from a
//! runtime service and default actor configuration. This module keeps that
//! setup beside its sole owning test tree.

use crate::host::async_runtime::{
    AsyncRuntimeActorConfig, AsyncRuntimeSessionActor, AsyncRuntimeSessionHandle,
};
use crate::runtime::RuntimeSessionService;

/// Builds an async runtime session actor with stable test defaults.
#[derive(Debug)]
pub(super) struct AsyncRuntimeActorFixture {
    service: RuntimeSessionService,
    config: AsyncRuntimeActorConfig,
}

impl AsyncRuntimeActorFixture {
    /// Creates a fixture from a caller-supplied runtime service.
    pub(super) fn from_service(service: RuntimeSessionService) -> Self {
        Self {
            service,
            config: AsyncRuntimeActorConfig::default(),
        }
    }

    /// Replaces the actor configuration for tests exercising bounded queues.
    pub(super) fn config(mut self, config: AsyncRuntimeActorConfig) -> Self {
        self.config = config;
        self
    }

    /// Builds the async runtime actor and handle.
    pub(super) fn build(
        self,
    ) -> crate::Result<(AsyncRuntimeSessionHandle, AsyncRuntimeSessionActor)> {
        AsyncRuntimeSessionActor::new(self.service, self.config)
    }
}
