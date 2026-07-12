//! Provider-independent agent harness and agent protocol state machines.
//!
//! This crate will own model-facing request normalization, MAAP contracts,
//! turn orchestration, and provider-independent macro and subagent behavior.
//! Product credentials, persistence, transports, process execution, and UI
//! remain behind ports implemented by the root package. The initial empty
//! facade establishes that dependency boundary before those ports are added.
