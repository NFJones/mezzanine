//! Focused actor boundary tests.

use super::{
    DEFAULT_PROVIDER_CLAIM_TIMEOUT_MS, DEFAULT_PROVIDER_TIMEOUT_MS, RuntimeSideEffect,
    coalesce_config_persistence_effects,
};
use crate::runtime::{PersistenceTarget, PersistenceWriteMode};
use std::path::PathBuf;

/// Verifies that the provider worker watchdog cannot fire before the
/// provider transport timeout. The watchdog cleans up abandoned async
/// claims, so it must leave enough time for a legitimate long-running
/// provider request to settle through the provider layer first.
#[test]
fn provider_claim_timeout_exceeds_provider_transport_timeout() {
    let claim_timeout_ms = std::hint::black_box(DEFAULT_PROVIDER_CLAIM_TIMEOUT_MS);
    let provider_timeout_ms = std::hint::black_box(DEFAULT_PROVIDER_TIMEOUT_MS);

    assert!(
        claim_timeout_ms > provider_timeout_ms,
        "provider claim watchdog {} ms must exceed provider timeout {} ms",
        claim_timeout_ms,
        provider_timeout_ms
    );
}

/// Verifies repeated deferred config replacements for the same destination
/// collapse to the newest complete document.
///
/// A single provider response can contain many `config_change` actions for
/// adjacent theme slots. The actor should persist only the final config text
/// for each file instead of queueing a long series of superseded full-file
/// replacements.
#[test]
fn coalesce_config_persistence_effects_keeps_latest_text_per_target() {
    let config_path = PathBuf::from("/tmp/mez/config.toml");
    let project_path = PathBuf::from("/tmp/project/.mezzanine/config.toml");

    let coalesced = coalesce_config_persistence_effects(vec![
        RuntimeSideEffect::Persist {
            target: PersistenceTarget::Config,
            path: config_path.clone(),
            bytes: b"first".to_vec(),
            mode: PersistenceWriteMode::Replace,
        },
        RuntimeSideEffect::Persist {
            target: PersistenceTarget::ProjectConfig,
            path: project_path.clone(),
            bytes: b"project".to_vec(),
            mode: PersistenceWriteMode::Replace,
        },
        RuntimeSideEffect::Persist {
            target: PersistenceTarget::Config,
            path: config_path.clone(),
            bytes: b"second".to_vec(),
            mode: PersistenceWriteMode::Replace,
        },
    ]);

    assert_eq!(coalesced.len(), 2);
    assert!(matches!(
        &coalesced[0],
        RuntimeSideEffect::Persist { target: PersistenceTarget::Config, path, bytes, .. }
            if path == &config_path && bytes == b"second"
    ));
    assert!(matches!(
        &coalesced[1],
        RuntimeSideEffect::Persist { target: PersistenceTarget::ProjectConfig, path, bytes, .. }
            if path == &project_path && bytes == b"project"
    ));
}
