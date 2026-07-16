//! Concrete product integration state ownership.
//!
//! This component owns live application bindings that join otherwise separate
//! configuration, security, provider, storage, and hook domains. Its state is
//! intentionally private: runtime adapters may borrow focused values through
//! typed operations, but the session coordinator does not expose a second
//! crate-wide field bag.

use std::path::{Path, PathBuf};

use crate::async_runtime::AsyncRuntimeActorMetrics;
use crate::config::ConfigLayer;

use super::service_state::RuntimeMetricsSnapshot;

/// Owns concrete application integration bindings for one runtime session.
#[derive(Debug, Default)]
pub(in crate::runtime) struct RuntimeIntegrationComponent {
    config_layers: Vec<ConfigLayer>,
    config_root: Option<PathBuf>,
    async_runtime_metrics: Option<AsyncRuntimeActorMetrics>,
    runtime_metrics: RuntimeMetricsSnapshot,
}

impl RuntimeIntegrationComponent {
    /// Returns the active configuration layers in precedence order.
    pub(in crate::runtime) fn config_layers(&self) -> &[ConfigLayer] {
        &self.config_layers
    }

    /// Returns active configuration layers for transactional mutation.
    pub(in crate::runtime) fn config_layers_mut(&mut self) -> &mut Vec<ConfigLayer> {
        &mut self.config_layers
    }

    /// Replaces every active configuration layer atomically.
    pub(in crate::runtime) fn replace_config_layers(&mut self, layers: Vec<ConfigLayer>) {
        self.config_layers = layers;
    }

    /// Returns the optional project configuration root.
    pub(in crate::runtime) fn config_root(&self) -> Option<&Path> {
        self.config_root.as_deref()
    }

    /// Replaces the optional project configuration root.
    pub(in crate::runtime) fn set_config_root(&mut self, root: Option<PathBuf>) {
        self.config_root = root;
    }

    /// Returns the latest async-actor metrics snapshot.
    pub(in crate::runtime) fn async_runtime_metrics(&self) -> Option<&AsyncRuntimeActorMetrics> {
        self.async_runtime_metrics.as_ref()
    }

    /// Replaces the latest async-actor metrics snapshot.
    pub(in crate::runtime) fn set_async_runtime_metrics(
        &mut self,
        metrics: Option<AsyncRuntimeActorMetrics>,
    ) {
        self.async_runtime_metrics = metrics;
    }

    /// Returns application runtime metrics.
    pub(in crate::runtime) fn runtime_metrics(&self) -> &RuntimeMetricsSnapshot {
        &self.runtime_metrics
    }

    /// Returns application runtime metrics for serialized mutation.
    pub(in crate::runtime) fn runtime_metrics_mut(&mut self) -> &mut RuntimeMetricsSnapshot {
        &mut self.runtime_metrics
    }
}
