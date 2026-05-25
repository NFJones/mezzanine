//! Async runtime actor fixtures for tests.
//!
//! Async runtime tests repeatedly build the same default session actor from a
//! runtime service and default actor configuration. This module keeps that
//! setup in one place so tests can override the service or configuration only
//! when the behavior under test requires it.

use crate::async_runtime::{
    AsyncRuntimeActorConfig, AsyncRuntimeSessionActor, AsyncRuntimeSessionHandle,
};
use crate::runtime::RuntimeSessionService;

use super::runtime::RuntimeServiceFixture;

/// Builds an async runtime session actor with stable test defaults.
#[derive(Debug)]
pub(crate) struct AsyncRuntimeActorFixture {
    service: RuntimeSessionService,
    config: AsyncRuntimeActorConfig,
}

impl AsyncRuntimeActorFixture {
    /// Creates a fixture backed by a default runtime service and actor config.
    pub(crate) fn new() -> Self {
        Self {
            service: RuntimeServiceFixture::new().build(),
            config: AsyncRuntimeActorConfig::default(),
        }
    }

    /// Creates a fixture from a caller-supplied runtime service.
    pub(crate) fn from_service(service: RuntimeSessionService) -> Self {
        Self {
            service,
            config: AsyncRuntimeActorConfig::default(),
        }
    }

    /// Replaces the actor configuration.
    pub(crate) fn config(mut self, config: AsyncRuntimeActorConfig) -> Self {
        self.config = config;
        self
    }

    /// Builds the async runtime actor and handle.
    pub(crate) fn build(
        self,
    ) -> crate::Result<(AsyncRuntimeSessionHandle, AsyncRuntimeSessionActor)> {
        AsyncRuntimeSessionActor::new(self.service, self.config)
    }
}

impl Default for AsyncRuntimeActorFixture {
    fn default() -> Self {
        Self::new()
    }
}
