//! Runtime persistence repositories and deferred external-effect ownership.
//!
//! This component owns concrete repository handles, durable pane references,
//! adapter handoff modes, sequence reservations, and queues that cross from
//! serialized runtime transitions into external I/O workers. It does not own
//! configuration, authorization policy, or control connection state.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use crate::security::audit::AuditLog;
use crate::storage::registry::SessionRegistry;
use crate::storage::snapshot::SnapshotRepository;
use crate::storage::token_usage::TokenUsageStore;
use crate::storage::transcript::AgentTranscriptStore;
use mez_core::ids::ClientId;
use mez_terminal::TerminalSize;

use super::RuntimeSideEffect;

mod adapters;
mod effects;
mod stores;

/// Owns repository handles and deferred effects for one application runtime.
#[derive(Debug, Default)]
pub(crate) struct RuntimePersistenceComponent {
    snapshot_repository: Option<SnapshotRepository>,
    agent_transcript_store: Option<AgentTranscriptStore>,
    token_usage_store: Option<TokenUsageStore>,
    token_usage_health_error: RefCell<Option<String>>,
    session_registry: Option<SessionRegistry>,
    audit_log: Option<AuditLog>,
    queued_pane_input_effects: Vec<RuntimeSideEffect>,
    queued_pane_resize_effects: BTreeMap<String, RuntimeSideEffect>,
    expected_pane_resize_sizes: BTreeMap<String, TerminalSize>,
    queued_pane_termination_effects: BTreeMap<String, RuntimeSideEffect>,
    queued_pane_pipe_effects: Vec<(String, RuntimeSideEffect)>,
    queued_audit_effects: Vec<RuntimeSideEffect>,
    queued_transcript_effects: Vec<RuntimeSideEffect>,
    pending_session_archive_conversation_ids: BTreeSet<String>,
    pending_session_archive_resumes: BTreeMap<String, (ClientId, String)>,
    queued_token_usage_effects: Vec<RuntimeSideEffect>,
    queued_provider_settlement_effects: Vec<RuntimeSideEffect>,
    queued_config_effects: Vec<RuntimeSideEffect>,
    queued_program_hook_effects: Vec<RuntimeSideEffect>,
    deferred_transcript_next_sequences: BTreeMap<String, u64>,
    pane_transcript_refs: BTreeMap<String, Vec<String>>,
    audit_effects_use_adapter: bool,
    pane_pipe_effects_use_adapter: bool,
    transcript_effects_use_adapter: bool,
    token_usage_effects_use_adapter: bool,
    provider_settlement_effects_use_adapter: bool,
    registry_effects_use_adapter: bool,
    config_effects_use_adapter: bool,
    hook_effects_use_adapter: bool,
}
